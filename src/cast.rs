//! Grabación y reproducción de sesiones de terminal en formato asciicast
//! (`.cast`, el de asciinema): una cabecera JSON y, después, una línea por
//! evento con su tiempo, `[1.25, "o", "texto"]`.
//!
//! Se graba en la versión 2 del formato. Solo se guarda lo que el terminal
//! muestra (eventos `o`) y sus cambios de tamaño (`r`): lo que se teclea no
//! se graba, así que una contraseña escrita sin eco no queda en el fichero.
//!
//! Para reproducir se leen las versiones 2 y 3 (la de asciinema 3, con
//! tiempos relativos). Un `.cast` que se abre es entrada no fiable: puede
//! venir de cualquier parte, así que se acotan su tamaño, sus dimensiones y
//! sus tiempos, y las líneas ilegibles se saltan.

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};

/// Extensión de las grabaciones.
pub const EXTENSION: &str = "cast";

/// Tamaño a partir del cual una grabación en curso se detiene sola.
const MAX_RECORDING_BYTES: u64 = 1024 * 1024 * 1024;
/// Fichero más grande que se abre para reproducir.
pub const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
/// Línea (un evento) más larga que se lee; las mayores se saltan.
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
/// Eventos que como mucho se guardan en memoria.
const MAX_EVENTS: usize = 2_000_000;
/// Silencio más largo que se respeta entre dos eventos, en segundos: un
/// tiempo disparatado no deja la barra de posición inservible.
pub const MAX_GAP: f64 = 3600.0;
/// Límites del tamaño del terminal, los mismos que usa el de verdad.
const MIN_COLS: u64 = 2;
const MAX_COLS: u64 = 500;
const MIN_ROWS: u64 = 1;
const MAX_ROWS: u64 = 300;
const DEFAULT_SIZE: (u16, u16) = (80, 24);

/// Grabación en curso de un terminal.
pub struct Recorder {
    path: PathBuf,
    file: std::io::BufWriter<std::fs::File>,
    started: Instant,
    /// Bytes de un carácter UTF-8 que aún no ha llegado entero.
    partial: Vec<u8>,
    written: u64,
    /// Primer problema al escribir (disco lleno, carpeta borrada, tamaño
    /// máximo…): la grabación se da por terminada.
    pub error: Option<String>,
}

impl Recorder {
    /// Empieza a grabar en un fichero nuevo de la carpeta indicada.
    pub fn start(dir: &Path, session: &str, cols: u16, rows: u16) -> Result<Self> {
        let (path, file) = crate::session_log::create_session_file(dir, session, EXTENSION)?;
        let mut recorder = Self {
            path,
            file: std::io::BufWriter::new(file),
            started: Instant::now(),
            partial: vec![],
            written: 0,
            error: None,
        };
        let header = serde_json::json!({
            "version": 2,
            "width": cols,
            "height": rows,
            "timestamp": chrono::Utc::now().timestamp(),
            "title": session,
            "env": { "TERM": "xterm-256color" },
        });
        recorder.write_line(&header.to_string());
        match recorder.error.take() {
            Some(error) => Err(anyhow!(error)),
            None => Ok(recorder),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn write_line(&mut self, line: &str) {
        if self.error.is_some() {
            return;
        }
        if self.written + line.len() as u64 > MAX_RECORDING_BYTES {
            self.error = Some(format!(
                "La grabación {} ha llegado al tamaño máximo ({} MB) y se ha detenido",
                self.path.display(),
                MAX_RECORDING_BYTES / (1024 * 1024)
            ));
            return;
        }
        let result = writeln!(self.file, "{line}").and_then(|()| self.file.flush());
        match result {
            Ok(()) => self.written += line.len() as u64 + 1,
            Err(e) => {
                self.error = Some(format!(
                    "No se pudo escribir en {}: {e}",
                    self.path.display()
                ));
            }
        }
    }

    fn event(&mut self, time: f64, code: &str, data: &str) {
        // El texto va como cadena JSON: los caracteres de control, escapados.
        let data = serde_json::Value::String(data.to_string());
        self.write_line(&format!("[{time:.6}, \"{code}\", {data}]"));
    }

    /// Anota salida del terminal, con el tiempo transcurrido desde el
    /// comienzo de la grabación.
    pub fn output(&mut self, bytes: &[u8]) {
        self.output_at(self.started.elapsed().as_secs_f64(), bytes);
    }

    fn output_at(&mut self, time: f64, bytes: &[u8]) {
        let mut data = std::mem::take(&mut self.partial);
        data.extend_from_slice(bytes);
        // El formato es texto: lo que no sea UTF-8 se sustituye, y un
        // carácter cortado entre dos bloques espera a su otra mitad.
        let mut text = String::with_capacity(data.len());
        let mut rest = data.as_slice();
        loop {
            match std::str::from_utf8(rest) {
                Ok(valid) => {
                    text.push_str(valid);
                    break;
                }
                Err(e) => {
                    let (valid, after) = rest.split_at(e.valid_up_to());
                    text.push_str(std::str::from_utf8(valid).unwrap_or_default());
                    match e.error_len() {
                        Some(len) => {
                            text.push('\u{fffd}');
                            rest = &after[len..];
                        }
                        None => {
                            self.partial = after.to_vec();
                            break;
                        }
                    }
                }
            }
        }
        if !text.is_empty() {
            self.event(time, "o", &text);
        }
    }

    /// Anota un cambio de tamaño del terminal.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let time = self.started.elapsed().as_secs_f64();
        self.event(time, "r", &format!("{cols}x{rows}"));
    }
}

/// Lo que hace un evento de una grabación.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    /// Salida: un tramo de [`Cast::data`].
    Output {
        start: usize,
        end: usize,
    },
    Resize(u16, u16),
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Event {
    /// Segundos desde el comienzo.
    time: f64,
    kind: Kind,
}

/// Una grabación leída de un fichero.
#[derive(Debug, Clone, PartialEq)]
pub struct Cast {
    pub cols: u16,
    pub rows: u16,
    pub title: String,
    /// Cuándo se grabó (segundos desde 1970), si lo dice.
    pub timestamp: Option<i64>,
    /// Recorte de silencios que recomienda la propia grabación.
    pub idle_limit: Option<f64>,
    /// Versión del formato (2 o 3).
    pub version: u8,
    /// Líneas que no se han podido leer.
    pub skipped: usize,
    events: Vec<Event>,
    /// La salida de todos los eventos, seguida.
    data: Vec<u8>,
}

/// Columnas y filas de una cabecera o de un evento `r`, dentro de límites.
fn clamp_size(cols: Option<u64>, rows: Option<u64>) -> (u16, u16) {
    match (cols, rows) {
        (Some(cols), Some(rows)) if cols > 0 && rows > 0 => (
            cols.clamp(MIN_COLS, MAX_COLS) as u16,
            rows.clamp(MIN_ROWS, MAX_ROWS) as u16,
        ),
        _ => DEFAULT_SIZE,
    }
}

/// Lee una línea sin cargar en memoria más de `max` bytes de ella. Devuelve
/// `None` al final de los datos y `Some(false)` si la línea era más larga
/// (se ha saltado entera).
fn read_bounded_line(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<Option<bool>> {
    line.clear();
    let mut fits = true;
    let mut any = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(any.then_some(fits));
        }
        any = true;
        let (chunk, found) = match buffer.iter().position(|b| *b == b'\n') {
            Some(at) => (&buffer[..at], true),
            None => (buffer, false),
        };
        if fits && line.len() + chunk.len() <= max {
            line.extend_from_slice(chunk);
        } else {
            fits = false;
            line.clear();
        }
        let used = chunk.len() + usize::from(found);
        reader.consume(used);
        if found {
            return Ok(Some(fits));
        }
    }
}

