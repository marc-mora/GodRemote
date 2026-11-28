//! Vista de un escritorio remoto integrado (RDP o VNC): muestra la imagen
//! y traduce ratón, teclado y portapapeles.

use std::collections::BTreeSet;

use egui::{Color32, Event, EventFilter, Key, Pos2, Rect, Sense, TextureHandle, TextureOptions};

use crate::desktop::{
    CursorShape, DesktopBackend, DesktopCursor, DesktopEvent, DesktopInput, KeyCode, MouseButton,
};

#[derive(Debug, Clone, PartialEq)]
pub enum DesktopState {
    Connecting(String),
    Connected,
    Closed(Option<String>),
}

/// Traduce una tecla de egui a una tecla física del escritorio remoto.
pub fn key_code(key: Key) -> Option<KeyCode> {
    Some(match key {
        Key::Enter => KeyCode::Enter,
        Key::Escape => KeyCode::Escape,
        Key::Backspace => KeyCode::Backspace,
        Key::Tab => KeyCode::Tab,
        Key::Space => KeyCode::Space,
        Key::ArrowLeft => KeyCode::Left,
        Key::ArrowRight => KeyCode::Right,
        Key::ArrowUp => KeyCode::Up,
        Key::ArrowDown => KeyCode::Down,
        Key::Home => KeyCode::Home,
        Key::End => KeyCode::End,
        Key::PageUp => KeyCode::PageUp,
        Key::PageDown => KeyCode::PageDown,
        Key::Insert => KeyCode::Insert,
        Key::Delete => KeyCode::Delete,
        Key::Minus => KeyCode::Minus,
        Key::Equals => KeyCode::Equal,
        Key::OpenBracket => KeyCode::BracketLeft,
        Key::CloseBracket => KeyCode::BracketRight,
        Key::Backslash => KeyCode::Backslash,
        Key::Semicolon => KeyCode::Semicolon,
        Key::Quote => KeyCode::Quote,
        Key::Backtick => KeyCode::Backquote,
        Key::Comma => KeyCode::Comma,
        Key::Period => KeyCode::Period,
        Key::Slash => KeyCode::Slash,
        Key::Num0 => KeyCode::Digit(0),
        Key::Num1 => KeyCode::Digit(1),
        Key::Num2 => KeyCode::Digit(2),
        Key::Num3 => KeyCode::Digit(3),
        Key::Num4 => KeyCode::Digit(4),
        Key::Num5 => KeyCode::Digit(5),
        Key::Num6 => KeyCode::Digit(6),
        Key::Num7 => KeyCode::Digit(7),
        Key::Num8 => KeyCode::Digit(8),
        Key::Num9 => KeyCode::Digit(9),
        Key::F1 => KeyCode::F(1),
        Key::F2 => KeyCode::F(2),
        Key::F3 => KeyCode::F(3),
        Key::F4 => KeyCode::F(4),
        Key::F5 => KeyCode::F(5),
        Key::F6 => KeyCode::F(6),
        Key::F7 => KeyCode::F(7),
        Key::F8 => KeyCode::F(8),
        Key::F9 => KeyCode::F(9),
        Key::F10 => KeyCode::F(10),
        Key::F11 => KeyCode::F(11),
        Key::F12 => KeyCode::F(12),
        other => {
            let name = other.name().as_bytes();
            if name.len() == 1 && name[0].is_ascii_alphabetic() {
                KeyCode::Letter(name[0].to_ascii_lowercase())
            } else {
                return None;
            }
        }
    })
}

/// `true` si la tecla produce texto por sí sola (se envía como texto, no
/// como tecla, salvo que forme parte de un atajo con Ctrl o Alt).
pub fn is_text_key(key: KeyCode) -> bool {
    matches!(
        key,
        KeyCode::Letter(_)
            | KeyCode::Digit(_)
            | KeyCode::Space
            | KeyCode::Minus
            | KeyCode::Equal
            | KeyCode::BracketLeft
            | KeyCode::BracketRight
            | KeyCode::Backslash
            | KeyCode::Semicolon
            | KeyCode::Quote
            | KeyCode::Backquote
            | KeyCode::Comma
            | KeyCode::Period
            | KeyCode::Slash
    )
}

