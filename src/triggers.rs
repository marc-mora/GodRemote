//! Disparadores: reglas «cuando en la salida de un terminal aparezca este
//! texto, hacer algo» (avisar o enviar una respuesta).
//!
//! Las reglas se guardan con los ajustes. Cada terminal lleva un
//! [`Watcher`] que convierte su salida en líneas y decide qué reglas se
//! disparan, con varias salvaguardas:
//!
//! - cada regla tiene un periodo de enfriamiento por terminal;
//! - la línea en la que se ha escrito algo (el usuario, un snippet o un
//!   disparador) no se vuelve a mirar hasta que termina, y el texto que
//!   envía un disparador se ignora si reaparece poco después: el eco de lo
//!   enviado no dispara nada;
//! - una regla que envía texto solo reacciona a la línea en la que espera
//!   el cursor (una pregunta), cuando lleva un momento quieta, y nunca a lo
//!   que pasa de largo en un listado ni a una aplicación a pantalla completa;
//! - si una regla que envía texto se dispara demasiadas veces seguidas, se
//!   pone en pausa en ese terminal.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::session_log::Plain;

pub const MAX_RULES: usize = 64;
pub const MAX_NAME_CHARS: usize = 80;
pub const MAX_PATTERN_CHARS: usize = 200;
pub const MAX_SEND_CHARS: usize = 400;
/// Enfriamiento mínimo, en segundos, de una regla que envía texto.
pub const MIN_SEND_COOLDOWN: u32 = 2;
pub const MAX_COOLDOWN: u32 = 86_400;
/// De una línea muy larga solo se mira el final.
const MAX_LINE_CHARS: usize = 4000;
/// Líneas completas que se guardan como mucho entre dos evaluaciones (con
/// una salida torrencial se miran las últimas).
const MAX_WAITING_LINES: usize = 2000;
/// Tiempo que la línea del cursor debe llevar sin cambios para que una
/// regla que envía texto la dé por una pregunta que espera respuesta.
pub const SETTLE: Duration = Duration::from_millis(300);
/// Tiempo durante el que se recuerda lo enviado para no reaccionar a su eco.
const ECHO_WINDOW: Duration = Duration::from_secs(5);
/// Envíos de una misma regla en un minuto a partir de los cuales se pone
/// en pausa en ese terminal.
pub const MAX_SENDS_PER_MINUTE: usize = 6;

/// A qué terminales se aplica una regla.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    /// Todos, incluidos los terminales locales.
    #[default]
    #[serde(rename = "todos")]
    All,
    /// Los equipos de una carpeta (y de sus subcarpetas).
    #[serde(rename = "carpeta")]
    Folder(Uuid),
    /// Un equipo.
    #[serde(rename = "equipo")]
    Session(Uuid),
}

/// Una regla.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Trigger {
    pub id: Uuid,
    #[serde(rename = "nombre")]
    pub name: String,
    #[serde(rename = "activo")]
    pub enabled: bool,
    /// Texto que la dispara.
    #[serde(rename = "texto")]
    pub pattern: String,
    /// `*` vale por cualquier texto y `?` por un carácter.
    #[serde(rename = "comodines")]
    pub wildcards: bool,
    #[serde(rename = "mayusculas")]
    pub case_sensitive: bool,
    #[serde(rename = "ambito")]
    pub scope: Scope,
    /// Mostrar un aviso y marcar la pestaña.
    #[serde(rename = "avisar")]
    pub notify: bool,
    /// Marcar además la pestaña con la campana y pedir atención.
    #[serde(rename = "campana")]
    pub bell: bool,
    /// Texto que se envía al terminal, en la forma editable de los snippets
    /// (un salto de línea es Intro). `None`: la regla no envía nada.
    #[serde(rename = "enviar", skip_serializing_if = "Option::is_none")]
    pub send: Option<String>,
    /// Segundos que deben pasar antes de que vuelva a dispararse en el
    /// mismo terminal.
    #[serde(rename = "enfriamiento")]
    pub cooldown_secs: u32,
}

impl Default for Trigger {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: String::new(),
            enabled: true,
            pattern: String::new(),
            wildcards: false,
            case_sensitive: false,
            scope: Scope::All,
            notify: true,
            bell: false,
            send: None,
            cooldown_secs: 10,
        }
    }
}

impl Trigger {
    /// La regla escribe en el terminal.
    pub fn sends(&self) -> bool {
        self.send.as_ref().is_some_and(|text| !text.is_empty())
    }

    /// Nombre para las listas: el suyo o, si no tiene, su texto.
    pub fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            self.pattern.trim().to_string()
        } else {
            self.name.trim().to_string()
        }
    }

    /// Enfriamiento efectivo: las reglas que envían texto tienen un mínimo.
    pub fn cooldown(&self) -> Duration {
        let min = if self.sends() { MIN_SEND_COOLDOWN } else { 0 };
        Duration::from_secs(u64::from(self.cooldown_secs.clamp(min, MAX_COOLDOWN)))
    }

    fn compiled_pattern(&self) -> Option<Pattern> {
        Pattern::new(&self.pattern, self.wildcards, self.case_sensitive)
    }

    /// Comprueba que la regla se puede usar.
    pub fn validate(&self) -> Result<(), String> {
        if self.pattern.trim().is_empty() {
            return Err("Falta el texto que dispara la regla".into());
        }
        if self.pattern.chars().count() > MAX_PATTERN_CHARS {
            return Err(format!(
                "El texto es demasiado largo (máximo {MAX_PATTERN_CHARS} caracteres)"
            ));
        }
        let Some(pattern) = self.compiled_pattern() else {
            return Err("El texto debe tener algo más que comodines".into());
        };
        if !self.notify && !self.sends() {
            return Err("La regla no hace nada: marca «Avisar» o «Enviar un texto»".into());
        }
        if let Some(text) = &self.send {
            if text.chars().count() > MAX_SEND_CHARS {
                return Err(format!(
                    "El texto que se envía es demasiado largo (máximo {MAX_SEND_CHARS} caracteres)"
                ));
            }
            // El eco de la respuesta volvería a disparar la regla.
            let echo = String::from_utf8_lossy(&crate::macros::decode(text)).into_owned();
            if echo
                .split(['\r', '\n'])
                .any(|line| pattern.is_match(&Line::new(line)))
            {
                return Err(
                    "Lo que envía contiene el texto que la dispara: se respondería a sí misma"
                        .into(),
                );
            }
        }
        Ok(())
    }
}