impl Cast {
    /// Lee una grabación de un fichero.
    pub fn load(path: &Path) -> Result<Self> {
        let meta = std::fs::metadata(path)
            .with_context(|| format!("No se pudo abrir {}", path.display()))?;
        if !meta.is_file() {
            bail!("{} no es un fichero", path.display());
        }
        if meta.len() > MAX_FILE_BYTES {
            bail!(
                "La grabación ocupa {} MB; se abren como mucho {} MB",
                meta.len() / (1024 * 1024),
                MAX_FILE_BYTES / (1024 * 1024)
            );
        }
        let file = std::fs::File::open(path)
            .with_context(|| format!("No se pudo abrir {}", path.display()))?;
        // Aunque el fichero crezca mientras se lee, no se pasa del límite.
        let reader = std::io::BufReader::new(file.take(MAX_FILE_BYTES));
        Self::parse(reader).with_context(|| format!("No se pudo leer {}", path.display()))
    }

    /// Lee una grabación en formato asciicast 2 o 3.
    pub fn parse(mut reader: impl BufRead) -> Result<Self> {
        let mut line = Vec::new();
        // La cabecera: la primera línea que no esté vacía ni sea un
        // comentario.
        let header = loop {
            match read_bounded_line(&mut reader, &mut line, MAX_LINE_BYTES)? {
                None => bail!("El fichero está vacío"),
                Some(false) => {
                    bail!("No es una grabación asciicast: la cabecera es demasiado larga")
                }
                Some(true) => {}
            }
            let text = String::from_utf8_lossy(&line);
            let text = text.trim().trim_start_matches('\u{feff}');
            if text.is_empty() || text.starts_with('#') {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(text)
                .map_err(|_| anyhow!("No es una grabación asciicast: falta la cabecera"))?;
            if !value.is_object() {
                bail!("No es una grabación asciicast: falta la cabecera");
            }
            break value;
        };
        let version = match header.get("version").and_then(|v| v.as_u64()) {
            Some(2) => 2,
            Some(3) => 3,
            Some(1) => bail!("Es una grabación asciicast de la versión 1, que no se admite"),
            Some(other) => bail!("Versión de asciicast desconocida: {other}"),
            None => bail!("No es una grabación asciicast: la cabecera no dice su versión"),
        };
        let number = |value: Option<&serde_json::Value>| value.and_then(|v| v.as_u64());
        let (cols, rows) = if version == 2 {
            clamp_size(number(header.get("width")), number(header.get("height")))
        } else {
            let term = header.get("term");
            clamp_size(
                number(term.and_then(|t| t.get("cols"))),
                number(term.and_then(|t| t.get("rows"))),
            )
        };
        let mut cast = Cast {
            cols,
            rows,
            title: header
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .chars()
                .filter(|c| !c.is_control())
                .take(200)
                .collect(),
            timestamp: header.get("timestamp").and_then(|t| t.as_i64()),
            idle_limit: header
                .get("idle_time_limit")
                .and_then(|l| l.as_f64())
                .filter(|l| l.is_finite() && *l > 0.0)
                .map(|l| l.min(MAX_GAP)),
            version,
            skipped: 0,
            events: vec![],
            data: vec![],
        };

        // Reloj de la reproducción y último tiempo leído del fichero.
        let mut clock = 0.0_f64;
        let mut last_raw = 0.0_f64;
        // Mientras no haya que recortar nada, el reloj es el tiempo del
        // fichero tal cual (sin acumular errores de redondeo).
        let mut verbatim = true;
        loop {
            match read_bounded_line(&mut reader, &mut line, MAX_LINE_BYTES)? {
                None => break,
                Some(false) => {
                    cast.skipped += 1;
                    continue;
                }
                Some(true) => {}
            }
            let Ok(text) = std::str::from_utf8(&line) else {
                cast.skipped += 1;
                continue;
            };
            let text = text.trim();
            if text.is_empty() || text.starts_with('#') {
                continue;
            }
            let Ok(serde_json::Value::Array(fields)) = serde_json::from_str(text) else {
                cast.skipped += 1;
                continue;
            };
            let (Some(raw), Some(code), Some(data)) = (
                fields.first().and_then(|t| t.as_f64()),
                fields.get(1).and_then(|c| c.as_str()),
                fields.get(2).and_then(|d| d.as_str()),
            ) else {
                cast.skipped += 1;
                continue;
            };
            // Tiempo transcurrido desde el evento anterior: en la versión 2
            // los tiempos son absolutos y en la 3, relativos. Nunca negativo
            // (el reloj no retrocede) y nunca disparatado.
            let gap = if version == 3 { raw } else { raw - last_raw };
            if gap.is_finite() && gap > 0.0 {
                if version == 2 {
                    last_raw = raw;
                }
                if version == 2 && verbatim && gap <= MAX_GAP {
                    clock = raw;
                } else {
                    verbatim = false;
                    clock += gap.min(MAX_GAP);
                }
            }
            let kind = match code {
                "o" => {
                    let start = cast.data.len();
                    cast.data.extend_from_slice(data.as_bytes());
                    Kind::Output {
                        start,
                        end: cast.data.len(),
                    }
                }
                "r" => {
                    let mut parts = data.split('x');
                    let size = (
                        parts.next().and_then(|n| n.trim().parse::<u64>().ok()),
                        parts.next().and_then(|n| n.trim().parse::<u64>().ok()),
                    );
                    match size {
                        (Some(cols), Some(rows)) if cols > 0 && rows > 0 => {
                            let (cols, rows) = clamp_size(Some(cols), Some(rows));
                            Kind::Resize(cols, rows)
                        }
                        _ => {
                            cast.skipped += 1;
                            continue;
                        }
                    }
                }
                // Teclas («i»), marcas («m») y el final («x») no se
                // reproducen, pero su tiempo cuenta.
                _ => continue,
            };
            // Salida seguida en el mismo instante: un solo evento.
            if let (
                Kind::Output { end, .. },
                Some(Event {
                    time,
                    kind: Kind::Output { end: last_end, .. },
                }),
            ) = (kind, cast.events.last_mut())
                && *time == clock
            {
                *last_end = end;
                continue;
            }
            if cast.events.len() >= MAX_EVENTS {
                bail!("La grabación tiene demasiados eventos (más de {MAX_EVENTS})");
            }
            cast.events.push(Event { time: clock, kind });
        }
        Ok(cast)
    }

    /// Duración en segundos (el tiempo de su último evento).
    pub fn duration(&self) -> f64 {
        self.events.last().map_or(0.0, |e| e.time)
    }

    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    /// Tiempo de cada evento en la reproducción. Con `idle_limit`, ningún
    /// silencio entre dos eventos dura más que eso.
    fn timeline(&self, idle_limit: Option<f64>) -> Vec<f64> {
        let limit = idle_limit
            .filter(|l| l.is_finite() && *l > 0.0)
            .unwrap_or(f64::INFINITY);
        let mut clock = 0.0;
        let mut previous = 0.0;
        self.events
            .iter()
            .map(|event| {
                clock += (event.time - previous).min(limit);
                previous = event.time;
                clock
            })
            .collect()
    }
}

/// Algo que el reproductor pide aplicar al terminal en el que se ve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step<'a> {
    Output(&'a [u8]),
    Resize(u16, u16),
}

/// Velocidades que ofrece el reproductor.
pub const SPEEDS: [f32; 5] = [0.5, 1.0, 2.0, 4.0, 8.0];

/// Reproducción de una grabación: por dónde va y qué falta por mostrar.
pub struct Player {
    cast: Cast,
    /// Tiempo de cada evento en la reproducción.
    times: Vec<f64>,
    idle_limit: Option<f64>,
    /// Posición, en segundos de reproducción.
    position: f64,
    /// Primer evento que aún no se ha mostrado.
    next: usize,
    pub playing: bool,
    pub speed: f32,
}

impl Player {
    pub fn new(cast: Cast) -> Self {
        let times = cast.timeline(None);
        Self {
            cast,
            times,
            idle_limit: None,
            position: 0.0,
            next: 0,
            playing: false,
            speed: 1.0,
        }
    }

