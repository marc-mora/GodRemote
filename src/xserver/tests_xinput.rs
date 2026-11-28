//! Pruebas de XKEYBOARD y de XInput2 con un cliente en memoria, como las
//! de `tests.rs`: en los dos órdenes de bytes y recorriendo cada respuesta
//! campo a campo, como hacen las librerías de los clientes.

use super::*;
use crate::desktop::{CursorShape, DesktopCursor, KeyCode};
use crate::xserver::xinput::XI_OPCODE as XI;
use crate::xserver::xkb::{XKB_FIRST_ERROR, XKB_OPCODE as XKB};

/// Código de los eventos genéricos (los de XInput2).
const GENERIC: u8 = 35;
const XI_FIRST_ERROR: u8 = crate::xserver::xinput::XI_FIRST_ERROR;

// Tipos de evento de XInput2.
const XI_KEY_PRESS: u16 = 2;
const XI_KEY_RELEASE: u16 = 3;
const XI_BUTTON_PRESS: u16 = 4;
const XI_BUTTON_RELEASE: u16 = 5;
const XI_MOTION: u16 = 6;
const XI_ENTER: u16 = 7;
const XI_LEAVE: u16 = 8;
const XI_FOCUS_IN: u16 = 9;

/// Lo que interesa de un evento de dispositivo de XInput2.
#[derive(Debug, PartialEq, Eq)]
struct XiEvent {
    evtype: u16,
    device: u16,
    source: u16,
    detail: u32,
    window: u32,
    child: u32,
    root: (i32, i32),
    at: (i32, i32),
    /// Máscara de botones (primer byte).
    buttons: u8,
    /// Modificadores efectivos.
    mods: u32,
    flags: u32,
    /// Valores de los ejes que trae (parte entera).
    axes: Vec<i32>,
}

impl TestClient {
    /// Petición de XKB para el teclado principal: `rest` va tras el
    /// identificador de dispositivo.
    async fn xkb(&mut self, minor: u8, rest: impl FnOnce(&mut Writer)) {
        let mut b = self.body();
        b.u16(0x100);
        rest(&mut b);
        self.request(XKB, minor, b).await;
    }

    async fn atom_name(&mut self, atom: u32) -> String {
        let mut b = self.body();
        b.u32(atom);
        self.request(17, 0, b).await;
        let r = self.reply().await;
        let len = self.u16(&r[8..10]) as usize;
        String::from_utf8_lossy(&r[32..32 + len]).into_owned()
    }

    /// Selecciona eventos de XInput2 en una ventana para un dispositivo.
    async fn xi_select(&mut self, window: u32, device: u16, evtypes: &[u16]) {
        let mask = evtypes.iter().fold(0u32, |m, t| m | 1 << t);
        let mut b = self.body();
        b.u32(window).u16(1).u16(0).u16(device).u16(1);
        b.bytes(&mask.to_le_bytes());
        self.request(XI, 46, b).await;
    }

    /// Siguiente evento de XInput2 del tipo dado (se salta todo lo demás
    /// que no sea un error o una respuesta).
    async fn xi_packet(&mut self, evtype: u16) -> Vec<u8> {
        loop {
            let p = self.packet().await.expect("conexión cerrada");
            assert!(
                p[0] > 1,
                "esperando el evento XI2 {evtype} llegó {:?}",
                &p[..12]
            );
            if p[0] == GENERIC && p[1] == XI && self.u16(&p[8..10]) == evtype {
                assert_eq!(self.u16(&p[2..4]), self.seq, "secuencia del evento");
                return p;
            }
        }
    }

    /// Evento de tecla, botón o movimiento de XInput2, ya desmenuzado.
    async fn xi_event(&mut self, evtype: u16) -> XiEvent {
        let p = self.xi_packet(evtype).await;
        let fp = |b: &[u8]| (self.u32(b) as i32) >> 16;
        let (buttons_len, axes_len) = (
            self.u16(&p[48..50]) as usize * 4,
            self.u16(&p[50..52]) as usize * 4,
        );
        let axis_mask = &p[80 + buttons_len..80 + buttons_len + axes_len];
        let count: u32 = axis_mask.iter().map(|b| b.count_ones()).sum();
        let values = 80 + buttons_len + axes_len;
        assert_eq!(p.len(), values + 8 * count as usize, "longitud del evento");
        XiEvent {
            evtype,
            device: self.u16(&p[10..12]),
            source: self.u16(&p[52..54]),
            detail: self.u32(&p[16..20]),
            window: self.u32(&p[24..28]),
            child: self.u32(&p[28..32]),
            root: (fp(&p[32..36]), fp(&p[36..40])),
            at: (fp(&p[40..44]), fp(&p[44..48])),
            buttons: p[80],
            mods: self.u32(&p[72..76]),
            flags: self.u32(&p[56..60]),
            axes: (0..count as usize)
                .map(|i| self.u32(&p[values + 8 * i..]) as i32)
                .collect(),
        }
    }