/// El aviso incorporado de fin de orden larga.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LongCommand {
    #[serde(rename = "activo")]
    pub enabled: bool,
    /// Duración mínima, en segundos, para avisar.
    #[serde(rename = "segundos")]
    pub seconds: u32,
    #[serde(rename = "campana")]
    pub bell: bool,
}

impl Default for LongCommand {
    fn default() -> Self {
        Self {
            enabled: true,
            seconds: 30,
            bell: false,
        }
    }
}

impl LongCommand {
    pub const MIN_SECONDS: u32 = 5;
    pub const MAX_SECONDS: u32 = 86_400;

    /// Duración a partir de la cual se avisa; `None` si está apagado.
    pub fn threshold(&self) -> Option<Duration> {
        self.enabled.then(|| {
            Duration::from_secs(u64::from(
                self.seconds.clamp(Self::MIN_SECONDS, Self::MAX_SECONDS),
            ))
        })
    }
}

/// Lo que se guarda en los ajustes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TriggerSettings {
    #[serde(rename = "reglas")]
    pub rules: Vec<Trigger>,
    #[serde(rename = "orden_larga")]
    pub long_command: LongCommand,
}

impl TriggerSettings {
    /// Recorta lo que venga desmedido de un fichero editado a mano.
    pub fn sanitize(&mut self) {
        self.rules.truncate(MAX_RULES);
        let clip = |text: &mut String, max: usize| {
            if text.chars().count() > max {
                *text = text.chars().take(max).collect();
            }
        };
        for rule in &mut self.rules {
            clip(&mut rule.name, MAX_NAME_CHARS);
            clip(&mut rule.pattern, MAX_PATTERN_CHARS);
            if let Some(text) = &mut rule.send {
                clip(text, MAX_SEND_CHARS);
            }
            rule.cooldown_secs = rule.cooldown_secs.min(MAX_COOLDOWN);
        }
        self.long_command.seconds = self
            .long_command
            .seconds
            .clamp(LongCommand::MIN_SECONDS, LongCommand::MAX_SECONDS);
    }
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token {
    Char(char),
    /// `?`: un carácter cualquiera.
    One,
    /// `*`: cualquier texto, también ninguno.
    Many,
}

/// Texto de una regla, preparado para buscarlo en una línea.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    tokens: Vec<Token>,
    case_sensitive: bool,
}

impl Pattern {
    /// Prepara el texto de una regla. Los espacios del principio y del final
    /// no cuentan. Con `wildcards`, `*` y `?` son comodines (`\*`, `\?` y
    /// `\\` son los caracteres tal cual). Devuelve `None` si no queda ningún
    /// carácter concreto que buscar.
    pub fn new(text: &str, wildcards: bool, case_sensitive: bool) -> Option<Self> {
        let mut tokens: Vec<Token> = vec![];
        let mut chars = text.trim().chars().take(MAX_PATTERN_CHARS).peekable();
        while let Some(c) = chars.next() {
            let token = match c {
                '\\' if wildcards && matches!(chars.peek(), Some('*' | '?' | '\\')) => {
                    Token::Char(chars.next().unwrap_or('\\'))
                }
                '*' if wildcards => Token::Many,
                '?' if wildcards => Token::One,
                c if case_sensitive => Token::Char(c),
                c => Token::Char(lower(c)),
            };
            if token == Token::Many && tokens.last() == Some(&Token::Many) {
                continue;
            }
            tokens.push(token);
        }
        // Se busca en cualquier parte de la línea: un `*` en un extremo sobra.
        while tokens.last() == Some(&Token::Many) {
            tokens.pop();
        }
        let leading = tokens.iter().take_while(|t| **t == Token::Many).count();
        tokens.drain(..leading);
        tokens
            .iter()
            .any(|t| matches!(t, Token::Char(_)))
            .then_some(Self {
                tokens,
                case_sensitive,
            })
    }

    /// El texto aparece en la línea.
    pub fn is_match(&self, line: &Line) -> bool {
        let text: &[char] = if self.case_sensitive {
            &line.exact
        } else {
            &line.lower
        };
        let pattern = &self.tokens;
        // Como un `*texto*`: el último comodín `*` visto (al principio hay
        // uno implícito) puede tragarse un carácter más y volver a probar.
        let (mut at, mut token) = (0, 0);
        let (mut star_token, mut star_at) = (0, 0);
        loop {
            if token == pattern.len() {
                return true;
            }
            let advanced = match (pattern[token], text.get(at)) {
                (Token::Many, _) => {
                    token += 1;
                    star_token = token;
                    star_at = at;
                    true
                }
                (Token::One, Some(_)) => {
                    token += 1;
                    at += 1;
                    true
                }
                (Token::Char(wanted), Some(found)) if wanted == *found => {
                    token += 1;
                    at += 1;
                    true
                }
                _ => false,
            };
            if !advanced {
                if star_at >= text.len() {
                    return false;
                }
                star_at += 1;
                at = star_at;
                token = star_token;
            }
        }
    }
}

