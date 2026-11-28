use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::*;

#[test]
fn templates_are_filled_by_name() {
    assert_eq!(render("sin huecos", &[]), "sin huecos");
    assert_eq!(
        render("Tema «{nombre}» guardado", &[("nombre", &"Nord")]),
        "Tema «Nord» guardado"
    );
    // Cada idioma ordena los huecos a su manera, y puede repetirlos.
    let args: [(&str, &dyn Display); 2] = [("a", &1), ("b", &"dos")];
    assert_eq!(render("{b} y {a} y {b}", &args), "dos y 1 y dos");
    // Llaves literales, huecos desconocidos y llaves sueltas.
    assert_eq!(render("{{a}} = {a}", &args), "{a} = 1");
    assert_eq!(render("{falta} {a}", &args), "{falta} 1");
    assert_eq!(render("llave { suelta", &args), "llave { suelta");
    assert_eq!(render("cierre } suelto {a}", &args), "cierre } suelto 1");
    // Un valor con llaves no se vuelve a interpretar.
    assert_eq!(render("{a}", &[("a", &"{b}")]), "{b}");
}

#[test]
fn the_macros_translate_and_fill() {
    let nombre = "copia";
    let veces = 3;
    assert_eq!(crate::trf!("nada que rellenar"), "nada que rellenar");
    assert_eq!(
        crate::trf!("{nombre} ({veces})", nombre, veces),
        "copia (3)"
    );
    assert_eq!(
        crate::trf!("{n} de {total}", n = veces - 1, total = veces,),
        "2 de 3"
    );
    assert_eq!(crate::trf!("{nombre}: {n}", nombre, n = 1 + 1), "copia: 2");
    assert_eq!(crate::mark!("tal cual"), "tal cual");
}

#[test]
fn system_tags_choose_the_language() {
    for tag in [
        "es",
        "es_ES.UTF-8",
        "es-MX",
        "ES_es",
        "ca_ES.utf8",
        "gl",
        "eu_ES",
        "ast",
    ] {
        assert_eq!(from_tag(tag), Lang::Es, "{tag}");
    }
    for tag in [
        "en_US.UTF-8",
        "en-GB",
        "fr_FR",
        "de",
        "C",
        "POSIX",
        "",
        "est",
        "esperanto",
    ] {
        assert_eq!(from_tag(tag), Lang::En, "{tag}");
    }
    assert_eq!(resolve(Language::Spanish), Lang::Es);
    assert_eq!(resolve(Language::English), Lang::En);

    // Variables de entorno, por precedencia; LANGUAGE es una lista.
    let env = |vars: &'static [(&'static str, &'static str)]| {
        environment_language(move |name| {
            vars.iter()
                .find(|(known, _)| *known == name)
                .map(|(_, value)| value.to_string())
        })
    };
    assert_eq!(
        env(&[("LANG", "es_ES.UTF-8")]).as_deref(),
        Some("es_ES.UTF-8")
    );
    assert_eq!(
        env(&[("LANG", "es_ES.UTF-8"), ("LC_ALL", "en_US.UTF-8")]).as_deref(),
        Some("en_US.UTF-8")
    );
    assert_eq!(
        env(&[("LANG", "en_US.UTF-8"), ("LANGUAGE", "es:en")]).as_deref(),
        Some("es")
    );
    assert_eq!(
        env(&[("LC_ALL", ""), ("LANG", "fr_FR")]).as_deref(),
        Some("fr_FR")
    );
    assert_eq!(env(&[]), None);

    // macOS: el primero de la lista de preferidos.
    assert_eq!(
        first_apple_language("(\n    \"es-ES\",\n    \"en-US\"\n)\n").as_deref(),
        Some("es-ES")
    );
    assert_eq!(first_apple_language("(\n)\n"), None);
    assert_eq!(first_apple_language(""), None);
}

#[test]
fn texts_and_numbers_follow_the_language() {
    // (El idioma en uso es global y las pruebas corren a la vez: aquí no se
    // cambia, se pregunta por cada idioma. Mientras nadie lo cambie es el
    // español, que es lo que esperan las demás pruebas.)
    assert_eq!(current(), Lang::Es);
    assert_eq!(tr("Idioma"), "Idioma");
    assert_eq!(translate(Lang::Es, "Idioma"), "Idioma");
    assert_eq!(translate(Lang::En, "Idioma"), "Language");
    // Lo que no tiene traducción se queda como está.
    assert_eq!(
        translate(Lang::En, "un texto que no existe"),
        "un texto que no existe"
    );
    assert_eq!(decimal_in(Lang::Es, 0.745, 2), "0,74");
    assert_eq!(decimal_in(Lang::Es, 12.0, 1), "12,0");
    assert_eq!(decimal_in(Lang::Es, 3.0, 0), "3");
    assert_eq!(decimal_in(Lang::En, 0.745, 2), "0.74");
    assert_eq!(decimal(1.5, 1), "1,5");
}

// --- Que no falte ni sobre ninguna traducción --------------------------------

/// Ficheros de código fuente, sin las propias tablas de traducción.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "i18n") {
                continue;
            }
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") && !path.ends_with("i18n.rs") {
            out.push(path);
        }
    }
}

