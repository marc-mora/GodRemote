//! Salto por bastión, túneles (local, remoto y SOCKS) y reenvío X11 contra
//! un `sshd` real de OpenSSH; y Telnet contra un `telnetd` real.
//!
//! Cada parte se omite si falta el programa que necesita.
#![cfg(unix)]

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{Killed, answers, find_program, free_port, is_root, prompter, start_sshd, wait_port};
use godremote::keys::{self, KeyAlgorithm};
use godremote::model::{AuthMethod, Session, Tunnel, TunnelKind};
use godremote::ssh::{self, Credentials, Endpoint};
use godremote::terminal::{TermBackend, TermCmd, TermEvent};
use godremote::tunnels::TunnelStatus;

/// Servidor de eco: responde `eco:` + lo recibido.
fn echo_server() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut stream = stream;
                let mut buf = [0u8; 256];
                while let Ok(n) = stream.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let mut reply = b"eco:".to_vec();
                    reply.extend_from_slice(&buf[..n]);
                    if stream.write_all(&reply).is_err() {
                        break;
                    }
                }
            });
        }
    });
    port
}

fn roundtrip(mut stream: TcpStream, message: &[u8]) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(message).unwrap();
    let mut buf = vec![0u8; 4 + message.len()];
    stream.read_exact(&mut buf).expect("respuesta del eco");
    buf
}

/// Lee de la shell hasta que la salida contenga `needle`.
fn wait_output(backend: &TermBackend, output: &mut String, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !output.contains(needle) {
        match backend
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(TermEvent::Data(d)) => output.push_str(&String::from_utf8_lossy(&d)),
            Ok(TermEvent::Status(s)) => output.push_str(&format!("[{s}]\n")),
            Ok(TermEvent::Closed(e)) => panic!("sesión cerrada: {e:?}\n{output}"),
            Ok(_) => {}
            Err(_) => panic!("no apareció {needle:?} en:\n{output}"),
        }
    }
}

