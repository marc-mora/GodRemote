//! Registro de una sesión de terminal en un fichero de texto.
//!
//! Se guarda lo que el terminal va mostrando, sin secuencias de escape ni
//! colores: los retrocesos y los retornos de carro se aplican sobre la
//! línea en curso, así que lo tecleado y las barras de progreso quedan como
//! se vieron al final, no carácter a carácter.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Preferencias del registro de sesiones.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogSettings {
    /// Registrar todas las sesiones de terminal sin pedirlo.
    #[serde(rename = "siempre")]
    pub always: bool,
    /// Carpeta de los registros; vacía = la de GodRemote.
    #[serde(rename = "carpeta")]
    pub folder: String,
    /// Anteponer la hora a cada línea.
    #[serde(rename = "horas")]
    pub timestamps: bool,
    /// Grabar además todas las sesiones con sus tiempos (asciicast,
    /// `.cast`), en la misma carpeta, para poder reproducirlas.
    #[serde(rename = "grabar_siempre")]
    pub record_always: bool,
}

impl LogSettings {
    /// Carpeta donde se escriben los registros.
    pub fn dir(&self) -> PathBuf {
        if self.folder.trim().is_empty() {
            crate::paths::logs_dir()
        } else {
            crate::paths::expand_tilde(&self.folder)
        }
    }
}

/// Estado del filtro que quita las secuencias de escape.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Escape {
    None,
    /// Se ha visto ESC.
    Start,
    /// Dentro de `ESC [ … letra`.
    Csi,
    /// Dentro de `ESC ] … BEL` o `ESC ] … ESC \` (títulos, enlaces…).
    Osc,
    /// Dentro de OSC, tras un ESC (posible terminador `ESC \`).
    OscEnd,
    /// `ESC (` y similares: falta un carácter más.
    Charset,
}

/// Una línea que no termina nunca (un fichero binario, un servidor
/// malintencionado) se corta al llegar a este tamaño para no acumularla
/// entera en memoria.
const MAX_LINE_CHARS: usize = 16 * 1024;

/// Convierte la salida de un terminal en líneas de texto legibles.
#[derive(Debug)]
pub struct Plain {
    escape: Escape,
    /// Línea en curso.
    line: Vec<char>,
    /// Posición del cursor dentro de la línea.
    column: usize,
    /// Bytes de un carácter UTF-8 que aún no ha llegado entero.
    partial: Vec<u8>,
}

impl Default for Plain {
    fn default() -> Self {
        Self {
            escape: Escape::None,
            line: vec![],
            column: 0,
            partial: vec![],
        }
    }
}