/// Modificadores que deben estar pulsados en el equipo remoto. Ctrl+Alt a
/// la vez se trata como AltGr (no se reenvía: el carácter llega como texto).
pub fn wanted_modifiers(m: egui::Modifiers) -> [(KeyCode, bool); 3] {
    let altgr = m.ctrl && m.alt;
    [
        (KeyCode::Shift, m.shift),
        (KeyCode::Control, m.ctrl && !altgr),
        (KeyCode::Alt, m.alt && !altgr),
    ]
}

/// Puntero de egui para una de las formas habituales.
pub fn cursor_icon(shape: CursorShape) -> egui::CursorIcon {
    use egui::CursorIcon as Icon;
    match shape {
        CursorShape::Default => Icon::Default,
        CursorShape::Text => Icon::Text,
        CursorShape::Hand => Icon::PointingHand,
        CursorShape::Wait => Icon::Wait,
        CursorShape::Progress => Icon::Progress,
        CursorShape::Crosshair => Icon::Crosshair,
        CursorShape::Move => Icon::Move,
        CursorShape::NotAllowed => Icon::NotAllowed,
        CursorShape::Help => Icon::Help,
        CursorShape::Hidden => Icon::None,
        CursorShape::ResizeHorizontal => Icon::ResizeHorizontal,
        CursorShape::ResizeVertical => Icon::ResizeVertical,
        CursorShape::ResizeN => Icon::ResizeNorth,
        CursorShape::ResizeS => Icon::ResizeSouth,
        CursorShape::ResizeE => Icon::ResizeEast,
        CursorShape::ResizeW => Icon::ResizeWest,
        CursorShape::ResizeNe => Icon::ResizeNorthEast,
        CursorShape::ResizeNw => Icon::ResizeNorthWest,
        CursorShape::ResizeSe => Icon::ResizeSouthEast,
        CursorShape::ResizeSw => Icon::ResizeSouthWest,
    }
}

/// Puntero del escritorio remoto tal como lo pone la interfaz: las formas
/// habituales las dibuja el sistema; las imágenes propias, egui, encima de
/// todo y con el puntero del sistema escondido.
pub struct CursorPainter {
    cursor: DesktopCursor,
    /// Texturas de los fotogramas de la imagen actual (se crean al usarla).
    textures: Vec<TextureHandle>,
}

impl Default for CursorPainter {
    fn default() -> Self {
        Self {
            cursor: DesktopCursor::Shape(CursorShape::Default),
            textures: vec![],
        }
    }
}

impl CursorPainter {
    pub fn set(&mut self, cursor: DesktopCursor) {
        if self.cursor != cursor {
            self.cursor = cursor;
            self.textures.clear();
        }
    }

    /// Pone el puntero para este fotograma. `pos` es dónde está y `scale`,
    /// los puntos de pantalla por píxel remoto.
    pub fn show(&mut self, ctx: &egui::Context, pos: Pos2, scale: f32) {
        let frames = match &self.cursor {
            DesktopCursor::Shape(shape) => {
                ctx.set_cursor_icon(cursor_icon(*shape));
                return;
            }
            DesktopCursor::Image(frames) => frames.clone(),
        };
        if self.textures.len() != frames.len() {
            self.textures = frames
                .iter()
                .map(|frame| {
                    let size = [frame.image.width as usize, frame.image.height as usize];
                    let image = egui::ColorImage::from_rgba_premultiplied(size, &frame.image.rgba);
                    ctx.load_texture("puntero-remoto", image, TextureOptions::LINEAR)
                })
                .collect();
        }
        // Fotograma que toca (los animados dan vueltas sin parar).
        let total: u64 = frames.iter().map(|f| f.delay_ms as u64).sum();
        let mut index = 0;
        if frames.len() > 1 && total > 0 {
            let mut left = (ctx.input(|i| i.time) * 1000.0) as u64 % total;
            for (i, frame) in frames.iter().enumerate() {
                if left < frame.delay_ms as u64 {
                    index = i;
                    let wait = frame.delay_ms as u64 - left;
                    ctx.request_repaint_after(std::time::Duration::from_millis(wait.max(1)));
                    break;
                }
                left -= frame.delay_ms as u64;
            }
        }
        let (Some(frame), Some(texture)) = (frames.get(index), self.textures.get(index)) else {
            return;
        };
        ctx.set_cursor_icon(egui::CursorIcon::None);
        let image = &frame.image;
        let hot = egui::vec2(image.hot_x as f32, image.hot_y as f32) * scale;
        let size = egui::vec2(image.width as f32, image.height as f32) * scale;
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            egui::Id::unique("puntero-remoto"),
        ));
        painter.image(
            texture.id(),
            Rect::from_min_size((pos - hot).round(), size),
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }
}

