//! Pruebas de los cursores y de minimizar, con un cliente en memoria como
//! las de `tests.rs`.

use super::*;
use crate::desktop::{CursorShape, DesktopCursor};

const ATTR_CURSOR: u32 = 14;
const ARGB32: u32 = 0x20;

impl TestClient {
    pub(super) async fn open_font(&mut self, name: &str) -> u32 {
        let id = self.id();
        let mut b = self.body();
        b.u32(id)
            .u16(name.len() as u16)
            .u16(0)
            .bytes(name.as_bytes());
        self.request(45, 0, b).await;
        id
    }

    /// Cursor de la fuente «cursor».
    pub(super) async fn glyph_cursor(&mut self, font: u32, glyph: u16) -> u32 {
        let id = self.id();
        let mut b = self.body();
        b.u32(id).u32(font).u32(font).u16(glyph).u16(glyph + 1);
        b.u16(0).u16(0).u16(0).u16(0xffff).u16(0xffff).u16(0xffff);
        self.request(94, 0, b).await;
        id
    }

    /// Cursor de dos mapas de bits, rojo sobre azul.
    async fn bitmap_cursor(&mut self, source: u32, mask: u32, hot: (u16, u16)) -> u32 {
        let id = self.id();
        let mut b = self.body();
        b.u32(id).u32(source).u32(mask);
        b.u16(0xffff).u16(0).u16(0).u16(0).u16(0).u16(0xffff);
        b.u16(hot.0).u16(hot.1);
        self.request(93, 0, b).await;
        id
    }

    async fn set_cursor(&mut self, window: u32, cursor: u32) {
        let mut b = self.body();
        b.u32(window).u32(1 << ATTR_CURSOR).u32(cursor);
        self.request(2, 0, b).await;
    }

    async fn free_cursor(&mut self, cursor: u32) {
        let mut b = self.body();
        b.u32(cursor);
        self.request(95, 0, b).await;
    }

    pub(super) async fn child_window(
        &mut self,
        parent: u32,
        (x, y, w, h): (i16, i16, u16, u16),
    ) -> u32 {
        let id = self.id();
        let mut b = self.body();
        b.u32(id).u32(parent).i16(x).i16(y).u16(w).u16(h).u16(0);
        b.u16(1).u32(0).u32(0);
        self.request(1, 0, b).await;
        id
    }
}

/// Siguiente cambio de cursor que el servidor avisa a la interfaz.
fn next_cursor(backend: &DesktopBackend) -> DesktopCursor {
    loop {
        match backend.events.recv_timeout(WAIT) {
            Ok(DesktopEvent::Cursor(cursor)) => return cursor,
            Ok(_) => {}
            Err(_) => panic!("no llegó ningún cambio de cursor"),
        }
    }
}