    /// Lee paquetes hasta la respuesta a la última petición y devuelve los
    /// eventos que llegaron antes.
    async fn events_until_reply(&mut self) -> Vec<Vec<u8>> {
        let mut events = vec![];
        loop {
            let p = self.packet().await.expect("conexión cerrada");
            match p[0] {
                1 => return events,
                0 => panic!("error X {} (valor {:#x})", p[1], self.u32(&p[4..8])),
                _ => events.push(p),
            }
        }
    }
}

/// Espera a que el servidor haya aplicado toda la entrada enviada hasta
/// ahora: manda un movimiento a un punto nuevo y espera a verlo.
async fn settle(server: &XServer, backend: &DesktopBackend) {
    static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (x, y) = (200 + n % 100, 150 + (n / 100) % 40);
    backend
        .input
        .send(DesktopInput::PointerMove { x, y })
        .unwrap();
    for _ in 0..2000 {
        if lock(&server.inner.state).pointer == (x as i32, y as i32) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("el servidor no aplicó la entrada");
}

fn key(backend: &DesktopBackend, key: KeyCode, pressed: bool) {
    backend
        .input
        .send(DesktopInput::Key { key, pressed })
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn xkb_describes_the_keyboard_and_follows_its_state() {
    let (server, backend) = start(320, 200);
    let (shift, letter_a, enye) = {
        let st = lock(&server.inner.state);
        let code = |ks: u32| st.keymap.keycode(ks).unwrap();
        (code(0xFFE1), code(b'a' as u32), code(0xF1))
    };
    for be in [false, true] {
        let mut c = TestClient::connect(&server, be).await;
        let (present, opcode, first_event, first_error) = c.extension("XKEYBOARD").await;
        assert_eq!(
            (present, opcode, first_event, first_error),
            (1, XKB, XKB_EVENT, XKB_FIRST_ERROR)
        );
        // UseExtension: versión 1.0.
        let mut b = c.body();
        b.u16(1).u16(0);
        c.request(XKB, 0, b).await;
        let r = c.reply().await;
        assert_eq!((r[1], c.u16(&r[8..10]), c.u16(&r[10..12])), (1, 1, 0));
        // GetDeviceInfo dice cuál es el teclado (así lo busca libxkbcommon).
        c.xkb(24, |b| {
            b.zeros(10);
        })
        .await;
        let r = c.reply().await;
        assert_eq!((r[1], r.len()), (3, 36));
        // Con otro dispositivo, BadKeyboard.
        let mut b = c.body();
        b.u16(7).zeros(2);
        c.request(XKB, 4, b).await;
        assert_eq!(c.error().await.0, XKB_FIRST_ERROR);

        // --- GetMap completo: se recorre como lo hace xcb -------------------
        c.xkb(8, |b| {
            b.u16(0xff).u16(0).zeros(18);
        })
        .await;
        let r = c.reply().await;
        assert_eq!((r[1], r[10], r[11], c.u16(&r[12..14])), (3, 8, 255, 0xff));
        let (n_types, first_sym, total_syms, n_syms) = (
            r[15] as usize,
            r[17],
            c.u16(&r[18..20]) as usize,
            r[20] as usize,
        );
        assert_eq!((r[14], n_types, r[16]), (0, 4, 4), "tipos");
        assert_eq!((first_sym, n_syms), (8, 248), "teclas con símbolos");
        let (first_action, total_actions, n_actions) =
            (r[21], c.u16(&r[22..24]) as usize, r[24] as usize);
        assert_eq!((first_action, n_actions), (8, 248), "teclas con acciones");
        assert_eq!(&r[25..31], [0; 6], "ni comportamientos ni explícitos");
        let (n_modmap, total_modmap) = (r[32], r[33] as usize);
        assert_eq!(&r[34..37], [0; 3], "sin mapa de modificadores virtuales");
        let vmods = c.u16(&r[38..40]);
        assert_eq!(vmods, 0b111);
        let mut at = 40;
        // Tipos: 8 bytes y 8 por entrada.
        let mut levels = vec![];
        for _ in 0..n_types {
            levels.push(r[at + 4]);
            assert_eq!(r[at + 6], 0, "sin «preserve»");
            at += 8 + 8 * r[at + 5] as usize;
        }
        assert_eq!(levels, [1, 2, 2, 2]);
        // Símbolos: 8 bytes y 4 por símbolo.
        let mut seen = 0;
        let mut keys = vec![];
        for _ in 0..n_syms {
            let (kind, groups, width) = (r[at], r[at + 4], r[at + 5]);
            let n = c.u16(&r[at + 6..at + 8]) as usize;
            assert_eq!(n, width as usize * groups as usize);
            assert!((kind as usize) < n_types && width <= levels[kind as usize]);
            let syms: Vec<u32> = (0..n).map(|i| c.u32(&r[at + 8 + 4 * i..])).collect();
            keys.push((kind, syms));
            seen += n;
            at += 8 + 4 * n;
        }
        assert_eq!(seen, total_syms);
        assert_eq!(keys[0], (0, vec![]), "el código 8 no tiene nada");
        assert_eq!(keys[(shift - 8) as usize], (0, vec![0xFFE1]));
        assert_eq!(keys[(letter_a - 8) as usize], (1, vec![0x61, 0x41]));
        assert_eq!(keys[(enye - 8) as usize], (0, vec![0xF1]));
        // Acciones: cuántas tiene cada tecla y, después, las acciones.
        let counts = &r[at..at + n_actions];
        assert_eq!(
            counts.iter().map(|n| *n as usize).sum::<usize>(),
            total_actions
        );
        assert_eq!(total_actions, 4, "Mayús, Control, Alt y Super");
        assert_eq!(counts[(shift - 8) as usize], 1);
        at += n_actions.div_ceil(4) * 4;
        assert_eq!(&r[at..at + 4], [1, 0, 1, 1], "SetMods(Shift)");
        at += 8 * total_actions;
        // Modificadores virtuales: Alt, Meta y Super.
        assert_eq!(&r[at..at + 4], [0x08, 0x08, 0x40, 0]);
        at += 4;
        // Mapa de modificadores: (tecla, máscara).
        assert_eq!(total_modmap, 4);
        assert_eq!(&r[at..at + 2], [shift, 1]);
        assert!(n_modmap >= 4);
        at += (2 * total_modmap).div_ceil(4) * 4;
        assert_eq!(at, r.len(), "la respuesta mide lo que dice su contenido");

        // Solo los símbolos de un tramo de teclas (así lo pide Xlib cuando
        // se le avisa de un cambio).
        c.xkb(8, |b| {
            b.u16(0).u16(2).zeros(2).u8(letter_a).u8(2).zeros(14);
        })
        .await;
        let r = c.reply().await;
        assert_eq!((c.u16(&r[12..14]), r[17], r[20]), (2, letter_a, 2));
        assert_eq!(c.u32(&r[48..52]), 0x61);

        // --- GetNames ---------------------------------------------------------
        c.xkb(17, |b| {
            b.u16(0).u32(0x3fff);
        })
        .await;
        let r = c.reply().await;
        let which = c.u32(&r[8..12]);
        assert_eq!(which, 0x1af5, "nombres que se sirven");
        assert_eq!((r[12], r[13], r[14], r[15]), (8, 255, 4, 1));
        assert_eq!((c.u16(&r[16..18]), r[18], r[19]), (0b111, 8, 248));
        assert_eq!(c.u16(&r[26..28]), 7, "niveles con nombre");
        // Cuatro nombres del mapa, cuatro de tipos, niveles por tipo (con
        // relleno), siete de niveles, tres de modificadores y uno de grupo.
        let mut at = 32;
        let keycodes = c.u32(&r[at..]);
        at += 16;
        let types: Vec<u32> = (0..4).map(|i| c.u32(&r[at + 4 * i..])).collect();
        at += 16;
        assert_eq!(&r[at..at + 4], [1, 2, 2, 2]);
        at += 4 + 7 * 4;
        let virtual_mods: Vec<u32> = (0..3).map(|i| c.u32(&r[at + 4 * i..])).collect();
        at += 12;
        let group = c.u32(&r[at..]);
        at += 4;
        let key_names = r[at..].to_vec();
        assert_eq!(key_names.len(), 248 * 4, "un nombre por tecla");
        assert_eq!(&key_names[(shift as usize - 8) * 4..][..4], b"LFSH");
        assert_eq!(&key_names[..4], [0; 4]);
        assert_eq!(c.atom_name(keycodes).await, "godremote");
        assert_eq!(c.atom_name(types[2]).await, "ALPHABETIC");
        assert_eq!(c.atom_name(virtual_mods[2]).await, "Super");
        assert_eq!(c.atom_name(group).await, "GodRemote");
        // Solo lo que GTK pide: grupos y modificadores virtuales.
        c.xkb(17, |b| {
            b.u16(0).u32(0x1800);
        })
        .await;
        let r = c.reply().await;
        assert_eq!((c.u32(&r[8..12]), r.len()), (0x1800, 32 + 16));

        // --- Lo demás que piden las librerías al arrancar ---------------------
        c.xkb(6, |_| {}).await;
        let r = c.reply().await;
        assert_eq!((r.len(), r[9]), (92, 1), "controles: un grupo");
        assert_eq!(&r[60..92], [0xff; 32], "todas las teclas repiten");
        c.xkb(10, |b| {
            b.u8(0).u8(1).u16(0).u16(0);
        })
        .await;
        let r = c.reply().await;
        let (first, n, total) = (c.u16(&r[10..12]), c.u16(&r[12..14]), c.u16(&r[14..16]));
        assert_eq!((r[8], first, n, total), (0, 0, 4, 4));
        assert_eq!(r.len(), 32 + 4 * 16);
        assert_eq!((c.u32(&r[32..36]), &r[40..44]), (0xFFE1, &[1, 0, 1, 1][..]));
        // (Con los grupos, como lo pide Xlib.)
        c.xkb(10, |b| {
            b.u8(0x0f).u8(1).u16(0).u16(0);
        })
        .await;
        assert_eq!(c.reply().await.len(), 32 + 4 * 16 + 4 * 4);
        c.xkb(13, |b| {
            b.u16(0).u32(u32::MAX);
        })
        .await;
        let r = c.reply().await;
        assert_eq!((r.len(), c.u32(&r[8..12]), r[16]), (32, 0, 0));
        c.xkb(21, |b| {
            b.u16(0).u32(1).u32(1).zeros(12);
        })
        .await;
        let r = c.reply().await;
        assert_eq!((c.u32(&r[8..12]), c.u32(&r[12..16])), (1, 1));
        // GetKbdByName no existe.
        c.xkb(23, |_| {}).await;
        assert_eq!(c.error().await.0, BAD_IMPLEMENTATION);

        // --- Estado y avisos ----------------------------------------------------
        c.xkb(4, |b| {
            b.zeros(2);
        })
        .await;
        let r = c.reply().await;
        assert_eq!((r[1], r[8], r[9]), (3, 0, 0), "sin modificadores");
        // Sin seleccionar nada no llega ningún evento de XKB.
        key(&backend, KeyCode::Shift, true);
        key(&backend, KeyCode::Shift, false);
        settle(&server, &backend).await;
        let b = c.body();
        c.request(43, 0, b).await;
        assert!(c.events_until_reply().await.is_empty());
        // SelectEvents: mapa (todo) y estado (los detalles que pide GTK).
        c.xkb(1, |b| {
            b.u16(0b110).u16(0).u16(0b010).u16(0xff).u16(0xff);
            b.u16(0x3fff).u16(0x11);
        })
        .await;
        c.sync().await;
        key(&backend, KeyCode::Shift, true);
        let ev = c.event(XKB_EVENT).await;
        assert_eq!(ev.0[1], 2, "StateNotify");
        // Dispositivo, modificadores efectivos y base; tecla y tipo.
        assert_eq!(&ev.0[8..11], [3, 1, 1]);
        assert_eq!((ev.0[28], ev.0[29]), (shift, KEY_PRESS));
        c.xkb(4, |b| {
            b.zeros(2);
        })
        .await;
        let r = c.reply().await;
        assert_eq!((r[8], r[9]), (1, 1), "Mayús pulsada");
        key(&backend, KeyCode::Shift, false);
        let ev = c.event(XKB_EVENT).await;
        assert_eq!((ev.0[1], ev.0[9], ev.0[29]), (2, 0, KEY_RELEASE));
        // Un carácter sin tecla propia cambia el mapa: MapNotify con la
        // tecla que le ha tocado, y luego el aviso básico.
        let fresh = if be { '₧' } else { '€' };
        backend
            .input
            .send(DesktopInput::Text(fresh.to_string()))
            .unwrap();
        let ev = c.event(XKB_EVENT).await;
        assert_eq!(ev.0[1], 1, "MapNotify");
        let changed = u16::from_le_bytes([ev.0[10], ev.0[11]]);
        let (first, count) = (ev.0[16], ev.0[17]);
        assert_eq!(
            (changed, count),
            (2, 1),
            "cambian los símbolos de una tecla"
        );
        let ev = c.event(MAPPING_NOTIFY).await;
        assert_eq!((ev.0[4], ev.0[5], ev.0[6]), (1, first, 1));
        c.xkb(8, |b| {
            b.u16(0).u16(2).zeros(2).u8(first).u8(1).zeros(14);
        })
        .await;
        let r = c.reply().await;
        assert_eq!(c.u32(&r[48..52]), 0x0100_0000 | fresh as u32);
        // Al dejar de seleccionarlos, se acabaron.
        c.xkb(1, |b| {
            b.u16(0b110).u16(0b110).u16(0).u16(0).u16(0);
        })
        .await;
        c.sync().await;
        key(&backend, KeyCode::Shift, true);
        key(&backend, KeyCode::Shift, false);
        settle(&server, &backend).await;
        let b = c.body();
        c.request(43, 0, b).await;
        let late = c.events_until_reply().await;
        assert!(late.iter().all(|p| p[0] != XKB_EVENT), "{late:?}");
    }
    server.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn xinput2_lists_devices_and_delivers_generic_events() {
    let (server, backend) = start(400, 300);
    let letter_a = lock(&server.inner.state)
        .keymap
        .keycode(b'a' as u32)
        .unwrap();
    for be in [false, true] {
        let mut c = TestClient::connect(&server, be).await;
        let (present, opcode, _, first_error) = c.extension("XInputExtension").await;
        assert_eq!((present, opcode, first_error), (1, XI, XI_FIRST_ERROR));
        assert_eq!(c.extension("Generic Event Extension").await.0, 1);
        // XIQueryVersion: se pida la que se pida, la 2.0.
        let mut b = c.body();
        b.u16(2).u16(4);
        c.request(XI, 47, b).await;
        let r = c.reply().await;
        assert_eq!((c.u16(&r[8..10]), c.u16(&r[10..12])), (2, 0));
        let mut b = c.body();
        b.u16(1).u16(5);
        c.request(XI, 47, b).await;
        assert_eq!(c.error().await.0, BAD_VALUE);
        // GetExtensionVersion (lo primero que pregunta libXi).
        let mut b = c.body();
        b.u16(15).u16(0).bytes(b"XInputExtension");
        c.request(XI, 1, b).await;
        let r = c.reply().await;
        assert_eq!((c.u16(&r[8..10]), c.u16(&r[10..12]), r[12]), (2, 0, 1));

        // --- XIQueryDevice: los cuatro dispositivos con sus clases ----------
        let mut b = c.body();
        b.u16(0).u16(0);
        c.request(XI, 48, b).await;
        let r = c.reply().await;
        assert_eq!(c.u16(&r[8..10]), 4);
        let mut at = 32;
        let mut devices = vec![];
        for _ in 0..4 {
            let (id, usage, attached) = (c.u16(&r[at..]), c.u16(&r[at + 2..]), c.u16(&r[at + 4..]));
            let (classes, name_len) = (c.u16(&r[at + 6..]), c.u16(&r[at + 8..]) as usize);
            assert_eq!(r[at + 10], 1, "activado");
            let name = String::from_utf8_lossy(&r[at + 12..at + 12 + name_len]).into_owned();
            at += 12 + name_len.div_ceil(4) * 4;
            let mut kinds = vec![];
            for _ in 0..classes {
                let (kind, units) = (c.u16(&r[at..]), c.u16(&r[at + 2..]) as usize);
                match kind {
                    // Teclas: tantas como códigos.
                    0 => assert_eq!((units, c.u16(&r[at + 6..])), (2 + 248, 248)),
                    // Botones: máscara de estado y una etiqueta por botón.
                    1 => assert_eq!((units, c.u16(&r[at + 6..])), (2 + 1 + 7, 7)),
                    // Ejes: 44 bytes.
                    _ => assert_eq!((kind, units), (2, 11)),
                }
                kinds.push(kind);
                at += units * 4;
            }
            devices.push((id, usage, attached, name, kinds));
        }
        assert_eq!(at, r.len(), "la respuesta mide lo que dice su contenido");
        let pointer = vec![1, 2, 2];
        assert_eq!(
            devices,
            [
                (2, 1, 3, "Virtual core pointer".to_string(), pointer.clone()),
                (3, 2, 2, "Virtual core keyboard".to_string(), vec![0]),
                (4, 3, 2, "GodRemote pointer".to_string(), pointer),
                (5, 4, 3, "GodRemote keyboard".to_string(), vec![0]),
            ]
        );
        // Solo los maestros; y uno que no existe.
        let mut b = c.body();
        b.u16(1).u16(0);
        c.request(XI, 48, b).await;
        let r = c.reply().await;
        assert_eq!(c.u16(&r[8..10]), 2);
        let mut b = c.body();
        b.u16(9).u16(0);
        c.request(XI, 48, b).await;
        assert_eq!(c.error().await.0, XI_FIRST_ERROR);
        // XIGetClientPointer.
        let mut b = c.body();
        b.u32(0);
        c.request(XI, 45, b).await;
        let r = c.reply().await;
        assert_eq!((r[8], c.u16(&r[10..12])), (1, 2));

        // --- Selección y eventos ----------------------------------------------
        // La ventana pide también los eventos básicos: en ella manda XI2.
        let core = M_KEY_PRESS | M_BUTTON_PRESS | M_BUTTON_RELEASE | M_POINTER_MOTION;
        let window = c
            .create_window(
                (100, 50, 80, 60),
                &[(ATTR_OVERRIDE, 1), (ATTR_EVENTS, core)],
            )
            .await;
        let inner = c.child_window(window, (10, 10, 20, 20)).await;
        c.map(inner).await;
        c.map(window).await;
        let all = [
            XI_KEY_PRESS,
            XI_KEY_RELEASE,
            XI_BUTTON_PRESS,
            XI_BUTTON_RELEASE,
            XI_MOTION,
            XI_ENTER,
            XI_LEAVE,
            XI_FOCUS_IN,
        ];
        c.xi_select(window, 1, &all).await;
        let mut b = c.body();
        b.u32(window);
        c.request(XI, 60, b).await;
        let r = c.reply().await;
        let mask = all.iter().fold(0u32, |m, t| m | 1 << t);
        assert_eq!((c.u16(&r[8..10]), c.u16(&r[32..34])), (1, 1));
        assert_eq!(&r[36..40], mask.to_le_bytes());
        // Un dispositivo que no existe.
        let mut b = c.body();
        b.u32(window).u16(1).u16(0).u16(77).u16(1).u32(1);
        c.request(XI, 46, b).await;
        assert_eq!(c.error().await.0, XI_FIRST_ERROR);

        let send = |input| backend.input.send(input).unwrap();
        // Entrar: XI_Enter, con la hija que hay debajo.
        send(DesktopInput::PointerMove { x: 115, y: 65 });
        let p = c.xi_packet(XI_ENTER).await;
        assert_eq!(p.len(), 72 + 4);
        assert_eq!((c.u16(&p[10..12]), c.u16(&p[16..18])), (2, 4));
        assert_eq!((c.u32(&p[24..28]), c.u32(&p[28..32])), (window, inner));
        assert_eq!(
            (c.u32(&p[32..36]) >> 16, c.u32(&p[44..48]) >> 16),
            (115, 15)
        );
        // Moverse: XI_Motion con los dos ejes; el evento básico no llega.
        let ev = c.xi_event(XI_MOTION).await;
        assert_eq!((ev.window, ev.child, ev.at), (window, inner, (15, 15)));
        send(DesktopInput::PointerMove { x: 150, y: 100 });
        let ev = c.xi_event(XI_MOTION).await;
        assert_eq!(
            (ev.device, ev.source, ev.window, ev.child),
            (2, 4, window, 0)
        );
        assert_eq!(
            (ev.root, ev.at, &ev.axes[..]),
            ((150, 100), (50, 50), &[150, 100][..])
        );
        // Botón: pulsación sin botones en el estado, suelta con el suyo.
        send(DesktopInput::PointerButton {
            button: MouseButton::Left,
            pressed: true,
            x: 150,
            y: 100,
        });
        let ev = c.xi_event(XI_BUTTON_PRESS).await;
        assert_eq!(
            (ev.detail, ev.buttons, ev.window, ev.at),
            (1, 0, window, (50, 50))
        );
        // Con el botón pulsado la ventana se queda el puntero: el
        // movimiento le sigue llegando desde fuera, y la suelta también.
        send(DesktopInput::PointerMove { x: 300, y: 200 });
        let ev = c.xi_event(XI_MOTION).await;
        assert_eq!((ev.window, ev.at, ev.buttons), (window, (200, 150), 0b10));
        send(DesktopInput::PointerButton {
            button: MouseButton::Left,
            pressed: false,
            x: 300,
            y: 200,
        });
        let ev = c.xi_event(XI_BUTTON_RELEASE).await;
        assert_eq!((ev.detail, ev.buttons, ev.window), (1, 0b10, window));
        c.xi_packet(XI_LEAVE).await;
        // La rueda llega como botones 4 y 5.
        send(DesktopInput::Wheel {
            x: 150,
            y: 100,
            delta: -1,
            horizontal: false,
        });
        assert_eq!(c.xi_event(XI_BUTTON_PRESS).await.detail, 5);
        assert_eq!(c.xi_event(XI_BUTTON_RELEASE).await.detail, 5);

        // Teclado: con el foco en la ventana, XI_FocusIn y teclas de XI2.
        let mut b = c.body();
        b.u32(window).u32(0);
        c.request(42, 1, b).await;
        let p = c.xi_packet(XI_FOCUS_IN).await;
        assert_eq!((c.u16(&p[10..12]), c.u32(&p[24..28])), (3, window));
        key(&backend, KeyCode::Shift, true);
        key(&backend, KeyCode::Letter(b'a'), true);
        key(&backend, KeyCode::Letter(b'a'), true);
        key(&backend, KeyCode::Letter(b'a'), false);
        key(&backend, KeyCode::Shift, false);
        let ev = c.xi_event(XI_KEY_PRESS).await;
        assert_eq!((ev.device, ev.source, ev.mods), (3, 5, 0), "Mayús");
        let ev = c.xi_event(XI_KEY_PRESS).await;
        assert_eq!((ev.detail, ev.mods, ev.flags), (letter_a as u32, 1, 0));
        // La segunda pulsación sin soltar es una repetición.
        let ev = c.xi_event(XI_KEY_PRESS).await;
        assert_eq!((ev.detail, ev.flags), (letter_a as u32, 1 << 16));
        let ev = c.xi_event(XI_KEY_RELEASE).await;
        assert_eq!((ev.detail, ev.window), (letter_a as u32, window));
        c.xi_event(XI_KEY_RELEASE).await;

        // XIQueryPointer.
        let mut b = c.body();
        b.u32(window).u16(2).u16(0);
        c.request(XI, 40, b).await;
        let r = c.reply().await;
        assert_eq!(r.len(), 56 + 4);
        assert_eq!((c.u32(&r[8..12]), c.u32(&r[12..16])), (ROOT, 0));
        assert_eq!(
            (c.u32(&r[16..20]) >> 16, c.u32(&r[24..28]) >> 16),
            (150, 50)
        );

        // --- Capturas -----------------------------------------------------------
        // Activa, del puntero: el movimiento fuera de la ventana le llega.
        let grab = |c: &TestClient, device: u16, mask: u32| {
            let mut b = c.body();
            b.u32(window)
                .u32(0)
                .u32(0)
                .u16(device)
                .u8(1)
                .u8(1)
                .u8(0)
                .u8(0);
            b.u16(1).bytes(&mask.to_le_bytes());
            b
        };
        let b = grab(&c, 2, 1 << XI_MOTION | 1 << XI_BUTTON_PRESS);
        c.request(XI, 51, b).await;
        assert_eq!(c.reply().await[8], 0, "captura concedida");
        send(DesktopInput::PointerMove { x: 20, y: 30 });
        let ev = c.xi_event(XI_MOTION).await;
        assert_eq!((ev.window, ev.at), (window, (-80, -20)));
        // Otro cliente no puede capturarlo mientras tanto.
        let mut other = TestClient::connect(&server, !be).await;
        let b = {
            let mut b = other.body();
            b.u32(ROOT).u32(0).u32(0).u16(2).u8(1).u8(1).u8(0).u8(0);
            b.u16(1).u32(0);
            b
        };
        other.request(XI, 51, b).await;
        assert_eq!(other.reply().await[8], 1, "ya capturado");
        let mut b = c.body();
        b.u32(0).u16(2).u16(0);
        c.request(XI, 52, b).await;
        c.sync().await;
        assert!(lock(&server.inner.state).grab.is_none());
        // Del teclado: las teclas van a la ventana aunque el foco esté en
        // otra parte.
        let b = grab(&c, 3, 1 << XI_KEY_PRESS);
        c.request(XI, 51, b).await;
        assert_eq!(c.reply().await[8], 0);
        let mut b = c.body();
        b.u32(1).u32(0);
        c.request(42, 1, b).await;
        c.sync().await;
        key(&backend, KeyCode::Letter(b'a'), true);
        key(&backend, KeyCode::Letter(b'a'), false);
        let ev = c.xi_event(XI_KEY_PRESS).await;
        assert_eq!((ev.detail, ev.window), (letter_a as u32, window));
        let mut b = c.body();
        b.u32(0).u16(3).u16(0);
        c.request(XI, 52, b).await;
        // Pasiva, del botón derecho con cualquier modificador.
        let mut b = c.body();
        b.u32(0).u32(window).u32(0).u32(3).u16(2).u16(1).u16(1);
        b.u8(0).u8(1).u8(1).u8(0).u16(0);
        b.bytes(&(1u32 << XI_BUTTON_PRESS | 1 << XI_BUTTON_RELEASE).to_le_bytes());
        b.u32(1 << 31);
        c.request(XI, 54, b).await;
        let r = c.reply().await;
        assert_eq!(c.u16(&r[8..10]), 0, "sin modificadores rechazados");
        send(DesktopInput::PointerButton {
            button: MouseButton::Right,
            pressed: true,
            x: 150,
            y: 100,
        });
        let ev = c.xi_event(XI_BUTTON_PRESS).await;
        assert_eq!((ev.detail, ev.window), (3, window));
        send(DesktopInput::PointerButton {
            button: MouseButton::Right,
            pressed: false,
            x: 150,
            y: 100,
        });
        c.xi_event(XI_BUTTON_RELEASE).await;
        // Las de entrada o de foco no existen: se devuelven como rechazadas.
        let mut b = c.body();
        b.u32(0).u32(window).u32(0).u32(0).u16(2).u16(1).u16(1);
        b.u8(2).u8(1).u8(1).u8(0).u16(0);
        b.u32(0).u32(0);
        c.request(XI, 54, b).await;
        let r = c.reply().await;
        assert_eq!(c.u16(&r[8..10]), 1);

        // XIChangeCursor: el cursor de la ventana (así lo pone GTK 4).
        while backend.events.try_recv().is_ok() {}
        let font = c.open_font("cursor").await;
        let text = c.glyph_cursor(font, 152).await;
        let mut b = c.body();
        b.u32(window).u32(text).u16(2).u16(0);
        c.request(XI, 42, b).await;
        c.sync().await;
        loop {
            match backend.events.recv_timeout(WAIT) {
                Ok(DesktopEvent::Cursor(cursor)) => {
                    assert_eq!(cursor, DesktopCursor::Shape(CursorShape::Text));
                    break;
                }
                Ok(_) => {}
                Err(_) => panic!("no cambió el cursor"),
            }
        }

        // Las propiedades de dispositivo no existen.
        let mut b = c.body();
        b.u16(2).u8(0).u8(0).u32(1).u32(0).u32(0).u32(8);
        c.request(XI, 59, b).await;
        let r = c.reply().await;
        assert_eq!((r.len(), c.u32(&r[8..12])), (32, 0));
        // Y lo de la versión 1 que no se sirve da BadRequest.
        let b = c.body();
        c.request(XI, 3, b).await;
        assert_eq!(c.error().await.0, BAD_REQUEST);

        // Sin XI2 seleccionado vuelven los eventos básicos.
        c.xi_select(window, 1, &[]).await;
        c.sync().await;
        send(DesktopInput::PointerMove { x: 151, y: 101 });
        let ev = c.event(MOTION_NOTIFY).await;
        assert_eq!(ev.u32_at(12), window);
        send(DesktopInput::PointerMove { x: 5, y: 5 });
        drop(c);
        drop(other);
        for _ in 0..500 {
            if server.client_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(server.client_count(), 0);
    }
    server.stop();
}