/// Una línea de la salida, lista para compararla con las reglas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    exact: Vec<char>,
    lower: Vec<char>,
}

impl Line {
    pub fn new(text: &str) -> Self {
        let count = text.chars().count();
        let exact: Vec<char> = text
            .chars()
            .skip(count.saturating_sub(MAX_LINE_CHARS))
            .collect();
        let lower = exact.iter().copied().map(lower).collect();
        Self { exact, lower }
    }

    fn is_blank(&self) -> bool {
        self.exact.iter().all(|c| c.is_whitespace())
    }

    /// La línea sin la última aparición de cada uno de estos textos.
    fn without(&self, texts: &[(Vec<char>, Instant)]) -> Self {
        let mut masked = self.clone();
        for (text, _) in texts {
            if text.is_empty() || text.len() > masked.exact.len() {
                continue;
            }
            let found = (0..=masked.exact.len() - text.len())
                .rev()
                .find(|start| masked.exact[*start..*start + text.len()] == text[..]);
            if let Some(start) = found {
                masked.exact[start..start + text.len()].fill('\0');
                masked.lower[start..start + text.len()].fill('\0');
            }
        }
        masked
    }
}

struct CompiledRule {
    id: Uuid,
    name: String,
    pattern: Pattern,
    scope: Scope,
    notify: bool,
    bell: bool,
    send: Option<Vec<u8>>,
    cooldown: Duration,
}

/// Las reglas activas, preparadas para aplicarlas a la salida.
pub struct Compiled {
    /// Cambia cada vez que se preparan unas reglas.
    pub version: u64,
    rules: Vec<CompiledRule>,
    pub long_command: LongCommand,
}

static VERSION: AtomicU64 = AtomicU64::new(1);

impl Compiled {
    pub fn new(settings: &TriggerSettings) -> Self {
        let rules = settings
            .rules
            .iter()
            .take(MAX_RULES)
            .filter(|rule| rule.enabled && rule.validate().is_ok())
            .filter_map(|rule| {
                Some(CompiledRule {
                    id: rule.id,
                    name: rule.display_name(),
                    pattern: rule.compiled_pattern()?,
                    scope: rule.scope,
                    notify: rule.notify,
                    bell: rule.bell,
                    send: rule
                        .send
                        .as_ref()
                        .filter(|text| !text.is_empty())
                        .map(|text| crate::macros::decode(text)),
                    cooldown: rule.cooldown(),
                })
            })
            .collect();
        Self {
            version: VERSION.fetch_add(1, Ordering::Relaxed),
            rules,
            long_command: settings.long_command,
        }
    }

    /// No hay reglas que aplicar a la salida.
    pub fn is_idle(&self) -> bool {
        self.rules.is_empty()
    }
}

impl Default for Compiled {
    fn default() -> Self {
        Self::new(&TriggerSettings::default())
    }
}

/// Una regla se ha disparado en un terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fired {
    pub rule: Uuid,
    pub name: String,
    /// La línea que la disparó.
    pub line: String,
    pub notify: bool,
    pub bell: bool,
    /// Bytes que hay que enviar al terminal.
    pub send: Option<Vec<u8>>,
    /// La regla iba a enviar su texto otra vez, pero lo ha hecho tantas
    /// veces seguidas que queda en pausa en este terminal.
    pub paused: bool,
}

struct DoneLine {
    text: String,
    /// En ella se escribió algo: no se mira.
    tainted: bool,
    /// Reglas que ya se dispararon con ella cuando aún estaba a medias.
    fired: Vec<Uuid>,
}

/// Vigila la salida de un terminal y decide qué reglas se disparan.
#[derive(Default)]
pub struct Watcher {
    plain: Plain,
    /// Líneas completas aún sin evaluar.
    done: VecDeque<DoneLine>,
    /// En la línea en curso se ha escrito algo (el usuario o un disparador).
    tainted: bool,
    /// Reglas ya disparadas con la línea en curso.
    fired_here: Vec<Uuid>,
    /// Ha llegado salida desde la última evaluación.
    dirty: bool,
    /// Hay reglas que envían texto esperando a que la línea se aquiete.
    unsettled: bool,
    /// Cuándo llegó la última salida.
    changed_at: Option<Instant>,
    last_fired: HashMap<Uuid, Instant>,
    /// Envíos recientes de cada regla.
    sends: HashMap<Uuid, VecDeque<Instant>>,
    /// Reglas en pausa en este terminal.
    paused: Vec<Uuid>,
    /// Textos enviados hace poco: su eco no cuenta.
    echoes: Vec<(Vec<char>, Instant)>,
    /// Versión de las reglas a la que corresponden las pausas.
    version: u64,
}

