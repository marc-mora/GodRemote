//! Extensión XInput, versión 2.0 (XI2): teclado y ratón como «dispositivos».
//!
//! GTK 4 no arranca sin ella, y GTK 3 y Qt 5 la usan en cuanto existe: en
//! lugar de los eventos básicos de teclado y ratón piden los de XI2, que son
//! eventos genéricos (de longitud variable) con las mismas coordenadas y el
//! mismo estado, más el dispositivo del que vienen.
//!
//! Hay cuatro dispositivos, como en cualquier servidor X: el puntero y el
//! teclado «maestros» (2 y 3) y un ratón y un teclado «físicos» (4 y 5)
//! enganchados a ellos. Los eventos salen siempre del maestro.
//!
//! Qué hay: `XIQueryVersion`, `XIQueryDevice`, `XISelectEvents`,
//! `XIGetSelectedEvents`, `XIQueryPointer`, `XIWarpPointer`,
//! `XIChangeCursor`, `XIGetClientPointer`, `XISetFocus`/`XIGetFocus`,
//! capturas activas (`XIGrabDevice`) y pasivas de botón y de tecla, y los
//! eventos de tecla, botón, movimiento, entrada/salida y foco. Se anuncia la
//! versión 2.0 a propósito: sin ejes de desplazamiento suave (2.1), la rueda
//! llega como botones 4 a 7, que es lo que este servidor sabe dar; tampoco
//! hay pantallas táctiles (2.2), barreras (2.3) ni gestos (2.4). De la
//! versión 1 solo se contesta lo que preguntan las librerías al arrancar.
//! Las propiedades de dispositivo no existen (se contestan vacías).

use super::keymap::{MAX_KEYCODE, MIN_KEYCODE};
use super::proto::*;
use super::server::{ClientId, Focus, KeyboardGrab, PassiveGrab, PointerGrab, ROOT, Server};

/// Código de operación de XInputExtension (el mismo que usa X.Org).
pub const XI_OPCODE: u8 = 131;
/// Primer evento de la versión 1 (no se envía ninguno).
pub const XI_FIRST_EVENT: u8 = 66;
pub const XI_FIRST_ERROR: u8 = 129;
const BAD_DEVICE: u8 = XI_FIRST_ERROR;
/// Extensión de eventos genéricos, que es como viajan los de XI2.
pub const GE_OPCODE: u8 = 128;
/// Código de evento de los eventos genéricos.
const GENERIC_EVENT: u8 = 35;

// Dispositivos.
const ALL_DEVICES: u16 = 0;
const ALL_MASTER_DEVICES: u16 = 1;
pub const POINTER: u16 = 2;
pub const KEYBOARD: u16 = 3;
const POINTER_SLAVE: u16 = 4;
const KEYBOARD_SLAVE: u16 = 5;
/// Botones del ratón: los tres de siempre y los cuatro de las ruedas.
const BUTTONS: u16 = 7;

// Tipos de evento de XI2.
pub const XI_KEY_PRESS: u16 = 2;
pub const XI_KEY_RELEASE: u16 = 3;
pub const XI_BUTTON_PRESS: u16 = 4;
pub const XI_BUTTON_RELEASE: u16 = 5;
pub const XI_MOTION: u16 = 6;
pub const XI_ENTER: u16 = 7;
pub const XI_LEAVE: u16 = 8;
pub const XI_FOCUS_IN: u16 = 9;
pub const XI_FOCUS_OUT: u16 = 10;
/// La tecla ya estaba pulsada: es una repetición.
pub const XI_KEY_REPEAT: u32 = 1 << 16;

/// Unidades de 4 bytes que puede medir una máscara de eventos.
const MAX_MASK_UNITS: usize = 64;
/// Modificadores de una captura pasiva.
const MAX_GRAB_MODIFIERS: usize = 64;
/// «Cualquier modificador» en una captura pasiva de XI2.
const XI_ANY_MODIFIER: u32 = 1 << 31;

/// Lo que un cliente seleccionó en una ventana para un dispositivo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XiSelection {
    pub client: ClientId,
    /// 0 todos, 1 todos los maestros o el identificador de uno.
    pub device: u16,
    /// Un bit por tipo de evento (los 32 primeros).
    pub mask: u32,
}