impl Plain {
    /// Procesa más salida y devuelve las líneas que se han completado.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut data = std::mem::take(&mut self.partial);
        data.extend_from_slice(bytes);
        let mut lines = vec![];
        let mut rest = data.as_slice();
        loop {
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    self.text(text, &mut lines);
                    break;
                }
                Err(e) => {
                    let (valid, after) = rest.split_at(e.valid_up_to());
                    self.text(std::str::from_utf8(valid).unwrap_or_default(), &mut lines);
                    match e.error_len() {
                        // Bytes que no son UTF-8: se sustituyen.
                        Some(len) => {
                            self.put('\u{fffd}');
                            rest = &after[len..];
                        }
                        // Carácter cortado: se espera al resto.
                        None => {
                            self.partial = after.to_vec();
                            break;
                        }
                    }
                }
            }
        }
        lines
    }

    /// La línea en curso, tal como va: lo escrito desde el último salto.
    pub fn pending(&self) -> String {
        self.line.iter().collect()
    }

    /// Devuelve la línea que quedara a medias (al cerrar el registro).
    pub fn finish(&mut self) -> Option<String> {
        let line: String = std::mem::take(&mut self.line).into_iter().collect();
        self.column = 0;
        let line = line.trim_end().to_string();
        (!line.is_empty()).then_some(line)
    }

    fn put(&mut self, c: char) {
        if self.column < self.line.len() {
            self.line[self.column] = c;
        } else {
            self.line.push(c);
        }
        self.column += 1;
    }

    fn text(&mut self, text: &str, lines: &mut Vec<String>) {
        for c in text.chars() {
            match self.escape {
                Escape::Start => {
                    self.escape = match c {
                        '[' => Escape::Csi,
                        ']' | 'P' | '_' | '^' | 'X' => Escape::Osc,
                        '(' | ')' | '*' | '+' | '#' | '%' => Escape::Charset,
                        _ => Escape::None,
                    };
                    continue;
                }
                Escape::Csi => {
                    // Termina con una letra o símbolo final (@ … ~).
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        self.escape = Escape::None;
                        // Borrar hasta el final de la línea.
                        if c == 'K' {
                            self.line.truncate(self.column);
                        }
                    }
                    continue;
                }
                Escape::Osc => {
                    self.escape = match c {
                        '\u{7}' => Escape::None,
                        '\u{1b}' => Escape::OscEnd,
                        _ => Escape::Osc,
                    };
                    continue;
                }
                Escape::OscEnd => {
                    self.escape = if c == '\\' { Escape::None } else { Escape::Osc };
                    continue;
                }
                Escape::Charset => {
                    self.escape = Escape::None;
                    continue;
                }
                Escape::None => {}
            }
            match c {
                '\u{1b}' => self.escape = Escape::Start,
                '\n' => {
                    let line: String = std::mem::take(&mut self.line).into_iter().collect();
                    lines.push(line.trim_end().to_string());
                    self.column = 0;
                }
                '\r' => self.column = 0,
                '\u{8}' => self.column = self.column.saturating_sub(1),
                '\t' => {
                    let next = (self.column / 8 + 1) * 8;
                    while self.column < next {
                        if self.column >= self.line.len() {
                            self.line.push(' ');
                        }
                        self.column += 1;
                    }
                }
                c if c.is_control() => {}
                c => self.put(c),
            }
            if self.line.len() >= MAX_LINE_CHARS {
                let line: String = std::mem::take(&mut self.line).into_iter().collect();
                lines.push(line.trim_end().to_string());
                self.column = 0;
            }
        }
    }
}

/// Registro abierto de una sesión.
pub struct SessionLog {
    path: PathBuf,
    file: std::io::BufWriter<std::fs::File>,
    plain: Plain,
    timestamps: bool,
    /// Primer error de escritura (disco lleno, carpeta borrada…).
    pub error: Option<String>,
}

/// Nombre de fichero seguro a partir del nombre de una sesión.
pub(crate) fn file_stem(name: &str) -> String {
    let stem: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '@') {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    if stem.trim_matches('_').is_empty() {
        "sesion".into()
    } else {
        stem
    }
}

/// Crea en `dir` un fichero nuevo para una sesión, con la fecha en el nombre
/// (`sesion_20260131-101500.ext`) y, en Unix, legible solo por el usuario:
/// lo que muestra un terminal puede incluir datos que nadie más debe ver.
/// Nunca reutiliza un fichero que ya exista.
pub(crate) fn create_session_file(
    dir: &Path,
    session: &str,
    extension: &str,
) -> Result<(PathBuf, std::fs::File)> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("No se pudo crear la carpeta {}", dir.display()))?;
    let now = chrono::Local::now();
    let base = format!("{}_{}", file_stem(session), now.format("%Y%m%d-%H%M%S"));
    let mut last_error = None;
    // Dos sesiones del mismo equipo abiertas en el mismo segundo.
    for n in 0..100 {
        let path = if n == 0 {
            dir.join(format!("{base}.{extension}"))
        } else {
            dir.join(format!("{base}-{n}.{extension}"))
        };
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last_error = Some(e),
            Err(e) => {
                return Err(e).with_context(|| format!("No se pudo crear {}", path.display()));
            }
        }
    }
    Err(anyhow::anyhow!(
        "No se pudo crear un fichero para «{session}» en {}: {}",
        dir.display(),
        last_error.map(|e| e.to_string()).unwrap_or_default()
    ))
}

