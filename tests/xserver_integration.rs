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
        let log = self.dir.path().join(format!("{program}.log"));
        let out = self.dir.path().join(format!("{program}.out"));
        let child = self
            .command(program)
            .args(args)
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

    /// Identificador (`0x…`) de la primera ventana de primer nivel cuyo
    /// nombre o clase, tal como los lista `xwininfo`, contiene el texto.
    fn find_window(&self, text: &str) -> Option<String> {
        let out = self.run("xwininfo", &["-root", "-children"]);
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim_start)
            .find(|l| l.starts_with("0x") && l.contains(text))
            .and_then(|l| l.split_whitespace().next())
            .map(str::to_string)
    }

    /// Espera a que exista una ventana visible con esa clase (o nombre) y
    /// devuelve su identificador y su posición en pantalla.
    fn wait_window(&self, client: &mut Client, class: &str) -> (String, Geometry) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(id) = self.find_window(&format!("\"{class}\""))
                && let Some(g) = self.geometry(&id)
                && g.viewable
            {
                return (id, g);
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
    assert_eq!(fx.stdout("xprop", &["-root"]).trim(), "");
    fx.stdout(
        "xprop",
        &["-root", "-f", "PRUEBA", "8s", "-set", "PRUEBA", "hola"],
    );
    let props = fx.stdout("xprop", &["-root"]);
    assert!(props.contains("PRUEBA(STRING) = \"hola\""), "{props}");
    fx.stdout("xprop", &["-root", "-remove", "PRUEBA"]);
    assert_eq!(fx.stdout("xprop", &["-root"]).trim(), "");

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