/// La selección vale para los eventos de ese dispositivo maestro.
fn covers(selection: u16, device: u16) -> bool {
    selection == ALL_DEVICES || selection == ALL_MASTER_DEVICES || selection == device
}

/// Lee una máscara de eventos (`units` unidades de 4 bytes): un bit por
/// tipo de evento, byte a byte. Solo cuentan los 32 primeros.
fn read_mask(rd: &mut Reader, units: usize) -> XResult<u32> {
    if units > MAX_MASK_UNITS {
        return Err(XError::new(BAD_LENGTH, 0));
    }
    let bytes = rd.bytes(units * 4)?;
    let mut mask = [0u8; 4];
    for (slot, byte) in mask.iter_mut().zip(bytes) {
        *slot = *byte;
    }
    Ok(u32::from_le_bytes(mask))
}

/// Coordenada entera en coma fija 16.16.
fn fp1616(v: i32) -> u32 {
    (v.clamp(-32_768, 32_767) << 16) as u32
}

/// Botones pulsados (bits de estado 8 a 12 del protocolo básico) como
/// máscara de XI2: el bit N es el botón N.
fn button_mask(state: u16) -> [u8; 4] {
    [((state >> 7) & 0x3e) as u8, 0, 0, 0]
}

impl Server {
    /// Clientes que seleccionaron en la ventana el evento `evtype` de ese
    /// dispositivo.
    pub(super) fn xi_clients(&self, window: Xid, device: u16, evtype: u16) -> Vec<ClientId> {
        let mut out = vec![];
        if let Some(w) = self.windows.get(&window) {
            for s in &w.xi_masks {
                if covers(s.device, device)
                    && s.mask & (1 << evtype) != 0
                    && !out.contains(&s.client)
                {
                    out.push(s.client);
                }
            }
        }
        out
    }

    /// Todo lo que un cliente seleccionó en una ventana para un dispositivo.
    pub(super) fn xi_mask_of(&self, window: Xid, client: ClientId, device: u16) -> u32 {
        self.windows.get(&window).map_or(0, |w| {
            w.xi_masks
                .iter()
                .filter(|s| s.client == client && covers(s.device, device))
                .fold(0, |acc, s| acc | s.mask)
        })
    }

    // --- Eventos --------------------------------------------------------------

    /// Envía un evento genérico de XI2 a un cliente. `body` escribe lo que
    /// va tras el tipo de evento, en el orden de bytes del cliente.
    fn xi_send(&mut self, client: ClientId, evtype: u16, body: impl FnOnce(&mut Writer)) {
        let Some(c) = self.clients.get_mut(&client) else {
            return;
        };
        let mut w = Writer::new(c.be);
        w.u8(GENERIC_EVENT)
            .u8(XI_OPCODE)
            .u16(c.seq)
            .u32(0)
            .u16(evtype);
        body(&mut w);
        w.pad();
        let extra = ((w.buf.len().max(32) - 32) / 4) as u32;
        w.buf.resize(32 + extra as usize * 4, 0);
        let length = if c.be {
            extra.to_be_bytes()
        } else {
            extra.to_le_bytes()
        };
        w.buf[4..8].copy_from_slice(&length);
        if super::requests::tracing() && evtype != XI_MOTION {
            eprintln!(
                "[x11] cliente {client}: evento XI2 {evtype}, {} bytes",
                w.buf.len()
            );
        }
        c.out.extend_from_slice(&w.buf);
    }

    /// Modificadores y grupo (ninguno) de un evento de XI2.
    fn xi_state(w: &mut Writer, state: u16) {
        let mods = (state & 0xff) as u32;
        // Base, latched, locked y efectivos; y lo mismo del grupo.
        w.u32(mods).u32(0).u32(0).u32(mods).zeros(4);
    }