    pub fn cast(&self) -> &Cast {
        &self.cast
    }

    /// Duración de la reproducción (más corta si se recortan los silencios).
    pub fn duration(&self) -> f64 {
        self.times.last().copied().unwrap_or(0.0)
    }

    pub fn position(&self) -> f64 {
        self.position
    }

    /// Ya se ha mostrado todo.
    pub fn is_finished(&self) -> bool {
        self.next >= self.times.len()
    }

    pub fn idle_limit(&self) -> Option<f64> {
        self.idle_limit
    }

    /// Recorta (o deja de recortar) los silencios largos, sin moverse del
    /// punto de la grabación en el que se está.
    pub fn set_idle_limit(&mut self, limit: Option<f64>) {
        let limit = limit.filter(|l| l.is_finite() && *l > 0.0);
        if limit == self.idle_limit {
            return;
        }
        let times = self.cast.timeline(limit);
        // La posición se mide desde el último evento mostrado.
        self.position = match self.next.checked_sub(1) {
            None => self.position.min(times.first().copied().unwrap_or(0.0)),
            Some(shown) => {
                let since = (self.position - self.times[shown]).max(0.0);
                let room = times.get(self.next).map_or(0.0, |next| next - times[shown]);
                times[shown] + since.min(room)
            }
        };
        self.times = times;
        self.idle_limit = limit;
    }

