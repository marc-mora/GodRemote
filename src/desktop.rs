//! Tipos comunes a los escritorios remotos integrados (RDP y VNC).
//!
//! Un *backend* (cliente RDP o VNC) se ejecuta en segundo plano y se comunica
//! con la interfaz mediante:
//!
//! - un [`Framebuffer`] compartido donde pinta la imagen remota,
//! - un canal de [`DesktopInput`] (teclado, ratón, portapapeles) hacia él,
//! - un canal de [`DesktopEvent`] (estado, portapapeles, cierre) desde él.

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

/// Rectángulo modificado desde la última vez que se consultó.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Imagen del escritorio remoto en RGBA de 8 bits (alfa siempre 255).
pub struct Framebuffer {
    width: u32,
    height: u32,
    /// `width * height * 4` bytes, fila a fila.
    pub pixels: Vec<u8>,
    dirty: Option<DirtyRect>,
    /// Cambia cada vez que cambia el tamaño (la interfaz recrea la textura).
    epoch: u64,
}

pub type SharedFrame = Arc<Mutex<Framebuffer>>;

impl Framebuffer {
    pub fn new(width: u32, height: u32) -> Self {
        let mut fb = Self {
            width: 0,
            height: 0,
            pixels: vec![],
            dirty: None,
            epoch: 0,
        };
        fb.resize(width, height);
        fb
    }

    pub fn shared(width: u32, height: u32) -> SharedFrame {
        Arc::new(Mutex::new(Self::new(width, height)))
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Cambia el tamaño; el contenido queda en negro.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.pixels.clear();
        self.pixels.resize(width as usize * height as usize * 4, 0);
        for px in self.pixels.chunks_exact_mut(4) {
            px[3] = 255;
        }
        self.epoch += 1;
        self.dirty = None;
        self.mark_dirty(0, 0, width, height);
    }

    /// Recorta un rectángulo a los límites de la imagen.
    fn clip(&self, x: u32, y: u32, w: u32, h: u32) -> Option<DirtyRect> {
        if x >= self.width || y >= self.height || w == 0 || h == 0 {
            return None;
        }
        Some(DirtyRect {
            x,
            y,
            width: w.min(self.width - x),
            height: h.min(self.height - y),
        })
    }

    /// Anota que una zona ha cambiado (se acumula en un único rectángulo).
    pub fn mark_dirty(&mut self, x: u32, y: u32, w: u32, h: u32) {
        let Some(r) = self.clip(x, y, w, h) else {
            return;
        };
        self.dirty = Some(match self.dirty {
            None => r,
            Some(d) => {
                let x0 = d.x.min(r.x);
                let y0 = d.y.min(r.y);
                let x1 = (d.x + d.width).max(r.x + r.width);
                let y1 = (d.y + d.height).max(r.y + r.height);
                DirtyRect {
                    x: x0,
                    y: y0,
                    width: x1 - x0,
                    height: y1 - y0,
                }
            }
        });
    }

    /// Devuelve (y olvida) la zona modificada.
    pub fn take_dirty(&mut self) -> Option<DirtyRect> {
        self.dirty.take()
    }

    /// Copia un bloque RGBA (`w * h * 4` bytes) en `(x, y)`. Lo que caiga
    /// fuera de la imagen se ignora.
    pub fn blit_rgba(&mut self, x: u32, y: u32, w: u32, h: u32, data: &[u8]) {
        if data.len() < w as usize * h as usize * 4 {
            return;
        }
        let Some(r) = self.clip(x, y, w, h) else {
            return;
        };
        let stride = self.width as usize * 4;
        for row in 0..r.height as usize {
            let src = row * w as usize * 4;
            let dst = (y as usize + row) * stride + x as usize * 4;
            let len = r.width as usize * 4;
            self.pixels[dst..dst + len].copy_from_slice(&data[src..src + len]);
        }
        self.mark_dirty(r.x, r.y, r.width, r.height);
    }