    /// Evento de tecla, de botón o de movimiento visto desde una ventana.
    pub(super) fn xi_device_event(
        &mut self,
        client: ClientId,
        evtype: u16,
        detail: u32,
        window: Xid,
        state: u16,
        flags: u32,
    ) {
        let keyboard = matches!(evtype, XI_KEY_PRESS | XI_KEY_RELEASE);
        let (device, source) = if keyboard {
            (KEYBOARD, KEYBOARD_SLAVE)
        } else {
            (POINTER, POINTER_SLAVE)
        };
        let (ox, oy) = self.abs_origin(window);
        let child = self.child_towards(window, self.pointer_window);
        let (x, y) = self.pointer;
        let time = self.now();
        // El movimiento lleva además los dos ejes del ratón.
        let axes = evtype == XI_MOTION;
        self.xi_send(client, evtype, |w| {
            w.u16(device).u32(time).u32(detail);
            w.u32(ROOT).u32(window).u32(child);
            w.u32(fp1616(x)).u32(fp1616(y));
            w.u32(fp1616(x - ox)).u32(fp1616(y - oy));
            w.u16(1).u16(axes as u16).u16(source).u16(0);
            w.u32(flags);
            Self::xi_state(w, state);
            w.bytes(&button_mask(state));
            if axes {
                w.bytes(&[3, 0, 0, 0]);
                w.u32(x as u32).u32(0).u32(y as u32).u32(0);
            }
        });
    }

