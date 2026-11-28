//! Historial de órdenes por equipo: las órdenes que se ejecutan en cada
//! equipo se recuerdan para volver a enviarlas o copiarlas.
//!
//! Lo que se guarda sale siempre de lo que el terminal **muestra**, nunca de
//! lo que se teclea: al pulsar Intro se espera a que llegue el eco completo
//! (hasta el salto de línea con el que contesta la shell) y se lee la línea
//! del cursor, a la que se le quita el indicador. Una contraseña escrita
//! sin eco no aparece en pantalla y, por tanto, no puede guardarse.
//!
//! Dónde acaba el indicador se sabe con exactitud si la shell lo marca
//! (OSC 133); si no, es el texto que había delante del cursor cuando se
//! empezó a teclear y, en último caso, se reconoce por su forma (`$ `,
//! `# `, `> `…). Sin marcadores no se distingue la shell de otro programa
//! que lea líneas, así que también se recuerda lo que se le escriba a
//! `mysql` o a `python`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::shell_marks::Mark;

/// Las órdenes más largas no se guardan.
pub const MAX_COMMAND_CHARS: usize = 1000;
pub const DEFAULT_MAX_PER_HOST: u32 = 500;
pub const MIN_PER_HOST: u32 = 20;
pub const MAX_PER_HOST: u32 = 5000;
/// Equipos de los que como mucho se guarda historial.
const MAX_HOSTS: usize = 2000;
/// Un fichero de historial mayor no se lee: no lo ha escrito GodRemote.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Clave del historial de los terminales locales.
pub const LOCAL_KEY: &str = "local";
/// Tiempo que se espera el eco de un Intro antes de desistir.
const ECHO_TIMEOUT: Duration = Duration::from_secs(5);
/// Lo tecleado en una línea que se compara con su eco, como mucho.
const MAX_TYPED_CHARS: usize = 4 * MAX_COMMAND_CHARS;

/// Fichero del historial de órdenes.
pub fn history_file() -> PathBuf {
    crate::paths::data_dir().join("command_history.json")
}

/// Preferencias del historial de órdenes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistorySettings {
    /// Recordar las órdenes que se ejecutan.
    #[serde(rename = "activo")]
    pub enabled: bool,
    /// Órdenes que se guardan como mucho de cada equipo.
    #[serde(rename = "maximo")]
    pub max_per_host: u32,
    /// Equipos (por su clave) cuyas órdenes no se recuerdan.
    #[serde(rename = "equipos_excluidos")]
    pub excluded: Vec<String>,
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_per_host: DEFAULT_MAX_PER_HOST,
            excluded: vec![],
        }
    }
}

impl HistorySettings {
    /// Recorta lo que venga desmedido de un fichero editado a mano.
    pub fn sanitize(&mut self) {
        self.max_per_host = self.max_per_host.clamp(MIN_PER_HOST, MAX_PER_HOST);
        self.excluded
            .retain(|key| !key.is_empty() && key.len() <= 64);
        self.excluded.sort();
        self.excluded.dedup();
        self.excluded.truncate(MAX_HOSTS);
    }

    /// Se recuerdan las órdenes del equipo con esa clave.
    pub fn records(&self, key: &str) -> bool {
        self.enabled && !self.excluded.iter().any(|k| k == key)
    }

    /// El equipo está excluido, se recuerden o no las órdenes de los demás.
    pub fn is_excluded(&self, key: &str) -> bool {
        self.excluded.iter().any(|k| k == key)
    }

    /// Excluye un equipo del historial o vuelve a incluirlo.
    pub fn set_excluded(&mut self, key: &str, excluded: bool) {
        self.excluded.retain(|k| k != key);
        if excluded {
            self.excluded.push(key.to_string());
            self.excluded.sort();
        }
    }
}

/// Una orden recordada.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    #[serde(rename = "orden")]
    pub command: String,
    /// Veces que se ha ejecutado.
    #[serde(rename = "veces", default)]
    pub count: u32,
    /// Última vez (segundos desde 1970).
    #[serde(rename = "ultima", default)]
    pub last: i64,
}

/// Las órdenes de un equipo, de la más reciente a la más antigua.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostHistory {
    /// Nombre del equipo la última vez que se usó (por si desaparece).
    #[serde(rename = "nombre", default)]
    pub name: String,
    #[serde(rename = "ordenes", default)]
    pub entries: Vec<Entry>,
}

#[derive(Default, Serialize, Deserialize)]
struct StoredHistory {
    #[serde(rename = "equipos", default)]
    hosts: BTreeMap<String, HostHistory>,
}

/// El historial de todos los equipos.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HistoryStore {
    hosts: BTreeMap<String, HostHistory>,
    /// Hay cambios sin guardar.
    dirty: bool,
}

