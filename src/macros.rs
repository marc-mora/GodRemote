//! Snippets de terminal: órdenes o secuencias de teclas con nombre que se
//! escriben, se graban tecleando o se editan, y se envían a cualquier
//! terminal desde el menú o con un atajo de teclado propio.
//!
//! (En el código y en el fichero `macros.json` conservan su nombre
//! original, «macros».)

use std::path::Path;

use anyhow::Result;
use egui::{Event, Key, Modifiers};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::i18n::tr;
use crate::{mark, trf};

/// Las teclas modificadoras llegan también como pulsaciones sueltas (Ctrl
/// izquierdo, Alt derecho…); por sí solas no son la tecla de un atajo.
pub fn is_modifier_key(key: Key) -> bool {
    matches!(
        key,
        Key::ShiftLeft
            | Key::ShiftRight
            | Key::ControlLeft
            | Key::ControlRight
            | Key::AltLeft
            | Key::AltRight
            | Key::SuperLeft
            | Key::SuperRight
    )
}

/// Lo que hace cada atajo propio de la aplicación.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppShortcut {
    Palette,
    NextTab,
    PreviousTab,
    NewTerminal,
    CloseTab,
}

const fn ctrl(key: Key, shift: bool) -> Shortcut {
    Shortcut {
        ctrl: true,
        alt: false,
        shift,
        cmd: false,
        key,
    }
}

/// Atajos de la aplicación. Todos llevan Mayús o una tecla que la shell no
/// usa con Ctrl, para no quitarle ninguna combinación al terminal. (El
/// texto dice para qué sirve cada uno; se traduce al enseñarlo.)
pub const APP_SHORTCUTS: [(Shortcut, AppShortcut, &str); 7] = [
    (
        ctrl(Key::P, true),
        AppShortcut::Palette,
        mark!("abrir la paleta de órdenes"),
    ),
    (
        ctrl(Key::Tab, false),
        AppShortcut::NextTab,
        mark!("pasar a la pestaña siguiente"),
    ),
    (
        ctrl(Key::Tab, true),
        AppShortcut::PreviousTab,
        mark!("pasar a la pestaña anterior"),
    ),
    (
        ctrl(Key::PageDown, false),
        AppShortcut::NextTab,
        mark!("pasar a la pestaña siguiente"),
    ),
    (
        ctrl(Key::PageUp, false),
        AppShortcut::PreviousTab,
        mark!("pasar a la pestaña anterior"),
    ),
    (
        ctrl(Key::T, true),
        AppShortcut::NewTerminal,
        mark!("abrir un terminal local"),
    ),
    (
        ctrl(Key::W, true),
        AppShortcut::CloseTab,
        mark!("cerrar la pestaña"),
    ),
];

/// Busca entre los eventos del fotograma un atajo de la aplicación y, si
/// está, retira la pulsación para que no llegue al terminal.
pub fn app_shortcut(events: &mut Vec<Event>) -> Option<AppShortcut> {
    APP_SHORTCUTS
        .iter()
        .find(|(shortcut, _, _)| shortcut.consume(events))
        .map(|(_, action, _)| *action)
}

/// Combinación de teclas que lanza un snippet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Shortcut {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Tecla Comando (solo macOS).
    pub cmd: bool,
    pub key: Key,
}

impl Shortcut {
    pub fn new(key: Key, modifiers: Modifiers) -> Self {
        Self {
            ctrl: modifiers.ctrl,
            alt: modifiers.alt,
            shift: modifiers.shift,
            cmd: modifiers.mac_cmd,
            key,
        }
    }

    fn is_function_key(&self) -> bool {
        self.key
            .name()
            .strip_prefix('F')
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    }