    /// Evento de entrada, salida o foco (`XI_Enter`, `XI_Leave`,
    /// `XI_FocusIn`, `XI_FocusOut`) visto desde una ventana.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn xi_crossing_event(
        &mut self,
        client: ClientId,
        evtype: u16,
        mode: u8,
        detail: u8,
        window: Xid,
        child: Xid,
        focused: bool,
    ) {
        let focus = matches!(evtype, XI_FOCUS_IN | XI_FOCUS_OUT);
        let (device, source) = if focus {
            (KEYBOARD, KEYBOARD_SLAVE)
        } else {
            (POINTER, POINTER_SLAVE)
        };
        let (ox, oy) = self.abs_origin(window);
        let (x, y) = self.pointer;
        let (time, state) = (self.now(), self.input_state());
        self.xi_send(client, evtype, |w| {
            w.u16(device).u32(time);
            w.u16(source).u8(mode).u8(detail);
            w.u32(ROOT).u32(window).u32(child);
            w.u32(fp1616(x)).u32(fp1616(y));
            w.u32(fp1616(x - ox)).u32(fp1616(y - oy));
            w.u8(1).u8(focused as u8).u16(1);
            Self::xi_state(w, state);
            w.bytes(&button_mask(state));
        });
    }

    /// `XI_FocusIn` o `XI_FocusOut` a quienes lo seleccionaron en la ventana.
    pub(super) fn xi_focus_event(&mut self, evtype: u16, detail: u8, window: Xid) {
        for client in self.xi_clients(window, KEYBOARD, evtype) {
            self.xi_crossing_event(client, evtype, 0, detail, window, 0, true);
        }
    }

    // --- Peticiones -----------------------------------------------------------

    /// Extensión de eventos genéricos: solo tiene `QueryVersion`.
    pub(super) fn dispatch_ge(&mut self, c: ClientId, minor: u8) -> XResult {
        if minor != 0 {
            return Err(XError::new(BAD_REQUEST, 0));
        }
        let mut w = self.reply(c, 0);
        w.u16(1).u16(0);
        self.send_reply(c, w);
        Ok(())
    }

    pub(super) fn dispatch_xinput(&mut self, c: ClientId, minor: u8, rd: &mut Reader) -> XResult {
        match minor {
            // GetExtensionVersion (XI 1): presente, versión 2.0.
            1 => {
                let mut w = self.reply(c, minor);
                w.u16(2).u16(0).u8(1);
                self.send_reply(c, w);
                Ok(())
            }
            2 => self.xi_list_input_devices(c),
            40 => self.xi_query_pointer(c, rd),
            // XIWarpPointer
            41 => {
                let (_src, dst) = (rd.u32()?, rd.u32()?);
                rd.skip(12)?;
                let (dx, dy) = (rd.i32()? >> 16, rd.i32()? >> 16);
                self.xi_pointer(rd.u16()?)?;
                let (x, y) = if dst == 0 {
                    (
                        self.pointer.0.saturating_add(dx),
                        self.pointer.1.saturating_add(dy),
                    )
                } else {
                    self.win(dst)?;
                    let (ox, oy) = self.abs_origin(dst);
                    (ox.saturating_add(dx), oy.saturating_add(dy))
                };
                self.pointer_motion(x, y);
                Ok(())
            }
            // XIChangeCursor: el cursor de la ventana (solo hay un puntero).
            42 => {
                let (window, cursor) = (rd.u32()?, rd.u32()?);
                self.xi_pointer(rd.u16()?)?;
                let cursor = self.optional_cursor(cursor)?;
                self.win_mut(window)?.cursor = cursor;
                Ok(())
            }
            // XIChangeHierarchy, XISetClientPointer, XIChangeProperty,
            // XIDeleteProperty y XIBarrierReleasePointer: nada que hacer.
            43 | 44 | 57 | 58 | 61 => Ok(()),
            // XIGetClientPointer: el único que hay.
            45 => {
                let _window = rd.u32()?;
                let mut w = self.reply(c, minor);
                w.u8(1).u8(0).u16(POINTER);
                self.send_reply(c, w);
                Ok(())
            }
            46 => self.xi_select_events(c, rd),
            // XIQueryVersion: la menor de la del cliente y la 2.0.
            47 => {
                let (major, _minor) = (rd.u16()?, rd.u16()?);
                if major < 2 {
                    return Err(XError::new(BAD_VALUE, major as u32));
                }
                let mut w = self.reply(c, minor);
                w.u16(2).u16(0);
                self.send_reply(c, w);
                Ok(())
            }
            48 => self.xi_query_device(c, rd),
            // XISetFocus
            49 => {
                let (focus, _time) = (rd.u32()?, rd.u32()?);
                self.xi_keyboard(rd.u16()?)?;
                let new = match focus {
                    0 => Focus::None,
                    1 => Focus::PointerRoot,
                    w => {
                        self.win(w)?;
                        if !self.viewable(w) {
                            return Err(XError::new(BAD_MATCH, w));
                        }
                        Focus::Window(w)
                    }
                };
                self.set_focus(new);
                Ok(())
            }
            // XIGetFocus
            50 => {
                self.xi_keyboard(rd.u16()?)?;
                let focus = match self.focus {
                    Focus::None => 0,
                    Focus::PointerRoot => 1,
                    Focus::Window(w) => w,
                };
                let mut w = self.reply(c, minor);
                w.u32(focus);
                self.send_reply(c, w);
                Ok(())
            }
            51 => self.xi_grab_device(c, rd),
            // XIUngrabDevice
            52 => {
                let _time = rd.u32()?;
                match rd.u16()? {
                    POINTER | POINTER_SLAVE => {
                        if self.grab.as_ref().is_some_and(|g| g.client == c) {
                            self.end_grab();
                        }
                    }
                    KEYBOARD | KEYBOARD_SLAVE => {
                        if self.keyboard_grab.as_ref().is_some_and(|g| g.client == c) {
                            self.keyboard_grab = None;
                        }
                    }
                    other => return Err(XError::new(BAD_DEVICE, other as u32)),
                }
                Ok(())
            }
            // XIAllowEvents: como en el protocolo básico, solo importa
            // «repetir» la pulsación de una captura pasiva síncrona.
            53 => {
                let (_time, device, mode) = (rd.u32()?, rd.u16()?, rd.u8()?);
                if matches!(device, POINTER | POINTER_SLAVE) {
                    if mode == 2 {
                        self.replay_pointer(c);
                    } else if let Some(g) = &mut self.grab
                        && g.client == c
                    {
                        g.replay = None;
                    }
                }
                Ok(())
            }
            54 => self.xi_passive_grab(c, rd),
            // XIPassiveUngrabDevice
            55 => {
                let (window, detail) = (rd.u32()?, rd.u32()?);
                let (_device, count, kind) = (rd.u16()?, rd.u16()? as usize, rd.u8()?);
                rd.skip(3)?;
                if count > MAX_GRAB_MODIFIERS {
                    return Err(XError::new(BAD_LENGTH, 0));
                }
                let mut modifiers = vec![];
                for _ in 0..count {
                    modifiers.push(grab_modifiers(rd.u32()?));
                }
                self.win_mut(window)?.grabs.retain(|g| {
                    g.client != c
                        || g.xi_mask.is_none()
                        || g.key != (kind == 1)
                        || g.detail as u32 != detail
                        || !modifiers.contains(&g.modifiers)
                });
                Ok(())
            }
            // XIListProperties: los dispositivos no tienen propiedades.
            56 => {
                self.xi_device(rd.u16()?)?;
                let w = self.reply(c, minor);
                self.send_reply(c, w);
                Ok(())
            }
            // XIGetProperty: no existe ninguna.
            59 => {
                self.xi_device(rd.u16()?)?;
                let w = self.reply(c, minor);
                self.send_reply(c, w);
                Ok(())
            }
            // XIGetSelectedEvents
            60 => {
                let window = rd.u32()?;
                let mine: Vec<XiSelection> = self
                    .win(window)?
                    .xi_masks
                    .iter()
                    .filter(|s| s.client == c)
                    .copied()
                    .collect();
                let mut w = self.reply(c, minor);
                w.u16(mine.len() as u16).zeros(22);
                for s in mine {
                    w.u16(s.device).u16(1).bytes(&s.mask.to_le_bytes());
                }
                self.send_reply(c, w);
                Ok(())
            }
            // El resto de la versión 1 (abrir dispositivos, sus eventos…).
            _ => Err(XError::new(BAD_REQUEST, 0)),
        }
    }

    /// Comprueba que es un dispositivo que existe.
    fn xi_device(&self, device: u16) -> XResult {
        if (POINTER..=KEYBOARD_SLAVE).contains(&device) {
            Ok(())
        } else {
            Err(XError::new(BAD_DEVICE, device as u32))
        }
    }

    fn xi_pointer(&self, device: u16) -> XResult {
        if matches!(device, POINTER | POINTER_SLAVE) {
            Ok(())
        } else {
            Err(XError::new(BAD_DEVICE, device as u32))
        }
    }

    fn xi_keyboard(&self, device: u16) -> XResult {
        if matches!(device, KEYBOARD | KEYBOARD_SLAVE) {
            Ok(())
        } else {
            Err(XError::new(BAD_DEVICE, device as u32))
        }
    }

    /// `ListInputDevices` (XI 1): los cuatro dispositivos, sin sus clases.
    fn xi_list_input_devices(&mut self, c: ClientId) -> XResult {
        // (identificador, uso, nombre)
        let devices: [(u8, u8, &str); 4] = [
            (POINTER as u8, 0, DEVICE_NAMES[0]),
            (KEYBOARD as u8, 1, DEVICE_NAMES[1]),
            (POINTER_SLAVE as u8, 4, DEVICE_NAMES[2]),
            (KEYBOARD_SLAVE as u8, 3, DEVICE_NAMES[3]),
        ];
        let mut w = self.reply(c, 2);
        w.u8(devices.len() as u8).zeros(23);
        for (id, usage, _) in devices {
            w.u32(0).u8(id).u8(0).u8(usage).u8(0);
        }
        for (_, _, name) in devices {
            w.u8(name.len() as u8).bytes(name.as_bytes());
        }
        self.send_reply(c, w);
        Ok(())
    }

    fn xi_query_pointer(&mut self, c: ClientId, rd: &mut Reader) -> XResult {
        let window = rd.u32()?;
        self.xi_pointer(rd.u16()?)?;
        self.win(window)?;
        let (ox, oy) = self.abs_origin(window);
        let child = self.child_towards(window, self.pointer_window);
        let (x, y) = self.pointer;
        let state = self.input_state();
        let mut w = self.reply(c, 40);
        w.u32(ROOT).u32(child);
        w.u32(fp1616(x)).u32(fp1616(y));
        w.u32(fp1616(x - ox)).u32(fp1616(y - oy));
        w.u8(1).u8(0).u16(1);
        Self::xi_state(&mut w, state);
        w.bytes(&button_mask(state));
        self.send_reply(c, w);
        Ok(())
    }

    fn xi_select_events(&mut self, c: ClientId, rd: &mut Reader) -> XResult {
        let window = rd.u32()?;
        let count = rd.u16()? as usize;
        rd.skip(2)?;
        self.win(window)?;
        // Cada máscara ocupa al menos su cabecera.
        if count * 4 > rd.remaining() {
            return Err(XError::new(BAD_LENGTH, 0));
        }
        let mut selections = vec![];
        for _ in 0..count {
            let (device, units) = (rd.u16()?, rd.u16()? as usize);
            if device > KEYBOARD_SLAVE {
                return Err(XError::new(BAD_DEVICE, device as u32));
            }
            selections.push((device, read_mask(rd, units)?));
        }
        let w = self.win_mut(window)?;
        for (device, mask) in selections {
            w.xi_masks
                .retain(|s| !(s.client == c && s.device == device));
            if mask != 0 {
                w.xi_masks.push(XiSelection {
                    client: c,
                    device,
                    mask,
                });
            }
        }
        Ok(())
    }

    fn xi_query_device(&mut self, c: ClientId, rd: &mut Reader) -> XResult {
        let wanted = rd.u16()?;
        let devices: Vec<u16> = match wanted {
            ALL_DEVICES => vec![POINTER, KEYBOARD, POINTER_SLAVE, KEYBOARD_SLAVE],
            ALL_MASTER_DEVICES => vec![POINTER, KEYBOARD],
            one => {
                self.xi_device(one)?;
                vec![one]
            }
        };
        let buttons = button_mask(self.buttons);
        let labels: Vec<Xid> = BUTTON_LABELS
            .iter()
            .map(|name| self.atoms.intern(name))
            .collect();
        let axes = [self.atoms.intern("Rel X"), self.atoms.intern("Rel Y")];
        let (x, y) = self.pointer;
        let mut w = self.reply(c, 48);
        w.u16(devices.len() as u16).zeros(22);
        for device in devices {
            // (uso, enganchado a, dispositivo del que vienen sus clases)
            let (usage, attachment, source) = match device {
                POINTER => (1, KEYBOARD, POINTER_SLAVE),
                KEYBOARD => (2, POINTER, KEYBOARD_SLAVE),
                POINTER_SLAVE => (3, POINTER, POINTER_SLAVE),
                _ => (4, KEYBOARD, KEYBOARD_SLAVE),
            };
            let pointer = matches!(device, POINTER | POINTER_SLAVE);
            let name = DEVICE_NAMES[(device - POINTER) as usize];
            w.u16(device).u16(usage).u16(attachment);
            w.u16(if pointer { 3 } else { 1 });
            w.u16(name.len() as u16).u8(1).u8(0);
            w.bytes(name.as_bytes()).pad();
            if pointer {
                // Botones: estado y etiquetas.
                w.u16(1).u16(3 + BUTTONS).u16(source).u16(BUTTONS);
                w.bytes(&buttons);
                for label in &labels {
                    w.u32(*label);
                }
                // Los dos ejes, relativos y sin límites.
                for (number, (label, value)) in axes.iter().zip([x, y]).enumerate() {
                    w.u16(2).u16(11).u16(source).u16(number as u16);
                    w.u32(*label);
                    w.u32(-1i32 as u32).u32(0).u32(-1i32 as u32).u32(0);
                    w.u32(value as u32).u32(0);
                    w.u32(0).u8(0).zeros(3);
                }
            } else {
                // Teclas: todos los códigos.
                let keys = (MAX_KEYCODE - MIN_KEYCODE) as u16 + 1;
                w.u16(0).u16(2 + keys).u16(source).u16(keys);
                for code in MIN_KEYCODE..=MAX_KEYCODE {
                    w.u32(code as u32);
                }
            }
        }
        self.send_reply(c, w);
        Ok(())
    }

    /// `XIGrabDevice`: captura activa del puntero o del teclado, con los
    /// eventos de XI2 que quiere recibir quien la pide.
    fn xi_grab_device(&mut self, c: ClientId, rd: &mut Reader) -> XResult {
        let (window, _time, cursor) = (rd.u32()?, rd.u32()?, rd.u32()?);
        let device = rd.u16()?;
        let (_mode, _paired_mode, owner_events) = (rd.u8()?, rd.u8()?, rd.u8()? != 0);
        rd.skip(1)?;
        let units = rd.u16()? as usize;
        let mask = read_mask(rd, units)?;
        self.win(window)?;
        let cursor = self.optional_cursor(cursor)?;
        let pointer = match device {
            POINTER | POINTER_SLAVE => true,
            KEYBOARD | KEYBOARD_SLAVE => false,
            other => return Err(XError::new(BAD_DEVICE, other as u32)),
        };
        let taken = if pointer {
            self.grab.as_ref().is_some_and(|g| g.client != c)
        } else {
            self.keyboard_grab.as_ref().is_some_and(|g| g.client != c)
        };
        let status = if taken {
            1 // AlreadyGrabbed
        } else if !self.viewable(window) {
            3 // NotViewable
        } else if pointer {
            self.start_grab(PointerGrab {
                client: c,
                window,
                owner_events,
                mask: 0,
                xi_mask: Some(mask),
                by_button: false,
                replay: None,
                cursor,
            });
            0
        } else {
            self.keyboard_grab = Some(KeyboardGrab {
                client: c,
                window,
                owner_events,
                xi_mask: Some(mask),
                by_key: None,
            });
            0
        };
        let mut w = self.reply(c, 51);
        w.u8(status);
        self.send_reply(c, w);
        Ok(())
    }

    /// `XIPassiveGrabDevice`: capturas pasivas de botón y de tecla. Las de
    /// entrada, foco y toque no existen: se contestan como no concedidas.
    fn xi_passive_grab(&mut self, c: ClientId, rd: &mut Reader) -> XResult {
        let (_time, window, cursor, detail) = (rd.u32()?, rd.u32()?, rd.u32()?, rd.u32()?);
        let (_device, count, units) = (rd.u16()?, rd.u16()? as usize, rd.u16()? as usize);
        let (kind, mode, _paired_mode, owner_events) =
            (rd.u8()?, rd.u8()?, rd.u8()?, rd.u8()? != 0);
        rd.skip(2)?;
        let mask = read_mask(rd, units)?;
        if count > MAX_GRAB_MODIFIERS {
            return Err(XError::new(BAD_LENGTH, 0));
        }
        let mut modifiers = vec![];
        for _ in 0..count {
            modifiers.push(rd.u32()?);
        }
        let cursor = self.optional_cursor(cursor)?;
        let supported = kind <= 1 && detail <= 255;
        let mut failed = vec![];
        for wire in modifiers {
            let wanted = grab_modifiers(wire);
            let key = kind == 1;
            let w = self.win_mut(window)?;
            let same = |g: &PassiveGrab| {
                g.key == key && g.detail as u32 == detail && g.modifiers == wanted
            };
            if !supported || w.grabs.iter().any(|g| same(g) && g.client != c) {
                failed.push(wire);
                continue;
            }
            w.grabs.retain(|g| !same(g));
            if w.grabs.len() >= super::requests::MAX_GRABS {
                return Err(XError::new(BAD_ALLOC, window));
            }
            w.grabs.push(PassiveGrab {
                client: c,
                key,
                detail: detail as u8,
                modifiers: wanted,
                owner_events,
                mask: 0,
                xi_mask: Some(mask),
                // (Modo 0: síncrona.)
                sync: mode == 0,
                cursor: cursor.clone(),
            });
        }
        let mut w = self.reply(c, 54);
        w.u16(failed.len() as u16).zeros(22);
        for wire in failed {
            w.u32(wire).u8(1).zeros(3); // AlreadyGrabbed
        }
        self.send_reply(c, w);
        Ok(())
    }
}

/// Modificadores de una captura pasiva de XI2 en la forma en que se guardan
/// las del protocolo básico.
fn grab_modifiers(wire: u32) -> u16 {
    if wire & XI_ANY_MODIFIER != 0 {
        S_ANY_MODIFIER
    } else {
        (wire & 0xff) as u16
    }
}

/// Nombres de los dispositivos 2 a 5. GTK deduce de ellos qué son: con
/// «pointer» en el nombre, un ratón.
const DEVICE_NAMES: [&str; 4] = [
    "Virtual core pointer",
    "Virtual core keyboard",
    "GodRemote pointer",
    "GodRemote keyboard",
];

const BUTTON_LABELS: [&str; BUTTONS as usize] = [
    "Button Left",
    "Button Middle",
    "Button Right",
    "Button Wheel Up",
    "Button Wheel Down",
    "Button Horiz Wheel Left",
    "Button Horiz Wheel Right",
];
