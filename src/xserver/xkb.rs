//! Extensión XKEYBOARD (XKB): la parte que los clientes usan para leer el
//! mapa de teclado y seguir su estado.
//!
//! GTK 4 no arranca sin ella, y en cuanto existe la usan también Xlib (y con
//! ella Tk, Motif, Athena y Java), GTK 3 y Qt (vía `libxkbcommon-x11`, que
//! comprueba con rigor cada respuesta). El teclado que se describe es el
//! mismo del protocolo básico ([`Keymap`]): cada carácter tiene su propia
//! tecla, hay un solo grupo, y los únicos modificadores son Mayús, Control,
//! Alt (Mod1) y Super (Mod4).
//!
//! Qué hay:
//!
//! - `GetMap` con todos sus componentes (tipos, símbolos, acciones de las
//!   teclas modificadoras, modificadores virtuales y mapa de modificadores),
//!   `GetNames`, `GetControls`, `GetState`, `GetCompatMap`,
//!   `GetIndicatorMap`, `GetDeviceInfo` y `PerClientFlags`;
//! - los eventos `StateNotify` (cambia un modificador) y `MapNotify` (cambia
//!   el mapa: pasa al teclear un carácter que aún no tenía tecla) para quien
//!   los selecciona.
//!
//! Lo que serviría para **cambiar** el teclado (`SetMap`, `SetNames`,
//! `SetControls`, `LatchLockState`…) se acepta y no hace nada: el mapa lo
//! decide el servidor. No hay geometría, ni indicadores, ni `GetKbdByName`.

use super::keymap::{Keymap, MAX_KEYCODE, MIN_KEYCODE};
use super::proto::*;
use super::server::{ClientId, Server};

/// Código de operación de XKEYBOARD (el mismo que usa X.Org).
pub const XKB_OPCODE: u8 = 135;
pub const XKB_FIRST_EVENT: u8 = XKB_EVENT;
pub const XKB_FIRST_ERROR: u8 = 137;
/// Identificador del teclado principal (el mismo que en XInput2).
pub const KEYBOARD_ID: u8 = 3;

// Subtipos de evento (segundo byte) y su bit en `SelectEvents`.
const MAP_NOTIFY: u8 = 1;
const STATE_NOTIFY: u8 = 2;
/// Tipos de evento distintos que se pueden seleccionar.
pub const XKB_EVENT_KINDS: usize = 12;

// Componentes del mapa.
const KEY_TYPES: u16 = 1;
const KEY_SYMS: u16 = 2;
const MODIFIER_MAP: u16 = 4;
const EXPLICIT: u16 = 8;
const KEY_ACTIONS: u16 = 16;
const KEY_BEHAVIORS: u16 = 32;
const VIRTUAL_MODS: u16 = 64;
const VIRTUAL_MOD_MAP: u16 = 128;

// Componentes de `GetNames` que se sirven.
const NAME_KEYCODES: u32 = 1 << 0;
const NAME_SYMBOLS: u32 = 1 << 2;
const NAME_TYPES: u32 = 1 << 4;
const NAME_COMPAT: u32 = 1 << 5;
const NAME_KEY_TYPES: u32 = 1 << 6;
const NAME_KT_LEVELS: u32 = 1 << 7;
const NAME_KEYS: u32 = 1 << 9;
const NAME_VMODS: u32 = 1 << 11;
const NAME_GROUPS: u32 = 1 << 12;

/// Tipo de tecla de XKB: `(nombre, máscara, [(modificadores, nivel)],
/// nombres de sus niveles)`. Los cuatro primeros son los canónicos, en el
/// orden que fija el protocolo.
type KeyType = (
    &'static str,
    u8,
    &'static [(u8, u8)],
    &'static [&'static str],
);

const TYPES: [KeyType; 4] = [
    ("ONE_LEVEL", 0, &[], &["Any"]),
    ("TWO_LEVEL", 1, &[(1, 1)], &["Base", "Shift"]),
    ("ALPHABETIC", 3, &[(1, 1), (2, 1)], &["Base", "Caps"]),
    ("KEYPAD", 1, &[(1, 1)], &["Base", "Number"]),
];