    /// Avanza el reloj `seconds` segundos de reloj real.
    pub fn advance(&mut self, seconds: f64) {
        if !self.playing || !seconds.is_finite() || seconds <= 0.0 {
            return;
        }
        let speed = f64::from(self.speed.clamp(0.1, 64.0));
        self.position = (self.position + seconds * speed).min(self.duration());
    }

    /// Va a un punto de la reproducción. Devuelve `true` si hay que
    /// empezar de cero (se ha vuelto atrás): el terminal debe vaciarse y
    /// ponerse al tamaño inicial antes de seguir aplicando eventos.
    pub fn seek(&mut self, position: f64) -> bool {
        let position = if position.is_finite() {
            position.clamp(0.0, self.duration())
        } else {
            0.0
        };
        self.position = position;
        let back = self
            .next
            .checked_sub(1)
            .is_some_and(|shown| self.times[shown] > position);
        if back {
            self.next = 0;
        }
        back
    }

    /// Entrega los eventos que tocan hasta la posición actual, sin pasar de
    /// `budget` bytes de salida por llamada (para no bloquear la interfaz
    /// al saltar lejos). Devuelve `true` si quedan eventos atrasados.
    pub fn drain(&mut self, budget: usize, mut apply: impl FnMut(Step<'_>)) -> bool {
        let mut used = 0;
        while let Some(time) = self.times.get(self.next) {
            if *time > self.position {
                break;
            }
            if used >= budget {
                return true;
            }
            match self.cast.events[self.next].kind {
                Kind::Output { start, end } => {
                    used += end - start;
                    apply(Step::Output(&self.cast.data[start..end]));
                }
                Kind::Resize(cols, rows) => apply(Step::Resize(cols, rows)),
            }
            self.next += 1;
        }
        if self.is_finished() && self.position >= self.duration() {
            self.playing = false;
        }
        false
    }

    /// Segundos de reloj real hasta el próximo evento, a la velocidad
    /// actual; `None` si no queda ninguno o está en pausa.
    pub fn next_event_in(&self) -> Option<f64> {
        if !self.playing {
            return None;
        }
        let next = self.times.get(self.next)?;
        Some((next - self.position).max(0.0) / f64::from(self.speed.clamp(0.1, 64.0)))
    }
}

/// «1:05» o «1:02:03».
pub fn clock_text(seconds: f64) -> String {
    let total = if seconds.is_finite() && seconds > 0.0 {
        seconds as u64
    } else {
        0
    };
    let (hours, minutes, secs) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Cast> {
        Cast::parse(std::io::Cursor::new(text.as_bytes().to_vec()))
    }

    /// Reproduce entera una grabación y devuelve lo que entrega.
    fn replay(cast: &Cast) -> Vec<(f64, String)> {
        cast.events
            .iter()
            .map(|event| {
                let what = match event.kind {
                    Kind::Output { start, end } => {
                        String::from_utf8_lossy(&cast.data[start..end]).into_owned()
                    }
                    Kind::Resize(cols, rows) => format!("<{cols}x{rows}>"),
                };
                (event.time, what)
            })
            .collect()
    }

    #[test]
    fn recordings_are_written_as_asciicast_v2() {
        let dir = tempfile::tempdir().unwrap();
        let mut recorder = Recorder::start(dir.path(), "web/01", 100, 30).unwrap();
        let path = recorder.path().to_path_buf();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            name.starts_with("web_01_") && name.ends_with(".cast"),
            "{name}"
        );
        recorder.output_at(0.5, b"\x1b[32m$\x1b[0m ls\r\n");
        // «ñ» partida entre dos bloques y un byte que no es UTF-8.
        recorder.output_at(1.25, b"a\xc3");
        recorder.output_at(1.5, b"\xb1o \xff \"comillas\" \\ fin\n");
        recorder.resize(120, 40);
        recorder.output_at(3.0, "日本語 ✓".as_bytes());
        assert!(recorder.error.is_none());
        drop(recorder);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 6);
        // Cabecera: un objeto JSON en una línea.
        let header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(header["version"], 2);
        assert_eq!(header["width"], 100);
        assert_eq!(header["height"], 30);
        assert_eq!(header["title"], "web/01");
        assert_eq!(header["env"]["TERM"], "xterm-256color");
        assert!(header["timestamp"].as_i64().unwrap() > 1_600_000_000);
        // Eventos: [tiempo, código, datos], todo JSON válido.
        assert_eq!(
            lines[1],
            r#"[0.500000, "o", "\u001b[32m$\u001b[0m ls\r\n"]"#
        );
        assert_eq!(lines[2], r#"[1.250000, "o", "a"]"#);
        assert_eq!(
            lines[3],
            "[1.500000, \"o\", \"ño \u{fffd} \\\"comillas\\\" \\\\ fin\\n\"]"
        );
        let resize: serde_json::Value = serde_json::from_str(lines[4]).unwrap();
        assert_eq!(resize[1], "r");
        assert_eq!(resize[2], "120x40");
        assert_eq!(lines[5], r#"[3.000000, "o", "日本語 ✓"]"#);

        // Y se vuelve a leer tal cual.
        let cast = Cast::load(&path).unwrap();
        assert_eq!((cast.cols, cast.rows, cast.version), (100, 30, 2));
        assert_eq!(cast.title, "web/01");
        assert_eq!(cast.skipped, 0);
        let events = replay(&cast);
        assert_eq!(events.len(), 5);
        assert_eq!(events[0], (0.5, "\x1b[32m$\x1b[0m ls\r\n".to_string()));
        assert_eq!(events[2].1, "ño \u{fffd} \"comillas\" \\ fin\n");
        assert_eq!(events[3].1, "<120x40>");
        assert_eq!(cast.duration(), 3.0);
    }

    #[test]
    fn asciinema_files_are_read() {
        // Versión 2, como la escribe asciinema 2: tiempos absolutos.
        let v2 = concat!(
            "{\"version\": 2, \"width\": 80, \"height\": 24, \"timestamp\": 1504467315, ",
            "\"title\": \"Demo\", \"idle_time_limit\": 2.5, \"env\": {\"TERM\": \"xterm\"}}\n",
            "[0.248848, \"o\", \"\\u001b[1;31mHola \\u001b[32mmundo\\u001b[0m\\r\\n\"]\n",
            "[1.001376, \"o\", \"Eso es todo\"]\n",
            "[1.5, \"i\", \"q\"]\n",
            "[2.0, \"m\", \"capítulo\"]\n",
            "[2.143733, \"r\", \"100x50\"]\n",
            "[4.0, \"o\", \"\\r\\nfin\"]\n",
        );
        let cast = parse(v2).unwrap();
        assert_eq!((cast.cols, cast.rows, cast.version), (80, 24, 2));
        assert_eq!(cast.title, "Demo");
        assert_eq!(cast.timestamp, Some(1504467315));
        assert_eq!(cast.idle_limit, Some(2.5));
        assert_eq!(
            replay(&cast),
            [
                (
                    0.248848,
                    "\x1b[1;31mHola \x1b[32mmundo\x1b[0m\r\n".to_string()
                ),
                (1.001376, "Eso es todo".to_string()),
                (2.143733, "<100x50>".to_string()),
                (4.0, "\r\nfin".to_string()),
            ]
        );

        // Versión 3 (asciinema 3): tiempos relativos, comentarios y el
        // tamaño dentro de «term».
        let v3 = concat!(
            "{\"version\": 3, \"term\": {\"cols\": 120, \"rows\": 40, \"type\": \"xterm\"}, ",
            "\"timestamp\": 1700000000}\n",
            "# un comentario\n",
            "[0.5, \"o\", \"uno\"]\n",
            "[1.5, \"o\", \"dos\"]\n",
            "\n",
            "[0.25, \"i\", \"x\"]\n",
            "[0.25, \"r\", \"90x30\"]\n",
            "[2.0, \"x\", \"0\"]\n",
        );
        let cast = parse(v3).unwrap();
        assert_eq!((cast.cols, cast.rows, cast.version), (120, 40, 3));
        assert_eq!(
            replay(&cast),
            [
                (0.5, "uno".to_string()),
                (2.0, "dos".to_string()),
                // El tiempo de los eventos que no se reproducen cuenta.
                (2.5, "<90x30>".to_string()),
            ]
        );
        assert_eq!(cast.skipped, 0);
        // Con marca de orden de bytes y finales de línea de Windows.
        let cast =
            parse("\u{feff}{\"version\":2,\"width\":10,\"height\":5}\r\n[1,\"o\",\"a\"]\r\n")
                .unwrap();
        assert_eq!(replay(&cast), [(1.0, "a".to_string())]);
    }

    #[test]
    fn hostile_files_do_not_break_the_reader() {
        // Lo que no es una grabación.
        for bad in [
            "",
            "\n\n",
            "hola",
            "[1, \"o\", \"sin cabecera\"]",
            "{\"width\": 80, \"height\": 24}",
            "{\"version\": 1, \"width\": 80, \"height\": 24, \"stdout\": []}",
            "{\"version\": 99}",
            "{\"version\": \"2\"}",
            "{\"version\": 2",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        // Tamaños absurdos: se acotan o se cambian por 80×24.
        let size = |header: &str| {
            let cast = parse(header).unwrap();
            (cast.cols, cast.rows)
        };
        assert_eq!(size(r#"{"version":2,"width":0,"height":0}"#), (80, 24));
        assert_eq!(size(r#"{"version":2}"#), (80, 24));
        assert_eq!(size(r#"{"version":2,"width":-5,"height":10}"#), (80, 24));
        assert_eq!(
            size(r#"{"version":2,"width":"80","height":[24]}"#),
            (80, 24)
        );
        assert_eq!(size(r#"{"version":2,"width":80.5,"height":24}"#), (80, 24));
        assert_eq!(
            size(r#"{"version":2,"width":4294967296,"height":18446744073709551615}"#),
            (500, 300)
        );
        assert_eq!(size(r#"{"version":2,"width":1,"height":1}"#), (2, 1));
        assert_eq!(
            size(r#"{"version":3,"term":{"cols":100000,"rows":3}}"#),
            (500, 3)
        );
        assert_eq!(size(r#"{"version":3,"term":"vt100"}"#), (80, 24));
        // Título con caracteres de control o enorme.
        let cast = parse(&format!(
            "{{\"version\":2,\"title\":\"a\\u001b[31m\\nb{}\"}}",
            "x".repeat(5000)
        ))
        .unwrap();
        assert!(cast.title.starts_with("a[31mb") && cast.title.chars().count() == 200);

        // Eventos mal formados: se saltan y se cuentan.
        let cast = parse(concat!(
            "{\"version\":2,\"width\":80,\"height\":24}\n",
            "[0.5, \"o\", \"bien\"]\n",
            "esto no es JSON\n",
            "{\"un\": \"objeto\"}\n",
            "[]\n",
            "[1.0]\n",
            "[1.0, \"o\"]\n",
            "[\"1.0\", \"o\", \"tiempo que no es número\"]\n",
            "[1.0, 7, \"código que no es texto\"]\n",
            "[1.0, \"o\", 42]\n",
            "[1.0, \"o\", null]\n",
            "[1.0, \"o\", \"cortado\n",
            "[1.5, \"r\", \"enorme\"]\n",
            "[1.5, \"r\", \"0x0\"]\n",
            "[1.5, \"r\", \"80\"]\n",
            "[1.5, \"r\", \"-3x9\"]\n",
            "[2.0, \"r\", \"99999999999x99999999999\"]\n",
            "[2.5, \"o\", \"también bien\", \"y un campo de más\"]\n",
        ))
        .unwrap();
        assert_eq!(
            replay(&cast),
            [
                (0.5, "bien".to_string()),
                (2.0, "<500x300>".to_string()),
                (2.5, "también bien".to_string()),
            ]
        );
        assert_eq!(cast.skipped, 14);

        // Tiempos negativos, desordenados o enormes.
        let cast = parse(concat!(
            "{\"version\":2,\"width\":80,\"height\":24}\n",
            "[-5.0, \"o\", \"a\"]\n",
            "[10.0, \"o\", \"b\"]\n",
            "[3.0, \"o\", \"c\"]\n",
            "[11.0, \"o\", \"d\"]\n",
            "[1e300, \"o\", \"e\"]\n",
            "[1e308, \"o\", \"f\"]\n",
            "[1e999, \"o\", \"g\"]\n",
        ))
        .unwrap();
        let events = replay(&cast);
        // «a» y «c» no retroceden; «e» y «f» llegan como mucho una hora
        // después; el último ni siquiera es un número válido.
        assert_eq!(
            events,
            [
                (0.0, "a".to_string()),
                (10.0, "bc".to_string()),
                (11.0, "d".to_string()),
                (11.0 + MAX_GAP, "e".to_string()),
                (11.0 + 2.0 * MAX_GAP, "f".to_string()),
            ]
        );
        assert!(cast.duration().is_finite());
        // En la versión 3 los intervalos negativos no hacen retroceder.
        let cast = parse(concat!(
            "{\"version\":3,\"term\":{\"cols\":80,\"rows\":24}}\n",
            "[1.0, \"o\", \"a\"]\n",
            "[-4.0, \"o\", \"b\"]\n",
            "[1e12, \"o\", \"c\"]\n",
        ))
        .unwrap();
        assert_eq!(
            replay(&cast),
            [(1.0, "ab".to_string()), (1.0 + MAX_GAP, "c".to_string())]
        );
        // Bytes que no son UTF-8 en una línea: esa línea se salta.
        let mut raw = b"{\"version\":2,\"width\":80,\"height\":24}\n[1.0, \"o\", \"".to_vec();
        raw.extend_from_slice(b"\xff\xfe\"]\n[2.0, \"o\", \"bien\"]\n");
        let cast = Cast::parse(std::io::Cursor::new(raw)).unwrap();
        assert_eq!(replay(&cast), [(2.0, "bien".to_string())]);
        assert_eq!(cast.skipped, 1);
    }

    #[test]
    fn huge_lines_and_event_floods_are_bounded() {
        // Una línea desmedida se salta sin cargarla entera.
        let mut line = Vec::new();
        let mut reader = std::io::Cursor::new(b"corta\nlarguisima de verdad\nfin".to_vec());
        assert_eq!(
            read_bounded_line(&mut reader, &mut line, 10).unwrap(),
            Some(true)
        );
        assert_eq!(line, b"corta");
        assert_eq!(
            read_bounded_line(&mut reader, &mut line, 10).unwrap(),
            Some(false)
        );
        assert!(line.is_empty());
        assert_eq!(
            read_bounded_line(&mut reader, &mut line, 10).unwrap(),
            Some(true)
        );
        assert_eq!(line, b"fin");
        assert_eq!(read_bounded_line(&mut reader, &mut line, 10).unwrap(), None);
        // También si llega troceada en lecturas pequeñas.
        let mut reader = std::io::BufReader::with_capacity(
            4,
            std::io::Cursor::new(b"0123456789abcdef\nok\n".to_vec()),
        );
        assert_eq!(
            read_bounded_line(&mut reader, &mut line, 10).unwrap(),
            Some(false)
        );
        assert_eq!(
            read_bounded_line(&mut reader, &mut line, 10).unwrap(),
            Some(true)
        );
        assert_eq!(line, b"ok");

        // Un evento más largo que el máximo se salta; el resto se lee.
        let mut text = String::from("{\"version\":2,\"width\":80,\"height\":24}\n");
        text.push_str(&format!(
            "[1.0, \"o\", \"{}\"]\n",
            "x".repeat(MAX_LINE_BYTES + 1)
        ));
        text.push_str("[2.0, \"o\", \"después\"]\n");
        let cast = parse(&text).unwrap();
        assert_eq!(replay(&cast), [(2.0, "después".to_string())]);
        assert_eq!(cast.skipped, 1);

        // Una avalancha de eventos en el mismo instante es un solo evento…
        let mut text = String::from("{\"version\":2,\"width\":80,\"height\":24}\n");
        for _ in 0..50_000 {
            text.push_str("[1.0, \"o\", \"ab\"]\n");
        }
        let cast = parse(&text).unwrap();
        assert_eq!(cast.event_count(), 1);
        assert_eq!(cast.data.len(), 100_000);
        // …y si cada uno lleva su tiempo, hay un tope.
        let mut text = String::from("{\"version\":3,\"term\":{\"cols\":80,\"rows\":24}}\n");
        for _ in 0..MAX_EVENTS + 1 {
            text.push_str("[0.001,\"o\",\"a\"]\n");
        }
        let error = parse(&text).unwrap_err().to_string();
        assert!(error.contains("demasiados eventos"), "{error}");
    }

    #[test]
    fn unreadable_and_oversized_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Cast::load(&dir.path().join("no-existe.cast")).is_err());
        assert!(Cast::load(dir.path()).is_err());
        // Más grande que el máximo (un fichero disperso: no ocupa disco).
        let big = dir.path().join("enorme.cast");
        let file = std::fs::File::create(&big).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        let error = format!("{:#}", Cast::load(&big).unwrap_err());
        assert!(error.contains("como mucho"), "{error}");
        // Un binario cualquiera.
        let noise = dir.path().join("ruido.cast");
        std::fs::write(&noise, [0u8, 159, 146, 150, 10, 0, 255]).unwrap();
        assert!(Cast::load(&noise).is_err());
    }

    fn sample() -> Cast {
        parse(concat!(
            "{\"version\":2,\"width\":80,\"height\":24}\n",
            "[1.0, \"o\", \"a\"]\n",
            "[2.0, \"o\", \"b\"]\n",
            "[2.5, \"r\", \"100x30\"]\n",
            "[62.5, \"o\", \"c\"]\n",
            "[63.0, \"o\", \"d\"]\n",
        ))
        .unwrap()
    }

    /// Lo que el reproductor entrega al avanzar.
    fn shown(player: &mut Player) -> String {
        let mut out = String::new();
        let pending = player.drain(usize::MAX, |step| match step {
            Step::Output(bytes) => out.push_str(&String::from_utf8_lossy(bytes)),
            Step::Resize(cols, rows) => out.push_str(&format!("<{cols}x{rows}>")),
        });
        assert!(!pending);
        out
    }

    #[test]
    fn playback_follows_the_clock_and_the_speed() {
        let mut player = Player::new(sample());
        assert_eq!(player.duration(), 63.0);
        assert!(!player.playing && !player.is_finished());
        // En pausa el reloj no corre.
        player.advance(10.0);
        assert_eq!(player.position(), 0.0);
        assert_eq!(shown(&mut player), "");
        assert_eq!(player.next_event_in(), None);
        player.playing = true;
        assert_eq!(player.next_event_in(), Some(1.0));
        player.advance(0.5);
        assert_eq!(shown(&mut player), "");
        player.advance(0.5);
        assert_eq!(shown(&mut player), "a");
        // Al doble de velocidad, medio segundo es uno.
        player.speed = 2.0;
        assert_eq!(player.next_event_in(), Some(0.5));
        player.advance(0.75);
        assert_eq!(player.position(), 2.5);
        assert_eq!(shown(&mut player), "b<100x30>");
        // A cámara lenta.
        player.speed = 0.5;
        player.advance(60.0);
        assert_eq!(player.position(), 32.5);
        assert_eq!(shown(&mut player), "");
        // Hasta el final: se para sola.
        player.speed = 8.0;
        player.advance(1000.0);
        assert_eq!(player.position(), 63.0);
        assert_eq!(shown(&mut player), "cd");
        assert!(player.is_finished() && !player.playing);
        // Tiempos que no valen no mueven nada.
        player.playing = true;
        player.advance(f64::NAN);
        player.advance(-3.0);
        assert_eq!(player.position(), 63.0);
    }

    #[test]
    fn seeking_goes_anywhere() {
        let mut player = Player::new(sample());
        // Hacia delante: se entrega todo lo anterior a ese punto.
        assert!(!player.seek(2.2));
        assert_eq!(shown(&mut player), "ab");
        assert!(!player.seek(2.4));
        assert_eq!(shown(&mut player), "");
        // Hacia atrás hay que empezar de cero.
        assert!(player.seek(1.5));
        assert_eq!(shown(&mut player), "a");
        // Volver a un punto posterior al último evento mostrado no obliga.
        assert!(!player.seek(1.2));
        assert!(player.seek(0.0));
        assert_eq!(shown(&mut player), "");
        // Fuera de rango, o un valor absurdo.
        assert!(!player.seek(1e9));
        assert_eq!(player.position(), 63.0);
        assert_eq!(shown(&mut player), "ab<100x30>cd");
        assert!(player.seek(-10.0));
        assert_eq!(player.position(), 0.0);
        assert!(!player.seek(f64::NAN));
        assert_eq!(player.position(), 0.0);
        // Con poco margen por llamada, un salto largo se reparte.
        player.seek(63.0);
        let mut calls = 0;
        let mut out = String::new();
        while player.drain(2, |step| {
            if let Step::Output(bytes) = step {
                out.push_str(&String::from_utf8_lossy(bytes));
            }
        }) {
            calls += 1;
            assert!(calls < 10);
        }
        assert_eq!(out, "abcd");
        assert!(calls >= 1);
    }

    #[test]
    fn long_silences_can_be_skipped() {
        let mut player = Player::new(sample());
        player.set_idle_limit(Some(2.0));
        // 1 + 1 + 0,5 + (60 → 2) + 0,5
        assert_eq!(player.duration(), 5.0);
        assert_eq!(player.idle_limit(), Some(2.0));
        player.playing = true;
        player.advance(2.5);
        assert_eq!(shown(&mut player), "ab<100x30>");
        // En mitad del silencio se quita el recorte: se sigue en el mismo
        // punto de la grabación, medido desde el último evento.
        player.advance(1.0);
        assert_eq!(player.position(), 3.5);
        player.set_idle_limit(None);
        assert_eq!(player.duration(), 63.0);
        assert_eq!(player.position(), 3.5);
        assert_eq!(shown(&mut player), "");
        // Bien entrado el silencio largo, al recortarlo se pasa a su final:
        // toca ya el evento siguiente.
        player.advance(40.0);
        assert_eq!(player.position(), 43.5);
        player.set_idle_limit(Some(2.0));
        assert_eq!(player.position(), 4.5);
        assert_eq!(shown(&mut player), "c");
        player.advance(0.5);
        assert_eq!(shown(&mut player), "d");
        // Límites que no valen equivalen a no recortar.
        player.set_idle_limit(Some(-1.0));
        assert_eq!(player.idle_limit(), None);
        player.set_idle_limit(Some(f64::NAN));
        assert_eq!(player.duration(), 63.0);
        // Una grabación vacía.
        let mut empty = Player::new(parse("{\"version\":2,\"width\":80,\"height\":24}\n").unwrap());
        assert_eq!(empty.duration(), 0.0);
        assert!(empty.is_finished());
        empty.playing = true;
        empty.advance(1.0);
        assert_eq!(shown(&mut empty), "");
        assert!(!empty.playing);
        empty.set_idle_limit(Some(2.0));
        assert!(!empty.seek(5.0));
    }

    #[test]
    fn clock_texts() {
        assert_eq!(clock_text(0.0), "0:00");
        assert_eq!(clock_text(65.9), "1:05");
        assert_eq!(clock_text(3723.0), "1:02:03");
        assert_eq!(clock_text(f64::NAN), "0:00");
        assert_eq!(clock_text(-4.0), "0:00");
    }
}