#[test]
fn jump_tunnels_and_x11() {
    if find_program("sshd").is_none() || !is_root() {
        eprintln!("OMITIDA: hace falta sshd y ejecutar como root");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: único test de este binario que toca el entorno, antes de crear hilos.
    unsafe { std::env::set_var("GODREMOTE_KNOWN_HOSTS", dir.path().join("known_hosts")) };

    // --- Servidor X local protegido con cookie (para el reenvío X11) -------
    let x11_tools = ["Xvfb", "xauth", "xdpyinfo"]
        .iter()
        .all(|p| find_program(p).is_some());
    let mut _xvfb = None;
    if x11_tools {
        let display = (60..100)
            .find(|n| !std::path::Path::new(&format!("/tmp/.X11-unix/X{n}")).exists())
            .unwrap();
        let xauthority = dir.path().join("Xauthority");
        let cookie = "0123456789abcdef0123456789abcdef";
        let ok = Command::new("xauth")
            .args(["-f", xauthority.to_str().unwrap(), "add"])
            .args([&format!(":{display}"), ".", cookie])
            .status()
            .unwrap()
            .success();
        assert!(ok, "xauth add");
        let child = Command::new("Xvfb")
            .arg(format!(":{display}"))
            .args(["-auth", xauthority.to_str().unwrap(), "-nolisten", "tcp"])
            .args(["-screen", "0", "640x480x24"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        _xvfb = Some(Killed(child));
        let socket = format!("/tmp/.X11-unix/X{display}");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !std::path::Path::new(&socket).exists() {
            assert!(Instant::now() < deadline, "Xvfb no arrancó");
            std::thread::sleep(Duration::from_millis(50));
        }
        // Sin la cookie, el servidor X rechaza la conexión.
        let denied = Command::new("xdpyinfo")
            .env("DISPLAY", format!(":{display}"))
            .env("XAUTHORITY", "/dev/null")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!denied.success(), "Xvfb debería exigir la cookie");
        unsafe {
            std::env::set_var("DISPLAY", format!(":{display}"));
            std::env::set_var("XAUTHORITY", &xauthority);
        }
    }

    let key = keys::generate(dir.path(), "id", KeyAlgorithm::Ed25519, "", None).unwrap();
    let authorized = dir.path().join("authorized_keys");
    std::fs::write(&authorized, format!("{}\n", key.public_openssh)).unwrap();
    let (_sshd, port, sshd_log) = start_sshd(
        dir.path(),
        &authorized,
        "LogLevel DEBUG1\nAllowTcpForwarding yes\nX11Forwarding yes\nXAuthLocation /usr/bin/xauth\n",
    );
    let user = String::from_utf8_lossy(&Command::new("id").arg("-un").output().unwrap().stdout)
        .trim()
        .to_string();
    let session = Session {
        name: "destino".into(),
        host: "127.0.0.1".into(),
        port,
        username: user,
        auth: AuthMethod::Key,
        key_path: key.path.display().to_string(),
        ..Default::default()
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let quiet = |_: String| {};

    // --- 1. Salto por bastión ---------------------------------------------
    let bastion = Session {
        name: "bastion".into(),
        ..session.clone()
    };
    let endpoint = Endpoint {
        session: session.clone(),
        creds: Credentials::default(),
        via: Some(Box::new(Endpoint::direct(
            bastion.clone(),
            Credentials::default(),
        ))),
    };
    let a = answers(true, &[]);
    let connected = rt
        .block_on(ssh::connect_endpoint(
            &endpoint,
            &prompter(a.clone()),
            &quiet,
        ))
        .expect("conexión a través del salto");
    // El host se pregunta una sola vez (salto y destino son el mismo servidor).
    assert_eq!(a.hosts(), 1);
    // Se ejecuta una orden en el destino para comprobar que la sesión funciona.
    let out = rt.block_on(async {
        let mut channel = connected.handle.channel_open_session().await.unwrap();
        channel.exec(true, "echo salto-$((6*7))").await.unwrap();
        let mut out = Vec::new();
        while let Some(msg) = channel.wait().await {
            if let russh::ChannelMsg::Data { data } = msg {
                out.extend_from_slice(&data);
            }
        }
        String::from_utf8_lossy(&out).to_string()
    });
    assert_eq!(out.trim(), "salto-42");
    drop(connected);
    // El registro de sshd confirma que el destino se alcanzó por un canal
    // direct-tcpip abierto en el salto, no con una conexión directa.
    let log = std::fs::read_to_string(&sshd_log).unwrap();
    assert!(
        log.contains("direct-tcpip"),
        "sshd no registró el salto:\n{log}"
    );

    // Si el salto falla, el error lo dice.
    let mut bad_bastion = bastion.clone();
    bad_bastion.key_path = dir.path().join("no-existe").display().to_string();
    let endpoint = Endpoint {
        session: session.clone(),
        creds: Credentials::default(),
        via: Some(Box::new(Endpoint::direct(
            bad_bastion,
            Credentials::default(),
        ))),
    };
    let err = rt
        .block_on(ssh::connect_endpoint(
            &endpoint,
            &prompter(answers(true, &[])),
            &quiet,
        ))
        .err()
        .expect("debe fallar");
    assert!(
        format!("{err:#}").contains("en el salto bastion"),
        "{err:#}"
    );

    // --- 2. Túneles ----------------------------------------------------------
    let echo = echo_server();
    let (local, socks, remote) = (free_port(), free_port(), free_port());
    let mut with_tunnels = session.clone();
    with_tunnels.x11_forwarding = x11_tools;
    with_tunnels.tunnels = vec![
        Tunnel {
            kind: TunnelKind::Local,
            listen_host: "127.0.0.1".into(),
            listen_port: local,
            target_host: "127.0.0.1".into(),
            target_port: echo,
        },
        Tunnel {
            kind: TunnelKind::Dynamic,
            listen_host: "127.0.0.1".into(),
            listen_port: socks,
            ..Default::default()
        },
        Tunnel {
            kind: TunnelKind::Remote,
            listen_host: "127.0.0.1".into(),
            listen_port: remote,
            target_host: "127.0.0.1".into(),
            target_port: echo,
        },
        // Mismo puerto local que el primero: debe fallar sin tumbar la sesión.
        Tunnel {
            kind: TunnelKind::Local,
            listen_host: "127.0.0.1".into(),
            listen_port: local,
            target_host: "127.0.0.1".into(),
            target_port: echo,
        },
    ];
    let backend = ssh::spawn_shell(
        rt.handle(),
        Endpoint::direct(with_tunnels, Credentials::default()),
        prompter(answers(true, &[])),
        80,
        24,
    );
    let mut statuses = vec![None; 4];
    let mut output = String::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match backend
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(TermEvent::Tunnel { index, status }) => statuses[index] = Some(status),
            Ok(TermEvent::Status(s)) => output.push_str(&format!("[{s}]\n")),
            Ok(TermEvent::Connected(_)) => break,
            Ok(TermEvent::Closed(e)) => panic!("cerrada: {e:?}"),
            Ok(_) => {}
            Err(_) => panic!("la shell no conectó: {output}"),
        }
    }
    for (i, status) in statuses.iter().take(3).enumerate() {
        assert_eq!(*status, Some(TunnelStatus::Active), "túnel {i}");
    }
    assert!(
        matches!(&statuses[3], Some(TunnelStatus::Failed(e)) if e.contains("no se pudo escuchar")),
        "{:?}",
        statuses[3]
    );

    // Local (-L).
    let reply = roundtrip(TcpStream::connect(("127.0.0.1", local)).unwrap(), b"local");
    assert_eq!(reply, b"eco:local");
    // Varias conexiones simultáneas por el mismo túnel.
    let streams: Vec<_> = (0..5)
        .map(|_| TcpStream::connect(("127.0.0.1", local)).unwrap())
        .collect();
    for (i, s) in streams.into_iter().enumerate() {
        let msg = format!("c{i}");
        assert_eq!(
            roundtrip(s, msg.as_bytes()),
            format!("eco:{msg}").as_bytes()
        );
    }

    // Dinámico (-D): SOCKS5 CONNECT por nombre de host.
    let mut s = TcpStream::connect(("127.0.0.1", socks)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(&[5, 1, 0]).unwrap();
    let mut two = [0u8; 2];
    s.read_exact(&mut two).unwrap();
    assert_eq!(two, [5, 0]);
    let mut req = vec![5, 1, 0, 3, 9];
    req.extend_from_slice(b"localhost");
    req.extend_from_slice(&echo.to_be_bytes());
    s.write_all(&req).unwrap();
    let mut resp = [0u8; 10];
    s.read_exact(&mut resp).unwrap();
    assert_eq!(&resp[..2], &[5, 0], "SOCKS debe aceptar");
    assert_eq!(roundtrip(s, b"socks"), b"eco:socks");
    // Destino cerrado: SOCKS contesta con error.
    let mut s = TcpStream::connect(("127.0.0.1", socks)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let closed = free_port();
    s.write_all(&[5, 1, 0]).unwrap();
    s.read_exact(&mut two).unwrap();
    s.write_all(&[5, 1, 0, 1, 127, 0, 0, 1]).unwrap();
    s.write_all(&closed.to_be_bytes()).unwrap();
    s.read_exact(&mut resp).unwrap();
    assert_eq!(resp[1], 5, "SOCKS debe informar del rechazo");

    // Remoto (-R): el servidor escucha y nos trae la conexión.
    let reply = roundtrip(
        TcpStream::connect(("127.0.0.1", remote)).unwrap(),
        b"remoto",
    );
    assert_eq!(reply, b"eco:remoto");

    // --- 3. Reenvío X11 ----------------------------------------------------------
    if x11_tools {
        assert!(output.contains("Reenvío X11 activo"), "{output}");
        backend
            .cmd
            .send(TermCmd::Input(
                b"xdpyinfo >/dev/null 2>&1 && echo x11-$((1+1))-bien || echo x11-$((1+1))-mal\n"
                    .to_vec(),
            ))
            .unwrap();
        wait_output(&backend, &mut output, "x11-2-");
        assert!(output.contains("x11-2-bien"), "{output}");
        // La pantalla remota es la que crea sshd, no la local.
        backend
            .cmd
            .send(TermCmd::Input(b"echo pantalla=$DISPLAY=\n".to_vec()))
            .unwrap();
        wait_output(&backend, &mut output, "pantalla=localhost:");
    } else {
        eprintln!("X11: OMITIDO (faltan Xvfb, xauth o xdpyinfo)");
    }

    // --- 4. Al cerrar la shell se cierran los túneles ------------------------
    backend
        .cmd
        .send(TermCmd::Input(b"exit\n".to_vec()))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match backend
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(TermEvent::Closed(e)) => {
                assert!(e.is_none(), "{e:?}");
                break;
            }
            Ok(_) => {}
            Err(_) => panic!("la shell no se cerró"),
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(("127.0.0.1", local)).is_ok() {
        assert!(Instant::now() < deadline, "el túnel local sigue abierto");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn telnet_against_real_server() {
    let (Some(telnetd), Some(socat)) = (find_program("telnetd"), find_program("socat")) else {
        eprintln!("OMITIDA: hace falta telnetd y socat");
        return;
    };
    // socat hace de inetd: cada conexión lanza telnetd con una shell.
    let port = free_port();
    let child = Command::new(socat)
        .arg(format!("TCP-LISTEN:{port},bind=127.0.0.1,reuseaddr,fork"))
        .arg(format!("EXEC:{} -h -E /bin/sh,nofork", telnetd.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _guard = Killed(child);
    wait_port(port);

    let rt = tokio::runtime::Runtime::new().unwrap();
    let backend = godremote::telnet::spawn(
        rt.handle(),
        "127.0.0.1".into(),
        port,
        std::sync::Arc::new(|| {}),
        132,
        43,
    );
    let mut output = String::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match backend
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(TermEvent::Connected(None)) => break,
            Ok(TermEvent::Closed(e)) => panic!("cerrada: {e:?}"),
            Ok(_) => {}
            Err(_) => panic!("no conectó"),
        }
    }
    // El eco y el tamaño de ventana negociado (NAWS) llegan al pty remoto.
    backend
        .cmd
        .send(TermCmd::Input(
            b"echo telnet-$((40+2)); stty size\r".to_vec(),
        ))
        .unwrap();
    wait_output(&backend, &mut output, "telnet-42");
    wait_output(&backend, &mut output, "43 132");
    // El tipo de terminal negociado.
    backend
        .cmd
        .send(TermCmd::Input(b"echo term=$TERM=\r".to_vec()))
        .unwrap();
    wait_output(&backend, &mut output, "term=xterm-256color=");
    // Redimensionar en caliente.
    backend
        .cmd
        .send(TermCmd::Resize { cols: 90, rows: 20 })
        .unwrap();
    backend
        .cmd
        .send(TermCmd::Input(b"stty size\r".to_vec()))
        .unwrap();
    wait_output(&backend, &mut output, "20 90");
    backend
        .cmd
        .send(TermCmd::Input(b"exit\r".to_vec()))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match backend
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(TermEvent::Closed(e)) => {
                assert!(e.is_none(), "{e:?}");
                break;
            }
            Ok(_) => {}
            Err(_) => panic!("no se cerró"),
        }
    }
}
