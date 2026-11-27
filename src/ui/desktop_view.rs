//! Vista de un escritorio remoto integrado (RDP o VNC): muestra la imagen
//! y traduce ratón, teclado y portapapeles.

use std::collections::BTreeSet;

use egui::{Color32, Event, EventFilter, Key, Pos2, Rect, Sense, TextureHandle, TextureOptions};

use crate::desktop::{DesktopBackend, DesktopEvent, DesktopInput, KeyCode, MouseButton};

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

pub struct DesktopView {
    backend: DesktopBackend,
    pub state: DesktopState,
    texture: Option<TextureHandle>,
    texture_epoch: u64,
    /// Tamaño del escritorio remoto, en píxeles.
    size: (u32, u32),
    pub secrets_to_save: Vec<(String, String)>,
    /// Teclas (incluidos modificadores) pulsadas en el equipo remoto.
    pressed: BTreeSet<u32>,
    pressed_keys: Vec<KeyCode>,
    buttons: [bool; 3],
    last_pointer: Option<(u16, u16)>,
    wheel_accum: egui::Vec2,
    /// Sincronizar el portapapeles de texto.
    clipboard: bool,
    /// Texto del portapapeles remoto pendiente de copiar al local.
    remote_clipboard: Option<String>,
    want_focus: bool,
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
            pressed: BTreeSet::new(),
            pressed_keys: vec![],
            buttons: [false; 3],
            last_pointer: None,
            wheel_accum: egui::Vec2::ZERO,
            clipboard,
            remote_clipboard: None,
            want_focus: true,
        }
    }

    pub fn request_focus(&mut self) {
        self.want_focus = true;
    }

    pub fn close(&mut self) {
        let _ = self.backend.input.send(DesktopInput::Close);
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

    fn key(&mut self, key: KeyCode, pressed: bool) {
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
            self.send(DesktopInput::Key { key, pressed });
        }
    }

    /// Pulsa y suelta una combinación (p. ej. Ctrl+Alt+Supr).
    pub fn send_combo(&mut self, keys: &[KeyCode]) {
        for key in keys {
            self.send(DesktopInput::Key {
                key: *key,
                pressed: true,
            });
        }
        for key in keys.iter().rev() {
            self.send(DesktopInput::Key {
                key: *key,
                pressed: false,
            });
        }
    }

    /// Suelta todo lo que esté pulsado en el equipo remoto.
    fn release_all(&mut self) {
        for key in std::mem::take(&mut self.pressed_keys) {
            self.send(DesktopInput::Key {
                key,
                pressed: false,
            });
        }
        self.pressed.clear();
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
                DesktopEvent::SaveSecrets(s) => self.secrets_to_save.extend(s),
                DesktopEvent::Closed(err) => {
                    self.pressed.clear();
                    self.pressed_keys.clear();
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
        } else if !self.pressed_keys.is_empty() {
            // Al perder el foco no deben quedar teclas «pegadas» en remoto.
            self.release_all();
        }

        // --- Modificadores ----------------------------------------------------
        let modifiers = ui.input(|i| i.modifiers);
        if has_focus {
            for (key, wanted) in wanted_modifiers(modifiers) {
                if wanted != self.pressed.contains(&key.keysym()) {
                    self.key(key, wanted);
                }
            }
        }

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
                Event::Text(text) if has_focus => {
                    // Alt+tecla ya se envía como tecla (atajos de menú).
                    if modifiers.alt && !modifiers.ctrl {
                        continue;
                    }
                    self.send(DesktopInput::Text(text));
                }
                Event::Key {
                    key,
                    pressed,
                    modifiers,
                    ..
                } if has_focus => {
                    let Some(code) = key_code(key) else {
                        continue;
                    };
                    if !pressed {
                        self.key(code, false);
                        continue;
                    }
                    let altgr = modifiers.ctrl && modifiers.alt;
                    let shortcut = (modifiers.ctrl || modifiers.alt || modifiers.mac_cmd) && !altgr;
                    if !is_text_key(code) || shortcut {
                        self.key(code, true);
                    }
                }
                // egui convierte Ctrl+C / Ctrl+X / Ctrl+V en estos eventos: se
                // reenvían como la tecla correspondiente (Ctrl ya está pulsado).
                Event::Copy if has_focus => self.send_combo(&[KeyCode::Letter(b'c')]),
                Event::Cut if has_focus => self.send_combo(&[KeyCode::Letter(b'x')]),
                Event::Paste(text) if has_focus => {
                    if self.clipboard {
                        self.send(DesktopInput::ClipboardText(text));
                    }
                    self.send_combo(&[KeyCode::Letter(b'v')]);
                }
                _ => {}
            }
        }
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
