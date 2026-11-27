//! Emulador de terminal (vt100) dibujado con egui, y shell local.

use std::io::{Read, Write};
use std::num::NonZeroU16;

use anyhow::Result;
use egui::{Align2, Color32, Event, EventFilter, FontId, Key, Pos2, Rect, Sense, Stroke, Vec2};
use tokio::sync::mpsc;

use std::sync::Arc;

use crate::backdrop::Backdrop;
use crate::highlight::{Compiled, HlColor};
use crate::ssh::{SshHandle, Waker};
use crate::theme::TermPalette;

pub enum TermCmd {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Close,
}

pub enum TermEvent {
    /// Mensaje de progreso (mientras conecta).
    Status(String),
    /// La shell está lista. Para SSH incluye la conexión (reutilizable por SFTP).
    Connected(Option<SshHandle>),
    Data(Vec<u8>),
    /// Secretos validados que el usuario pidió guardar en el baúl.
    SaveSecrets(Vec<(String, String)>),
    /// Estado de uno de los túneles de la sesión.
    Tunnel {
        index: usize,
        status: crate::tunnels::TunnelStatus,
    },
    /// Fin de la sesión, con el error si lo hubo.
    Closed(Option<String>),
}

/// Algo que el usuario introduce en un terminal, antes de traducirlo a
/// bytes. Se guarda así para poder repetirlo en otros terminales (difusión):
/// cada uno lo traduce según su propio estado (modo de las flechas, pegado
/// entre corchetes…).
#[derive(Debug, Clone, PartialEq)]
pub enum TermInput {
    /// Texto tecleado o bytes ya preparados (un snippet).
    Bytes(Vec<u8>),
    /// Tecla especial (flechas, Intro, Ctrl+letra…).
    Key(Key, egui::Modifiers),
    /// Texto pegado del portapapeles.
    Paste(String),
}

/// Extremos de comunicación con una shell (local o remota).
pub struct TermBackend {
    pub cmd: mpsc::UnboundedSender<TermCmd>,
    pub events: std::sync::mpsc::Receiver<TermEvent>,
}