    /// Comprueba que el atajo se puede usar sin estropear el terminal.
    pub fn validate(&self) -> Result<(), String> {
        if is_modifier_key(self.key) {
            return Err(tr("Falta la tecla: Ctrl, Alt y Mayús solos no son un atajo").into());
        }
        if !(self.ctrl || self.alt || self.cmd || self.is_function_key()) {
            return Err(tr(
                "Usa Ctrl, Alt o una tecla de función (F1–F12): una tecla sola ya no se podría \
                 escribir en el terminal",
            )
            .into());
        }
        let plain_ctrl = (self.ctrl || self.cmd) && !self.alt && !self.shift;
        if plain_ctrl && matches!(self.key, Key::C | Key::V | Key::X) {
            return Err(trf!(
                "{shortcut} está reservado para copiar, pegar y cortar",
                shortcut = self
            ));
        }
        if let Some((_, _, purpose)) = APP_SHORTCUTS.iter().find(|(s, _, _)| s == self) {
            return Err(trf!(
                "{shortcut} está reservado para {purpose}",
                shortcut = self,
                purpose = tr(purpose)
            ));
        }
        if *self == ctrl(Key::F, true) {
            return Err(trf!(
                "{shortcut} está reservado para buscar en el terminal",
                shortcut = self
            ));
        }
        Ok(())
    }

    fn matches(&self, key: Key, modifiers: Modifiers) -> bool {
        *self == Shortcut::new(key, modifiers)
    }

    /// Busca el atajo entre los eventos del fotograma. Si está, retira la
    /// pulsación para que no llegue además al terminal. Devuelve `true` solo
    /// para la pulsación inicial, no para las repeticiones por mantener la
    /// tecla.
    pub fn consume(&self, events: &mut Vec<Event>) -> bool {
        let mut fired = false;
        let mut matched = false;
        events.retain(|event| match event {
            Event::Key {
                key,
                pressed: true,
                repeat,
                modifiers,
                ..
            } if self.matches(*key, *modifiers) => {
                matched = true;
                fired |= !*repeat;
                false
            }
            _ => true,
        });
        if matched {
            // La misma pulsación puede venir además como texto (AltGr en
            // Windows es Ctrl+Alt): tampoco debe escribirse.
            events.retain(|event| !matches!(event, Event::Text(_)));
        }
        fired
    }

    /// Forma en que se guarda: `Ctrl+Alt+Shift+K`.
    pub fn to_storage(&self) -> String {
        self.format("Shift")
    }

    fn format(&self, shift: &str) -> String {
        let mut parts = vec![];
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.cmd {
            parts.push("Cmd");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push(shift);
        }
        parts.push(self.key.name());
        parts.join("+")
    }

    /// Lee un atajo escrito como `Ctrl+Alt+K` (admite también «Mayús»).
    pub fn parse(text: &str) -> Option<Self> {
        let mut shortcut = Shortcut {
            ctrl: false,
            alt: false,
            shift: false,
            cmd: false,
            key: Key::Escape,
        };
        let mut key = None;
        for part in text.split('+').map(str::trim) {
            match part.to_lowercase().as_str() {
                "ctrl" | "control" => shortcut.ctrl = true,
                "alt" => shortcut.alt = true,
                "shift" | "mayús" | "mayus" => shortcut.shift = true,
                "cmd" | "command" => shortcut.cmd = true,
                // Solo puede haber una tecla que no sea modificador.
                _ if key.is_some() => return None,
                _ => key = Some(Key::from_name(part)?),
            }
        }
        shortcut.key = key?;
        Some(shortcut)
    }
}

/// Forma visible: `Ctrl+Alt+Mayús+K` (`Shift` en inglés).
impl std::fmt::Display for Shortcut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.format(tr("Mayús")))
    }
}

fn shortcut_to_json<S: Serializer>(shortcut: &Option<Shortcut>, s: S) -> Result<S::Ok, S::Error> {
    match shortcut {
        Some(shortcut) => s.serialize_str(&shortcut.to_storage()),
        None => s.serialize_none(),
    }
}