impl HistoryStore {
    /// Lee el historial. Si el fichero no existe o no se entiende, empieza
    /// vacío: es un dato de conveniencia, no merece un error.
    pub fn load(path: &Path) -> Self {
        let mut store = Self::default();
        let Ok(meta) = std::fs::metadata(path) else {
            return store;
        };
        if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
            return store;
        }
        let Some(stored) = std::fs::read(path)
            .ok()
            .and_then(|data| serde_json::from_slice::<StoredHistory>(&data).ok())
        else {
            return store;
        };
        // Lo leído se acota como si viniera de fuera.
        for (key, mut host) in stored.hosts.into_iter().take(MAX_HOSTS) {
            if key.is_empty() || key.len() > 64 {
                continue;
            }
            host.name = host.name.chars().take(200).collect();
            host.entries
                .retain(|entry| clean_command(&entry.command).is_some());
            host.entries.truncate(MAX_PER_HOST as usize);
            if !host.entries.is_empty() {
                store.hosts.insert(key, host);
            }
        }
        store
    }

    /// Guarda el historial en un fichero legible solo por el usuario.
    pub fn save(&mut self, path: &Path) -> Result<()> {
        let stored = StoredHistory {
            hosts: self.hosts.clone(),
        };
        crate::paths::write_private(path, &serde_json::to_vec_pretty(&stored)?)?;
        self.dirty = false;
        Ok(())
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }

    /// Anota una orden ejecutada en un equipo. Una orden repetida sube al
    /// principio y cuenta una vez más. Devuelve `false` si no se guarda.
    pub fn record(&mut self, key: &str, name: &str, command: &str, now: i64, max: u32) -> bool {
        let Some(command) = clean_command(command) else {
            return false;
        };
        if key.is_empty() || (!self.hosts.contains_key(key) && self.hosts.len() >= MAX_HOSTS) {
            return false;
        }
        let host = self.hosts.entry(key.to_string()).or_default();
        if !name.is_empty() {
            host.name = name.chars().take(200).collect();
        }
        let count = match host.entries.iter().position(|e| e.command == command) {
            Some(index) => host.entries.remove(index).count.saturating_add(1),
            None => 1,
        };
        host.entries.insert(
            0,
            Entry {
                command,
                count,
                last: now,
            },
        );
        host.entries
            .truncate(max.clamp(MIN_PER_HOST, MAX_PER_HOST) as usize);
        self.dirty = true;
        true
    }

    pub fn host(&self, key: &str) -> Option<&HostHistory> {
        self.hosts.get(key)
    }

    /// Órdenes de un equipo, de la más reciente a la más antigua.
    pub fn entries(&self, key: &str) -> &[Entry] {
        self.hosts.get(key).map_or(&[], |h| h.entries.as_slice())
    }

    /// Claves de los equipos con historial y su nombre guardado.
    pub fn hosts(&self) -> impl Iterator<Item = (&str, &str)> {
        self.hosts
            .iter()
            .map(|(key, host)| (key.as_str(), host.name.as_str()))
    }

    /// Órdenes de un equipo que encajan con lo buscado, la mejor primero;
    /// sin búsqueda, todas, de la más reciente a la más antigua.
    pub fn search(&self, key: &str, query: &str) -> Vec<&Entry> {
        let entries = self.entries(key);
        crate::palette::rank(query, entries.iter().map(|e| e.command.as_str()))
            .into_iter()
            .map(|index| &entries[index])
            .collect()
    }

    /// Borra una orden de un equipo.
    pub fn remove(&mut self, key: &str, command: &str) -> bool {
        let Some(host) = self.hosts.get_mut(key) else {
            return false;
        };
        let before = host.entries.len();
        host.entries.retain(|e| e.command != command);
        let removed = host.entries.len() != before;
        if host.entries.is_empty() {
            self.hosts.remove(key);
        }
        self.dirty |= removed;
        removed
    }

    /// Borra todo el historial de un equipo.
    pub fn clear_host(&mut self, key: &str) -> bool {
        let removed = self.hosts.remove(key).is_some();
        self.dirty |= removed;
        removed
    }

    /// Borra el historial de todos los equipos.
    pub fn clear(&mut self) {
        self.dirty |= !self.hosts.is_empty();
        self.hosts.clear();
    }

    /// Se queda solo con el historial de los equipos que `keep` acepte
    /// (para olvidar el de los que ya no existen).
    pub fn retain_hosts(&mut self, keep: impl Fn(&str) -> bool) {
        let before = self.hosts.len();
        self.hosts.retain(|key, _| keep(key));
        self.dirty |= self.hosts.len() != before;
    }
}

/// Deja una orden lista para guardarla, o `None` si no debe guardarse: vacía,
/// desmedida, con caracteres de control, hecha solo de los asteriscos con
/// los que algunos programas tapan una contraseña, o empezada por un espacio
/// (la costumbre de las shells para dejar algo fuera del historial).
pub fn clean_command(text: &str) -> Option<String> {
    let command = text.trim_end();
    if command.is_empty()
        || command.starts_with(char::is_whitespace)
        || command.chars().count() > MAX_COMMAND_CHARS
        || command.chars().any(char::is_control)
        || command.chars().all(|c| matches!(c, '*' | '•' | '●' | '·'))
    {
        return None;
    }
    Some(command.to_string())
}

/// Finales de un indicador de shell, con el espacio que los separa de la
/// orden.
const PROMPT_MARKS: [&str; 8] = ["$ ", "# ", "% ", "> ", "❯ ", "➜ ", "» ", "λ "];

/// Separa, por su forma, el indicador de la shell de lo que le sigue:
/// `ana@web:~$ ls` → (`ana@web:~$ `, `ls`). Devuelve `None` si la línea no
/// parece empezar por un indicador.
pub fn strip_prompt(line: &str) -> Option<(&str, &str)> {
    // «C:\Users\ana>dir»: cmd no deja espacio tras su indicador.
    let bytes = line.as_bytes();
    if bytes.len() > 3
        && bytes[0].is_ascii_alphabetic()
        && &bytes[1..3] == b":\\"
        && let Some(end) = line.find('>')
    {
        return Some(line.split_at(end + 1));
    }
    // El primero que aparezca: lo de detrás ya es la orden.
    PROMPT_MARKS
        .iter()
        .filter_map(|mark| line.find(mark).map(|at| at + mark.len()))
        .min()
        .map(|end| line.split_at(end))
}

/// Lo que precede al cursor no es el indicador de una shell sino una
/// pregunta de algún programa: termina en dos puntos o en un signo de
/// interrogación («Password:», «Usuario:», «¿Seguro?»), o en las opciones
/// de una respuesta («[y/N]», «(yes/no)»). Lo que se conteste ahí no es una
/// orden y puede ser un secreto aunque se vea.
pub fn is_question(prompt: &str) -> bool {
    let prompt = prompt.trim_end();
    if prompt.ends_with([':', '?', '：', '？']) {
        return true;
    }
    let choices = |open: char, close: char| {
        prompt.ends_with(close)
            && prompt
                .rfind(open)
                .is_some_and(|start| prompt[start..].contains('/'))
    };
    choices('[', ']') || choices('(', ')')
}