/// Lanza la shell local del sistema en un pseudo-terminal.
pub fn spawn_local_shell(waker: Waker, cols: u16, rows: u16) -> Result<TermBackend> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    let size = PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    };
    let pair = native_pty_system().openpty(size)?;
    let mut cmd = CommandBuilder::new_default_prog();
    cmd.env("TERM", "xterm-256color");
    cmd.cwd(crate::paths::home_dir());
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let mut killer = child.clone_killer();
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let master = pair.master;

    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<TermCmd>();
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    let _ = ev_tx.send(TermEvent::Connected(None));

    // Lector: salida de la shell -> interfaz.
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if ev_tx.send(TermEvent::Data(buf[..n].to_vec())).is_err() {
                        break;
                    }
                    waker();
                }
            }
        }
        let _ = child.wait();
        let _ = ev_tx.send(TermEvent::Closed(None));
        waker();
    });

    // Escritor: teclado / redimensionado -> shell.
    std::thread::spawn(move || {
        while let Some(cmd) = cmd_rx.blocking_recv() {
            match cmd {
                TermCmd::Input(data) => {
                    if writer
                        .write_all(&data)
                        .and_then(|_| writer.flush())
                        .is_err()
                    {
                        break;
                    }
                }
                TermCmd::Resize { cols, rows } => {
                    let _ = master.resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    });
                }
                TermCmd::Close => break,
            }
        }
        let _ = killer.kill();
    });

    Ok(TermBackend {
        cmd: cmd_tx,
        events: ev_rx,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConnState {
    Connecting,
    Connected,
    Closed(Option<String>),
}

const SCROLLBACK: usize = 10_000;

fn indexed_color(i: u8, palette: &TermPalette) -> Color32 {
    match i {
        0..=15 => palette.ansi[i as usize],
        16..=231 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color32::from_rgb(level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        232..=255 => {
            let v = 8 + (i - 232) * 10;
            Color32::from_rgb(v, v, v)
        }
    }
}

fn resolve(color: vt100::Color, default: Color32, bold: bool, palette: &TermPalette) -> Color32 {
    match color {
        vt100::Color::Default => default,
        // La negrita se representa con la variante brillante del color.
        vt100::Color::Idx(i) if bold && i < 8 => indexed_color(i + 8, palette),
        vt100::Color::Idx(i) => indexed_color(i, palette),
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

/// Traduce una tecla especial a la secuencia que espera el terminal.
pub fn key_bytes(key: Key, mods: egui::Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let csi = |c: char| -> Vec<u8> {
        if app_cursor && !mods.shift && !mods.ctrl && !mods.alt {
            format!("\x1bO{c}").into_bytes()
        } else if mods.ctrl || mods.shift || mods.alt {
            let m = 1 + mods.shift as u8 + 2 * mods.alt as u8 + 4 * mods.ctrl as u8;
            format!("\x1b[1;{m}{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };
    let tilde = |n: u8| format!("\x1b[{n}~").into_bytes();
    Some(match key {
        Key::Enter => {
            if mods.alt {
                b"\x1b\r".to_vec()
            } else {
                b"\r".to_vec()
            }
        }
        Key::Backspace => {
            if mods.alt {
                b"\x1b\x7f".to_vec()
            } else if mods.ctrl {
                b"\x08".to_vec()
            } else {
                b"\x7f".to_vec()
            }
        }
        Key::Tab => {
            if mods.shift {
                b"\x1b[Z".to_vec()
            } else {
                b"\t".to_vec()
            }
        }
        Key::Escape => b"\x1b".to_vec(),
        Key::ArrowUp => csi('A'),
        Key::ArrowDown => csi('B'),
        Key::ArrowRight => csi('C'),
        Key::ArrowLeft => csi('D'),
        Key::Home => csi('H'),
        Key::End => csi('F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F1 => b"\x1bOP".to_vec(),
        Key::F2 => b"\x1bOQ".to_vec(),
        Key::F3 => b"\x1bOR".to_vec(),
        Key::F4 => b"\x1bOS".to_vec(),
        Key::F5 => tilde(15),
        Key::F6 => tilde(17),
        Key::F7 => tilde(18),
        Key::F8 => tilde(19),
        Key::F9 => tilde(20),
        Key::F10 => tilde(21),
        Key::F11 => tilde(23),
        Key::F12 => tilde(24),
        Key::Space if mods.ctrl => vec![0],
        _ => {
            // Ctrl+letra -> carácter de control; Alt+letra -> ESC + letra.
            let name = key.name();
            let b = name.as_bytes();
            if b.len() == 1 && b[0].is_ascii_alphabetic() {
                let lower = b[0].to_ascii_lowercase();
                if mods.ctrl && !mods.alt {
                    return Some(vec![lower & 0x1f]);
                }
                if mods.alt && !mods.ctrl {
                    let c = if mods.shift {
                        b[0].to_ascii_uppercase()
                    } else {
                        lower
                    };
                    return Some(vec![0x1b, c]);
                }
            }
            match key {
                Key::OpenBracket if mods.ctrl => vec![0x1b],
                Key::Backslash if mods.ctrl => vec![0x1c],
                Key::CloseBracket if mods.ctrl => vec![0x1d],
                _ => return None,
            }
        }
    })
}

/// Codifica un evento de ratón para la aplicación remota (xterm).
///
/// `code`: 0 izquierdo, 1 central, 2 derecho, 3 ninguno; +32 movimiento;
/// 64/65 rueda arriba/abajo. `col`/`row` empiezan en 0.
pub fn mouse_report(
    encoding: vt100::MouseProtocolEncoding,
    code: u8,
    mods: egui::Modifiers,
    col: u16,
    row: u16,
    release: bool,
) -> Option<Vec<u8>> {
    let mut cb = code as u32;
    if mods.shift {
        cb += 4;
    }
    if mods.alt {
        cb += 8;
    }
    if mods.ctrl {
        cb += 16;
    }
    let (x, y) = (col as u32 + 1, row as u32 + 1);
    match encoding {
        vt100::MouseProtocolEncoding::Sgr => {
            Some(format!("\x1b[<{cb};{x};{y}{}", if release { 'm' } else { 'M' }).into_bytes())
        }
        vt100::MouseProtocolEncoding::Default => {
            // El formato clásico no distingue qué botón se suelta y no puede
            // expresar coordenadas mayores de 223.
            let cb = if release { (cb & !3) | 3 } else { cb };
            if x > 223 || y > 223 {
                return None;
            }
            Some(vec![
                0x1b,
                b'[',
                b'M',
                (cb + 32) as u8,
                (x + 32) as u8,
                (y + 32) as u8,
            ])
        }
        vt100::MouseProtocolEncoding::Utf8 => {
            let cb = if release { (cb & !3) | 3 } else { cb };
            let mut out = vec![0x1b, b'[', b'M'];
            for v in [cb + 32, x + 32, y + 32] {
                let mut buf = [0u8; 4];
                out.extend_from_slice(char::from_u32(v)?.encode_utf8(&mut buf).as_bytes());
            }
            Some(out)
        }
    }
}

/// Lo que la shell remota cuenta de sí misma con secuencias de control: el
/// título de la ventana y, si lo anuncia (OSC 7), su carpeta de trabajo.
#[derive(Debug, Default)]
struct TermCallbacks {
    title: String,
    cwd: Option<String>,
    /// Ha sonado la campana (BEL) desde la última vez que se miró.
    bell: bool,
}

impl vt100::Callbacks for TermCallbacks {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bell = true;
    }

    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = String::from_utf8_lossy(title).chars().take(300).collect();
    }

    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        // OSC 7: «file://equipo/ruta».
        if let [b"7", url] = params {
            self.cwd = dir_from_url(&String::from_utf8_lossy(url));
        }
    }
}

/// Carpeta de una URL `file://equipo/ruta` (con `%XX` descodificado).
fn dir_from_url(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    let bytes = path.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = path.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok().filter(|p| !p.is_empty())
}

/// Carpeta que aparece en el título habitual de las shells
/// (`usuario@equipo: ~/proyecto`, `usuario@equipo:/var/log`). Puede empezar
/// por `~`.
fn dir_from_title(title: &str) -> Option<String> {
    let at = title.find('@')?;
    let after = &title[at..];
    let dir = after[after.find(':')? + 1..].trim();
    (dir.starts_with('/') || dir == "~" || dir.starts_with("~/")).then(|| dir.to_string())
}

/// Búsqueda abierta sobre el contenido del terminal (pantalla e historial).
#[derive(Default)]
struct Search {
    query: String,
    /// Coincidencia actual: línea contada desde la última (0) y columna.
    current: Option<(usize, usize)>,
    /// Posición de la coincidencia actual y total de coincidencias.
    position: (usize, usize),
    /// Hay que dar el foco al campo de texto.
    focus: bool,
}

/// Posiciones (en caracteres) donde aparece `needle` en `line`, sin
/// distinguir mayúsculas. `needle` ya viene en minúsculas.
fn find_all(line: &[char], needle: &[char]) -> Vec<usize> {
    if needle.is_empty() || line.len() < needle.len() {
        return vec![];
    }
    let lower = |c: char| c.to_lowercase().next().unwrap_or(c);
    (0..=line.len() - needle.len())
        .filter(|&start| {
            line[start..start + needle.len()]
                .iter()
                .zip(needle)
                .all(|(a, b)| lower(*a) == *b)
        })
        .collect()
}

/// Un enlace visible en pantalla.
struct Link {
    url: String,
    /// Sus tramos: fila, primera columna y la siguiente a la última (un
    /// enlace largo sigue en la fila de debajo).
    spans: Vec<(u16, u16, u16)>,
}

/// Lo que se añade al texto de la pantalla: palabras resaltadas y enlaces.
#[derive(Default)]
struct Decor {
    cols: u16,
    /// Color impuesto a cada celda (fila × columna).
    colors: Vec<Option<HlColor>>,
    links: Vec<Link>,
}

impl Decor {
    fn color(&self, row: u16, col: u16) -> Option<HlColor> {
        self.colors
            .get(row as usize * self.cols as usize + col as usize)
            .copied()
            .flatten()
    }

    fn link_at(&self, row: u16, col: u16) -> Option<&Link> {
        self.links.iter().find(|link| {
            link.spans
                .iter()
                .any(|(r, start, end)| *r == row && (*start..*end).contains(&col))
        })
    }
}

/// Estado para el que se calculó un [`Decor`]: contenido, desplazamiento del
/// historial, filas, columnas y versión de las reglas.
type DecorKey = (u64, usize, u16, u16, u64);

/// Busca enlaces y palabras resaltadas en lo que muestra la pantalla. Una
/// línea que no cabe y sigue en la fila siguiente se trata entera.
fn decorate(screen: &vt100::Screen, rows: u16, cols: u16, rules: &Compiled) -> Decor {
    let width = cols as usize;
    let mut decor = Decor {
        cols,
        colors: vec![None; rows as usize * width],
        links: vec![],
    };
    if rules.is_idle() {
        return decor;
    }
    let mut row = 0;
    while row < rows {
        let first = row as usize;
        let mut line: Vec<char> = Vec::with_capacity(width);
        loop {
            for col in 0..cols {
                let ch = screen
                    .cell(row, col)
                    .and_then(|cell| cell.contents().chars().next())
                    .unwrap_or(' ');
                line.push(ch);
            }
            let wrapped = screen.row_wrapped(row);
            row += 1;
            if !wrapped || row >= rows {
                break;
            }
        }
        if line.iter().all(|c| *c == ' ') {
            continue;
        }
        let cell_of = |index: usize| ((first + index / width) as u16, (index % width) as u16);
        for (range, color) in rules.spans(&line) {
            for index in range {
                // Lo que la aplicación remota ya coloreó se respeta.
                let (r, c) = cell_of(index);
                let plain = screen
                    .cell(r, c)
                    .is_some_and(|cell| cell.fgcolor() == vt100::Color::Default && !cell.inverse());
                if plain {
                    decor.colors[first * width + index] = Some(color);
                }
            }
        }
        if rules.links {
            for (range, url) in crate::highlight::find_links(&line) {
                let mut spans = vec![];
                let mut start = range.start;
                while start < range.end {
                    let line_row = start / width;
                    let end = range.end.min((line_row + 1) * width);
                    spans.push((
                        (first + line_row) as u16,
                        (start - line_row * width) as u16,
                        (end - line_row * width) as u16,
                    ));
                    start = end;
                }
                decor.links.push(Link { url, spans });
            }
        }
    }
    decor
}

/// Una pestaña de terminal: estado del emulador + conexión con la shell.
pub struct TerminalView {
    parser: vt100::Parser<TermCallbacks>,
    backend: TermBackend,
    cols: u16,
    rows: u16,
    pub state: ConnState,
    /// Conexión SSH subyacente (para abrir SFTP sobre ella).
    pub ssh: Option<SshHandle>,
    /// Secretos pendientes de guardar en el baúl (los recoge la app).
    pub secrets_to_save: Vec<(String, String)>,
    /// Estado de los túneles de la sesión (por índice).
    pub tunnels: Vec<Option<crate::tunnels::TunnelStatus>>,
    /// Selección: (fila, columna) inicial y final, en coordenadas de pantalla.
    selection: Option<((u16, u16), (u16, u16))>,
    /// Ajuste del tamaño de letra respecto al del tema (botones A+ / A−).
    pub font_delta: f32,
    scroll_accum: f32,
    want_focus: bool,
    /// Botón pulsado mientras la aplicación remota controla el ratón.
    mouse_down: Option<u8>,
    mouse_cell: (u16, u16),
    /// Teclas grabadas para una macro (si se está grabando).
    recording: Option<Vec<u8>>,
    /// Ha llegado salida desde la última vez que la aplicación lo miró.
    activity: bool,
    /// Cambia cada vez que cambia el contenido de la pantalla.
    generation: u64,
    /// Enlaces y resaltado de lo que se ve, y para qué estado se calcularon
    /// (contenido, desplazamiento, tamaño y versión de las reglas).
    decor: Option<(DecorKey, Arc<Decor>)>,
    /// Enlace sobre el que se abrió el menú contextual.
    menu_link: Option<String>,
    /// Avisos para el usuario (los recoge la aplicación).
    pub notices: Vec<String>,
    /// La sesión llegó a conectarse: si después se cierra con error es una
    /// caída de la conexión, no un fallo al conectar.
    pub was_connected: bool,
    /// Registro de la sesión en un fichero, si está activo.
    pub log: Option<crate::session_log::SessionLog>,
    /// Búsqueda en curso, si la barra de búsqueda está abierta.
    search: Option<Search>,
    /// Si es `Some`, lo que el usuario introduce se va anotando aquí para
    /// repetirlo en otros terminales (lo recoge la aplicación).
    pub mirror: Option<Vec<TermInput>>,
    /// El terminal tenía el foco del teclado la última vez que se dibujó.
    pub has_focus: bool,
    /// Fondo propio de la sesión (y el identificador de su imagen); si no
    /// hay, se usa el del tema.
    pub backdrop: Option<(Arc<Backdrop>, u64)>,
    /// Consulta del estado del servidor para la barra inferior.
    pub monitor: Option<crate::monitor::Monitor>,
}

pub const INITIAL_COLS: u16 = 100;
pub const INITIAL_ROWS: u16 = 30;

impl TerminalView {
    pub fn new(backend: TermBackend) -> Self {
        let rows = NonZeroU16::new(INITIAL_ROWS).unwrap();
        let cols = NonZeroU16::new(INITIAL_COLS).unwrap();
        Self {
            parser: vt100::Parser::new_with_callbacks(
                rows,
                cols,
                SCROLLBACK,
                TermCallbacks::default(),
            ),
            backend,
            cols: INITIAL_COLS,
            rows: INITIAL_ROWS,
            state: ConnState::Connecting,
            ssh: None,
            secrets_to_save: vec![],
            tunnels: vec![],
            selection: None,
            font_delta: 0.0,
            scroll_accum: 0.0,
            want_focus: true,
            mouse_down: None,
            mouse_cell: (0, 0),
            recording: None,
            activity: false,
            generation: 0,
            decor: None,
            menu_link: None,
            notices: vec![],
            was_connected: false,
            log: None,
            search: None,
            mirror: None,
            has_focus: false,
            backdrop: None,
            monitor: None,
        }
    }

    /// Usa un fondo propio en lugar del del tema.
    pub fn set_backdrop(&mut self, backdrop: Option<&Backdrop>) {
        self.backdrop = backdrop.map(|b| (Arc::new(b.clone()), b.image_key()));
    }

    /// Vuelve a conectar el mismo terminal con otra conexión, conservando
    /// lo que había en pantalla, el historial y el registro.
    pub fn restart(&mut self, backend: TermBackend, message: &str) {
        let _ = self.backend.cmd.send(TermCmd::Close);
        self.backend = backend;
        self.state = ConnState::Connecting;
        self.was_connected = false;
        self.ssh = None;
        self.monitor = None;
        self.tunnels.clear();
        self.mouse_down = None;
        // La aplicación que hubiera a pantalla completa ya no existe: se
        // sale de la pantalla alternativa y se restablecen los modos.
        if self.parser.screen().alternate_screen() {
            self.parser.process(b"\x1b[?1049l");
        }
        self.parser
            .process(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1l\x1b[?2004l\x1b[0m");
        self.note(33, message);
        if let Some(log) = &mut self.log {
            log.note(message);
        }
        // La conexión nueva arranca con el tamaño inicial: se le comunica
        // el real en el siguiente fotograma.
        let _ = self.backend.cmd.send(TermCmd::Resize {
            cols: self.cols,
            rows: self.rows,
        });
    }

    /// Devuelve si ha llegado salida y si ha sonado la campana desde la
    /// última llamada.
    pub fn take_activity(&mut self) -> (bool, bool) {
        let bell = std::mem::take(&mut self.parser.callbacks_mut().bell);
        (std::mem::take(&mut self.activity), bell)
    }

    /// Carpeta de trabajo de la shell, si esta la anuncia (con OSC 7 o en
    /// el título de la ventana). Puede empezar por `~`.
    pub fn working_dir(&self) -> Option<String> {
        let told = self.parser.callbacks();
        told.cwd.clone().or_else(|| dir_from_title(&told.title))
    }

    /// Texto visible actualmente (útil para pruebas y para copiar todo).
    pub fn contents(&self) -> String {
        self.parser.screen().contents()
    }

    pub fn request_focus(&mut self) {
        self.want_focus = true;
    }

    fn note(&mut self, color: u8, text: &str) {
        let line = format!("\r\x1b[{color}m{text}\x1b[0m\r\n");
        self.parser.process(line.as_bytes());
        self.generation += 1;
    }

    /// Enlaces y palabras resaltadas de lo que se ve ahora. Solo se
    /// recalculan cuando cambia la pantalla.
    fn decor(&mut self, rules: &Compiled) -> Arc<Decor> {
        let screen = self.parser.screen();
        let key = (
            self.generation,
            screen.scrollback(),
            self.rows,
            self.cols,
            rules.version,
        );
        if let Some((cached, decor)) = &self.decor
            && *cached == key
        {
            return decor.clone();
        }
        let decor = Arc::new(decorate(screen, self.rows, self.cols, rules));
        self.decor = Some((key, decor.clone()));
        decor
    }

    /// Abre un enlace con el navegador del sistema.
    fn open_link(&mut self, url: &str) {
        if let Err(e) = crate::paths::open_url(url) {
            self.notices
                .push(format!("No se pudo abrir el enlace: {e}"));
        }
    }

    /// Procesa los eventos pendientes de la shell.
    pub fn poll(&mut self) {
        while let Ok(ev) = self.backend.events.try_recv() {
            match ev {
                TermEvent::Status(s) => self.note(33, &s),
                TermEvent::Connected(handle) => {
                    self.ssh = handle;
                    self.state = ConnState::Connected;
                    self.was_connected = true;
                }
                TermEvent::Data(data) => {
                    if let Some(log) = &mut self.log {
                        log.feed(&data);
                    }
                    self.activity |= !data.is_empty();
                    self.parser.process(&data);
                    self.generation += 1;
                }
                TermEvent::SaveSecrets(s) => self.secrets_to_save.extend(s),
                TermEvent::Tunnel { index, status } => {
                    if let crate::tunnels::TunnelStatus::Failed(e) = &status {
                        self.note(31, &format!("Túnel {}: {e}", index + 1));
                    }
                    if self.tunnels.len() <= index {
                        self.tunnels.resize(index + 1, None);
                    }
                    self.tunnels[index] = Some(status);
                }
                TermEvent::Closed(err) => {
                    match &err {
                        Some(e) => self.note(31, &format!("Error: {e}")),
                        None => self.note(33, "[Sesión finalizada]"),
                    }
                    if let Some(log) = &mut self.log {
                        match &err {
                            Some(e) => log.note(&format!("Conexión cerrada: {e}")),
                            None => log.note("Sesión finalizada"),
                        }
                    }
                    self.ssh = None;
                    self.monitor = None;
                    self.tunnels.clear();
                    self.state = ConnState::Closed(err);
                }
            }
        }
    }

    /// Envía bytes a la shell sin registrarlos en la macro en curso.
    fn send_raw(&mut self, data: Vec<u8>) {
        if self.state == ConnState::Connected && !data.is_empty() {
            self.parser.screen_mut().set_scrollback(0);
            let _ = self.backend.cmd.send(TermCmd::Input(data));
        }
    }

    /// Envía entrada de teclado (se graba si hay una macro en curso).
    fn send(&mut self, data: Vec<u8>) {
        if self.state == ConnState::Connected
            && let Some(rec) = &mut self.recording
        {
            rec.extend_from_slice(&data);
        }
        self.send_raw(data);
    }

    /// Envía una secuencia de teclas ya preparada (macros).
    pub fn send_macro(&mut self, bytes: Vec<u8>) {
        self.send_raw(bytes);
    }

    /// Abre la barra de búsqueda (o le devuelve el foco).
    pub fn open_search(&mut self) {
        let search = self.search.get_or_insert_with(Search::default);
        search.focus = true;
    }

    pub fn is_searching(&self) -> bool {
        self.search.is_some()
    }

    fn close_search(&mut self) {
        self.search = None;
        self.want_focus = true;
    }

    /// Todas las líneas del terminal, de la más antigua a la última: el
    /// historial y la pantalla.
    fn history_lines(&mut self) -> Vec<Vec<char>> {
        let saved = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(usize::MAX);
        let max = self.parser.screen().scrollback();
        let rows = self.rows as usize;
        let total = max + rows;
        let mut lines: Vec<Vec<char>> = Vec::with_capacity(total);
        let mut offset = max;
        loop {
            self.parser.screen_mut().set_scrollback(offset);
            // Esta página cubre las líneas [total - rows - offset, total - offset).
            let first = total - rows - offset;
            let page: Vec<String> = self.parser.screen().rows(0, self.cols).collect();
            for line in page.into_iter().skip(lines.len().saturating_sub(first)) {
                lines.push(line.chars().collect());
            }
            if offset == 0 {
                break;
            }
            offset = offset.saturating_sub(rows);
        }
        self.parser.screen_mut().set_scrollback(saved);
        lines
    }

    /// Salta a la coincidencia anterior (más antigua) o a la siguiente y
    /// desplaza el historial para que se vea.
    fn search_step(&mut self, older: bool) {
        let Some(query) = self.search.as_ref().map(|s| s.query.to_lowercase()) else {
            return;
        };
        let needle: Vec<char> = query.chars().collect();
        let lines = self.history_lines();
        let total_lines = lines.len();
        // (línea desde abajo, columna), de la más antigua a la más reciente.
        let matches: Vec<(usize, usize)> = lines
            .iter()
            .enumerate()
            .flat_map(|(i, line)| {
                find_all(line, &needle)
                    .into_iter()
                    .map(move |col| (total_lines - 1 - i, col))
            })
            .collect();
        let rows = self.rows as usize;
        let Some(search) = &mut self.search else {
            return;
        };
        if matches.is_empty() {
            search.current = None;
            search.position = (0, 0);
            return;
        }
        let index = match search
            .current
            .and_then(|c| matches.iter().position(|m| *m == c))
        {
            Some(i) if older => (i + matches.len() - 1) % matches.len(),
            Some(i) => (i + 1) % matches.len(),
            // Sin posición previa se empieza por la más reciente.
            None => matches.len() - 1,
        };
        let current = matches[index];
        search.current = Some(current);
        search.position = (index + 1, matches.len());
        // La línea encontrada, hacia el centro de la pantalla.
        let offset = current.0.saturating_sub(rows / 2);
        self.parser.screen_mut().set_scrollback(offset);
        self.selection = None;
    }

    /// Coincidencias de la búsqueda en las filas visibles: por fila, los
    /// tramos de columnas [inicio, fin) y si es la coincidencia actual.
    fn visible_matches(&self, rows: u16, cols: u16) -> Vec<Vec<(u16, u16, bool)>> {
        let mut out = vec![vec![]; rows as usize];
        let Some(search) = &self.search else {
            return out;
        };
        let needle: Vec<char> = search.query.to_lowercase().chars().collect();
        if needle.is_empty() {
            return out;
        }
        let screen = self.parser.screen();
        let offset = screen.scrollback();
        for row in 0..rows {
            // Texto de la fila y la columna de cada carácter (los de ancho
            // doble ocupan dos columnas).
            let mut chars = vec![];
            let mut columns = vec![];
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let contents = cell.contents();
                chars.push(contents.chars().next().unwrap_or(' '));
                columns.push(col);
            }
            let from_bottom = offset + (rows - 1 - row) as usize;
            for start in find_all(&chars, &needle) {
                let end = start + needle.len();
                let first = columns[start];
                let last = columns.get(end).copied().unwrap_or(cols);
                let current = search.current == Some((from_bottom, start));
                out[row as usize].push((first, last, current));
            }
        }
        out
    }

    /// Entrada del usuario en este terminal: se aplica y, si hay difusión,
    /// se anota para los demás.
    fn input(&mut self, input: TermInput) {
        self.apply_input(&input);
        if let Some(mirror) = &mut self.mirror {
            mirror.push(input);
        }
    }

    /// Aplica una entrada (propia o difundida desde otro terminal),
    /// traduciéndola según el estado de este terminal.
    pub fn apply_input(&mut self, input: &TermInput) {
        match input {
            TermInput::Bytes(bytes) => self.send(bytes.clone()),
            TermInput::Key(key, modifiers) => {
                let app_cursor = self.parser.screen().application_cursor();
                if let Some(bytes) = key_bytes(*key, *modifiers, app_cursor) {
                    self.send(bytes);
                }
            }
            TermInput::Paste(text) => self.paste(text),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.state == ConnState::Connected
    }

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    pub fn start_recording(&mut self) {
        self.recording = Some(vec![]);
    }

    /// Termina la grabación y devuelve lo tecleado.
    pub fn stop_recording(&mut self) -> Vec<u8> {
        self.recording.take().unwrap_or_default()
    }

    pub fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if self.parser.screen().bracketed_paste() {
            // Se eliminan los ESC para que el texto no pueda cerrar el modo.
            let clean = text.replace('\x1b', "");
            self.send(format!("\x1b[200~{clean}\x1b[201~").into_bytes());
        } else {
            self.send(text.into_bytes());
        }
    }

    pub fn close(&mut self) {
        let _ = self.backend.cmd.send(TermCmd::Close);
    }

    fn selection_ordered(&self) -> Option<((u16, u16), (u16, u16))> {
        let (a, b) = self.selection?;
        if a == b {
            return None;
        }
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection_ordered()?;
        let text = self
            .parser
            .screen()
            .contents_between(start.0, start.1, end.0, end.1 + 1);
        (!text.is_empty()).then_some(text)
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        if cols == self.cols && rows == self.rows {
            return;
        }
        let (Some(r), Some(c)) = (NonZeroU16::new(rows), NonZeroU16::new(cols)) else {
            return;
        };
        self.cols = cols;
        self.rows = rows;
        self.parser.set_size(r, c);
        self.selection = None;
        let _ = self.backend.cmd.send(TermCmd::Resize { cols, rows });
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.poll();

        let palette = crate::theme::terminal_palette();
        let font = FontId::monospace((palette.font_size + self.font_delta).clamp(6.0, 60.0));
        let (glyph_w, narrow_w, row_h) = ui.fonts_mut(|f| {
            (
                f.glyph_width(&font, 'M'),
                f.glyph_width(&font, 'i'),
                f.row_height(&font),
            )
        });
        // Con una letra que no es de ancho fijo cada carácter se coloca en
        // su celda; si no, las columnas no cuadrarían.
        let per_cell = palette.letter_spacing != 0.0 || (glyph_w - narrow_w).abs() > 0.01;
        // El interlineado y la separación entre letras del tema agrandan la
        // celda; el texto se centra verticalmente en ella.
        let letter_spacing = palette.letter_spacing;
        let cell_w = (glyph_w + letter_spacing).max(1.0);
        let cell_h = (row_h * palette.line_height).round().max(1.0);
        let text_dy = ((cell_h - row_h) / 2.0).round();

        let avail = ui.available_size();
        let (rect, response) = ui.allocate_exact_size(avail, Sense::click_and_drag());
        let pad = palette.padding;
        let cols = (((rect.width() - 2.0 * pad) / cell_w).floor() as i32).clamp(2, 500) as u16;
        let rows = (((rect.height() - 2.0 * pad) / cell_h).floor() as i32).clamp(1, 300) as u16;
        self.resize(cols, rows);

        let origin = rect.min + Vec2::splat(pad);
        let to_cell = |pos: Pos2| -> (u16, u16) {
            let col = ((pos.x - origin.x) / cell_w)
                .floor()
                .clamp(0.0, (cols - 1) as f32) as u16;
            let row = ((pos.y - origin.y) / cell_h)
                .floor()
                .clamp(0.0, (rows - 1) as f32) as u16;
            (row, col)
        };

        // --- Foco y ratón -------------------------------------------------
        if self.want_focus || response.clicked() || response.drag_started() {
            response.request_focus();
            self.want_focus = false;
        }
        // Si la aplicación remota pide el ratón (htop, mc, vim…) se le envían
        // los eventos; con Mayús pulsada se vuelve a la selección local.
        let mouse_mode = self.parser.screen().mouse_protocol_mode();
        let mouse_encoding = self.parser.screen().mouse_protocol_encoding();
        let app_mouse = mouse_mode != vt100::MouseProtocolMode::None
            && self.state == ConnState::Connected
            && !ui.input(|i| i.modifiers.shift);
        if app_mouse {
            self.selection = None;
            let over = response.contains_pointer();
            for event in ui.input(|i| i.events.clone()) {
                match event {
                    Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        modifiers,
                    } => {
                        let code = match button {
                            egui::PointerButton::Primary => 0,
                            egui::PointerButton::Middle => 1,
                            egui::PointerButton::Secondary => 2,
                            _ => continue,
                        };
                        let (row, col) = to_cell(pos);
                        if pressed {
                            if !over || !rect.contains(pos) {
                                continue;
                            }
                            self.mouse_down = Some(code);
                            self.mouse_cell = (row, col);
                            if let Some(r) =
                                mouse_report(mouse_encoding, code, modifiers, col, row, false)
                            {
                                self.send_raw(r);
                            }
                        } else if self.mouse_down.take().is_some()
                            && mouse_mode != vt100::MouseProtocolMode::Press
                            && let Some(r) =
                                mouse_report(mouse_encoding, code, modifiers, col, row, true)
                        {
                            self.send_raw(r);
                        }
                    }
                    Event::PointerMoved(pos) => {
                        let cell = to_cell(pos);
                        if cell == self.mouse_cell {
                            continue;
                        }
                        let code = match (mouse_mode, self.mouse_down) {
                            (vt100::MouseProtocolMode::AnyMotion, down) => {
                                (over && rect.contains(pos) || down.is_some())
                                    .then_some(down.unwrap_or(3))
                            }
                            (vt100::MouseProtocolMode::ButtonMotion, Some(down)) => Some(down),
                            _ => None,
                        };
                        if let Some(code) = code {
                            self.mouse_cell = cell;
                            let mods = ui.input(|i| i.modifiers);
                            if let Some(r) =
                                mouse_report(mouse_encoding, code + 32, mods, cell.1, cell.0, false)
                            {
                                self.send_raw(r);
                            }
                        }
                    }
                    _ => {}
                }
            }
        } else {
            self.mouse_down = None;
            if response.drag_started()
                && let Some(pos) = response.interact_pointer_pos()
            {
                let cell = to_cell(pos);
                self.selection = Some((cell, cell));
            } else if response.dragged()
                && let Some(pos) = response.interact_pointer_pos()
                && let Some(sel) = &mut self.selection
            {
                sel.1 = to_cell(pos);
            } else if response.clicked() {
                self.selection = None;
            }
        }

        // --- Enlaces --------------------------------------------------------
        // Enlace bajo el puntero: se abre con Ctrl+clic o desde el menú.
        let rules = crate::highlight::current();
        let grid = Rect::from_min_size(
            origin,
            Vec2::new(cols as f32 * cell_w, rows as f32 * cell_h),
        );
        let hovered_url = (!app_mouse)
            .then(|| response.hover_pos())
            .flatten()
            .filter(|pos| grid.contains(*pos))
            .map(to_cell)
            .and_then(|(row, col)| {
                self.decor(&rules)
                    .link_at(row, col)
                    .map(|link| link.url.clone())
            });
        if response.secondary_clicked() {
            self.menu_link = hovered_url.clone();
        }
        if let Some(url) = &hovered_url {
            if ui.input(|i| i.modifiers.command) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                if response.clicked() {
                    self.open_link(url);
                }
            }
            response
                .clone()
                .on_hover_text_at_pointer(format!("{url}\nCtrl+clic para abrirlo"));
        }

        let has_focus = response.has_focus();
        self.has_focus = has_focus;
        if has_focus {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
        }

        // --- Rueda ---------------------------------------------------------
        if response.hovered() {
            let dy = ui.input(|i| i.smooth_scroll_delta().y);
            if dy != 0.0 {
                self.scroll_accum += dy / cell_h;
                let lines = self.scroll_accum.trunc();
                if lines != 0.0 {
                    self.scroll_accum -= lines;
                    let count = (lines.abs() as usize).min(20);
                    if app_mouse {
                        // Un evento por cada ~3 líneas, como una muesca de rueda.
                        let count = count.div_ceil(3);
                        // La aplicación remota recibe la rueda.
                        let (row, col) = ui
                            .input(|i| i.pointer.hover_pos())
                            .map(to_cell)
                            .unwrap_or((0, 0));
                        let code = if lines > 0.0 { 64 } else { 65 };
                        let mods = ui.input(|i| i.modifiers);
                        for _ in 0..count {
                            if let Some(r) =
                                mouse_report(mouse_encoding, code, mods, col, row, false)
                            {
                                self.send_raw(r);
                            }
                        }
                    } else if self.parser.screen().alternate_screen() {
                        // Pantalla alternativa (less, man…): la rueda son flechas.
                        let key = if lines > 0.0 {
                            Key::ArrowUp
                        } else {
                            Key::ArrowDown
                        };
                        let app_cursor = self.parser.screen().application_cursor();
                        if let Some(bytes) = key_bytes(key, egui::Modifiers::NONE, app_cursor) {
                            self.send_raw(bytes.repeat(count));
                        }
                    } else {
                        let current = self.parser.screen().scrollback() as i64;
                        let next = (current + lines as i64).max(0) as usize;
                        self.parser.screen_mut().set_scrollback(next);
                        self.selection = None;
                    }
                }
            }
        }

        // --- Teclado ------------------------------------------------------
        if has_focus {
            let events = ui.input(|i| i.events.clone());
            for event in events {
                match event {
                    Event::Text(text) => {
                        let mods = ui.input(|i| i.modifiers);
                        // Alt+letra ya se envía como ESC+letra desde Event::Key.
                        if mods.alt && !mods.ctrl && text.is_ascii() {
                            continue;
                        }
                        self.input(TermInput::Bytes(text.into_bytes()));
                    }
                    Event::Paste(text) => self.input(TermInput::Paste(text)),
                    Event::Copy => match self.selected_text() {
                        Some(text) => {
                            ui.ctx().copy_text(text);
                            self.selection = None;
                        }
                        // Sin selección, Ctrl+C es la interrupción habitual.
                        None => self.input(TermInput::Bytes(vec![0x03])),
                    },
                    Event::Cut => self.input(TermInput::Bytes(vec![0x18])),
                    Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        if modifiers.shift && key == Key::PageUp {
                            let cur = self.parser.screen().scrollback();
                            self.parser
                                .screen_mut()
                                .set_scrollback(cur + rows as usize / 2);
                        } else if modifiers.shift && key == Key::PageDown {
                            let cur = self.parser.screen().scrollback();
                            self.parser
                                .screen_mut()
                                .set_scrollback(cur.saturating_sub(rows as usize / 2));
                        } else if modifiers.ctrl && modifiers.shift && key == Key::F {
                            self.open_search();
                        } else {
                            self.input(TermInput::Key(key, modifiers));
                        }
                    }
                    _ => {}
                }
            }
        }

        // --- Menú contextual ----------------------------------------------
        let selected = self.selected_text();
        let mut paste_request = false;
        let menu_link = self.menu_link.clone();
        let mut open_link = None;
        // (Si la aplicación remota usa el ratón, el clic derecho es suyo.)
        let menu_response = if app_mouse { None } else { Some(&response) };
        menu_response.map(|r| {
            r.context_menu(|ui| {
                if let Some(url) = &menu_link {
                    if ui.button("🔗 Abrir enlace").on_hover_text(url).clicked() {
                        open_link = Some(url.clone());
                        ui.close();
                    }
                    if ui.button("Copiar enlace").clicked() {
                        ui.ctx().copy_text(url.clone());
                        ui.close();
                    }
                    ui.separator();
                }
                if ui
                    .add_enabled(selected.is_some(), egui::Button::new("Copiar"))
                    .clicked()
                {
                    if let Some(text) = &selected {
                        ui.ctx().copy_text(text.clone());
                    }
                    ui.close();
                }
                if ui.button("Pegar").clicked() {
                    paste_request = true;
                    ui.close();
                }
                if ui.button("Copiar toda la pantalla").clicked() {
                    ui.ctx().copy_text(self.parser.screen().contents());
                    ui.close();
                }
                if ui.button("Limpiar historial").clicked() {
                    self.parser.screen_mut().clear_scrollback();
                    self.generation += 1;
                    ui.close();
                }
            })
        });
        if let Some(url) = open_link {
            self.open_link(&url);
        }
        if paste_request {
            // El portapapeles se pide al sistema; llega como Event::Paste.
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            self.want_focus = true;
        }

        // --- Dibujo -------------------------------------------------------
        if !ui.is_rect_visible(rect) {
            return;
        }
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, palette.background);
        match &self.backdrop {
            Some((backdrop, key)) => backdrop.paint(&painter, rect, *key, palette.foreground),
            None => {
                palette
                    .backdrop
                    .paint(&painter, rect, palette.backdrop_key, palette.foreground)
            }
        }

        let selection = self.selection_ordered();
        let in_selection = |row: u16, col: u16| -> bool {
            selection.is_some_and(|(s, e)| (row, col) >= s && (row, col) <= e)
        };

        // Coincidencias de la búsqueda, por debajo del texto.
        let warn = crate::theme::warn_color();
        for (row, spans) in self.visible_matches(rows, cols).iter().enumerate() {
            let y = origin.y + row as f32 * cell_h;
            for (start, end, current) in spans {
                let span = Rect::from_min_max(
                    Pos2::new(origin.x + *start as f32 * cell_w, y),
                    Pos2::new(origin.x + *end as f32 * cell_w, y + cell_h),
                );
                let alpha = if *current { 0.75 } else { 0.3 };
                painter.rect_filled(span, 2.0, warn.gamma_multiply(alpha));
            }
        }

        let decor = self.decor(&rules);
        let screen = self.parser.screen();
        for row in 0..rows {
            let y = origin.y + row as f32 * cell_h;
            // Fondo: tramos consecutivos del mismo color.
            let mut bg_run: Option<(u16, Color32)> = None;
            // Texto: tramos ASCII consecutivos con el mismo color.
            let mut text_run: Option<(u16, Color32, String)> = None;
            let flush_bg = |run: &mut Option<(u16, Color32)>, end: u16| {
                if let Some((start, color)) = run.take() {
                    painter.rect_filled(
                        Rect::from_min_max(
                            Pos2::new(origin.x + start as f32 * cell_w, y),
                            Pos2::new(origin.x + end as f32 * cell_w, y + cell_h),
                        ),
                        0.0,
                        color,
                    );
                }
            };
            let flush_text = |run: &mut Option<(u16, Color32, String)>| {
                if let Some((start, color, text)) = run.take()
                    && !text.trim_end().is_empty()
                {
                    painter.text(
                        Pos2::new(origin.x + start as f32 * cell_w, y + text_dy),
                        Align2::LEFT_TOP,
                        text,
                        font.clone(),
                        color,
                    );
                }
            };

            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let mut fg = match decor.color(row, col) {
                    // Palabra resaltada por una regla de los ajustes.
                    Some(color) => color.resolve(&palette),
                    None => resolve(cell.fgcolor(), palette.foreground, cell.bold(), &palette),
                };
                let mut bg = resolve(cell.bgcolor(), palette.background, false, &palette);
                if cell.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if cell.dim() {
                    fg = fg.gamma_multiply(0.6);
                }
                if in_selection(row, col) {
                    bg = palette.selection_bg;
                    fg = palette.selection_fg;
                }

                // Fondo
                match &bg_run {
                    Some((_, c)) if *c == bg => {}
                    _ => {
                        flush_bg(&mut bg_run, col);
                        if bg != palette.background {
                            bg_run = Some((col, bg));
                        }
                    }
                }

                // Texto
                let contents = cell.contents();
                // Con separación entre letras cada carácter va en su celda:
                // un tramo dibujado de una vez no la respetaría.
                let simple = contents.len() <= 1 && contents.is_ascii() && !per_cell;
                if simple {
                    let ch = contents.chars().next().unwrap_or(' ');
                    match &mut text_run {
                        Some((_, c, text)) if *c == fg => text.push(ch),
                        _ => {
                            flush_text(&mut text_run);
                            text_run = Some((col, fg, ch.to_string()));
                        }
                    }
                } else {
                    // No ASCII (o ancho doble): se dibuja en su celda exacta
                    // para no desalinear el resto de la fila.
                    flush_text(&mut text_run);
                    if !contents.trim().is_empty() {
                        painter.text(
                            Pos2::new(origin.x + col as f32 * cell_w, y + text_dy),
                            Align2::LEFT_TOP,
                            contents,
                            font.clone(),
                            fg,
                        );
                    }
                }
                if cell.underline() {
                    let x = origin.x + col as f32 * cell_w;
                    painter.line_segment(
                        [
                            Pos2::new(x, y + cell_h - 1.5),
                            Pos2::new(x + cell_w, y + cell_h - 1.5),
                        ],
                        Stroke::new(1.0, fg),
                    );
                }
            }
            flush_bg(&mut bg_run, cols);
            flush_text(&mut text_run);
        }

        // Enlace bajo el puntero: subrayado.
        if let Some(link) = hovered_url
            .as_ref()
            .and_then(|url| decor.links.iter().find(|link| link.url == *url))
        {
            let color = ui.visuals().hyperlink_color;
            for (row, start, end) in &link.spans {
                let y = origin.y + (*row + 1) as f32 * cell_h - 1.5;
                painter.line_segment(
                    [
                        Pos2::new(origin.x + *start as f32 * cell_w, y),
                        Pos2::new(origin.x + *end as f32 * cell_w, y),
                    ],
                    Stroke::new(1.0, color),
                );
            }
        }

        // Cursor
        if !screen.hide_cursor() && screen.scrollback() == 0 && self.state == ConnState::Connected {
            let (crow, ccol) = screen.cursor_position();
            let cursor = Rect::from_min_size(
                Pos2::new(
                    origin.x + ccol as f32 * cell_w,
                    origin.y + crow as f32 * cell_h,
                ),
                Vec2::new(cell_w, cell_h),
            );
            if has_focus {
                painter.rect_filled(cursor, 0.0, palette.cursor.gamma_multiply(0.6));
            } else {
                painter.rect_stroke(
                    cursor,
                    0.0,
                    Stroke::new(1.0, palette.cursor.gamma_multiply(0.7)),
                    egui::StrokeKind::Inside,
                );
            }
        }

        // Indicador de historial
        let offset = screen.scrollback();
        if offset > 0 {
            painter.text(
                rect.right_top() + Vec2::new(-8.0, 4.0),
                Align2::RIGHT_TOP,
                format!("⬆ historial ({offset} líneas)"),
                FontId::proportional(12.0),
                crate::theme::warn_color(),
            );
        }
        self.search_bar(ui, rect);
    }

    /// Barra de búsqueda, superpuesta en la esquina superior derecha.
    fn search_bar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let Some(search) = &mut self.search else {
            return;
        };
        let width = 380.0_f32.min(rect.width() - 16.0);
        let bar = Rect::from_min_size(
            Pos2::new(rect.right() - width - 8.0, rect.top() + 22.0),
            Vec2::new(width, 30.0),
        );
        let mut step = None;
        let mut close = false;
        let mut changed = false;
        ui.scope_builder(egui::UiBuilder::new().max_rect(bar), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("🔍");
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut search.query)
                            .hint_text("Buscar en el terminal")
                            .desired_width(width - 190.0),
                    );
                    if search.focus {
                        field.request_focus();
                        search.focus = false;
                    }
                    changed = field.changed();
                    if field.lost_focus() {
                        if ui.input(|i| i.key_pressed(Key::Enter)) {
                            // Intro: anterior; Mayús+Intro: siguiente.
                            step = Some(!ui.input(|i| i.modifiers.shift));
                            search.focus = true;
                        } else if ui.input(|i| i.key_pressed(Key::Escape)) {
                            close = true;
                        }
                    }
                    let (at, of) = search.position;
                    let count = if search.query.is_empty() {
                        String::new()
                    } else if of == 0 {
                        "sin resultados".to_string()
                    } else {
                        format!("{at} de {of}")
                    };
                    ui.weak(count);
                    if ui
                        .small_button("⬆")
                        .on_hover_text("Anterior (Intro)")
                        .clicked()
                    {
                        step = Some(true);
                    }
                    if ui
                        .small_button("⬇")
                        .on_hover_text("Siguiente (Mayús+Intro)")
                        .clicked()
                    {
                        step = Some(false);
                    }
                    if ui.small_button("×").on_hover_text("Cerrar (Esc)").clicked() {
                        close = true;
                    }
                });
            });
        });
        if changed {
            // Al cambiar el texto se vuelve a la coincidencia más reciente.
            search.current = None;
            self.search_step(true);
        } else if let Some(older) = step {
            self.search_step(older);
        }
        if close {
            self.close_search();
        }
    }
}

