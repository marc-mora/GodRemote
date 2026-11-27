//! XKEYBOARD mínimo, solo para `xdotool`. Desactivado por defecto.
//!
//! Este servidor **no** implementa XKB, y normalmente dice a los clientes
//! que no existe: Xlib y los toolkits pasan entonces al teclado básico, que
//! es lo que se quiere (un XKB a medias les rompería el teclado a GTK o Qt).
//!
//! Pero `libxdo` da por hecho que `XkbGetMap` funciona y, si no, se cae con
//! una violación de segmento nada más arrancar, antes incluso de un simple
//! `xdotool search`. Para poder usar esa herramienta (en pruebas, sobre
//! todo) existe `XServer::set_xkb_compat`, que activa lo justo:
//! `UseExtension`, `GetMap` con el mapa de teclado básico, `GetState` y poco
//! más. Las aplicaciones Xlib (xterm, Athena…) funcionan igual con él.

use super::keymap::{Keymap, MAX_KEYCODE, MIN_KEYCODE};
use super::proto::*;
use super::server::{ClientId, Server};

/// Código de operación de XKEYBOARD (el mismo que usa X.Org).
pub const XKB_OPCODE: u8 = 135;
pub const XKB_FIRST_EVENT: u8 = 85;
pub const XKB_FIRST_ERROR: u8 = 137;
/// Identificador del teclado principal.
const DEVICE: u8 = 3;

// Componentes del mapa que se sirven.
const KEY_TYPES: u16 = 1;
const KEY_SYMS: u16 = 2;
const MODIFIER_MAP: u16 = 4;

/// Tipo de tecla de XKB: `(máscara, niveles, [(modificadores, nivel)])`.
type KeyType = (u8, u8, &'static [(u8, u8)]);

impl Server {
    pub(super) fn dispatch_xkb(&mut self, c: ClientId, minor: u8, rd: &mut Reader) -> XResult {
        match minor {
            // UseExtension: versión 1.0.
            0 => {
                let mut w = self.reply(c, 1);
                w.u16(1).u16(0);
                self.send_reply(c, w);
                Ok(())
            }
            // SelectEvents y Bell: sin respuesta.
            1 | 3 => Ok(()),
            // GetState: sin grupos; los modificadores, los de ahora.
            4 => {
                let mods = (self.keymap.mods & 0xff) as u8;
                let mut w = self.reply(c, DEVICE);
                w.u8(mods).u8(mods).u8(0).u8(0); // mods, base, latched, locked
                w.u8(0).u8(0).u16(0).u16(0); // grupo, bloqueado, base, latched
                w.u8(mods).u8(mods).u8(mods).u8(mods).u8(mods).u8(0);
                w.u16(self.buttons);
                self.send_reply(c, w);
                Ok(())
            }
            8 => self.xkb_get_map(c, rd),
            // PerClientFlags: no se admite ninguno.
            21 => {
                let mut w = self.reply(c, DEVICE);
                w.u32(0).u32(0).u32(0).u32(0);
                self.send_reply(c, w);
                Ok(())
            }
            _ => Err(XError::new(BAD_IMPLEMENTATION, 0)),
        }
    }

    fn xkb_get_map(&mut self, c: ClientId, rd: &mut Reader) -> XResult {
        let (_device, full, partial) = (rd.u16()?, rd.u16()?, rd.u16()?);
        let present = (full | partial) & (KEY_TYPES | KEY_SYMS | MODIFIER_MAP);

        // Tipos de tecla canónicos: un nivel, dos niveles, alfabética y
        // teclado numérico.
        let types: [KeyType; 4] = [
            (0, 1, &[]),
            (1, 2, &[(1, 1)]),
            (3, 2, &[(1, 1), (2, 1)]),
            (1, 2, &[(1, 1)]),
        ];
        // Símbolos de cada tecla: (tipo, símbolos).
        let keys: Vec<(u8, Vec<u32>)> = (MIN_KEYCODE..=MAX_KEYCODE)
            .map(|code| match self.keymap.keysyms(code) {
                [0, _] => (0, vec![]),
                [a, b] if b == 0 || b == a => (0, vec![a]),
                [a, b] => (1, vec![a, b]),
            })
            .collect();
        let total_syms: usize = keys.iter().map(|(_, syms)| syms.len()).sum();
        // Teclas modificadoras: (código, máscara).
        let mut modifiers: Vec<(u8, u8)> = (MIN_KEYCODE..=MAX_KEYCODE)
            .filter_map(|code| {
                let bit = Keymap::modifier_bit(self.keymap.keysyms(code)[0]);
                (bit != 0).then_some((code, bit as u8))
            })
            .collect();
        modifiers.sort_unstable();
        let has = |component: u16| present & component != 0;
        let count = |component: u16, n: usize| if has(component) { n } else { 0 };
        let (first_mod, mod_span) = match (modifiers.first(), modifiers.last()) {
            (Some(first), Some(last)) if has(MODIFIER_MAP) => (first.0, last.0 - first.0 + 1),
            _ => (0, 0),
        };

        let mut w = self.reply(c, DEVICE);
        w.u16(0).u8(MIN_KEYCODE).u8(MAX_KEYCODE).u16(present);
        // Tipos: primero, cuántos, total.
        w.u8(0)
            .u8(count(KEY_TYPES, types.len()) as u8)
            .u8(types.len() as u8);
        // Símbolos: primera tecla, total de símbolos, número de teclas.
        w.u8(if has(KEY_SYMS) { MIN_KEYCODE } else { 0 })
            .u16(count(KEY_SYMS, total_syms) as u16)
            .u8(count(KEY_SYMS, keys.len()) as u8);
        // Acciones, comportamientos y explícitos: nada.
        w.u8(0).u16(0).u8(0);
        w.u8(0).u8(0).u8(0);
        w.u8(0).u8(0).u8(0);
        // Mapa de modificadores: primera tecla, rango, entradas.
        w.u8(first_mod)
            .u8(mod_span)
            .u8(count(MODIFIER_MAP, modifiers.len()) as u8);
        // Modificadores virtuales: nada.
        w.u8(0).u8(0).u8(0).u8(0).u16(0);
        if has(KEY_TYPES) {
            for (mask, levels, map) in types {
                w.u8(mask)
                    .u8(mask)
                    .u16(0)
                    .u8(levels)
                    .u8(map.len() as u8)
                    .u8(0)
                    .u8(0);
                for (mods, level) in map {
                    w.u8(1).u8(*mods).u8(*level).u8(*mods).u16(0).u16(0);
                }
            }
        }
        if has(KEY_SYMS) {
            for (kind, syms) in &keys {
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
        if has(MODIFIER_MAP) {
            for (code, mask) in &modifiers {
                w.u8(*code).u8(*mask);
            }
            w.pad();
        }
        self.send_reply(c, w);
        Ok(())
    }
}