impl SessionLog {
    /// Crea el fichero de registro de una sesión en la carpeta indicada.
    pub fn start(dir: &Path, session: &str, target: &str, timestamps: bool) -> Result<Self> {
        let now = chrono::Local::now();
        let (path, file) = create_session_file(dir, session, "log")?;
        let mut log = Self {
            path,
            file: std::io::BufWriter::new(file),
            plain: Plain::default(),
            timestamps,
            error: None,
        };
        let header = if target.is_empty() {
            format!(
                "# GodRemote · {session} · {}",
                now.format("%Y-%m-%d %H:%M:%S")
            )
        } else {
            format!(
                "# GodRemote · {session} · {target} · {}",
                now.format("%Y-%m-%d %H:%M:%S")
            )
        };
        log.write_raw(&header);
        log.flush();
        Ok(log)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn write_raw(&mut self, line: &str) {
        if self.error.is_none()
            && let Err(e) = writeln!(self.file, "{line}")
        {
            self.error = Some(format!(
                "No se pudo escribir en {}: {e}",
                self.path.display()
            ));
        }
    }

    fn write_line(&mut self, line: &str) {
        if self.timestamps {
            let stamp = chrono::Local::now().format("[%H:%M:%S] ");
            self.write_raw(&format!("{stamp}{line}"));
        } else {
            self.write_raw(line);
        }
    }

    fn flush(&mut self) {
        if self.error.is_none()
            && let Err(e) = self.file.flush()
        {
            self.error = Some(format!(
                "No se pudo escribir en {}: {e}",
                self.path.display()
            ));
        }
    }

    /// Añade al registro la salida recibida del terminal.
    pub fn feed(&mut self, bytes: &[u8]) {
        for line in self.plain.feed(bytes) {
            self.write_line(&line);
        }
        self.flush();
    }

    /// Anota un mensaje propio (conexión cerrada, etc.).
    pub fn note(&mut self, text: &str) {
        if let Some(line) = self.plain.finish() {
            self.write_line(&line);
        }
        self.write_raw(&format!("# {text}"));
        self.flush();
    }
}

impl Drop for SessionLog {
    fn drop(&mut self) {
        if let Some(line) = self.plain.finish() {
            self.write_line(&line);
        }
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(chunks: &[&[u8]]) -> Vec<String> {
        let mut p = Plain::default();
        let mut lines: Vec<String> = chunks.iter().flat_map(|c| p.feed(c)).collect();
        lines.extend(p.finish());
        lines
    }

    #[test]
    fn escapes_and_colours_are_removed() {
        let lines = plain(&[
            b"\x1b]0;root@web: ~\x07\x1b[01;32mroot@web\x1b[00m:\x1b[01;34m~\x1b[00m# ls\r\n",
            b"\x1b[0m\x1b[01;34mbin\x1b[0m  notas.txt\r\n",
            b"\x1b]8;;http://x\x1b\\enlace\x1b]8;;\x1b\\ \x1b(Bfin\r\n",
        ]);
        assert_eq!(
            lines,
            vec!["root@web:~# ls", "bin  notas.txt", "enlace fin"]
        );
    }

    #[test]
    fn typing_corrections_and_progress_bars_are_resolved() {
        // «sl», dos retrocesos con borrado, «ls».
        let lines = plain(&[b"$ sl\x08\x08\x1b[Kls\r\n"]);
        assert_eq!(lines, vec!["$ ls"]);
        // Una barra de progreso que se reescribe con retornos de carro.
        let lines = plain(&[b"10%\r50%\r100% hecho\r\nlisto\r\n"]);
        assert_eq!(lines, vec!["100% hecho", "listo"]);
        // Tabuladores a columnas de ocho y una línea final sin salto.
        let lines = plain(&[b"a\tb\r\nsin salto"]);
        assert_eq!(lines, vec!["a       b", "sin salto"]);
    }

    #[test]
    fn split_utf8_and_binary_noise() {
        // «ñ» (c3 b1) partida entre dos bloques; un byte suelto no válido.
        let lines = plain(&[b"a\xc3", b"\xb1o \xff fin\n"]);
        assert_eq!(lines, vec!["año \u{fffd} fin"]);
        // Una secuencia de escape partida entre bloques.
        let lines = plain(&[b"\x1b[3", b"1mrojo\x1b", b"[0m\n"]);
        assert_eq!(lines, vec!["rojo"]);
    }

    #[test]
    fn log_files_are_written() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("registros");
        let mut log =
            SessionLog::start(&folder, "web/01: producción", "root@10.0.0.5:22", false).unwrap();
        let path = log.path().to_path_buf();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            name.starts_with("web_01__producción_") && name.ends_with(".log"),
            "{name}"
        );
        log.feed(b"\x1b[32m$\x1b[0m uptime\r\n 16:20 up 3 days\r\n$ ");
        log.note("Sesión finalizada");
        // Un segundo registro del mismo equipo no pisa al primero.
        let other = SessionLog::start(&folder, "web/01: producción", "", true).unwrap();
        assert_ne!(other.path(), path);
        drop(other);
        drop(log);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("# GodRemote · web/01: producción · root@10.0.0.5:22 · "));
        assert_eq!(
            &lines[1..],
            &["$ uptime", " 16:20 up 3 days", "$", "# Sesión finalizada"]
        );
    }

    #[test]
    fn endless_lines_are_cut() {
        // Sin saltos de línea (un binario, un servidor malintencionado) la
        // línea en curso no crece sin límite.
        let mut p = Plain::default();
        let lines = p.feed("x".repeat(MAX_LINE_CHARS * 2 + 10).as_bytes());
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| l.chars().count() == MAX_LINE_CHARS));
        assert_eq!(p.pending(), "x".repeat(10));
        assert_eq!(p.feed(b" fin\r\n"), vec![format!("{} fin", "x".repeat(10))]);
        assert_eq!(p.pending(), "");
    }

    #[test]
    fn session_files_are_private_and_never_reused() {
        let dir = tempfile::tempdir().unwrap();
        let (first, _file) = create_session_file(dir.path(), "web", "log").unwrap();
        let (second, _file) = create_session_file(dir.path(), "web", "log").unwrap();
        assert_ne!(first, second);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [&first, &second] {
                let mode = std::fs::metadata(path).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o600, "{}", path.display());
            }
        }
        // Una carpeta que no se puede crear es un error, no un pánico.
        let blocked = first.join("dentro");
        assert!(create_session_file(&blocked, "web", "log").is_err());
    }

    #[test]
    fn timestamps_and_settings() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = SessionLog::start(dir.path(), "", "", true).unwrap();
        let path = log.path().to_path_buf();
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("sesion_")
        );
        log.feed(b"hola\n");
        drop(log);
        let text = std::fs::read_to_string(&path).unwrap();
        let line = text.lines().nth(1).unwrap();
        // [HH:MM:SS] hola
        assert!(
            line.starts_with('[') && line.ends_with("] hola") && line.len() == 15,
            "{line}"
        );

        let settings: LogSettings =
            serde_json::from_str(r#"{"siempre":true,"carpeta":"/tmp/reg"}"#).unwrap();
        assert!(settings.always && !settings.timestamps && !settings.record_always);
        assert_eq!(settings.dir(), PathBuf::from("/tmp/reg"));
        assert_eq!(LogSettings::default().dir(), crate::paths::logs_dir());
    }
}
