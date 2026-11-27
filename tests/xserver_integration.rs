//! Servidor X propio contra clientes X reales (xterm, xclock, xeyes…), por
//! TCP en 127.0.0.1. No hace falta Xvfb: el servidor es el nuestro.
//!
//! Cada prueba se omite sola si falta alguno de los programas que usa.
#![cfg(unix)]

mod common;

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Killed, find_program};
use godremote::desktop::{DesktopBackend, DesktopEvent, DesktopInput, KeyCode, MouseButton};
use godremote::xserver::{XServer, XServerConfig};

const WIDTH: u32 = 800;
const HEIGHT: u32 = 600;
const TIMEOUT: Duration = Duration::from_secs(20);

/// Un servidor X en marcha con su pantalla y lo necesario para lanzarle
/// clientes.
struct Fixture {
    rt: tokio::runtime::Runtime,
    server: XServer,
    backend: DesktopBackend,
    display: String,
    dir: tempfile::TempDir,
}

/// `true` (y un aviso) si falta alguno de los programas.
fn missing(programs: &[&str]) -> bool {
    match programs.iter().find(|p| find_program(p).is_none()) {
        Some(p) => {
            eprintln!("OMITIDA: falta {p}");
            true
        }
        None => false,
    }
}

impl Fixture {
    fn new() -> Self {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (server, backend) = XServer::start(
            rt.handle(),
            XServerConfig {
                width: WIDTH,
                height: HEIGHT,
            },
            Arc::new(|| {}),
        );
        match backend.events.recv_timeout(TIMEOUT) {
            Ok(DesktopEvent::Connected) => {}
            _ => panic!("el servidor X no arrancó"),
        }
        let number = rt.block_on(server.listen_tcp(None)).expect("puerto X");
        Fixture {
            rt,
            server,
            backend,
            display: format!("127.0.0.1:{number}"),
            dir: tempfile::tempdir().unwrap(),
        }
    }

