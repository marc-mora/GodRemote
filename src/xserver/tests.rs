//! Pruebas del servidor X sin clientes reales: un cliente de mentira habla
//! el protocolo por un canal en memoria, en los dos órdenes de bytes.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

use super::draw::Budget;
use super::proto::*;
use super::server::{A_ATOM, A_STRING, A_WM_NAME, ROOT, SERVER_WINDOW};
use super::wm::{FRAME, TITLE_H};
use super::{XServer, XServerConfig, lock};
use crate::desktop::{DesktopBackend, DesktopEvent, DesktopInput, MouseButton};

const WAIT: Duration = Duration::from_secs(10);
const COOKIE: [u8; 16] = *b"0123456789abcdef";

fn start(width: u32, height: u32) -> (XServer, DesktopBackend) {
    XServer::start(
        &tokio::runtime::Handle::current(),
        XServerConfig { width, height },
        Arc::new(|| {}),
    )
}

/// Cliente X mínimo para las pruebas.
struct TestClient {
    stream: DuplexStream,
    be: bool,
    /// Número de secuencia de la última petición enviada.
    seq: u16,
    /// Base de sus identificadores de recurso.
    base: u32,
    /// Respuesta de conexión completa.
    setup: Vec<u8>,
    next_id: u32,
}

impl TestClient {
    /// Conecta presentando (o no) una cookie. `Err` lleva el motivo del
    /// rechazo.
    async fn connect_with(
        server: &XServer,
        be: bool,
        required: Option<[u8; 16]>,
        offered: Option<&[u8]>,
    ) -> Result<TestClient, String> {
        let (mut stream, remote) = tokio::io::duplex(1 << 20);
        let task = server.clone();
        tokio::spawn(async move {
            let _ = task.serve(remote, required).await;
        });
        let mut w = Writer::new(be);
        let (name, data): (&[u8], &[u8]) = match offered {
            Some(cookie) => (b"MIT-MAGIC-COOKIE-1", cookie),
            None => (b"", b""),
        };
        w.u8(if be { 0x42 } else { 0x6c })
            .u8(0)
            .u16(11)
            .u16(0)
            .u16(name.len() as u16)
            .u16(data.len() as u16)
            .u16(0)
            .bytes(name)
            .pad()
            .bytes(data)
            .pad();
        stream.write_all(&w.buf).await.unwrap();
        let mut head = [0u8; 8];
        tokio::time::timeout(WAIT, stream.read_exact(&mut head))
            .await
            .expect("respuesta de conexión")
            .unwrap();
        let extra = u16_of(be, &head[6..8]) as usize * 4;
        let mut rest = vec![0u8; extra];
        stream.read_exact(&mut rest).await.unwrap();
        if head[0] != 1 {
            let reason = String::from_utf8_lossy(&rest[..head[1] as usize]).into_owned();
            return Err(reason);
        }
        let base = u32_of(be, &rest[4..8]);
        let mut setup = head.to_vec();
        setup.extend_from_slice(&rest);
        Ok(TestClient {
            stream,
            be,
            seq: 0,
            base,
            setup,
            next_id: 1,
        })
    }

    async fn connect(server: &XServer, be: bool) -> TestClient {
        Self::connect_with(server, be, None, None)
            .await
            .expect("conexión")
    }

    fn id(&mut self) -> u32 {
        self.next_id += 1;
        self.base | self.next_id
    }

    fn body(&self) -> Writer {
        Writer::new(self.be)
    }

    /// Envía una petición con el cuerpo dado (se rellena a múltiplo de 4).
    async fn request(&mut self, opcode: u8, data: u8, mut body: Writer) {
        body.pad();
        let mut w = Writer::new(self.be);
        w.u8(opcode)
            .u8(data)
            .u16((body.buf.len() / 4 + 1) as u16)
            .bytes(&body.buf);
        self.stream.write_all(&w.buf).await.unwrap();
        self.seq = self.seq.wrapping_add(1);
    }

    /// Siguiente paquete del servidor (respuesta, error o evento), o `None`
    /// si cerró la conexión.
    async fn packet(&mut self) -> Option<Vec<u8>> {
        let mut head = [0u8; 32];
        match tokio::time::timeout(WAIT, self.stream.read_exact(&mut head)).await {
            Err(_) => panic!("el servidor no contesta"),
            Ok(Err(_)) => return None,
            Ok(Ok(_)) => {}
        }
        let mut packet = head.to_vec();
        if head[0] == 1 {
            let extra = u32_of(self.be, &head[4..8]) as usize * 4;
            packet.resize(32 + extra, 0);
            self.stream.read_exact(&mut packet[32..]).await.unwrap();
        }
        Some(packet)
    }

    /// Espera la respuesta a la última petición, saltándose los eventos.
    async fn reply(&mut self) -> Vec<u8> {
        loop {
            let p = self.packet().await.expect("conexión cerrada");
            match p[0] {
                1 => {
                    assert_eq!(self.u16(&p[2..4]), self.seq, "secuencia de la respuesta");
                    return p;
                }
                0 => panic!("error X {} (valor {:#x})", p[1], self.u32(&p[4..8])),
                _ => {}
            }
        }
    }

    /// Espera un error: devuelve `(código, valor, secuencia, petición)`.
    async fn error(&mut self) -> (u8, u32, u16, u8) {
        loop {
            let p = self.packet().await.expect("conexión cerrada");
            match p[0] {
                0 => return (p[1], self.u32(&p[4..8]), self.u16(&p[2..4]), p[10]),
                1 => panic!("llegó una respuesta en vez de un error"),
                _ => {}
            }
        }
    }

    /// Espera un evento del tipo dado (ya en orden canónico).
    async fn event(&mut self, code: u8) -> Event {
        loop {
            let p = self.packet().await.expect("conexión cerrada");
            if p[0] & 0x7f == code {
                return Event::from_wire(&p, self.be);
            }
            assert!(p[0] > 1, "esperando el evento {code} llegó {:?}", &p[..12]);
        }
    }

    /// Petición con respuesta que sirve para saber que el servidor ya
    /// procesó todo lo anterior (`GetInputFocus`).
    async fn sync(&mut self) {
        let body = self.body();
        self.request(43, 0, body).await;
        self.reply().await;
    }

    fn u16(&self, b: &[u8]) -> u16 {
        u16_of(self.be, b)
    }

    fn u32(&self, b: &[u8]) -> u32 {
        u32_of(self.be, b)
    }

    async fn intern(&mut self, name: &str) -> u32 {
        let mut b = self.body();
        b.u16(name.len() as u16).u16(0).bytes(name.as_bytes());
        self.request(16, 0, b).await;
        let r = self.reply().await;
        self.u32(&r[8..12])
    }

    /// Crea una ventana hija de la raíz. `attrs` son `(bit, valor)` en orden.
    async fn create_window(
        &mut self,
        (x, y, w, h): (i16, i16, u16, u16),
        attrs: &[(u32, u32)],
    ) -> u32 {
        let id = self.id();
        let mut b = self.body();
        b.u32(id).u32(ROOT).i16(x).i16(y).u16(w).u16(h).u16(0);
        b.u16(1).u32(0);
        b.u32(attrs.iter().fold(0, |m, (bit, _)| m | (1 << bit)));
        for (_, value) in attrs {
            b.u32(*value);
        }
        self.request(1, 0, b).await;
        id
    }

    async fn map(&mut self, window: u32) {
        let mut b = self.body();
        b.u32(window);
        self.request(8, 0, b).await;
    }

    async fn change_property(
        &mut self,
        window: u32,
        name: u32,
        kind: u32,
        format: u8,
        data: &[u8],
    ) {
        let mut b = self.body();
        b.u32(window).u32(name).u32(kind).u8(format).zeros(3);
        b.u32((data.len() / (format as usize / 8)) as u32)
            .bytes(data);
        self.request(18, 0, b).await;
    }

    /// Devuelve `(tipo, formato, datos)` de una propiedad.
    async fn get_property(&mut self, window: u32, name: u32) -> (u32, u8, Vec<u8>) {
        let mut b = self.body();
        b.u32(window).u32(name).u32(0).u32(0).u32(1 << 20);
        self.request(20, 0, b).await;
        let r = self.reply().await;
        let units = self.u32(&r[16..20]) as usize;
        let bytes = units * (r[1] as usize / 8).max(1);
        (self.u32(&r[8..12]), r[1], r[32..32 + bytes].to_vec())
    }

    async fn create_gc(&mut self, drawable: u32, foreground: u32) -> u32 {
        let id = self.id();
        let mut b = self.body();
        b.u32(id).u32(drawable).u32(1 << 2).u32(foreground);
        self.request(55, 0, b).await;
        id
    }

    async fn create_pixmap(&mut self, depth: u8, w: u16, h: u16) -> u32 {
        let id = self.id();
        let mut b = self.body();
        b.u32(id).u32(ROOT).u16(w).u16(h);
        self.request(53, depth, b).await;
        id
    }

    async fn fill(&mut self, drawable: u32, gc: u32, (x, y, w, h): (i16, i16, u16, u16)) {
        let mut b = self.body();
        b.u32(drawable).u32(gc).i16(x).i16(y).u16(w).u16(h);
        self.request(70, 0, b).await;
    }
}