impl Drop for TerminalView {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_sequences() {
        let none = egui::Modifiers::NONE;
        assert_eq!(key_bytes(Key::Enter, none, false).unwrap(), b"\r");
        assert_eq!(key_bytes(Key::ArrowUp, none, false).unwrap(), b"\x1b[A");
        assert_eq!(key_bytes(Key::ArrowUp, none, true).unwrap(), b"\x1bOA");
        assert_eq!(
            key_bytes(Key::ArrowRight, egui::Modifiers::CTRL, false).unwrap(),
            b"\x1b[1;5C"
        );
        assert_eq!(
            key_bytes(Key::D, egui::Modifiers::CTRL, false).unwrap(),
            [0x04]
        );
        assert_eq!(
            key_bytes(Key::B, egui::Modifiers::ALT, false).unwrap(),
            b"\x1bb"
        );
        assert_eq!(key_bytes(Key::Delete, none, false).unwrap(), b"\x1b[3~");
        assert!(key_bytes(Key::A, none, false).is_none());
    }

    #[test]
    fn mouse_reports() {
        use vt100::MouseProtocolEncoding as Enc;
        let none = egui::Modifiers::NONE;
        // SGR: coordenadas desde 1; soltar termina en 'm'.
        assert_eq!(
            mouse_report(Enc::Sgr, 0, none, 9, 4, false).unwrap(),
            b"\x1b[<0;10;5M"
        );
        assert_eq!(
            mouse_report(Enc::Sgr, 2, none, 0, 0, true).unwrap(),
            b"\x1b[<2;1;1m"
        );
        assert_eq!(
            mouse_report(Enc::Sgr, 64, egui::Modifiers::CTRL, 299, 99, false).unwrap(),
            b"\x1b[<80;300;100M"
        );
        // Clásico: todo desplazado 32; soltar es el botón 3.
        assert_eq!(
            mouse_report(Enc::Default, 0, none, 0, 0, false).unwrap(),
            [0x1b, b'[', b'M', 32, 33, 33]
        );
        assert_eq!(
            mouse_report(Enc::Default, 1, none, 1, 1, true).unwrap(),
            [0x1b, b'[', b'M', 35, 34, 34]
        );
        assert!(mouse_report(Enc::Default, 0, none, 250, 0, false).is_none());
        // UTF-8 admite columnas grandes.
        let r = mouse_report(Enc::Utf8, 0, none, 250, 0, false).unwrap();
        assert_eq!(&r[..3], b"\x1b[M");
        assert_eq!(
            std::str::from_utf8(&r[3..]).unwrap().chars().nth(1),
            char::from_u32(283)
        );
    }