/// Teclado hacia un escritorio remoto: lleva la cuenta de lo que está
/// pulsado allí y traduce los eventos de teclado de egui. `send` es por
/// donde se le mandan las órdenes.
#[derive(Default)]
pub struct KeyRelay {
    /// Teclas (incluidos modificadores) pulsadas en el equipo remoto.
    pressed: BTreeSet<u32>,
    pressed_keys: Vec<KeyCode>,
    /// Cuándo se pidió el portapapeles al sistema para sincronizarlo: el
    /// «pegar» que llegue enseguida no es una pulsación de Ctrl+V.
    pub clipboard_requested: Option<f64>,
}

impl KeyRelay {
    /// Queda alguna tecla pulsada en el equipo remoto.
    pub fn any_pressed(&self) -> bool {
        !self.pressed_keys.is_empty()
    }

    /// Olvida lo que estuviera pulsado (la sesión se cerró).
    pub fn clear(&mut self) {
        self.pressed.clear();
        self.pressed_keys.clear();
    }

    pub fn key(&mut self, key: KeyCode, pressed: bool, send: &mut dyn FnMut(DesktopInput)) {
        // Identificador estable de la tecla para llevar la cuenta.
        let id = key.keysym();
        let changed = if pressed {
            self.pressed.insert(id)
        } else {
            self.pressed.remove(&id)
        };
        if pressed && changed {
            self.pressed_keys.push(key);
        } else if !pressed {
            self.pressed_keys.retain(|k| *k != key);
        }
        // Las repeticiones de tecla se reenvían; soltar algo no pulsado, no.
        if pressed || changed {
            send(DesktopInput::Key { key, pressed });
        }
    }

    /// Pulsa y suelta una combinación (p. ej. Ctrl+Alt+Supr).
    pub fn combo(keys: &[KeyCode], send: &mut dyn FnMut(DesktopInput)) {
        for key in keys {
            send(DesktopInput::Key {
                key: *key,
                pressed: true,
            });
        }
        for key in keys.iter().rev() {
            send(DesktopInput::Key {
                key: *key,
                pressed: false,
            });
        }
    }

    /// Deja pulsados en el equipo remoto exactamente estos modificadores.
    pub fn sync_modifiers(
        &mut self,
        modifiers: egui::Modifiers,
        send: &mut dyn FnMut(DesktopInput),
    ) {
        for (key, wanted) in wanted_modifiers(modifiers) {
            if wanted != self.pressed.contains(&key.keysym()) {
                self.key(key, wanted, send);
            }
        }
    }

    /// Envía Ctrl+letra, pulsando Ctrl si aún no lo está en el equipo remoto.
    pub fn ctrl_shortcut(&mut self, letter: u8, send: &mut dyn FnMut(DesktopInput)) {
        let held = self.pressed.contains(&KeyCode::Control.keysym());
        if !held {
            self.key(KeyCode::Control, true, send);
        }
        Self::combo(&[KeyCode::Letter(letter)], send);
        if !held {
            self.key(KeyCode::Control, false, send);
        }
    }

    /// Suelta todas las teclas que estén pulsadas en el equipo remoto.
    pub fn release(&mut self, send: &mut dyn FnMut(DesktopInput)) {
        for key in std::mem::take(&mut self.pressed_keys) {
            send(DesktopInput::Key {
                key,
                pressed: false,
            });
        }
        self.pressed.clear();
    }