    /// Rellena un rectángulo con un color RGB.
    pub fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, rgb: [u8; 3]) {
        let Some(r) = self.clip(x, y, w, h) else {
            return;
        };
        let stride = self.width as usize * 4;
        for row in 0..r.height as usize {
            let start = (r.y as usize + row) * stride + r.x as usize * 4;
            for px in self.pixels[start..start + r.width as usize * 4].chunks_exact_mut(4) {
                px[0] = rgb[0];
                px[1] = rgb[1];
                px[2] = rgb[2];
                px[3] = 255;
            }
        }
        self.mark_dirty(r.x, r.y, r.width, r.height);
    }

    /// Copia una zona de la propia imagen a otra posición (pueden solaparse).
    pub fn copy_rect(&mut self, src_x: u32, src_y: u32, w: u32, h: u32, dst_x: u32, dst_y: u32) {
        if src_x + w > self.width
            || src_y + h > self.height
            || dst_x + w > self.width
            || dst_y + h > self.height
            || w == 0
            || h == 0
        {
            return;
        }
        let stride = self.width as usize * 4;
        let len = w as usize * 4;
        let mut copy_row = |row: usize| {
            let src = (src_y as usize + row) * stride + src_x as usize * 4;
            let dst = (dst_y as usize + row) * stride + dst_x as usize * 4;
            self.pixels.copy_within(src..src + len, dst);
        };
        // El orden de las filas evita pisar datos aún no copiados.
        if dst_y <= src_y {
            (0..h as usize).for_each(&mut copy_row);
        } else {
            (0..h as usize).rev().for_each(&mut copy_row);
        }
        self.mark_dirty(dst_x, dst_y, w, h);
    }
}

/// Tecla física, independiente del protocolo. Los caracteres imprimibles
/// normales no pasan por aquí: se envían como [`DesktopInput::Text`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// Letra `b'a'..=b'z'`.
    Letter(u8),
    /// Dígito `0..=9` de la fila superior.
    Digit(u8),
    /// Tecla de función `1..=12`.
    F(u8),
    Enter,
    Escape,
    Backspace,
    Tab,
    Space,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    Minus,
    Equal,
    BracketLeft,
    BracketRight,
    Backslash,
    Semicolon,
    Quote,
    Backquote,
    Comma,
    Period,
    Slash,
    Shift,
    Control,
    Alt,
    /// Tecla Windows / Super.
    Meta,
}

impl KeyCode {
    /// Código de exploración de PC (conjunto 1) y si lleva prefijo extendido
    /// (`0xE0`). Es lo que espera RDP.
    pub fn scancode(self) -> (u8, bool) {
        const LETTERS: [u8; 26] = [
            0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31,
            0x18, 0x19, 0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
        ];
        match self {
            KeyCode::Letter(c) => (
                LETTERS[(c.to_ascii_lowercase().clamp(b'a', b'z') - b'a') as usize],
                false,
            ),
            KeyCode::Digit(0) => (0x0B, false),
            KeyCode::Digit(d) => (0x01 + d.min(9), false),
            KeyCode::F(n @ 1..=10) => (0x3A + n, false),
            KeyCode::F(11) => (0x57, false),
            KeyCode::F(_) => (0x58, false),
            KeyCode::Enter => (0x1C, false),
            KeyCode::Escape => (0x01, false),
            KeyCode::Backspace => (0x0E, false),
            KeyCode::Tab => (0x0F, false),
            KeyCode::Space => (0x39, false),
            KeyCode::Left => (0x4B, true),
            KeyCode::Right => (0x4D, true),
            KeyCode::Up => (0x48, true),
            KeyCode::Down => (0x50, true),
            KeyCode::Home => (0x47, true),
            KeyCode::End => (0x4F, true),
            KeyCode::PageUp => (0x49, true),
            KeyCode::PageDown => (0x51, true),
            KeyCode::Insert => (0x52, true),
            KeyCode::Delete => (0x53, true),
            KeyCode::Minus => (0x0C, false),
            KeyCode::Equal => (0x0D, false),
            KeyCode::BracketLeft => (0x1A, false),
            KeyCode::BracketRight => (0x1B, false),
            KeyCode::Backslash => (0x2B, false),
            KeyCode::Semicolon => (0x27, false),
            KeyCode::Quote => (0x28, false),
            KeyCode::Backquote => (0x29, false),
            KeyCode::Comma => (0x33, false),
            KeyCode::Period => (0x34, false),
            KeyCode::Slash => (0x35, false),
            KeyCode::Shift => (0x2A, false),
            KeyCode::Control => (0x1D, false),
            KeyCode::Alt => (0x38, false),
            KeyCode::Meta => (0x5B, true),
        }
    }