/// Modificadores virtuales: nombre y modificadores reales que les
/// corresponden. Con ellos GTK reconoce Alt, Meta y Super.
const VMODS: [(&str, u8); 3] = [("Alt", 0x08), ("Meta", 0x08), ("Super", 0x40)];

/// Interpretaciones de símbolos del mapa de compatibilidad: `(símbolo,
/// modificadores que pone)`.
const INTERPRETS: [(u32, u8); 4] = [
    (0xFFE1, 0x01),
    (0xFFE3, 0x04),
    (0xFFE9, 0x08),
    (0xFFEB, 0x40),
];

/// Nombre (cuatro caracteres) de las teclas que tienen uno habitual.
fn key_name(keysym: u32) -> Option<&'static [u8; 4]> {
    Some(match keysym {
        0xFFE1 => b"LFSH",
        0xFFE3 => b"LCTL",
        0xFFE9 => b"LALT",
        0xFFEB => b"LWIN",
        0xFF0D => b"RTRN",
        0xFF1B => b"ESC\0",
        0xFF08 => b"BKSP",
        0xFF09 => b"TAB\0",
        0xFF51 => b"LEFT",
        0xFF52 => b"UP\0\0",
        0xFF53 => b"RGHT",
        0xFF54 => b"DOWN",
        0xFF50 => b"HOME",
        0xFF57 => b"END\0",
        0xFF55 => b"PGUP",
        0xFF56 => b"PGDN",
        0xFF63 => b"INS\0",
        0xFFFF => b"DELE",
        0x20 => b"SPCE",
        _ => return None,
    })
}

