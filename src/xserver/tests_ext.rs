//! Pruebas de las extensiones (RENDER, SHAPE y RANDR) con un cliente en
//! memoria, como las de `tests.rs`.

use super::*;

const ARGB32: u32 = 0x20;
const RGB24: u32 = 0x21;
const A8: u32 = 0x22;
const OP_SRC: u8 = 1;
const OP_OVER: u8 = 3;

impl TestClient {
    pub(super) async fn extension(&mut self, name: &str) -> (u8, u8, u8, u8) {
        let mut b = self.body();
        b.u16(name.len() as u16).u16(0).bytes(name.as_bytes());
        self.request(98, 0, b).await;
        let r = self.reply().await;
        (r[8], r[9], r[10], r[11])
    }

    pub(super) async fn picture(&mut self, drawable: u32, format: u32) -> u32 {
        let id = self.id();
        let mut b = self.body();
        b.u32(id).u32(drawable).u32(format).u32(0);
        self.request(RENDER, 4, b).await;
        id
    }

    /// `Picture` de un color (16 bits por canal, ya premultiplicado).
    async fn solid(&mut self, (r, g, b, a): (u16, u16, u16, u16)) -> u32 {
        let id = self.id();
        let mut body = self.body();
        body.u32(id).u16(r).u16(g).u16(b).u16(a);
        self.request(RENDER, 33, body).await;
        id
    }

    #[allow(clippy::too_many_arguments)]
    async fn composite(
        &mut self,
        op: u8,
        src: u32,
        mask: u32,
        dst: u32,
        src_at: (i16, i16),
        dst_at: (i16, i16),
        size: (u16, u16),
    ) {
        let mut b = self.body();
        b.u8(op).zeros(3).u32(src).u32(mask).u32(dst);
        b.i16(src_at.0).i16(src_at.1).i16(0).i16(0);
        b.i16(dst_at.0).i16(dst_at.1).u16(size.0).u16(size.1);
        self.request(RENDER, 8, b).await;
    }

    pub(super) async fn fill_rectangle(
        &mut self,
        op: u8,
        dst: u32,
        (r, g, b, a): (u16, u16, u16, u16),
        (x, y, w, h): (i16, i16, u16, u16),
    ) {
        let mut body = self.body();
        body.u8(op).zeros(3).u32(dst).u16(r).u16(g).u16(b).u16(a);
        body.i16(x).i16(y).u16(w).u16(h);
        self.request(RENDER, 26, body).await;
    }

    /// Sube una imagen de un byte por píxel a un pixmap de profundidad 8.
    async fn put_a8(&mut self, pixmap: u32, w: u16, h: u16, data: &[u8]) {
        let gc = self.create_gc(pixmap, 0).await;
        let mut b = self.body();
        b.u32(pixmap).u32(gc).u16(w).u16(h).i16(0).i16(0);
        b.u8(0).u8(8).u16(0);
        for row in data.chunks(w as usize) {
            b.bytes(row).pad();
        }
        self.request(72, 2, b).await;
    }
}

fn fixed(v: f64) -> u32 {
    (v * 65536.0) as i32 as u32
}