    /// Trata un evento de teclado (o de copiar y pegar) de quien tiene el
    /// foco. `held` son los modificadores pulsados en este fotograma,
    /// `clipboard` dice si se sincroniza el portapapeles y `now` es la hora
    /// de egui. Devuelve si el evento era de teclado.
    pub fn event(
        &mut self,
        event: &Event,
        held: egui::Modifiers,
        clipboard: bool,
        now: f64,
        send: &mut dyn FnMut(DesktopInput),
    ) -> bool {
        match event {
            Event::Text(text) => {
                // Alt+tecla ya se envía como tecla (atajos de menú).
                if !(held.alt && !held.ctrl) {
                    send(DesktopInput::Text(text.clone()));
                }
            }
            Event::Key {
                key,
                pressed,
                modifiers,
                ..
            } => {
                let Some(code) = key_code(*key) else {
                    return true;
                };
                if !pressed {
                    self.key(code, false, send);
                    return true;
                }
                self.sync_modifiers(*modifiers, send);
                let altgr = modifiers.ctrl && modifiers.alt;
                let shortcut = (modifiers.ctrl || modifiers.alt || modifiers.mac_cmd) && !altgr;
                if !is_text_key(code) || shortcut {
                    self.key(code, true, send);
                }
            }
            // egui convierte Ctrl+C / Ctrl+X / Ctrl+V en estos eventos: se
            // reenvían como la tecla correspondiente (Ctrl ya está pulsado).
            Event::Copy => self.ctrl_shortcut(b'c', send),
            Event::Cut => self.ctrl_shortcut(b'x', send),
            Event::Paste(text) => {
                if clipboard {
                    send(DesktopInput::ClipboardText(text.clone()));
                }
                // Si el portapapeles lo acabamos de pedir nosotros
                // para sincronizarlo, nadie ha pulsado Ctrl+V.
                let requested = self
                    .clipboard_requested
                    .take()
                    .is_some_and(|at| now - at < 0.5);
                if !requested {
                    self.ctrl_shortcut(b'v', send);
                }
            }
            _ => return false,
        }
        true
    }
}

pub struct DesktopView {
    backend: DesktopBackend,
    pub state: DesktopState,
    texture: Option<TextureHandle>,
    texture_epoch: u64,
    /// Tamaño del escritorio remoto, en píxeles.
    size: (u32, u32),
    pub secrets_to_save: Vec<(String, String)>,
    /// Teclado: lo que está pulsado en el equipo remoto.
    keys: KeyRelay,
    buttons: [bool; 3],
    last_pointer: Option<(u16, u16)>,
    wheel_accum: egui::Vec2,
    /// Sincronizar el portapapeles de texto.
    clipboard: bool,
    /// Texto del portapapeles remoto pendiente de copiar al local.
    remote_clipboard: Option<String>,
    want_focus: bool,
    /// Espacio que tuvo la imagen la última vez que se dibujó (en puntos).
    pub last_area: egui::Vec2,
    /// Entregar el portapapeles local al recibir el foco, sin esperar a
    /// Ctrl+V (las aplicaciones X pegan con el botón central o
    /// Mayús+Insert, no con Ctrl+V).
    pub clipboard_on_focus: bool,
    /// Tenía el foco del teclado la última vez que se dibujó, y en qué
    /// fotograma fue (una pestaña oculta no se dibuja).
    was_focused: bool,
    last_frame: u64,
    /// Puntero que pide el escritorio remoto.
    cursor: CursorPainter,
}

impl DesktopView {
    pub fn new(backend: DesktopBackend, clipboard: bool) -> Self {
        Self {
            backend,
            state: DesktopState::Connecting("Conectando…".into()),
            texture: None,
            texture_epoch: 0,
            size: (0, 0),
            secrets_to_save: vec![],
            keys: KeyRelay::default(),
            buttons: [false; 3],
            last_pointer: None,
            wheel_accum: egui::Vec2::ZERO,
            clipboard,
            remote_clipboard: None,
            want_focus: true,
            last_area: egui::Vec2::ZERO,
            clipboard_on_focus: false,
            was_focused: false,
            last_frame: 0,
            cursor: CursorPainter::default(),
        }
    }