impl Server {
    pub(super) fn dispatch_xkb(&mut self, c: ClientId, minor: u8, rd: &mut Reader) -> XResult {
        match minor {
            // UseExtension: versión 1.0.
            0 => {
                let wanted = rd.u16()?;
                let mut w = self.reply(c, (wanted == 1) as u8);
                w.u16(1).u16(0);
                self.send_reply(c, w);
                Ok(())
            }
            1 => self.xkb_select_events(c, rd),
            // Bell, LatchLockState, SetControls, SetMap, SetCompatMap,
            // SetIndicatorMap, SetNamedIndicator, SetNames, SetGeometry y
            // SetDeviceInfo: se aceptan sin hacer nada.
            3 | 5 | 7 | 9 | 11 | 14 | 16 | 18 | 20 | 25 => self.xkb_device(rd.u16()?),
            // GetState: sin grupos; los modificadores, los de ahora.
            4 => {
                self.xkb_device(rd.u16()?)?;
                let mods = (self.keymap.mods & 0xff) as u8;
                let mut w = self.reply(c, KEYBOARD_ID);
                w.u8(mods).u8(mods).u8(0).u8(0); // mods, base, latched, locked
                w.u8(0).u8(0).u16(0).u16(0); // grupo, bloqueado, base, latched
                w.u8(mods).u8(mods).u8(mods).u8(mods).u8(mods).u8(0);
                w.u16(self.buttons);
                self.send_reply(c, w);
                Ok(())
            }
            // GetControls: un grupo y repetición de teclas.
            6 => {
                self.xkb_device(rd.u16()?)?;
                let mut w = self.reply(c, KEYBOARD_ID);
                w.u8(1).u8(1).u8(0).zeros(5); // botón, grupos, vuelta, mods internos
                w.u16(0).u16(0); // modificadores virtuales internos e ignorados
                w.u16(400).u16(33); // espera y periodo de la repetición
                w.u16(300).u16(300); // teclas lentas y antirrebote
                w.u16(160).u16(40).u16(30).u16(30).u16(500); // teclas de ratón
                w.zeros(10); // AccessX
                w.u32(0).u32(0);
                w.u32(1); // controles activos: repetición
                w.bytes(&[0xff; 32]); // todas las teclas repiten
                self.send_reply(c, w);
                Ok(())
            }
            8 => self.xkb_get_map(c, rd),
            // GetCompatMap: qué hace cada tecla modificadora, y los cuatro
            // grupos sin modificadores propios.
            10 => {
                self.xkb_device(rd.u16()?)?;
                let (groups, all) = (rd.u8()? & 0x0f, rd.u8()? != 0);
                let (first, count) = (rd.u16()? as usize, rd.u16()? as usize);
                let range = if all {
                    0..INTERPRETS.len()
                } else {
                    let first = first.min(INTERPRETS.len());
                    first..first.saturating_add(count).min(INTERPRETS.len())
                };
                let mut w = self.reply(c, KEYBOARD_ID);
                w.u8(groups).u8(0);
                w.u16(range.start as u16).u16(range.len() as u16);
                w.u16(INTERPRETS.len() as u16).zeros(16);
                for (sym, mods) in &INTERPRETS[range] {
                    // Coincide con cualquier estado; sin modificador virtual;
                    // acción: poner esos modificadores.
                    w.u32(*sym).u8(0xff).u8(1).u8(0xff).u8(0);
                    w.u8(1).u8(0).u8(*mods).u8(*mods).zeros(4);
                }
                w.zeros(4 * groups.count_ones() as usize);
                self.send_reply(c, w);
                Ok(())
            }
            // GetIndicatorState: todo apagado.
            12 => {
                self.xkb_device(rd.u16()?)?;
                let mut w = self.reply(c, KEYBOARD_ID);
                w.u32(0);
                self.send_reply(c, w);
                Ok(())
            }
            // GetIndicatorMap: no hay indicadores.
            13 => {
                self.xkb_device(rd.u16()?)?;
                let mut w = self.reply(c, KEYBOARD_ID);
                w.u32(0).u32(0).u8(0);
                self.send_reply(c, w);
                Ok(())
            }
            // GetNamedIndicator: ninguno existe.
            15 => {
                self.xkb_device(rd.u16()?)?;
                rd.skip(6)?;
                let indicator = rd.u32()?;
                let mut w = self.reply(c, KEYBOARD_ID);
                w.u32(indicator).u8(0).u8(0).u8(0).u8(0xff);
                self.send_reply(c, w);
                Ok(())
            }
            17 => self.xkb_get_names(c, rd),
            // GetGeometry: no hay ninguna.
            19 => {
                self.xkb_device(rd.u16()?)?;
                let mut w = self.reply(c, KEYBOARD_ID);
                w.u32(0).u8(0);
                self.send_reply(c, w);
                Ok(())
            }
            // PerClientFlags: solo la repetición «detectable», que es como
            // llegan aquí las repeticiones de todos modos.
            21 => {
                self.xkb_device(rd.u16()?)?;
                rd.skip(2)?;
                let (change, value) = (rd.u32()?, rd.u32()?);
                let mut w = self.reply(c, KEYBOARD_ID);
                w.u32(1).u32(change & value & 1).u32(0).u32(0);
                self.send_reply(c, w);
                Ok(())
            }
            // ListComponents: ninguno.
            22 => {
                self.xkb_device(rd.u16()?)?;
                let w = self.reply(c, KEYBOARD_ID);
                self.send_reply(c, w);
                Ok(())
            }
            // GetDeviceInfo: lo que importa es el identificador del teclado.
            24 => {
                self.xkb_device(rd.u16()?)?;
                let wanted = rd.u16()?;
                let mut w = self.reply(c, KEYBOARD_ID);
                w.u16(0).u16(0).u16(wanted).u16(0); // presente, admitido, no admitido, LEDs
                w.u8(0).u8(0).u8(0).u8(0).u8(0).u8(0); // botones; sin estado propio
                w.u16(0).u16(0).u16(0); // clases por defecto
                w.u32(0); // tipo de dispositivo
                w.u16(0).pad(); // nombre vacío
                self.send_reply(c, w);
                Ok(())
            }
            // SetDebuggingFlags.
            101 => {
                let w = self.reply(c, 0);
                self.send_reply(c, w);
                Ok(())
            }
            // GetKbdByName y lo que no existe.
            _ => Err(XError::new(BAD_IMPLEMENTATION, 0)),
        }
    }

    /// Comprueba que la petición va para el teclado (el único que hay).
    fn xkb_device(&self, spec: u16) -> XResult {
        // 0x100 es «el teclado principal»; 5, su teclado físico en XInput2.
        if matches!(spec, 0x100 | 3 | 5) {
            Ok(())
        } else {
            // BadKeyboard.
            Err(XError::new(XKB_FIRST_ERROR, spec as u32))
        }
    }

