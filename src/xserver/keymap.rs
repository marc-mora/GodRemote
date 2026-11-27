//! Mapa de teclado del servidor X.
//!
//! Los códigos de tecla los define el servidor, así que no imita ningún
//! teclado físico: cada símbolo imprimible Latin-1 tiene su propio código y
//! un `KeyPress` sin modificadores ya produce ese carácter. Las teclas
//! especiales y los modificadores tienen los suyos, y quedan códigos libres
//! que se reasignan sobre la marcha a los caracteres que no caben.

use std::collections::HashMap;

use super::proto::{S_CONTROL, S_MOD1, S_MOD4, S_SHIFT};

pub const MIN_KEYCODE: u8 = 8;
pub const MAX_KEYCODE: u8 = 255;
/// Símbolos por código de tecla: sin Mayús y con Mayús.
pub const KEYSYMS_PER_KEYCODE: usize = 2;

pub const XK_SHIFT_L: u32 = 0xFFE1;
pub const XK_CONTROL_L: u32 = 0xFFE3;
pub const XK_ALT_L: u32 = 0xFFE9;
pub const XK_SUPER_L: u32 = 0xFFEB;

/// Teclas que no son texto: las de `KeyCode::keysym()` y alguna más.
const SPECIAL: &[u32] = &[
    XK_SHIFT_L,
    XK_CONTROL_L,
    XK_ALT_L,
    XK_SUPER_L,
    0xFF0D, // Return
    0xFF1B, // Escape
    0xFF08, // BackSpace
    0xFF09, // Tab
    0xFF51, // Left
    0xFF52, // Up
    0xFF53, // Right
    0xFF54, // Down
    0xFF50, // Home
    0xFF57, // End
    0xFF55, // Prior
    0xFF56, // Next
    0xFF63, // Insert
    0xFFFF, // Delete
    0xFFBE, // F1 … F12
    0xFFBF,
    0xFFC0,
    0xFFC1,
    0xFFC2,
    0xFFC3,
    0xFFC4,
    0xFFC5,
    0xFFC6,
    0xFFC7,
    0xFFC8,
    0xFFC9,
];

pub struct Keymap {
    /// Símbolos de cada código, desde `MIN_KEYCODE`.
    table: Vec<[u32; KEYSYMS_PER_KEYCODE]>,
    by_keysym: HashMap<u32, u8>,
    /// Primer código reasignable y cuál toca reutilizar.
    spare_first: u8,
    spare_next: u8,
    /// Teclas pulsadas (un bit por código), para `QueryKeymap`.
    pub down: [u8; 32],
    /// Modificadores activos (bits de estado de los eventos).
    pub mods: u16,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::new()
    }
}

impl Keymap {
    pub fn new() -> Self {
        let mut table = vec![];
        let mut by_keysym = HashMap::new();
        let mut push = |syms: [u32; 2]| {
            by_keysym.insert(syms[0], MIN_KEYCODE + table.len() as u8);
            table.push(syms);
        };
        // El código 8 se deja vacío: algunos clientes lo tratan como «ninguno».
        push([0, 0]);
        for ks in SPECIAL {
            push([*ks, 0]);
        }
        for ks in (0x20..=0x7Eu32).chain(0xA0..=0xFF) {
            if (b'a' as u32..=b'z' as u32).contains(&ks) {
                // La misma tecla sirve para los atajos (Ctrl+Mayús+C).
                push([ks, ks - 0x20]);
            } else {
                // Las dos columnas iguales: Xlib no cambia mayúsculas.
                push([ks, ks]);
            }
        }
        let spare_first = MIN_KEYCODE + table.len() as u8;
        table.resize((MAX_KEYCODE - MIN_KEYCODE) as usize + 1, [0, 0]);
        by_keysym.remove(&0);
        Self {
            table,
            by_keysym,
            spare_first,
            spare_next: spare_first,
            down: [0; 32],
            mods: 0,
        }
    }