impl Watcher {
    /// Salida recibida del terminal.
    pub fn feed(&mut self, bytes: &[u8], now: Instant) {
        if bytes.is_empty() {
            return;
        }
        for mut text in self.plain.feed(bytes) {
            // De una línea desmedida solo se guarda lo que se va a mirar.
            let extra = text.chars().count().saturating_sub(MAX_LINE_CHARS);
            if extra > 0 {
                text = text.chars().skip(extra).collect();
            }
            self.done.push_back(DoneLine {
                text,
                tainted: std::mem::take(&mut self.tainted),
                fired: std::mem::take(&mut self.fired_here),
            });
            if self.done.len() > MAX_WAITING_LINES {
                self.done.pop_front();
            }
        }
        self.dirty = true;
        self.changed_at = Some(now);
    }

    /// Se ha enviado algo al terminal (teclado, pegado, snippet o
    /// disparador): la línea en curso ya no se mira, porque lo que aparezca
    /// en ella es su eco, y lo enviado se recuerda un momento por si su eco
    /// llega en otra línea.
    pub fn note_sent(&mut self, bytes: &[u8], now: Instant) {
        if bytes.is_empty() {
            return;
        }
        self.tainted = true;
        self.unsettled = false;
        let text = String::from_utf8_lossy(bytes);
        for line in text.split(['\r', '\n']) {
            let line: Vec<char> = line
                .trim()
                .chars()
                .filter(|c| !c.is_control())
                .take(MAX_SEND_CHARS)
                .collect();
            // Una tecla suelta no se recuerda: borraría esa letra de
            // cualquier línea.
            if line.len() >= 2 {
                self.echoes.push((line, now));
            }
        }
        let excess = self.echoes.len().saturating_sub(32);
        self.echoes.drain(..excess);
    }

    /// Cuánto falta para que haya que volver a evaluar (una regla que
    /// envía texto espera a que la línea del cursor se aquiete).
    pub fn next_check(&self, now: Instant) -> Option<Duration> {
        if !self.unsettled {
            return None;
        }
        let since = now.saturating_duration_since(self.changed_at?);
        Some(SETTLE.saturating_sub(since))
    }

    /// Evalúa las reglas sobre lo recibido desde la última llamada.
    ///
    /// `in_scope` dice si una regla con ese ámbito vale para este terminal;
    /// `pending` es la línea en la que está el cursor, y `full_screen`
    /// indica que hay una aplicación a pantalla completa (lo que escribe no
    /// son líneas: no se mira).
    pub fn evaluate(
        &mut self,
        rules: &Compiled,
        in_scope: &dyn Fn(&Scope) -> bool,
        pending: &str,
        full_screen: bool,
        now: Instant,
    ) -> Vec<Fired> {
        if self.version != rules.version {
            self.version = rules.version;
            self.paused.clear();
            self.sends.clear();
        }
        self.echoes
            .retain(|(_, at)| now.saturating_duration_since(*at) < ECHO_WINDOW);
        let done = std::mem::take(&mut self.done);
        let dirty = std::mem::take(&mut self.dirty);
        let settled = self
            .changed_at
            .is_none_or(|at| now.saturating_duration_since(at) >= SETTLE);
        // Una regla que envía texto esperaba a que la línea se aquietara.
        let recheck = self.unsettled && settled;
        if recheck || full_screen {
            self.unsettled = false;
        }
        if full_screen {
            return vec![];
        }
        let active: Vec<&CompiledRule> = rules
            .rules
            .iter()
            .filter(|rule| !self.paused.contains(&rule.id) && in_scope(&rule.scope))
            .collect();
        if active.is_empty() {
            return vec![];
        }
        let mut fired = vec![];
        // Líneas completas: solo avisos. Lo que ya pasó de largo no es una
        // pregunta que contestar.
        for line in &done {
            if line.tainted {
                continue;
            }
            let text = Line::new(&line.text);
            if text.is_blank() {
                continue;
            }
            for rule in &active {
                if rule.send.is_none() && !line.fired.contains(&rule.id) {
                    self.try_fire(rule, &text, &line.text, now, &mut fired);
                }
            }
        }
        // La línea del cursor.
        if self.tainted || !(dirty || recheck) {
            return fired;
        }
        let text = Line::new(pending);
        if text.is_blank() {
            return fired;
        }
        let mut sent = false;
        for rule in &active {
            if self.fired_here.contains(&rule.id) {
                continue;
            }
            if rule.send.is_some() {
                // Solo responde una regla por pregunta.
                if sent {
                    continue;
                }
                if !settled {
                    self.unsettled = true;
                    continue;
                }
            }
            if self.try_fire(rule, &text, pending, now, &mut fired) {
                self.fired_here.push(rule.id);
                sent |= fired.last().is_some_and(|f| f.send.is_some());
            }
        }
        fired
    }

