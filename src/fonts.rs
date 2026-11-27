//! Tipos de letra: los que hay instalados en el equipo y cómo se cargan para
//! la interfaz y el terminal.
//!
//! Un tema guarda el nombre de la familia (o la ruta de un fichero de
//! fuente). Si en este equipo no existe, se usa la letra integrada.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use skrifa::MetadataProvider as _;
use skrifa::raw::{FileRef, TableProvider as _};
use skrifa::string::StringId;

/// Un tipo de letra instalado (su variante normal).
#[derive(Debug, Clone, PartialEq)]
pub struct SystemFont {
    /// Nombre de la familia: «DejaVu Sans Mono».
    pub family: String,
    pub path: PathBuf,
    /// Posición dentro del fichero (las colecciones `.ttc` traen varias).
    pub index: u32,
    /// Todas las letras tienen el mismo ancho (apta para el terminal).
    pub monospace: bool,
}

/// Carpetas donde el sistema guarda sus tipos de letra.
fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![];
    if cfg!(windows) {
        if let Some(root) = std::env::var_os("WINDIR") {
            dirs.push(PathBuf::from(root).join("Fonts"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Microsoft/Windows/Fonts"));
        }
    } else if cfg!(target_os = "macos") {
        dirs.push("/System/Library/Fonts".into());
        dirs.push("/Library/Fonts".into());
        dirs.push(crate::paths::home_dir().join("Library/Fonts"));
    } else {
        dirs.push("/usr/share/fonts".into());
        dirs.push("/usr/local/share/fonts".into());
        dirs.push(crate::paths::home_dir().join(".fonts"));
        dirs.push(crate::paths::home_dir().join(".local/share/fonts"));
    }
    dirs
}

/// Ficheros de fuente de una carpeta y sus subcarpetas.
fn font_files(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    if depth > 5 || out.len() > 5000 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            font_files(&path, depth + 1, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_lowercase().as_str(), "ttf" | "otf" | "ttc"))
        {
            out.push(path);
        }
    }
}

/// Lo que interesa de una cara de un fichero de fuente.
struct Face {
    family: String,
    index: u32,
    monospace: bool,
    /// 0 para la variante normal; mayor cuanto más se aleja (negrita,
    /// cursiva, condensada…).
    oddness: u32,
}

fn faces(data: &[u8]) -> Vec<Face> {
    let Ok(file) = FileRef::new(data) else {
        return vec![];
    };
    file.fonts()
        .enumerate()
        .filter_map(|(index, font)| {
            let font = font.ok()?;
            // Sin letras latinas (emojis, símbolos) no sirve para escribir.
            let charmap = font.charmap();
            if charmap.map('a').is_none() || charmap.map('M').is_none() {
                return None;
            }
            // El nombre tipográfico agrupa las variantes («JetBrains Mono
            // Thin» es de la familia «JetBrains Mono»).
            let family = font
                .localized_strings(StringId::TYPOGRAPHIC_FAMILY_NAME)
                .english_or_first()
                .or_else(|| {
                    font.localized_strings(StringId::FAMILY_NAME)
                        .english_or_first()
                })?
                .to_string();
            let family = family.trim().to_string();
            // Las fuentes ocultas del sistema empiezan por punto.
            if family.is_empty() || family.starts_with('.') {
                return None;
            }
            let attributes = font.attributes();
            let weight = attributes.weight.value();
            let oddness = (weight - 400.0).abs() as u32
                + if attributes.style == skrifa::attribute::Style::Normal {
                    0
                } else {
                    1000
                }
                + ((attributes.stretch.ratio() - 1.0).abs() * 1000.0) as u32;
            Some(Face {
                family,
                index: index as u32,
                monospace: font.post().is_ok_and(|post| post.is_fixed_pitch() != 0),
                oddness,
            })
        })
        .collect()
}

/// Recorre las carpetas dadas y devuelve una entrada por familia (su
/// variante más normal), en orden alfabético.
pub fn scan(dirs: &[PathBuf]) -> Vec<SystemFont> {
    let mut files = vec![];
    for dir in dirs {
        font_files(dir, 0, &mut files);
    }
    files.sort();
    let mut best: BTreeMap<String, (u32, SystemFont)> = BTreeMap::new();
    for path in files {
        // Las colecciones CJK pesan decenas de megas: no compensa leerlas
        // enteras solo para saber cómo se llaman.
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 48 * 1024 * 1024) {
            continue;
        }
        let Ok(data) = std::fs::read(&path) else {
            continue;
        };
        for face in faces(&data) {
            let key = face.family.to_lowercase();
            if best
                .get(&key)
                .is_none_or(|(oddness, _)| face.oddness < *oddness)
            {
                best.insert(
                    key,
                    (
                        face.oddness,
                        SystemFont {
                            family: face.family,
                            path: path.clone(),
                            index: face.index,
                            monospace: face.monospace,
                        },
                    ),
                );
            }
        }
    }
    best.into_values().map(|(_, font)| font).collect()
}

/// Tipos de letra del sistema. La primera vez se buscan en segundo plano;
/// devuelve `None` mientras la búsqueda no ha terminado.
pub fn system_fonts() -> Option<Arc<Vec<SystemFont>>> {
    static FONTS: OnceLock<Mutex<Option<Arc<Vec<SystemFont>>>>> = OnceLock::new();
    static STARTED: OnceLock<()> = OnceLock::new();
    let slot = FONTS.get_or_init(|| Mutex::new(None));
    STARTED.get_or_init(|| {
        std::thread::spawn(move || {
            let found = Arc::new(scan(&font_dirs()));
            *slot.lock().unwrap() = Some(found);
        });
    });
    slot.lock().unwrap().clone()
}

