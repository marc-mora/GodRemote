//! Codificación del protocolo X11 en el cable: lector de peticiones,
//! escritor de respuestas, eventos y errores, en los dos órdenes de bytes.

/// Identificador de recurso (ventana, pixmap, GC, fuente, átomo…).
pub type Xid = u32;

// --- Códigos de error -------------------------------------------------------
pub const BAD_REQUEST: u8 = 1;
pub const BAD_VALUE: u8 = 2;
pub const BAD_WINDOW: u8 = 3;
pub const BAD_PIXMAP: u8 = 4;
pub const BAD_ATOM: u8 = 5;
pub const BAD_CURSOR: u8 = 6;
pub const BAD_FONT: u8 = 7;
pub const BAD_MATCH: u8 = 8;
pub const BAD_DRAWABLE: u8 = 9;
pub const BAD_ACCESS: u8 = 10;
pub const BAD_ALLOC: u8 = 11;
pub const BAD_COLOR: u8 = 12;
pub const BAD_GC: u8 = 13;
pub const BAD_ID_CHOICE: u8 = 14;
pub const BAD_NAME: u8 = 15;
pub const BAD_LENGTH: u8 = 16;
pub const BAD_IMPLEMENTATION: u8 = 17;

// --- Códigos de evento ------------------------------------------------------
pub const KEY_PRESS: u8 = 2;
pub const KEY_RELEASE: u8 = 3;
pub const BUTTON_PRESS: u8 = 4;
pub const BUTTON_RELEASE: u8 = 5;
pub const MOTION_NOTIFY: u8 = 6;
pub const ENTER_NOTIFY: u8 = 7;
pub const LEAVE_NOTIFY: u8 = 8;
pub const FOCUS_IN: u8 = 9;
pub const FOCUS_OUT: u8 = 10;
pub const KEYMAP_NOTIFY: u8 = 11;
pub const EXPOSE: u8 = 12;
pub const GRAPHICS_EXPOSE: u8 = 13;
pub const NO_EXPOSE: u8 = 14;
pub const VISIBILITY_NOTIFY: u8 = 15;
pub const CREATE_NOTIFY: u8 = 16;
pub const DESTROY_NOTIFY: u8 = 17;
pub const UNMAP_NOTIFY: u8 = 18;
pub const MAP_NOTIFY: u8 = 19;
pub const MAP_REQUEST: u8 = 20;
pub const REPARENT_NOTIFY: u8 = 21;
pub const CONFIGURE_NOTIFY: u8 = 22;
pub const CONFIGURE_REQUEST: u8 = 23;
pub const GRAVITY_NOTIFY: u8 = 24;
pub const RESIZE_REQUEST: u8 = 25;
pub const CIRCULATE_NOTIFY: u8 = 26;
pub const CIRCULATE_REQUEST: u8 = 27;
pub const PROPERTY_NOTIFY: u8 = 28;
pub const SELECTION_CLEAR: u8 = 29;
pub const SELECTION_REQUEST: u8 = 30;
pub const SELECTION_NOTIFY: u8 = 31;
pub const COLORMAP_NOTIFY: u8 = 32;
pub const CLIENT_MESSAGE: u8 = 33;
pub const MAPPING_NOTIFY: u8 = 34;

// --- Máscaras de eventos ----------------------------------------------------
pub const M_KEY_PRESS: u32 = 1 << 0;
pub const M_KEY_RELEASE: u32 = 1 << 1;
pub const M_BUTTON_PRESS: u32 = 1 << 2;
pub const M_BUTTON_RELEASE: u32 = 1 << 3;
pub const M_ENTER_WINDOW: u32 = 1 << 4;
pub const M_LEAVE_WINDOW: u32 = 1 << 5;
pub const M_POINTER_MOTION: u32 = 1 << 6;
pub const M_POINTER_MOTION_HINT: u32 = 1 << 7;
pub const M_BUTTON1_MOTION: u32 = 1 << 8;
pub const M_BUTTON_MOTION: u32 = 1 << 13;
pub const M_EXPOSURE: u32 = 1 << 15;
pub const M_VISIBILITY_CHANGE: u32 = 1 << 16;
pub const M_STRUCTURE_NOTIFY: u32 = 1 << 17;
pub const M_RESIZE_REDIRECT: u32 = 1 << 18;
pub const M_SUBSTRUCTURE_NOTIFY: u32 = 1 << 19;
pub const M_SUBSTRUCTURE_REDIRECT: u32 = 1 << 20;
pub const M_FOCUS_CHANGE: u32 = 1 << 21;
pub const M_PROPERTY_CHANGE: u32 = 1 << 22;
pub const M_OWNER_GRAB_BUTTON: u32 = 1 << 24;
/// Bits válidos de una máscara de eventos.
pub const M_ALL: u32 = 0x01ff_ffff;