    pub fn request_focus(&mut self) {
        self.want_focus = true;
    }

    pub fn close(&mut self) {
        let _ = self.backend.input.send(DesktopInput::Close);
    }

    /// Puntero que pide el escritorio remoto (para quien pinte sus
    /// ventanas por su cuenta).
    pub fn cursor(&mut self) -> &mut CursorPainter {
        &mut self.cursor
    }

    /// Texto que el escritorio remoto dejó en su portapapeles y aún no se
    /// ha copiado al local.
    pub fn take_remote_clipboard(&mut self) -> Option<String> {
        self.remote_clipboard.take()
    }

    /// Tamaño del escritorio remoto, si ya se conoce.
    pub fn remote_size(&self) -> Option<(u32, u32)> {
        (self.state == DesktopState::Connected && self.size.0 > 0).then_some(self.size)
    }

    fn send(&self, input: DesktopInput) {
        if self.state == DesktopState::Connected {
            let _ = self.backend.input.send(input);
        }
    }

    /// Por dónde se mandan las órdenes al escritorio remoto (solo con la
    /// sesión establecida).
    fn sender(&self) -> impl FnMut(DesktopInput) + use<> {
        let (input, connected) = (
            self.backend.input.clone(),
            self.state == DesktopState::Connected,
        );
        move |order| {
            if connected {
                let _ = input.send(order);
            }
        }
    }

    /// Pulsa y suelta una combinación (p. ej. Ctrl+Alt+Supr).
    pub fn send_combo(&mut self, keys: &[KeyCode]) {
        KeyRelay::combo(keys, &mut self.sender());
    }

    /// Suelta todo lo que esté pulsado en el equipo remoto.
    fn release_all(&mut self) {
        let mut send = self.sender();
        self.keys.release(&mut send);
        if let Some((x, y)) = self.last_pointer {
            for (i, button) in [MouseButton::Left, MouseButton::Middle, MouseButton::Right]
                .into_iter()
                .enumerate()
            {
                if std::mem::take(&mut self.buttons[i]) {
                    self.send(DesktopInput::PointerButton {
                        button,
                        pressed: false,
                        x,
                        y,
                    });
                }
            }
        }
    }

    /// Procesa los avisos pendientes del escritorio remoto.
    pub fn poll(&mut self) {
        while let Ok(event) = self.backend.events.try_recv() {
            match event {
                DesktopEvent::Status(s) => {
                    if matches!(self.state, DesktopState::Connecting(_)) {
                        self.state = DesktopState::Connecting(s);
                    }
                }
                DesktopEvent::Connected => self.state = DesktopState::Connected,
                DesktopEvent::ClipboardText(text) => {
                    if self.clipboard {
                        self.remote_clipboard = Some(text);
                    }
                }
                DesktopEvent::Cursor(cursor) => self.cursor.set(cursor),
                DesktopEvent::SaveSecrets(s) => self.secrets_to_save.extend(s),
                DesktopEvent::Closed(err) => {
                    self.keys.clear();
                    self.buttons = [false; 3];
                    self.state = DesktopState::Closed(err);
                }
            }
        }
    }

