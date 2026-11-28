//! Pruebas del modo de ventanas sueltas (sin raíz) con un cliente en
//! memoria: qué publica el servidor de cada ventana de primer nivel y cómo
//! atiende lo que le cuenta la interfaz.

use super::*;
use crate::xserver::{RootlessInput, RootlessRequest, RootlessWindow};

fn start_rootless(width: u32, height: u32) -> (XServer, DesktopBackend) {
    XServer::start(
        &tokio::runtime::Handle::current(),
        XServerConfig {
            width,
            height,
            rootless: true,
        },
        Arc::new(|| {}),
    )
}

/// Las ventanas que el servidor tiene publicadas ahora mismo.
fn published(server: &XServer) -> Vec<RootlessWindow> {
    server.client_count(); // el lote en curso ya acabó
    lock(&server.rootless().expect("modo sin raíz"))
        .windows
        .clone()
}

fn find(server: &XServer, id: u32) -> RootlessWindow {
    published(server)
        .into_iter()
        .find(|w| w.id == id)
        .unwrap_or_else(|| panic!("la ventana {id:#x} no está publicada"))
}

/// Píxel RGBA de la imagen de una ventana.
fn window_pixel(window: &RootlessWindow, x: u32, y: u32) -> [u8; 4] {
    let fb = lock(&window.frame);
    let i = ((y * fb.width() + x) * 4) as usize;
    [
        fb.pixels[i],
        fb.pixels[i + 1],
        fb.pixels[i + 2],
        fb.pixels[i + 3],
    ]
}

/// Peticiones pendientes para la interfaz (y las recoge).
fn requests(server: &XServer) -> Vec<RootlessRequest> {
    server.client_count();
    std::mem::take(&mut lock(&server.rootless().unwrap()).requests)
}