    /// Orden con el entorno de un cliente X de este servidor: sin fichero
    /// de autorización (no manda cookie) ni recursos del usuario.
    fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.env("DISPLAY", &self.display)
            .env("XAUTHORITY", self.dir.path().join("no-existe"))
            .env("HOME", self.dir.path())
            .env("LC_ALL", "C")
            .env_remove("XENVIRONMENT")
            .env_remove("WAYLAND_DISPLAY")
            .stdin(Stdio::null());
        cmd
    }

    /// Lanza un cliente que se queda en marcha; su salida de errores va a
    /// un fichero para poder enseñarla si algo falla.
    fn spawn(&self, program: &str, args: &[&str]) -> Client {
        self.spawn_env(program, program, args, &[])
    }

    /// Como `spawn`, con variables de entorno añadidas; `name` da nombre a
    /// los ficheros de salida.
    fn spawn_env(&self, name: &str, program: &str, args: &[&str], env: &[(&str, &str)]) -> Client {
        let log = self.dir.path().join(format!("{name}.log"));
        let out = self.dir.path().join(format!("{name}.out"));
        let child = self
            .command(program)
            .args(args)
            .envs(env.iter().copied())
            .stdout(std::fs::File::create(&out).unwrap())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap_or_else(|e| panic!("{program}: {e}"));
        Client {
            process: Killed(child),
            log,
            out,
        }
    }

    /// Ejecuta una herramienta de consulta hasta que termina.
    fn run(&self, program: &str, args: &[&str]) -> Output {
        self.command(program)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("{program}: {e}"))
    }

    fn stdout(&self, program: &str, args: &[&str]) -> String {
        let out = self.run(program, args);
        assert!(
            out.status.success(),
            "{program} {args:?} falló: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn send(&self, input: DesktopInput) {
        self.backend.input.send(input).unwrap();
    }

    fn click(&self, x: i32, y: i32) {
        let (x, y) = (x as u16, y as u16);
        self.send(DesktopInput::PointerMove { x, y });
        for pressed in [true, false] {
            self.send(DesktopInput::PointerButton {
                button: MouseButton::Left,
                pressed,
                x,
                y,
            });
        }
    }

    /// Pulsa y suelta una tecla.
    fn key(&self, key: KeyCode) {
        for pressed in [true, false] {
            self.send(DesktopInput::Key { key, pressed });
        }
    }

    /// Arrastra con el botón izquierdo de un punto a otro, en varios pasos.
    fn drag(&self, from: (i32, i32), to: (i32, i32)) {
        let point = |p: (i32, i32)| (p.0.max(0) as u16, p.1.max(0) as u16);
        let (x, y) = point(from);
        self.send(DesktopInput::PointerMove { x, y });
        self.send(DesktopInput::PointerButton {
            button: MouseButton::Left,
            pressed: true,
            x,
            y,
        });
        for step in 1..=8 {
            let (x, y) = point((
                from.0 + (to.0 - from.0) * step / 8,
                from.1 + (to.1 - from.1) * step / 8,
            ));
            self.send(DesktopInput::PointerMove { x, y });
        }
        let (x, y) = point(to);
        self.send(DesktopInput::PointerButton {
            button: MouseButton::Left,
            pressed: false,
            x,
            y,
        });
    }

    fn pixel(&self, x: i32, y: i32) -> [u8; 3] {
        let fb = self.backend.frame.lock().unwrap();
        let i = ((y as u32 * fb.width() + x as u32) * 4) as usize;
        [fb.pixels[i], fb.pixels[i + 1], fb.pixels[i + 2]]
    }

    /// Número de colores distintos en una zona de la pantalla.
    fn colors_in(&self, g: Geometry) -> usize {
        let fb = self.backend.frame.lock().unwrap();
        let mut seen = std::collections::HashSet::new();
        for y in g.y.max(0)..(g.y + g.h).min(fb.height() as i32) {
            for x in g.x.max(0)..(g.x + g.w).min(fb.width() as i32) {
                let i = ((y as u32 * fb.width() + x as u32) * 4) as usize;
                seen.insert([fb.pixels[i], fb.pixels[i + 1], fb.pixels[i + 2]]);
            }
        }
        seen.len()
    }

    /// Resumen del contenido de una zona de la pantalla (para saber si ha
    /// cambiado).
    fn checksum(&self, g: Geometry) -> u64 {
        let fb = self.backend.frame.lock().unwrap();
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for y in g.y.max(0)..(g.y + g.h).min(fb.height() as i32) {
            for x in g.x.max(0)..(g.x + g.w).min(fb.width() as i32) {
                let i = ((y as u32 * fb.width() + x as u32) * 4) as usize;
                for byte in &fb.pixels[i..i + 3] {
                    hash = (hash ^ *byte as u64).wrapping_mul(0x0100_0000_01b3);
                }
            }
        }
        hash
    }

    /// Guarda la pantalla en PNG si se pide con `GODREMOTE_X11_PNG=carpeta`
    /// (para mirar a ojo qué se está pintando).
    fn snapshot(&self, name: &str) {
        let Some(dir) = std::env::var_os("GODREMOTE_X11_PNG") else {
            return;
        };
        let fb = self.backend.frame.lock().unwrap();
        let path = PathBuf::from(dir).join(format!("{name}.png"));
        image::save_buffer(
            &path,
            &fb.pixels,
            fb.width(),
            fb.height(),
            image::ColorType::Rgba8,
        )
        .unwrap();
        eprintln!("pantalla guardada en {}", path.display());
    }

    /// Identificadores (`0x…`) de las ventanas de primer nivel cuyo nombre
    /// o clase, tal como los lista `xwininfo`, contienen el texto.
    fn find_windows(&self, text: &str) -> Vec<String> {
        let out = self.run("xwininfo", &["-root", "-children"]);
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim_start)
            .filter(|l| l.starts_with("0x") && l.contains(text))
            .filter_map(|l| l.split_whitespace().next())
            .map(str::to_string)
            .collect()
    }

    fn find_window(&self, text: &str) -> Option<String> {
        self.find_windows(text).into_iter().next()
    }

    /// Espera a que exista una ventana visible con esa clase (o nombre) y
    /// devuelve su identificador y su posición en pantalla.
    fn wait_window(&self, client: &mut Client, class: &str) -> (String, Geometry) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            // (GTK crea además una ventana «líder» con el mismo nombre que
            // nunca se muestra.)
            for id in self.find_windows(&format!("\"{class}\"")) {
                if let Some(g) = self.geometry(&id)
                    && g.viewable
                {
                    return (id, g);
                }
            }
            client.assert_running();
            assert!(
                Instant::now() < deadline,
                "no apareció la ventana de {class}: {}",
                client.errors()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Geometría absoluta de una ventana según `xwininfo`.
    fn geometry(&self, id: &str) -> Option<Geometry> {
        let out = self.run("xwininfo", &["-id", id]);
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let field = |label: &str| -> Option<i32> {
            let line = text.lines().find(|l| l.trim_start().starts_with(label))?;
            line.rsplit(':').next()?.trim().parse().ok()
        };
        Some(Geometry {
            x: field("Absolute upper-left X")?,
            y: field("Absolute upper-left Y")?,
            w: field("Width")?,
            h: field("Height")?,
            viewable: text.contains("IsViewable"),
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.stop();
    }
}

#[derive(Debug, Clone, Copy)]
struct Geometry {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    viewable: bool,
}

/// Cliente X lanzado por la prueba; se mata al salir del ámbito.
struct Client {
    process: Killed,
    log: PathBuf,
    out: PathBuf,
}

impl Client {
    fn errors(&self) -> String {
        String::from_utf8_lossy(&std::fs::read(&self.log).unwrap_or_default()).into_owned()
    }

    /// Salida estándar hasta ahora (xev escribe bytes Latin-1 sueltos).
    fn output(&self) -> String {
        String::from_utf8_lossy(&std::fs::read(&self.out).unwrap_or_default()).into_owned()
    }

    fn assert_running(&mut self) {
        if let Ok(Some(status)) = self.process.0.try_wait() {
            panic!("el cliente terminó ({status}): {}", self.errors());
        }
    }

    /// Espera a que el proceso termine por sí mismo.
    fn wait_exit(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Ok(Some(status)) = self.process.0.try_wait() {
                return status;
            }
            assert!(Instant::now() < deadline, "el cliente no terminó");
            std::thread::sleep(Duration::from_millis(30));
        }
    }
}

/// Espera, con plazo, a que se cumpla una condición.
fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !condition() {
        assert!(Instant::now() < deadline, "tiempo agotado: {what}");
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Exploración manual: lanza los clientes de `EXPLORE` (separados por
/// comas) y guarda la pantalla en `GODREMOTE_X11_PNG`.
///
///   EXPLORE="xclock,xterm -fn 9x15" GODREMOTE_X11_PNG=/tmp \
///       cargo test --test xserver_integration explore -- --ignored --nocapture
#[test]
#[ignore]
fn explore() {
    let fx = Fixture::new();
    eprintln!("DISPLAY={}", fx.display);
    // `EXPLORE_SIZE=1400x900` cambia el tamaño de la pantalla.
    if let Ok(size) = std::env::var("EXPLORE_SIZE")
        && let Some((w, h)) = size.split_once('x')
    {
        fx.server.resize(w.parse().unwrap(), h.parse().unwrap());
    }
    let which = std::env::var("EXPLORE").unwrap_or_else(|_| "xlogo".into());
    let mut clients = vec![];
    for program in which.split(',') {
        let mut parts = program.split(' ');
        let name = parts.next().unwrap();
        let args: Vec<&str> = parts.collect();
        clients.push((name.to_string(), fx.spawn(name, &args)));
        std::thread::sleep(Duration::from_millis(1500));
    }
    if let Ok(text) = std::env::var("EXPLORE_TEXT") {
        fx.send(DesktopInput::Text(text));
    }
    // `EXPLORE_CLICKS="700,50;130,80"`: clics, con una captura tras cada uno.
    if let Ok(clicks) = std::env::var("EXPLORE_CLICKS") {
        for (i, click) in clicks.split(';').enumerate() {
            let (x, y) = click.split_once(',').unwrap();
            std::thread::sleep(Duration::from_millis(1500));
            fx.click(x.parse().unwrap(), y.parse().unwrap());
            std::thread::sleep(Duration::from_millis(1500));
            fx.snapshot(&format!("clic{}", i + 1));
        }
    }
    // `EXPLORE_WAIT=30` deja tiempo para lanzar más cosas a mano.
    let wait = std::env::var("EXPLORE_WAIT")
        .ok()
        .and_then(|w| w.parse().ok())
        .unwrap_or(1);
    std::thread::sleep(Duration::from_secs(wait));
    fx.snapshot("explore");
    eprintln!("{}", fx.stdout("xwininfo", &["-root", "-tree"]));
    for (name, client) in &mut clients {
        let status = client.process.0.try_wait();
        eprintln!("--- {name}: {status:?}\n{}", client.errors());
    }
}

#[test]
fn query_tools_see_the_server_and_its_windows() {
    if missing(&["xdpyinfo", "xprop", "xwininfo", "xlogo"]) {
        return;
    }
    let fx = Fixture::new();

    // --- El servidor, visto por xdpyinfo ------------------------------------
    let info = fx.stdout("xdpyinfo", &[]);
    for expected in [
        "vendor string:    GodRemote",
        "dimensions:    800x600 pixels",
        "class:    TrueColor",
        "depth of root window:    24 planes",
        "BIG-REQUESTS",
    ] {
        assert!(info.contains(expected), "falta «{expected}» en:\n{info}");
    }

    // --- Propiedades de la raíz ---------------------------------------------
    // De entrada solo están las del gestor de ventanas integrado (EWMH).
    let props = fx.stdout("xprop", &["-root"]);
    assert!(
        props.contains("_NET_SUPPORTING_WM_CHECK(WINDOW)"),
        "{props}"
    );
    assert!(
        props.contains("_NET_WORKAREA(CARDINAL) = 0, 0, 800, 600"),
        "{props}"
    );
    assert!(!props.contains("PRUEBA"), "{props}");
    fx.stdout(
        "xprop",
        &["-root", "-f", "PRUEBA", "8s", "-set", "PRUEBA", "hola"],
    );
    let props = fx.stdout("xprop", &["-root"]);
    assert!(props.contains("PRUEBA(STRING) = \"hola\""), "{props}");
    fx.stdout("xprop", &["-root", "-remove", "PRUEBA"]);
    assert!(!fx.stdout("xprop", &["-root"]).contains("PRUEBA"));

    // --- Una ventana de verdad ----------------------------------------------
    let tree = fx.stdout("xwininfo", &["-root", "-tree"]);
    assert!(tree.contains("0 children"), "{tree}");
    let mut logo = fx.spawn(
        "xlogo",
        &["-geometry", "120x90+200+150", "-title", "Logotipo"],
    );
    let (id, g) = fx.wait_window(&mut logo, "XLogo");
    // El tamaño y la posición pedidos se respetan.
    assert_eq!((g.x, g.y, g.w, g.h), (200, 150, 120, 90));
    let tree = fx.stdout("xwininfo", &["-root", "-tree"]);
    assert!(tree.contains("1 child"), "{tree}");
    assert!(
        tree.contains("\"Logotipo\": (\"xlogo\" \"XLogo\")  120x90+200+150"),
        "{tree}"
    );
    let props = fx.stdout("xprop", &["-id", &id]);
    for expected in [
        "WM_NAME(STRING) = \"Logotipo\"",
        "WM_CLASS(STRING) = \"xlogo\", \"XLogo\"",
        "WM_DELETE_WINDOW",
    ] {
        assert!(props.contains(expected), "falta «{expected}» en:\n{props}");
    }
    assert_eq!(fx.find_window("\"Logotipo\""), Some(id.clone()));
    assert_eq!(fx.find_window("no-existe"), None);
    // Por nombre, como lo buscaría un usuario.
    let by_name = fx.stdout("xwininfo", &["-name", "Logotipo"]);
    assert!(by_name.contains(&format!("Window id: {id}")), "{by_name}");
    // La ventana nueva tiene el foco del teclado.
    let info = fx.stdout("xdpyinfo", &[]);
    assert!(info.contains(&format!("focus:  window {id}")), "{info}");
    assert_eq!(fx.server.client_count(), 1);

    // --- Fuentes --------------------------------------------------------------
    if find_program("xlsfonts").is_some() {
        let fonts = fx.stdout("xlsfonts", &["-fn", "-*-fixed-medium-r-*-*-13-*"]);
        assert!(
            fonts
                .lines()
                .any(|l| l.starts_with("-misc-fixed-medium-r-") && l.ends_with("-iso8859-1")),
            "{fonts}"
        );
        // Con -l pide las métricas de cada una (ListFontsWithInfo).
        let long = fx.stdout("xlsfonts", &["-l", "-fn", "9x15"]);
        assert!(long.contains("9x15"), "{long}");
    }
    logo.assert_running();
    assert!(!logo.errors().contains("X Error"), "{}", logo.errors());
    fx.snapshot("consultas");
}

#[test]
fn xterm_shows_its_background_and_receives_typing() {
    if missing(&["xterm", "xwininfo", "xdpyinfo"]) {
        return;
    }
    let fx = Fixture::new();
    let typed = fx.dir.path().join("tecleado.txt");
    // Antes de leer del teclado, el terminal escribe más líneas de las que
    // caben: tiene que desplazar su contenido (CopyArea + NoExpose).
    let script = format!("seq 1 100; cat > {}", typed.display());
    let mut xterm = fx.spawn(
        "xterm",
        &[
            "-geometry",
            "80x24+40+60",
            "-bg",
            "#204080",
            "-fg",
            "white",
            "-e",
            "sh",
            "-c",
            &script,
        ],
    );
    let (id, g) = fx.wait_window(&mut xterm, "XTerm");
    // 80×24 celdas de la fuente «fixed» (más el borde interior).
    assert!(g.w > 80 * 5 && g.h > 24 * 10, "{g:?}");
    // El fondo pedido aparece en la pantalla (lejos del texto).
    let corner = (g.x + g.w - 30, g.y + 8);
    wait_for("fondo del xterm", || {
        fx.pixel(corner.0, corner.1) == [0x20, 0x40, 0x80]
    });
    // Y el texto (los números de `seq`) también: hay más de un color.
    let text_zone = Geometry { w: 60, ..g };
    wait_for("texto en el xterm", || fx.colors_in(text_zone) > 2);
    let info = fx.stdout("xdpyinfo", &[]);
    assert!(info.contains(&format!("focus:  window {id}")), "{info}");
    // Cuando `seq` termina la pantalla se queda quieta.
    let mut last = fx.checksum(g);
    let mut stable = Instant::now();
    wait_for("fin de la salida de seq", || {
        let now = fx.checksum(g);
        if now != last {
            (last, stable) = (now, Instant::now());
        }
        stable.elapsed() > Duration::from_millis(400)
    });

    // Portapapeles hacia el xterm: lo que ofrece la interfaz se pega con
    // el botón central (selección PRIMARY) y aparece en la línea en curso.
    // (El texto empieza tras el borde de 1 y el margen de 2 píxeles; las
    // celdas son de 7×13 y el cursor está en la última de las 24 filas.)
    let paste_zone = Geometry {
        x: g.x + 3 + 7,
        y: g.y + 3 + 23 * 13,
        w: 4 * 7,
        h: 13,
        viewable: true,
    };
    assert_eq!(
        fx.colors_in(paste_zone),
        1,
        "la línea en curso no está vacía"
    );
    let inside = ((g.x + 150) as u16, (g.y + 100) as u16);
    fx.send(DesktopInput::ClipboardText("pegado ".into()));
    for pressed in [true, false] {
        fx.send(DesktopInput::PointerButton {
            button: MouseButton::Middle,
            pressed,
            x: inside.0,
            y: inside.1,
        });
    }
    wait_for("texto pegado en pantalla", || fx.colors_in(paste_zone) > 1);
    // Y del xterm a la interfaz: un doble clic selecciona una palabra (el
    // «78» de la primera línea) y llega como portapapeles.
    let word = ((g.x + 1 + 2 + 5) as u16, (g.y + 1 + 2 + 6) as u16);
    for pressed in [true, false, true, false] {
        fx.send(DesktopInput::PointerButton {
            button: MouseButton::Left,
            pressed,
            x: word.0,
            y: word.1,
        });
    }
    let selected = loop {
        match fx.backend.events.recv_timeout(TIMEOUT) {
            Ok(DesktopEvent::ClipboardText(text)) => break text,
            Ok(_) => {}
            Err(_) => panic!("no llegó la selección del xterm"),
        }
    };
    assert_eq!(selected, "78");

    // Texto, teclas sueltas y una combinación con Ctrl.
    fx.send(DesktopInput::Text("Hola Mundo 123 (x+y)*2=?".into()));
    for (key, pressed) in [
        (KeyCode::Enter, true),
        (KeyCode::Enter, false),
        (KeyCode::Letter(b'x'), true),
        (KeyCode::Letter(b'x'), false),
        (KeyCode::Backspace, true),
        (KeyCode::Backspace, false),
        // Mayús + letra como tecla (un atajo) da la mayúscula.
        (KeyCode::Shift, true),
        (KeyCode::Letter(b'z'), true),
        (KeyCode::Letter(b'z'), false),
        (KeyCode::Shift, false),
        (KeyCode::Enter, true),
        (KeyCode::Enter, false),
        // Ctrl+D: fin de fichero para `cat`.
        (KeyCode::Control, true),
        (KeyCode::Letter(b'd'), true),
        (KeyCode::Letter(b'd'), false),
        (KeyCode::Control, false),
    ] {
        fx.send(DesktopInput::Key { key, pressed });
    }
    let expected = "pegado Hola Mundo 123 (x+y)*2=?\nZ\n";
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let text = std::fs::read_to_string(&typed).unwrap_or_default();
        if text == expected {
            break;
        }
        assert!(Instant::now() < deadline, "tecleado: {text:?}");
        std::thread::sleep(Duration::from_millis(30));
    }
    fx.snapshot("xterm");
    // Con el fin de fichero termina `cat`, y con él el xterm.
    let status = xterm.wait_exit();
    assert!(status.success(), "{status}: {}", xterm.errors());
    assert!(!xterm.errors().contains("X Error"), "{}", xterm.errors());
    wait_for("baja del cliente", || fx.server.client_count() == 0);
    // Su ventana ya no está en pantalla.
    wait_for("pantalla limpia", || {
        fx.pixel(corner.0, corner.1) != [0x20, 0x40, 0x80]
    });
}

#[test]
fn classic_clients_start_and_draw() {
    if missing(&["xwininfo"]) {
        return;
    }
    let fx = Fixture::new();
    let clients: [(&str, &str, &[&str]); 6] = [
        ("xclock", "XClock", &[]),
        ("xeyes", "XEyes", &[]),
        ("xcalc", "XCalc", &[]),
        ("xlogo", "XLogo", &[]),
        ("xmessage", "Xmessage", &["Hola desde GodRemote"]),
        ("xfontsel", "XFontSel", &[]),
    ];
    // De uno en uno: cada cual pinta algo en su ventana y sigue vivo.
    for (program, class, args) in clients {
        if find_program(program).is_none() {
            eprintln!("OMITIDO: falta {program}");
            continue;
        }
        let mut client = fx.spawn(program, args);
        let (_, g) = fx.wait_window(&mut client, class);
        wait_for(&format!("{program} pinta algo"), || fx.colors_in(g) >= 2);
        // Un momento para que termine de dibujar (y de equivocarse, si se
        // equivoca: los errores X llegan de forma asíncrona).
        std::thread::sleep(Duration::from_millis(300));
        fx.snapshot(program);
        client.assert_running();
        assert!(
            !client.errors().contains("X Error"),
            "{program}: {}",
            client.errors()
        );
        drop(client);
        wait_for("baja del cliente", || fx.server.client_count() == 0);
        // Al irse el cliente, su zona vuelve a ser fondo liso.
        wait_for("pantalla limpia", || fx.colors_in(g) == 1);
    }
    // Todos a la vez, solapados.
    let mut running = vec![];
    for (program, class, args) in clients {
        if find_program(program).is_some() {
            let mut client = fx.spawn(program, args);
            fx.wait_window(&mut client, class);
            running.push((program, client));
        }
    }
    let count = running.len();
    wait_for("todos conectados", || fx.server.client_count() == count);
    std::thread::sleep(Duration::from_millis(300));
    fx.snapshot("todos");
    let tree = fx.stdout("xwininfo", &["-root", "-children"]);
    assert!(tree.contains(&format!("{count} children")), "{tree}");
    for (program, client) in &mut running {
        client.assert_running();
        assert!(
            !client.errors().contains("X Error"),
            "{program}: {}",
            client.errors()
        );
    }
}

/// Ventanas hoja de un árbol de `xwininfo -tree`, con su posición absoluta.
fn leaf_windows(tree: &str) -> Vec<Geometry> {
    let mut out = vec![];
    let lines: Vec<&str> = tree.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        // «0x1200021 (has no name): ()  29x17+4+29  +116+160»
        if !line.trim_start().starts_with("0x") {
            continue;
        }
        let mut fields = line.split_whitespace().rev();
        let (Some(abs), Some(size)) = (fields.next(), fields.next()) else {
            continue;
        };
        let has_children = lines.get(i + 1).is_some_and(|next| next.contains("child"));
        if has_children {
            continue;
        }
        let numbers = |text: &str| -> Vec<i32> {
            text.replace(['x', '+'], " ")
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect()
        };
        let (size, abs) = (numbers(size), numbers(abs));
        if size.len() >= 2 && abs.len() == 2 {
            out.push(Geometry {
                x: abs[0],
                y: abs[1],
                w: size[0],
                h: size[1],
                viewable: true,
            });
        }
    }
    out
}

#[test]
fn mouse_and_keyboard_events_reach_clients() {
    if missing(&["xmessage", "xev", "xwininfo"]) {
        return;
    }
    let fx = Fixture::new();

    // --- Un clic en el botón de xmessage lo cierra con su código ------------
    let mut message = fx.spawn("xmessage", &["-buttons", "Vale:7", "Pulsa el botón"]);
    let (id, _) = fx.wait_window(&mut message, "Xmessage");
    let tree = fx.stdout("xwininfo", &["-id", &id, "-tree"]);
    // El botón es la ventana hoja que queda más abajo.
    let button = leaf_windows(&tree)
        .into_iter()
        .max_by_key(|g| g.y)
        .unwrap_or_else(|| panic!("sin ventanas hijas:\n{tree}"));
    fx.click(button.x + button.w / 2, button.y + button.h / 2);
    let status = message.wait_exit();
    assert_eq!(status.code(), Some(7), "{}", message.errors());

    // --- xev cuenta lo que le llega -------------------------------------------
    let mut xev = fx.spawn("xev", &["-geometry", "200x150+300+200"]);
    let id = {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(id) = fx.find_window("\"Event Tester\"") {
                break id;
            }
            xev.assert_running();
            assert!(Instant::now() < deadline, "xev no abrió su ventana");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let g = {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match fx.geometry(&id) {
                Some(g) if g.viewable => break g,
                _ => assert!(Instant::now() < deadline, "xev no se mostró"),
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    // Entra el puntero, se mueve, clic izquierdo, rueda y teclas.
    let (cx, cy) = (g.x + 150, g.y + 100);
    fx.send(DesktopInput::PointerMove {
        x: (cx - 20) as u16,
        y: cy as u16,
    });
    fx.click(cx, cy);
    fx.send(DesktopInput::Wheel {
        x: cx as u16,
        y: cy as u16,
        delta: -1,
        horizontal: false,
    });
    fx.send(DesktopInput::Text("aÑ€".into()));
    for pressed in [true, false] {
        fx.send(DesktopInput::Key {
            key: KeyCode::F(5),
            pressed,
        });
    }
    let seen = |xev: &Client, what: &str| xev.output().contains(what);
    let expected = [
        "EnterNotify event",
        "MotionNotify event",
        "ButtonPress event",
        "ButtonRelease event",
        "state 0x100, button 1",
        "button 5",
        "KeyPress event",
        "KeyRelease event",
        "(keysym 0x61, a)",
        "(keysym 0xd1, Ntilde)",
        "(keysym 0xffc2, F5)",
        "XLookupString gives 1 bytes: (61) \"a\"",
        "FocusIn event",
        "Expose event",
        "MappingNotify event",
    ];
    // La salida de xev puede ir con búfer: si pasado un rato no ha aparecido
    // todo, se le cierra la ventana y termina volcándola.
    let frame_close = (g.x + g.w + 3 - 12, g.y - 12);
    let started = Instant::now();
    let mut closed = false;
    loop {
        if expected.iter().all(|e| seen(&xev, e)) {
            break;
        }
        if !closed && started.elapsed() > Duration::from_secs(3) {
            fx.click(frame_close.0, frame_close.1);
            closed = true;
        }
        if started.elapsed() > TIMEOUT {
            let missing: Vec<&&str> = expected.iter().filter(|e| !seen(&xev, e)).collect();
            panic!(
                "xev no vio {missing:?}:\n{}\n{}",
                xev.output(),
                xev.errors()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // El euro no cabe en el mapa fijo: llega tras reasignar una tecla.
    assert!(
        seen(&xev, "EuroSign") || seen(&xev, "0x10020ac") || seen(&xev, "0x20ac"),
        "{}",
        xev.output()
    );
}

#[test]
fn built_in_window_manager_moves_and_closes_windows() {
    if missing(&["xclock", "xwininfo"]) {
        return;
    }
    let fx = Fixture::new();
    let mut clock = fx.spawn("xclock", &["-geometry", "150x150+100+100"]);
    let (id, g) = fx.wait_window(&mut clock, "XClock");
    // `xwininfo` da la esquina exterior; Xt pone un borde de un píxel a
    // cada lado de sus ventanas de primer nivel.
    let border = 2;

    // La barra de título está encima de la ventana y lleva su nombre.
    let title_y = g.y - 11;
    let bar = fx.pixel(g.x + g.w / 2, title_y);
    assert_ne!(bar, fx.pixel(700, 500), "no hay barra de título");
    let title = Geometry {
        y: g.y - 22,
        h: 22,
        ..g
    };
    assert!(fx.colors_in(title) > 3, "barra de título sin texto");

    // Arrastrar por la barra mueve la ventana.
    let grab = (g.x + 40, title_y);
    fx.send(DesktopInput::PointerMove {
        x: grab.0 as u16,
        y: grab.1 as u16,
    });
    fx.send(DesktopInput::PointerButton {
        button: MouseButton::Left,
        pressed: true,
        x: grab.0 as u16,
        y: grab.1 as u16,
    });
    for step in 1..=10 {
        fx.send(DesktopInput::PointerMove {
            x: (grab.0 + step * 12) as u16,
            y: (grab.1 + step * 7) as u16,
        });
    }
    fx.send(DesktopInput::PointerButton {
        button: MouseButton::Left,
        pressed: false,
        x: (grab.0 + 120) as u16,
        y: (grab.1 + 70) as u16,
    });
    wait_for("ventana movida", || {
        fx.geometry(&id)
            .is_some_and(|n| (n.x, n.y, n.w, n.h) == (g.x + 120, g.y + 70, g.w, g.h))
    });
    // Donde estaba ya solo queda el fondo, y el reloj se ve en su sitio nuevo.
    wait_for("zona antigua limpia", || {
        fx.colors_in(Geometry { w: 100, h: 60, ..g }) == 1
    });
    let moved = fx.geometry(&id).unwrap();
    wait_for("reloj en su sitio nuevo", || fx.colors_in(moved) >= 2);
    fx.snapshot("movida");

    // Tirar de la esquina inferior derecha del marco cambia el tamaño.
    let corner = (
        moved.x + moved.w + border + 1,
        moved.y + moved.h + border + 1,
    );
    fx.send(DesktopInput::PointerMove {
        x: corner.0 as u16,
        y: corner.1 as u16,
    });
    fx.send(DesktopInput::PointerButton {
        button: MouseButton::Left,
        pressed: true,
        x: corner.0 as u16,
        y: corner.1 as u16,
    });
    fx.send(DesktopInput::PointerMove {
        x: (corner.0 + 60) as u16,
        y: (corner.1 + 40) as u16,
    });
    fx.send(DesktopInput::PointerButton {
        button: MouseButton::Left,
        pressed: false,
        x: (corner.0 + 60) as u16,
        y: (corner.1 + 40) as u16,
    });
    wait_for("ventana redimensionada", || {
        fx.geometry(&id)
            .is_some_and(|n| (n.w, n.h) == (moved.w + 60, moved.h + 40))
    });
    let resized = fx.geometry(&id).unwrap();
    assert_eq!((resized.x, resized.y), (moved.x, moved.y));
    fx.snapshot("redimensionada");

    // El botón de cerrar pide a la aplicación que termine (WM_DELETE_WINDOW).
    clock.assert_running();
    let close = (resized.x + resized.w + border + 3 - 12, resized.y - 11);
    fx.click(close.0, close.1);
    let status = clock.wait_exit();
    assert!(status.success(), "{status}: {}", clock.errors());
    wait_for("baja del cliente", || fx.server.client_count() == 0);
    wait_for("pantalla limpia", || fx.colors_in(resized) == 1);
}

#[test]
fn cookie_is_required_when_configured() {
    if missing(&["xdpyinfo"]) {
        return;
    }
    let fx = Fixture::new();
    // Otro puerto del mismo servidor, que exige una cookie.
    let cookie: [u8; 16] = [
        0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc,
        0xfe,
    ];
    let (listener, number) = (700u16..900)
        .find_map(|n| {
            fx.rt
                .block_on(tokio::net::TcpListener::bind(("127.0.0.1", 6000 + n)))
                .ok()
                .map(|l| (l, n))
        })
        .expect("puerto libre");
    let server = fx.server.clone();
    fx.rt.spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let server = server.clone();
            tokio::spawn(async move {
                let _ = server.serve(socket, Some(cookie)).await;
            });
        }
    });
    let display = format!("127.0.0.1:{number}");
    let query = |authority: &std::path::Path| {
        fx.command("xdpyinfo")
            .env("DISPLAY", &display)
            .env("XAUTHORITY", authority)
            .output()
            .unwrap()
    };

    // Sin cookie: rechazado, y no queda como cliente.
    let out = query(&fx.dir.path().join("no-existe"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "se aceptó a un cliente sin cookie");
    assert!(stderr.contains("unable to open display"), "{stderr}");
    assert_eq!(fx.server.client_count(), 0);
    // El puerto sin cookie sigue aceptando.
    fx.stdout("xdpyinfo", &[]);

    if find_program("xauth").is_none() {
        eprintln!("OMITIDO: falta xauth para probar la cookie correcta");
        return;
    }
    let hostname =
        String::from_utf8_lossy(&Command::new("uname").arg("-n").output().unwrap().stdout)
            .trim()
            .to_string();
    let add = |file: &std::path::Path, hex: &str| {
        let status = Command::new("xauth")
            .arg("-f")
            .arg(file)
            .args(["add", &format!("{hostname}/unix:{number}")])
            .args(["MIT-MAGIC-COOKIE-1", hex])
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    };
    // Con una cookie equivocada: rechazado.
    let wrong = fx.dir.path().join("mala");
    add(&wrong, "00112233445566778899aabbccddeeff");
    assert!(
        !query(&wrong).status.success(),
        "cookie equivocada aceptada"
    );
    // Con la buena: aceptado.
    let right = fx.dir.path().join("buena");
    add(&right, "0123456789abcdef1032547698badcfe");
    let out = query(&right);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("GodRemote"));
}

#[test]
fn screen_can_be_resized_and_stopped() {
    if missing(&["xwininfo", "xlogo", "xdpyinfo"]) {
        return;
    }
    let fx = Fixture::new();
    let mut logo = fx.spawn("xlogo", &["-geometry", "100x100+600+400"]);
    let (id, before) = fx.wait_window(&mut logo, "XLogo");
    let epoch = fx.backend.frame.lock().unwrap().epoch();

    fx.server.resize(500, 320);
    {
        let fb = fx.backend.frame.lock().unwrap();
        assert_eq!((fb.width(), fb.height()), (500, 320));
        assert!(fb.epoch() > epoch);
        assert_eq!(fb.pixels.len(), 500 * 320 * 4);
    }
    let root = fx.stdout("xwininfo", &["-root"]);
    assert!(
        root.contains("Width: 500") && root.contains("Height: 320"),
        "{root}"
    );
    // La ventana que quedaba fuera se trae a la vista.
    wait_for("ventana recolocada", || {
        fx.geometry(&id)
            .is_some_and(|g| g.x < 500 && g.y < 320 && (g.x, g.y) != (before.x, before.y))
    });
    let g = fx.geometry(&id).unwrap();
    wait_for("logotipo visible", || {
        fx.colors_in(Geometry {
            w: g.w.min(500 - g.x),
            h: g.h.min(320 - g.y),
            ..g
        }) >= 2
    });

    // Al parar el servidor los clientes pierden la conexión y terminan.
    fx.send(DesktopInput::Close);
    let status = logo.wait_exit();
    assert!(!status.success());
    wait_for("servidor parado", || fx.server.client_count() == 0);
    let closed = (0..50).any(|_| {
        matches!(
            fx.backend.events.recv_timeout(Duration::from_millis(200)),
            Ok(DesktopEvent::Closed(None))
        )
    });
    assert!(closed, "no llegó el aviso de cierre");
    // Ya no acepta clientes ni se le puede poner a escuchar. (No se prueba
    // contra el puerto: otra prueba puede haberlo ocupado ya.)
    assert!(fx.rt.block_on(fx.server.listen_tcp(None)).is_err());
    let (_local, remote) = tokio::io::duplex(1024);
    assert!(fx.rt.block_on(fx.server.serve(remote, None)).is_err());
}

/// `xdotool` se cae si el servidor no tiene XKB, así que solo funciona con
/// el XKEYBOARD mínimo activado; con él, el teclado de las aplicaciones Xlib
/// sigue funcionando igual.
#[test]
fn xdotool_works_with_the_xkb_compatibility_switch() {
    if missing(&["xdotool", "xterm", "xwininfo"]) {
        return;
    }
    let fx = Fixture::new();
    fx.server.set_xkb_compat(true);
    let typed = fx.dir.path().join("tecleado.txt");
    let script = format!("cat > {}", typed.display());
    let mut xterm = fx.spawn("xterm", &["-title", "Prueba", "-e", "sh", "-c", &script]);
    let (id, g) = fx.wait_window(&mut xterm, "XTerm");
    // xdotool da los identificadores en decimal.
    let decimal = u32::from_str_radix(id.trim_start_matches("0x"), 16)
        .unwrap()
        .to_string();
    let search = |kind: &str, pattern: &str| {
        fx.stdout("xdotool", &["search", kind, pattern])
            .trim()
            .to_string()
    };
    assert_eq!(search("--class", "XTerm"), decimal);
    assert_eq!(search("--name", "Prueba"), decimal);
    assert!(
        !fx.run("xdotool", &["search", "--class", "NoExiste"])
            .status
            .success()
    );
    assert_eq!(fx.stdout("xdotool", &["getwindowfocus"]).trim(), decimal);
    assert_eq!(
        fx.stdout("xdotool", &["getwindowname", &decimal]).trim(),
        "Prueba"
    );
    let geometry = fx.stdout("xdotool", &["getwindowgeometry", &decimal]);
    assert!(
        geometry.contains(&format!("Geometry: {}x{}", g.w, g.h)),
        "{geometry}"
    );

    // El puntero, donde lo deja la interfaz.
    fx.send(DesktopInput::PointerMove { x: 321, y: 123 });
    wait_for("posición del puntero", || {
        fx.stdout("xdotool", &["getmouselocation"])
            .contains("x:321 y:123")
    });
    fx.send(DesktopInput::PointerMove {
        x: (g.x + 10) as u16,
        y: (g.y + 10) as u16,
    });
    wait_for("ventana bajo el puntero", || {
        fx.stdout("xdotool", &["getmouselocation", "--shell"])
            .contains(&format!("WINDOW={decimal}"))
    });

    // El xterm, que también ve el XKB mínimo, sigue recibiendo bien el teclado.
    fx.send(DesktopInput::Text("Con XKB: aB1!".into()));
    for (key, pressed) in [
        (KeyCode::Enter, true),
        (KeyCode::Enter, false),
        (KeyCode::Control, true),
        (KeyCode::Letter(b'd'), true),
        (KeyCode::Letter(b'd'), false),
        (KeyCode::Control, false),
    ] {
        fx.send(DesktopInput::Key { key, pressed });
    }
    wait_for("texto tecleado", || {
        std::fs::read_to_string(&typed).unwrap_or_default() == "Con XKB: aB1!\n"
    });
    assert!(xterm.wait_exit().success(), "{}", xterm.errors());
}

// --- Aplicaciones GTK, Qt, Tk y Motif ----------------------------------------

/// Texto con el que se prueba el teclado: ASCII, Latin-1 y un símbolo de
/// fuera de Latin-1.
const TYPED: &str = "Hola, señor Ñu: 5 €";
const UTF8: [(&str, &str); 1] = [("LC_ALL", "C.UTF-8")];

/// La aplicación no se ha quejado de errores X.
fn assert_no_x_errors(client: &Client) {
    let log = client.errors();
    for bad in [
        "X Error",
        "X Window System error",
        "BadRequest",
        "BadImplementation",
        "BadMatch",
        "BadValue",
        "BadLength",
        "BadDrawable",
        "BadPicture",
        "BadGlyph",
    ] {
        assert!(
            !log.contains(bad),
            "«{bad}» en la salida de errores:\n{log}"
        );
    }
}

/// Espera a que la salida estándar de un cliente contenga una línea que
/// empiece por `prefix` y devuelve los números que la siguen.
fn wait_numbers(client: &mut Client, prefix: &str) -> Vec<i32> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let out = client.output();
        if let Some(line) = out
            .lines()
            .find(|l| l.starts_with(prefix) && out.contains(&format!("{l}\n")))
        {
            return line[prefix.len()..]
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
        }
        client.assert_running();
        assert!(
            Instant::now() < deadline,
            "no llegó «{prefix}»: {out}\n{}",
            client.errors()
        );
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn wait_output(client: &mut Client, text: &str) {
    let deadline = Instant::now() + TIMEOUT;
    while !client.output().contains(text) {
        assert!(
            Instant::now() < deadline,
            "no llegó «{text}»: {}\n{}",
            client.output(),
            client.errors()
        );
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Lo que queda escrito tras [`type_with_shortcuts`].
const PASTED_AND_TYPED: &str = "pegado: Hola, señor Ñu: 5 €";

/// Escribe en la entrada (vacía) que tenga el foco, pasando por el
/// portapapeles en los dos sentidos, atajos con Ctrl y teclas de edición, y
/// pulsa Intro. `start` es la zona donde empieza el texto de la entrada.
fn type_with_shortcuts(fx: &Fixture, start: Geometry) {
    let ctrl = |letter: u8| {
        for (key, pressed) in [
            (KeyCode::Control, true),
            (KeyCode::Letter(letter), true),
            (KeyCode::Letter(letter), false),
            (KeyCode::Control, false),
        ] {
            fx.send(DesktopInput::Key { key, pressed });
        }
    };
    // Pegar (Ctrl+V) lo que hay en el portapapeles local. (Se espera a
    // verlo: GTK pega cuando le llega el texto, y lo que se tecleara
    // mientras tanto quedaría delante.)
    assert_eq!(fx.colors_in(start), 1, "la entrada no está vacía");
    fx.send(DesktopInput::ClipboardText("xpegado: ".into()));
    ctrl(b'v');
    wait_for("texto pegado", || fx.colors_in(start) > 1);
    fx.send(DesktopInput::Text(format!("{TYPED}!!")));
    fx.key(KeyCode::Backspace);
    fx.key(KeyCode::Backspace);
    fx.key(KeyCode::Home);
    fx.key(KeyCode::Delete);
    // Seleccionarlo todo (Ctrl+A) y copiarlo (Ctrl+C) lo lleva al
    // portapapeles local.
    ctrl(b'a');
    ctrl(b'c');
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match fx.backend.events.recv_timeout(left) {
            Ok(DesktopEvent::ClipboardText(text)) if text == PASTED_AND_TYPED => break,
            Ok(_) => {}
            Err(_) => panic!("no llegó al portapapeles local lo copiado"),
        }
    }
    fx.key(KeyCode::Enter);
}

#[test]
fn gtk3_dialogs_draw_and_take_keyboard_and_mouse() {
    if missing(&["yad", "xwininfo"]) {
        return;
    }
    let fx = Fixture::new();
    // Teclado: lo escrito en la entrada sale por la salida estándar al
    // pulsar Intro.
    let mut entry = fx.spawn_env(
        "entrada",
        "yad",
        &["--entry", "--title=Entrada", "--text=Nombre:"],
        &UTF8,
    );
    let (_, g) = fx.wait_window(&mut entry, "Entrada");
    // Texto suavizado, degradados de los botones…: muchos colores.
    wait_for("yad pinta su diálogo", || fx.colors_in(g) > 40);
    fx.snapshot("gtk3-entrada");
    // (La entrada va bajo la etiqueta, a todo lo ancho.)
    let start = Geometry {
        x: g.x + 30,
        y: g.y + 32,
        w: 60,
        h: 16,
        viewable: true,
    };
    type_with_shortcuts(&fx, start);
    let status = entry.wait_exit();
    assert!(status.success(), "{status}: {}", entry.errors());
    assert_eq!(entry.output().trim_end(), PASTED_AND_TYPED);
    assert_no_x_errors(&entry);

    // Ratón: cada botón termina el programa con un código distinto.
    let mut buttons = fx.spawn_env(
        "botones",
        "yad",
        &[
            "--title=Botones",
            "--text=Elige uno",
            "--width=320",
            "--button=Uno:3",
            "--button=Dos:7",
        ],
        &UTF8,
    );
    let (_, g) = fx.wait_window(&mut buttons, "Botones");
    wait_for("yad pinta sus botones", || fx.colors_in(g) > 40);
    fx.snapshot("gtk3-botones");
    // Los botones van abajo, repartidos a lo ancho: el segundo, a la derecha.
    fx.click(g.x + g.w * 3 / 4, g.y + g.h - 20);
    let status = buttons.wait_exit();
    assert_eq!(status.code(), Some(7), "{}", buttons.errors());
    assert_no_x_errors(&buttons);
    wait_for("baja de los clientes", || fx.server.client_count() == 0);
}

#[test]
fn gtk3_widget_factory_paints_with_render() {
    if missing(&["gtk3-widget-factory", "xwininfo"]) {
        return;
    }
    let fx = Fixture::new();
    fx.server.resize(1400, 800);
    let started = Instant::now();
    let mut app = fx.spawn("gtk3-widget-factory", &[]);
    let (id, g) = fx.wait_window(&mut app, "gtk3-widget-factory");
    // Un escaparate de todos los controles de GTK: cientos de colores.
    wait_for("gtk3-widget-factory pinta", || fx.colors_in(g) > 400);
    eprintln!(
        "gtk3-widget-factory: {}x{} pintada a los {} ms",
        g.w,
        g.h,
        started.elapsed().as_millis()
    );
    fx.snapshot("gtk3-widget-factory");
    // La barra de título la pinta la propia aplicación: el gestor integrado
    // no le pone la suya, pero la coloca dentro de la pantalla.
    assert!(
        g.x >= 0 && g.y >= 0 && g.x + g.w <= 1400 && g.y + g.h <= 800,
        "{g:?}"
    );
    assert_ne!(fx.pixel(g.x + 40, g.y - 5), fx.pixel(g.x + 40, g.y + 5));
    assert_eq!(fx.pixel(g.x + 40, g.y - 5), fx.pixel(1390, 790));
    // Un clic en la segunda pestaña de arriba cambia el contenido.
    // (Se mira una zona sin animaciones: la primera entrada de la página.)
    let page = Geometry {
        x: g.x + 20,
        y: g.y + 60,
        w: 300,
        h: 40,
        viewable: true,
    };
    let before = fx.checksum(page);
    fx.click(g.x + g.w / 2, g.y + 26);
    wait_for("cambio de página", || fx.checksum(page) != before);
    fx.snapshot("gtk3-widget-factory-pagina2");
    // Y su botón de maximizar (el segundo por la derecha) pide el estado al
    // gestor de ventanas, que la agranda a toda la pantalla.
    fx.click(g.x + g.w - 68, g.y + 26);
    wait_for("ventana maximizada", || {
        fx.geometry(&id)
            .is_some_and(|n| (n.x, n.y, n.w, n.h) == (0, 0, 1400, 800))
    });
    // …y la aplicación se redibuja al tamaño nuevo, hasta el borde derecho
    // (donde antes no llegaba).
    let edge = Geometry {
        x: 1380,
        y: 60,
        w: 20,
        h: 700,
        viewable: true,
    };
    wait_for("redibujada a pantalla completa", || fx.colors_in(edge) > 2);
    fx.snapshot("gtk3-widget-factory-maximizada");
    app.assert_running();
    assert_no_x_errors(&app);
}

const QT_SCRIPT: &str = r#"
import sys
from PyQt5.QtCore import QTimer
from PyQt5.QtWidgets import (QApplication, QLabel, QLineEdit, QPushButton,
                             QVBoxLayout, QWidget)

app = QApplication(sys.argv)
w = QWidget()
w.setWindowTitle("Prueba Qt")
lay = QVBoxLayout(w)
lay.addWidget(QLabel("Escribe algo:"))
e = QLineEdit()
lay.addWidget(e)
b = QPushButton("Aceptar")
lay.addWidget(b)
b.clicked.connect(lambda: print("boton", flush=True))


def done():
    print("texto: " + e.text(), flush=True)
    app.exit(5)


def geometry():
    p = b.mapToGlobal(b.rect().topLeft())
    print("geometria", p.x(), p.y(), b.width(), b.height(), flush=True)


e.returnPressed.connect(done)
w.resize(320, 160)
w.show()
e.setFocus()
QTimer.singleShot(600, geometry)
sys.exit(app.exec_())
"#;

/// Intérprete de Python que tiene PyQt5, si hay alguno.
fn python_with_pyqt5() -> Option<&'static str> {
    ["python3", "/usr/bin/python3", "/usr/bin/python3.12"]
        .into_iter()
        .find(|python| {
            Command::new(python)
                .args(["-c", "import PyQt5.QtWidgets"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        })
}

#[test]
fn qt5_application_draws_and_takes_keyboard_and_mouse() {
    let Some(python) = python_with_pyqt5() else {
        eprintln!("OMITIDA: falta PyQt5");
        return;
    };
    if missing(&["xwininfo"]) {
        return;
    }
    let fx = Fixture::new();
    let script = fx.dir.path().join("prueba_qt.py");
    std::fs::write(&script, QT_SCRIPT).unwrap();
    let mut app = fx.spawn_env("qt", python, &[script.to_str().unwrap()], &UTF8);
    let (_, g) = fx.wait_window(&mut app, "Prueba Qt");
    wait_for("Qt pinta su ventana", || fx.colors_in(g) > 20);
    // Ratón: un clic en el botón.
    let b = wait_numbers(&mut app, "geometria");
    fx.snapshot("qt5");
    fx.click(b[0] + b[2] / 2, b[1] + b[3] / 2);
    wait_output(&mut app, "boton\n");
    // Teclado: el foco vuelve a la entrada con un clic y se escribe en ella.
    fx.click(g.x + g.w / 2, b[1] - 20);
    // (La entrada va justo encima del botón.)
    let start = Geometry {
        x: g.x + 30,
        y: b[1] - 24,
        w: 60,
        h: 12,
        viewable: true,
    };
    type_with_shortcuts(&fx, start);
    let status = app.wait_exit();
    assert_eq!(status.code(), Some(5), "{}", app.errors());
    assert!(
        app.output()
            .contains(&format!("texto: {PASTED_AND_TYPED}\n")),
        "{}",
        app.output()
    );
    assert_no_x_errors(&app);
}

const TK_SCRIPT: &str = r##"
wm title . "Prueba Tk"
wm geometry . 280x170+60+80
. configure -background "#cc2222"
menubutton .m -text "Menu" -menu .m.menu -relief raised
menu .m.menu -tearoff 0
.m.menu add command -label "Primera opcion" -command { puts uno; flush stdout }
.m.menu add command -label "Segunda opcion" -command { puts dos; flush stdout }
entry .e -width 24
button .b -text "Aceptar" -command { puts boton; flush stdout }
pack .m -side top -anchor w
pack .e -side top -fill x -padx 6 -pady 6
pack .b -side bottom -padx 6 -pady 6
bind .e <Return> { puts "texto: [.e get]"; flush stdout; exit 3 }
focus -force .e
# Un diálogo de la ventana principal y una ventana sin gestor (como un
# menú o un aviso emergente).
toplevel .d -background "#22aa22" -width 140 -height 70
wm title .d "Dialogo"
wm transient .d .
toplevel .o -background "#2222cc" -width 60 -height 40
wm overrideredirect .o 1
wm geometry .o 60x40+400+300
proc geo {w} { return "[winfo rootx $w] [winfo rooty $w] [winfo width $w] [winfo height $w]" }
after 700 {
    puts "menu [geo .m]"
    puts "boton [geo .b]"
    puts "dialogo [geo .d]"
    puts "principal [geo .]"
    flush stdout
}
"##;

#[test]
fn tk_application_with_menu_dialog_and_popup() {
    if missing(&["wish", "xwininfo"]) {
        return;
    }
    let fx = Fixture::new();
    let script = fx.dir.path().join("prueba.tcl");
    std::fs::write(&script, TK_SCRIPT).unwrap();
    let mut app = fx.spawn_env("tk", "wish", &[script.to_str().unwrap()], &UTF8);
    let (_, g) = fx.wait_window(&mut app, "Prueba Tk");
    let menu = wait_numbers(&mut app, "menu");
    let button = wait_numbers(&mut app, "boton");
    let dialog = wait_numbers(&mut app, "dialogo");
    let main = wait_numbers(&mut app, "principal");
    wait_for("Tk pinta su ventana", || fx.colors_in(g) > 5);
    fx.snapshot("tk");
    const RED: [u8; 3] = [0xcc, 0x22, 0x22];
    const GREEN: [u8; 3] = [0x22, 0xaa, 0x22];
    const BLUE: [u8; 3] = [0x22, 0x22, 0xcc];

    // La ventana sin gestor no lleva marco: justo encima de ella se ve el
    // fondo del escritorio, no una barra de título.
    assert_eq!(fx.pixel(430, 320), BLUE);
    assert_eq!(fx.pixel(430, 295), fx.pixel(790, 590));
    // El diálogo sale centrado sobre su ventana, con barra de título, y por
    // encima de ella.
    let center = (dialog[0] + dialog[2] / 2, dialog[1] + dialog[3] / 2);
    assert_eq!(fx.pixel(center.0, center.1), GREEN);
    let main_center = (main[0] + main[2] / 2, main[1] + main[3] / 2);
    assert!(
        (center.0 - main_center.0).abs() <= 2 && (center.1 - main_center.1).abs() <= 2,
        "diálogo en {dialog:?}, principal en {main:?}"
    );
    assert_ne!(fx.pixel(center.0, dialog[1] - 11), RED);
    // Activar la ventana principal (un clic en su fondo) no tapa el diálogo.
    fx.click(main[0] + 8, main[1] + main[3] - 8);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(fx.pixel(center.0, center.1), GREEN);
    fx.snapshot("tk-dialogo");
    // El diálogo se lleva, arrastrándolo por su barra de título, hasta
    // debajo de la ventana sin gestor: esta sigue viéndose entera, encima.
    let bar = (center.0, dialog[1] - 11);
    fx.drag(bar, (430, 320 - dialog[3] / 2 - 11));
    wait_for("diálogo movido", || fx.pixel(430 - 50, 320) == GREEN);
    for (x, y) in [(401, 301), (430, 320), (458, 338)] {
        assert_eq!(fx.pixel(x, y), BLUE, "en ({x}, {y})");
    }
    fx.snapshot("tk-apilado");

    // Ratón: el botón, y una opción del menú desplegable (una ventana sin
    // gestor que tiene que quedar por encima de todo).
    fx.click(button[0] + button[2] / 2, button[1] + button[3] / 2);
    wait_output(&mut app, "boton\n");
    fx.click(menu[0] + menu[2] / 2, menu[1] + menu[3] / 2);
    // El menú tapa el fondo rojo que hay bajo la entrada.
    let under = (menu[0] + 40, menu[1] + menu[3] + 36);
    assert_eq!(fx.pixel(under.0, under.1), RED);
    wait_for("menú desplegado", || fx.pixel(under.0, under.1) != RED);
    fx.snapshot("tk-menu");
    fx.click(under.0, under.1);
    wait_output(&mut app, "dos\n");
    wait_for("menú recogido", || fx.pixel(under.0, under.1) == RED);

    // Teclado. (Las aplicaciones de Xlib traducen las teclas según su
    // configuración regional, y la «C» de estas pruebas no pasa de Latin-1.)
    let typed = "Hola, señor Ñu: ¿5?";
    fx.click(g.x + g.w / 2, button[1] - 25);
    fx.send(DesktopInput::Text(typed.into()));
    fx.key(KeyCode::Enter);
    let status = app.wait_exit();
    assert_eq!(status.code(), Some(3), "{}", app.errors());
    assert!(
        app.output().contains(&format!("texto: {typed}\n")),
        "{}",
        app.output()
    );
    assert_no_x_errors(&app);
}

#[test]
fn motif_editor_starts_and_takes_typing() {
    if missing(&["nedit", "xwininfo"]) {
        return;
    }
    let fx = Fixture::new();
    let mut app = fx.spawn("nedit", &["-geometry", "60x12"]);
    let (_, g) = fx.wait_window(&mut app, "NEdit");
    // Barra de menús, relieves y texto.
    wait_for("nedit pinta su ventana", || fx.colors_in(g) > 5);
    std::thread::sleep(Duration::from_millis(300));
    fx.snapshot("nedit");
    // Lo tecleado aparece en la primera línea de la zona de texto (bajo la
    // barra de menús; se mira a la derecha del cursor, que parpadea).
    let line = Geometry {
        x: g.x + 20,
        y: g.y + 28,
        w: 90,
        h: 20,
        viewable: true,
    };
    assert_eq!(fx.colors_in(line), 1, "la primera línea no está vacía");
    fx.click(g.x + g.w / 2, g.y + g.h / 2);
    fx.send(DesktopInput::Text("Hola desde Motif".into()));
    wait_for("texto en el editor", || fx.colors_in(line) > 1);
    fx.snapshot("nedit-texto");
    app.assert_running();
    assert_no_x_errors(&app);
}

// --- Gestor de ventanas integrado: maximizar, restaurar, redimensionar -------

/// Filas y columnas que el xterm dice tener (lo que ve el programa de dentro).
fn terminal_size(path: &std::path::Path) -> Option<(i32, i32)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut parts = text.split_whitespace().map(|n| n.parse().ok());
    Some((parts.next()??, parts.next()??))
}

#[test]
fn built_in_window_manager_maximizes_restores_and_resizes() {
    if missing(&["xterm", "xwininfo", "xprop", "stty"]) {
        return;
    }
    let fx = Fixture::new();
    // Un xterm cuyo intérprete apunta cada poco el tamaño de su terminal:
    // así se comprueba el tamaño que de verdad le llega al cliente.
    let size_file = fx.dir.path().join("tamano");
    let report = format!(
        "while :; do stty size > '{0}.tmp' && mv '{0}.tmp' '{0}'; sleep 0.05; done",
        size_file.display()
    );
    let mut term = fx.spawn(
        "xterm",
        &[
            "-title",
            "Tallas",
            "-fn",
            "fixed",
            "-geometry",
            "40x10+100+120",
            "-e",
            "sh",
            "-c",
            &report,
        ],
    );
    let (id, g) = fx.wait_window(&mut term, "Tallas");
    wait_for("tamaño inicial del terminal", || {
        terminal_size(&size_file) == Some((10, 40))
    });
    // El xterm declara en WM_NORMAL_HINTS el tamaño de sus celdas y el
    // margen interior; `xwininfo` da la esquina exterior y Xt pone un borde
    // de un píxel a cada lado.
    let hints = fx.stdout("xprop", &["-id", &id, "WM_NORMAL_HINTS"]);
    let pair = |label: &str| -> (i32, i32) {
        let line = hints
            .lines()
            .find(|l| l.contains(label))
            .unwrap_or_else(|| panic!("falta «{label}» en {hints}"));
        let mut numbers = line
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|n| n.parse().ok());
        (numbers.next().unwrap(), numbers.next().unwrap())
    };
    let (cell_w, cell_h) = pair("resize increment");
    let (extra_w, extra_h) = pair("base size");
    let border = 1;
    assert_eq!((g.w, g.h), (extra_w + 40 * cell_w, extra_h + 10 * cell_h));
    let title = |g: Geometry| (g.x + 40, g.y - 11);
    // Tamaño de la zona útil de una ventana maximizada: la pantalla menos
    // el marco (3 px), la barra de título (22 px) y el borde.
    let full = (
        WIDTH as i32 - 2 * 3 - 2 * border,
        HEIGHT as i32 - 22 - 3 - 2 * border,
    );
    let cells = |w: i32, h: i32| ((h - extra_h) / cell_h, (w - extra_w) / cell_w);
    let maximized = |fx: &Fixture| {
        fx.geometry(&id)
            .is_some_and(|n| (n.x, n.y, n.w, n.h) == (3, 22, full.0, full.1))
            && terminal_size(&size_file) == Some(cells(full.0, full.1))
    };
    let restored = |fx: &Fixture| {
        fx.geometry(&id)
            .is_some_and(|n| (n.x, n.y, n.w, n.h) == (g.x, g.y, g.w, g.h))
            && terminal_size(&size_file) == Some((10, 40))
    };
    let state = |fx: &Fixture| fx.stdout("xprop", &["-id", &id, "_NET_WM_STATE"]);

    // Doble clic en la barra de título: maximiza; otro: restaura.
    let at = title(g);
    fx.click(at.0, at.1);
    fx.click(at.0, at.1);
    wait_for("maximizada con doble clic", || maximized(&fx));
    assert!(
        state(&fx).contains("_NET_WM_STATE_MAXIMIZED_VERT"),
        "{}",
        state(&fx)
    );
    assert!(
        state(&fx).contains("_NET_WM_STATE_MAXIMIZED_HORZ"),
        "{}",
        state(&fx)
    );
    fx.snapshot("maximizada");
    std::thread::sleep(Duration::from_millis(500));
    fx.click(200, 11);
    fx.click(200, 11);
    wait_for("restaurada con doble clic", || restored(&fx));
    assert!(!state(&fx).contains("MAXIMIZED"), "{}", state(&fx));

    // El botón de maximizar de la barra (a la izquierda del de cerrar).
    std::thread::sleep(Duration::from_millis(500));
    let button = (g.x + g.w + 2 * border + 3 - 2 * 20 + 8, g.y - 11);
    fx.click(button.0, button.1);
    wait_for("maximizada con el botón", || maximized(&fx));
    fx.click(WIDTH as i32 - 2 * 20 + 8, 11);
    wait_for("restaurada con el botón", || restored(&fx));

    // Lo mismo, pedido por otro programa con los mensajes de EWMH.
    if find_program("wmctrl").is_some() {
        let toggle = |action: &str| {
            fx.stdout("wmctrl", &["-r", "Tallas", "-b", action]);
        };
        toggle("add,maximized_vert");
        wait_for("maximizada a lo alto", || {
            fx.geometry(&id)
                .is_some_and(|n| (n.x, n.y, n.w, n.h) == (g.x, 22, g.w, full.1))
                && terminal_size(&size_file) == Some((cells(g.w, full.1).0, 40))
        });
        toggle("add,maximized_horz");
        wait_for("maximizada del todo", || maximized(&fx));
        toggle("remove,maximized_vert,maximized_horz");
        wait_for("restaurada por mensaje", || restored(&fx));
        // Y la lista de ventanas del gestor.
        let list = fx.stdout("wmctrl", &["-l"]);
        assert!(list.contains("Tallas"), "{list}");
    }

    // Tirar del borde izquierdo: cambia el ancho de celda en celda (nueve
    // celdas y pico son nueve celdas) y el borde derecho no se mueve.
    let left = (g.x - 2, g.y + g.h / 2);
    fx.drag(left, (left.0 - 9 * cell_w - 3, left.1 + 5));
    wait_for("más ancha por la izquierda", || {
        fx.geometry(&id)
            .is_some_and(|n| (n.x, n.y, n.w, n.h) == (g.x - 9 * cell_w, g.y, g.w + 9 * cell_w, g.h))
            && terminal_size(&size_file) == Some((10, 49))
    });
    // Tirar del borde de arriba (por encima de la barra de título): cambia
    // el alto y el borde de abajo no se mueve.
    let wide = fx.geometry(&id).unwrap();
    let top = (wide.x + wide.w / 2, wide.y - 22);
    fx.drag(top, (top.0, top.1 - 3 * cell_h - 5));
    wait_for("más alta por arriba", || {
        fx.geometry(&id).is_some_and(|n| {
            (n.x, n.y, n.w, n.h) == (wide.x, wide.y - 3 * cell_h, wide.w, wide.h + 3 * cell_h)
        }) && terminal_size(&size_file) == Some((13, 49))
    });
    // Encoger sin medida desde la esquina inferior derecha: se queda en el
    // tamaño mínimo que declara la aplicación (una celda).
    let tall = fx.geometry(&id).unwrap();
    let corner = (
        tall.x + tall.w + 2 * border + 1,
        tall.y + tall.h + 2 * border + 1,
    );
    fx.drag(corner, (tall.x - 200, tall.y - 200));
    // (El gestor no deja las ventanas tan estrechas que no se puedan
    // agarrar: el ancho se queda en dos celdas.)
    wait_for("en su tamaño mínimo", || {
        fx.geometry(&id).is_some_and(|n| {
            (n.x, n.y, n.w, n.h) == (tall.x, tall.y, extra_w + 2 * cell_w, extra_h + cell_h)
        }) && terminal_size(&size_file) == Some((1, 2))
    });
    fx.snapshot("minima");
    term.assert_running();
}

// --- RANDR y SHAPE ------------------------------------------------------------

#[test]
fn randr_reports_the_screen_and_its_changes() {
    if missing(&["xrandr", "xev"]) {
        return;
    }
    let fx = Fixture::new();
    let info = fx.stdout("xrandr", &[]);
    assert!(info.contains("current 800 x 600"), "{info}");
    assert!(
        info.contains("GodRemote-0 connected primary 800x600+0+0"),
        "{info}"
    );
    // Un cliente apuntado a los avisos se entera de que la pestaña (la
    // pantalla) cambia de tamaño.
    let mut xev = fx.spawn("xev", &["-root", "-event", "randr"]);
    wait_for("xev conectado", || fx.server.client_count() == 1);
    std::thread::sleep(Duration::from_millis(500));
    fx.server.resize(1024, 700);
    wait_output(&mut xev, "RRScreenChangeNotify event");
    wait_output(&mut xev, "width 1024, height 700");
    wait_output(&mut xev, "RRNotify event");
    let info = fx.stdout("xrandr", &[]);
    assert!(info.contains("current 1024 x 700"), "{info}");
    assert!(info.contains("1024x700+0+0"), "{info}");
    xev.assert_running();
}

#[test]
fn shaped_windows_show_what_is_behind_them() {
    if missing(&["xeyes", "xwininfo"]) {
        return;
    }
    let fx = Fixture::new();
    let mut eyes = fx.spawn("xeyes", &["-geometry", "200x120+100+100"]);
    let (_, g) = fx.wait_window(&mut eyes, "XEyes");
    let desktop = fx.pixel(790, 590);
    // Los ojos se ven (blanco y negro)…
    wait_for("xeyes pinta", || {
        fx.pixel(g.x + g.w / 4, g.y + g.h / 2) != desktop
    });
    // …y entre ellos, y en las esquinas, se ve el fondo del escritorio.
    for (x, y) in [
        (g.x + 2, g.y + 2),
        (g.x + g.w - 3, g.y + g.h - 3),
        (g.x + g.w / 2, g.y + 3),
    ] {
        assert_eq!(fx.pixel(x, y), desktop, "en ({x}, {y})");
    }
    fx.snapshot("xeyes-forma");
    eyes.assert_running();
}

// --- Medidas ------------------------------------------------------------------

/// Espera (sondeando cada milisegundo) a que cambie una zona de la pantalla
/// y devuelve lo que ha tardado.
fn time_to_change(fx: &Fixture, zone: Geometry, before: u64) -> Duration {
    let start = Instant::now();
    while fx.checksum(zone) == before {
        assert!(start.elapsed() < TIMEOUT, "la pantalla no cambia");
        std::thread::sleep(Duration::from_millis(1));
    }
    start.elapsed()
}

/// Medidas de rendimiento con GTK 3 (no comprueban nada: se lanzan a mano).
///
///   cargo test --release --test xserver_integration measure -- --ignored --nocapture
#[test]
#[ignore]
fn measure_gtk3_responsiveness() {
    if missing(&["yad", "gtk3-widget-factory", "xwininfo"]) {
        return;
    }
    // Eco de las teclas en una entrada de texto.
    let fx = Fixture::new();
    let mut entry = fx.spawn_env("entrada", "yad", &["--entry", "--title=Entrada"], &UTF8);
    let (_, g) = fx.wait_window(&mut entry, "Entrada");
    wait_for("yad pinta su diálogo", || fx.colors_in(g) > 40);
    std::thread::sleep(Duration::from_millis(500));
    // (Todo menos los botones de abajo. El cursor parpadea ahí dentro, pero
    // es difícil que lo haga justo en los milisegundos que tarda el eco.)
    let text = Geometry { h: g.h - 45, ..g };
    let mut echoes = vec![];
    for letter in "El veloz murciélago hindú comía".chars() {
        let before = fx.checksum(text);
        fx.send(DesktopInput::Text(letter.into()));
        echoes.push(time_to_change(&fx, text, before));
        std::thread::sleep(Duration::from_millis(60));
    }
    echoes.sort();
    eprintln!(
        "eco de una tecla en una entrada GTK 3: mediana {:.1} ms, máximo {:.1} ms",
        echoes[echoes.len() / 2].as_secs_f64() * 1000.0,
        echoes[echoes.len() - 1].as_secs_f64() * 1000.0
    );
    fx.snapshot("medida-eco");
    drop(entry);

    // Primer pintado de una ventana grande y respuesta a un clic.
    let fx = Fixture::new();
    fx.server.resize(1400, 800);
    let started = Instant::now();
    let mut app = fx.spawn("gtk3-widget-factory", &[]);
    let (_, g) = fx.wait_window(&mut app, "gtk3-widget-factory");
    wait_for("gtk3-widget-factory pinta", || fx.colors_in(g) > 400);
    eprintln!(
        "gtk3-widget-factory ({}x{}): pintada a los {} ms de lanzarla",
        g.w,
        g.h,
        started.elapsed().as_millis()
    );
    std::thread::sleep(Duration::from_secs(2));
    // El menú de la cabecera se despliega con una animación.
    let menu = Geometry {
        x: g.x + g.w - 240,
        y: g.y + 70,
        w: 160,
        h: 150,
        viewable: true,
    };
    let before = fx.checksum(menu);
    fx.click(g.x + g.w - 156, g.y + 26);
    let first = time_to_change(&fx, menu, before);
    // Hasta que deja de cambiar (300 ms seguidos igual).
    let start = Instant::now();
    let (mut last, mut last_change, mut frames) = (fx.checksum(menu), Instant::now(), 0);
    while last_change.elapsed() < Duration::from_millis(300) {
        let now = fx.checksum(menu);
        if now != last {
            (last, last_change, frames) = (now, Instant::now(), frames + 1);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    eprintln!(
        "menú de la cabecera: primer cambio a los {:.0} ms del clic; desplegado del todo {:.0} ms \
         después, en {frames} pasos",
        first.as_secs_f64() * 1000.0,
        (start.elapsed() - Duration::from_millis(300)).as_secs_f64() * 1000.0
    );
    fx.snapshot("medida-menu");
}

// --- Java (Swing) -------------------------------------------------------------

const JAVA_PROGRAM: &str = r#"
import java.awt.FlowLayout;
import java.awt.Point;
import javax.swing.JButton;
import javax.swing.JFrame;
import javax.swing.JLabel;
import javax.swing.JTextField;
import javax.swing.Timer;

public class Prueba {
    public static void main(String[] args) {
        JFrame frame = new JFrame("Prueba Java");
        JTextField text = new JTextField(20);
        JButton button = new JButton("Aceptar");
        button.addActionListener(e -> System.out.println("boton"));
        text.addActionListener(e -> {
            System.out.println("texto: " + text.getText());
            System.exit(4);
        });
        frame.setLayout(new FlowLayout());
        frame.add(new JLabel("Escribe algo:"));
        frame.add(text);
        frame.add(button);
        frame.setDefaultCloseOperation(JFrame.EXIT_ON_CLOSE);
        frame.setSize(360, 140);
        frame.setVisible(true);
        Timer timer = new Timer(800, e -> {
            Point b = button.getLocationOnScreen();
            Point t = text.getLocationOnScreen();
            System.out.println("boton en " + b.x + " " + b.y + " " + button.getWidth() + " " + button.getHeight());
            System.out.println("entrada en " + t.x + " " + t.y + " " + text.getWidth() + " " + text.getHeight());
        });
        timer.setRepeats(false);
        timer.start();
    }
}
"#;

#[test]
fn java_swing_application_draws_and_takes_keyboard_and_mouse() {
    // (Hace falta un JDK: el programa se lanza desde su fuente.)
    if missing(&["java", "javac", "xwininfo"]) {
        return;
    }
    let fx = Fixture::new();
    let source = fx.dir.path().join("Prueba.java");
    std::fs::write(&source, JAVA_PROGRAM).unwrap();
    let mut app = fx.spawn_env(
        "java",
        "java",
        &["-Dfile.encoding=UTF-8", source.to_str().unwrap()],
        &UTF8,
    );
    // La máquina virtual tarda lo suyo en arrancar y compilar.
    let slow = Instant::now() + 3 * TIMEOUT;
    while fx.find_windows("\"Prueba Java\"").is_empty() {
        app.assert_running();
        assert!(Instant::now() < slow, "no arrancó: {}", app.errors());
        std::thread::sleep(Duration::from_millis(100));
    }
    let (_, g) = fx.wait_window(&mut app, "Prueba Java");
    let button = wait_numbers(&mut app, "boton en");
    let entry = wait_numbers(&mut app, "entrada en");
    // La entrada y su etiqueta quedan dentro de la ventana, arriba (y no
    // descuadradas por la barra de título, como pasa si Java cree que el
    // gestor ha metido su ventana dentro de otra).
    assert!(
        entry[1] >= g.y && entry[1] < g.y + 20 && entry[0] > g.x + 60,
        "entrada en {entry:?}, ventana en {g:?}"
    );
    let inside = Geometry {
        x: entry[0],
        y: entry[1],
        w: entry[2],
        h: entry[3],
        viewable: true,
    };
    let label = Geometry {
        x: g.x + 4,
        w: entry[0] - g.x - 8,
        ..inside
    };
    wait_for("Java pinta la etiqueta", || fx.colors_in(label) >= 2);
    wait_for("Java pinta la entrada", || fx.colors_in(inside) >= 2);
    fx.snapshot("java");
    // Ratón.
    fx.click(button[0] + button[2] / 2, button[1] + button[3] / 2);
    wait_output(&mut app, "boton\n");
    // Teclado.
    fx.click(entry[0] + entry[2] / 2, entry[1] + entry[3] / 2);
    fx.send(DesktopInput::Text(TYPED.into()));
    fx.key(KeyCode::Enter);
    let status = app.wait_exit();
    assert_eq!(status.code(), Some(4), "{}", app.errors());
    assert!(
        app.output().contains(&format!("texto: {TYPED}\n")),
        "{}",
        app.output()
    );
    assert_no_x_errors(&app);
}