    /// *Keysym* de X11. Es lo que espera VNC.
    pub fn keysym(self) -> u32 {
        match self {
            KeyCode::Letter(c) => c.to_ascii_lowercase() as u32,
            KeyCode::Digit(d) => (b'0' + d.min(9)) as u32,
            KeyCode::F(n) => 0xFFBE + (n.clamp(1, 12) as u32 - 1),
            KeyCode::Enter => 0xFF0D,
            KeyCode::Escape => 0xFF1B,
            KeyCode::Backspace => 0xFF08,
            KeyCode::Tab => 0xFF09,
            KeyCode::Space => 0x20,
            KeyCode::Left => 0xFF51,
            KeyCode::Up => 0xFF52,
            KeyCode::Right => 0xFF53,
            KeyCode::Down => 0xFF54,
            KeyCode::Home => 0xFF50,
            KeyCode::End => 0xFF57,
            KeyCode::PageUp => 0xFF55,
            KeyCode::PageDown => 0xFF56,
            KeyCode::Insert => 0xFF63,
            KeyCode::Delete => 0xFFFF,
            KeyCode::Minus => b'-' as u32,
            KeyCode::Equal => b'=' as u32,
            KeyCode::BracketLeft => b'[' as u32,
            KeyCode::BracketRight => b']' as u32,
            KeyCode::Backslash => b'\\' as u32,
            KeyCode::Semicolon => b';' as u32,
            KeyCode::Quote => b'\'' as u32,
            KeyCode::Backquote => b'`' as u32,
            KeyCode::Comma => b',' as u32,
            KeyCode::Period => b'.' as u32,
            KeyCode::Slash => b'/' as u32,
            KeyCode::Shift => 0xFFE1,
            KeyCode::Control => 0xFFE3,
            KeyCode::Alt => 0xFFE9,
            KeyCode::Meta => 0xFFEB,
        }
    }
}