/// Espacios seguidos a partir de los cuales lo que queda en la línea ya no
/// es la orden (el indicador que zsh pone a la derecha).
const RIGHT_PROMPT_GAP: &str = "          ";

/// Saca la orden de la línea del cursor tal como quedó en pantalla.
///
/// - `prefix`: lo que había delante del cursor antes de teclear (el
///   indicador); `marked` si lo señaló la shell con OSC 133.
/// - `typed`: lo tecleado, si fueron solo caracteres corrientes (sin
///   flechas, tabulador, borrados ni pegados). Sirve para comprobar que lo
///   que se ve es su eco; si no lo es, no se guarda nada.
pub fn extract(
    line: &str,
    prefix: Option<&str>,
    marked: bool,
    typed: Option<&str>,
) -> Option<String> {
    let line = line.trim_end();
    let prefix = prefix.filter(|p| !p.trim().is_empty());
    // El indicador conocido, si la línea sigue empezando por él; si no (se
    // tecleó antes de que apareciera, o la shell ha reescrito la línea),
    // se reconoce por su forma.
    let (prompt, rest) = match prefix.and_then(|p| line.strip_prefix(p).map(|rest| (p, rest))) {
        Some(found) => found,
        // La línea es solo el indicador: no se ha escrito nada.
        None if prefix.is_some_and(|p| p.trim_end() == line) => return None,
        None => strip_prompt(line)?,
    };
    // Sin la palabra de la shell, lo que parece una pregunta lo es.
    if !marked && is_question(prompt) {
        return None;
    }
    // Lo que hay en pantalla tras el indicador, sin lo que quede muy a la
    // derecha.
    let shown = || clean_command(rest.split(RIGHT_PROMPT_GAP).next().unwrap_or(rest));
    match typed.map(str::trim_end) {
        // Tecleo corriente que la pantalla muestra tal cual: es la orden.
        Some(typed) if !typed.is_empty() && rest.starts_with(typed) => clean_command(typed),
        // La shell dice dónde acaba su indicador: lo que sigue es la
        // orden, aunque haya reescrito lo tecleado (una abreviatura).
        Some(_) if marked => shown(),
        // Sin esa garantía, si lo que se ve no es el eco de lo tecleado
        // (o no se tecleó nada), no hay orden que guardar.
        Some(_) => None,
        // Línea editada con flechas, tabulador o pegados: de la pantalla.
        None => shown(),
    }
}

/// Un Intro pulsado cuyo eco aún no ha llegado.
struct Pending {
    prefix: Option<String>,
    marked: bool,
    typed: Option<Zeroizing<String>>,
    at: Instant,
}

/// Sigue lo que el usuario teclea en un terminal para saber qué orden
/// ejecuta. No guarda nada por sí mismo: entrega la orden cuando la ve.
#[derive(Default)]
pub struct Tracker {
    /// Lo que había delante del cursor al empezar la línea en curso.
    prefix: Option<String>,
    /// Ese texto lo marcó la shell (OSC 133;B).
    marked: bool,
    /// Ya se ha tecleado algo en la línea en curso.
    started: bool,
    /// Lo tecleado en ella, mientras sean caracteres corrientes.
    typed: Option<Zeroizing<String>>,
    /// La línea en curso no se va a guardar (un pegado de varias líneas).
    spoiled: bool,
    pending: Option<Pending>,
    /// El indicador de la última orden reconocida.
    last_prompt: Option<String>,
}

impl Tracker {
    /// Olvida la línea en curso.
    fn reset_line(&mut self) {
        self.prefix = None;
        self.marked = false;
        self.started = false;
        self.typed = None;
        self.spoiled = false;
    }

    /// Olvida todo (la sesión se cierra o se reconecta).
    pub fn reset(&mut self) {
        self.reset_line();
        self.pending = None;
        self.last_prompt = None;
    }

    /// El indicador que precedía a la última orden reconocida: sirve para
    /// saber, más tarde, que la shell vuelve a esperar una orden.
    pub fn last_prompt(&self) -> Option<&str> {
        self.last_prompt.as_deref()
    }

    /// Hay un Intro a la espera de su eco.
    pub fn is_waiting(&self) -> bool {
        self.pending.is_some()
    }

    /// La shell ha emitido un marcador. Con `B` llega lo que hay delante
    /// del cursor en ese momento: el indicador, exacto.
    pub fn mark(&mut self, mark: &Mark, prefix: Option<String>) {
        match mark {
            Mark::PromptStart => self.reset_line(),
            Mark::PromptEnd => {
                self.reset_line();
                if let Some(prefix) = prefix {
                    self.prefix = Some(prefix);
                    self.marked = true;
                }
            }
            Mark::CommandStart | Mark::CommandEnd(_) => {}
        }
    }

