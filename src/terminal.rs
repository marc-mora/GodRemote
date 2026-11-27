//! Emulador de terminal (vt100) dibujado con egui, y shell local.

use std::io::{Read, Write};
use std::num::NonZeroU16;

use anyhow::Result;
use egui::{Align2, Color32, Event, EventFilter, FontId, Key, Pos2, Rect, Sense, Stroke, Vec2};
use tokio::sync::mpsc;

use std::sync::Arc;

use crate::backdrop::Backdrop;
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

/// Una pestaña de terminal: estado del emulador + conexión con la shell.
pub struct TerminalView {
    parser: vt100::Parser,
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
            parser: vt100::Parser::new(rows, cols, SCROLLBACK),
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
            backdrop: None,
            monitor: None,
        }
    }

    /// Usa un fondo propio en lugar del del tema.
    pub fn set_backdrop(&mut self, backdrop: Option<&Backdrop>) {
        self.backdrop = backdrop.map(|b| (Arc::new(b.clone()), b.image_key()));
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
    }

    /// Procesa los eventos pendientes de la shell.
    pub fn poll(&mut self) {
        while let Ok(ev) = self.backend.events.try_recv() {
            match ev {
                TermEvent::Status(s) => self.note(33, &s),
                TermEvent::Connected(handle) => {
                    self.ssh = handle;
                    self.state = ConnState::Connected;
                }
                TermEvent::Data(data) => self.parser.process(&data),
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
        let (glyph_w, row_h) = ui.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
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

        let has_focus = response.has_focus();
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
            let app_cursor = self.parser.screen().application_cursor();
            for event in events {
                match event {
                    Event::Text(text) => {
                        let mods = ui.input(|i| i.modifiers);
                        // Alt+letra ya se envía como ESC+letra desde Event::Key.
                        if mods.alt && !mods.ctrl && text.is_ascii() {
                            continue;
                        }
                        self.send(text.into_bytes());
                    }
                    Event::Paste(text) => self.paste(&text),
                    Event::Copy => match self.selected_text() {
                        Some(text) => {
                            ui.ctx().copy_text(text);
                            self.selection = None;
                        }
                        // Sin selección, Ctrl+C es la interrupción habitual.
                        None => self.send(vec![0x03]),
                    },
                    Event::Cut => self.send(vec![0x18]),
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
                        } else if let Some(bytes) = key_bytes(key, modifiers, app_cursor) {
                            self.send(bytes);
                        }
                    }
                    _ => {}
                }
            }
        }

        // --- Menú contextual ----------------------------------------------
        let selected = self.selected_text();
        let mut paste_request = false;
        // (Si la aplicación remota usa el ratón, el clic derecho es suyo.)
        let menu_response = if app_mouse { None } else { Some(&response) };
        menu_response.map(|r| {
            r.context_menu(|ui| {
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
                    ui.close();
                }
            })
        });
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
                let mut fg = resolve(cell.fgcolor(), palette.foreground, cell.bold(), &palette);
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
                let simple = contents.len() <= 1 && contents.is_ascii() && letter_spacing == 0.0;
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