    /// Copia a la textura lo que haya cambiado en la imagen remota.
    fn update_texture(&mut self, ctx: &egui::Context) {
        let mut frame = self.backend.frame.lock().unwrap();
        let (w, h) = (frame.width() as usize, frame.height() as usize);
        if w == 0 || h == 0 {
            return;
        }
        let options = TextureOptions::LINEAR;
        if self.texture.is_none() || self.texture_epoch != frame.epoch() {
            frame.take_dirty();
            let image = egui::ColorImage::from_rgba_premultiplied([w, h], &frame.pixels);
            match &mut self.texture {
                Some(texture) => texture.set(image, options),
                None => self.texture = Some(ctx.load_texture("escritorio-remoto", image, options)),
            }
            self.texture_epoch = frame.epoch();
            self.size = (w as u32, h as u32);
        } else if let Some(dirty) = frame.take_dirty()
            && let Some(texture) = &mut self.texture
        {
            let (dx, dy) = (dirty.x as usize, dirty.y as usize);
            let (dw, dh) = (dirty.width as usize, dirty.height as usize);
            let mut region = Vec::with_capacity(dw * dh * 4);
            for row in dy..dy + dh {
                let start = (row * w + dx) * 4;
                region.extend_from_slice(&frame.pixels[start..start + dw * 4]);
            }
            let image = egui::ColorImage::from_rgba_premultiplied([dw, dh], &region);
            texture.set_partial([dx, dy], image, options);
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.poll();
        match &self.state {
            DesktopState::Connecting(msg) => {
                ui.centered_and_justified(|ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(msg);
                    });
                });
                return;
            }
            DesktopState::Closed(err) => {
                ui.add_space(24.0);
                ui.vertical_centered(|ui| match err {
                    Some(e) => {
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            format!("Sesión cerrada: {e}"),
                        );
                    }
                    None => {
                        ui.label("Sesión de escritorio remoto finalizada.");
                    }
                });
                return;
            }
            DesktopState::Connected => {}
        }

        self.update_texture(ui.ctx());
        if let Some(text) = self.remote_clipboard.take() {
            ui.ctx().copy_text(text);
        }

        let avail = ui.available_size();
        self.last_area = avail;
        let (area, response) = ui.allocate_exact_size(avail, Sense::click_and_drag());
        let (rw, rh) = (self.size.0.max(1) as f32, self.size.1.max(1) as f32);
        // Se reduce para caber, pero nunca se amplía (se vería borroso).
        let scale = (area.width() / rw).min(area.height() / rh).min(1.0);
        let shown = egui::vec2(rw * scale, rh * scale);
        let rect = Rect::from_center_size(area.center(), shown);
        let painter = ui.painter_at(area);
        painter.rect_filled(area, 0.0, Color32::BLACK);
        if let Some(texture) = &self.texture {
            painter.image(
                texture.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        let size = self.size;
        let to_remote = |pos: Pos2| -> (u16, u16) {
            let x = ((pos.x - rect.min.x) / scale).clamp(0.0, (size.0.max(1) - 1) as f32);
            let y = ((pos.y - rect.min.y) / scale).clamp(0.0, (size.1.max(1) - 1) as f32);
            (x as u16, y as u16)
        };

        // --- Foco -----------------------------------------------------------
        if self.want_focus || response.clicked() || response.drag_started() {
            response.request_focus();
            self.want_focus = false;
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
        } else if self.keys.any_pressed() {
            // Al perder el foco no deben quedar teclas «pegadas» en remoto.
            self.release_all();
        }

        // Los modificadores se sincronizan justo antes de cada tecla (con el
        // estado que tenían en ese instante) y otra vez al final: así una
        // combinación tecleada muy rápido no pierde su Ctrl o su Mayús.
        let modifiers = ui.input(|i| i.modifiers);

        // --- Eventos de ratón y teclado ----------------------------------------
        let over = response.contains_pointer();
        for event in ui.input(|i| i.events.clone()) {
            match event {
                Event::PointerMoved(pos) => {
                    if over || self.buttons.iter().any(|b| *b) {
                        let (x, y) = to_remote(pos);
                        if self.last_pointer != Some((x, y)) {
                            self.last_pointer = Some((x, y));
                            self.send(DesktopInput::PointerMove { x, y });
                        }
                    }
                }
                Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    ..
                } => {
                    let (index, button) = match button {
                        egui::PointerButton::Primary => (0, MouseButton::Left),
                        egui::PointerButton::Middle => (1, MouseButton::Middle),
                        egui::PointerButton::Secondary => (2, MouseButton::Right),
                        _ => continue,
                    };
                    let (x, y) = to_remote(pos);
                    if pressed {
                        if !over || !rect.contains(pos) {
                            continue;
                        }
                        self.buttons[index] = true;
                    } else if !std::mem::take(&mut self.buttons[index]) {
                        continue;
                    }
                    self.last_pointer = Some((x, y));
                    self.send(DesktopInput::PointerButton {
                        button,
                        pressed,
                        x,
                        y,
                    });
                }
                Event::MouseWheel { unit, delta, .. } if over => {
                    let notches = match unit {
                        egui::MouseWheelUnit::Point => delta / 50.0,
                        egui::MouseWheelUnit::Line => delta,
                        egui::MouseWheelUnit::Page => delta * 5.0,
                    };
                    self.wheel_accum += notches;
                    let (x, y) = self.last_pointer.unwrap_or((0, 0));
                    for (value, horizontal) in [
                        (&mut self.wheel_accum.y, false),
                        (&mut self.wheel_accum.x, true),
                    ] {
                        let whole = value.trunc();
                        if whole != 0.0 {
                            *value -= whole;
                            let _ = self.backend.input.send(DesktopInput::Wheel {
                                x,
                                y,
                                delta: whole.clamp(-20.0, 20.0) as i16,
                                horizontal,
                            });
                        }
                    }
                }
                // Teclado y copiar y pegar, de quien tiene el foco.
                event if has_focus => {
                    let now = ui.input(|i| i.time);
                    let mut send = self.sender();
                    self.keys
                        .event(&event, modifiers, self.clipboard, now, &mut send);
                }
                _ => {}
            }
        }
        if has_focus {
            let mut send = self.sender();
            self.keys.sync_modifiers(modifiers, &mut send);
        }
        // El puntero que pide el escritorio remoto, mientras esté encima (o
        // se esté arrastrando algo que empezó en él).
        if let Some(pos) = ui.input(|i| i.pointer.latest_pos())
            && ((over && rect.contains(pos)) || self.buttons.iter().any(|b| *b))
        {
            self.cursor.show(ui.ctx(), pos, scale);
        }
        // Al volver a esta pestaña (o a la ventana) el portapapeles local
        // puede haber cambiado: se pide para entregarlo.
        let window_back = ui.input(|i| {
            i.events
                .iter()
                .any(|event| matches!(event, Event::WindowFocused(true)))
        });
        // Si no se dibujó en el fotograma anterior, la pestaña estaba oculta.
        let frame = ui.ctx().cumulative_frame_nr();
        let shown_before = self.was_focused && frame <= self.last_frame + 1;
        self.last_frame = frame;
        if has_focus && (!shown_before || window_back) && self.clipboard && self.clipboard_on_focus
        {
            self.keys.clipboard_requested = Some(ui.input(|i| i.time));
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
        }
        self.was_focused = has_focus;
    }
}