    /// El usuario envía algo al terminal (antes de que salga).
    ///
    /// `at_prompt` es `false` si lo recibe un programa y no la shell (una
    /// aplicación a pantalla completa, o una orden en marcha según los
    /// marcadores de la shell). `before_cursor` da lo que hay ahora delante
    /// del cursor y solo se consulta al empezar una línea.
    pub fn input(
        &mut self,
        bytes: &[u8],
        at_prompt: bool,
        before_cursor: impl FnOnce() -> String,
        now: Instant,
    ) {
        if !at_prompt {
            self.reset_line();
            return;
        }
        if !self.started {
            self.started = true;
            self.typed = Some(Zeroizing::new(String::new()));
            if self.prefix.is_none() {
                self.prefix = Some(before_cursor());
            }
        }
        // Un pegado entre corchetes con varias líneas: la shell lo deja en
        // el editor y lo que se vea al ejecutarlo no es una línea.
        let bracketed = bytes.starts_with(b"\x1b[200~");
        let (line, enter) = match bytes.iter().position(|b| *b == b'\r' || *b == b'\n') {
            Some(_) if bracketed => {
                self.spoiled = true;
                (bytes, false)
            }
            Some(at) => (&bytes[..at], true),
            None => (bytes, false),
        };
        match std::str::from_utf8(line) {
            Ok(text) if !text.chars().any(char::is_control) => {
                if let Some(typed) = &mut self.typed {
                    if typed.chars().count() + text.chars().count() <= MAX_TYPED_CHARS {
                        typed.push_str(text);
                    } else {
                        self.typed = None;
                    }
                }
            }
            // Ctrl+C o Ctrl+D: la línea se abandona.
            _ if line == [0x03] || line == [0x04] => {
                self.reset_line();
                return;
            }
            // Flechas, tabulador, borrados…: lo tecleado ya no es la línea.
            _ => self.typed = None,
        }
        if enter {
            // Si había otro Intro sin eco, este lo sustituye: o su eco no
            // va a llegar nunca (una lectura sin eco no deja salto de
            // línea) o las dos órdenes se han tecleado tan seguidas que no
            // se sabría cuál es cuál; en ese caso lo que se vea no encajará
            // con esta y no se guardará ninguna.
            self.pending = (!self.spoiled).then(|| Pending {
                prefix: self.prefix.take(),
                marked: self.marked,
                typed: self.typed.take(),
                at: now,
            });
            self.reset_line();
        }
    }

    /// Ha llegado salida. Si se espera el eco de un Intro y la salida trae
    /// el salto de línea con el que termina, devuelve su posición: hay que
    /// procesar la salida hasta ahí, llamar a [`Tracker::resolve`] con la
    /// línea del cursor y procesar después el resto.
    pub fn echo_end(&self, data: &[u8]) -> Option<usize> {
        self.pending.as_ref()?;
        data.iter().position(|b| *b == b'\n')
    }

    /// El eco del Intro está completo: saca la orden de la línea del
    /// cursor. `readable` es `false` si lo que hay en pantalla no es una
    /// línea de la shell (una aplicación a pantalla completa).
    pub fn resolve(&mut self, line: &str, readable: bool) -> Option<String> {
        let pending = self.pending.take()?;
        if !readable {
            return None;
        }
        let command = extract(
            line,
            pending.prefix.as_deref(),
            pending.marked,
            pending.typed.as_deref().map(String::as_str),
        )?;
        if let Some(prefix) = pending.prefix
            && !prefix.trim().is_empty()
            && line.starts_with(&prefix)
        {
            self.last_prompt = Some(prefix);
        }
        Some(command)
    }

    /// Desiste del Intro cuyo eco no ha llegado en un tiempo razonable.
    pub fn expire(&mut self, now: Instant) {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| now.saturating_duration_since(p.at) > ECHO_TIMEOUT)
        {
            self.pending = None;
        }
    }
}