/// Mueve el puntero y espera a que el servidor lo haya procesado.
async fn move_to(server: &XServer, backend: &DesktopBackend, c: &mut TestClient, x: u16, y: u16) {
    backend
        .input
        .send(DesktopInput::PointerMove { x, y })
        .unwrap();
    // La entrada va por otra tarea: se espera a ver el puntero en su sitio.
    for _ in 0..500 {
        if lock(&server.inner.state).pointer == (x as i32, y as i32) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    c.sync().await;
}

fn shape(shape: CursorShape) -> DesktopCursor {
    DesktopCursor::Shape(shape)
}

#[tokio::test(flavor = "multi_thread")]
async fn windows_get_the_cursor_they_ask_for() {
    let (server, backend) = start(400, 300);
    for be in [false, true] {
        let mut c = TestClient::connect(&server, be).await;
        let font = c.open_font("cursor").await;
        let text = c.glyph_cursor(font, 152).await;
        let hand = c.glyph_cursor(font, 60).await;
        let window = c
            .create_window(
                (100, 50, 80, 60),
                &[(ATTR_OVERRIDE, 1), (ATTR_CURSOR, text)],
            )
            .await;
        // Una hija sin cursor hereda el de su madre; otra tiene el suyo.
        let plain = c.child_window(window, (0, 0, 20, 20)).await;
        let link = c.child_window(window, (40, 0, 20, 20)).await;
        c.set_cursor(link, hand).await;
        for w in [plain, link, window] {
            c.map(w).await;
        }
        c.sync().await;

        move_to(&server, &backend, &mut c, 150, 100).await;
        assert_eq!(next_cursor(&backend), shape(CursorShape::Text));
        move_to(&server, &backend, &mut c, 145, 55).await;
        assert_eq!(next_cursor(&backend), shape(CursorShape::Hand));
        move_to(&server, &backend, &mut c, 105, 55).await;
        assert_eq!(next_cursor(&backend), shape(CursorShape::Text));
        move_to(&server, &backend, &mut c, 10, 10).await;
        assert_eq!(next_cursor(&backend), shape(CursorShape::Default));

        // Liberado por el cliente, la ventana lo conserva (es lo que hace
        // Xlib: `XDefineCursor` y `XFreeCursor` seguidos).
        c.free_cursor(text).await;
        c.set_cursor(window, text).await;
        assert_eq!(c.error().await.0, BAD_CURSOR);
        move_to(&server, &backend, &mut c, 150, 100).await;
        assert_eq!(next_cursor(&backend), shape(CursorShape::Text));
        // Cambiarlo con el puntero encima se nota al momento.
        c.set_cursor(window, hand).await;
        c.sync().await;
        assert_eq!(next_cursor(&backend), shape(CursorShape::Hand));
        c.set_cursor(window, 0).await;
        c.sync().await;
        assert_eq!(next_cursor(&backend), shape(CursorShape::Default));

        // Con el puntero capturado manda el cursor de la captura.
        move_to(&server, &backend, &mut c, 10, 10).await;
        let mut b = c.body();
        b.u32(window).u16(0).u8(1).u8(1).u32(0).u32(hand).u32(0);
        c.request(26, 0, b).await;
        assert_eq!(c.reply().await[1], 0, "captura concedida");
        assert_eq!(next_cursor(&backend), shape(CursorShape::Hand));
        let mut b = c.body();
        b.u32(0);
        c.request(27, 0, b).await;
        c.sync().await;
        assert_eq!(next_cursor(&backend), shape(CursorShape::Default));
        drop(c);
        // Al irse el cliente no queda nada suyo.
        for _ in 0..500 {
            if server.client_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(lock(&server.inner.state).cursors.is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn image_cursors_keep_their_pixels_and_their_budget() {
    const BUDGET: u64 = 2_000_000;
    let (server, backend) = start(400, 300);
    lock(&server.inner.state).budget = Budget::new(BUDGET);
    let used = || lock(&server.inner.state).budget.used();
    let before = used();
    let mut c = TestClient::connect(&server, false).await;
    let window = c
        .create_window((100, 50, 80, 60), &[(ATTR_OVERRIDE, 1)])
        .await;
    c.map(window).await;

    // Dos mapas de bits de 8×8: la máscara cubre las 4 primeras columnas y
    // la figura, las 2 primeras.
    let source = c.create_pixmap(1, 8, 8).await;
    let mask = c.create_pixmap(1, 8, 8).await;
    let (zero, one) = (c.create_gc(source, 0).await, c.create_gc(source, 1).await);
    for pixmap in [source, mask] {
        c.fill(pixmap, zero, (0, 0, 8, 8)).await;
    }
    c.fill(mask, one, (0, 0, 4, 8)).await;
    c.fill(source, one, (0, 0, 2, 8)).await;
    let cursor = c.bitmap_cursor(source, mask, (1, 2)).await;
    c.set_cursor(window, cursor).await;
    move_to(&server, &backend, &mut c, 150, 100).await;
    let DesktopCursor::Image(frames) = next_cursor(&backend) else {
        panic!("se esperaba un cursor de imagen");
    };
    assert_eq!(frames.len(), 1);
    let image = &frames[0].image;
    assert_eq!((image.width, image.height), (8, 8));
    assert_eq!((image.hot_x, image.hot_y), (1, 2));
    let at = |x: usize, y: usize| &image.rgba[(y * 8 + x) * 4..][..4];
    assert_eq!(at(0, 0), [255, 0, 0, 255], "figura: color de frente");
    assert_eq!(at(3, 7), [0, 0, 255, 255], "máscara sin figura: fondo");
    assert_eq!(at(5, 3), [0, 0, 0, 0], "fuera de la máscara: nada");

    // Un cursor en blanco esconde el puntero.
    c.fill(mask, zero, (0, 0, 8, 8)).await;
    let blank = c.bitmap_cursor(source, mask, (0, 0)).await;
    c.set_cursor(window, blank).await;
    c.sync().await;
    assert_eq!(next_cursor(&backend), shape(CursorShape::Hidden));

    // El punto caliente tiene que caer dentro (o justo en el borde, que es
    // lo que admite X.Org y lo que usa GTK), y las máscaras, casar.
    let edge = c.bitmap_cursor(source, source, (8, 8)).await;
    c.free_cursor(edge).await;
    let mut b = c.body();
    b.u32(c.base | 0x900).u32(source).u32(mask).zeros(12);
    b.u16(9).u16(0);
    c.request(93, 0, b).await;
    assert_eq!(c.error().await.0, BAD_MATCH);
    let deep = c.create_pixmap(24, 8, 8).await;
    let mut b = c.body();
    b.u32(c.base | 0x900).u32(deep).u32(0).zeros(16);
    c.request(93, 0, b).await;
    assert_eq!(c.error().await.0, BAD_MATCH);

    // RENDER: la imagen de un Picture ARGB, medio transparente.
    let pixmap = c.create_pixmap(32, 4, 4).await;
    let picture = c.picture(pixmap, ARGB32).await;
    c.fill_rectangle(1, picture, (0x8000, 0, 0x4000, 0x8000), (0, 0, 4, 4))
        .await;
    c.fill_rectangle(1, picture, (0, 0, 0, 0), (2, 0, 2, 4))
        .await;
    let render_cursor = |id: u32, hot: (u16, u16)| {
        let mut b = Writer::new(false);
        b.u32(id).u32(picture).u16(hot.0).u16(hot.1);
        b
    };
    let (soft, soft2) = (c.base | 0x901, c.base | 0x902);
    let b = render_cursor(soft, (3, 1));
    c.request(RENDER, 27, b).await;
    let b = render_cursor(soft2, (0, 0));
    c.request(RENDER, 27, b).await;
    c.set_cursor(window, soft).await;
    c.sync().await;
    let DesktopCursor::Image(frames) = next_cursor(&backend) else {
        panic!("se esperaba un cursor de imagen");
    };
    let image = &frames[0].image;
    assert_eq!((image.width, image.height), (4, 4));
    assert_eq!((image.hot_x, image.hot_y), (3, 1));
    assert_eq!(&image.rgba[..4], [0x80, 0, 0x40, 0x80]);
    assert_eq!(&image.rgba[8..12], [0, 0, 0, 0]);

    // Animado: los fotogramas de los dos, con su duración.
    let anim = c.base | 0x903;
    let mut b = c.body();
    b.u32(anim).u32(soft).u32(100).u32(soft2).u32(250);
    c.request(RENDER, 31, b).await;
    c.set_cursor(window, anim).await;
    c.sync().await;
    let DesktopCursor::Image(frames) = next_cursor(&backend) else {
        panic!("se esperaba un cursor animado");
    };
    let delays: Vec<u32> = frames.iter().map(|f| f.delay_ms).collect();
    assert_eq!(delays, [100, 250]);
    assert!(std::sync::Arc::ptr_eq(&frames[0].image, image));
    // Un cursor animado con un cursor que no existe no se crea.
    let mut b = c.body();
    b.u32(c.base | 0x904).u32(0x7777).u32(10);
    c.request(RENDER, 31, b).await;
    assert_eq!(c.error().await.0, BAD_CURSOR);

    // Las imágenes cuentan en el presupuesto de memoria… (8×8 la primera
    // y 4×4 cada una de las de RENDER; la animada no añade nada)
    let pixmaps = 8 * 8 * 3 + 4 * 4;
    let window_pixels = 80 * 60;
    assert_eq!(used() - before, window_pixels + pixmaps + 64 + 16 + 16);
    // …y un cursor enorme se recorta; cuando ya no caben más se siguen
    // creando (un error haría cerrarse a GTK o a Qt), pero sin imagen.
    let huge = c.create_pixmap(1, 1000, 1000).await;
    c.fill(huge, one, (0, 0, 1000, 1000)).await;
    for i in 0..40u32 {
        let mut b = c.body();
        b.u32(c.base | (0x1000 + i)).u32(huge).u32(0).zeros(16);
        c.request(93, 0, b).await;
    }
    c.sync().await;
    let side = crate::xserver::cursor::MAX_CURSOR_SIDE as u64;
    let room = (BUDGET - 1_000_000) / (side * side);
    let (images, plain) = {
        let st = lock(&server.inner.state);
        let huge = |i: &u32| st.cursors.get(&(c.base | (0x1000 + i)));
        let images = (0..40u32)
            .filter(
                |i| matches!(huge(i), Some((_, d)) if matches!(d.cursor, DesktopCursor::Image(_))),
            )
            .count() as u64;
        let plain = (0..40u32)
            .filter(|i| matches!(huge(i), Some((_, d)) if d.cursor == shape(CursorShape::Default)))
            .count() as u64;
        (images, plain)
    };
    assert!(images > 0 && images <= room, "{images} con imagen");
    assert_eq!(images + plain, 40);
    assert!(used() <= BUDGET);

    // Al irse el cliente se devuelve todo, también lo del cursor que la
    // interfaz aún tiene puesto.
    drop(c);
    for _ in 0..1000 {
        if server.client_count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(used(), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn window_frames_show_resize_cursors() {
    let (server, backend) = start(400, 300);
    let mut c = TestClient::connect(&server, false).await;
    let window = c
        .create_window(
            (100, 80, 120, 80),
            &[
                (ATTR_BACKGROUND, 0xffffff),
                (ATTR_EVENTS, M_STRUCTURE_NOTIFY),
            ],
        )
        .await;
    c.map(window).await;
    c.event(MAP_NOTIFY).await;
    c.sync().await;
    let (x, y, w, h) = {
        let st = lock(&server.inner.state);
        let win = &st.windows[&window];
        (win.x as u16, win.y as u16, win.w as u16, win.h as u16)
    };
    // Borde derecho, esquina de abajo a la derecha, borde de abajo…
    move_to(&server, &backend, &mut c, x + w + 1, y + h / 2).await;
    assert_eq!(next_cursor(&backend), shape(CursorShape::ResizeE));
    move_to(&server, &backend, &mut c, x + w + 1, y + h + 1).await;
    assert_eq!(next_cursor(&backend), shape(CursorShape::ResizeSe));
    move_to(&server, &backend, &mut c, x + w / 2, y + h + 1).await;
    assert_eq!(next_cursor(&backend), shape(CursorShape::ResizeS));
    move_to(&server, &backend, &mut c, x - 2, y + h / 2).await;
    assert_eq!(next_cursor(&backend), shape(CursorShape::ResizeW));
    // …la barra de título, con la flecha normal, y su filo de arriba.
    move_to(&server, &backend, &mut c, x + w / 2, y - 10).await;
    assert_eq!(next_cursor(&backend), shape(CursorShape::Default));
    move_to(&server, &backend, &mut c, x + w / 2, y - TITLE_H as u16).await;
    assert_eq!(next_cursor(&backend), shape(CursorShape::ResizeN));
    // Mientras se arrastra la barra, el de mover.
    move_to(&server, &backend, &mut c, x + 30, y - 10).await;
    assert_eq!(next_cursor(&backend), shape(CursorShape::Default));
    backend
        .input
        .send(DesktopInput::PointerButton {
            button: MouseButton::Left,
            pressed: true,
            x: x + 30,
            y: y - 10,
        })
        .unwrap();
    assert_eq!(next_cursor(&backend), shape(CursorShape::Move));
    backend
        .input
        .send(DesktopInput::PointerButton {
            button: MouseButton::Left,
            pressed: false,
            x: x + 30,
            y: y - 10,
        })
        .unwrap();
    assert_eq!(next_cursor(&backend), shape(CursorShape::Default));
}

// --- Minimizar ----------------------------------------------------------------

impl TestClient {
    /// Mensaje al gestor de ventanas: `ClientMessage` de 32 bits a la raíz.
    pub(super) async fn wm_message(&mut self, window: u32, kind: u32, data: [u32; 5]) {
        let mut b = self.body();
        b.u32(ROOT)
            .u32(M_SUBSTRUCTURE_REDIRECT | M_SUBSTRUCTURE_NOTIFY);
        b.u8(CLIENT_MESSAGE).u8(32).u16(0).u32(window).u32(kind);
        for value in data {
            b.u32(value);
        }
        self.request(25, 0, b).await;
    }

    /// Valores de 32 bits de una propiedad (vacío si no existe).
    pub(super) async fn cardinals(&mut self, window: u32, name: u32) -> Vec<u32> {
        let (_, format, data) = self.get_property(window, name).await;
        if format != 32 {
            return vec![];
        }
        data.chunks_exact(4).map(|c| self.u32(c)).collect()
    }
}

/// Pulsa y suelta el botón izquierdo en un punto.
fn click(backend: &DesktopBackend, x: i32, y: i32) {
    for pressed in [true, false] {
        backend
            .input
            .send(DesktopInput::PointerButton {
                button: MouseButton::Left,
                pressed,
                x: x as u16,
                y: y as u16,
            })
            .unwrap();
    }
}

/// Geometría `(x, y, ancho, alto)` de una ventana y si está mapeada.
fn geometry(server: &XServer, window: u32) -> (i32, i32, i32, i32, bool) {
    let st = lock(&server.inner.state);
    let w = &st.windows[&window];
    (w.x, w.y, w.w as i32, w.h as i32, w.mapped)
}

#[tokio::test(flavor = "multi_thread")]
async fn windows_can_be_minimized_and_brought_back() {
    use crate::xserver::wm::STRIP_H;
    let (server, backend) = start(400, 300);
    for be in [false, true] {
        let mut c = TestClient::connect(&server, be).await;
        let wm_state = c.intern("WM_STATE").await;
        let net_state = c.intern("_NET_WM_STATE").await;
        let hidden = c.intern("_NET_WM_STATE_HIDDEN").await;
        let max_v = c.intern("_NET_WM_STATE_MAXIMIZED_VERT").await;
        let max_h = c.intern("_NET_WM_STATE_MAXIMIZED_HORZ").await;
        let change_state = c.intern("WM_CHANGE_STATE").await;
        let active = c.intern("_NET_ACTIVE_WINDOW").await;
        let client_list = c.intern("_NET_CLIENT_LIST").await;
        let workarea = c.intern("_NET_WORKAREA").await;
        let supported = c.intern("_NET_SUPPORTED").await;
        assert!(c.cardinals(ROOT, supported).await.contains(&hidden));

        let mask = M_STRUCTURE_NOTIFY | M_FOCUS_CHANGE;
        let mut windows = vec![];
        for (i, name) in ["Primera", "Segunda"].into_iter().enumerate() {
            let window = c
                .create_window(
                    (40 + 150 * i as i16, 60, 120, 80),
                    &[(ATTR_BACKGROUND, 0xffffff), (ATTR_EVENTS, mask)],
                )
                .await;
            c.change_property(window, A_WM_NAME, A_STRING, 8, name.as_bytes())
                .await;
            c.map(window).await;
            c.event(MAP_NOTIFY).await;
            windows.push(window);
        }
        let (first, second) = (windows[0], windows[1]);
        // (El puntero, lejos de todo, y sin avisos de cursor atrasados.)
        move_to(&server, &backend, &mut c, 390, 5).await;
        while backend.events.try_recv().is_ok() {}
        let desktop = pixel(&server, &backend, 390, 290);
        let chip = (10, 300 - STRIP_H as u32 + 10);
        assert_eq!(pixel(&server, &backend, chip.0, chip.1), desktop);

        // El botón de minimizar de la barra (el tercero desde la derecha).
        let (x, y, w, _, _) = geometry(&server, second);
        click(&backend, x + w + FRAME - 3 * 20 + 8, y - TITLE_H / 2);
        let ev = c.event(UNMAP_NOTIFY).await;
        assert_eq!(ev.u32_at(8), second);
        // El foco pasa a la otra ventana.
        loop {
            let ev = c.event(FOCUS_IN).await;
            if ev.u32_at(4) == first {
                break;
            }
        }
        c.sync().await;
        assert!(!geometry(&server, second).4);
        // Para la aplicación está «icónica» (ICCCM) y «oculta» (EWMH), y
        // sigue en la lista de ventanas del gestor.
        assert_eq!(c.cardinals(second, wm_state).await, [3, 0]);
        assert_eq!(c.cardinals(second, net_state).await, [hidden]);
        assert_eq!(c.cardinals(ROOT, client_list).await, [first, second]);
        assert_eq!(
            c.cardinals(ROOT, workarea).await,
            [0, 0, 400, 300 - STRIP_H as u32]
        );
        // En el pie de la pantalla queda su ficha, con el título.
        assert_ne!(pixel(&server, &backend, chip.0, chip.1), desktop);
        let mut colors = std::collections::HashSet::new();
        for dx in 20..120 {
            colors.insert(pixel(&server, &backend, 4 + dx, chip.1));
        }
        assert!(colors.len() > 2, "ficha sin título: {colors:?}");
        // Sobre la ficha el puntero es una mano; un clic la devuelve.
        move_to(&server, &backend, &mut c, chip.0 as u16, chip.1 as u16).await;
        assert_eq!(next_cursor(&backend), shape(CursorShape::Hand));
        click(&backend, chip.0 as i32, chip.1 as i32);
        let ev = c.event(MAP_NOTIFY).await;
        assert_eq!(ev.u32_at(8), second);
        loop {
            let ev = c.event(FOCUS_IN).await;
            if ev.u32_at(4) == second {
                break;
            }
        }
        c.sync().await;
        assert_eq!(geometry(&server, second), (x, y, w, 80, true));
        assert_eq!(c.cardinals(second, wm_state).await, [1, 0]);
        assert_eq!(c.cardinals(second, net_state).await, [0u32; 0]);
        assert_eq!(c.cardinals(ROOT, workarea).await, [0, 0, 400, 300]);
        assert_eq!(pixel(&server, &backend, chip.0, chip.1), desktop);

        // Lo pide la aplicación: `XIconifyWindow` (WM_CHANGE_STATE) y, para
        // volver, mostrarse otra vez.
        c.wm_message(first, change_state, [3, 0, 0, 0, 0]).await;
        let ev = c.event(UNMAP_NOTIFY).await;
        assert_eq!(ev.u32_at(8), first);
        c.sync().await;
        assert_eq!(c.cardinals(first, wm_state).await, [3, 0]);
        c.map(first).await;
        let ev = c.event(MAP_NOTIFY).await;
        assert_eq!(ev.u32_at(8), first);
        c.sync().await;
        assert_eq!(c.cardinals(first, wm_state).await, [1, 0]);
        assert!(lock(&server.inner.state).minimized.is_empty());

        // Por EWMH: `_NET_WM_STATE_HIDDEN` y `_NET_ACTIVE_WINDOW`. Con una
        // ventana minimizada, la maximizada deja sitio a la tira.
        c.wm_message(second, net_state, [1, max_v, max_h, 0, 0])
            .await;
        c.sync().await;
        let full = geometry(&server, second);
        assert_eq!((full.2, full.3), (400 - 2 * FRAME, 300 - TITLE_H - FRAME));
        c.wm_message(first, net_state, [1, hidden, 0, 0, 0]).await;
        c.event(UNMAP_NOTIFY).await;
        c.sync().await;
        let shrunk = geometry(&server, second);
        assert_eq!(shrunk.3, full.3 - STRIP_H);
        assert_eq!(c.cardinals(first, net_state).await, [hidden]);
        c.wm_message(first, active, [1, 0, 0, 0, 0]).await;
        let ev = c.event(MAP_NOTIFY).await;
        assert_eq!(ev.u32_at(8), first);
        c.sync().await;
        assert_eq!(geometry(&server, second).3, full.3);
        assert_eq!(c.cardinals(first, net_state).await, [0u32; 0]);

        // Retirar (`UnmapWindow`) una ventana minimizada la da de baja.
        c.wm_message(first, net_state, [2, hidden, 0, 0, 0]).await;
        c.event(UNMAP_NOTIFY).await;
        let mut b = c.body();
        b.u32(first);
        c.request(10, 0, b).await;
        c.sync().await;
        assert_eq!(c.cardinals(first, wm_state).await, [0u32; 0]);
        assert_eq!(c.cardinals(ROOT, client_list).await, [second]);
        assert!(lock(&server.inner.state).minimized.is_empty());

        // Y si su cliente se va, su ficha desaparece.
        c.wm_message(second, change_state, [3, 0, 0, 0, 0]).await;
        c.event(UNMAP_NOTIFY).await;
        c.sync().await;
        assert_ne!(pixel(&server, &backend, chip.0, chip.1), desktop);
        drop(c);
        for _ in 0..500 {
            if server.client_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(pixel(&server, &backend, chip.0, chip.1), desktop);
        assert!(lock(&server.inner.state).minimized.is_empty());
    }
}
