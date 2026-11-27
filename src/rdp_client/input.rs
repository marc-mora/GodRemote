//! Traducción de la entrada de la interfaz a eventos RDP.

use ironrdp_connector::DesktopSize;
use ironrdp_pdu::input::MousePdu;
use ironrdp_pdu::input::fast_path::{FastPathInputEvent, KeyboardFlags};
use ironrdp_pdu::input::mouse::PointerFlags;
use ironrdp_pdu::rdp::capability_sets::InputFlags;

use crate::desktop::{DesktopInput, KeyCode, MouseButton};

/// Una muesca de rueda, en las unidades de RDP.
const WHEEL_NOTCH: i16 = 120;
const SCANCODE_ENTER: u8 = 0x1C;
const SCANCODE_TAB: u8 = 0x0F;

/// Traduce la entrada de la interfaz a eventos RDP de vía rápida.
///
/// El teclado remoto se trata como uno estadounidense (la distribución que
/// se pide al servidor al conectar). Los caracteres ASCII de un
/// [`DesktopInput::Text`] se teclean con sus códigos de exploración: es lo
/// que entienden todos los servidores y aplicaciones, y permite combinarlos
/// con Alt. Solo el resto viaja como eventos Unicode, que no todos los
/// servidores saben inyectar.
pub(super) struct InputEncoder {
    width: u16,
    height: u16,
    /// El servidor admite eventos de teclado Unicode.
    unicode: bool,
    /// El servidor admite la rueda horizontal.
    horizontal_wheel: bool,
    /// Última posición del puntero enviada al servidor.
    pointer: Option<(u16, u16)>,
    /// Modificadores que la interfaz mantiene pulsados.
    shift: bool,
    control: bool,
    alt: bool,
}

impl InputEncoder {
    pub(super) fn new(size: DesktopSize, flags: InputFlags) -> Self {
        let mut encoder = Self {
            width: size.width,
            height: size.height,
            unicode: false,
            horizontal_wheel: false,
            pointer: None,
            shift: false,
            control: false,
            alt: false,
        };
        encoder.set_capabilities(flags);
        encoder
    }

    /// Lo que el servidor dice admitir (puede cambiar al reactivar la sesión).
    pub(super) fn set_capabilities(&mut self, flags: InputFlags) {
        self.unicode = flags.contains(InputFlags::UNICODE);
        self.horizontal_wheel = flags.contains(InputFlags::TS_MOUSE_HWHEEL);
    }

    /// El escritorio remoto ha cambiado de tamaño.
    pub(super) fn set_size(&mut self, size: DesktopSize) {
        self.width = size.width;
        self.height = size.height;
    }

    /// Añade a `out` los eventos RDP que corresponden a una entrada.
    pub(super) fn push(&mut self, input: DesktopInput, out: &mut Vec<FastPathInputEvent>) {
        match input {
            DesktopInput::Key { key, pressed } => {
                match key {
                    KeyCode::Shift => self.shift = pressed,
                    KeyCode::Control => self.control = pressed,
                    KeyCode::Alt => self.alt = pressed,
                    _ => {}
                }
                out.push(key_event(key, pressed));
            }
            DesktopInput::Text(text) => self.push_text(&text, out),
            DesktopInput::PointerMove { x, y } => self.move_to(x, y, out),
            DesktopInput::PointerButton {
                button,
                pressed,
                x,
                y,
            } => {
                let mut flags = match button {
                    MouseButton::Left => PointerFlags::LEFT_BUTTON,
                    MouseButton::Right => PointerFlags::RIGHT_BUTTON,
                    MouseButton::Middle => PointerFlags::MIDDLE_BUTTON_OR_WHEEL,
                };
                if pressed {
                    flags |= PointerFlags::DOWN;
                }
                self.ensure_at(x, y, out);
                out.push(FastPathInputEvent::MouseEvent(self.mouse(flags, 0, x, y)));
            }
            DesktopInput::Wheel {
                x,
                y,
                delta,
                horizontal,
            } => {
                if horizontal && !self.horizontal_wheel {
                    return;
                }
                let flags = if horizontal {
                    PointerFlags::HORIZONTAL_WHEEL
                } else {
                    PointerFlags::VERTICAL_WHEEL
                };
                // El protocolo solo admite 9 bits de giro por evento: se
                // envía una muesca cada vez.
                let notch = if delta > 0 { WHEEL_NOTCH } else { -WHEEL_NOTCH };
                self.ensure_at(x, y, out);
                for _ in 0..delta.unsigned_abs().min(20) {
                    out.push(FastPathInputEvent::MouseEvent(
                        self.mouse(flags, notch, x, y),
                    ));
                }
            }
            // El portapapeles y el cierre se atienden en el bucle de sesión.
            DesktopInput::ClipboardText(_) | DesktopInput::Close => {}
        }
    }