/// *Keysym* de X11 para un carácter Unicode.
pub fn char_keysym(c: char) -> u32 {
    let cp = c as u32;
    match cp {
        0x20..=0x7E | 0xA0..=0xFF => cp,
        _ => 0x0100_0000 | cp,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

/// Órdenes de la interfaz hacia el escritorio remoto. Las coordenadas van
/// en píxeles del escritorio remoto.
#[derive(Debug, Clone, PartialEq)]
pub enum DesktopInput {
    Key {
        key: KeyCode,
        pressed: bool,
    },
    /// Texto ya compuesto por el teclado local (se envía como Unicode).
    Text(String),
    PointerMove {
        x: u16,
        y: u16,
    },
    PointerButton {
        button: MouseButton,
        pressed: bool,
        x: u16,
        y: u16,
    },
    /// Rueda: `delta` en muescas (positivo = hacia arriba / derecha).
    Wheel {
        x: u16,
        y: u16,
        delta: i16,
        horizontal: bool,
    },
    /// El portapapeles local contiene este texto.
    ClipboardText(String),
    Close,
}

/// Avisos del escritorio remoto hacia la interfaz.
pub enum DesktopEvent {
    /// Mensaje de progreso mientras conecta.
    Status(String),
    /// Sesión establecida; el [`Framebuffer`] ya tiene el tamaño negociado
    /// (el servidor aún puede cambiarlo después: cambia entonces su `epoch`).
    Connected,
    /// El portapapeles remoto contiene este texto.
    ClipboardText(String),
    /// Secretos validados que el usuario pidió guardar en el baúl.
    SaveSecrets(Vec<(String, String)>),
    /// Fin de la sesión, con el error si lo hubo.
    Closed(Option<String>),
}

/// Extremos de comunicación con un escritorio remoto en marcha.
pub struct DesktopBackend {
    pub input: mpsc::UnboundedSender<DesktopInput>,
    pub events: std::sync::mpsc::Receiver<DesktopEvent>,
    pub frame: SharedFrame,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirty_rects_accumulate_and_clip() {
        let mut fb = Framebuffer::new(100, 50);
        assert_eq!(
            fb.take_dirty(),
            Some(DirtyRect {
                x: 0,
                y: 0,
                width: 100,
                height: 50
            })
        );
        assert_eq!(fb.take_dirty(), None);
        fb.mark_dirty(10, 10, 5, 5);
        fb.mark_dirty(90, 40, 50, 50); // se recorta al borde
        assert_eq!(
            fb.take_dirty(),
            Some(DirtyRect {
                x: 10,
                y: 10,
                width: 90,
                height: 40
            })
        );
        fb.mark_dirty(200, 0, 5, 5); // fuera: se ignora
        assert_eq!(fb.take_dirty(), None);
    }

    #[test]
    fn blit_fill_and_copy() {
        let mut fb = Framebuffer::new(4, 4);
        fb.take_dirty();
        fb.fill_rect(0, 0, 2, 2, [10, 20, 30]);
        assert_eq!(&fb.pixels[0..4], &[10, 20, 30, 255]);
        assert_eq!(&fb.pixels[8..12], &[0, 0, 0, 255]);
        // Copia solapada hacia abajo-derecha.
        fb.copy_rect(0, 0, 2, 2, 1, 1);
        let at = |fb: &Framebuffer, x: usize, y: usize| fb.pixels[(y * 4 + x) * 4];
        assert_eq!(at(&fb, 1, 1), 10);
        assert_eq!(at(&fb, 2, 2), 10);
        assert_eq!(at(&fb, 3, 3), 0);
        // Un blit que se sale por la derecha se recorta sin romper nada.
        fb.blit_rgba(3, 0, 2, 1, &[1, 2, 3, 255, 9, 9, 9, 255]);
        assert_eq!(at(&fb, 3, 0), 1);
        let epoch = fb.epoch();
        fb.resize(2, 2);
        assert_eq!(fb.pixels.len(), 16);
        assert!(fb.epoch() > epoch);
    }

    #[test]
    fn key_tables() {
        assert_eq!(KeyCode::Letter(b'a').scancode(), (0x1E, false));
        assert_eq!(KeyCode::Letter(b'Z').scancode(), (0x2C, false));
        assert_eq!(KeyCode::Digit(1).scancode(), (0x02, false));
        assert_eq!(KeyCode::Digit(0).scancode(), (0x0B, false));
        assert_eq!(KeyCode::F(1).scancode(), (0x3B, false));
        assert_eq!(KeyCode::F(10).scancode(), (0x44, false));
        assert_eq!(KeyCode::F(12).scancode(), (0x58, false));
        assert_eq!(KeyCode::Delete.scancode(), (0x53, true));
        assert_eq!(KeyCode::Letter(b'Q').keysym(), b'q' as u32);
        assert_eq!(KeyCode::F(12).keysym(), 0xFFC9);
        assert_eq!(KeyCode::Enter.keysym(), 0xFF0D);
        assert_eq!(char_keysym('a'), 0x61);
        assert_eq!(char_keysym('ñ'), 0xF1);
        assert_eq!(char_keysym('€'), 0x0100_20AC);
    }
}