fn u16_of(be: bool, b: &[u8]) -> u16 {
    if be {
        u16::from_be_bytes([b[0], b[1]])
    } else {
        u16::from_le_bytes([b[0], b[1]])
    }
}

fn u32_of(be: bool, b: &[u8]) -> u32 {
    if be {
        u32::from_be_bytes([b[0], b[1], b[2], b[3]])
    } else {
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    }
}

/// Espera a que el servidor termine el lote en curso (composición incluida)
/// y devuelve un píxel de la pantalla.
fn pixel(server: &XServer, backend: &DesktopBackend, x: u32, y: u32) -> [u8; 3] {
    server.client_count(); // toma el candado del estado: el lote ya acabó
    let fb = backend.frame.lock().unwrap();
    let i = ((y * fb.width() + x) * 4) as usize;
    [fb.pixels[i], fb.pixels[i + 1], fb.pixels[i + 2]]
}

// Bits de los atributos de ventana.
const ATTR_BACKGROUND: u32 = 1;
const ATTR_OVERRIDE: u32 = 9;
const ATTR_EVENTS: u32 = 11;

#[tokio::test(flavor = "multi_thread")]
async fn handshake_and_queries_in_both_byte_orders() {
    let (server, backend) = start(640, 480);
    assert!(matches!(
        backend.events.recv_timeout(WAIT),
        Ok(DesktopEvent::Connected)
    ));
    let mut bases = vec![];
    let mut clients = vec![];
    for be in [false, true] {
        let mut c = TestClient::connect(&server, be).await;
        // Cabecera: aceptado, protocolo 11.0.
        assert_eq!((c.setup[0], c.u16(&c.setup[2..4])), (1, 11));
        let vendor = c.u16(&c.setup[24..26]) as usize;
        assert_eq!(&c.setup[40..40 + vendor], b"GodRemote");
        assert_eq!((c.setup[34], c.setup[35]), (8, 255), "códigos de tecla");
        // Tras el fabricante (relleno a 4) y los 3 formatos va la pantalla.
        let screen = 40 + vendor.div_ceil(4) * 4 + 8 * c.setup[29] as usize;
        assert_eq!(c.u32(&c.setup[screen..screen + 4]), ROOT);
        assert_eq!(c.u16(&c.setup[screen + 20..screen + 22]), 640);
        assert_eq!(c.u16(&c.setup[screen + 22..screen + 24]), 480);
        assert_eq!(c.setup[screen + 38], 24, "profundidad de la raíz");
        // El visual TrueColor con sus máscaras.
        let visual = screen + 40 + 8;
        assert_eq!(c.setup[visual + 4], 4);
        assert_eq!(c.u32(&c.setup[visual + 8..visual + 12]), 0xff0000);
        assert_eq!(c.u32(&c.setup[visual + 16..visual + 20]), 0x0000ff);
        bases.push(c.base);

        // InternAtom de uno predefinido y de uno nuevo; GetAtomName.
        assert_eq!(c.intern("WM_NAME").await, A_WM_NAME);
        let atom = c.intern("_PRUEBA_DE_ORDEN").await;
        assert!(atom > 68);
        let mut b = c.body();
        b.u32(atom);
        c.request(17, 0, b).await;
        let r = c.reply().await;
        let len = c.u16(&r[8..10]) as usize;
        assert_eq!(&r[32..32 + len], b"_PRUEBA_DE_ORDEN");

        // GetGeometry de la raíz.
        let mut b = c.body();
        b.u32(ROOT);
        c.request(14, 0, b).await;
        let r = c.reply().await;
        assert_eq!(r[1], 24);
        assert_eq!((c.u16(&r[16..18]), c.u16(&r[18..20])), (640, 480));

        // QueryExtension: BIG-REQUESTS sí; RENDER y XKEYBOARD no.
        for (name, present) in [("BIG-REQUESTS", 1), ("RENDER", 0), ("XKEYBOARD", 0)] {
            let mut b = c.body();
            b.u16(name.len() as u16).u16(0).bytes(name.as_bytes());
            c.request(98, 0, b).await;
            assert_eq!(c.reply().await[8], present, "{name}");
        }
        // GetKeyboardMapping: la tecla de la «a» da «a» y, con Mayús, «A».
        let code = lock(&server.inner.state).keymap.keycode(0x61).unwrap();
        let mut b = c.body();
        b.u8(code).u8(1);
        c.request(101, 0, b).await;
        let r = c.reply().await;
        assert_eq!(r[1], 2, "símbolos por tecla");
        assert_eq!((c.u32(&r[32..36]), c.u32(&r[36..40])), (0x61, 0x41));
        clients.push(c);
    }
    assert_ne!(bases[0], bases[1]);
    assert_eq!(server.client_count(), 2);
    server.stop();
    assert_eq!(server.client_count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn cookie_authentication() {
    let (server, _backend) = start(320, 200);
    for be in [false, true] {
        let refused = TestClient::connect_with(&server, be, Some(COOKIE), None).await;
        assert_eq!(
            refused.err().as_deref(),
            Some("Invalid MIT-MAGIC-COOKIE-1 key")
        );
        let wrong = TestClient::connect_with(&server, be, Some(COOKIE), Some(&[7; 16])).await;
        assert!(wrong.is_err());
        let short = TestClient::connect_with(&server, be, Some(COOKIE), Some(&COOKIE[..8])).await;
        assert!(short.is_err());
        assert_eq!(server.client_count(), 0);
        let mut ok = TestClient::connect_with(&server, be, Some(COOKIE), Some(&COOKIE))
            .await
            .expect("con la cookie correcta");
        ok.sync().await;
        assert_eq!(server.client_count(), 1);
        drop(ok);
        for _ in 0..200 {
            if server.client_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(server.client_count(), 0, "el cliente no se dio de baja");
    }
    // Sin cookie exigida, ofrecer una no molesta.
    TestClient::connect_with(&server, false, None, Some(&COOKIE))
        .await
        .expect("sin cookie exigida");
    server.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn errors_carry_code_value_sequence_and_opcode() {
    let (server, _backend) = start(320, 200);
    for be in [false, true] {
        let mut c = TestClient::connect(&server, be).await;
        // BadWindow en GetWindowAttributes.
        let mut b = c.body();
        b.u32(0x0bad_0001);
        c.request(3, 0, b).await;
        assert_eq!(c.error().await, (BAD_WINDOW, 0x0bad_0001, 1, 3));
        // BadLength: petición más corta de lo que debe.
        let b = c.body();
        c.request(3, 0, b).await;
        assert_eq!(c.error().await.0, BAD_LENGTH);
        // BadRequest: código de operación desconocido (y de extensión).
        let b = c.body();
        c.request(200, 5, b).await;
        assert_eq!(c.error().await, (BAD_REQUEST, 0, 3, 200));
        // BadDrawable, BadGC, BadAtom, BadFont, BadPixmap, BadValue, BadMatch.
        let mut b = c.body();
        b.u32(0x0bad_0002);
        c.request(14, 0, b).await;
        assert_eq!(c.error().await.0, BAD_DRAWABLE);
        let mut b = c.body();
        b.u32(ROOT).u32(0x0bad_0003).i16(0).i16(0).u16(1).u16(1);
        c.request(70, 0, b).await;
        assert_eq!(c.error().await, (BAD_GC, 0x0bad_0003, 5, 70));
        let mut b = c.body();
        b.u32(0x00ff_ffff);
        c.request(17, 0, b).await;
        assert_eq!(c.error().await.0, BAD_ATOM);
        let mut b = c.body();
        b.u32(0x0bad_0004);
        c.request(47, 0, b).await;
        assert_eq!(c.error().await.0, BAD_FONT);
        let mut b = c.body();
        b.u32(0x0bad_0005);
        c.request(54, 0, b).await;
        assert_eq!(c.error().await.0, BAD_PIXMAP);
        let id = c.id();
        let mut b = c.body();
        b.u32(id).u32(ROOT).u16(10).u16(10);
        c.request(53, 7, b).await; // profundidad 7: no existe
        assert_eq!(c.error().await.0, BAD_VALUE);
        // Una fuente que no existe: BadName. Un identificador ajeno: BadIDChoice.
        let id = c.id();
        let mut b = c.body();
        b.u32(id).u16(9).u16(0).bytes(b"no-existe");
        c.request(45, 0, b).await;
        assert_eq!(c.error().await.0, BAD_NAME);
        let mut b = c.body();
        b.u32(0x1f00_0001).u32(ROOT).u16(10).u16(10);
        c.request(53, 24, b).await;
        assert_eq!(c.error().await.0, BAD_ID_CHOICE);
        // CopyArea entre profundidades distintas: BadMatch.
        let bitmap = c.create_pixmap(1, 8, 8).await;
        let gc = c.create_gc(ROOT, 0).await;
        let mut b = c.body();
        b.u32(bitmap).u32(ROOT).u32(gc);
        b.i16(0).i16(0).i16(0).i16(0).u16(4).u16(4);
        c.request(62, 0, b).await;
        assert_eq!(c.error().await.0, BAD_MATCH);
        // Los números de secuencia siguen contando las peticiones fallidas.
        c.sync().await;
        assert_eq!(c.seq, 15);
    }
    server.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn properties_and_structure_events_cross_byte_orders() {
    let (server, _backend) = start(640, 480);
    let mut big = TestClient::connect(&server, true).await;
    let mut little = TestClient::connect(&server, false).await;
    let mask = M_STRUCTURE_NOTIFY | M_EXPOSURE | M_PROPERTY_CHANGE;
    let window = big
        .create_window(
            (30, 40, 100, 80),
            &[(ATTR_OVERRIDE, 1), (ATTR_EVENTS, mask)],
        )
        .await;
    // Propiedad de 32 bits escrita por el cliente big-endian…
    let cardinal = 6;
    let name = big.intern("_VALORES").await;
    big.change_property(
        window,
        name,
        cardinal,
        32,
        &[0x11, 0x22, 0x33, 0x44, 0, 0, 0, 5],
    )
    .await;
    let ev = big.event(PROPERTY_NOTIFY).await;
    assert_eq!((ev.u32_at(4), ev.u32_at(8), ev.0[16]), (window, name, 0));
    assert_eq!(
        big.get_property(window, name).await,
        (cardinal, 32, vec![0x11, 0x22, 0x33, 0x44, 0, 0, 0, 5])
    );
    // …y leída por el little-endian: los mismos valores en su orden.
    let same = little.intern("_VALORES").await;
    assert_eq!(same, name);
    assert_eq!(
        little.get_property(window, name).await,
        (cardinal, 32, vec![0x44, 0x33, 0x22, 0x11, 5, 0, 0, 0])
    );
    // Las cadenas (formato 8) no se tocan; añadir al final y al principio.
    little
        .change_property(window, A_WM_NAME, A_STRING, 8, b"mundo")
        .await;
    for (mode, text) in [(2u8, &b"!"[..]), (1, &b"hola "[..])] {
        let mut b = little.body();
        b.u32(window).u32(A_WM_NAME).u32(A_STRING).u8(8).zeros(3);
        b.u32(text.len() as u32).bytes(text);
        little.request(18, mode, b).await;
    }
    // (El otro cliente no debe leer antes de que el servidor las procese.)
    little.sync().await;
    assert_eq!(
        big.get_property(window, A_WM_NAME).await,
        (A_STRING, 8, b"hola mundo!".to_vec())
    );
    // Tipo equivocado: dice cuál es y cuánto ocupa, sin datos.
    let mut b = big.body();
    b.u32(window).u32(A_WM_NAME).u32(A_ATOM).u32(0).u32(100);
    big.request(20, 0, b).await;
    let r = big.reply().await;
    assert_eq!(
        (r[1], big.u32(&r[8..12]), big.u32(&r[12..16])),
        (8, A_STRING, 11)
    );
    assert_eq!(big.u32(&r[16..20]), 0);
    // ListProperties y borrado.
    let mut b = little.body();
    b.u32(window);
    little.request(21, 0, b).await;
    let r = little.reply().await;
    assert_eq!(little.u16(&r[8..10]), 2);
    let mut b = little.body();
    b.u32(window).u32(name);
    little.request(19, 0, b).await;
    assert_eq!(little.get_property(window, name).await, (0, 0, vec![]));

    // Mapear: MapNotify y Expose con la geometría en el orden del cliente.
    big.map(window).await;
    let ev = big.event(MAP_NOTIFY).await;
    assert_eq!((ev.u32_at(4), ev.u32_at(8), ev.0[12]), (window, window, 1));
    let ev = big.event(EXPOSE).await;
    assert_eq!(ev.u32_at(4), window);
    assert_eq!(&ev.0[8..18], &[0, 0, 0, 0, 100, 0, 80, 0, 0, 0]);
    // ConfigureWindow (x, ancho): ConfigureNotify y Expose de lo nuevo.
    let mut b = big.body();
    b.u32(window).u16(1 | 4).u16(0).u32(55).u32(160);
    big.request(12, 0, b).await;
    let ev = big.event(CONFIGURE_NOTIFY).await;
    assert_eq!(&ev.0[16..24], &[55, 0, 40, 0, 160, 0, 80, 0]);
    // QueryTree de la raíz desde el otro cliente.
    let mut b = little.body();
    b.u32(ROOT);
    little.request(15, 0, b).await;
    let r = little.reply().await;
    assert_eq!(little.u16(&r[16..18]), 1);
    assert_eq!(little.u32(&r[32..36]), window);
    // TranslateCoordinates: (5, 5) de la ventana en la raíz.
    let mut b = little.body();
    b.u32(window).u32(ROOT).i16(5).i16(5);
    little.request(40, 0, b).await;
    let r = little.reply().await;
    assert_eq!((little.u16(&r[12..14]), little.u16(&r[14..16])), (60, 45));
    assert_eq!(
        little.u32(&r[8..12]),
        window,
        "hija de la raíz en ese punto"
    );
    // DestroyWindow: UnmapNotify y DestroyNotify.
    let mut b = little.body();
    b.u32(window);
    little.request(4, 0, b).await;
    big.event(UNMAP_NOTIFY).await;
    let ev = big.event(DESTROY_NOTIFY).await;
    assert_eq!(ev.u32_at(8), window);
    server.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn drawing_is_composed_onto_the_shared_frame() {
    let (server, backend) = start(200, 150);
    let desktop = pixel(&server, &backend, 150, 100);
    let mut c = TestClient::connect(&server, false).await;
    let window = c
        .create_window(
            (20, 30, 60, 40),
            &[(ATTR_BACKGROUND, 0x204080), (ATTR_OVERRIDE, 1)],
        )
        .await;
    c.map(window).await;
    c.sync().await;
    // Fondo dentro, escritorio fuera.
    assert_eq!(pixel(&server, &backend, 25, 35), [0x20, 0x40, 0x80]);
    assert_eq!(pixel(&server, &backend, 19, 35), desktop);
    assert_eq!(pixel(&server, &backend, 80, 35), desktop);
    // Un rectángulo relleno, recortado a la ventana.
    let gc = c.create_gc(window, 0xff8000).await;
    c.fill(window, gc, (50, 30, 100, 100)).await;
    c.sync().await;
    assert_eq!(
        pixel(&server, &backend, 20 + 55, 30 + 35),
        [0xff, 0x80, 0x00]
    );
    assert_eq!(
        pixel(&server, &backend, 20 + 45, 30 + 35),
        [0x20, 0x40, 0x80]
    );
    assert_eq!(pixel(&server, &backend, 85, 65), desktop);
    // Solo se marcó como cambiada la zona de la ventana.
    backend.frame.lock().unwrap().take_dirty();
    c.fill(window, gc, (0, 0, 5, 5)).await;
    c.sync().await;
    server.client_count();
    let dirty = backend
        .frame
        .lock()
        .unwrap()
        .take_dirty()
        .expect("zona cambiada");
    assert_eq!(
        (dirty.x, dirty.y, dirty.width, dirty.height),
        (20, 30, 5, 5)
    );

    // Una ventana hija tapa a la madre; al desmapearla vuelve lo de debajo.
    let child = c.id();
    let mut b = c.body();
    b.u32(child)
        .u32(window)
        .i16(10)
        .i16(10)
        .u16(20)
        .u16(10)
        .u16(0);
    b.u16(1).u32(0).u32(1 << ATTR_BACKGROUND).u32(0x00ff00);
    c.request(1, 0, b).await;
    c.map(child).await;
    c.sync().await;
    assert_eq!(pixel(&server, &backend, 35, 45), [0, 0xff, 0]);
    let mut b = c.body();
    b.u32(child);
    c.request(10, 0, b).await;
    c.sync().await;
    assert_eq!(pixel(&server, &backend, 35, 45), [0x20, 0x40, 0x80]);

    // CopyArea con «graphics exposures» contesta NoExpose (xterm lo espera
    // al desplazar) y copia de verdad.
    let exposing = c.id();
    let mut b = c.body();
    b.u32(exposing)
        .u32(window)
        .u32(1 << 2 | 1 << 16)
        .u32(0)
        .u32(1);
    c.request(55, 0, b).await;
    let mut b = c.body();
    b.u32(window).u32(window).u32(exposing);
    b.i16(50).i16(30).i16(10).i16(20).u16(10).u16(10);
    c.request(62, 0, b).await;
    let ev = c.event(NO_EXPOSE).await;
    assert_eq!((ev.u32_at(4), ev.0[10]), (window, 62));
    assert_eq!(
        pixel(&server, &backend, 20 + 12, 30 + 22),
        [0xff, 0x80, 0x00]
    );
    // Copiar desde fuera del origen: GraphicsExpose de lo que faltó.
    let mut b = c.body();
    b.u32(window).u32(window).u32(exposing);
    b.i16(55).i16(0).i16(0).i16(0).u16(10).u16(10);
    c.request(62, 0, b).await;
    let ev = c.event(GRAPHICS_EXPOSE).await;
    assert_eq!(&ev.0[8..16], &[5, 0, 0, 0, 5, 0, 10, 0]);

    // PutImage y GetImage en un pixmap (ZPixmap de 32 bits por píxel).
    let pixmap = c.create_pixmap(24, 2, 2).await;
    let pgc = c.create_gc(pixmap, 0).await;
    let image = [1u8, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0, 10, 11, 12, 0];
    let mut b = c.body();
    b.u32(pixmap)
        .u32(pgc)
        .u16(2)
        .u16(2)
        .i16(0)
        .i16(0)
        .u8(0)
        .u8(24);
    b.u16(0).bytes(&image);
    c.request(72, 2, b).await;
    let mut b = c.body();
    b.u32(pixmap).i16(0).i16(0).u16(2).u16(2).u32(0xffff_ffff);
    c.request(73, 2, b).await;
    let r = c.reply().await;
    assert_eq!((r[1], &r[32..]), (24, &image[..]));
    // GetImage de la raíz ve la pantalla compuesta (ventanas incluidas).
    let mut b = c.body();
    b.u32(ROOT).i16(25).i16(35).u16(1).u16(1).u32(0xffff_ffff);
    c.request(73, 2, b).await;
    let r = c.reply().await;
    assert_eq!(&r[32..36], &[0x80, 0x40, 0x20, 0]);
    // Texto: ImageText8 pinta fondo y letras.
    let text_gc = c.id();
    let mut b = c.body();
    b.u32(text_gc)
        .u32(window)
        .u32(1 << 2 | 1 << 3)
        .u32(0xffffff)
        .u32(0);
    c.request(55, 0, b).await;
    let mut b = c.body();
    b.u32(window).u32(text_gc).i16(2).i16(36).bytes(b"Hola");
    c.request(76, 4, b).await;
    c.sync().await;
    server.client_count();
    let (mut black, mut light) = (0, 0);
    {
        let fb = backend.frame.lock().unwrap();
        for y in 30 + 26..30 + 39 {
            for x in 22..22 + 28 {
                let i = ((y * fb.width() + x) * 4) as usize;
                match fb.pixels[i] {
                    0 => black += 1,
                    100.. => light += 1,
                    _ => {}
                }
            }
        }
    }
    assert!(black > 150 && light > 15, "fondo {black}, tinta {light}");
    server.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn input_reaches_the_window_under_the_pointer_and_the_focus() {
    let (server, backend) = start(400, 300);
    for be in [false, true] {
        let mut c = TestClient::connect(&server, be).await;
        let mask = M_KEY_PRESS | M_KEY_RELEASE | M_BUTTON_PRESS | M_BUTTON_RELEASE;
        let mask = mask | M_ENTER_WINDOW | M_POINTER_MOTION;
        let window = c
            .create_window(
                (100, 50, 80, 60),
                &[(ATTR_OVERRIDE, 1), (ATTR_EVENTS, mask)],
            )
            .await;
        c.map(window).await;
        c.sync().await;
        let send = |input| backend.input.send(input).unwrap();
        send(DesktopInput::PointerMove { x: 110, y: 65 });
        let ev = c.event(ENTER_NOTIFY).await;
        assert_eq!(ev.u32_at(12), window);
        // Coordenadas de raíz y de ventana.
        assert_eq!(&ev.0[20..28], &[110, 0, 65, 0, 10, 0, 15, 0]);
        send(DesktopInput::PointerButton {
            button: MouseButton::Right,
            pressed: true,
            x: 120,
            y: 70,
        });
        let ev = c.event(BUTTON_PRESS).await;
        assert_eq!((ev.0[1], ev.u32_at(8), ev.u32_at(12)), (3, ROOT, window));
        assert_eq!(&ev.0[24..28], &[20, 0, 20, 0]);
        assert_eq!(u16::from_le_bytes([ev.0[28], ev.0[29]]), 0, "estado antes");
        send(DesktopInput::PointerButton {
            button: MouseButton::Right,
            pressed: false,
            x: 120,
            y: 70,
        });
        let ev = c.event(BUTTON_RELEASE).await;
        // Al soltar, el estado aún lleva el botón 3 pulsado.
        assert_eq!(u16::from_le_bytes([ev.0[28], ev.0[29]]), S_BUTTON1 << 2);

        // QueryPointer coincide.
        let mut b = c.body();
        b.u32(window);
        c.request(38, 0, b).await;
        let r = c.reply().await;
        assert_eq!((c.u16(&r[16..18]), c.u16(&r[18..20])), (120, 70));
        assert_eq!((c.u16(&r[20..22]), c.u16(&r[22..24])), (20, 20));

        // Teclado: con el puntero fuera, va a la ventana que tiene el foco.
        send(DesktopInput::PointerMove { x: 5, y: 5 });
        let mut b = c.body();
        b.u32(window).u32(0);
        c.request(42, 0, b).await;
        c.sync().await;
        send(DesktopInput::Text("a".into()));
        let code = lock(&server.inner.state).keymap.keycode(0x61).unwrap();
        let ev = c.event(KEY_PRESS).await;
        assert_eq!((ev.0[1], ev.u32_at(12)), (code, window));
        assert_eq!(c.event(KEY_RELEASE).await.0[1], code);
        // Un carácter que no cabe en el mapa fijo: MappingNotify y su tecla.
        // (Uno distinto en cada vuelta: el primero ya tiene tecla asignada.)
        let (extra, keysym) = if be {
            ('λ', 0x0100_03bb)
        } else {
            ('€', 0x0100_20ac)
        };
        send(DesktopInput::Text(extra.to_string()));
        let ev = c.event(MAPPING_NOTIFY).await;
        let assigned = ev.0[5];
        assert_eq!((ev.0[4], ev.0[6]), (1, 1));
        assert_eq!(c.event(KEY_PRESS).await.0[1], assigned);
        c.event(KEY_RELEASE).await;
        let mut b = c.body();
        b.u8(assigned).u8(1);
        c.request(101, 0, b).await;
        let r = c.reply().await;
        assert_eq!(c.u32(&r[32..36]), keysym);
        // Sin foco en ninguna ventana no llega ninguna tecla: lo siguiente
        // que ve el cliente es la entrada del puntero.
        let mut b = c.body();
        b.u32(0).u32(0);
        c.request(42, 0, b).await;
        c.sync().await;
        send(DesktopInput::Text("z".into()));
        send(DesktopInput::PointerMove { x: 150, y: 80 });
        let p = c.packet().await.unwrap();
        assert_eq!(p[0], ENTER_NOTIFY, "{:?}", &p[..8]);
        // El puntero vuelve a una esquina para la siguiente vuelta.
        send(DesktopInput::PointerMove { x: 5, y: 5 });
        loop {
            let mut b = c.body();
            b.u32(ROOT);
            c.request(38, 0, b).await;
            let r = c.reply().await;
            if (c.u16(&r[16..18]), c.u16(&r[18..20])) == (5, 5) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        drop(c);
        for _ in 0..200 {
            if server.client_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    server.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn clipboard_goes_both_ways() {
    let (server, backend) = start(320, 200);
    let mut c = TestClient::connect(&server, true).await;
    let clipboard = c.intern("CLIPBOARD").await;
    let utf8 = c.intern("UTF8_STRING").await;
    let targets = c.intern("TARGETS").await;
    let destination = c.intern("_DESTINO").await;
    let window = c.create_window((0, 0, 10, 10), &[(ATTR_OVERRIDE, 1)]).await;

    // --- De la interfaz a los clientes --------------------------------------
    backend
        .input
        .send(DesktopInput::ClipboardText("café con leche".into()))
        .unwrap();
    let owner = loop {
        let mut b = c.body();
        b.u32(clipboard);
        c.request(23, 0, b).await;
        let r = c.reply().await;
        let owner = c.u32(&r[8..12]);
        if owner != 0 {
            break owner;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(owner, SERVER_WINDOW);
    for (target, check) in [(utf8, 0), (A_STRING, 1), (targets, 2)] {
        let mut b = c.body();
        b.u32(window)
            .u32(clipboard)
            .u32(target)
            .u32(destination)
            .u32(0);
        c.request(24, 0, b).await;
        let ev = c.event(SELECTION_NOTIFY).await;
        assert_eq!((ev.u32_at(8), ev.u32_at(20)), (window, destination));
        let (kind, format, data) = c.get_property(window, destination).await;
        match check {
            0 => assert_eq!(
                (kind, format, &data[..]),
                (utf8, 8, "café con leche".as_bytes())
            ),
            1 => assert_eq!((kind, &data[..]), (A_STRING, &b"caf\xe9 con leche"[..])),
            _ => {
                assert_eq!((kind, format), (A_ATOM, 32));
                let offered: Vec<u32> = data.chunks(4).map(|a| c.u32(a)).collect();
                assert!(offered.contains(&utf8) && offered.contains(&A_STRING));
            }
        }
    }
    // Un formato que no se sabe dar: se rechaza (propiedad «None»).
    let mut b = c.body();
    b.u32(window)
        .u32(clipboard)
        .u32(A_ATOM)
        .u32(destination)
        .u32(0);
    c.request(24, 0, b).await;
    assert_eq!(c.event(SELECTION_NOTIFY).await.u32_at(20), 0);

    // --- De un cliente a la interfaz -----------------------------------------
    let mut b = c.body();
    b.u32(window).u32(clipboard).u32(0);
    c.request(22, 0, b).await;
    // El servidor pide el texto: SelectionRequest a nombre de su ventana.
    let ev = c.event(SELECTION_REQUEST).await;
    let (requestor, target, property) = (ev.u32_at(12), ev.u32_at(20), ev.u32_at(24));
    assert_eq!(
        (ev.u32_at(8), requestor, target),
        (window, SERVER_WINDOW, utf8)
    );
    c.change_property(
        requestor,
        property,
        utf8,
        8,
        "copiado en remoto ✓".as_bytes(),
    )
    .await;
    let mut b = c.body();
    b.u32(requestor).u32(0);
    b.u8(SELECTION_NOTIFY).u8(0).u16(0).u32(0);
    b.u32(requestor)
        .u32(clipboard)
        .u32(target)
        .u32(property)
        .zeros(8);
    c.request(25, 0, b).await;
    c.sync().await;
    let text = loop {
        match backend.events.recv_timeout(WAIT) {
            Ok(DesktopEvent::ClipboardText(text)) => break text,
            Ok(_) => {}
            Err(_) => panic!("no llegó el portapapeles del cliente"),
        }
    };
    assert_eq!(text, "copiado en remoto ✓");
    // Devolver ese mismo texto desde la interfaz no le quita la selección.
    backend
        .input
        .send(DesktopInput::ClipboardText(text))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut b = c.body();
    b.u32(clipboard);
    c.request(23, 0, b).await;
    let r = c.reply().await;
    assert_eq!(c.u32(&r[8..12]), window);
    server.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn built_in_window_manager_decorates_focuses_and_kills() {
    let (server, backend) = start(400, 300);
    let mut c = TestClient::connect(&server, false).await;
    let mask = M_STRUCTURE_NOTIFY | M_FOCUS_CHANGE;
    let window = c
        .create_window(
            (0, 0, 120, 80),
            &[(ATTR_BACKGROUND, 0xffffff), (ATTR_EVENTS, mask)],
        )
        .await;
    c.change_property(window, A_WM_NAME, A_STRING, 8, b"Ventana de prueba")
        .await;
    c.map(window).await;
    // Sin posición pedida se coloca en cascada: llega el ConfigureNotify.
    let ev = c.event(CONFIGURE_NOTIFY).await;
    let x = i16::from_le_bytes([ev.0[16], ev.0[17]]) as i32;
    let y = i16::from_le_bytes([ev.0[18], ev.0[19]]) as i32;
    assert!(x >= FRAME && y >= TITLE_H, "({x}, {y})");
    c.event(MAP_NOTIFY).await;
    let ev = c.event(FOCUS_IN).await;
    assert_eq!(ev.u32_at(4), window);
    c.sync().await;
    // La barra de título se ve encima de la ventana, con texto.
    let bar = pixel(&server, &backend, (x + 100) as u32, (y - 4) as u32);
    assert_ne!(bar, [0xff, 0xff, 0xff]);
    assert_ne!(bar, pixel(&server, &backend, 390, 290));
    let mut colors = std::collections::HashSet::new();
    for dx in 0..100 {
        colors.insert(pixel(&server, &backend, (x + dx) as u32, (y - 9) as u32));
    }
    assert!(colors.len() > 2, "título sin texto: {colors:?}");
    assert_eq!(
        pixel(&server, &backend, (x + 5) as u32, (y + 5) as u32),
        [0xff; 3]
    );

    // Arrastrar la barra mueve la ventana.
    let send = |input| backend.input.send(input).unwrap();
    let grab = ((x + 30) as u16, (y - 10) as u16);
    send(DesktopInput::PointerButton {
        button: MouseButton::Left,
        pressed: true,
        x: grab.0,
        y: grab.1,
    });
    send(DesktopInput::PointerMove {
        x: grab.0 + 40,
        y: grab.1 + 25,
    });
    send(DesktopInput::PointerButton {
        button: MouseButton::Left,
        pressed: false,
        x: grab.0 + 40,
        y: grab.1 + 25,
    });
    let ev = c.event(CONFIGURE_NOTIFY).await;
    let moved = (
        i16::from_le_bytes([ev.0[16], ev.0[17]]) as i32,
        i16::from_le_bytes([ev.0[18], ev.0[19]]) as i32,
    );
    assert_eq!(moved, (x + 40, y + 25));

    // La ventana no declara WM_DELETE_WINDOW: el botón de cerrar corta la
    // conexión de su cliente (y solo la suya).
    let mut other = TestClient::connect(&server, true).await;
    other.sync().await;
    let close = (
        (moved.0 + 120 + FRAME - 12) as u16,
        (moved.1 - TITLE_H / 2) as u16,
    );
    for pressed in [true, false] {
        send(DesktopInput::PointerButton {
            button: MouseButton::Left,
            pressed,
            x: close.0,
            y: close.1,
        });
    }
    loop {
        match c.packet().await {
            None => break,
            Some(p) => assert!(p[0] > 1, "{:?}", &p[..8]),
        }
    }
    other.sync().await;
    assert_eq!(server.client_count(), 1);
    assert_eq!(
        pixel(
            &server,
            &backend,
            (moved.0 + 5) as u32,
            (moved.1 + 5) as u32
        ),
        pixel(&server, &backend, 390, 290),
        "la ventana del cliente expulsado sigue en pantalla"
    );
    server.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_window_manager_gets_the_requests_redirected() {
    let (server, backend) = start(400, 300);
    let mut wm = TestClient::connect(&server, false).await;
    let mut b = wm.body();
    b.u32(ROOT).u32(1 << ATTR_EVENTS);
    b.u32(M_SUBSTRUCTURE_REDIRECT | M_SUBSTRUCTURE_NOTIFY);
    wm.request(2, 0, b).await;
    wm.sync().await;
    // Solo puede haber uno.
    let mut second = TestClient::connect(&server, false).await;
    let mut b = second.body();
    b.u32(ROOT)
        .u32(1 << ATTR_EVENTS)
        .u32(M_SUBSTRUCTURE_REDIRECT);
    second.request(2, 0, b).await;
    assert_eq!(second.error().await.0, BAD_ACCESS);

    let mut app = TestClient::connect(&server, true).await;
    let window = app
        .create_window((10, 20, 100, 50), &[(ATTR_BACKGROUND, 0xffffff)])
        .await;
    let ev = wm.event(CREATE_NOTIFY).await;
    assert_eq!((ev.u32_at(4), ev.u32_at(8)), (ROOT, window));
    // MapWindow no mapea: le llega MapRequest al gestor.
    app.map(window).await;
    let ev = wm.event(MAP_REQUEST).await;
    assert_eq!((ev.u32_at(4), ev.u32_at(8)), (ROOT, window));
    app.sync().await;
    let desktop = pixel(&server, &backend, 390, 290);
    assert_eq!(
        pixel(&server, &backend, 50, 40),
        desktop,
        "aún no debe verse"
    );
    // ConfigureWindow tampoco: ConfigureRequest con lo pedido.
    let mut b = app.body();
    b.u32(window).u16(1 | 2 | 4).u16(0).u32(30).u32(40).u32(150);
    app.request(12, 0, b).await;
    let ev = wm.event(CONFIGURE_REQUEST).await;
    assert_eq!(ev.u32_at(8), window);
    assert_eq!(&ev.0[16..22], &[30, 0, 40, 0, 150, 0]);
    assert_eq!(u16::from_le_bytes([ev.0[26], ev.0[27]]), 1 | 2 | 4);
    // El gestor la coloca y la mapea él: ahora sí, y sin decoración propia.
    let mut b = wm.body();
    b.u32(window).u16(1 | 2).u16(0).u32(60).u32(70);
    wm.request(12, 0, b).await;
    wm.map(window).await;
    wm.sync().await;
    assert_eq!(pixel(&server, &backend, 65, 75), [0xff; 3]);
    assert_eq!(
        pixel(&server, &backend, 65, 65),
        desktop,
        "sin barra de título"
    );
    // Al irse el gestor vuelven las decoraciones del servidor.
    drop(wm);
    for _ in 0..200 {
        if server.client_count() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_ne!(
        pixel(&server, &backend, 65, 65),
        desktop,
        "con barra de título"
    );
    server.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_is_budgeted_and_lengths_are_checked() {
    let (server, _backend) = start(100, 100);
    // Presupuesto pequeño para la prueba: 100 000 píxeles.
    lock(&server.inner.state).budget = Budget::new(100_000);
    let mut c = TestClient::connect(&server, false).await;
    let used = |server: &XServer| lock(&server.inner.state).budget.used();

    // --- Presupuesto global de imagen ---------------------------------------
    let first = c.create_pixmap(24, 200, 200).await;
    let second = c.create_pixmap(24, 200, 200).await;
    c.sync().await;
    assert_eq!(used(&server), 80_000);
    let third = c.id();
    let mut b = c.body();
    b.u32(third).u32(ROOT).u16(200).u16(200);
    c.request(53, 24, b).await;
    assert_eq!(c.error().await, (BAD_ALLOC, third, c.seq, 53));
    // Tampoco una ventana grande…
    let window = c.id();
    let mut b = c.body();
    b.u32(window)
        .u32(ROOT)
        .i16(0)
        .i16(0)
        .u16(300)
        .u16(300)
        .u16(0);
    b.u16(1).u32(0).u32(0);
    c.request(1, 0, b).await;
    assert_eq!(c.error().await.0, BAD_ALLOC);
    assert_eq!(used(&server), 80_000, "lo rechazado no se queda reservado");
    // …pero al liberar un pixmap vuelve a haber sitio.
    let mut b = c.body();
    b.u32(first);
    c.request(54, 0, b).await;
    c.sync().await;
    assert_eq!(used(&server), 40_000);
    let small = c
        .create_window((0, 0, 100, 100), &[(ATTR_OVERRIDE, 1)])
        .await;
    c.sync().await;
    assert_eq!(used(&server), 50_000);
    // El mosaico de un GC conserva el contenido del pixmap aunque este se
    // libere: sigue contando hasta que se libera el GC.
    let gc = c.id();
    let mut b = c.body();
    b.u32(gc).u32(ROOT).u32(1 << 10).u32(second);
    c.request(55, 0, b).await;
    let mut b = c.body();
    b.u32(second);
    c.request(54, 0, b).await;
    c.sync().await;
    assert_eq!(used(&server), 50_000);
    let mut b = c.body();
    b.u32(gc);
    c.request(60, 0, b).await;
    c.sync().await;
    assert_eq!(used(&server), 10_000);
    // Un pixmap desmesurado no se intenta siquiera.
    let huge = c.id();
    let mut b = c.body();
    b.u32(huge).u32(ROOT).u16(65_535).u16(65_535);
    c.request(53, 24, b).await;
    assert_eq!(c.error().await.0, BAD_ALLOC);
    // Destruir la ventana devuelve lo suyo.
    let mut b = c.body();
    b.u32(small);
    c.request(4, 0, b).await;
    c.sync().await;
    assert_eq!(used(&server), 0);

    // --- Peticiones de imagen -------------------------------------------------
    lock(&server.inner.state).budget = Budget::new(u64::MAX);
    // GetImage de 65535×65535 no reserva 16 GB: BadAlloc.
    let mut b = c.body();
    b.u32(ROOT).i16(0).i16(0).u16(65_535).u16(65_535).u32(!0);
    c.request(73, 2, b).await;
    assert_eq!(c.error().await.0, BAD_ALLOC);
    // PutImage que dice traer más de lo que trae: BadLength, sin reservar.
    let gc = c.create_gc(ROOT, 0).await;
    for format in [0u8, 1, 2] {
        let depth = if format == 0 { 1 } else { 24 };
        let mut b = c.body();
        b.u32(ROOT)
            .u32(gc)
            .u16(2000)
            .u16(2000)
            .i16(0)
            .i16(0)
            .u8(0)
            .u8(depth);
        b.u16(0).bytes(&[0xff; 64]);
        c.request(72, format, b).await;
        assert_eq!(c.error().await.0, BAD_LENGTH, "formato {format}");
    }
    let mut b = c.body();
    b.u32(ROOT)
        .u32(gc)
        .u16(60_000)
        .u16(60_000)
        .i16(0)
        .i16(0)
        .u8(0)
        .u8(24);
    b.u16(0);
    c.request(72, 2, b).await;
    assert_eq!(c.error().await.0, BAD_ALLOC);
    // CopyArea de un área gigantesca: solo se copia lo que existe.
    let mut b = c.body();
    b.u32(ROOT).u32(ROOT).u32(gc);
    b.i16(-30_000)
        .i16(-30_000)
        .i16(-20_000)
        .i16(-20_000)
        .u16(65_535)
        .u16(65_535);
    c.request(62, 0, b).await;
    c.event(GRAPHICS_EXPOSE).await;

    // --- Longitudes declaradas mayores que la petición -----------------------
    // ChangeProperty: dice 1 000 000 de elementos y trae 4 bytes.
    let mut b = c.body();
    b.u32(ROOT).u32(A_WM_NAME).u32(A_STRING).u8(32).zeros(3);
    b.u32(1_000_000).u32(0);
    c.request(18, 0, b).await;
    assert_eq!(c.error().await.0, BAD_LENGTH);
    // InternAtom, OpenFont, ListFonts y AllocNamedColor con nombre «largo».
    for (opcode, prefix) in [(16u8, 0), (45, 4), (49, 0), (85, 4)] {
        let mut b = c.body();
        b.zeros(prefix);
        if opcode == 49 {
            b.u16(10).u16(60_000);
        } else {
            b.u16(60_000).u16(0);
        }
        b.bytes(b"abcd");
        if opcode == 45 {
            // (un identificador válido para que llegue a mirar el nombre)
            let id = c.id().to_le_bytes();
            b.buf[..4].copy_from_slice(&id);
        } else if opcode == 85 {
            b.buf[..4].copy_from_slice(&0x101u32.to_le_bytes());
        }
        c.request(opcode, 0, b).await;
        assert_eq!(c.error().await.0, BAD_LENGTH, "petición {opcode}");
    }
    // ImageText8 y PolyText8 con más caracteres de los que vienen.
    let mut b = c.body();
    b.u32(ROOT).u32(gc).i16(0).i16(10).bytes(b"ab");
    c.request(76, 200, b).await;
    assert_eq!(c.error().await.0, BAD_LENGTH);
    let mut b = c.body();
    b.u32(ROOT)
        .u32(gc)
        .i16(0)
        .i16(10)
        .u8(200)
        .u8(0)
        .bytes(b"ab");
    c.request(74, 0, b).await;
    assert_eq!(c.error().await.0, BAD_LENGTH);
    // SetDashes y RotateProperties.
    let mut b = c.body();
    b.u32(gc).u16(0).u16(500).bytes(&[1, 1]);
    c.request(58, 0, b).await;
    assert_eq!(c.error().await.0, BAD_LENGTH);
    let mut b = c.body();
    b.u32(ROOT).u16(500).i16(1).u32(A_WM_NAME);
    c.request(114, 0, b).await;
    assert_eq!(c.error().await.0, BAD_LENGTH);

    // --- Árbol de ventanas: profundidad acotada -------------------------------
    let mut parent = ROOT;
    let mut refused = None;
    for depth in 0..300 {
        let id = c.id();
        let mut b = c.body();
        b.u32(id).u32(parent).i16(0).i16(0).u16(1).u16(1).u16(0);
        b.u16(1).u32(0).u32(0);
        c.request(1, 0, b).await;
        let b = c.body();
        c.request(43, 0, b).await;
        let p = c.packet().await.unwrap();
        if p[0] == 0 {
            assert_eq!(p[1], BAD_ALLOC);
            c.packet().await.unwrap();
            refused = Some(depth);
            break;
        }
        parent = id;
    }
    let depth = refused.expect("se pudo anidar sin límite");
    assert!((100..=250).contains(&depth), "{depth}");
    // Y todo ese árbol se destruye sin problema al irse el cliente.
    drop(c);
    for _ in 0..500 {
        if server.client_count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(lock(&server.inner.state).windows.len(), 2);
    server.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_panicking_request_only_expels_its_client() {
    let (server, backend) = start(200, 150);
    let mut victim = TestClient::connect(&server, false).await;
    let mut other = TestClient::connect(&server, true).await;
    // La víctima tiene una ventana en pantalla; el otro, una propiedad.
    let window = victim
        .create_window(
            (10, 10, 50, 50),
            &[(ATTR_BACKGROUND, 0xff0000), (ATTR_OVERRIDE, 1)],
        )
        .await;
    victim.map(window).await;
    victim.sync().await;
    assert_eq!(pixel(&server, &backend, 20, 20), [0xff, 0, 0]);
    let mine = other
        .create_window(
            (100, 10, 50, 50),
            &[(ATTR_BACKGROUND, 0x00ff00), (ATTR_OVERRIDE, 1)],
        )
        .await;
    other.map(mine).await;
    other.sync().await;

    // Gancho de pruebas: la petición «Bell» entra en pánico en su manejador.
    lock(&server.inner.state).panic_on_opcode = Some(104);
    let b = victim.body();
    victim.request(104, 0, b).await;
    // A la víctima se le corta la conexión…
    loop {
        match victim.packet().await {
            None => break,
            Some(p) => assert!(p[0] > 1),
        }
    }
    lock(&server.inner.state).panic_on_opcode = None;
    // …y el servidor sigue: el candado se puede tomar, el otro cliente
    // contesta, la ventana de la víctima ha desaparecido y la del otro no.
    assert_eq!(server.client_count(), 1);
    other.sync().await;
    let desktop = pixel(&server, &backend, 190, 140);
    assert_eq!(pixel(&server, &backend, 20, 20), desktop);
    assert_eq!(pixel(&server, &backend, 110, 20), [0, 0xff, 0]);
    // Las llamadas de la interfaz funcionan y se admiten clientes nuevos.
    server.resize(300, 200);
    assert_eq!(backend.frame.lock().unwrap().width(), 300);
    let mut fresh = TestClient::connect(&server, false).await;
    fresh.sync().await;
    assert_eq!(server.client_count(), 2);
    // Otro pánico, ahora del otro cliente: mismo trato.
    lock(&server.inner.state).panic_on_opcode = Some(4);
    let mut b = other.body();
    b.u32(mine);
    other.request(4, 0, b).await;
    while other.packet().await.is_some() {}
    lock(&server.inner.state).panic_on_opcode = None;
    fresh.sync().await;
    assert_eq!(server.client_count(), 1);
    assert_eq!(pixel(&server, &backend, 110, 20), desktop);
    let contained = server
        .inner
        .panics
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(contained, 2);
    server.stop();
    assert!(
        backend
            .events
            .try_iter()
            .any(|e| matches!(e, DesktopEvent::Closed(None)))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_does_not_read_is_throttled() {
    let (server, _backend) = start(800, 600);
    let mut c = TestClient::connect(&server, false).await;
    // 300 capturas de 400×300 (480 kB cada una) sin leer ni una respuesta:
    // 144 MB si el servidor las generase todas de golpe.
    const REQUESTS: usize = 300;
    for _ in 0..REQUESTS {
        let mut b = c.body();
        b.u32(ROOT).i16(0).i16(0).u16(400).u16(300).u32(!0);
        c.request(73, 2, b).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let queued = {
        let st = lock(&server.inner.state);
        let client = st.clients.values().next().expect("cliente");
        client
            .link
            .queued
            .load(std::sync::atomic::Ordering::Relaxed)
            + client.out.len()
    };
    assert!(queued < 4 * 1024 * 1024, "salida acumulada: {queued} bytes");
    // Cuando el cliente lee, le llegan todas, en orden.
    for i in 0..REQUESTS {
        let p = c.packet().await.expect("respuesta");
        assert_eq!((p[0], p.len()), (1, 32 + 400 * 300 * 4));
        assert_eq!(c.u16(&p[2..4]) as usize, i + 1);
    }
    server.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn xkb_is_only_offered_when_asked_for() {
    let (server, _backend) = start(320, 200);
    let query = |c: &TestClient| {
        let mut b = c.body();
        b.u16(9).u16(0).bytes(b"XKEYBOARD");
        b
    };
    let mut plain = TestClient::connect(&server, false).await;
    let b = query(&plain);
    plain.request(98, 0, b).await;
    assert_eq!(plain.reply().await[8], 0);
    let b = plain.body();
    plain.request(135, 0, b).await;
    assert_eq!(plain.error().await.0, BAD_REQUEST);

    server.set_xkb_compat(true);
    for be in [false, true] {
        let mut c = TestClient::connect(&server, be).await;
        let b = query(&c);
        c.request(98, 0, b).await;
        let r = c.reply().await;
        assert_eq!((r[8], r[9]), (1, 135));
        // UseExtension y GetMap con tipos, símbolos y modificadores.
        let mut b = c.body();
        b.u16(1).u16(0);
        c.request(135, 0, b).await;
        assert_eq!(c.reply().await[1], 1);
        let mut b = c.body();
        b.u16(0x100).u16(7).u16(0).zeros(18);
        c.request(135, 8, b).await;
        let r = c.reply().await;
        assert_eq!((r[10], r[11], c.u16(&r[12..14])), (8, 255, 7));
        assert_eq!((r[15], r[17], r[20]), (4, 8, 248), "tipos y teclas");
        // Tras los 4 tipos (8 bytes cada uno más sus entradas) van las teclas.
        let syms = 40 + 4 * 8 + 4 * 8;
        // La primera tecla (código 8) no tiene símbolos.
        assert_eq!(&r[syms..syms + 8], &[0; 8]);
    }
    server.stop();
}

/// Generador pseudoaleatorio determinista (xorshift) para la prueba de
/// peticiones al azar.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u32) -> u32 {
        (self.next() % n.max(1) as u64) as u32
    }

    fn pick(&mut self, values: &[u32]) -> u32 {
        values[self.below(values.len() as u32) as usize]
    }

    fn chance(&mut self, percent: u32) -> bool {
        self.below(100) < percent
    }
}

/// Bytes de una petición con su cabecera.
fn raw_request(be: bool, opcode: u8, data: u8, mut body: Writer) -> Vec<u8> {
    body.pad();
    let mut w = Writer::new(be);
    w.u8(opcode)
        .u8(data)
        .u16((body.buf.len() / 4 + 1) as u16)
        .bytes(&body.buf);
    w.buf
}

/// Recursos con los que juega la prueba de peticiones al azar. Sus
/// identificadores son fijos para poder recrearlos cuando el azar los
/// destruye.
struct FuzzIds {
    top: u32,
    child: u32,
    popup: u32,
    pixmap: u32,
    bitmap: u32,
    gc: u32,
    bitmap_gc: u32,
    font: u32,
}

impl FuzzIds {
    fn new(base: u32) -> Self {
        FuzzIds {
            top: base | 0x101,
            child: base | 0x102,
            popup: base | 0x103,
            pixmap: base | 0x104,
            bitmap: base | 0x105,
            gc: base | 0x106,
            bitmap_gc: base | 0x107,
            font: base | 0x108,
        }
    }

    /// Peticiones que crean (o vuelven a crear) todos los recursos.
    fn create_all(&self, be: bool) -> Vec<u8> {
        let mut out = vec![];
        let window =
            |id: u32, parent: u32, geometry: (i16, i16, u16, u16), attrs: &[(u32, u32)]| {
                let mut b = Writer::new(be);
                let (x, y, w, h) = geometry;
                b.u32(id).u32(parent).i16(x).i16(y).u16(w).u16(h).u16(1);
                b.u16(1).u32(0);
                b.u32(attrs.iter().fold(0, |m, (bit, _)| m | (1 << bit)));
                for (_, value) in attrs {
                    b.u32(*value);
                }
                raw_request(be, 1, 0, b)
            };
        let events = M_ALL & !M_SUBSTRUCTURE_REDIRECT & !M_RESIZE_REDIRECT;
        out.extend(window(
            self.top,
            ROOT,
            (20, 30, 120, 90),
            &[(ATTR_BACKGROUND, 0xffffff), (ATTR_EVENTS, events)],
        ));
        out.extend(window(
            self.child,
            self.top,
            (5, 5, 40, 30),
            &[(ATTR_EVENTS, events & !M_BUTTON_PRESS)],
        ));
        out.extend(window(
            self.popup,
            ROOT,
            (150, 20, 60, 60),
            &[(ATTR_OVERRIDE, 1)],
        ));
        for id in [self.top, self.child, self.popup] {
            let mut b = Writer::new(be);
            b.u32(id);
            out.extend(raw_request(be, 8, 0, b));
        }
        for (id, depth, w, h) in [(self.pixmap, 24u8, 64u16, 48u16), (self.bitmap, 1, 16, 16)] {
            let mut b = Writer::new(be);
            b.u32(id).u32(ROOT).u16(w).u16(h);
            out.extend(raw_request(be, 53, depth, b));
        }
        for (id, drawable) in [(self.gc, ROOT), (self.bitmap_gc, self.bitmap)] {
            let mut b = Writer::new(be);
            b.u32(id).u32(drawable).u32(1 << 2).u32(0x336699);
            out.extend(raw_request(be, 55, 0, b));
        }
        let mut b = Writer::new(be);
        b.u32(self.font).u16(5).u16(0).bytes(b"fixed");
        out.extend(raw_request(be, 45, 0, b));
        out
    }

    fn window(&self, rng: &mut Rng) -> u32 {
        rng.pick(&[ROOT, self.top, self.top, self.child, self.popup])
    }

    /// Un *drawable* y un GC de su misma profundidad.
    fn drawable_and_gc(&self, rng: &mut Rng) -> (u32, u32) {
        if rng.chance(20) {
            (self.bitmap, self.bitmap_gc)
        } else {
            (
                rng.pick(&[ROOT, self.top, self.child, self.popup, self.pixmap]),
                self.gc,
            )
        }
    }
}

/// Los clientes X son entrada no fiable: miles de peticiones al azar (bien
/// formadas a medias, con identificadores válidos mezclados con valores
/// límite) y entrada de teclado y ratón intercalada no deben provocar ni un
/// solo pánico, ni siquiera de los que el servidor sabe contener.
#[tokio::test(flavor = "multi_thread")]
async fn random_requests_never_panic() {
    const BUDGET: u64 = 3_000_000;
    const REQUESTS: usize = 15_000;
    // Dos semillas fijas; con `GODREMOTE_X11_FUZZ=n` se prueban n más.
    let mut seeds = vec![
        (false, 0x9e37_79b9_7f4a_7c15u64),
        (true, 0x0123_4567_89ab_cdef),
    ];
    let extra = std::env::var("GODREMOTE_X11_FUZZ")
        .ok()
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(0);
    for n in 0..extra {
        let seed = (n + 1).wrapping_mul(0x2545_f491_4f6c_dd1d) | 1;
        seeds.push((n % 2 == 1, seed));
    }
    for (be, seed) in seeds {
        let (server, backend) = start(300, 200);
        // Con poco presupuesto de imagen las reservas grandes fallan pronto
        // (y de paso la prueba no necesita gigas de memoria).
        lock(&server.inner.state).budget = Budget::new(BUDGET);
        let c = TestClient::connect(&server, be).await;
        let ids = FuzzIds::new(c.base);
        let base = c.base;
        let known = [
            ROOT,
            SERVER_WINDOW,
            super::server::DEFAULT_COLORMAP,
            ids.top,
            ids.child,
            ids.popup,
            ids.pixmap,
            ids.bitmap,
            ids.gc,
            ids.bitmap_gc,
            ids.font,
        ];
        let edges = [
            0,
            1,
            2,
            3,
            4,
            8,
            16,
            24,
            32,
            255,
            256,
            0x7fff,
            0x8000,
            0xffff,
            0x1_0000,
            0x7fff_ffff,
            0x8000_0000,
            0xffff_ffff,
        ];
        let mut rng = Rng(seed);
        let value = |rng: &mut Rng| -> u32 {
            match rng.below(12) {
                0..=2 => known[rng.below(known.len() as u32) as usize],
                // Identificadores propios aún sin usar (o ya usados).
                3 => base | rng.below(48),
                4 => rng.pick(&edges),
                5..=7 => rng.below(80),
                8 => rng.below(400),
                9 => 0x1_0000 - rng.below(300),
                10 => rng.below(1 << 20),
                _ => rng.next() as u32,
            }
        };
        // El servidor contesta sin parar: alguien tiene que leer.
        let (mut reader, mut writer) = tokio::io::split(c.stream);
        let drain = tokio::spawn(async move {
            let mut buf = vec![0u8; 1 << 16];
            while matches!(reader.read(&mut buf).await, Ok(n) if n > 0) {}
        });
        let input = |rng: &mut Rng| -> DesktopInput {
            let (x, y) = (rng.below(320) as u16, rng.below(220) as u16);
            match rng.below(6) {
                0 | 1 => DesktopInput::PointerMove { x, y },
                2 | 3 => DesktopInput::PointerButton {
                    button: [MouseButton::Left, MouseButton::Middle, MouseButton::Right]
                        [rng.below(3) as usize],
                    pressed: rng.below(2) == 0,
                    x,
                    y,
                },
                4 => DesktopInput::Text("aÑ€\n".into()),
                _ => DesktopInput::Wheel {
                    x,
                    y,
                    delta: rng.below(5) as i16 - 2,
                    horizontal: rng.below(2) == 0,
                },
            }
        };
        for i in 0..REQUESTS {
            if i % 250 == 0 {
                // El azar destruye cosas: se reponen cada cierto tiempo.
                writer.write_all(&ids.create_all(be)).await.unwrap();
            }
            // Cualquier petición del núcleo salvo KillClient (que cortaría
            // la conexión de la propia prueba); a veces, de extensiones.
            let opcode = match rng.below(40) {
                0 => 128 + rng.below(128) as u8,
                _ => loop {
                    let op = 1 + rng.below(119) as u8;
                    if op != 113 {
                        break op;
                    }
                },
            };
            let mut data = match rng.below(4) {
                0 => rng.next() as u8,
                _ => rng.below(34) as u8,
            };
            let mut body = Writer::new(be);
            // Tres de cada cuatro veces la petición empieza bien formada,
            // para llegar más allá de la primera comprobación.
            if rng.chance(75) {
                let fresh = base | (0x200 + rng.below(64));
                match opcode {
                    1 => {
                        let parent = ids.window(&mut rng);
                        body.u32(fresh).u32(parent);
                        body.u16(value(&mut rng) as u16).u16(value(&mut rng) as u16);
                        body.u16(value(&mut rng) as u16).u16(value(&mut rng) as u16);
                        body.u16(rng.below(4) as u16)
                            .u16(rng.below(3) as u16)
                            .u32(0);
                        body.u32(rng.below(1 << 15));
                        data = 0;
                    }
                    2 => {
                        body.u32(ids.window(&mut rng)).u32(rng.below(1 << 15));
                    }
                    12 => {
                        body.u32(ids.window(&mut rng))
                            .u16(rng.below(128) as u16)
                            .u16(0);
                    }
                    18 => {
                        let format = rng.pick(&[8, 16, 32]);
                        let units = rng.below(12);
                        body.u32(ids.window(&mut rng)).u32(1 + rng.below(70));
                        body.u32(1 + rng.below(70))
                            .u8(format as u8)
                            .zeros(3)
                            .u32(units);
                        body.zeros((units * format / 8) as usize);
                        data = rng.below(3) as u8;
                    }
                    45 => {
                        let name: &[u8] = [
                            &b"fixed"[..],
                            b"9x15bold",
                            b"-*-helvetica-bold-o-*-*-17-*-*-*-*-*-iso10646-1",
                            b"-adobe-symbol-*-*-*-*-*-120-*-*-*-*-*-*",
                            b"cursor",
                        ][rng.below(5) as usize];
                        body.u32(fresh).u16(name.len() as u16).u16(0).bytes(name);
                    }
                    46..=48 => {
                        body.u32(rng.pick(&[ids.font, ids.gc, fresh]));
                    }
                    53 => {
                        body.u32(fresh).u32(ROOT);
                        data = rng.pick(&[1, 24, 32, 8]) as u8;
                    }
                    55 => {
                        let (drawable, _) = ids.drawable_and_gc(&mut rng);
                        body.u32(fresh).u32(drawable).u32(rng.below(1 << 23));
                    }
                    56 => {
                        let (_, gc) = ids.drawable_and_gc(&mut rng);
                        body.u32(gc).u32(rng.below(1 << 23));
                    }
                    57 => {
                        body.u32(ids.gc)
                            .u32(rng.pick(&[ids.gc, ids.bitmap_gc, fresh]));
                        body.u32(rng.below(1 << 23));
                    }
                    58..=60 => {
                        let (_, gc) = ids.drawable_and_gc(&mut rng);
                        body.u32(rng.pick(&[gc, fresh]));
                    }
                    62 | 63 => {
                        let (src, gc) = ids.drawable_and_gc(&mut rng);
                        let (dst, _) = ids.drawable_and_gc(&mut rng);
                        body.u32(src).u32(dst).u32(gc);
                    }
                    72 => {
                        // PutImage con los datos que dice traer.
                        let (drawable, gc) = ids.drawable_and_gc(&mut rng);
                        let (w, h) = (1 + rng.below(40), 1 + rng.below(20));
                        let depth = if drawable == ids.bitmap { 1 } else { 24 };
                        data = rng.below(3) as u8;
                        let pad = if data == 2 { 0 } else { rng.below(8) };
                        body.u32(drawable).u32(gc).u16(w as u16).u16(h as u16);
                        body.u16(value(&mut rng) as u16).u16(value(&mut rng) as u16);
                        body.u8(pad as u8)
                            .u8(if data == 0 { 1 } else { depth })
                            .u16(0);
                        let row = (w + pad).div_ceil(32) * 4;
                        let bytes = match (data, depth) {
                            (0, _) | (2, 1) => row * h,
                            (1, _) => row * h * depth as u32,
                            _ => w * h * 4,
                        };
                        for _ in 0..bytes {
                            body.u8(rng.next() as u8);
                        }
                    }
                    73 => {
                        let (drawable, _) = ids.drawable_and_gc(&mut rng);
                        body.u32(drawable);
                        data = 1 + rng.below(2) as u8;
                    }
                    76 | 77 => {
                        let (drawable, gc) = ids.drawable_and_gc(&mut rng);
                        data = rng.below(40) as u8;
                        body.u32(drawable).u32(gc);
                        body.u16(value(&mut rng) as u16).u16(value(&mut rng) as u16);
                        for _ in 0..data as u32 * (opcode as u32 - 75) {
                            body.u8(rng.next() as u8);
                        }
                    }
                    61 | 64..=71 | 74 | 75 => {
                        let (drawable, gc) = ids.drawable_and_gc(&mut rng);
                        body.u32(drawable);
                        if opcode != 61 {
                            body.u32(gc);
                        }
                    }
                    78 => {
                        body.u32(fresh).u32(ROOT).u32(super::server::ROOT_VISUAL);
                    }
                    79..=92 => {
                        body.u32(super::server::DEFAULT_COLORMAP);
                    }
                    93 => {
                        body.u32(fresh)
                            .u32(ids.bitmap)
                            .u32(rng.pick(&[0, ids.bitmap]));
                    }
                    94 => {
                        body.u32(fresh).u32(ids.font).u32(ids.font);
                    }
                    3..=44 | 114 => {
                        body.u32(ids.window(&mut rng));
                    }
                    _ => {}
                }
            }
            let words = match rng.below(10) {
                0 => rng.below(200),
                _ => rng.below(14),
            };
            for _ in 0..words {
                if rng.below(3) == 0 {
                    body.u16(value(&mut rng) as u16).u16(value(&mut rng) as u16);
                } else {
                    body.u32(value(&mut rng));
                }
            }
            if rng.below(8) == 0 {
                let extra: Vec<u8> = (0..rng.below(64)).map(|_| rng.next() as u8).collect();
                body.bytes(&extra);
            }
            writer
                .write_all(&raw_request(be, opcode, data, body))
                .await
                .unwrap();
            if i % 5 == 0 {
                let _ = backend.input.send(input(&mut rng));
            }
            if i % 500 == 0 {
                server.resize(200 + rng.below(300), 150 + rng.below(200));
            }
        }
        // Una última petición reconocible para saber que ya lo procesó todo.
        let marker = "_FIN_DE_LA_PRUEBA";
        let mut b = Writer::new(be);
        b.u16(marker.len() as u16).u16(0).bytes(marker.as_bytes());
        writer.write_all(&raw_request(be, 16, 0, b)).await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(180);
        while lock(&server.inner.state).atoms.find(marker).is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "el servidor no terminó de procesar las peticiones"
            );
            assert_eq!(
                server.client_count(),
                1,
                "se expulsó al cliente de la prueba (semilla {seed:#x})"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let panics = server
            .inner
            .panics
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(panics, 0, "hubo pánicos (semilla {seed:#x})");
        assert_eq!(server.client_count(), 1);
        {
            let st = lock(&server.inner.state);
            assert!(st.budget.used() <= BUDGET, "{}", st.budget.used());
            assert!(st.windows.contains_key(&ROOT));
            assert!(st.property_bytes <= super::server::MAX_PROPERTY_BYTES);
        }
        // El servidor sigue atendiendo con normalidad.
        let mut fresh = TestClient::connect(&server, !be).await;
        fresh.sync().await;
        server.stop();
        drop(writer);
        let _ = drain.await;
    }
}