    /// Dispara la regla si la línea encaja y no está en su enfriamiento.
    fn try_fire(
        &mut self,
        rule: &CompiledRule,
        line: &Line,
        shown: &str,
        now: Instant,
        fired: &mut Vec<Fired>,
    ) -> bool {
        if !rule.pattern.is_match(line) {
            return false;
        }
        if !self.echoes.is_empty() && !rule.pattern.is_match(&line.without(&self.echoes)) {
            return false;
        }
        if self
            .last_fired
            .get(&rule.id)
            .is_some_and(|last| now.saturating_duration_since(*last) < rule.cooldown)
        {
            return false;
        }
        self.last_fired.insert(rule.id, now);
        let mut send = rule.send.clone();
        let mut paused = false;
        if send.is_some() {
            let times = self.sends.entry(rule.id).or_default();
            while times
                .front()
                .is_some_and(|at| now.saturating_duration_since(*at) > Duration::from_secs(60))
            {
                times.pop_front();
            }
            if times.len() >= MAX_SENDS_PER_MINUTE {
                // Algo va mal (un bucle): se deja de contestar.
                paused = true;
                send = None;
                self.paused.push(rule.id);
            } else {
                times.push_back(now);
            }
        }
        fired.push(Fired {
            rule: rule.id,
            name: rule.name.clone(),
            line: shown.trim().chars().take(200).collect(),
            notify: rule.notify,
            bell: rule.bell,
            send,
            paused,
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, wildcards: bool, case_sensitive: bool, line: &str) -> bool {
        Pattern::new(pattern, wildcards, case_sensitive)
            .expect("patrón válido")
            .is_match(&Line::new(line))
    }

    #[test]
    fn literal_text_is_found_anywhere_in_the_line() {
        assert!(matches("error", false, false, "ERROR: disco lleno"));
        assert!(matches("disco lleno", false, false, "ERROR: Disco Lleno."));
        assert!(!matches("error", false, true, "ERROR: disco lleno"));
        assert!(matches("ERROR", false, true, "un ERROR grave"));
        assert!(!matches("error", false, false, "todo bien"));
        // Sin comodines, los signos son texto corriente.
        assert!(matches("[y/N]?", false, false, "Continue [y/N]? "));
        assert!(!matches("[y/N]?", false, false, "Continue [y/N]: "));
        assert!(matches("*.log", false, false, "rm *.log"));
        assert!(!matches("*.log", false, false, "rm a.log"));
        // Los espacios de los extremos del texto no cuentan; los de dentro sí.
        assert!(matches("  password: ", false, false, "Password:"));
        assert!(!matches("a  b", false, false, "a b"));
        // Acentos y otros alfabetos.
        assert!(matches("CONTRASEÑA", false, false, "Contraseña: "));
        assert!(matches("ошибка", false, false, "ОШИБКА!"));
        // Línea vacía o más corta que el texto.
        assert!(!matches("error", false, false, ""));
        assert!(!matches("error", false, false, "err"));
    }

    #[test]
    fn simple_wildcards() {
        assert!(matches(
            "password for *:",
            true,
            false,
            "[sudo] password for ana: "
        ));
        assert!(!matches("password for *:", true, false, "password for ana"));
        assert!(matches(
            "error ?: *fatal",
            true,
            false,
            "ERROR 7: muy fatal"
        ));
        assert!(!matches("error ?: fatal", true, false, "ERROR 17: fatal"));
        // `?` es exactamente un carácter; `*` puede no ser ninguno.
        assert!(matches("a?c", true, false, "abc"));
        assert!(!matches("a?c", true, false, "ac"));
        assert!(matches("a*c", true, false, "ac"));
        assert!(matches("a*c", true, false, "a-----c"));
        // Varios `*`: hay que volver atrás para encajar.
        assert!(matches("a*b*c", true, false, "xxaxxbxxbxxcxx"));
        assert!(!matches("a*b*c", true, false, "xxaxxcxxbxx"));
        assert!(matches("ab*ab*c", true, false, "ababab-c"));
        assert!(matches("*fin", true, false, "el fin"));
        assert!(matches("fin*", true, false, "fin"));
        // Con barra invertida, los comodines son texto.
        assert!(matches(r"100\*", true, false, "son 100* de todo"));
        assert!(!matches(r"100\*", true, false, "son 1000"));
        assert!(matches(r"seguro\?", true, false, "¿seguro?"));
        assert!(!matches(r"seguro\?", true, false, "seguros"));
        assert!(matches(r"c:\\tmp", true, false, r"C:\tmp"));
        // Un patrón sin nada concreto no vale: encajaría con todo.
        assert!(Pattern::new("*", true, false).is_none());
        assert!(Pattern::new("?*?", true, false).is_none());
        assert!(Pattern::new("   ", false, false).is_none());
        assert!(Pattern::new("*", false, false).is_some());
    }

    #[test]
    fn hostile_lines_and_patterns_stay_cheap() {
        // Un patrón que con una búsqueda ingenua sería exponencial.
        let pattern = Pattern::new(&"a*".repeat(60), true, false).unwrap();
        let line = Line::new(&"a".repeat(100_000));
        assert_eq!(line.exact.len(), MAX_LINE_CHARS);
        let started = Instant::now();
        assert!(pattern.is_match(&line));
        let pattern = Pattern::new(&format!("{}b", "a*".repeat(60)), true, false).unwrap();
        assert!(!pattern.is_match(&line));
        assert!(started.elapsed() < Duration::from_secs(5));
        // De una línea enorme se mira el final.
        let line = Line::new(&format!("{}fin", "x".repeat(50_000)));
        assert!(Pattern::new("xfin", false, false).unwrap().is_match(&line));
        // Una salida torrencial sin evaluar no se acumula sin límite.
        let mut watcher = Watcher::default();
        let now = Instant::now();
        let flood = format!("{}\r\n", "x".repeat(10_000)).repeat(MAX_WAITING_LINES + 500);
        watcher.feed(flood.as_bytes(), now);
        watcher.feed(b"ERROR al final\r\n", now);
        assert_eq!(watcher.done.len(), MAX_WAITING_LINES);
        assert!(
            watcher
                .done
                .iter()
                .all(|line| line.text.chars().count() <= MAX_LINE_CHARS)
        );
        let rules = compile(vec![rule("error")]);
        let fired = watcher.evaluate(&rules, &|_| true, "", false, now);
        assert_eq!(fired.len(), 1);
        assert!(watcher.done.is_empty());
    }

    fn rule(pattern: &str) -> Trigger {
        Trigger {
            pattern: pattern.into(),
            cooldown_secs: 0,
            ..Default::default()
        }
    }

    fn answer(pattern: &str, send: &str) -> Trigger {
        Trigger {
            send: Some(send.into()),
            notify: false,
            ..rule(pattern)
        }
    }

    fn compile(rules: Vec<Trigger>) -> Compiled {
        Compiled::new(&TriggerSettings {
            rules,
            ..Default::default()
        })
    }

    /// Un terminal de mentira: su salida y lo que el disparador le envía.
    struct Terminal {
        watcher: Watcher,
        now: Instant,
        /// Línea en la que está el cursor.
        pending: String,
        sent: Vec<Vec<u8>>,
    }

    impl Terminal {
        fn new() -> Self {
            Self {
                watcher: Watcher::default(),
                now: Instant::now(),
                pending: String::new(),
                sent: vec![],
            }
        }

        fn output(&mut self, text: &str) {
            self.watcher.feed(text.as_bytes(), self.now);
            match text.rfind('\n') {
                Some(at) => self.pending = text[at + 1..].to_string(),
                None => self.pending.push_str(text),
            }
        }

        fn wait(&mut self, time: Duration) {
            self.now += time;
        }

        /// Evalúa y envía lo que toque, como hace el terminal de verdad.
        fn run(&mut self, rules: &Compiled) -> Vec<Fired> {
            let fired = self
                .watcher
                .evaluate(rules, &|_| true, &self.pending, false, self.now);
            for f in &fired {
                if let Some(bytes) = &f.send {
                    self.watcher.note_sent(bytes, self.now);
                    self.sent.push(bytes.clone());
                }
            }
            fired
        }

        fn names(&mut self, rules: &Compiled) -> Vec<String> {
            self.run(rules).into_iter().map(|f| f.name).collect()
        }
    }

    #[test]
    fn notices_fire_on_complete_and_pending_lines() {
        let rules = compile(vec![rule("error"), rule("password:")]);
        let mut term = Terminal::new();
        term.output("todo bien\r\nERROR: disco lleno\r\notra\r\n");
        let fired = term.run(&rules);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].name, "error");
        assert_eq!(fired[0].line, "ERROR: disco lleno");
        assert!(fired[0].notify && fired[0].send.is_none() && !fired[0].paused);
        // Sin salida nueva no se repite.
        assert!(term.run(&rules).is_empty());
        // Una pregunta sin salto de línea también avisa, y una sola vez
        // aunque la línea siga creciendo y luego termine.
        term.output("Password: ");
        assert_eq!(term.names(&rules), ["password:"]);
        term.output("  (3 intentos)");
        assert!(term.run(&rules).is_empty());
        term.output("\r\n");
        assert!(term.run(&rules).is_empty());
        // Una línea partida entre dos bloques se evalúa entera.
        term.output("un ERR");
        assert!(term.run(&rules).is_empty());
        term.output("OR partido\r\n");
        assert_eq!(term.names(&rules), ["error"]);
    }