// --- Máscaras de estado (modificadores y botones) ---------------------------
pub const S_SHIFT: u16 = 1 << 0;
pub const S_CONTROL: u16 = 1 << 2;
pub const S_MOD1: u16 = 1 << 3;
pub const S_MOD4: u16 = 1 << 6;
pub const S_BUTTON1: u16 = 1 << 8;
pub const S_ANY_MODIFIER: u16 = 1 << 15;

/// Error X que se devuelve al cliente en lugar de una respuesta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XError {
    pub code: u8,
    /// Valor o identificador culpable.
    pub value: u32,
}

impl XError {
    pub fn new(code: u8, value: u32) -> Self {
        Self { code, value }
    }
}

pub type XResult<T = ()> = Result<T, XError>;

/// Relleno hasta el siguiente múltiplo de 4.
pub fn pad4(n: usize) -> usize {
    (4 - n % 4) % 4
}

/// Lector del cuerpo de una petición en el orden de bytes del cliente. Leer
/// más allá del final da `BadLength`.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    pub be: bool,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8], be: bool) -> Self {
        Self { buf, pos: 0, be }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn bytes(&mut self, n: usize) -> XResult<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|e| *e <= self.buf.len());
        let Some(end) = end else {
            return Err(XError::new(BAD_LENGTH, 0));
        };
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    /// Lo que queda por leer.
    pub fn rest(&mut self) -> &'a [u8] {
        let out = &self.buf[self.pos..];
        self.pos = self.buf.len();
        out
    }

    pub fn skip(&mut self, n: usize) -> XResult {
        self.bytes(n).map(|_| ())
    }

    pub fn u8(&mut self) -> XResult<u8> {
        Ok(self.bytes(1)?[0])
    }

    pub fn u16(&mut self) -> XResult<u16> {
        let b = self.bytes(2)?;
        let b = [b[0], b[1]];
        Ok(if self.be {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        })
    }

    pub fn i16(&mut self) -> XResult<i16> {
        self.u16().map(|v| v as i16)
    }

    pub fn u32(&mut self) -> XResult<u32> {
        let b = self.bytes(4)?;
        let b = [b[0], b[1], b[2], b[3]];
        Ok(if self.be {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    }
}

/// Escritor en el orden de bytes del cliente.
pub struct Writer {
    pub buf: Vec<u8>,
    pub be: bool,
}

impl Writer {
    pub fn new(be: bool) -> Self {
        Self {
            buf: Vec::with_capacity(64),
            be,
        }
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    pub fn u16(&mut self, v: u16) -> &mut Self {
        if self.be {
            self.buf.extend_from_slice(&v.to_be_bytes());
        } else {
            self.buf.extend_from_slice(&v.to_le_bytes());
        }
        self
    }

    pub fn i16(&mut self, v: i16) -> &mut Self {
        self.u16(v as u16)
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        if self.be {
            self.buf.extend_from_slice(&v.to_be_bytes());
        } else {
            self.buf.extend_from_slice(&v.to_le_bytes());
        }
        self
    }

    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(b);
        self
    }

    pub fn zeros(&mut self, n: usize) -> &mut Self {
        self.buf.resize(self.buf.len() + n, 0);
        self
    }

    /// Rellena con ceros hasta múltiplo de 4.
    pub fn pad(&mut self) -> &mut Self {
        let n = pad4(self.buf.len());
        self.zeros(n)
    }

    /// Empieza una respuesta: cabecera de 8 bytes (la longitud se fija en
    /// [`Writer::finish_reply`]).
    pub fn reply(be: bool, seq: u16, data: u8) -> Self {
        let mut w = Self::new(be);
        w.u8(1).u8(data).u16(seq).u32(0);
        w
    }

    /// Completa la respuesta: mínimo 32 bytes, múltiplo de 4 y campo de
    /// longitud (en unidades de 4 bytes por encima de 32).
    pub fn finish_reply(mut self) -> Vec<u8> {
        if self.buf.len() < 32 {
            self.buf.resize(32, 0);
        }
        self.pad();
        let extra = ((self.buf.len() - 32) / 4) as u32;
        let len = if self.be {
            extra.to_be_bytes()
        } else {
            extra.to_le_bytes()
        };
        self.buf[4..8].copy_from_slice(&len);
        self.buf
    }
}

/// Paquete de error (32 bytes).
pub fn encode_error(be: bool, seq: u16, err: XError, major: u8, minor: u16) -> Vec<u8> {
    let mut w = Writer::new(be);
    w.u8(0)
        .u8(err.code)
        .u16(seq)
        .u32(err.value)
        .u16(minor)
        .u8(major);
    w.buf.resize(32, 0);
    w.buf
}

/// Evento de 32 bytes construido en orden *little-endian* canónico; al
/// entregarlo se le pone el número de secuencia del destinatario y, si este
/// es *big-endian*, se invierten sus campos.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event(pub [u8; 32]);

/// Constructor de eventos campo a campo.
pub struct Ev {
    data: [u8; 32],
    pos: usize,
}

impl Ev {
    pub fn new(code: u8, detail: u8) -> Self {
        let mut data = [0u8; 32];
        data[0] = code;
        data[1] = detail;
        Self { data, pos: 4 }
    }

    pub fn u8(mut self, v: u8) -> Self {
        self.data[self.pos] = v;
        self.pos += 1;
        self
    }

    pub fn u16(mut self, v: u16) -> Self {
        self.data[self.pos..self.pos + 2].copy_from_slice(&v.to_le_bytes());
        self.pos += 2;
        self
    }

    pub fn i16(self, v: i16) -> Self {
        self.u16(v as u16)
    }

    pub fn u32(mut self, v: u32) -> Self {
        self.data[self.pos..self.pos + 4].copy_from_slice(&v.to_le_bytes());
        self.pos += 4;
        self
    }

    pub fn done(self) -> Event {
        Event(self.data)
    }
}

/// Tamaños de los campos de cada evento a partir del byte 4 (el resto es
/// relleno). Sirve para cambiar el orden de bytes.
fn event_layout(code: u8, format: u8) -> &'static [u8] {
    match code & 0x7f {
        KEY_PRESS | KEY_RELEASE | BUTTON_PRESS | BUTTON_RELEASE | MOTION_NOTIFY | ENTER_NOTIFY
        | LEAVE_NOTIFY => &[4, 4, 4, 4, 2, 2, 2, 2, 2, 1, 1],
        FOCUS_IN | FOCUS_OUT => &[4, 1],
        EXPOSE => &[4, 2, 2, 2, 2, 2],
        GRAPHICS_EXPOSE => &[4, 2, 2, 2, 2, 2, 2, 1],
        NO_EXPOSE => &[4, 2, 1],
        VISIBILITY_NOTIFY => &[4, 1],
        CREATE_NOTIFY => &[4, 4, 2, 2, 2, 2, 2, 1],
        DESTROY_NOTIFY => &[4, 4],
        UNMAP_NOTIFY | MAP_NOTIFY => &[4, 4, 1],
        MAP_REQUEST => &[4, 4],
        REPARENT_NOTIFY => &[4, 4, 4, 2, 2, 1],
        CONFIGURE_NOTIFY => &[4, 4, 4, 2, 2, 2, 2, 2, 1],
        CONFIGURE_REQUEST => &[4, 4, 4, 2, 2, 2, 2, 2, 2],
        GRAVITY_NOTIFY => &[4, 4, 2, 2],
        RESIZE_REQUEST => &[4, 2, 2],
        CIRCULATE_NOTIFY | CIRCULATE_REQUEST => &[4, 4, 4, 1],
        PROPERTY_NOTIFY => &[4, 4, 4, 1],
        SELECTION_CLEAR => &[4, 4, 4],
        SELECTION_REQUEST => &[4, 4, 4, 4, 4, 4],
        SELECTION_NOTIFY => &[4, 4, 4, 4, 4],
        COLORMAP_NOTIFY => &[4, 4, 1, 1],
        CLIENT_MESSAGE => match format {
            16 => &[4, 4, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2],
            32 => &[4, 4, 4, 4, 4, 4, 4],
            _ => &[4, 4],
        },
        MAPPING_NOTIFY => &[1, 1, 1],
        _ => &[],
    }
}