/// Lee un literal de cadena de Rust que empieza en `text[0] == '"'` y lo
/// devuelve como lo vería el programa.
fn string_literal(text: &str) -> Option<String> {
    let mut chars = text.chars();
    if chars.next() != Some('"') {
        return None;
    }
    let mut out = String::new();
    loop {
        match chars.next()? {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                '0' => out.push('\0'),
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                '\'' => out.push('\''),
                'x' => {
                    let hex: String = chars.by_ref().take(2).collect();
                    out.push(u8::from_str_radix(&hex, 16).ok()? as char);
                }
                'u' => {
                    let hex: String = chars.by_ref().skip(1).take_while(|c| *c != '}').collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                // Continuación de línea: se salta el salto y la sangría.
                '\n' => {
                    let rest = chars.as_str();
                    chars = rest.trim_start().chars();
                }
                '\r' => {
                    let rest = chars.as_str();
                    chars = rest.trim_start().chars();
                }
                _ => return None,
            },
            c => out.push(c),
        }
    }
}

/// Los textos que el código pide traducir: `tr("…")`, `trf!("…"…)` y
/// `mark!("…")`. Devuelve cada texto con el primer sitio donde aparece.
fn texts_in_code() -> BTreeMap<String, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    // (El manifiesto de la edición reforzada vive en una subcarpeta.)
    let src = [root.join("src"), root.join("../src")]
        .into_iter()
        .find(|dir| dir.join("i18n.rs").exists())
        .expect("carpeta src");
    let mut files = Vec::new();
    sources(&src, &mut files);
    files.sort();
    let mut found = BTreeMap::new();
    for file in files {
        let code = std::fs::read_to_string(&file).unwrap();
        for marker in ["tr(", "trf!(", "mark!("] {
            let mut from = 0;
            while let Some(at) = code[from..].find(marker) {
                let start = from + at;
                from = start + marker.len();
                // `tr(` suelto, no el final de otro nombre (`attr(`, `to_str(`).
                let before = code[..start].chars().next_back();
                if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                let rest = code[from..].trim_start();
                let line = code[..start].lines().count();
                let place = format!("{}:{line}", file.display());
                if rest.starts_with("r\"") || rest.starts_with("r#") {
                    panic!("{place}: las cadenas «r\"…\"» no valen para traducir");
                }
                if let Some(text) = string_literal(rest) {
                    found.entry(text).or_insert(place);
                }
            }
        }
    }
    found
}

/// Los huecos `{nombre}` de una plantilla.
fn holes(template: &str) -> BTreeSet<String> {
    let mut holes = BTreeSet::new();
    let literal = template.replace("{{", "").replace("}}", "");
    let mut rest = literal.as_str();
    while let Some(at) = rest.find('{') {
        let after = &rest[at + 1..];
        match after.find('}') {
            Some(end) => {
                holes.insert(after[..end].to_string());
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    holes
}

#[test]
fn every_text_has_its_translation() {
    let code = texts_in_code();
    let mut problems = Vec::new();

    // Ninguna clave repetida entre tablas (ni dentro de una).
    let mut table: BTreeMap<&str, &str> = BTreeMap::new();
    for (spanish, english) in en::tables().iter().flat_map(|t| t.iter().copied()) {
        if table.insert(spanish, english).is_some() {
            problems.push(format!("traducción repetida: {spanish:?}"));
        }
        if english.trim().is_empty() {
            problems.push(format!("traducción vacía: {spanish:?}"));
        }
        if holes(spanish) != holes(english) {
            problems.push(format!(
                "los huecos no coinciden: {spanish:?} → {english:?}"
            ));
        }
    }
    // Todo lo que el código pide traducir está en las tablas…
    for (text, place) in &code {
        if !table.contains_key(text.as_str()) {
            problems.push(format!("sin traducir ({place}): {text:?}"));
        }
    }
    // …y en las tablas no queda nada que el código ya no use.
    for spanish in table.keys() {
        if !code.contains_key(*spanish) {
            problems.push(format!("traducción que ya no se usa: {spanish:?}"));
        }
    }
    assert!(
        problems.is_empty(),
        "{} problema(s) con las traducciones:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

#[test]
fn literals_are_read_like_the_compiler_does() {
    assert_eq!(string_literal(r#""hola" resto"#).as_deref(), Some("hola"));
    assert_eq!(
        string_literal(r#""con \"comillas\" y \\ barra\n""#).as_deref(),
        Some("con \"comillas\" y \\ barra\n")
    );
    assert_eq!(
        string_literal("\"una \\\n        sola línea\"").as_deref(),
        Some("una sola línea")
    );
    assert_eq!(
        string_literal(r#""caf\u{e9} \x41""#).as_deref(),
        Some("café A")
    );
    assert_eq!(string_literal("sin comillas"), None);
    assert_eq!(string_literal("\"sin cerrar"), None);
    assert_eq!(
        holes("{a} y {b}, {{no}} y {a}"),
        BTreeSet::from(["a".to_string(), "b".to_string()])
    );
}