    /// Código de tecla de un símbolo, si lo tiene asignado.
    pub fn keycode(&self, keysym: u32) -> Option<u8> {
        self.by_keysym.get(&keysym).copied()
    }

    /// Código para un símbolo; si no tiene, se le asigna uno de los libres.
    /// El segundo valor indica que el mapa cambió (hay que avisar con
    /// `MappingNotify`).
    pub fn keycode_or_assign(&mut self, keysym: u32) -> (u8, bool) {
        if let Some(code) = self.keycode(keysym) {
            return (code, false);
        }
        let code = self.spare_next;
        self.spare_next = if code == MAX_KEYCODE {
            self.spare_first
        } else {
            code + 1
        };
        let old = self.table[(code - MIN_KEYCODE) as usize][0];
        if self.by_keysym.get(&old) == Some(&code) {
            self.by_keysym.remove(&old);
        }
        self.table[(code - MIN_KEYCODE) as usize] = [keysym, keysym];
        self.by_keysym.insert(keysym, code);
        (code, true)
    }

    /// Símbolos de un código de tecla.
    pub fn keysyms(&self, code: u8) -> [u32; KEYSYMS_PER_KEYCODE] {
        code.checked_sub(MIN_KEYCODE)
            .and_then(|i| self.table.get(i as usize))
            .copied()
            .unwrap_or([0, 0])
    }

    /// Cambia los símbolos de un código (`ChangeKeyboardMapping`).
    pub fn set_keysyms(&mut self, code: u8, syms: [u32; KEYSYMS_PER_KEYCODE]) {
        let Some(i) = code.checked_sub(MIN_KEYCODE) else {
            return;
        };
        let old = self.table[i as usize][0];
        if self.by_keysym.get(&old) == Some(&code) {
            self.by_keysym.remove(&old);
        }
        self.table[i as usize] = syms;
        if syms[0] != 0 {
            self.by_keysym.entry(syms[0]).or_insert(code);
        }
    }

    /// Bit de estado del modificador que produce un símbolo (0 si no es un
    /// modificador).
    pub fn modifier_bit(keysym: u32) -> u16 {
        match keysym {
            XK_SHIFT_L | 0xFFE2 => S_SHIFT,
            XK_CONTROL_L | 0xFFE4 => S_CONTROL,
            XK_ALT_L | 0xFFEA | 0xFFE7 | 0xFFE8 => S_MOD1,
            XK_SUPER_L | 0xFFEC => S_MOD4,
            _ => 0,
        }
    }

    /// Anota una pulsación o suelta. Devuelve el estado de modificadores
    /// **anterior** al cambio, que es el que lleva el evento.
    pub fn press(&mut self, code: u8, pressed: bool) -> u16 {
        let before = self.mods;
        let (byte, bit) = (code as usize / 8, 1u8 << (code % 8));
        if pressed {
            self.down[byte] |= bit;
        } else {
            self.down[byte] &= !bit;
        }
        let modifier = Self::modifier_bit(self.keysyms(code)[0]);
        if pressed {
            self.mods |= modifier;
        } else {
            self.mods &= !modifier;
        }
        before
    }

    /// Códigos de las teclas de cada uno de los 8 modificadores (uno por
    /// modificador; 0 = ninguno), para `GetModifierMapping`.
    pub fn modifier_map(&self) -> [u8; 8] {
        let code = |ks| self.keycode(ks).unwrap_or(0);
        [
            code(XK_SHIFT_L),
            0,
            code(XK_CONTROL_L),
            code(XK_ALT_L),
            0,
            0,
            code(XK_SUPER_L),
            0,
        ]
    }