    #[test]
    fn cooldown_is_per_rule() {
        let rules = compile(vec![
            Trigger {
                cooldown_secs: 10,
                ..rule("error")
            },
            rule("aviso"),
        ]);
        let mut term = Terminal::new();
        term.output("error 1\r\naviso 1\r\nerror 2\r\naviso 2\r\n");
        // La primera se dispara una vez; la otra, sin enfriamiento, las dos.
        assert_eq!(term.names(&rules), ["error", "aviso", "aviso"]);
        term.wait(Duration::from_secs(9));
        term.output("error 3\r\n");
        assert!(term.run(&rules).is_empty());
        term.wait(Duration::from_secs(1));
        term.output("error 4\r\n");
        assert_eq!(term.names(&rules), ["error"]);
        // El enfriamiento es de cada terminal.
        let mut other = Terminal::new();
        other.now = term.now;
        other.output("error en otro\r\n");
        assert_eq!(other.names(&rules), ["error"]);
    }

    #[test]
    fn answers_wait_for_a_settled_question() {
        let rules = compile(vec![answer("[y/N]", "y\n"), rule("continue")]);
        let mut term = Terminal::new();
        // La pregunta pasa de largo en un listado: no se contesta.
        term.output("Continue? [y/N]\r\nmás salida\r\n");
        assert_eq!(term.names(&rules), ["continue"]);
        assert!(term.sent.is_empty());
        // Recién escrita aún puede venir más salida detrás: se espera.
        term.output("Continue? [y/N] ");
        assert_eq!(term.names(&rules), ["continue"]);
        assert!(term.sent.is_empty());
        assert_eq!(term.watcher.next_check(term.now), Some(SETTLE));
        term.wait(Duration::from_millis(100));
        assert!(term.run(&rules).is_empty());
        assert_eq!(
            term.watcher.next_check(term.now),
            Some(SETTLE - Duration::from_millis(100))
        );
        // Quieta: es una pregunta. Se contesta una vez.
        term.wait(SETTLE);
        let fired = term.run(&rules);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].send.as_deref(), Some(&b"y\r"[..]));
        assert_eq!(term.sent, [b"y\r".to_vec()]);
        assert_eq!(term.watcher.next_check(term.now), None);
        term.wait(Duration::from_secs(60));
        assert!(term.run(&rules).is_empty());
        assert_eq!(term.sent.len(), 1);
        // Si mientras se esperaba llega más salida, no era una pregunta.
        let mut term = Terminal::new();
        term.output("Continue? [y/N] ");
        term.run(&rules);
        term.wait(Duration::from_millis(200));
        term.output("no, era un listado\r\notra línea\r\n");
        term.wait(SETTLE);
        term.run(&rules);
        assert!(term.sent.is_empty());
        // Una aplicación a pantalla completa no se contesta.
        let mut term = Terminal::new();
        term.output("Continue? [y/N] ");
        term.wait(SETTLE);
        let fired = term
            .watcher
            .evaluate(&rules, &|_| true, "Continue? [y/N] ", true, term.now);
        assert!(fired.is_empty());
    }

    #[test]
    fn the_echo_of_what_a_trigger_sends_never_fires_anything() {
        // La respuesta contiene texto que otra regla vigila, y ella misma
        // contesta a algo que aparece en su propio eco.
        let rules = compile(vec![
            answer("name:", "error-prone name\n"),
            rule("error"),
            answer("prone", "otra vez\n"),
        ]);
        let mut term = Terminal::new();
        term.output("Project name: ");
        term.wait(SETTLE);
        assert_eq!(term.names(&rules), ["name:"]);
        assert_eq!(term.sent, [b"error-prone name\r".to_vec()]);
        // El eco llega en la misma línea, a trozos y con su salto.
        term.output("error-pr");
        term.wait(SETTLE);
        assert!(term.run(&rules).is_empty());
        term.output("one name");
        term.wait(SETTLE);
        assert!(term.run(&rules).is_empty());
        term.output("\r\n");
        assert!(term.run(&rules).is_empty());
        // El programa repite lo recibido en otra línea, completa o a medias.
        term.output("Creating error-prone name\r\n");
        assert!(term.run(&rules).is_empty());
        term.output("Using error-prone name... ");
        term.wait(SETTLE);
        assert!(term.run(&rules).is_empty());
        assert_eq!(term.sent.len(), 1);
        // Pero un error de verdad en esa misma línea sí cuenta.
        term.output("\r\nerror-prone name: ERROR al crearlo\r\n");
        assert_eq!(term.names(&rules), ["error"]);
        // Pasado un rato, el texto vuelve a valer como cualquier otro.
        term.wait(ECHO_WINDOW);
        term.output("error-prone name\r\n");
        assert_eq!(term.names(&rules), ["error"]);
    }

    #[test]
    fn what_the_user_types_is_not_watched() {
        let rules = compile(vec![rule("error"), answer("sure?", "yes\n")]);
        let mut term = Terminal::new();
        term.output("ana@web:~$ ");
        term.run(&rules);
        // El usuario teclea «grep error log» y su eco va llegando.
        for key in "grep error log".bytes() {
            term.watcher.note_sent(&[key], term.now);
            term.output(std::str::from_utf8(&[key]).unwrap());
            assert!(term.run(&rules).is_empty());
        }
        term.watcher.note_sent(b"\r", term.now);
        term.output("\r\n");
        assert!(term.run(&rules).is_empty());
        // La salida de la orden sí se vigila.
        term.output("log:3: error de lectura\r\nana@web:~$ ");
        assert_eq!(term.names(&rules), ["error"]);
        // Tampoco se contesta a una pregunta que el usuario ya ha empezado
        // a contestar.
        term.output("\r\nAre you sure? ");
        term.watcher.note_sent(b"n", term.now);
        term.output("n");
        term.wait(SETTLE);
        assert!(term.run(&rules).is_empty());
        assert!(term.sent.is_empty());
    }

    #[test]
    fn a_looping_answer_is_paused() {
        let rules = compile(vec![Trigger {
            notify: true,
            ..answer("again?", "y\n")
        }]);
        let mut term = Terminal::new();
        let mut paused = 0;
        for _ in 0..20 {
            term.output("again? ");
            term.wait(Duration::from_secs(3));
            for fired in term.run(&rules) {
                if fired.paused {
                    paused += 1;
                    assert!(fired.send.is_none());
                }
            }
            term.output("y\r\n");
        }
        // Contesta unas cuantas veces, avisa una vez de la pausa y calla.
        assert_eq!(term.sent.len(), MAX_SENDS_PER_MINUTE);
        assert_eq!(paused, 1);
        // Al cambiar las reglas se le da otra oportunidad.
        let rules = compile(vec![answer("again?", "y\n")]);
        term.output("again? ");
        term.wait(Duration::from_secs(3));
        term.run(&rules);
        assert_eq!(term.sent.len(), MAX_SENDS_PER_MINUTE + 1);
    }

    #[test]
    fn only_one_rule_answers_a_question() {
        let rules = compile(vec![
            answer("delete", "n\n"),
            answer("[y/n]", "y\n"),
            rule("delete"),
        ]);
        let mut term = Terminal::new();
        term.output("Delete everything? [y/n] ");
        term.wait(SETTLE);
        let fired = term.run(&rules);
        // Contesta la primera; el aviso se da igualmente.
        assert_eq!(term.sent, [b"n\r".to_vec()]);
        assert_eq!(fired.len(), 2);
        term.wait(Duration::from_secs(30));
        term.output("n\r\n");
        term.run(&rules);
        assert_eq!(term.sent.len(), 1);
    }

    #[test]
    fn scope_and_disabled_rules() {
        let folder = Uuid::new_v4();
        let host = Uuid::new_v4();
        let rules = compile(vec![
            Trigger {
                name: "todos".into(),
                ..rule("error")
            },
            Trigger {
                name: "carpeta".into(),
                scope: Scope::Folder(folder),
                ..rule("error")
            },
            Trigger {
                name: "equipo".into(),
                scope: Scope::Session(host),
                ..rule("error")
            },
            Trigger {
                name: "apagada".into(),
                enabled: false,
                ..rule("error")
            },
            // Sin acción o sin texto: no llegan a compilarse.
            Trigger {
                notify: false,
                ..rule("error")
            },
            rule("  "),
        ]);
        assert_eq!(rules.rules.len(), 3);
        let fire = |in_scope: &dyn Fn(&Scope) -> bool| {
            let mut watcher = Watcher::default();
            let now = Instant::now();
            watcher.feed(b"error\r\n", now);
            watcher
                .evaluate(&rules, in_scope, "", false, now)
                .into_iter()
                .map(|f| f.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(fire(&|scope| *scope == Scope::All), ["todos"]);
        assert_eq!(
            fire(&|scope| match scope {
                Scope::All => true,
                Scope::Folder(id) => *id == folder,
                Scope::Session(_) => false,
            }),
            ["todos", "carpeta"]
        );
        assert_eq!(fire(&|_| true), ["todos", "carpeta", "equipo"]);
        assert!(fire(&|_| false).is_empty());
        assert!(Compiled::default().is_idle());
    }

    #[test]
    fn rules_are_validated() {
        assert!(rule("error").validate().is_ok());
        assert!(rule(" ").validate().is_err());
        assert!(
            Trigger {
                wildcards: true,
                ..rule("*?*")
            }
            .validate()
            .is_err()
        );
        assert!(
            Trigger {
                notify: false,
                ..rule("error")
            }
            .validate()
            .is_err()
        );
        assert!(answer("[y/N]", "y\n").validate().is_ok());
        // Se contestaría a sí misma con su propio eco.
        let looping = answer("yes", "yes\n");
        assert!(looping.validate().unwrap_err().contains("sí misma"));
        assert!(answer("y*s", "no\nYES\n").validate().is_ok());
        assert!(
            Trigger {
                wildcards: true,
                ..answer("y*s", "no\nYES\n")
            }
            .validate()
            .is_err()
        );
        assert!(rule(&"x".repeat(MAX_PATTERN_CHARS + 1)).validate().is_err());
        assert!(
            answer("a", &"x".repeat(MAX_SEND_CHARS + 1))
                .validate()
                .is_err()
        );
        // Enviar texto impone un enfriamiento mínimo.
        assert_eq!(rule("x").cooldown(), Duration::ZERO);
        assert_eq!(
            answer("x", "y").cooldown(),
            Duration::from_secs(u64::from(MIN_SEND_COOLDOWN))
        );
        // Un texto vacío no es enviar.
        assert!(!answer("x", "").sends());
    }

    #[test]
    fn settings_are_stored_in_spanish_and_old_files_load() {
        let folder = Uuid::new_v4();
        let settings = TriggerSettings {
            rules: vec![
                Trigger {
                    name: "Contraseña de sudo".into(),
                    wildcards: true,
                    scope: Scope::Folder(folder),
                    bell: true,
                    ..rule("password for *:")
                },
                answer("[y/N]", "y\n"),
            ],
            long_command: LongCommand {
                enabled: false,
                seconds: 120,
                bell: true,
            },
        };
        let json = serde_json::to_value(&settings).unwrap();
        assert_eq!(json["reglas"][0]["texto"], "password for *:");
        assert_eq!(json["reglas"][0]["ambito"]["carpeta"], folder.to_string());
        assert_eq!(json["reglas"][1]["ambito"], "todos");
        assert_eq!(json["reglas"][1]["enviar"], "y\n");
        // Una regla que no envía no lleva el campo.
        assert!(json["reglas"][0].get("enviar").is_none());
        assert_eq!(json["orden_larga"]["segundos"], 120);
        let back: TriggerSettings = serde_json::from_value(json).unwrap();
        assert_eq!(back, settings);

        // Lo que falte toma su valor de fábrica.
        let partial: TriggerSettings =
            serde_json::from_str(r#"{"reglas":[{"texto":"error"}]}"#).unwrap();
        assert_eq!(partial.long_command, LongCommand::default());
        let loaded = &partial.rules[0];
        assert!(loaded.enabled && loaded.notify && !loaded.sends());
        assert_eq!(loaded.scope, Scope::All);

        // Un fichero retocado a mano con valores desmedidos.
        let mut wild = TriggerSettings {
            rules: vec![
                Trigger {
                    name: "n".repeat(500),
                    cooldown_secs: u32::MAX,
                    send: Some("s".repeat(5000)),
                    ..rule(&"p".repeat(5000))
                };
                MAX_RULES + 10
            ],
            long_command: LongCommand {
                enabled: true,
                seconds: 0,
                bell: false,
            },
        };
        wild.sanitize();
        assert_eq!(wild.rules.len(), MAX_RULES);
        assert_eq!(wild.rules[0].name.chars().count(), MAX_NAME_CHARS);
        assert_eq!(wild.rules[0].pattern.chars().count(), MAX_PATTERN_CHARS);
        assert_eq!(
            wild.rules[0].send.as_ref().unwrap().chars().count(),
            MAX_SEND_CHARS
        );
        assert_eq!(wild.rules[0].cooldown_secs, MAX_COOLDOWN);
        assert_eq!(wild.long_command.seconds, LongCommand::MIN_SECONDS);
        assert_eq!(
            LongCommand {
                enabled: true,
                seconds: 1,
                bell: false
            }
            .threshold(),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            LongCommand {
                enabled: false,
                ..Default::default()
            }
            .threshold(),
            None
        );
    }
}
