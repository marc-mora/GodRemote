//! Idioma de la interfaz.
//!
//! El español es el idioma original: los textos están escritos en español en
//! el código y esos mismos textos sirven de clave para buscar su traducción
//! (como en gettext). En español no se busca nada; en inglés, un texto sin
//! traducción se enseña en español, y una prueba vigila que no falte ninguna.
//!
//! - `tr("Texto")` devuelve el texto en el idioma en uso.
//! - `trf!("Tema «{nombre}» guardado", nombre = n)` traduce y rellena los
//!   huecos, que van siempre con nombre para que cada idioma los ordene a su
//!   manera (`trf!("…{n}…", n)` vale si la variable se llama igual).
//! - `mark!("Texto")` solo señala un texto que se traducirá más tarde con
//!   `tr(variable)` (tablas, constantes).
//!
//! No se traduce lo que se guarda o se compara: nombres de temas, claves de
//! la configuración, lo que se escribe en registros o lo que viaja por la
//! red.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

mod en;

/// Lo que elige el usuario en los ajustes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Language {
    /// El del sistema operativo: español si está en español, y si no inglés.
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "es")]
    Spanish,
    #[serde(rename = "en")]
    English,
}

/// Idioma en el que se está mostrando la interfaz.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Es,
    En,
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// Pone el idioma de toda la aplicación.
pub fn set(lang: Lang) {
    CURRENT.store(lang as u8, Ordering::Relaxed);
}

/// Idioma en uso. Mientras nadie lo cambie es el español (así las pruebas y
/// las herramientas ven siempre los textos originales).
pub fn current() -> Lang {
    if CURRENT.load(Ordering::Relaxed) == Lang::En as u8 {
        Lang::En
    } else {
        Lang::Es
    }
}

/// El idioma que corresponde a lo elegido en los ajustes.
pub fn resolve(choice: Language) -> Lang {
    match choice {
        Language::Spanish => Lang::Es,
        Language::English => Lang::En,
        Language::Auto => {
            // Se pregunta al sistema una sola vez: no cambia en marcha, y en
            // algún sistema averiguarlo cuesta lanzar un programa.
            static SYSTEM: OnceLock<Lang> = OnceLock::new();
            *SYSTEM.get_or_init(|| from_tag(system_language().as_deref().unwrap_or_default()))
        }
    }
}

/// Idioma para una etiqueta de configuración regional («es_ES.UTF-8»,
/// «en-US», «C»…). Con las lenguas de España se ofrece el español; con todo
/// lo demás, incluido lo que no se reconozca, el inglés.
pub fn from_tag(tag: &str) -> Lang {
    let code: String = tag
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase();
    match code.as_str() {
        "es" | "ca" | "gl" | "eu" | "ast" => Lang::Es,
        _ => Lang::En,
    }
}

/// Idioma del sistema operativo, como etiqueta regional.
pub fn system_language() -> Option<String> {
    platform::system_language().filter(|tag| !tag.trim().is_empty())
}

#[cfg(windows)]
mod platform {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;

    pub fn system_language() -> Option<String> {
        // LOCALE_NAME_MAX_LENGTH
        let mut name = [0u16; 85];
        // SAFETY: se pasa un búfer válido con su tamaño en caracteres; la
        // función escribe como mucho eso, con el nulo final incluido.
        let written = unsafe { GetUserDefaultLocaleName(name.as_mut_ptr(), name.len() as i32) };
        (written > 1).then(|| String::from_utf16_lossy(&name[..written as usize - 1]))
    }
}