    #[test]
    fn color_cube() {
        let palette = crate::theme::terminal_palette();
        assert_eq!(indexed_color(16, &palette), Color32::from_rgb(0, 0, 0));
        assert_eq!(
            indexed_color(231, &palette),
            Color32::from_rgb(255, 255, 255)
        );
        assert_eq!(indexed_color(232, &palette), Color32::from_rgb(8, 8, 8));
        // Los 16 primeros salen del tema.
        assert_eq!(indexed_color(1, &palette), palette.ansi[1]);
    }

    #[cfg(unix)]
    /// Terminal conectado a una shell de mentira: devuelve lo que se le
    /// envía y deja inyectarle la salida de la «aplicación remota».
    fn fake_terminal() -> (
        TerminalView,
        mpsc::UnboundedReceiver<TermCmd>,
        std::sync::mpsc::Sender<TermEvent>,
    ) {
        let (cmd, sent) = mpsc::unbounded_channel();
        let (remote, events) = std::sync::mpsc::channel();
        let mut view = TerminalView::new(TermBackend { cmd, events });
        remote.send(TermEvent::Connected(None)).unwrap();
        view.poll();
        (view, sent, remote)
    }

    fn sent_bytes(sent: &mut mpsc::UnboundedReceiver<TermCmd>) -> Vec<Vec<u8>> {
        let mut out = vec![];
        while let Ok(cmd) = sent.try_recv() {
            if let TermCmd::Input(bytes) = cmd {
                out.push(bytes);
            }
        }
        out
    }

