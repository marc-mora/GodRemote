//! Enlaces y resaltado de palabras en el texto del terminal.
//!
//! Las reglas se guardan con los ajustes; el terminal consulta la versión
//! ya preparada ([`Compiled`]) cada vez que cambia lo que muestra.

use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use egui::Color32;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::theme::{Rgb, TermPalette};

pub const MAX_RULES: usize = 32;
const MAX_WORDS_CHARS: usize = 600;
/// Los enlaces más largos se ignoran: no son algo que abrir con un clic.
const MAX_URL_CHARS: usize = 2048;

/// Color de una regla: uno de la paleta del tema (cambia con él) o uno fijo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlColor {
    Red,
    Yellow,
    Green,
    Blue,
    Magenta,
    Cyan,
    Custom(Rgb),
}

impl HlColor {
    /// Los colores que siguen al tema, con su nombre (en español: quien lo
    /// pinte lo traduce con `tr`, o usa [`HlColor::label`]).
    pub const THEMED: [(HlColor, &'static str); 6] = [
        (HlColor::Red, crate::mark!("Rojo")),
        (HlColor::Yellow, crate::mark!("Amarillo")),
        (HlColor::Green, crate::mark!("Verde")),
        (HlColor::Blue, crate::mark!("Azul")),
        (HlColor::Magenta, crate::mark!("Magenta")),
        (HlColor::Cyan, crate::mark!("Cian")),
    ];

    /// Nombre visible, en el idioma de la interfaz.
    pub fn label(self) -> &'static str {
        Self::THEMED
            .iter()
            .find(|(color, _)| *color == self)
            .map_or(crate::i18n::tr("Propio"), |(_, label)| {
                crate::i18n::tr(label)
            })
    }

    /// Color con el que se pinta, según la paleta del tema.
    pub fn resolve(self, palette: &TermPalette) -> Color32 {
        // Los colores vivos de la paleta ANSI (9–14).
        match self {
            HlColor::Red => palette.ansi[9],
            HlColor::Green => palette.ansi[10],
            HlColor::Yellow => palette.ansi[11],
            HlColor::Blue => palette.ansi[12],
            HlColor::Magenta => palette.ansi[13],
            HlColor::Cyan => palette.ansi[14],
            HlColor::Custom(rgb) => rgb.color32(),
        }
    }

    fn to_storage(self) -> String {
        match self {
            HlColor::Red => "rojo".into(),
            HlColor::Yellow => "amarillo".into(),
            HlColor::Green => "verde".into(),
            HlColor::Blue => "azul".into(),
            HlColor::Magenta => "magenta".into(),
            HlColor::Cyan => "cian".into(),
            HlColor::Custom(rgb) => rgb.to_hex(),
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match text.trim().to_lowercase().as_str() {
            "rojo" => HlColor::Red,
            "amarillo" => HlColor::Yellow,
            "verde" => HlColor::Green,
            "azul" => HlColor::Blue,
            "magenta" => HlColor::Magenta,
            "cian" => HlColor::Cyan,
            other => HlColor::Custom(Rgb::parse(other)?),
        })
    }
}

impl Serialize for HlColor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_storage())
    }
}

impl<'de> Deserialize<'de> for HlColor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        HlColor::parse(&text)
            .ok_or_else(|| serde::de::Error::custom(crate::trf!("color desconocido: {text}", text)))
    }
}

/// Una regla: las palabras (o frases) que se pintan de un color.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// Palabras o frases separadas por comas.
    #[serde(rename = "palabras")]
    pub words: String,
    #[serde(rename = "color")]
    pub color: HlColor,
}

impl Rule {
    pub fn new(words: &str, color: HlColor) -> Self {
        Self {
            words: words.into(),
            color,
        }
    }
}

/// Ajustes del resaltado y de los enlaces del terminal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Highlight {
    /// Pintar las palabras de las reglas.
    #[serde(rename = "activo")]
    pub enabled: bool,
    /// Reconocer las direcciones web para abrirlas con Ctrl+clic.
    #[serde(rename = "enlaces")]
    pub links: bool,
    #[serde(rename = "reglas")]
    pub rules: Vec<Rule>,
}

impl Default for Highlight {
    fn default() -> Self {
        Self {
            enabled: true,
            links: true,
            rules: Self::default_rules(),
        }
    }
}