/// «hace 5 min», «hace 3 h», «hace 2 días» o la fecha.
pub fn ago(then: i64, now: i64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        i64::MIN..=59 => crate::i18n::tr("ahora").to_string(),
        60..=3599 => crate::trf!("hace {n} min", n = secs / 60),
        3600..=86_399 => crate::trf!("hace {n} h", n = secs / 3600),
        86_400..=172_799 => crate::i18n::tr("ayer").to_string(),
        172_800..=2_591_999 => crate::trf!("hace {n} días", n = secs / 86_400),
        _ => chrono::DateTime::from_timestamp(then, 0)
            .map(|date| {
                date.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d")
                    .to_string()
            })
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_cleaned_before_being_kept() {
        assert_eq!(clean_command("ls -la  "), Some("ls -la".to_string()));
        assert_eq!(
            clean_command("echo 'a  b'"),
            Some("echo 'a  b'".to_string())
        );
        // Vacías, o empezadas por un espacio (para dejarlas fuera a propósito).
        assert_eq!(clean_command(""), None);
        assert_eq!(clean_command("   "), None);
        assert_eq!(clean_command(" mysql -psecreto"), None);
        // Asteriscos con los que se tapa una contraseña.
        assert_eq!(clean_command("********"), None);
        assert_eq!(clean_command("•••••"), None);
        assert_eq!(clean_command("rm *"), Some("rm *".to_string()));
        // Caracteres de control y tamaños desmedidos.
        assert_eq!(clean_command("ls\x1b[31m"), None);
        assert_eq!(clean_command("a\nb"), None);
        assert_eq!(clean_command(&"x".repeat(MAX_COMMAND_CHARS + 1)), None);
        assert!(clean_command(&"x".repeat(MAX_COMMAND_CHARS)).is_some());
    }

    #[test]
    fn prompts_are_separated_by_their_shape() {
        assert_eq!(
            strip_prompt("ana@web:~$ ls -la"),
            Some(("ana@web:~$ ", "ls -la"))
        );
        assert_eq!(
            strip_prompt("root@web:/var/log# tail -f syslog"),
            Some(("root@web:/var/log# ", "tail -f syslog"))
        );
        assert_eq!(strip_prompt("web% uptime"), Some(("web% ", "uptime")));
        assert_eq!(
            strip_prompt("[ana@web ~]$ echo a > b"),
            Some(("[ana@web ~]$ ", "echo a > b"))
        );
        assert_eq!(strip_prompt("❯ git status"), Some(("❯ ", "git status")));
        assert_eq!(
            strip_prompt("PS C:\\Users\\ana> dir"),
            Some(("PS C:\\Users\\ana> ", "dir"))
        );
        assert_eq!(
            strip_prompt("C:\\Users\\ana>dir /w"),
            Some(("C:\\Users\\ana>", "dir /w"))
        );
        assert_eq!(
            strip_prompt("mysql> select 1;"),
            Some(("mysql> ", "select 1;"))
        );
        // Solo el indicador: no hay orden.
        assert_eq!(strip_prompt("ana@web:~$ "), Some(("ana@web:~$ ", "")));
        // Lo que no empieza por un indicador.
        assert_eq!(strip_prompt("[sudo] password for ana:"), None);
        assert_eq!(strip_prompt("Descargando 45%"), None);
        assert_eq!(strip_prompt(""), None);
    }

    #[test]
    fn questions_are_told_from_shell_prompts() {
        for prompt in [
            "Password: ",
            "[sudo] password for ana: ",
            "Enter passphrase for key '/home/ana/.ssh/id_ed25519': ",
            "Contraseña:",
            "Usuario: ",
            "Nombre del proyecto: ",
            "¿Seguro? ",
            "Continue? [y/N] ",
            "Overwrite (yes/no)? ",
            "Sobrescribir (s/n) ",
            "Are you sure you want to continue connecting (yes/no/[fingerprint])? ",
        ] {
            assert!(is_question(prompt), "{prompt:?}");
        }
        for prompt in [
            "ana@web:~$ ",
            // Una carpeta con un nombre sospechoso no es una pregunta.
            "ana@web:~/claves$ ",
            "root@web:/etc/passwords# ",
            "[ana@web ~]$ ",
            "[ana@web ~] ",
            "web% ",
            "PS C:\\Users\\ana> ",
            "mysql> ",
            "(venv) ana@web:~$ ",
            "web (main) ",
            "",
        ] {
            assert!(!is_question(prompt), "{prompt:?}");
        }
    }

    #[test]
    fn commands_are_read_from_the_screen() {
        let prompt = "ana@web:~$ ";
        // Lo normal: indicador conocido y tecleo corriente.
        assert_eq!(
            extract("ana@web:~$ ls -la", Some(prompt), false, Some("ls -la")),
            Some("ls -la".to_string())
        );
        // Sin eco (una contraseña): la línea es solo lo que había.
        assert_eq!(
            extract("Password: ", Some("Password: "), false, Some("hunter2")),
            None
        );
        assert_eq!(
            extract("Password:", Some("Password: "), false, Some("hunter2")),
            None
        );
        // Con eco, pero es la respuesta a una pregunta (quizá un secreto
        // que se pide a la vista), no una orden.
        assert_eq!(
            extract(
                "API token: abc123",
                Some("API token: "),
                false,
                Some("abc123")
            ),
            None
        );
        assert_eq!(
            extract(
                "Continue? [y/N] y",
                Some("Continue? [y/N] "),
                false,
                Some("y")
            ),
            None
        );
        // Lo tecleado tiene que verse justo detrás del indicador, no en
        // cualquier parte de la línea.
        assert_eq!(
            extract(
                "ana@web:~$ cat false.txt",
                Some("ana@web:~$ "),
                false,
                Some("ls")
            ),
            None
        );
        // Intro sin haber escrito nada.
        assert_eq!(extract("ana@web:~$", Some(prompt), false, Some("")), None);
        assert_eq!(extract("ana@web:~$ ", Some(prompt), false, Some("")), None);
        // Línea recuperada del historial de la shell o completada con el
        // tabulador: no hay tecleo que comparar, se toma de la pantalla.
        assert_eq!(
            extract(
                "ana@web:~$ systemctl status nginx",
                Some(prompt),
                false,
                None
            ),
            Some("systemctl status nginx".to_string())
        );
        // El indicador que zsh pone a la derecha no es parte de la orden.
        assert_eq!(
            extract(
                "web% git pull                                   [main]",
                Some("web% "),
                false,
                None
            ),
            Some("git pull".to_string())
        );
        // Empezada por un espacio: se deja fuera a propósito.
        assert_eq!(
            extract(
                "ana@web:~$  export CLAVE=x",
                Some(prompt),
                false,
                Some(" export CLAVE=x")
            ),
            None
        );

        // Se empezó a teclear antes de que apareciera el indicador: lo que
        // había delante del cursor no vale y se reconoce por su forma.
        assert_eq!(
            extract("ana@web:~$ uptime", Some(""), false, Some("uptime")),
            Some("uptime".to_string())
        );
        assert_eq!(
            extract(
                "ana@web:~$ uptime",
                Some("compilando…"),
                false,
                Some("uptime")
            ),
            Some("uptime".to_string())
        );
        // …y si lo que se ve no es el eco de lo tecleado, no se guarda.
        assert_eq!(
            extract("[sudo] password for ana:", Some(""), false, Some("hunter2")),
            None
        );
        assert_eq!(
            extract("ana@web:~$ otra cosa", Some(""), false, Some("hunter2")),
            None
        );
        assert_eq!(
            extract(
                "Leyendo paquetes... Hecho",
                Some("Leyendo paquetes... "),
                false,
                Some("ls")
            ),
            None
        );
        // Sin indicador reconocible no hay orden.
        assert_eq!(
            extract("texto cualquiera", None, false, Some("texto")),
            None
        );
        assert_eq!(extract("", None, false, None), None);

        // Con marcadores de la shell el indicador es exacto, aunque no se
        // parezca a ninguno, y la shell puede haber reescrito lo tecleado
        // (una abreviatura de fish).
        assert_eq!(
            extract(
                "web ~ ⟩ git checkout main",
                Some("web ~ ⟩ "),
                true,
                Some("gco main")
            ),
            Some("git checkout main".to_string())
        );
        assert_eq!(
            extract("web ~ ⟩ ls", Some("web ~ ⟩ "), true, Some("ls")),
            Some("ls".to_string())
        );
        assert_eq!(extract("web ~ ⟩", Some("web ~ ⟩ "), true, Some("")), None);
    }

    /// Una sesión de mentira: lo que hay en la línea del cursor y el
    /// rastreador, con el eco llegando cuando se le dice.
    struct Session {
        tracker: Tracker,
        line: String,
        now: Instant,
    }

    impl Session {
        fn new(prompt: &str) -> Self {
            Self {
                tracker: Tracker::default(),
                line: prompt.to_string(),
                now: Instant::now(),
            }
        }

        /// El usuario teclea texto corriente y la shell lo repite.
        fn type_text(&mut self, text: &str, echo: bool) {
            for c in text.chars() {
                let line = self.line.clone();
                self.tracker
                    .input(c.to_string().as_bytes(), true, || line, self.now);
                if echo {
                    self.line.push(c);
                }
            }
        }

        fn key(&mut self, bytes: &[u8]) {
            let line = self.line.clone();
            self.tracker.input(bytes, true, || line, self.now);
        }

        /// Intro, y la shell contesta con el salto de línea y `next` (el
        /// indicador siguiente). Devuelve la orden reconocida.
        fn enter(&mut self, next: &str) -> Option<String> {
            self.key(b"\r");
            let output = format!("\r\n{next}");
            let at = self.tracker.echo_end(output.as_bytes())?;
            assert_eq!(&output.as_bytes()[..at], b"\r");
            let command = self.tracker.resolve(&self.line, true);
            self.line = next.to_string();
            command
        }
    }

    #[test]
    fn the_tracker_follows_typing_at_a_prompt() {
        let mut s = Session::new("ana@web:~$ ");
        s.type_text("ls -la", true);
        assert!(!s.tracker.is_waiting());
        assert_eq!(s.tracker.last_prompt(), None);
        assert_eq!(s.enter("ana@web:~$ "), Some("ls -la".to_string()));
        assert!(!s.tracker.is_waiting());
        assert_eq!(s.tracker.last_prompt(), Some("ana@web:~$ "));
        // Intro a secas.
        assert_eq!(s.enter("ana@web:~$ "), None);
        // Una contraseña: se teclea, no hay eco, no se guarda.
        s.type_text("sudo -s", true);
        assert_eq!(
            s.enter("[sudo] password for ana: "),
            Some("sudo -s".to_string())
        );
        s.type_text("hunter2", false);
        assert_eq!(s.enter("root@web:~# "), None);
        // Con el eco en asteriscos tampoco.
        let mut stars = Session::new("Password: ");
        for _ in 0..7 {
            stars.key(b"x");
            stars.line.push('*');
        }
        assert_eq!(stars.enter("$ "), None);
        // Una línea corregida con retrocesos: se lee de la pantalla.
        s.type_text("upitme", true);
        for _ in 0..4 {
            s.key(b"\x7f");
            s.line.pop();
        }
        s.type_text("time", true);
        assert_eq!(s.enter("root@web:~# "), Some("uptime".to_string()));
        // Flecha arriba: la shell pone la orden anterior.
        s.key(b"\x1b[A");
        s.line.push_str("uptime");
        assert_eq!(s.enter("root@web:~# "), Some("uptime".to_string()));
        // Ctrl+C abandona la línea; lo siguiente empieza de cero.
        s.type_text("rm -rf /tmp/x", true);
        s.key(&[0x03]);
        s.line = "root@web:~# ".into();
        s.type_text("ls", true);
        assert_eq!(s.enter("root@web:~# "), Some("ls".to_string()));
    }

    #[test]
    fn the_tracker_waits_for_the_whole_echo() {
        // Con retardo en la red, al pulsar Intro aún no se ve todo lo
        // tecleado: el eco llega después, a trozos.
        let mut tracker = Tracker::default();
        let now = Instant::now();
        let mut line = "ana@web:~$ ".to_string();
        for key in ["d", "f", " ", "-", "h"] {
            let before = line.clone();
            tracker.input(key.as_bytes(), true, || before, now);
        }
        line.push_str("df");
        tracker.input(b"\r", true, String::new, now);
        assert!(tracker.is_waiting());
        // Más eco sin el salto: todavía no.
        assert_eq!(tracker.echo_end(b" -"), None);
        line.push_str(" -");
        // El final del eco y, en el mismo bloque, la salida de la orden.
        let output = b"h\r\nFilesystem  Size\r\n";
        let at = tracker.echo_end(output).unwrap();
        assert_eq!(&output[..at], b"h\r");
        line.push('h');
        assert_eq!(tracker.resolve(&line, true), Some("df -h".to_string()));
        assert!(!tracker.is_waiting());
        assert_eq!(tracker.echo_end(b"mas\r\n"), None);

        // Dos órdenes tecleadas tan seguidas que el eco de la primera no
        // ha llegado al pulsar el segundo Intro: no se sabe cuál es cuál y
        // no se guarda ninguna (mejor eso que una equivocada).
        let mut tracker = Tracker::default();
        tracker.input(b"uno", true, || "$ ".into(), now);
        tracker.input(b"\r", true, String::new, now);
        tracker.input(b"dos", true, || "$ uno".into(), now);
        tracker.input(b"\r", true, String::new, now);
        assert_eq!(tracker.resolve("$ uno", true), None);
        assert!(!tracker.is_waiting());
        assert_eq!(tracker.resolve("$ dos", true), None);
        // Una lectura sin eco no contesta con un salto de línea: su Intro
        // se queda sin eco, y el indicador siguiente sale en la misma
        // línea. La orden que se teclee después es la que cuenta.
        let mut tracker = Tracker::default();
        tracker.input(b"hunter2", true, || "Valor: ".into(), now);
        tracker.input(b"\r", true, String::new, now);
        assert!(tracker.is_waiting());
        tracker.input(b"ls", true, || "Valor: ana@web:~$ ".into(), now);
        tracker.input(b"\r", true, String::new, now);
        assert_eq!(
            tracker.resolve("Valor: ana@web:~$ ls", true),
            Some("ls".to_string())
        );
        // Si el eco no llega, se desiste.
        tracker.input(b"tres\r", true, || "$ ".into(), now);
        assert!(tracker.is_waiting());
        tracker.expire(now + Duration::from_secs(1));
        assert!(tracker.is_waiting());
        tracker.expire(now + ECHO_TIMEOUT + Duration::from_secs(1));
        assert!(!tracker.is_waiting());
        // Lo que en ese momento hay en pantalla no es una línea.
        tracker.input(b"vim\r", true, || "$ ".into(), now);
        assert_eq!(tracker.resolve("~", false), None);
        assert!(!tracker.is_waiting());
        // Al reconectar se olvida lo pendiente.
        tracker.input(b"x\r", true, || "$ ".into(), now);
        tracker.reset();
        assert!(!tracker.is_waiting());
    }

    #[test]
    fn programs_and_pastes_are_not_mistaken_for_commands() {
        let now = Instant::now();
        // Lo que se teclea a un programa (según los marcadores de la
        // shell, o a pantalla completa) no es una orden.
        let mut tracker = Tracker::default();
        tracker.input(b"respuesta\r", false, || unreachable!(), now);
        assert!(!tracker.is_waiting());
        // Un pegado de varias líneas: solo la primera se puede reconocer.
        let mut tracker = Tracker::default();
        tracker.input(b"uno\rdos\rtres\r", true, || "$ ".into(), now);
        assert_eq!(tracker.resolve("$ uno", true), Some("uno".to_string()));
        assert!(!tracker.is_waiting());
        // Un pegado entre corchetes de varias líneas no se guarda, ni al
        // pulsar Intro después.
        let mut tracker = Tracker::default();
        tracker.input(b"\x1b[200~uno\rdos\x1b[201~", true, || "$ ".into(), now);
        assert!(!tracker.is_waiting());
        tracker.input(b"\r", true, String::new, now);
        assert!(!tracker.is_waiting());
        // De una línea, sí: se lee de la pantalla.
        tracker.input(b"\x1b[200~ls /tmp\x1b[201~", true, || "$ ".into(), now);
        tracker.input(b"\r", true, String::new, now);
        assert_eq!(
            tracker.resolve("$ ls /tmp", true),
            Some("ls /tmp".to_string())
        );
        // Una línea desmedida deja de compararse, pero no se acumula.
        let mut tracker = Tracker::default();
        tracker.input(b"", true, || "$ ".into(), now);
        for _ in 0..10 {
            tracker.input("x".repeat(1000).as_bytes(), true, String::new, now);
        }
        assert!(tracker.typed.is_none());
    }

    #[test]
    fn shell_marks_give_the_exact_prompt() {
        let now = Instant::now();
        let mut tracker = Tracker::default();
        tracker.mark(&Mark::PromptStart, None);
        tracker.mark(&Mark::PromptEnd, Some("web ~ ⟩ ".into()));
        // No hace falta mirar la pantalla al empezar a teclear.
        tracker.input(b"ls", true, || unreachable!(), now);
        tracker.input(b"\r", true, || unreachable!(), now);
        assert_eq!(tracker.resolve("web ~ ⟩ ls", true), Some("ls".to_string()));
        // Un indicador nuevo descarta lo que hubiera a medias.
        tracker.mark(&Mark::PromptEnd, Some("web ~ ⟩ ".into()));
        tracker.input(b"a medias", true, || unreachable!(), now);
        tracker.mark(&Mark::PromptStart, None);
        tracker.mark(&Mark::PromptEnd, Some("web /tmp ⟩ ".into()));
        tracker.input(b"pwd\r", true, || unreachable!(), now);
        assert_eq!(
            tracker.resolve("web /tmp ⟩ pwd", true),
            Some("pwd".to_string())
        );
        // Si la shell no marca el final del indicador, se mira la pantalla.
        tracker.mark(&Mark::PromptStart, None);
        tracker.input(b"id\r", true, || "$ ".into(), now);
        assert_eq!(tracker.resolve("$ id", true), Some("id".to_string()));
    }

    #[test]
    fn the_store_keeps_recent_commands_first() {
        let mut store = HistoryStore::default();
        assert!(store.is_empty() && !store.is_dirty());
        assert!(store.record("web", "web-01", "ls -la", 100, 500));
        assert!(store.record("web", "web-01", "uptime", 200, 500));
        assert!(store.record("bd", "bd-01", "psql", 300, 500));
        assert!(store.is_dirty());
        // Repetida: sube al principio y cuenta una vez más.
        assert!(store.record("web", "web-01 (nuevo nombre)", "ls -la  ", 400, 500));
        let web = store.host("web").unwrap();
        assert_eq!(web.name, "web-01 (nuevo nombre)");
        assert_eq!(
            web.entries,
            [
                Entry {
                    command: "ls -la".into(),
                    count: 2,
                    last: 400
                },
                Entry {
                    command: "uptime".into(),
                    count: 1,
                    last: 200
                },
            ]
        );
        assert!(store.entries("no-existe").is_empty());
        assert_eq!(
            store.hosts().collect::<Vec<_>>(),
            [("bd", "bd-01"), ("web", "web-01 (nuevo nombre)")]
        );
        // Lo que no debe guardarse no se guarda.
        assert!(!store.record("web", "", " con espacio delante", 500, 500));
        assert!(!store.record("web", "", "", 500, 500));
        assert!(!store.record("", "", "ls", 500, 500));
        assert_eq!(store.entries("web").len(), 2);

        // Búsqueda: sin acentos ni mayúsculas, la mejor primero.
        store.record("web", "", "systemctl restart nginx", 600, 500);
        store.record("web", "", "journalctl -u nginx", 700, 500);
        let found: Vec<&str> = store
            .search("web", "NGINX rest")
            .iter()
            .map(|e| e.command.as_str())
            .collect();
        assert_eq!(found, ["systemctl restart nginx"]);
        assert_eq!(store.search("web", "").len(), 4);
        assert_eq!(
            store.search("web", "").first().unwrap().command,
            "journalctl -u nginx"
        );
        assert!(store.search("web", "no está").is_empty());

        // Borrar una orden, un equipo o todo.
        assert!(store.remove("web", "uptime"));
        assert!(!store.remove("web", "uptime"));
        assert!(!store.remove("otro", "uptime"));
        assert_eq!(store.entries("web").len(), 3);
        assert!(store.remove("bd", "psql"));
        assert!(store.host("bd").is_none());
        assert!(store.clear_host("web"));
        assert!(!store.clear_host("web"));
        assert!(store.is_empty());
        store.record("a", "", "x", 1, 500);
        store.record("b", "", "y", 1, 500);
        store.retain_hosts(|key| key == "b");
        assert_eq!(store.hosts().map(|(k, _)| k).collect::<Vec<_>>(), ["b"]);
        store.clear();
        assert!(store.is_empty());
    }

    #[test]
    fn the_store_has_a_size_limit() {
        let mut store = HistoryStore::default();
        for n in 0..100 {
            store.record("web", "", &format!("orden {n}"), n, MIN_PER_HOST);
        }
        let entries = store.entries("web");
        assert_eq!(entries.len(), MIN_PER_HOST as usize);
        assert_eq!(entries[0].command, "orden 99");
        assert_eq!(entries.last().unwrap().command, "orden 80");
        // Un máximo absurdo se acota.
        for n in 0..(MIN_PER_HOST + 5) {
            store.record("otro", "", &format!("orden {n}"), 0, 0);
        }
        assert_eq!(store.entries("otro").len(), MIN_PER_HOST as usize);
        // Y hay un tope de equipos.
        let mut store = HistoryStore::default();
        for n in 0..MAX_HOSTS + 10 {
            store.record(&format!("equipo-{n}"), "", "ls", 0, 500);
        }
        assert_eq!(store.hosts().count(), MAX_HOSTS);
        // En uno que ya está sí se sigue anotando.
        assert!(store.record("equipo-0", "", "pwd", 1, 500));
    }

    #[test]
    fn the_file_is_private_and_survives_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("command_history.json");
        // Sin fichero: vacío.
        assert!(HistoryStore::load(&path).is_empty());
        let mut store = HistoryStore::default();
        store.record("web", "web-01", "ls -la", 100, 500);
        store.record("web", "web-01", "echo 'año' | grep ñ", 200, 500);
        store.record(LOCAL_KEY, "Terminal local", "cargo test", 300, 500);
        store.save(&path).unwrap();
        assert!(!store.is_dirty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"equipos\"") && text.contains("\"orden\": \"ls -la\""));
        assert_eq!(HistoryStore::load(&path), store);

        // Un fichero retocado a mano o dañado no rompe nada.
        std::fs::write(&path, "esto no es JSON").unwrap();
        assert!(HistoryStore::load(&path).is_empty());
        std::fs::write(&path, "[1, 2, 3]").unwrap();
        assert!(HistoryStore::load(&path).is_empty());
        std::fs::write(
            &path,
            format!(
                r#"{{"equipos": {{
                    "web": {{"nombre": "web", "ordenes": [
                        {{"orden": "bien"}},
                        {{"orden": " con espacio"}},
                        {{"orden": "con\u001bescape"}},
                        {{"orden": "{}"}},
                        {{"orden": "también bien", "veces": 7, "ultima": 9}}
                    ]}},
                    "": {{"ordenes": [{{"orden": "sin clave"}}]}},
                    "vacío": {{"ordenes": []}}
                }}}}"#,
                "x".repeat(MAX_COMMAND_CHARS + 1)
            ),
        )
        .unwrap();
        let loaded = HistoryStore::load(&path);
        assert_eq!(loaded.hosts().map(|(k, _)| k).collect::<Vec<_>>(), ["web"]);
        let commands: Vec<&str> = loaded
            .entries("web")
            .iter()
            .map(|e| e.command.as_str())
            .collect();
        assert_eq!(commands, ["bien", "también bien"]);
        assert_eq!(loaded.entries("web")[1].count, 7);
        // Una carpeta en lugar del fichero.
        let folder = dir.path().join("carpeta");
        std::fs::create_dir(&folder).unwrap();
        assert!(HistoryStore::load(&folder).is_empty());
    }

    #[test]
    fn settings_switch_the_history_off() {
        let mut settings = HistorySettings::default();
        assert!(settings.enabled && settings.records("web") && settings.records(LOCAL_KEY));
        settings.set_excluded("web", true);
        settings.set_excluded("web", true);
        assert_eq!(settings.excluded, ["web"]);
        assert!(!settings.records("web") && settings.is_excluded("web"));
        assert!(settings.records("bd"));
        settings.set_excluded("web", false);
        assert!(settings.records("web") && settings.excluded.is_empty());
        settings.enabled = false;
        assert!(!settings.records("web") && !settings.is_excluded("web"));

        // En español en el fichero, y los ficheros antiguos cargan.
        let stored = HistorySettings {
            enabled: false,
            max_per_host: 50,
            excluded: vec!["local".into()],
        };
        let json = serde_json::to_value(&stored).unwrap();
        assert_eq!(json["activo"], false);
        assert_eq!(json["maximo"], 50);
        assert_eq!(json["equipos_excluidos"][0], "local");
        assert_eq!(
            serde_json::from_value::<HistorySettings>(json).unwrap(),
            stored
        );
        assert_eq!(
            serde_json::from_str::<HistorySettings>("{}").unwrap(),
            HistorySettings::default()
        );
        let mut wild = HistorySettings {
            enabled: true,
            max_per_host: 0,
            excluded: vec![
                "b".into(),
                String::new(),
                "a".into(),
                "b".into(),
                "x".repeat(100),
            ],
        };
        wild.sanitize();
        assert_eq!(wild.max_per_host, MIN_PER_HOST);
        assert_eq!(wild.excluded, ["a", "b"]);
    }

    #[test]
    fn times_read_well() {
        let now = 1_800_000_000;
        assert_eq!(ago(now, now), "ahora");
        assert_eq!(ago(now + 50, now), "ahora");
        assert_eq!(ago(now - 300, now), "hace 5 min");
        assert_eq!(ago(now - 7200, now), "hace 2 h");
        assert_eq!(ago(now - 90_000, now), "ayer");
        assert_eq!(ago(now - 5 * 86_400, now), "hace 5 días");
        assert_eq!(ago(now - 400 * 86_400, now).len(), 10);
    }
}