/// Invierte el orden de bytes de los campos de un evento (la operación es su
/// propia inversa: sirve en los dos sentidos).
pub fn swap_event(data: &mut [u8; 32]) {
    if data[0] & 0x7f == KEYMAP_NOTIFY {
        return; // Sin número de secuencia ni campos de más de un byte.
    }
    data.swap(2, 3);
    let mut pos = 4;
    for size in event_layout(data[0], data[1]) {
        let size = *size as usize;
        data[pos..pos + size].reverse();
        pos += size;
    }
}

impl Event {
    pub fn code(&self) -> u8 {
        self.0[0] & 0x7f
    }

    /// Campo de 32 bits en el desplazamiento dado (orden canónico).
    pub fn u32_at(&self, pos: usize) -> u32 {
        u32::from_le_bytes([
            self.0[pos],
            self.0[pos + 1],
            self.0[pos + 2],
            self.0[pos + 3],
        ])
    }

    /// Bytes listos para enviar a un cliente.
    pub fn encode(&self, be: bool, seq: u16) -> [u8; 32] {
        let mut data = self.0;
        if self.code() != KEYMAP_NOTIFY {
            data[2..4].copy_from_slice(&seq.to_le_bytes());
        }
        if be {
            swap_event(&mut data);
        }
        data
    }