    #[test]
    fn mirrored_input_is_translated_by_each_terminal() {
        let (mut a, mut sent_a, _remote_a) = fake_terminal();
        let (mut b, mut sent_b, remote_b) = fake_terminal();
        // En el segundo hay una aplicación a pantalla completa: flechas en
        // modo aplicación y pegado entre corchetes.
        remote_b
            .send(TermEvent::Data(b"\x1b[?1h\x1b[?2004h".to_vec()))
            .unwrap();
        b.poll();

        // Sin difusión no se anota nada.
        a.input(TermInput::Bytes(b"solo aqui".to_vec()));
        assert!(a.mirror.is_none());
        assert_eq!(sent_bytes(&mut sent_a), vec![b"solo aqui".to_vec()]);

        // Con difusión, lo tecleado en el primero se repite en el segundo.
        a.mirror = Some(vec![]);
        a.input(TermInput::Bytes(b"hola".to_vec()));
        a.input(TermInput::Key(Key::ArrowUp, egui::Modifiers::NONE));
        a.input(TermInput::Key(Key::Enter, egui::Modifiers::NONE));
        a.input(TermInput::Paste("uno\ndos".into()));
        let mirrored = a.mirror.take().unwrap();
        assert_eq!(mirrored.len(), 4);
        for input in &mirrored {
            b.apply_input(input);
        }
        assert_eq!(
            sent_bytes(&mut sent_a),
            vec![
                b"hola".to_vec(),
                b"\x1b[A".to_vec(),
                b"\r".to_vec(),
                b"uno\rdos".to_vec()
            ]
        );
        // Mismo contenido, pero traducido a su manera.
        assert_eq!(
            sent_bytes(&mut sent_b),
            vec![
                b"hola".to_vec(),
                b"\x1bOA".to_vec(),
                b"\r".to_vec(),
                b"\x1b[200~uno\rdos\x1b[201~".to_vec()
            ]
        );
        // Lo que llega por difusión no se vuelve a difundir.
        assert!(b.mirror.is_none());

        // Un terminal desconectado no recibe nada.
        remote_b.send(TermEvent::Closed(None)).unwrap();
        b.poll();
        b.apply_input(&TermInput::Bytes(b"perdido".to_vec()));
        assert!(sent_bytes(&mut sent_b).is_empty());
    }