/// Un atajo que no se entiende (escrito a mano, de otra versión) se ignora
/// en lugar de invalidar todos los snippets.
fn shortcut_from_json<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Shortcut>, D::Error> {
    let text = Option::<String>::deserialize(d)?;
    Ok(text
        .as_deref()
        .and_then(Shortcut::parse)
        .filter(|s| s.validate().is_ok()))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Macro {
    pub id: Uuid,
    pub name: String,
    /// Contenido en forma editable (véase [`encode`] y [`decode`]).
    pub text: String,
    /// Atajo de teclado que lo envía al terminal activo.
    #[serde(
        default,
        rename = "atajo",
        skip_serializing_if = "Option::is_none",
        serialize_with = "shortcut_to_json",
        deserialize_with = "shortcut_from_json"
    )]
    pub shortcut: Option<Shortcut>,
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
            shortcut: None,
        });
        id
    }

    pub fn remove(&mut self, id: Uuid) {
        self.macros.retain(|m| m.id != id);
    }

    /// Otro snippet (distinto de `except`) que ya usa ese atajo.
    pub fn shortcut_owner(&self, shortcut: &Shortcut, except: Option<Uuid>) -> Option<&Macro> {
        self.macros
            .iter()
            .find(|m| m.shortcut.as_ref() == Some(shortcut) && Some(m.id) != except)
    }

    /// Snippet cuyo atajo se ha pulsado en este fotograma, si hay alguno.
    /// Retira la pulsación de los eventos.
    pub fn triggered(&self, events: &mut Vec<Event>) -> Option<Uuid> {
        self.macros
            .iter()
            .find(|m| m.shortcut.is_some_and(|s| s.consume(events)))
            .map(|m| m.id)
    }
}

/// Datos del terminal al que se envía un snippet, para sus variables.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SnippetContext {
    pub host: String,
    pub port: String,
    pub user: String,
    /// Nombre de la sesión (o de la pestaña).
    pub name: String,
}

/// Un trozo de un snippet: texto tal cual o una variable `{{…}}`.
enum Piece<'a> {
    Text(&'a str),
    /// Variable conocida.
    Variable(&'static str),
    /// `{{?Pregunta}}` o `{{?Pregunta=valor por defecto}}`.
    Question {
        label: &'a str,
        default: &'a str,
    },
}

/// Nombres de las variables (en español y en inglés) → nombre canónico.
fn variable(name: &str) -> Option<&'static str> {
    Some(match name.to_lowercase().as_str() {
        "host" | "equipo" | "servidor" => "host",
        "puerto" | "port" => "puerto",
        "usuario" | "user" => "usuario",
        "nombre" | "name" | "sesion" | "sesión" => "nombre",
        "fecha" | "date" => "fecha",
        "hora" | "time" => "hora",
        _ => return None,
    })
}

/// Parte un snippet en texto y variables. Lo que va entre `{{` y `}}` y no
/// es una variable conocida se deja como está: las plantillas de otras
/// herramientas (`docker ps --format '{{.Names}}'`) siguen funcionando.
fn pieces(text: &str) -> Vec<Piece<'_>> {
    let mut out = vec![];
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let end = after.find("}}");
        let token = end.map(|e| &after[..e]);
        let piece = token
            .filter(|t| !t.contains('\n') && t.len() <= 200)
            .and_then(|t| {
                let t = t.trim();
                match t.strip_prefix('?') {
                    Some(question) => {
                        let (label, default) = question.split_once('=').unwrap_or((question, ""));
                        let label = label.trim();
                        (!label.is_empty()).then_some(Piece::Question {
                            label,
                            default: default.trim(),
                        })
                    }
                    None => variable(t).map(Piece::Variable),
                }
            });
        match (piece, end) {
            (Some(piece), Some(end)) => {
                if start > 0 {
                    out.push(Piece::Text(&rest[..start]));
                }
                out.push(piece);
                rest = &after[end + 2..];
            }
            // No es una variable: las llaves se quedan como texto.
            _ => {
                out.push(Piece::Text(&rest[..start + 2]));
                rest = after;
            }
        }
    }
    if !rest.is_empty() {
        out.push(Piece::Text(rest));
    }
    out
}

/// Preguntas (`{{?…}}`) de un snippet, sin repetir y en orden, con su valor
/// por defecto.
pub fn questions(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![];
    for piece in pieces(text) {
        if let Piece::Question { label, default } = piece
            && !out.iter().any(|(l, _)| l == label)
        {
            out.push((label.to_string(), default.to_string()));
        }
    }
    out
}