/// Espera a que el servidor haya aplicado todo lo enviado hasta ahora por
/// el canal de la interfaz: manda un tamaño de pantalla nuevo y espera a
/// verlo.
async fn settle(server: &XServer) {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let size = (1001 + n % 500, 701 + n % 300);
    server.rootless_send(RootlessInput::Screen {
        width: size.0,
        height: size.1,
    });
    let applied = || {
        let st = lock(&server.inner.state);
        (st.width, st.height) == size
    };
    for _ in 0..2000 {
        if applied() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("el servidor no aplicó la entrada");
}

#[tokio::test(flavor = "multi_thread")]
async fn rootless_publishes_each_window_and_follows_the_interface() {
    let (server, _backend) = start_rootless(1000, 700);
    let used = || lock(&server.inner.state).budget.used();
    let before = used();
    for be in [false, true] {
        let mut c = TestClient::connect(&server, be).await;
        let protocols = c.intern("WM_PROTOCOLS").await;
        let delete = c.intern("WM_DELETE_WINDOW").await;
        let active = c.intern("_NET_ACTIVE_WINDOW").await;
        let change_state = c.intern("WM_CHANGE_STATE").await;
        let net_state = c.intern("_NET_WM_STATE").await;
        let max_v = c.intern("_NET_WM_STATE_MAXIMIZED_VERT").await;
        let max_h = c.intern("_NET_WM_STATE_MAXIMIZED_HORZ").await;
        let moveresize = c.intern("_NET_WM_MOVERESIZE").await;
        let mask = M_STRUCTURE_NOTIFY
            | M_FOCUS_CHANGE
            | M_KEY_PRESS
            | M_BUTTON_PRESS
            | M_BUTTON_RELEASE
            | M_ENTER_WINDOW
            | M_POINTER_MOTION;
        let window = c
            .create_window(
                (60, 40, 120, 80),
                &[(ATTR_BACKGROUND, 0xffffff), (ATTR_EVENTS, mask)],
            )
            .await;
        c.change_property(window, A_WM_NAME, A_STRING, 8, b"Suelta")
            .await;
        let data = if be {
            delete.to_be_bytes()
        } else {
            delete.to_le_bytes()
        };
        c.change_property(window, protocols, A_ATOM, 32, &data)
            .await;
        // Un emergente (menú): sin marco, donde lo pone la aplicación.
        let popup = c
            .create_window(
                (300, 200, 50, 40),
                &[(ATTR_BACKGROUND, 0xff0000), (ATTR_OVERRIDE, 1)],
            )
            .await;
        c.map(window).await;
        c.map(popup).await;
        c.sync().await;

        // Cada ventana, con su sitio (el pedido: aquí no coloca el gestor
        // integrado ni hay barra de título), su título y su imagen.
        let w = find(&server, window);
        assert_eq!((w.x, w.y, w.width, w.height), (60, 40, 120, 80));
        assert_eq!(
            (w.title.as_str(), w.popup, w.decorated),
            ("Suelta", false, true)
        );
        assert_eq!(window_pixel(&w, 5, 5), [255, 255, 255, 255]);
        let p = find(&server, popup);
        assert_eq!(
            (p.x, p.y, p.width, p.height, p.popup),
            (300, 200, 50, 40, true)
        );
        assert_eq!(window_pixel(&p, 49, 39), [255, 0, 0, 255]);
        assert_eq!(published(&server).len(), 2);
        // Lo que se dibuja en la ventana llega a su imagen, y solo esa zona
        // queda marcada como cambiada.
        lock(&w.frame).take_dirty();
        let gc = c.create_gc(window, 0x0000ff).await;
        c.fill(window, gc, (10, 20, 30, 10)).await;
        c.sync().await;
        let w = find(&server, window);
        assert_eq!(window_pixel(&w, 15, 25), [0, 0, 255, 255]);
        assert_eq!(window_pixel(&w, 5, 5), [255, 255, 255, 255]);
        let dirty = lock(&w.frame).take_dirty().expect("zona cambiada");
        assert_eq!(
            (dirty.x, dirty.y, dirty.width, dirty.height),
            (10, 20, 30, 10)
        );

        // --- Ratón: en coordenadas de su ventana -----------------------------
        // Otra ventana en el mismo sitio de la pantalla X, y por encima.
        let other = c
            .create_window(
                (60, 40, 120, 80),
                &[(ATTR_BACKGROUND, 0x00ff00), (ATTR_EVENTS, mask)],
            )
            .await;
        c.map(other).await;
        c.sync().await;
        server.rootless_send(RootlessInput::Pointer {
            window,
            x: 10,
            y: 15,
        });
        let ev = c.event(ENTER_NOTIFY).await;
        assert_eq!(
            ev.u32_at(12),
            window,
            "entra en la suya, no en la de encima"
        );
        // Coordenadas de raíz y de ventana.
        assert_eq!(&ev.0[20..28], &[70, 0, 55, 0, 10, 0, 15, 0]);
        server.rootless_send(RootlessInput::Button {
            window,
            x: 12,
            y: 16,
            button: MouseButton::Left,
            pressed: true,
        });
        let ev = c.event(BUTTON_PRESS).await;
        assert_eq!((ev.0[1], ev.u32_at(12)), (1, window));
        assert_eq!(&ev.0[24..28], &[12, 0, 16, 0]);

        // La aplicación pinta su propia barra y pide que la muevan con el
        // botón que está pulsado: se le pide al sistema.
        c.wm_message(window, moveresize, [0, 0, 8, 1, 0]).await;
        c.sync().await;
        assert_eq!(requests(&server), [RootlessRequest::Move(window)]);
        // El sistema se queda el ratón: el botón se da por soltado.
        server.rootless_send(RootlessInput::DragStarted);
        settle(&server).await;
        assert_eq!(lock(&server.inner.state).buttons, 0);

        // --- La ventana nativa se mueve y cambia de tamaño --------------------
        server.rootless_send(RootlessInput::Geometry {
            window,
            x: 200,
            y: 150,
            width: 300,
            height: 200,
        });
        let ev = loop {
            let ev = c.event(CONFIGURE_NOTIFY).await;
            if ev.u32_at(8) == window {
                break ev;
            }
        };
        let field = |at: usize| i16::from_le_bytes([ev.0[at], ev.0[at + 1]]);
        assert_eq!(
            (field(16), field(18), field(20), field(22)),
            (200, 150, 300, 200)
        );
        c.sync().await;
        let w = find(&server, window);
        assert_eq!((w.x, w.y, w.width, w.height), (200, 150, 300, 200));
        let size = {
            let fb = lock(&w.frame);
            (fb.width(), fb.height())
        };
        assert_eq!(size, (300, 200));
        // Un emergente no se deja mover por la interfaz.
        server.rootless_send(RootlessInput::Geometry {
            window: popup,
            x: 1,
            y: 1,
            width: 9,
            height: 9,
        });
        settle(&server).await;
        assert_eq!(find(&server, popup).x, 300);

        // --- Foco y teclado -------------------------------------------------------
        server.rootless_send(RootlessInput::Focused(other));
        loop {
            let ev = c.event(FOCUS_IN).await;
            if ev.u32_at(4) == other {
                break;
            }
        }
        server.rootless_send(RootlessInput::Focused(window));
        loop {
            let ev = c.event(FOCUS_IN).await;
            if ev.u32_at(4) == window {
                break;
            }
        }
        settle(&server).await;
        assert!(find(&server, window).active && !find(&server, other).active);
        server.rootless_send(RootlessInput::Desktop(DesktopInput::Text("a".into())));
        let ev = c.event(KEY_PRESS).await;
        assert_eq!(ev.u32_at(12), window);
        server.rootless_send(RootlessInput::Unfocused);
        let ev = c.event(FOCUS_OUT).await;
        assert_eq!(ev.u32_at(4), window);
        settle(&server).await;
        assert!(!find(&server, window).active);
        // Con un menú abierto (el ratón capturado), irse a otra aplicación
        // le da a esta la pulsación fuera que espera para cerrarlo.
        server.rootless_send(RootlessInput::Focused(window));
        let mut b = c.body();
        b.u32(popup).u16(0x0c).u8(1).u8(1).u32(0).u32(0).u32(0);
        c.request(26, 1, b).await;
        assert_eq!(c.reply().await[1], 0, "captura concedida");
        server.rootless_send(RootlessInput::Unfocused);
        let ev = c.event(BUTTON_PRESS).await;
        // (La ventana del evento es la de la captura; el puntero, en la
        // raíz, fuera de todas.)
        assert_eq!((ev.0[1], ev.u32_at(12), ev.u32_at(16)), (1, popup, 0));
        let rx = i16::from_le_bytes([ev.0[20], ev.0[21]]) as i32;
        let ry = i16::from_le_bytes([ev.0[22], ev.0[23]]) as i32;
        assert!(
            published(&server).iter().all(|w| {
                rx < w.x || ry < w.y || rx >= w.x + w.width as i32 || ry >= w.y + w.height as i32
            }),
            "pulsación fuera de las ventanas: {rx},{ry}"
        );
        c.event(BUTTON_RELEASE).await;
        let mut b = c.body();
        b.u32(0);
        c.request(27, 0, b).await;
        c.sync().await;

        // --- Lo que la aplicación pide al gestor de ventanas ----------------------
        c.wm_message(window, active, [1, 0, 0, 0, 0]).await;
        c.wm_message(window, change_state, [3, 0, 0, 0, 0]).await;
        c.wm_message(window, net_state, [1, max_v, max_h, 0, 0])
            .await;
        c.sync().await;
        assert_eq!(
            requests(&server),
            [
                RootlessRequest::Activate(window),
                RootlessRequest::Minimize(window)
            ]
        );
        // Minimizada o maximizada, para X sigue igual: lo hace el sistema.
        let w = find(&server, window);
        assert!(w.maximized && !w.fullscreen);
        assert_eq!((w.x, w.y, w.width, w.height), (200, 150, 300, 200));
        assert_eq!(c.cardinals(window, net_state).await, [max_v, max_h]);

        // --- Cerrar -----------------------------------------------------------------
        server.rootless_send(RootlessInput::Close(window));
        let ev = c.event(CLIENT_MESSAGE).await;
        assert_eq!(
            (ev.u32_at(4), ev.u32_at(8), ev.u32_at(12)),
            (window, protocols, delete)
        );
        // Al ocultarse deja de estar publicada.
        let mut b = c.body();
        b.u32(window);
        c.request(10, 0, b).await;
        c.sync().await;
        let mut left: Vec<u32> = published(&server).iter().map(|w| w.id).collect();
        left.sort_unstable();
        assert_eq!(left, [popup, other]);
        // Las imágenes cuentan en el presupuesto de memoria (además de los
        // búferes de las propias ventanas).
        assert!(used() - before >= 2 * (50 * 40 + 120 * 80));
        drop(c);
        for _ in 0..500 {
            if server.client_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(published(&server).is_empty());
        // Al irse el cliente se devuelve todo: solo queda la raíz (que ha
        // ido cambiando de tamaño con la pantalla).
        let screen = {
            let st = lock(&server.inner.state);
            st.width as u64 * st.height as u64
        };
        assert_eq!(used(), screen);
    }
    // En el modo normal no hay nada de esto.
    let (plain, _backend) = start(320, 200);
    assert!(plain.rootless().is_none());
    plain.rootless_send(RootlessInput::Unfocused);
    plain.stop();
    server.stop();
}

/// Un pánico contenido no saca al servidor del modo de ventanas sueltas, ni
/// aunque llegue con el estado de ese modo apartado (a media publicación).
#[tokio::test(flavor = "multi_thread")]
async fn rootless_survives_a_contained_panic() {
    const GREEN: [u8; 4] = [0, 0xff, 0, 0xff];
    let (server, _backend) = start_rootless(640, 480);
    let mut victim = TestClient::connect(&server, false).await;
    let mut other = TestClient::connect(&server, true).await;
    let mine = other
        .create_window((100, 10, 50, 50), &[(ATTR_BACKGROUND, 0x00ff00)])
        .await;
    other.map(mine).await;
    other.sync().await;
    assert_eq!(window_pixel(&find(&server, mine), 5, 5), GREEN);
    let doomed = victim
        .create_window((10, 10, 40, 40), &[(ATTR_BACKGROUND, 0xff0000)])
        .await;
    victim.map(doomed).await;
    victim.sync().await;
    find(&server, doomed);

    {
        let mut st = lock(&server.inner.state);
        st.rootless = None;
        // Gancho de pruebas: la petición «Bell» entra en pánico.
        st.panic_on_opcode = Some(104);
    }
    let b = victim.body();
    victim.request(104, 0, b).await;
    while victim.packet().await.is_some() {}
    lock(&server.inner.state).panic_on_opcode = None;
    other.sync().await;
    let contained = server
        .inner
        .panics
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(contained, 1);

    // La ventana del otro sigue publicada, con su imagen (rehecha); la de
    // la víctima se fue con ella.
    let left: Vec<u32> = published(&server).iter().map(|w| w.id).collect();
    assert_eq!(left, [mine]);
    assert_eq!(window_pixel(&find(&server, mine), 5, 5), GREEN);
    // Y se sigue atendiendo a la interfaz.
    server.rootless_send(RootlessInput::Geometry {
        window: mine,
        x: 300,
        y: 200,
        width: 80,
        height: 60,
    });
    settle(&server).await;
    let w = find(&server, mine);
    assert_eq!((w.x, w.y, w.width, w.height), (300, 200, 80, 60));
    assert_eq!(window_pixel(&w, 70, 50), GREEN);
    server.stop();
}