    /// `SelectEvents`: qué eventos de XKB quiere el cliente, y con qué
    /// detalles (de estos solo se mira si queda alguno).
    fn xkb_select_events(&mut self, c: ClientId, rd: &mut Reader) -> XResult {
        self.xkb_device(rd.u16()?)?;
        let (affect, clear, select_all) = (rd.u16()?, rd.u16()?, rd.u16()?);
        let (affect_map, map) = (rd.u16()? as u32, rd.u16()? as u32);
        let mut details = self
            .clients
            .get(&c)
            .map_or([0; XKB_EVENT_KINDS], |client| client.xkb_details);
        for (kind, slot) in details.iter_mut().enumerate() {
            let bit = 1u16 << kind;
            if affect & bit == 0 {
                continue;
            }
            if clear & bit != 0 {
                *slot = 0;
            } else if select_all & bit != 0 {
                *slot = u32::MAX;
            } else if kind == MAP_NOTIFY as usize {
                *slot = (*slot & !affect_map) | (map & affect_map);
            } else {
                // Cada tipo trae «a cuáles afecta» y «cuáles quiere», con
                // el tamaño que le corresponde.
                let (which, wanted) = match kind {
                    3..=5 => (rd.u32()?, rd.u32()?),
                    7..=9 => (rd.u8()? as u32, rd.u8()? as u32),
                    _ => (rd.u16()? as u32, rd.u16()? as u32),
                };
                *slot = (*slot & !which) | (wanted & which);
            }
        }
        if let Some(client) = self.clients.get_mut(&c) {
            client.xkb_details = details;
        }
        Ok(())
    }

    /// Entrega un evento de XKB a los clientes que seleccionaron su tipo.
    fn xkb_send(&mut self, kind: u8, ev: Event) {
        let clients: Vec<ClientId> = self
            .clients
            .iter()
            .filter(|(_, client)| client.xkb_details[kind as usize] != 0)
            .map(|(id, _)| *id)
            .collect();
        for client in clients {
            self.send(client, ev);
        }
    }

    /// Avisa (`StateNotify`) de que los modificadores son ahora `mods`, si
    /// no es lo último que se dijo. Quien sigue el estado del teclado por
    /// XKB (Qt, con libxkbcommon) calcula con él qué carácter da cada tecla:
    /// tiene que coincidir siempre con el estado que llevan los eventos.
    pub(super) fn xkb_announce(&mut self, mods: u16, keycode: u8, event_type: u8) {
        let mods = (mods & 0xff) as u8;
        if mods == self.xkb_mods {
            return;
        }
        self.xkb_mods = mods;
        // Cambian el estado efectivo y el base de los modificadores, y lo
        // que se deriva de ellos (estado compatible, de captura y de
        // búsqueda).
        const CHANGED: u16 = 0x1f03;
        let ev = Ev::new(XKB_EVENT, STATE_NOTIFY)
            .u32(self.now())
            .u8(KEYBOARD_ID)
            .u8(mods)
            .u8(mods)
            .u8(0)
            .u8(0)
            .u8(0)
            .u16(0)
            .u16(0)
            .u8(0)
            .u8(mods)
            .u8(mods)
            .u8(mods)
            .u8(mods)
            .u8(mods)
            .u16(self.buttons)
            .u16(CHANGED)
            .u8(keycode)
            .u8(event_type)
            .done();
        self.xkb_send(STATE_NOTIFY, ev);
    }

    /// Avisa (`MapNotify`) de que han cambiado los símbolos de unas teclas.
    pub(super) fn xkb_map_changed(&mut self, first: u8, count: u8) {
        let ev = Ev::new(XKB_EVENT, MAP_NOTIFY)
            .u32(self.now())
            .u8(KEYBOARD_ID)
            .u8(0)
            .u16(KEY_SYMS)
            .u8(MIN_KEYCODE)
            .u8(MAX_KEYCODE)
            .u8(0)
            .u8(0)
            .u8(first)
            .u8(count)
            .done();
        self.xkb_send(MAP_NOTIFY, ev);
    }