#[cfg(target_os = "macos")]
mod platform {
    pub fn system_language() -> Option<String> {
        // Las aplicaciones abiertas desde el Finder no tienen LANG: la lista
        // de idiomas preferidos está en las preferencias del usuario.
        let output = std::process::Command::new("/usr/bin/defaults")
            .args(["read", "-g", "AppleLanguages"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        super::first_apple_language(&String::from_utf8_lossy(&output.stdout))
            .or_else(super::from_environment)
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    pub fn system_language() -> Option<String> {
        super::from_environment()
    }
}

/// Las variables de entorno de siempre, por orden de precedencia.
#[cfg(not(windows))]
fn from_environment() -> Option<String> {
    environment_language(|name| std::env::var(name).ok())
}

#[cfg(any(test, not(windows)))]
fn environment_language(var: impl Fn(&str) -> Option<String>) -> Option<String> {
    // LANGUAGE es una lista («es:en»); las demás, una sola etiqueta.
    ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"]
        .iter()
        .filter_map(|name| var(name))
        .map(|value| {
            value
                .split(':')
                .next()
                .unwrap_or_default()
                .trim()
                .to_string()
        })
        .find(|value| !value.is_empty())
}

/// Primer idioma de la salida de `defaults read -g AppleLanguages`:
/// `(\n    "es-ES",\n    "en-US"\n)`.
#[cfg(any(test, target_os = "macos"))]
fn first_apple_language(output: &str) -> Option<String> {
    output
        .split(['(', ')', ',', '\n'])
        .map(|item| item.trim().trim_matches('"').trim())
        .find(|item| !item.is_empty())
        .map(str::to_string)
}

fn english() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| {
        en::tables()
            .iter()
            .flat_map(|t| t.iter().copied())
            .collect()
    })
}

/// El texto en el idioma en uso. Lo que no tenga traducción se queda como
/// está.
pub fn tr(text: &str) -> &str {
    translate(current(), text)
}

/// El texto en un idioma dado.
pub fn translate(lang: Lang, text: &str) -> &str {
    match lang {
        Lang::Es => text,
        Lang::En => english().get(text).copied().unwrap_or(text),
    }
}

/// Rellena los huecos `{nombre}` de una plantilla. `{{` y `}}` son llaves
/// literales; un hueco sin valor se deja a la vista, tal cual.
pub fn render(template: &str, args: &[(&str, &dyn Display)]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(at) = rest.find(['{', '}']) {
        out.push_str(&rest[..at]);
        let brace = rest.as_bytes()[at];
        let after = &rest[at + 1..];
        if after.as_bytes().first() == Some(&brace) {
            out.push(brace as char);
            rest = &after[1..];
        } else if brace == b'{'
            && let Some(end) = after.find('}')
        {
            let name = &after[..end];
            match args.iter().find(|(known, _)| *known == name) {
                Some((_, value)) => {
                    let _ = write!(out, "{value}");
                }
                None => {
                    let _ = write!(out, "{{{name}}}");
                }
            }
            rest = &after[end + 1..];
        } else {
            out.push(brace as char);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Traduce una plantilla y rellena sus huecos con nombre.
#[macro_export]
macro_rules! trf {
    ($template:literal $(, $($args:tt)*)?) => {{
        let template = $crate::i18n::tr($template);
        $crate::trf!(@args template [] $($($args)*)?)
    }};
    (@args $template:ident [$($done:tt)*]) => {
        $crate::i18n::render($template, &[$($done)*])
    };
    (@args $template:ident [$($done:tt)*] $name:ident = $value:expr $(, $($rest:tt)*)?) => {
        $crate::trf!(@args $template [$($done)* (stringify!($name), &$value as &dyn ::std::fmt::Display),] $($($rest)*)?)
    };
    (@args $template:ident [$($done:tt)*] $name:ident $(, $($rest:tt)*)?) => {
        $crate::trf!(@args $template [$($done)* (stringify!($name), &$name as &dyn ::std::fmt::Display),] $($($rest)*)?)
    };
}

/// Señala un texto que se traducirá después con [`tr`] (no hace nada más).
#[macro_export]
macro_rules! mark {
    ($text:literal) => {
        $text
    };
}

/// Un número con decimales como se escribe en el idioma en uso: con coma en
/// español y con punto en inglés.
pub fn decimal(value: f64, decimals: usize) -> String {
    decimal_in(current(), value, decimals)
}

/// Lo mismo, en un idioma dado.
pub fn decimal_in(lang: Lang, value: f64, decimals: usize) -> String {
    let text = format!("{value:.decimals$}");
    match lang {
        Lang::Es => text.replace('.', ","),
        Lang::En => text,
    }
}

#[cfg(test)]
mod tests;