impl Drop for DesktopView {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_mapping() {
        assert_eq!(key_code(Key::A), Some(KeyCode::Letter(b'a')));
        assert_eq!(key_code(Key::Z), Some(KeyCode::Letter(b'z')));
        assert_eq!(key_code(Key::Num7), Some(KeyCode::Digit(7)));
        assert_eq!(key_code(Key::F12), Some(KeyCode::F(12)));
        assert_eq!(key_code(Key::Delete), Some(KeyCode::Delete));
        // Variantes con Mayús (no son teclas físicas): llegan como texto.
        assert_eq!(key_code(Key::Colon), None);
        assert!(is_text_key(KeyCode::Letter(b'q')) && is_text_key(KeyCode::Space));
        assert!(!is_text_key(KeyCode::Enter) && !is_text_key(KeyCode::F(5)));
    }

    #[test]
    fn altgr_is_not_forwarded_as_ctrl_alt() {
        let m = |ctrl, alt, shift| egui::Modifiers {
            ctrl,
            alt,
            shift,
            ..Default::default()
        };
        let state = |mods| wanted_modifiers(mods).map(|(_, down)| down);
        assert_eq!(state(m(true, false, false)), [false, true, false]);
        assert_eq!(state(m(false, true, true)), [true, false, true]);
        // Ctrl+Alt (AltGr en teclados españoles): no se pulsa nada en remoto.
        assert_eq!(state(m(true, true, false)), [false, false, false]);
    }
}