/// Como [`system_fonts`], pero espera a que la búsqueda termine.
fn system_fonts_blocking() -> Arc<Vec<SystemFont>> {
    loop {
        if let Some(fonts) = system_fonts() {
            return fonts;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Datos de una fuente lista para cargar.
#[derive(Clone)]
pub struct LoadedFont {
    pub data: Arc<Vec<u8>>,
    pub index: u32,
}

/// Localiza el tipo de letra que pide un tema: una familia instalada o la
/// ruta de un fichero. `None` si no se indica ninguno; `Err` si no existe.
pub fn resolve(choice: &str) -> Result<Option<LoadedFont>, String> {
    let choice = choice.trim();
    if choice.is_empty() {
        return Ok(None);
    }
    let read = |path: &Path, index: u32| -> Result<Option<LoadedFont>, String> {
        let data =
            std::fs::read(path).map_err(|e| format!("No se pudo leer {}: {e}", path.display()))?;
        if faces(&data).is_empty() {
            return Err(format!("{} no es un tipo de letra válido", path.display()));
        }
        Ok(Some(LoadedFont {
            data: Arc::new(data),
            index,
        }))
    };
    // Una ruta (tiene separadores o extensión de fuente).
    let as_path = crate::paths::expand_tilde(choice);
    if choice.contains(['/', '\\']) || as_path.is_file() {
        return read(&as_path, 0);
    }
    let fonts = system_fonts_blocking();
    match fonts.iter().find(|f| f.family.eq_ignore_ascii_case(choice)) {
        Some(font) => read(&font.path, font.index),
        None => Err(format!(
            "El tipo de letra «{choice}» no está instalado en este equipo"
        )),
    }
}

/// Carga en la interfaz los tipos de letra elegidos (vacío = el integrado).
/// Devuelve los avisos de los que no se pudieron cargar.
pub fn apply(ctx: &egui::Context, ui_font: &str, terminal_font: &str) -> Vec<String> {
    // Recargar las fuentes es caro: solo si la elección ha cambiado.
    static APPLIED: Mutex<Option<(String, String)>> = Mutex::new(None);
    static WARNINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let wanted = (ui_font.trim().to_string(), terminal_font.trim().to_string());
    let mut applied = APPLIED.lock().unwrap();
    if applied.as_ref() == Some(&wanted) {
        return WARNINGS.lock().unwrap().clone();
    }
    let mut warnings = vec![];
    let mut definitions = egui::FontDefinitions::default();
    for (choice, family, key) in [
        (&wanted.0, egui::FontFamily::Proportional, "tema-interfaz"),
        (&wanted.1, egui::FontFamily::Monospace, "tema-terminal"),
    ] {
        match resolve(choice) {
            Ok(Some(font)) => {
                let mut data = egui::FontData::from_owned(font.data.as_ref().clone());
                data.index = font.index;
                definitions.font_data.insert(key.into(), Arc::new(data));
                // Primero la elegida; las integradas quedan de reserva para
                // los símbolos que le falten.
                definitions
                    .families
                    .entry(family)
                    .or_default()
                    .insert(0, key.into());
            }
            Ok(None) => {}
            Err(e) => warnings.push(e),
        }
    }
    ctx.set_fonts(definitions);
    *applied = Some(wanted);
    *WARNINGS.lock().unwrap() = warnings.clone();
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_fonts_are_listed_by_family() {
        let fonts = scan(&font_dirs());
        if fonts.is_empty() {
            eprintln!("OMITIDA: este equipo no tiene tipos de letra instalados");
            return;
        }
        // Una entrada por familia, en orden, y con su fichero.
        let mut names: Vec<String> = fonts.iter().map(|f| f.family.to_lowercase()).collect();
        let sorted = {
            let mut s = names.clone();
            s.sort();
            s.dedup();
            s
        };
        names.dedup();
        assert_eq!(names, sorted);
        assert!(
            fonts
                .iter()
                .all(|f| f.path.is_file() && !f.family.is_empty())
        );
        // Las que se llaman «Mono» se reconocen como de ancho fijo.
        if let Some(mono) = fonts.iter().find(|f| f.family == "DejaVu Sans Mono") {
            assert!(mono.monospace);
            // Y se elige la variante normal, no la negrita ni la cursiva.
            let name = mono
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_lowercase();
            assert!(
                !name.contains("bold") && !name.contains("oblique"),
                "{name}"
            );
        }
        if let Some(sans) = fonts.iter().find(|f| f.family == "DejaVu Sans") {
            assert!(!sans.monospace);
        }
        // Las variantes de peso no aparecen como familias aparte, ni las
        // fuentes que no tienen letras.
        assert!(
            !fonts
                .iter()
                .any(|f| f.family.ends_with(" Thin") || f.family.ends_with(" ExtraBold")),
            "{:?}",
            fonts.iter().map(|f| &f.family).collect::<Vec<_>>()
        );
        assert!(!fonts.iter().any(|f| f.family.contains("Emoji")));

        // Se localiza por nombre (sin distinguir mayúsculas) o por ruta.
        let first = &fonts[0];
        assert!(resolve(&first.family.to_uppercase()).unwrap().is_some());
        assert!(
            resolve(&first.path.display().to_string())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn missing_fonts_are_reported() {
        assert!(resolve("").unwrap().is_none());
        assert!(resolve("   ").unwrap().is_none());
        let e = resolve("Tipografía Que No Existe 42").err().unwrap();
        assert!(e.contains("no está instalado"), "{e}");
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("falsa.ttf");
        std::fs::write(&fake, b"esto no es una fuente").unwrap();
        let e = resolve(&fake.display().to_string()).err().unwrap();
        assert!(e.contains("no es un tipo de letra válido"), "{e}");
        assert!(resolve("/no/existe/fuente.ttf").is_err());
    }
}