    /// Recorta unas coordenadas a los límites del escritorio remoto.
    fn clamp(&self, x: u16, y: u16) -> (u16, u16) {
        (
            x.min(self.width.saturating_sub(1)),
            y.min(self.height.saturating_sub(1)),
        )
    }

    fn mouse(&self, flags: PointerFlags, wheel: i16, x: u16, y: u16) -> MousePdu {
        let (x_position, y_position) = self.clamp(x, y);
        MousePdu {
            flags,
            number_of_wheel_rotation_units: wheel,
            x_position,
            y_position,
        }
    }

    fn move_to(&mut self, x: u16, y: u16, out: &mut Vec<FastPathInputEvent>) {
        let event = self.mouse(PointerFlags::MOVE, 0, x, y);
        self.pointer = Some((event.x_position, event.y_position));
        // De una ráfaga de movimientos solo importa el último.
        match out.last_mut() {
            Some(FastPathInputEvent::MouseEvent(last)) if last.flags == PointerFlags::MOVE => {
                *last = event;
            }
            _ => out.push(FastPathInputEvent::MouseEvent(event)),
        }
    }

    /// Los botones y la rueda llevan su posición, pero hay servidores que
    /// solo mueven el puntero con los eventos de movimiento: si la posición
    /// no es la última enviada, se manda antes un movimiento.
    fn ensure_at(&mut self, x: u16, y: u16, out: &mut Vec<FastPathInputEvent>) {
        if self.pointer != Some(self.clamp(x, y)) {
            self.move_to(x, y, out);
        }
    }

    fn push_text(&self, text: &str, out: &mut Vec<FastPathInputEvent>) {
        // Control y Alt pulsados a la vez y aun así llega texto: es AltGr en
        // el teclado local (Windows lo presenta así). El carácter ya viene
        // compuesto, de modo que el remoto no debe ver esos modificadores.
        let alt_gr = self.control && self.alt;
        if alt_gr {
            out.push(key_event(KeyCode::Control, false));
            out.push(key_event(KeyCode::Alt, false));
        }
        for c in text.chars() {
            self.push_char(c, out);
        }
        if alt_gr {
            out.push(key_event(KeyCode::Control, true));
            out.push(key_event(KeyCode::Alt, true));
        }
    }

    fn push_char(&self, c: char, out: &mut Vec<FastPathInputEvent>) {
        match c {
            '\r' => {}
            '\n' => tap(SCANCODE_ENTER, out),
            '\t' => tap(SCANCODE_TAB, out),
            c if c.is_control() => {}
            c => match us_layout_scancode(c) {
                Some((code, shift)) => {
                    // Mayús se pone como pide el carácter y después se deja
                    // como la tiene la interfaz.
                    if shift != self.shift {
                        out.push(key_event(KeyCode::Shift, shift));
                    }
                    tap(code, out);
                    if shift != self.shift {
                        out.push(key_event(KeyCode::Shift, self.shift));
                    }
                }
                None if self.unicode => {
                    let mut units = [0u16; 2];
                    for unit in c.encode_utf16(&mut units) {
                        out.push(FastPathInputEvent::UnicodeKeyboardEvent(
                            KeyboardFlags::empty(),
                            *unit,
                        ));
                        out.push(FastPathInputEvent::UnicodeKeyboardEvent(
                            KeyboardFlags::RELEASE,
                            *unit,
                        ));
                    }
                }
                // Ni está en el teclado estadounidense ni el servidor admite
                // Unicode: no hay forma de teclearlo.
                None => {}
            },
        }
    }
}

fn key_event(key: KeyCode, pressed: bool) -> FastPathInputEvent {
    let (code, extended) = key.scancode();
    scancode_event(code, extended, pressed)
}

fn scancode_event(code: u8, extended: bool, pressed: bool) -> FastPathInputEvent {
    let mut flags = KeyboardFlags::empty();
    if extended {
        flags |= KeyboardFlags::EXTENDED;
    }
    if !pressed {
        flags |= KeyboardFlags::RELEASE;
    }
    FastPathInputEvent::KeyboardEvent(flags, code)
}

/// Pulsa y suelta una tecla.
fn tap(code: u8, out: &mut Vec<FastPathInputEvent>) {
    out.push(scancode_event(code, false, true));
    out.push(scancode_event(code, false, false));
}