    #[test]
    fn working_directory_is_read_from_the_shell() {
        let (mut view, _sent, remote) = fake_terminal();
        assert_eq!(view.working_dir(), None);
        // El título que pone el PS1 habitual de Debian, Ubuntu o Fedora.
        let feed = |view: &mut TerminalView, bytes: &[u8]| {
            remote.send(TermEvent::Data(bytes.to_vec())).unwrap();
            view.poll();
        };
        feed(&mut view, b"\x1b]0;root@web: ~\x07root@web:~# ");
        assert_eq!(view.working_dir().as_deref(), Some("~"));
        feed(&mut view, b"\x1b]2;deploy@web:/var/www/mi sitio\x07");
        assert_eq!(view.working_dir().as_deref(), Some("/var/www/mi sitio"));
        feed(&mut view, b"\x1b]0;root@web: ~/proyecto\x1b\\");
        assert_eq!(view.working_dir().as_deref(), Some("~/proyecto"));
        // Un título que no es una carpeta (vim, top…) no cuenta.
        feed(&mut view, b"\x1b]0;vim notas.txt\x07");
        assert_eq!(view.working_dir(), None);
        // La salida y la campana se anotan una sola vez.
        assert_eq!(view.take_activity(), (true, false));
        assert_eq!(view.take_activity(), (false, false));
        feed(&mut view, b"ya\x07");
        assert_eq!(view.take_activity(), (true, true));
        // OSC 7 manda sobre el título.
        feed(&mut view, b"\x1b]7;file://web/srv/datos%20nuevos\x07");
        assert_eq!(view.working_dir().as_deref(), Some("/srv/datos nuevos"));
        assert_eq!(dir_from_url("http://x/y"), None);
        assert_eq!(dir_from_title("sin arroba: /tmp"), None);
        assert_eq!(dir_from_title("u@h: no-es-ruta"), None);
    }

