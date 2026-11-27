//! Macros de terminal: secuencias de teclas con nombre que se pueden grabar
//! y reproducir en cualquier terminal.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Macro {
    pub id: Uuid,
    pub name: String,
    /// Contenido en forma editable (véase [`encode`] y [`decode`]).
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MacroStore {
    #[serde(default)]
    pub macros: Vec<Macro>,
}

impl MacroStore {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(data) => Ok(serde_json::from_slice(&data)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::paths::write_private(path, &serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    pub fn add(&mut self, name: &str, text: String) -> Uuid {
        let id = Uuid::new_v4();
        self.macros.push(Macro {
            id,
            name: name.trim().to_string(),
            text,
        });
        id
    }

    pub fn remove(&mut self, id: Uuid) {
        self.macros.retain(|m| m.id != id);
    }
}

/// Convierte teclas grabadas en texto editable: Intro es un salto de línea,
/// `\e` es Escape, `\t` el tabulador y `\xNN` cualquier otro byte de control.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    // El texto normal (UTF-8 válido) se conserva tal cual.
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '\r' => out.push('\n'),
                '\n' => out.push_str("\\x0a"),
                '\t' => out.push_str("\\t"),
                '\x1b' => out.push_str("\\e"),
                '\\' => out.push_str("\\\\"),
                c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                    out.push_str(&format!("\\x{:02x}", c as u32))
                }
                c => out.push(c),
            }
        }
        for b in chunk.invalid() {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    out
}

/// Inversa de [`encode`]: texto editable a los bytes que se envían.
pub fn decode(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut buf = [0u8; 4];
    while let Some(c) = chars.next() {
        match c {
            '\r' => {}
            '\n' => out.push(b'\r'),
            '\\' => match chars.peek().copied() {
                Some('e') => {
                    chars.next();
                    out.push(0x1b);
                }
                Some('t') => {
                    chars.next();
                    out.push(b'\t');
                }
                Some('n') | Some('r') => {
                    chars.next();
                    out.push(b'\r');
                }
                Some('\\') => {
                    chars.next();
                    out.push(b'\\');
                }
                Some('x') => {
                    let mut probe = chars.clone();
                    probe.next();
                    let hex: String = probe.by_ref().take(2).collect();
                    match u8::from_str_radix(&hex, 16) {
                        Ok(b) if hex.len() == 2 => {
                            out.push(b);
                            chars = probe;
                        }
                        // No es un escape válido: se deja la barra tal cual.
                        _ => out.push(b'\\'),
                    }
                }
                _ => out.push(b'\\'),
            },
            c => out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let recorded = b"sudo systemctl restart nginx\r\x03ls -la\t\x1b[A\\n \xc3\xb1\xff\x00";
        let text = encode(recorded);
        assert_eq!(
            text,
            "sudo systemctl restart nginx\n\\x03ls -la\\t\\e[A\\\\n ñ\\xff\\x00"
        );
        assert_eq!(decode(&text), recorded);
        // Lo que escribe una persona a mano.
        assert_eq!(decode("cd /tmp\nls\n"), b"cd /tmp\rls\r");
        assert_eq!(decode("fin\\x04"), b"fin\x04");
        assert_eq!(decode("a\\zb \\x4"), b"a\\zb \\x4");
        assert_eq!(decode("linea\r\n"), b"linea\r");
    }

    #[test]
    fn store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("macros.json");
        assert!(MacroStore::load(&path).unwrap().macros.is_empty());
        let mut store = MacroStore::default();
        let id = store.add("  reiniciar  ", "systemctl restart x\n".into());
        store.add("otra", "ls\n".into());
        store.save(&path).unwrap();
        let mut loaded = MacroStore::load(&path).unwrap();
        assert_eq!(loaded, store);
        assert_eq!(loaded.macros[0].name, "reiniciar");
        loaded.remove(id);
        assert_eq!(loaded.macros.len(), 1);
    }
}