    /// Número de códigos que quedan para reasignar.
    pub fn spare_count(&self) -> usize {
        (MAX_KEYCODE - self.spare_first) as usize + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::{KeyCode, char_keysym};

    #[test]
    fn every_latin1_symbol_has_its_own_keycode() {
        let map = Keymap::new();
        let mut seen = std::collections::HashSet::new();
        for cp in (0x20..=0x7Eu32).chain(0xA0..=0xFF) {
            let code = map
                .keycode(cp)
                .unwrap_or_else(|| panic!("sin código: {cp:#x}"));
            assert!(code >= MIN_KEYCODE);
            assert!(seen.insert(code), "código repetido para {cp:#x}");
            // Sin Mayús ya produce el símbolo.
            assert_eq!(map.keysyms(code)[0], cp);
        }
        // Mayúsculas y minúsculas van en teclas distintas; la minúscula
        // lleva la mayúscula en la columna de Mayús (para los atajos).
        let (a, upper) = (
            map.keycode(b'a' as u32).unwrap(),
            map.keycode(b'A' as u32).unwrap(),
        );
        assert_ne!(a, upper);
        assert_eq!(map.keysyms(a), [b'a' as u32, b'A' as u32]);
        assert_eq!(map.keysyms(upper), [b'A' as u32, b'A' as u32]);
        let enye = map.keycode(char_keysym('ñ')).unwrap();
        assert_eq!(map.keysyms(enye), [0xF1, 0xF1]);
    }

    #[test]
    fn special_keys_and_modifiers() {
        let mut map = Keymap::new();
        for key in [
            KeyCode::Enter,
            KeyCode::Escape,
            KeyCode::Backspace,
            KeyCode::Tab,
            KeyCode::Space,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Insert,
            KeyCode::Delete,
            KeyCode::F(1),
            KeyCode::F(12),
            KeyCode::Letter(b'q'),
            KeyCode::Digit(7),
            KeyCode::Minus,
            KeyCode::Slash,
            KeyCode::Shift,
            KeyCode::Control,
            KeyCode::Alt,
            KeyCode::Meta,
        ] {
            assert!(map.keycode(key.keysym()).is_some(), "{key:?}");
        }
        let modifiers = map.modifier_map();
        assert_eq!(modifiers[0], map.keycode(XK_SHIFT_L).unwrap());
        assert_eq!(modifiers[2], map.keycode(XK_CONTROL_L).unwrap());
        assert_eq!(modifiers[3], map.keycode(XK_ALT_L).unwrap());
        assert_eq!(modifiers[6], map.keycode(XK_SUPER_L).unwrap());

        // El evento lleva el estado anterior a la propia tecla.
        let ctrl = map.keycode(XK_CONTROL_L).unwrap();
        let d = map.keycode(b'd' as u32).unwrap();
        assert_eq!(map.press(ctrl, true), 0);
        assert_eq!(map.press(d, true), S_CONTROL);
        assert_ne!(map.down[d as usize / 8] & (1 << (d % 8)), 0);
        assert_eq!(map.press(d, false), S_CONTROL);
        assert_eq!(map.press(ctrl, false), S_CONTROL);
        assert_eq!(map.mods, 0);
        assert_eq!(map.down, [0; 32]);
    }

    #[test]
    fn other_characters_reuse_spare_keycodes() {
        let mut map = Keymap::new();
        assert!(map.spare_count() >= 16);
        let euro = char_keysym('€');
        assert_eq!(map.keycode(euro), None);
        let (code, changed) = map.keycode_or_assign(euro);
        assert!(changed);
        assert_eq!(map.keysyms(code), [euro, euro]);
        assert_eq!(map.keycode_or_assign(euro), (code, false));
        // Al agotarse los libres se reutiliza el más antiguo.
        let spare = map.spare_count() as u32;
        for i in 0..spare {
            map.keycode_or_assign(0x0100_4e00 + i);
        }
        assert_eq!(map.keycode(euro), None);
        assert_eq!(map.keycode_or_assign(0x0100_4e00).1, false);
        // Lo fijo no se toca.
        assert_eq!(
            map.keysyms(map.keycode(b'a' as u32).unwrap())[0],
            b'a' as u32
        );
    }
}