    fn xkb_get_map(&mut self, c: ClientId, rd: &mut Reader) -> XResult {
        self.xkb_device(rd.u16()?)?;
        let (full, partial) = (rd.u16()?, rd.u16()?);
        let (_first_type, _n_types) = (rd.u8()?, rd.u8()?);
        let (first_sym, n_syms) = (rd.u8()?, rd.u8()?);
        let present = (full | partial) & 0xff;
        let has = |component: u16| present & component != 0;

        // Símbolos de cada tecla: (tipo, símbolos).
        let keys: Vec<(u8, Vec<u32>)> = (MIN_KEYCODE..=MAX_KEYCODE)
            .map(|code| match self.keymap.keysyms(code) {
                [0, _] => (0, vec![]),
                [a, b] if b == 0 || b == a => (0, vec![a]),
                [a, b] => (1, vec![a, b]),
            })
            .collect();
        // Los símbolos se pueden pedir de un tramo de teclas (es lo que hace
        // Xlib cuando se le avisa de un cambio); lo demás va siempre entero.
        let sym_range = if full & KEY_SYMS != 0 || n_syms == 0 || first_sym < MIN_KEYCODE {
            0..keys.len()
        } else {
            let start = (first_sym - MIN_KEYCODE) as usize;
            start..(start + n_syms as usize).min(keys.len())
        };
        let sym_keys = &keys[sym_range.clone()];
        let total_syms: usize = sym_keys.iter().map(|(_, syms)| syms.len()).sum();
        // Teclas modificadoras: (código, máscara).
        let modifiers: Vec<(u8, u8)> = (MIN_KEYCODE..=MAX_KEYCODE)
            .filter_map(|code| {
                let bit = Keymap::modifier_bit(self.keymap.keysyms(code)[0]);
                (bit != 0).then_some((code, bit as u8))
            })
            .collect();
        let count = |component: u16, n: usize| if has(component) { n } else { 0 };
        let (first_mod, mod_span) = match (modifiers.first(), modifiers.last()) {
            (Some(first), Some(last)) if has(MODIFIER_MAP) => (first.0, last.0 - first.0 + 1),
            _ => (0, 0),
        };
        let all_keys = keys.len();

        let mut w = self.reply(c, KEYBOARD_ID);
        w.u16(0).u8(MIN_KEYCODE).u8(MAX_KEYCODE).u16(present);
        // Tipos: primero, cuántos, total.
        w.u8(0)
            .u8(count(KEY_TYPES, TYPES.len()) as u8)
            .u8(TYPES.len() as u8);
        // Símbolos: primera tecla, total de símbolos, número de teclas.
        w.u8(if has(KEY_SYMS) {
            MIN_KEYCODE + sym_range.start as u8
        } else {
            0
        })
        .u16(count(KEY_SYMS, total_syms) as u16)
        .u8(count(KEY_SYMS, sym_keys.len()) as u8);
        // Acciones: primera tecla, total de acciones, número de teclas.
        w.u8(if has(KEY_ACTIONS) { MIN_KEYCODE } else { 0 })
            .u16(count(KEY_ACTIONS, modifiers.len()) as u16)
            .u8(count(KEY_ACTIONS, all_keys) as u8);
        // Comportamientos y componentes explícitos: ninguno.
        w.u8(0).u8(0).u8(0);
        w.u8(0).u8(0).u8(0);
        // Mapa de modificadores: primera tecla, rango, entradas.
        w.u8(first_mod)
            .u8(mod_span)
            .u8(count(MODIFIER_MAP, modifiers.len()) as u8);
        // Mapa de modificadores virtuales: ninguna tecla.
        w.u8(0).u8(0).u8(0);
        w.u8(0);
        let vmods = if has(VIRTUAL_MODS) {
            (1u16 << VMODS.len()) - 1
        } else {
            0
        };
        w.u16(vmods);
        if has(KEY_TYPES) {
            for (_, mask, map, levels) in TYPES {
                w.u8(mask)
                    .u8(mask)
                    .u16(0)
                    .u8(levels.len() as u8)
                    .u8(map.len() as u8)
                    .u8(0)
                    .u8(0);
                for (mods, level) in map {
                    w.u8(1).u8(*mods).u8(*level).u8(*mods).u16(0).u16(0);
                }
            }
        }
        if has(KEY_SYMS) {
            for (kind, syms) in sym_keys {
                let groups = u8::from(!syms.is_empty());
                w.bytes(&[*kind; 4])
                    .u8(groups)
                    .u8(syms.len() as u8)
                    .u16(syms.len() as u16);
                for sym in syms {
                    w.u32(*sym);
                }
            }
        }
        if has(KEY_ACTIONS) {
            // Una acción por tecla modificadora: «poner sus modificadores».
            for code in MIN_KEYCODE..=MAX_KEYCODE {
                w.u8(modifiers.iter().any(|(m, _)| *m == code) as u8);
            }
            w.pad();
            for (_, mask) in &modifiers {
                w.u8(1).u8(0).u8(*mask).u8(*mask).zeros(4);
            }
        }
        // (Comportamientos: nada que enviar.)
        let _ = KEY_BEHAVIORS | EXPLICIT | VIRTUAL_MOD_MAP;
        if has(VIRTUAL_MODS) {
            for (_, real) in VMODS {
                w.u8(real);
            }
            w.pad();
        }
        if has(MODIFIER_MAP) {
            for (code, mask) in &modifiers {
                w.u8(*code).u8(*mask);
            }
            w.pad();
        }
        self.send_reply(c, w);
        Ok(())
    }