impl Highlight {
    pub fn default_rules() -> Vec<Rule> {
        vec![
            Rule::new(
                "error, errors, fail, failed, failure, fatal, critical, panic, denied, refused, \
                 unreachable, fallo, denegado",
                HlColor::Red,
            ),
            Rule::new(
                "warn, warning, warnings, deprecated, timeout, timed out, aviso, advertencia",
                HlColor::Yellow,
            ),
            Rule::new(
                "ok, success, successful, succeeded, done, passed, correcto",
                HlColor::Green,
            ),
        ]
    }

    /// Recorta lo que venga desmedido de un fichero editado a mano.
    pub fn sanitize(&mut self) {
        self.rules.truncate(MAX_RULES);
        for rule in &mut self.rules {
            if rule.words.chars().count() > MAX_WORDS_CHARS {
                rule.words = rule.words.chars().take(MAX_WORDS_CHARS).collect();
            }
        }
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Las reglas preparadas para buscarlas deprisa.
pub struct Compiled {
    /// Cambia cada vez que se aplican unos ajustes distintos.
    pub version: u64,
    pub links: bool,
    /// Términos en minúsculas de cada regla y su color.
    rules: Vec<(Vec<Vec<char>>, HlColor)>,
}

static VERSION: AtomicU64 = AtomicU64::new(1);

impl Compiled {
    pub fn new(settings: &Highlight) -> Self {
        let rules = if settings.enabled {
            settings
                .rules
                .iter()
                .take(MAX_RULES)
                .map(|rule| {
                    let terms: Vec<Vec<char>> = rule
                        .words
                        .split([',', '\n'])
                        .map(|term| term.trim().chars().map(lower).collect::<Vec<char>>())
                        .filter(|term| !term.is_empty())
                        .collect();
                    (terms, rule.color)
                })
                .filter(|(terms, _)| !terms.is_empty())
                .collect()
        } else {
            vec![]
        };
        Self {
            version: VERSION.fetch_add(1, Ordering::Relaxed),
            links: settings.links,
            rules,
        }
    }

    /// No hay nada que buscar: ni reglas ni enlaces.
    pub fn is_idle(&self) -> bool {
        self.rules.is_empty() && !self.links
    }

    /// Tramos de `line` que hay que colorear, en orden y sin solaparse.
    /// Una palabra solo cuenta si está entera («error» no pinta «terror»);
    /// si dos reglas chocan, gana la que va antes.
    pub fn spans(&self, line: &[char]) -> Vec<(Range<usize>, HlColor)> {
        if self.rules.is_empty() || line.is_empty() {
            return vec![];
        }
        let lowered: Vec<char> = line.iter().copied().map(lower).collect();
        // Regla dueña de cada carácter (0 = ninguna).
        let mut owner = vec![0u8; line.len()];
        for (index, (terms, _)) in self.rules.iter().enumerate() {
            for term in terms {
                if term.len() > lowered.len() {
                    continue;
                }
                let starts_word = is_word_char(term[0]);
                let ends_word = is_word_char(term[term.len() - 1]);
                let mut start = 0;
                while start + term.len() <= lowered.len() {
                    let end = start + term.len();
                    let found = lowered[start] == term[0]
                        && lowered[start..end] == term[..]
                        && !(starts_word && start > 0 && is_word_char(lowered[start - 1]))
                        && !(ends_word && end < lowered.len() && is_word_char(lowered[end]))
                        && owner[start..end].iter().all(|o| *o == 0);
                    if found {
                        owner[start..end].fill(index as u8 + 1);
                        start = end;
                    } else {
                        start += 1;
                    }
                }
            }
        }
        let mut spans = vec![];
        let mut start = 0;
        while start < owner.len() {
            let rule = owner[start];
            let mut end = start + 1;
            while end < owner.len() && owner[end] == rule {
                end += 1;
            }
            if rule != 0 {
                spans.push((start..end, self.rules[rule as usize - 1].1));
            }
            start = end;
        }
        spans
    }
}

static CURRENT: RwLock<Option<Arc<Compiled>>> = RwLock::new(None);

/// Aplica los ajustes de resaltado a todos los terminales.
pub fn set(settings: &Highlight) {
    let compiled = Arc::new(Compiled::new(settings));
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = Some(compiled);
}

/// Las reglas en vigor (las de fábrica si aún no se ha aplicado ninguna).
pub fn current() -> Arc<Compiled> {
    if let Some(compiled) = CURRENT.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return compiled.clone();
    }
    let mut guard = CURRENT.write().unwrap_or_else(|e| e.into_inner());
    guard
        .get_or_insert_with(|| Arc::new(Compiled::new(&Highlight::default())))
        .clone()
}

/// Comienzos que delatan una dirección web.
const LINK_PREFIXES: [&str; 4] = ["https://", "http://", "ftp://", "www."];

fn is_url_char(c: char) -> bool {
    c.is_ascii_graphic() && !matches!(c, '<' | '>' | '"' | '`' | '{' | '}' | '|' | '\\' | '^')
}

fn starts_with_ignore_case(line: &[char], prefix: &str) -> bool {
    let mut chars = line.iter();
    prefix
        .chars()
        .all(|p| chars.next().is_some_and(|c| c.to_ascii_lowercase() == p))
}

/// Direcciones web que aparecen en `line`: su posición (en caracteres) y la
/// dirección completa, lista para abrir. Solo http, https y ftp.
pub fn find_links(line: &[char]) -> Vec<(Range<usize>, String)> {
    let mut links = vec![];
    let mut i = 0;
    while i < line.len() {
        let boundary = i == 0 || !line[i - 1].is_alphanumeric();
        let prefix = if boundary {
            LINK_PREFIXES
                .iter()
                .find(|p| starts_with_ignore_case(&line[i..], p))
        } else {
            None
        };
        let Some(prefix) = prefix else {
            i += 1;
            continue;
        };
        let mut end = i;
        while end < line.len() && is_url_char(line[end]) {
            end += 1;
        }
        // La puntuación del final suele ser de la frase, no del enlace; un
        // paréntesis de cierre solo cuenta si se abrió dentro.
        while end > i {
            let last = line[end - 1];
            let unbalanced = |open: char, close: char| {
                let count = |c: char| line[i..end].iter().filter(|x| **x == c).count();
                last == close && count(close) > count(open)
            };
            if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*')
                || unbalanced('(', ')')
                || unbalanced('[', ']')
            {
                end -= 1;
            } else {
                break;
            }
        }
        let body = i + prefix.len();
        let has_host = end > body && line[body].is_alphanumeric();
        // «www.» a secas necesita al menos otro punto (www.ejemplo.com).
        let plausible = has_host
            && (*prefix != "www."
                || line[body..end]
                    .iter()
                    .take_while(|c| **c != '/')
                    .any(|c| *c == '.'));
        if plausible && end - i <= MAX_URL_CHARS {
            let text: String = line[i..end].iter().collect();
            let url = if *prefix == "www." {
                format!("https://{text}")
            } else {
                text
            };
            links.push((i..end, url));
            i = end;
        } else {
            i += prefix.len();
        }
    }
    links
}

/// Una dirección que se puede entregar al navegador del sistema: http,
/// https o ftp, sin espacios ni caracteres de control.
pub fn is_safe_url(url: &str) -> bool {
    let lowered = url.to_ascii_lowercase();
    let scheme_ok = ["https://", "http://", "ftp://"]
        .iter()
        .any(|scheme| lowered.starts_with(scheme) && lowered.len() > scheme.len());
    scheme_ok && url.len() <= MAX_URL_CHARS && url.chars().all(is_url_char)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    fn links(text: &str) -> Vec<String> {
        find_links(&chars(text))
            .into_iter()
            .map(|(_, url)| url)
            .collect()
    }

    #[test]
    fn links_are_found_with_their_limits() {
        assert_eq!(
            links("mira https://ejemplo.com/a?b=1&c=2#x y luego"),
            ["https://ejemplo.com/a?b=1&c=2#x"]
        );
        // La puntuación de la frase no forma parte del enlace.
        assert_eq!(
            links("Visita http://ejemplo.com/doc."),
            ["http://ejemplo.com/doc"]
        );
        assert_eq!(
            links("(ver https://ejemplo.com/a)"),
            ["https://ejemplo.com/a"]
        );
        assert_eq!(
            links("https://es.wikipedia.org/wiki/Rust_(lenguaje)"),
            ["https://es.wikipedia.org/wiki/Rust_(lenguaje)"]
        );
        assert_eq!(
            links("<https://a.example/x> 'ftp://b.example/y', \"http://c.example\""),
            [
                "https://a.example/x",
                "ftp://b.example/y",
                "http://c.example"
            ]
        );
        // «www.» se completa con https.
        assert_eq!(
            links("en www.ejemplo.com/x."),
            ["https://www.ejemplo.com/x"]
        );
        assert_eq!(links("HTTPS://EJEMPLO.COM"), ["HTTPS://EJEMPLO.COM"]);
        // Las posiciones son las del texto original.
        let found = find_links(&chars("→ http://a.b/c│"));
        assert_eq!(found[0].0, 2..14);
    }

    #[test]
    fn things_that_are_not_links() {
        assert!(links("nada que ver").is_empty());
        assert!(links("https:// http://").is_empty());
        assert!(links("www. www.sinpunto xhttp://a.b awww.a.b").is_empty());
        assert!(links("file:///etc/passwd javascript:alert(1) ssh://a.b").is_empty());
        assert!(links("https://-opcion").is_empty());
    }

    #[test]
    fn only_web_addresses_are_safe_to_open() {
        assert!(is_safe_url("https://ejemplo.com/a?b=c"));
        assert!(is_safe_url("ftp://ejemplo.com"));
        assert!(!is_safe_url("https://"));
        assert!(!is_safe_url("file:///etc/passwd"));
        assert!(!is_safe_url("https://a.b/ & calc.exe"));
        assert!(!is_safe_url("https://a.b/\"x"));
        assert!(!is_safe_url("-https://a.b"));
        assert!(!is_safe_url("https://a.b/\nx"));
    }

    fn spans(rules: &Compiled, text: &str) -> Vec<(String, HlColor)> {
        let line = chars(text);
        rules
            .spans(&line)
            .into_iter()
            .map(|(range, color)| (line[range].iter().collect(), color))
            .collect()
    }

    #[test]
    fn whole_words_are_highlighted() {
        let rules = Compiled::new(&Highlight::default());
        assert_eq!(
            spans(&rules, "ERROR: disk Failed, status OK"),
            [
                ("ERROR".to_string(), HlColor::Red),
                ("Failed".to_string(), HlColor::Red),
                ("OK".to_string(), HlColor::Green),
            ]
        );
        // Solo palabras enteras.
        assert!(spans(&rules, "terror errores error_count book okay").is_empty());
        assert_eq!(
            spans(&rules, "[warning] connection timed out"),
            [
                ("warning".to_string(), HlColor::Yellow),
                ("timed out".to_string(), HlColor::Yellow),
            ]
        );
        assert_eq!(
            spans(&rules, "Acceso denegado; Permission denied."),
            [
                ("denegado".to_string(), HlColor::Red),
                ("denied".to_string(), HlColor::Red),
            ]
        );
    }

    #[test]
    fn rules_can_be_customized() {
        let mut settings = Highlight {
            enabled: true,
            links: false,
            rules: vec![
                Rule::new("[FAILED], núcleo", HlColor::Custom(Rgb::hex(0x112233))),
                // Choca con la anterior: gana la primera.
                Rule::new("failed, , ", HlColor::Blue),
            ],
        };
        let rules = Compiled::new(&settings);
        assert_eq!(
            spans(&rules, "x[failed]y failed NÚCLEO"),
            [
                ("[failed]".to_string(), HlColor::Custom(Rgb::hex(0x112233))),
                ("failed".to_string(), HlColor::Blue),
                ("NÚCLEO".to_string(), HlColor::Custom(Rgb::hex(0x112233))),
            ]
        );
        // Desactivado no pinta nada.
        settings.enabled = false;
        let rules = Compiled::new(&settings);
        assert!(spans(&rules, "failed").is_empty());
        assert!(rules.is_idle());
    }

    #[test]
    fn settings_round_trip() {
        let settings = Highlight {
            enabled: false,
            links: false,
            rules: vec![
                Rule::new("uno, dos", HlColor::Cyan),
                Rule::new("tres", HlColor::Custom(Rgb::hex(0xff8800))),
            ],
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("\"cian\""), "{json}");
        assert!(json.contains("\"#ff8800\""), "{json}");
        assert_eq!(serde_json::from_str::<Highlight>(&json).unwrap(), settings);
        // Sin el campo, las reglas de fábrica.
        let loaded: Highlight = serde_json::from_str("{}").unwrap();
        assert_eq!(loaded, Highlight::default());
        assert!(
            serde_json::from_str::<Highlight>(r#"{"reglas":[{"palabras":"a","color":"fucsia"}]}"#)
                .is_err()
        );
    }
}