    #[test]
    fn links_and_highlights_follow_the_screen() {
        let (mut view, _sent, events) = fake_terminal();
        let feed = |view: &mut TerminalView, data: &[u8]| {
            events.send(TermEvent::Data(data.to_vec())).unwrap();
            view.poll();
        };
        let rules = Compiled::new(&crate::highlight::Highlight::default());
        // Un enlace más largo que la fila sigue en la de debajo.
        let long = format!("https://ejemplo.com/{}", "a".repeat(100));
        feed(
            &mut view,
            format!("ver {long}.\r\nERROR: algo \x1b[32mfailed\x1b[0m; todo OK\r\n").as_bytes(),
        );
        let decor = view.decor(&rules);
        assert_eq!(decor.links.len(), 1);
        assert_eq!(decor.links[0].url, long);
        assert_eq!(decor.links[0].spans, [(0, 4, 100), (1, 0, 24)]);
        assert!(decor.link_at(0, 3).is_none());
        assert!(decor.link_at(0, 4).is_some());
        assert!(decor.link_at(1, 23).is_some());
        // El punto final no es del enlace.
        assert!(decor.link_at(1, 24).is_none());
        // «ERROR» y «OK» se pintan; «failed» ya venía en verde y se respeta.
        assert_eq!(decor.color(2, 0), Some(HlColor::Red));
        assert_eq!(decor.color(2, 4), Some(HlColor::Red));
        assert_eq!(decor.color(2, 5), None);
        assert_eq!(decor.color(2, 12), None);
        assert_eq!(decor.color(2, 25), Some(HlColor::Green));
        // Sin cambios en pantalla se reutiliza lo calculado.
        assert!(Arc::ptr_eq(&decor, &view.decor(&rules)));
        feed(&mut view, b"warning\r\n");
        let decor = view.decor(&rules);
        assert_eq!(decor.color(3, 0), Some(HlColor::Yellow));
        // Al desplazar el historial, los enlaces se mueven con el texto.
        feed(&mut view, "\r\n".repeat(27).as_bytes());
        assert!(view.decor(&rules).links.is_empty());
        view.parser.screen_mut().set_scrollback(2);
        assert_eq!(view.decor(&rules).links[0].spans, [(0, 4, 100), (1, 0, 24)]);
        assert_eq!(view.decor(&rules).color(2, 0), Some(HlColor::Red));
        // Con las reglas apagadas no hay nada que pintar.
        let off = Compiled::new(&crate::highlight::Highlight {
            enabled: false,
            links: false,
            rules: vec![],
        });
        let decor = view.decor(&off);
        assert!(decor.links.is_empty() && decor.colors.iter().all(Option::is_none));
    }