    fn xkb_get_names(&mut self, c: ClientId, rd: &mut Reader) -> XResult {
        self.xkb_device(rd.u16()?)?;
        rd.skip(2)?;
        let which = rd.u32()?
            & (NAME_KEYCODES
                | NAME_SYMBOLS
                | NAME_TYPES
                | NAME_COMPAT
                | NAME_KEY_TYPES
                | NAME_KT_LEVELS
                | NAME_KEYS
                | NAME_VMODS
                | NAME_GROUPS);
        let has = |component: u32| which & component != 0;
        let levels: usize = TYPES.iter().map(|t| t.3.len()).sum();
        let keys = (MAX_KEYCODE - MIN_KEYCODE) as usize + 1;

        let mut w = self.reply(c, KEYBOARD_ID);
        w.u32(which).u8(MIN_KEYCODE).u8(MAX_KEYCODE);
        w.u8(TYPES.len() as u8).u8(1); // tipos; grupos con nombre (el único)
        w.u16((1 << VMODS.len()) - 1);
        w.u8(MIN_KEYCODE).u8(keys as u8);
        w.u32(0).u8(0).u8(0); // indicadores, grupos de radio, alias
        w.u16(levels as u16).zeros(4);
        // Nombres del mapa y de sus partes.
        for (component, name) in [
            (NAME_KEYCODES, "godremote"),
            (NAME_SYMBOLS, "godremote"),
            (NAME_TYPES, "complete"),
            (NAME_COMPAT, "complete"),
        ] {
            if has(component) {
                w.u32(self.atoms.intern(name));
            }
        }
        if has(NAME_KEY_TYPES) {
            for (name, ..) in TYPES {
                w.u32(self.atoms.intern(name));
            }
        }
        if has(NAME_KT_LEVELS) {
            for (.., names) in TYPES {
                w.u8(names.len() as u8);
            }
            w.pad();
            for (.., names) in TYPES {
                for name in names {
                    w.u32(self.atoms.intern(name));
                }
            }
        }
        if has(NAME_VMODS) {
            for (name, _) in VMODS {
                w.u32(self.atoms.intern(name));
            }
        }
        if has(NAME_GROUPS) {
            w.u32(self.atoms.intern("GodRemote"));
        }
        if has(NAME_KEYS) {
            for code in MIN_KEYCODE..=MAX_KEYCODE {
                let keysym = self.keymap.keysyms(code)[0];
                match key_name(keysym) {
                    Some(name) => w.bytes(name),
                    // «K» y el código en hexadecimal; las vacías, sin nombre.
                    None if keysym != 0 => w.bytes(format!("K{code:03X}").as_bytes()),
                    None => w.zeros(4),
                };
            }
        }
        self.send_reply(c, w);
        Ok(())
    }
}