/// Código de exploración (y si lleva Mayús) de un carácter ASCII en un
/// teclado estadounidense.
fn us_layout_scancode(c: char) -> Option<(u8, bool)> {
    const ROWS: [(&str, &str, u8); 4] = [
        ("`1234567890-=", "~!@#$%^&*()_+", 0x29),
        ("qwertyuiop[]", "QWERTYUIOP{}", 0x10),
        ("asdfghjkl;'", "ASDFGHJKL:\"", 0x1E),
        ("zxcvbnm,./", "ZXCVBNM<>?", 0x2C),
    ];
    match c {
        ' ' => return Some((0x39, false)),
        '\\' => return Some((0x2B, false)),
        '|' => return Some((0x2B, true)),
        _ => {}
    }
    for (plain, shifted, first) in ROWS {
        // La fila de números empieza en el acento grave (0x29) y sigue en 0x02.
        let code = |index: usize| {
            let index = u8::try_from(index).ok()?;
            Some(match (first, index) {
                (0x29, 0) => 0x29,
                (0x29, n) => 0x01 + n,
                (first, n) => first + n,
            })
        };
        if let Some(index) = plain.find(c) {
            return Some((code(index)?, false));
        }
        if let Some(index) = shifted.find(c) {
            return Some((code(index)?, true));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoder(unicode: bool) -> InputEncoder {
        let flags = if unicode {
            InputFlags::UNICODE
        } else {
            InputFlags::empty()
        };
        InputEncoder::new(
            DesktopSize {
                width: 800,
                height: 600,
            },
            flags,
        )
    }

    fn text(encoder: &mut InputEncoder, text: &str) -> Vec<FastPathInputEvent> {
        let mut out = vec![];
        encoder.push(DesktopInput::Text(text.into()), &mut out);
        out
    }

    fn hold(encoder: &mut InputEncoder, key: KeyCode, pressed: bool) {
        encoder.push(DesktopInput::Key { key, pressed }, &mut vec![]);
    }

    /// Evento de tecla por su código de exploración.
    fn key(code: u8, pressed: bool) -> FastPathInputEvent {
        scancode_event(code, false, pressed)
    }

    const SHIFT: u8 = 0x2A;
    const CONTROL: u8 = 0x1D;
    const ALT: u8 = 0x38;

    #[test]
    fn keys_become_scancodes() {
        let mut out = vec![];
        let mut enc = encoder(true);
        enc.push(
            DesktopInput::Key {
                key: KeyCode::Letter(b'a'),
                pressed: true,
            },
            &mut out,
        );
        enc.push(
            DesktopInput::Key {
                key: KeyCode::Delete,
                pressed: false,
            },
            &mut out,
        );
        assert_eq!(
            out,
            vec![
                FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x1E),
                FastPathInputEvent::KeyboardEvent(
                    KeyboardFlags::EXTENDED | KeyboardFlags::RELEASE,
                    0x53
                ),
            ]
        );
    }

    #[test]
    fn non_ascii_text_is_unicode() {
        let unit = |flags, unit| FastPathInputEvent::UnicodeKeyboardEvent(flags, unit);
        let down = KeyboardFlags::empty();
        let up = KeyboardFlags::RELEASE;
        assert_eq!(
            text(&mut encoder(true), "ñ\n😀\r\u{7}"),
            vec![
                unit(down, 0xF1),
                unit(up, 0xF1),
                key(SCANCODE_ENTER, true),
                key(SCANCODE_ENTER, false),
                // Fuera del plano básico: un par subrogado.
                unit(down, 0xD83D),
                unit(up, 0xD83D),
                unit(down, 0xDE00),
                unit(up, 0xDE00),
                // El retorno de carro y los caracteres de control se omiten.
            ]
        );
        // Si el servidor no admite Unicode no hay cómo teclearlo.
        assert_eq!(text(&mut encoder(false), "ñ€"), vec![]);
    }

    #[test]
    fn ascii_text_is_typed_on_a_us_keyboard() {
        for unicode in [true, false] {
            assert_eq!(
                text(&mut encoder(unicode), "aA1!\t"),
                vec![
                    key(0x1E, true),
                    key(0x1E, false),
                    key(SHIFT, true),
                    key(0x1E, true),
                    key(0x1E, false),
                    key(SHIFT, false),
                    key(0x02, true),
                    key(0x02, false),
                    key(SHIFT, true),
                    key(0x02, true),
                    key(0x02, false),
                    key(SHIFT, false),
                    key(SCANCODE_TAB, true),
                    key(SCANCODE_TAB, false),
                ]
            );
        }
        assert_eq!(us_layout_scancode('`'), Some((0x29, false)));
        assert_eq!(us_layout_scancode('0'), Some((0x0B, false)));
        assert_eq!(us_layout_scancode('='), Some((0x0D, false)));
        assert_eq!(us_layout_scancode('q'), Some((0x10, false)));
        assert_eq!(us_layout_scancode(']'), Some((0x1B, false)));
        assert_eq!(us_layout_scancode('?'), Some((0x35, true)));
        assert_eq!(us_layout_scancode('|'), Some((0x2B, true)));
        assert_eq!(us_layout_scancode(' '), Some((0x39, false)));
        assert_eq!(us_layout_scancode('ñ'), None);
        // Las letras coinciden con las teclas «físicas» de la interfaz.
        for letter in b'a'..=b'z' {
            assert_eq!(
                us_layout_scancode(char::from(letter)),
                Some((KeyCode::Letter(letter).scancode().0, false))
            );
        }
        for digit in 0..=9u8 {
            assert_eq!(
                us_layout_scancode(char::from(b'0' + digit)),
                Some((KeyCode::Digit(digit).scancode().0, false))
            );
        }
    }

    #[test]
    fn typed_text_respects_held_modifiers() {
        // Con Mayús pulsada en la interfaz no se vuelve a pulsar, y para una
        // minúscula (Bloq Mayús local) se suelta un momento.
        let mut enc = encoder(true);
        hold(&mut enc, KeyCode::Shift, true);
        assert_eq!(
            text(&mut enc, "Ab"),
            vec![
                key(0x1E, true),
                key(0x1E, false),
                key(SHIFT, false),
                key(0x30, true),
                key(0x30, false),
                key(SHIFT, true),
            ]
        );
        hold(&mut enc, KeyCode::Shift, false);
        assert_eq!(text(&mut enc, "b"), vec![key(0x30, true), key(0x30, false)]);

        // Alt + letra: la letra se teclea con Alt puesta (atajos de menú).
        hold(&mut enc, KeyCode::Alt, true);
        assert_eq!(text(&mut enc, "f"), vec![key(0x21, true), key(0x21, false)]);

        // Control + Alt y aun así texto: era AltGr; el remoto no debe verlos.
        hold(&mut enc, KeyCode::Control, true);
        assert_eq!(
            text(&mut enc, "@"),
            vec![
                key(CONTROL, false),
                key(ALT, false),
                key(SHIFT, true),
                key(0x03, true),
                key(0x03, false),
                key(SHIFT, false),
                key(CONTROL, true),
                key(ALT, true),
            ]
        );
    }

    #[test]
    fn pointer_events() {
        let mouse = |flags, wheel, x, y| {
            FastPathInputEvent::MouseEvent(MousePdu {
                flags,
                number_of_wheel_rotation_units: wheel,
                x_position: x,
                y_position: y,
            })
        };
        let mut out = vec![];
        let mut enc = encoder(true);
        enc.push(DesktopInput::PointerMove { x: 1, y: 2 }, &mut out);
        // Fuera del escritorio: se recorta al borde. Y de dos movimientos
        // seguidos solo queda el último.
        enc.push(DesktopInput::PointerMove { x: 5000, y: 5000 }, &mut out);
        assert_eq!(out, vec![mouse(PointerFlags::MOVE, 0, 799, 599)]);
        enc.push(
            DesktopInput::PointerButton {
                button: MouseButton::Right,
                pressed: true,
                x: 10,
                y: 20,
            },
            &mut out,
        );
        enc.push(
            DesktopInput::Wheel {
                x: 10,
                y: 20,
                delta: -2,
                horizontal: false,
            },
            &mut out,
        );
        // Rueda horizontal no admitida por el servidor: se descarta.
        enc.push(
            DesktopInput::Wheel {
                x: 10,
                y: 20,
                delta: 1,
                horizontal: true,
            },
            &mut out,
        );
        assert_eq!(
            out,
            vec![
                // El botón llega en otra posición: antes va un movimiento
                // hasta ella (que sustituye al que estaba pendiente).
                mouse(PointerFlags::MOVE, 0, 10, 20),
                mouse(PointerFlags::RIGHT_BUTTON | PointerFlags::DOWN, 0, 10, 20),
                // La rueda ya está donde el puntero: sin movimiento extra.
                mouse(PointerFlags::VERTICAL_WHEEL, -120, 10, 20),
                mouse(PointerFlags::VERTICAL_WHEEL, -120, 10, 20),
            ]
        );
    }
}