#[tokio::test(flavor = "multi_thread")]
async fn render_composites_with_alpha_masks_glyphs_and_gradients() {
    for be in [false, true] {
        let (server, backend) = start(300, 200);
        let mut c = TestClient::connect(&server, be).await;
        let (present, opcode, _, first_error) = c.extension("RENDER").await;
        assert_eq!((present, opcode, first_error), (1, RENDER, 142));
        // Versión 0.10 y los cuatro formatos de siempre.
        let mut b = c.body();
        b.u32(0).u32(11);
        c.request(RENDER, 0, b).await;
        let r = c.reply().await;
        assert_eq!((c.u32(&r[8..12]), c.u32(&r[12..16])), (0, 10));
        let b = c.body();
        c.request(RENDER, 1, b).await;
        let r = c.reply().await;
        assert_eq!(c.u32(&r[8..12]), 4);
        let formats: Vec<(u32, u8)> = (0..4)
            .map(|i| (c.u32(&r[32 + 28 * i..36 + 28 * i]), r[32 + 28 * i + 5]))
            .collect();
        assert_eq!(formats, [(ARGB32, 32), (RGB24, 24), (A8, 8), (0x23, 1)]);

        // Una ventana blanca sin gestor, en (50, 40).
        let win = c
            .create_window(
                (50, 40, 100, 80),
                &[(ATTR_BACKGROUND, 0xffffff), (ATTR_OVERRIDE, 1)],
            )
            .await;
        c.map(win).await;
        let dst = c.picture(win, RGB24).await;
        let at = |x: u32, y: u32| pixel(&server, &backend, 50 + x, 40 + y);

        // Relleno opaco y relleno al 50 % (negro sobre blanco: gris).
        c.fill_rectangle(OP_SRC, dst, (0xffff, 0, 0, 0xffff), (0, 0, 20, 20))
            .await;
        c.fill_rectangle(OP_OVER, dst, (0, 0, 0, 0x8000), (20, 0, 10, 10))
            .await;
        c.sync().await;
        assert_eq!(at(5, 5), [0xff, 0, 0]);
        assert_eq!(at(25, 5), [0x7f, 0x7f, 0x7f]);
        assert_eq!(at(35, 5), [0xff, 0xff, 0xff]);

        // Azul a través de una máscara de cobertura 0, 128 y 255.
        let blue = c.solid((0, 0, 0xffff, 0xffff)).await;
        let mask_pixmap = c.create_pixmap(8, 3, 1).await;
        c.put_a8(mask_pixmap, 3, 1, &[0, 128, 255]).await;
        let mask = c.picture(mask_pixmap, A8).await;
        c.composite(OP_OVER, blue, mask, dst, (0, 0), (40, 0), (3, 1))
            .await;
        c.sync().await;
        assert_eq!(at(40, 0), [0xff, 0xff, 0xff]);
        assert_eq!(at(41, 0), [0x7f, 0x7f, 0xff]);
        assert_eq!(at(42, 0), [0, 0, 0xff]);

        // El pixmap de la máscara se puede liberar: el `Picture` lo conserva,
        // aunque su identificador se reutilice para otra cosa.
        let mut b = c.body();
        b.u32(mask_pixmap);
        c.request(54, 0, b).await;
        let mut b = c.body();
        b.u32(mask_pixmap).u32(ROOT).u16(3).u16(1);
        c.request(53, 24, b).await;
        c.composite(OP_OVER, blue, mask, dst, (0, 0), (40, 2), (3, 1))
            .await;
        c.sync().await;
        assert_eq!(at(41, 2), [0x7f, 0x7f, 0xff]);
        assert_eq!(lock(&server.inner.state).zombie_pixmaps.len(), 1);
        let mut b = c.body();
        b.u32(mask);
        c.request(RENDER, 7, b).await;
        c.sync().await;
        assert!(lock(&server.inner.state).zombie_pixmaps.is_empty());

        // Texto: dos glifos de 2×2 (uno entero y otro a medias) en negro.
        let glyphs = c.id();
        let mut b = c.body();
        b.u32(glyphs).u32(A8);
        c.request(RENDER, 17, b).await;
        let mut b = c.body();
        b.u32(glyphs).u32(2).u32(65).u32(66);
        for _ in 0..2 {
            // Tamaño 2×2, origen (0, 2) (el glifo sube desde la línea base)
            // y avance de 3.
            b.u16(2).u16(2).i16(0).i16(2).i16(3).i16(0);
        }
        b.bytes(&[255, 255, 0, 0, 255, 255, 0, 0]);
        b.bytes(&[128, 0, 0, 0, 0, 128, 0, 0]);
        c.request(RENDER, 20, b).await;
        let black = c.solid((0, 0, 0, 0xffff)).await;
        let mut b = c.body();
        b.u8(OP_OVER)
            .zeros(3)
            .u32(black)
            .u32(dst)
            .u32(A8)
            .u32(glyphs);
        b.i16(0).i16(0);
        b.u8(2).zeros(3).i16(10).i16(32).bytes(&[65, 66]).pad();
        c.request(RENDER, 23, b).await;
        c.sync().await;
        assert_eq!(at(10, 30), [0, 0, 0]);
        assert_eq!(at(11, 31), [0, 0, 0]);
        assert_eq!(at(12, 30), [0xff, 0xff, 0xff]);
        assert_eq!(at(13, 30), [0x7f, 0x7f, 0x7f]);
        assert_eq!(at(14, 30), [0xff, 0xff, 0xff]);
        assert_eq!(at(14, 31), [0x7f, 0x7f, 0x7f]);

        // Degradado de rojo a azul, de x = 0 a x = 20.
        let gradient = c.id();
        let mut b = c.body();
        b.u32(gradient)
            .u32(fixed(0.0))
            .u32(0)
            .u32(fixed(20.0))
            .u32(0);
        b.u32(2).u32(fixed(0.0)).u32(fixed(1.0));
        b.u16(0xffff).u16(0).u16(0).u16(0xffff);
        b.u16(0).u16(0).u16(0xffff).u16(0xffff);
        c.request(RENDER, 34, b).await;
        c.composite(OP_SRC, gradient, 0, dst, (0, 0), (60, 0), (20, 4))
            .await;
        c.sync().await;
        let (left, middle, right) = (at(60, 1), at(70, 1), at(79, 1));
        assert!(left[0] > 0xf0 && left[2] < 0x10, "{left:?}");
        assert!(right[2] > 0xf0 && right[0] < 0x10, "{right:?}");
        assert!(
            (0x70..0x90).contains(&middle[0]) && middle[1] == 0,
            "{middle:?}"
        );

        // Un trapecio verde con el lado izquierdo a media columna.
        let green = c.solid((0, 0xffff, 0, 0xffff)).await;
        let mut b = c.body();
        b.u8(OP_OVER)
            .zeros(3)
            .u32(green)
            .u32(dst)
            .u32(A8)
            .i16(0)
            .i16(0);
        b.u32(fixed(50.0)).u32(fixed(60.0));
        b.u32(fixed(10.5))
            .u32(fixed(50.0))
            .u32(fixed(10.5))
            .u32(fixed(60.0));
        b.u32(fixed(30.0))
            .u32(fixed(50.0))
            .u32(fixed(30.0))
            .u32(fixed(60.0));
        c.request(RENDER, 10, b).await;
        c.sync().await;
        assert_eq!(at(20, 55), [0, 0xff, 0]);
        assert_eq!(at(10, 55), [0x7f, 0xff, 0x7f]);
        assert_eq!(at(9, 55), [0xff, 0xff, 0xff]);
        assert_eq!(at(30, 55), [0xff, 0xff, 0xff]);
        assert_eq!(at(20, 49), [0xff, 0xff, 0xff]);
        assert_eq!(at(20, 60), [0xff, 0xff, 0xff]);

        // Una imagen de 2×1 ampliada al doble con una transformación, y
        // recortada a los tres primeros píxeles del destino.
        let tiny = c.create_pixmap(32, 2, 1).await;
        let tiny_gc = c.create_gc(tiny, 0).await;
        let mut b = c.body();
        b.u32(tiny).u32(tiny_gc).u16(2).u16(1).i16(0).i16(0);
        b.u8(0).u8(32).u16(0);
        b.bytes(&[0, 0, 0xff, 0xff, 0, 0xff, 0, 0xff]);
        c.request(72, 2, b).await;
        let src = c.picture(tiny, ARGB32).await;
        let mut b = c.body();
        b.u32(src);
        for v in [0.5, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] {
            b.u32(fixed(v));
        }
        c.request(RENDER, 28, b).await;
        let mut b = c.body();
        b.u32(dst).i16(0).i16(0).i16(0).i16(70).u16(3).u16(1);
        c.request(RENDER, 6, b).await;
        c.composite(OP_SRC, src, 0, dst, (0, 0), (0, 70), (4, 1))
            .await;
        c.sync().await;
        assert_eq!(at(0, 70), [0xff, 0, 0]);
        assert_eq!(at(1, 70), [0xff, 0, 0]);
        assert_eq!(at(2, 70), [0, 0xff, 0]);
        assert_eq!(at(3, 70), [0xff, 0xff, 0xff]);

        // Errores propios de la extensión, con su código de operación.
        c.composite(OP_OVER, 0x1234, 0, dst, (0, 0), (0, 0), (1, 1))
            .await;
        assert_eq!(c.error().await, (143, 0x1234, c.seq, RENDER));
        c.composite(99, blue, 0, dst, (0, 0), (0, 0), (1, 1)).await;
        assert_eq!(c.error().await, (144, 99, c.seq, RENDER));
        // Formato que no casa con la profundidad de la ventana.
        let wrong = c.id();
        let mut b = c.body();
        b.u32(wrong).u32(win).u32(A8).u32(0);
        c.request(RENDER, 4, b).await;
        assert_eq!(c.error().await.0, BAD_MATCH);
        // Glifos que dicen traer más datos de los que traen.
        let mut b = c.body();
        b.u32(glyphs).u32(1).u32(70);
        b.u16(2000).u16(2000).i16(0).i16(0).i16(0).i16(0);
        c.request(RENDER, 20, b).await;
        assert_eq!(c.error().await.0, BAD_LENGTH);
        let mut b = c.body();
        b.u32(glyphs).u32(0x00ff_ffff);
        c.request(RENDER, 20, b).await;
        assert_eq!(c.error().await.0, BAD_LENGTH);
        // Un degradado con más paradas de las que caben en la petición.
        let mut b = c.body();
        b.u32(c.base | 0x999).zeros(16).u32(4000);
        c.request(RENDER, 34, b).await;
        assert_eq!(c.error().await.0, BAD_LENGTH);
        for _ in 0..2 {
            let mut b = c.body();
            b.u32(glyphs);
            c.request(RENDER, 19, b).await;
        }
        assert_eq!(c.error().await, (145, glyphs, c.seq, RENDER));

        // Al irse el cliente no queda nada suyo.
        {
            let st = lock(&server.inner.state);
            assert!(st.pictures.len() >= 5 && st.glyph_bytes == 0);
        }
        drop(c);
        let deadline = std::time::Instant::now() + WAIT;
        while server.client_count() != 0 {
            assert!(std::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let st = lock(&server.inner.state);
        assert!(st.pictures.is_empty() && st.glyphsets.is_empty());
        assert!(st.zombie_pixmaps.is_empty() && st.pixmaps.is_empty());
        drop(st);
        server.stop();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn render_memory_is_bounded() {
    let (server, _backend) = start(300, 200);
    let mut c = TestClient::connect(&server, false).await;
    // Los glifos tienen un tope de tamaño…
    let glyphs = c.id();
    let mut b = c.body();
    b.u32(glyphs).u32(A8);
    c.request(RENDER, 17, b).await;
    let mut b = c.body();
    b.u32(glyphs).u32(1).u32(1);
    b.u16(4000).u16(1).i16(0).i16(0).i16(0).i16(0);
    b.zeros(4000);
    c.request(RENDER, 20, b).await;
    assert_eq!(c.error().await.0, BAD_ALLOC);
    // …y entre todos, un presupuesto: al llegar a él, `Alloc`.
    let glyph_bytes = |server: &XServer| lock(&server.inner.state).glyph_bytes;
    let mut sent = 0u32;
    let refused = loop {
        let mut b = c.body();
        b.u32(glyphs).u32(1).u32(sent);
        b.u16(1024).u16(1024).i16(0).i16(0).i16(0).i16(0);
        b.zeros(1024 * 1024);
        // Petición grande: hace falta BIG-REQUESTS.
        if sent == 0 {
            let enable = c.body();
            c.request(133, 0, enable).await;
            c.reply().await;
        }
        b.pad();
        let mut w = Writer::new(false);
        w.u8(RENDER).u8(20).u16(0).u32((b.buf.len() / 4 + 2) as u32);
        w.bytes(&b.buf);
        c.stream.write_all(&w.buf).await.unwrap();
        c.seq = c.seq.wrapping_add(1);
        let sync = c.body();
        c.request(43, 0, sync).await;
        let p = c.packet().await.unwrap();
        if p[0] == 0 {
            c.packet().await.unwrap();
            break p[1];
        }
        sent += 1;
        assert!(sent < 200, "sin tope: {} bytes", glyph_bytes(&server));
    };
    assert_eq!(refused, BAD_ALLOC);
    assert!(sent >= 8 && glyph_bytes(&server) <= 96 * 1024 * 1024);
    // Liberar el conjunto devuelve la memoria.
    let mut b = c.body();
    b.u32(glyphs);
    c.request(RENDER, 19, b).await;
    c.sync().await;
    assert_eq!(glyph_bytes(&server), 0);

    // Una composición de una ventana sobre sí misma necesita una copia: si
    // no cabe en el presupuesto de imagen, `Alloc` (y nada de pánico).
    let win = c
        .create_window((0, 0, 200, 150), &[(ATTR_OVERRIDE, 1)])
        .await;
    let pic = c.picture(win, RGB24).await;
    c.sync().await;
    lock(&server.inner.state).budget = Budget::new(1000);
    c.composite(OP_OVER, pic, 0, pic, (0, 0), (5, 5), (50, 50))
        .await;
    assert_eq!(c.error().await.0, BAD_ALLOC);
    lock(&server.inner.state).budget = Budget::new(u64::MAX / 2);
    c.composite(OP_OVER, pic, 0, pic, (0, 0), (5, 5), (50, 50))
        .await;
    c.sync().await;
    // Trapecios desmesurados (de −30000 a 30000 en las dos direcciones):
    // se recortan al destino…
    let solid = c.solid((0xffff, 0xffff, 0xffff, 0xffff)).await;
    let huge = |c: &TestClient, dst: u32, format: u32, count: usize| {
        let mut b = c.body();
        b.u8(OP_OVER).zeros(3).u32(solid).u32(dst).u32(format);
        b.i16(0).i16(0);
        for _ in 0..count {
            b.u32(fixed(-30000.0)).u32(fixed(30000.0));
            for x in [-30000.0, 30000.0] {
                b.u32(fixed(x)).u32(fixed(-30000.0));
                b.u32(fixed(x)).u32(fixed(30000.0));
            }
        }
        b.pad();
        let mut w = Writer::new(false);
        w.u8(RENDER).u8(10).u16(0).u32((b.buf.len() / 4 + 2) as u32);
        w.bytes(&b.buf);
        w.buf
    };
    let bytes = huge(&c, pic, 0, 200);
    c.stream.write_all(&bytes).await.unwrap();
    c.seq = c.seq.wrapping_add(1);
    c.sync().await;
    // …y si aun así el trabajo se dispara (miles de figuras sobre un
    // destino de miles de filas), la petición se rechaza.
    let tall = c.create_pixmap(32, 1, 16_000).await;
    let tall_pic = c.picture(tall, ARGB32).await;
    let bytes = huge(&c, tall_pic, A8, 1300);
    c.stream.write_all(&bytes).await.unwrap();
    c.seq = c.seq.wrapping_add(1);
    assert_eq!(c.error().await, (BAD_ALLOC, tall_pic, c.seq, RENDER));
    assert_eq!(
        server
            .inner
            .panics
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    server.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn shape_clips_what_is_seen_and_where_the_pointer_lands() {
    for be in [false, true] {
        let (server, backend) = start(300, 200);
        let mut c = TestClient::connect(&server, be).await;
        let (present, opcode, first_event, _) = c.extension("SHAPE").await;
        assert_eq!((present, opcode, first_event), (1, SHAPE, SHAPE_NOTIFY));
        let b = c.body();
        c.request(SHAPE, 0, b).await;
        let r = c.reply().await;
        assert_eq!((c.u16(&r[8..10]), c.u16(&r[10..12])), (1, 1));

        let events = M_BUTTON_PRESS | M_STRUCTURE_NOTIFY;
        let win = c
            .create_window(
                (50, 40, 100, 80),
                &[
                    (ATTR_BACKGROUND, 0xff0000),
                    (ATTR_OVERRIDE, 1),
                    (ATTR_EVENTS, events),
                ],
            )
            .await;
        // Una hija verde que la cubre entera: también queda recortada.
        let child = c.id();
        let mut b = c.body();
        b.u32(child).u32(win).i16(0).i16(40).u16(100).u16(40).u16(0);
        b.u16(1).u32(0).u32(1 << ATTR_BACKGROUND).u32(0x00ff00);
        c.request(1, 0, b).await;
        c.map(child).await;
        c.map(win).await;
        c.sync().await;
        let desktop = pixel(&server, &backend, 290, 190);
        assert_eq!(pixel(&server, &backend, 140, 50), [0xff, 0, 0]);

        // Avisos de cambio de forma.
        let mut b = c.body();
        b.u32(win).u8(1).zeros(3);
        c.request(SHAPE, 6, b).await;
        // Forma: dos rectángulos que se solapan (una «L» de 50 de ancho).
        let mut b = c.body();
        b.u8(0).u8(0).u8(0).u8(0).u32(win).i16(0).i16(0);
        b.i16(0).i16(0).u16(50).u16(80);
        b.i16(0).i16(60).u16(100).u16(20);
        c.request(SHAPE, 1, b).await;
        let ev = c.event(SHAPE_NOTIFY).await;
        assert_eq!((ev.0[1], ev.u32_at(4)), (0, win));
        // Extensión de la forma: (0, 0) 100×80; y está puesta.
        assert_eq!(&ev.0[8..16], &[0, 0, 0, 0, 100, 0, 80, 0]);
        assert_eq!(ev.0[20], 1);
        c.sync().await;
        assert_eq!(pixel(&server, &backend, 60, 50), [0xff, 0, 0]);
        assert_eq!(pixel(&server, &backend, 140, 50), desktop);
        assert_eq!(pixel(&server, &backend, 140, 90), desktop);
        assert_eq!(pixel(&server, &backend, 60, 90), [0, 0xff, 0]);
        assert_eq!(pixel(&server, &backend, 140, 110), [0, 0xff, 0]);

        // GetRectangles: en bandas, sin solapes.
        let mut b = c.body();
        b.u32(win).u8(0).zeros(3);
        c.request(SHAPE, 8, b).await;
        let r = c.reply().await;
        assert_eq!(c.u32(&r[8..12]), 2);
        let rect =
            |i: usize| -> Vec<u16> { (0..4).map(|k| c.u16(&r[32 + 8 * i + 2 * k..])).collect() };
        assert_eq!(rect(0), [0, 0, 50, 60]);
        assert_eq!(rect(1), [0, 60, 100, 20]);
        // QueryExtents.
        let mut b = c.body();
        b.u32(win);
        c.request(SHAPE, 5, b).await;
        let r = c.reply().await;
        assert_eq!(r[8], 1);
        assert_eq!((c.u16(&r[16..18]), c.u16(&r[18..20])), (100, 80));

        // El ratón, fuera de la forma, no toca la ventana.
        let click = |x: u16, y: u16| {
            for pressed in [true, false] {
                backend
                    .input
                    .send(DesktopInput::PointerButton {
                        button: MouseButton::Left,
                        pressed,
                        x,
                        y,
                    })
                    .unwrap();
            }
        };
        click(140, 50);
        click(60, 50);
        let ev = c.event(BUTTON_PRESS).await;
        // Coordenadas dentro de la ventana: (10, 10).
        assert_eq!(&ev.0[24..28], &[10, 0, 10, 0]);

        // Una máscara de 1 bit como forma: solo la columna de la derecha.
        let bitmap = c.create_pixmap(1, 100, 80).await;
        let clear = c.create_gc(bitmap, 0).await;
        let set = c.create_gc(bitmap, 1).await;
        c.fill(bitmap, clear, (0, 0, 100, 80)).await;
        c.fill(bitmap, set, (90, 0, 10, 80)).await;
        let mut b = c.body();
        b.u8(0).u8(0).u16(0).u32(win).i16(0).i16(0).u32(bitmap);
        c.request(SHAPE, 2, b).await;
        c.sync().await;
        assert_eq!(pixel(&server, &backend, 60, 50), desktop);
        assert_eq!(pixel(&server, &backend, 145, 50), [0xff, 0, 0]);
        // Y sin máscara vuelve a ser rectangular.
        let mut b = c.body();
        b.u8(0).u8(0).u16(0).u32(win).i16(0).i16(0).u32(0);
        c.request(SHAPE, 2, b).await;
        c.sync().await;
        assert_eq!(pixel(&server, &backend, 60, 50), [0xff, 0, 0]);
        assert!(lock(&server.inner.state).windows[&win].shape.is_none());

        // Errores: clase de forma desconocida, ventana que no existe.
        let mut b = c.body();
        b.u8(0).u8(9).u8(0).u8(0).u32(win).i16(0).i16(0);
        c.request(SHAPE, 1, b).await;
        assert_eq!(c.error().await, (BAD_VALUE, 9, c.seq, SHAPE));
        let mut b = c.body();
        b.u8(0).u8(0).u8(0).u8(0).u32(0x4444).i16(0).i16(0);
        c.request(SHAPE, 1, b).await;
        assert_eq!(c.error().await.0, BAD_WINDOW);
        server.stop();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn randr_describes_the_screen_and_announces_changes() {
    for be in [false, true] {
        let (server, _backend) = start(300, 200);
        let mut c = TestClient::connect(&server, be).await;
        let (present, opcode, first_event, first_error) = c.extension("RANDR").await;
        assert_eq!(
            (present, opcode, first_event, first_error),
            (1, RANDR, RR_SCREEN_CHANGE_NOTIFY, 147)
        );
        let mut b = c.body();
        b.u32(1).u32(6);
        c.request(RANDR, 0, b).await;
        let r = c.reply().await;
        assert_eq!((c.u32(&r[8..12]), c.u32(&r[12..16])), (1, 3));

        // Recursos: un CRTC, una salida y un modo del tamaño de la pantalla.
        let mut b = c.body();
        b.u32(ROOT);
        c.request(RANDR, 25, b).await;
        let r = c.reply().await;
        let counts: Vec<u16> = (0..4).map(|i| c.u16(&r[16 + 2 * i..])).collect();
        assert_eq!(counts, [1, 1, 1, 7]);
        let (crtc, output) = (c.u32(&r[32..36]), c.u32(&r[36..40]));
        let mode = &r[40..72];
        assert_eq!((c.u16(&mode[4..6]), c.u16(&mode[6..8])), (300, 200));
        assert_eq!(&r[72..79], b"300x200");
        assert_eq!(r.len(), 80);
        // La salida está conectada a ese CRTC…
        let mut b = c.body();
        b.u32(output).u32(0);
        c.request(RANDR, 9, b).await;
        let r = c.reply().await;
        assert_eq!((r[1], c.u32(&r[12..16]), r[24]), (0, crtc, 0));
        let name_len = c.u16(&r[34..36]) as usize;
        assert_eq!(&r[44..44 + name_len], b"GodRemote-0");
        // …que ocupa toda la pantalla.
        let mut b = c.body();
        b.u32(crtc).u32(0);
        c.request(RANDR, 20, b).await;
        let r = c.reply().await;
        let geometry: Vec<u16> = (0..4).map(|i| c.u16(&r[12 + 2 * i..])).collect();
        assert_eq!(geometry, [0, 0, 300, 200]);
        assert_eq!(c.u32(&r[32..36]), output);
        let mut b = c.body();
        b.u32(ROOT);
        c.request(RANDR, 31, b).await;
        let r = c.reply().await;
        assert_eq!(c.u32(&r[8..12]), output);

        // Avisos al cambiar el tamaño de la pantalla.
        let mut b = c.body();
        b.u32(ROOT).u16(1 | 2).u16(0);
        c.request(RANDR, 4, b).await;
        c.sync().await;
        server.resize(640, 400);
        let ev = c.event(RR_SCREEN_CHANGE_NOTIFY).await;
        assert_eq!((ev.u32_at(12), ev.u32_at(16)), (ROOT, ROOT));
        assert_eq!(&ev.0[24..28], &[0x80, 0x02, 0x90, 0x01]);
        let ev = c.event(RR_NOTIFY).await;
        assert_eq!((ev.0[1], ev.u32_at(12)), (0, crtc));
        assert_eq!(&ev.0[28..32], &[0x80, 0x02, 0x90, 0x01]);

        // Nada se puede configurar; lo desconocido no existe.
        let mut b = c.body();
        b.u32(ROOT).u16(100).u16(100).u32(30).u32(30);
        c.request(RANDR, 7, b).await;
        assert_eq!(c.error().await, (BAD_ACCESS, 0, c.seq, RANDR));
        let mut b = c.body();
        b.u32(0x5555).u32(0);
        c.request(RANDR, 9, b).await;
        assert_eq!(c.error().await, (147, 0x5555, c.seq, RANDR));
        let b = c.body();
        c.request(RANDR, 200, b).await;
        assert_eq!(c.error().await.0, BAD_REQUEST);
        server.stop();
    }
}