    #[test]
    fn restart_keeps_the_history() {
        let (mut view, _sent, remote) = fake_terminal();
        remote
            .send(TermEvent::Data(b"antes de la caida\r\n".to_vec()))
            .unwrap();
        remote
            .send(TermEvent::Closed(Some("Connection reset".into())))
            .unwrap();
        view.poll();
        assert!(view.was_connected && !view.is_connected());

        let (cmd, mut sent) = mpsc::unbounded_channel();
        let (remote, events) = std::sync::mpsc::channel();
        view.restart(TermBackend { cmd, events }, "[Reconectando…]");
        assert_eq!(view.state, ConnState::Connecting);
        assert!(!view.was_connected);
        // A la conexión nueva se le dice el tamaño del terminal.
        assert!(matches!(sent.try_recv(), Ok(TermCmd::Resize { .. })));
        remote.send(TermEvent::Connected(None)).unwrap();
        remote
            .send(TermEvent::Data(b"despues\r\n".to_vec()))
            .unwrap();
        view.poll();
        let text = view.contents();
        assert!(text.contains("antes de la caida"), "{text}");
        assert!(
            text.contains("[Reconectando…]") && text.contains("despues"),
            "{text}"
        );
        assert!(view.is_connected() && view.was_connected);
    }

    #[test]
    fn search_walks_through_screen_and_history() {
        let (mut view, _sent, remote) = fake_terminal();
        view.resize(40, 5);
        // 30 líneas en un terminal de 5 filas: casi todo queda en el historial.
        let mut output = String::new();
        for i in 1..=30 {
            let word = if i % 10 == 0 { "ERROR" } else { "ok" };
            output.push_str(&format!("linea {i:02} {word}\r\n"));
        }
        output.push_str("fin error error");
        remote.send(TermEvent::Data(output.into_bytes())).unwrap();
        view.poll();
        let lines = view.history_lines();
        assert_eq!(lines.len(), 31);
        assert_eq!(lines[0].iter().collect::<String>(), "linea 01 ok");
        assert_eq!(lines[30].iter().collect::<String>(), "fin error error");
        // Leer el historial no mueve la vista.
        assert_eq!(view.parser.screen().scrollback(), 0);

        view.open_search();
        view.search.as_mut().unwrap().query = "Error".into();
        // Empieza por la más reciente (la segunda de la última línea)…
        view.search_step(true);
        let s = view.search.as_ref().unwrap();
        assert_eq!((s.current, s.position), (Some((0, 10)), (5, 5)));
        // …y hacia atrás: la primera de esa línea, luego la línea 30.
        view.search_step(true);
        assert_eq!(view.search.as_ref().unwrap().current, Some((0, 4)));
        view.search_step(true);
        let s = view.search.as_ref().unwrap();
        assert_eq!((s.current, s.position), (Some((1, 9)), (3, 5)));
        // La línea 10 está en el historial: la vista se desplaza hasta ella.
        view.search_step(true);
        view.search_step(true);
        let s = view.search.as_ref().unwrap();
        assert_eq!((s.current, s.position), (Some((21, 9)), (1, 5)));
        assert!(view.parser.screen().scrollback() >= 17);
        let shown = view.visible_matches(5, 40);
        assert!(
            shown
                .iter()
                .flatten()
                .any(|(start, end, current)| { (*start, *end, *current) == (9, 14, true) })
        );
        // Desde la primera se da la vuelta; hacia delante también.
        view.search_step(true);
        assert_eq!(view.search.as_ref().unwrap().position, (5, 5));
        view.search_step(false);
        assert_eq!(view.search.as_ref().unwrap().position, (1, 5));

        // Sin resultados.
        view.search.as_mut().unwrap().query = "no está".into();
        view.search_step(true);
        let s = view.search.as_ref().unwrap();
        assert_eq!((s.current, s.position), (None, (0, 0)));
        view.close_search();
        assert!(!view.is_searching());
    }

    #[test]
    fn local_shell_runs_commands() {
        let waker: Waker = std::sync::Arc::new(|| {});
        let backend = spawn_local_shell(waker, 80, 24).unwrap();
        let mut view = TerminalView::new(backend);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut sent = false;
        loop {
            view.poll();
            if view.state == ConnState::Connected && !sent {
                view.send(b"echo god$((40+2))remote\r".to_vec());
                sent = true;
            }
            if view.contents().contains("god42remote") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "sin salida: {}",
                view.contents()
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}