    /// Evento recibido de un cliente (`SendEvent`), pasado a orden canónico.
    pub fn from_wire(bytes: &[u8], be: bool) -> Event {
        let mut data = [0u8; 32];
        data.copy_from_slice(&bytes[..32]);
        if be {
            swap_event(&mut data);
        }
        Event(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_honours_byte_order_and_length() {
        let data = [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 1, 2, 3];
        let mut le = Reader::new(&data, false);
        assert_eq!(le.u16().unwrap(), 0x3412);
        assert_eq!(le.u32().unwrap(), 0xbc9a_7856);
        let mut be = Reader::new(&data, true);
        assert_eq!(be.u16().unwrap(), 0x1234);
        assert_eq!(be.u32().unwrap(), 0x5678_9abc);
        assert_eq!(be.bytes(1).unwrap(), &[1]);
        assert_eq!(be.remaining(), 2);
        assert_eq!(be.rest(), &[2, 3]);
        assert_eq!(be.u8().unwrap_err().code, BAD_LENGTH);
        let mut short = Reader::new(&data[..3], false);
        assert_eq!(short.u32().unwrap_err().code, BAD_LENGTH);
        assert_eq!(short.i16().unwrap(), 0x3412);
    }

    #[test]
    fn replies_are_padded_and_sized() {
        let mut w = Writer::reply(false, 0x0102, 7);
        w.u32(0xdead_beef);
        let buf = w.finish_reply();
        assert_eq!(buf.len(), 32);
        assert_eq!(
            &buf[..12],
            &[1, 7, 2, 1, 0, 0, 0, 0, 0xef, 0xbe, 0xad, 0xde]
        );

        let mut w = Writer::reply(true, 0x0102, 0);
        w.zeros(24).bytes(b"hola!");
        let buf = w.finish_reply();
        assert_eq!(buf.len(), 40);
        // Longitud adicional: 2 unidades de 4 bytes, en big-endian.
        assert_eq!(&buf[..8], &[1, 0, 1, 2, 0, 0, 0, 2]);
        assert_eq!(&buf[32..], b"hola!\0\0\0");
    }

    #[test]
    fn errors_have_the_wire_layout() {
        let e = encode_error(true, 9, XError::new(BAD_WINDOW, 0x0123_4567), 8, 0);
        assert_eq!(e.len(), 32);
        assert_eq!(&e[..11], &[0, 3, 0, 9, 0x01, 0x23, 0x45, 0x67, 0, 0, 8]);
        let e = encode_error(false, 9, XError::new(BAD_VALUE, 5), 133, 1);
        assert_eq!(&e[..11], &[0, 2, 9, 0, 5, 0, 0, 0, 1, 0, 133]);
    }

    #[test]
    fn events_swap_per_field() {
        // ConfigureNotify: event, window, above, x, y, w, h, bw, override.
        let ev = Ev::new(CONFIGURE_NOTIFY, 0)
            .u32(0x0a0b_0c0d)
            .u32(0x0102_0304)
            .u32(0)
            .i16(-2)
            .i16(5)
            .u16(640)
            .u16(480)
            .u16(1)
            .u8(1)
            .done();
        let le = ev.encode(false, 0x1234);
        assert_eq!(&le[..8], &[22, 0, 0x34, 0x12, 0x0d, 0x0c, 0x0b, 0x0a]);
        assert_eq!(&le[16..18], &[0xfe, 0xff]);
        let be = ev.encode(true, 0x1234);
        assert_eq!(&be[..8], &[22, 0, 0x12, 0x34, 0x0a, 0x0b, 0x0c, 0x0d]);
        assert_eq!(&be[16..18], &[0xff, 0xfe]);
        assert_eq!(&be[20..22], &[0x02, 0x80]); // 640
        assert_eq!(be[26], 1);
        // Ida y vuelta por el cable de un cliente big-endian.
        assert_eq!(Event::from_wire(&be, true).encode(false, 0x1234), le);
    }

    #[test]
    fn client_messages_swap_by_format() {
        let ev32 = Ev::new(CLIENT_MESSAGE, 32)
            .u32(1)
            .u32(2)
            .u32(0x1122_3344)
            .done();
        let be = ev32.encode(true, 0);
        assert_eq!(&be[12..16], &[0x11, 0x22, 0x33, 0x44]);
        let ev8 = Ev::new(CLIENT_MESSAGE, 8)
            .u32(1)
            .u32(2)
            .u8(b'a')
            .u8(b'b')
            .done();
        let be = ev8.encode(true, 0);
        assert_eq!(&be[12..14], b"ab");
        let ev16 = Ev::new(CLIENT_MESSAGE, 16).u32(1).u32(2).u16(0x0102).done();
        let be = ev16.encode(true, 0);
        assert_eq!(&be[12..14], &[1, 2]);
    }
}