/// Un valor insertado no debe interpretarse como teclas especiales: se
/// quitan los caracteres de control y se protegen las barras invertidas.
fn literal(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .replace('\\', "\\\\")
}

/// Sustituye las variables de un snippet: los datos del terminal de
/// destino, la fecha y la hora, y las respuestas a sus preguntas. El
/// resultado sigue en forma editable (se pasa después por [`decode`]).
pub fn expand(
    text: &str,
    context: &SnippetContext,
    answers: &[(String, String)],
    now: chrono::DateTime<chrono::Local>,
) -> String {
    let mut out = String::with_capacity(text.len());
    for piece in pieces(text) {
        match piece {
            Piece::Text(text) => out.push_str(text),
            Piece::Variable(name) => out.push_str(&literal(&match name {
                "host" => context.host.clone(),
                "puerto" => context.port.clone(),
                "usuario" => context.user.clone(),
                "nombre" => context.name.clone(),
                "fecha" => now.format("%Y-%m-%d").to_string(),
                _ => now.format("%H:%M:%S").to_string(),
            })),
            Piece::Question { label, default } => {
                let answer = answers
                    .iter()
                    .find(|(l, _)| l == label)
                    .map(|(_, value)| value.as_str())
                    .unwrap_or(default);
                out.push_str(&literal(answer));
            }
        }
    }
    out
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
    fn variables_are_replaced_per_terminal() {
        use chrono::TimeZone as _;
        let now = chrono::Local
            .with_ymd_and_hms(2026, 11, 27, 9, 5, 7)
            .unwrap();
        let web = SnippetContext {
            host: "10.0.0.5".into(),
            port: "2222".into(),
            user: "deploy".into(),
            name: "web-01".into(),
        };
        let text = "echo {{usuario}}@{{host}}:{{puerto}} ({{ Nombre }}) {{fecha}} {{hora}}\n";
        assert_eq!(
            expand(text, &web, &[], now),
            "echo deploy@10.0.0.5:2222 (web-01) 2026-11-27 09:05:07\n"
        );
        // En inglés también, y cada terminal con sus datos.
        let db = SnippetContext {
            host: "db.interno".into(),
            ..web.clone()
        };
        assert_eq!(expand("ping {{HOST}}", &db, &[], now), "ping db.interno");

        // Lo que no es una variable se respeta: plantillas de otras
        // herramientas, llaves de la shell, awk…
        let untouched =
            "docker ps --format '{{.Names}}'; awk '{print $1}' x; echo ${host} {{ {{raro";
        assert_eq!(expand(untouched, &web, &[], now), untouched);
        assert!(questions(untouched).is_empty());

        // Preguntas: sin repetir, con valor por defecto, y la misma
        // respuesta en todas sus apariciones.
        let ask = "systemctl {{?Acción=status}} {{?Servicio}}; journalctl -u {{?Servicio}}\n";
        assert_eq!(
            questions(ask),
            vec![
                ("Acción".to_string(), "status".to_string()),
                ("Servicio".to_string(), String::new())
            ]
        );
        let answers = vec![("Servicio".to_string(), "nginx".to_string())];
        assert_eq!(
            expand(ask, &web, &answers, now),
            "systemctl status nginx; journalctl -u nginx\n"
        );

        // Un valor no puede colar teclas especiales: ni saltos de línea ni
        // secuencias con barra invertida.
        let tricky = vec![("Ruta".to_string(), "C:\\new\\x03\rrm".to_string())];
        let expanded = expand("cd {{?Ruta}}", &web, &tricky, now);
        assert_eq!(decode(&expanded), b"cd C:\\new\\x03rm");
    }

    fn key_event(key: Key, modifiers: Modifiers, repeat: bool) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat,
            modifiers,
        }
    }

    #[test]
    fn shortcuts_are_readable_and_validated() {
        let ctrl_alt = Modifiers {
            ctrl: true,
            alt: true,
            ..Default::default()
        };
        let s = Shortcut::new(Key::K, ctrl_alt);
        assert_eq!(s.to_string(), "Ctrl+Alt+K");
        assert_eq!(Shortcut::parse("Ctrl+Alt+K"), Some(s));
        assert_eq!(Shortcut::parse(" alt + ctrl + k "), Some(s));
        let f5 = Shortcut::new(Key::F5, Modifiers::SHIFT);
        assert_eq!(f5.to_string(), "Mayús+F5");
        assert_eq!(f5.to_storage(), "Shift+F5");
        assert_eq!(Shortcut::parse("Mayús+F5"), Some(f5));
        assert_eq!(Shortcut::parse("Ctrl+K+J"), None);
        assert_eq!(Shortcut::parse("Ctrl+"), None);
        assert_eq!(Shortcut::parse("Ctrl+Inventada"), None);

        // Válidos: con Ctrl o Alt, o una tecla de función sola.
        assert!(s.validate().is_ok());
        assert!(f5.validate().is_ok());
        assert!(Shortcut::new(Key::F12, Modifiers::NONE).validate().is_ok());
        assert!(Shortcut::new(Key::Num1, Modifiers::ALT).validate().is_ok());
        // Las teclas modificadoras llegan como pulsaciones propias: no son
        // la tecla del atajo.
        assert!(
            Shortcut::new(Key::AltLeft, Modifiers::CTRL)
                .validate()
                .is_err()
        );
        assert!(is_modifier_key(Key::ControlRight) && !is_modifier_key(Key::D));
        // Una letra sola (o con Mayús) dejaría de poder escribirse.
        assert!(Shortcut::new(Key::A, Modifiers::NONE).validate().is_err());
        assert!(Shortcut::new(Key::A, Modifiers::SHIFT).validate().is_err());
        assert!(Shortcut::new(Key::F, Modifiers::NONE).validate().is_err());
        // Copiar, pegar y cortar no se pueden pisar; con Mayús sí.
        let e = Shortcut::new(Key::C, Modifiers::CTRL)
            .validate()
            .unwrap_err();
        assert!(e.contains("Ctrl+C"), "{e}");
        assert!(Shortcut::new(Key::V, Modifiers::CTRL).validate().is_err());
        assert!(
            Shortcut::new(Key::C, Modifiers::CTRL | Modifiers::SHIFT)
                .validate()
                .is_ok()
        );
        // Los atajos de la aplicación tampoco se pueden pisar.
        let e = Shortcut::new(Key::P, Modifiers::CTRL | Modifiers::SHIFT)
            .validate()
            .unwrap_err();
        assert!(e.contains("paleta"), "{e}");
        assert!(Shortcut::new(Key::Tab, Modifiers::CTRL).validate().is_err());
        assert!(
            Shortcut::new(Key::F, Modifiers::CTRL | Modifiers::SHIFT)
                .validate()
                .is_err()
        );
        assert!(
            Shortcut::new(Key::P, Modifiers::CTRL | Modifiers::ALT)
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn app_shortcuts_are_taken_from_the_events() {
        let press = |key, modifiers| Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        let mut events = vec![
            press(Key::A, Modifiers::NONE),
            press(Key::Tab, Modifiers::CTRL | Modifiers::SHIFT),
        ];
        assert_eq!(app_shortcut(&mut events), Some(AppShortcut::PreviousTab));
        // La pulsación del atajo ya no está; lo demás sigue.
        assert_eq!(events.len(), 1);
        assert_eq!(app_shortcut(&mut events), None);
        // Ctrl+W y Ctrl+T a secas son de la shell.
        let mut events = vec![
            press(Key::W, Modifiers::CTRL),
            press(Key::T, Modifiers::CTRL),
        ];
        assert_eq!(app_shortcut(&mut events), None);
        assert_eq!(events.len(), 2);
        let mut events = vec![press(Key::W, Modifiers::CTRL | Modifiers::SHIFT)];
        assert_eq!(app_shortcut(&mut events), Some(AppShortcut::CloseTab));
    }

    #[test]
    fn shortcut_presses_are_taken_from_the_terminal() {
        let ctrl_alt = Modifiers {
            ctrl: true,
            alt: true,
            ..Default::default()
        };
        let mut store = MacroStore::default();
        let deploy = store.add("desplegar", "make deploy\n".into());
        let logs = store.add("registros", "tail -f /var/log/syslog\n".into());
        store.macros[0].shortcut = Some(Shortcut::new(Key::D, ctrl_alt));
        store.macros[1].shortcut = Some(Shortcut::new(Key::F6, Modifiers::NONE));

        // La pulsación se retira (y el texto que la acompañe); lo demás no.
        let mut events = vec![
            Event::Text("x".into()),
            key_event(Key::A, Modifiers::NONE, false),
            key_event(Key::D, ctrl_alt, false),
            Event::Text("đ".into()),
        ];
        assert_eq!(store.triggered(&mut events), Some(deploy));
        assert_eq!(events, vec![key_event(Key::A, Modifiers::NONE, false)]);
        assert_eq!(store.triggered(&mut events), None);

        // Los modificadores deben coincidir exactamente.
        let mut events = vec![
            key_event(Key::D, Modifiers::CTRL, false),
            key_event(Key::F6, Modifiers::SHIFT, false),
        ];
        assert_eq!(store.triggered(&mut events), None);
        assert_eq!(events.len(), 2);
        let mut events = vec![key_event(Key::F6, Modifiers::NONE, false)];
        assert_eq!(store.triggered(&mut events), Some(logs));
        assert!(events.is_empty());

        // Mantener la tecla no relanza el snippet, pero tampoco se escribe.
        let mut events = vec![key_event(Key::F6, Modifiers::NONE, true)];
        assert_eq!(store.triggered(&mut events), None);
        assert!(events.is_empty());

        // Un atajo solo puede ser de un snippet.
        let f6 = Shortcut::new(Key::F6, Modifiers::NONE);
        assert_eq!(store.shortcut_owner(&f6, None).map(|m| m.id), Some(logs));
        assert!(store.shortcut_owner(&f6, Some(logs)).is_none());
    }

    #[test]
    fn store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("macros.json");
        assert!(MacroStore::load(&path).unwrap().macros.is_empty());
        let mut store = MacroStore::default();
        let id = store.add("  reiniciar  ", "systemctl restart x\n".into());
        store.add("otra", "ls\n".into());
        store.macros[1].shortcut = Shortcut::parse("Ctrl+Shift+L");
        store.save(&path).unwrap();
        let json = std::fs::read_to_string(&path).unwrap();
        assert!(json.contains("\"atajo\": \"Ctrl+Shift+L\""), "{json}");
        // Sin atajo no se escribe la clave.
        assert_eq!(json.matches("atajo").count(), 1, "{json}");
        let mut loaded = MacroStore::load(&path).unwrap();
        assert_eq!(loaded, store);
        assert_eq!(loaded.macros[0].name, "reiniciar");
        loaded.remove(id);
        assert_eq!(loaded.macros.len(), 1);

        // Ficheros de versiones anteriores (sin atajos) y atajos que no se
        // entienden o no son válidos: se cargan sin atajo.
        std::fs::write(
            &path,
            r#"{"macros":[
                {"id":"6f1c2a54-0c1e-4f0e-9a51-3d6f0c0f2b11","name":"vieja","text":"ls\n"},
                {"id":"6f1c2a54-0c1e-4f0e-9a51-3d6f0c0f2b12","name":"rara","text":"x","atajo":"Hiper+Q"},
                {"id":"6f1c2a54-0c1e-4f0e-9a51-3d6f0c0f2b13","name":"letra","text":"x","atajo":"A"},
                {"id":"6f1c2a54-0c1e-4f0e-9a51-3d6f0c0f2b14","name":"buena","text":"x","atajo":"Alt+F2"}
            ]}"#,
        )
        .unwrap();
        let loaded = MacroStore::load(&path).unwrap();
        let shortcuts: Vec<Option<String>> = loaded
            .macros
            .iter()
            .map(|m| m.shortcut.map(|s| s.to_string()))
            .collect();
        assert_eq!(
            shortcuts,
            vec![None, None, None, Some("Alt+F2".to_string())]
        );
    }
}
