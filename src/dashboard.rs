//! Panel de estado de una carpeta: vigila a la vez todos sus equipos SSH.
//!
//! Cada equipo tiene su propia conexión, que se mantiene abierta entre
//! refrescos. Por ella corre la consulta ligera de [`crate::monitor`] (un
//! bucle de shell que imprime los contadores cada pocos segundos) y se mide
//! la latencia. Todo el trabajo —conectar, interpretar las muestras, decidir
//! las alertas, guardar la tendencia— se hace en segundo plano: la interfaz
//! solo lee el estado ya calculado, y solo se la despierta cuando cambia.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use chrono::{DateTime, Local};
use russh::Disconnect;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, SemaphorePermit, watch};
use uuid::Uuid;

use crate::i18n::tr;
use crate::monitor::{Disk, Probe, Sampler, Stats, format_kb};
use crate::ssh::{Connected, Endpoint, PromptRequest, Prompter, SshHandle, Waker};
use crate::trf;

// ----------------------------------------------------------------------
// Preferencias
// ----------------------------------------------------------------------

/// Qué dibuja la minigráfica de cada equipo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrendMetric {
    #[default]
    Cpu,
    Load,
}

impl TrendMetric {
    pub const ALL: [TrendMetric; 2] = [TrendMetric::Cpu, TrendMetric::Load];

    /// Nombre con el que se guarda en las preferencias.
    fn key(self) -> &'static str {
        match self {
            TrendMetric::Cpu => "cpu",
            TrendMetric::Load => "carga",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TrendMetric::Cpu => tr("Uso de CPU"),
            TrendMetric::Load => tr("Carga media"),
        }
    }
}

impl Serialize for TrendMetric {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.key())
    }
}

impl<'de> Deserialize<'de> for TrendMetric {
    /// Un nombre desconocido (de otra versión) no descarta el resto de las
    /// preferencias: se queda con el de fábrica.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let key = String::deserialize(d)?;
        Ok(Self::ALL
            .into_iter()
            .find(|m| m.key() == key)
            .unwrap_or_default())
    }
}

/// Umbrales de las alertas. Un valor a cero deja de vigilar ese dato.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Thresholds {
    /// Uso de CPU (%) que, sostenido, dispara la alerta.
    #[serde(rename = "cpu")]
    pub cpu: f32,
    /// Carga media del último minuto, por núcleo, que dispara la alerta si
    /// se sostiene.
    #[serde(rename = "carga")]
    pub load: f32,
    /// Muestras seguidas por encima del umbral para dar por alta la CPU o
    /// la carga (y por debajo para darla por resuelta).
    #[serde(rename = "muestras")]
    pub sustained: u32,
    /// Memoria en uso (%).
    #[serde(rename = "memoria")]
    pub memory: f32,
    /// Ocupación (%) del disco más lleno.
    #[serde(rename = "disco")]
    pub disk: f32,
    /// Avisar de los equipos a los que no se consigue conectar.
    #[serde(rename = "inalcanzable")]
    pub unreachable: bool,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            cpu: 90.0,
            load: 2.0,
            sustained: 3,
            memory: 90.0,
            disk: 90.0,
            unreachable: true,
        }
    }
}

impl Thresholds {
    fn sanitize(&mut self) {
        let defaults = Self::default();
        let fix = |value: f32, default: f32, max: f32| {
            if value.is_finite() {
                value.clamp(0.0, max)
            } else {
                default
            }
        };
        self.cpu = fix(self.cpu, defaults.cpu, 100.0);
        self.load = fix(self.load, defaults.load, 100.0);
        self.memory = fix(self.memory, defaults.memory, 100.0);
        self.disk = fix(self.disk, defaults.disk, 100.0);
        self.sustained = self.sustained.clamp(1, 20);
    }
}

/// Preferencias del panel de estado.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DashboardSettings {
    /// Segundos entre muestras.
    #[serde(rename = "intervalo")]
    pub interval: u32,
    /// Conexiones abiertas a la vez, como mucho. Con más equipos que esto,
    /// se van turnando.
    #[serde(rename = "conexiones")]
    pub max_connections: u32,
    /// Dato de la minigráfica.
    #[serde(rename = "tendencia")]
    pub trend: TrendMetric,
    /// Minutos que abarca la minigráfica.
    #[serde(rename = "minutos_tendencia")]
    pub trend_minutes: u32,
    /// Mostrar un aviso en la aplicación cuando salta o se resuelve una
    /// alerta.
    #[serde(rename = "avisos")]
    pub notify: bool,
    #[serde(rename = "umbrales")]
    pub thresholds: Thresholds,
}

impl Default for DashboardSettings {
    fn default() -> Self {
        Self {
            interval: 10,
            max_connections: 32,
            trend: TrendMetric::Cpu,
            trend_minutes: 15,
            notify: true,
            thresholds: Thresholds::default(),
        }
    }
}

impl DashboardSettings {
    pub const MIN_INTERVAL: u32 = 2;
    pub const MAX_INTERVAL: u32 = 3600;
    pub const MAX_CONNECTIONS: u32 = 256;
    pub const MAX_TREND_MINUTES: u32 = 240;

    pub fn sanitize(&mut self) {
        self.interval = self.interval.clamp(Self::MIN_INTERVAL, Self::MAX_INTERVAL);
        self.max_connections = self.max_connections.clamp(1, Self::MAX_CONNECTIONS);
        self.trend_minutes = self.trend_minutes.clamp(1, Self::MAX_TREND_MINUTES);
        self.thresholds.sanitize();
    }

    /// Tiempo que abarca la minigráfica.
    pub fn trend_window(&self) -> Duration {
        Duration::from_secs(u64::from(self.trend_minutes) * 60)
    }
}

// ----------------------------------------------------------------------
// Alertas
// ----------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlertKind {
    /// No se consigue conectar con el equipo.
    Unreachable,
    /// Se llega al equipo, pero no se puede entrar: falta una credencial o
    /// el servidor rechaza el acceso. Mientras tanto no se vigila.
    Access,
    Cpu,
    Load,
    Memory,
    Disk,
}

impl AlertKind {
    pub const ALL: [AlertKind; 6] = [
        AlertKind::Unreachable,
        AlertKind::Access,
        AlertKind::Cpu,
        AlertKind::Load,
        AlertKind::Memory,
        AlertKind::Disk,
    ];

    fn index(self) -> usize {
        self as usize
    }

    /// Nombre corto, para las listas.
    pub fn label(self) -> &'static str {
        match self {
            AlertKind::Unreachable => tr("inalcanzable"),
            AlertKind::Access => tr("sin acceso"),
            AlertKind::Cpu => "CPU",
            AlertKind::Load => tr("carga"),
            AlertKind::Memory => tr("memoria"),
            AlertKind::Disk => tr("disco"),
        }
    }
}

/// Lo que se sabe de la conexión con un equipo, a efectos de las alertas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reach<'a> {
    Online,
    /// No se consigue conectar, con el motivo.
    Unreachable(&'a str),
    /// Se llega, pero no se puede entrar, con el motivo.
    NoAccess(&'a str),
}

/// Una alerta que salta (`raised`) o se resuelve.
#[derive(Debug, Clone, PartialEq)]
pub struct AlertChange {
    pub kind: AlertKind,
    pub raised: bool,
    pub detail: String,
}

/// Margen (en puntos de porcentaje) que la memoria o el disco tienen que
/// bajar del umbral para dar la alerta por resuelta: así un valor que baila
/// justo en el umbral no avisa una y otra vez.
pub const HYSTERESIS: f32 = 2.0;

/// Muestras seguidas que un valor lleva por encima y por debajo del umbral.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Run {
    over: u32,
    under: u32,
}

/// Alertas activas de un equipo y lo necesario para decidir las siguientes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Alerts {
    active: [bool; AlertKind::ALL.len()],
    cpu: Run,
    load: Run,
}

fn decimal(value: f32) -> String {
    crate::i18n::decimal(f64::from(value), 2)
}

impl Alerts {
    pub fn is_active(&self, kind: AlertKind) -> bool {
        self.active[kind.index()]
    }

    pub fn any(&self) -> bool {
        self.active.iter().any(|a| *a)
    }

    /// Las que están activas ahora.
    pub fn active(&self) -> impl Iterator<Item = AlertKind> + '_ {
        AlertKind::ALL.into_iter().filter(|k| self.is_active(*k))
    }

    /// Retira una alerta sin avisar (se ha dejado de vigilar ese dato).
    fn withdraw(&mut self, kind: AlertKind) {
        self.active[kind.index()] = false;
        match kind {
            AlertKind::Cpu => self.cpu = Run::default(),
            AlertKind::Load => self.load = Run::default(),
            _ => {}
        }
    }

    /// Retira las alertas de lo que los umbrales ya no vigilan.
    fn retune(&mut self, limits: &Thresholds) {
        for (kind, watched) in [
            (AlertKind::Unreachable, limits.unreachable),
            (AlertKind::Access, limits.unreachable),
            (AlertKind::Cpu, limits.cpu > 0.0),
            (AlertKind::Load, limits.load > 0.0),
            (AlertKind::Memory, limits.memory > 0.0),
            (AlertKind::Disk, limits.disk > 0.0),
        ] {
            if !watched {
                self.withdraw(kind);
            }
        }
    }

    /// Valor que tiene que sostenerse `needed` muestras seguidas por encima
    /// del umbral para saltar, y otras tantas por debajo para resolverse.
    /// Devuelve el cambio, si lo hay.
    fn sustained(&mut self, kind: AlertKind, over: bool, needed: u32) -> Option<bool> {
        let run = match kind {
            AlertKind::Cpu => &mut self.cpu,
            _ => &mut self.load,
        };
        if over {
            run.over = run.over.saturating_add(1);
            run.under = 0;
        } else {
            run.under = run.under.saturating_add(1);
            run.over = 0;
        }
        let run = *run;
        let active = &mut self.active[kind.index()];
        if !*active && run.over >= needed {
            *active = true;
            Some(true)
        } else if *active && run.under >= needed {
            *active = false;
            Some(false)
        } else {
            None
        }
    }

    /// Valor que salta en cuanto llega al umbral y se resuelve al bajar de
    /// él con cierto margen.
    fn level(&mut self, kind: AlertKind, value: f32, limit: f32) -> Option<bool> {
        let active = &mut self.active[kind.index()];
        if !*active && value >= limit {
            *active = true;
            Some(true)
        } else if *active && value < limit - HYSTERESIS {
            *active = false;
            Some(false)
        } else {
            None
        }
    }

    /// Tiene en cuenta una muestra nueva. Devuelve lo que cambia: nada
    /// mientras cada dato siga como estaba, bien o mal.
    pub fn observe(&mut self, limits: &Thresholds, stats: &Stats) -> Vec<AlertChange> {
        self.retune(limits);
        let mut changes = vec![];
        let mut change = |kind, raised, detail: String| {
            changes.push(AlertChange {
                kind,
                raised,
                detail,
            })
        };
        let needed = limits.sustained.max(1);
        let streak = |raised: bool| match (raised, needed) {
            (false, _) => String::new(),
            (true, 1) => tr(" (umbral superado)").to_string(),
            (true, n) => trf!(" ({n} muestras seguidas por encima del umbral)", n),
        };
        if limits.cpu > 0.0
            && let Some(cpu) = stats.cpu
            && let Some(raised) = self.sustained(AlertKind::Cpu, cpu >= limits.cpu, needed)
        {
            change(
                AlertKind::Cpu,
                raised,
                trf!(
                    "CPU al {percent} %{streak}",
                    percent = format!("{cpu:.0}"),
                    streak = streak(raised)
                ),
            );
        }
        if limits.load > 0.0
            && let Some(per_core) = load_per_core(stats)
            && let Some(raised) = self.sustained(AlertKind::Load, per_core >= limits.load, needed)
        {
            change(
                AlertKind::Load,
                raised,
                trf!(
                    "carga {load} con {cores} núcleo(s){streak}",
                    load = decimal(stats.load.unwrap_or_default()[0]),
                    cores = stats.cores.unwrap_or_default(),
                    streak = streak(raised)
                ),
            );
        }
        if limits.memory > 0.0
            && let Some(percent) = memory_percent(stats)
            && let Some(raised) = self.level(AlertKind::Memory, percent, limits.memory)
        {
            let (used, total) = stats.memory.unwrap_or_default();
            change(
                AlertKind::Memory,
                raised,
                trf!(
                    "memoria al {percent} % ({used} de {total})",
                    percent = format!("{percent:.0}"),
                    used = format_kb(used),
                    total = format_kb(total)
                ),
            );
        }
        if limits.disk > 0.0
            && let Some(disk) = fullest_disk(stats)
            && let Some(raised) = self.level(AlertKind::Disk, disk.used_percent(), limits.disk)
        {
            change(
                AlertKind::Disk,
                raised,
                trf!(
                    "disco {mount} al {percent} % (quedan {free})",
                    mount = brief(&disk.mount, 60),
                    percent = format!("{:.0}", disk.used_percent()),
                    free = format_kb(disk.available)
                ),
            );
        }
        changes
    }

    /// Tiene en cuenta si se consigue conectar con el equipo y entrar en
    /// él. Devuelve lo que cambia.
    pub fn observe_link(&mut self, limits: &Thresholds, reach: Reach) -> Vec<AlertChange> {
        self.retune(limits);
        if !limits.unreachable {
            return vec![];
        }
        let mut changes = vec![];
        let mut set = |kind: AlertKind, raised: bool, detail: String| {
            let active = &mut self.active[kind.index()];
            if *active != raised {
                *active = raised;
                changes.push(AlertChange {
                    kind,
                    raised,
                    detail,
                });
            }
        };
        match reach {
            Reach::Online => {
                set(
                    AlertKind::Unreachable,
                    false,
                    tr("vuelve a responder").into(),
                );
                set(AlertKind::Access, false, tr("se ha podido entrar").into());
            }
            Reach::Unreachable(why) => {
                set(
                    AlertKind::Unreachable,
                    true,
                    trf!("no responde: {why}", why),
                );
                // De lo que pasaba antes ya no se sabe nada.
                self.active[AlertKind::Access.index()] = false;
            }
            Reach::NoAccess(why) => {
                set(AlertKind::Access, true, trf!("no se vigila: {why}", why));
                self.active[AlertKind::Unreachable.index()] = false;
            }
        }
        changes
    }
}

/// Recorta un texto que viene de fuera (un error, una ruta del servidor)
/// para que no ocupe más de la cuenta ni en pantalla ni en memoria.
pub fn brief(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

/// Memoria en uso, en porcentaje.
pub fn memory_percent(stats: &Stats) -> Option<f32> {
    stats
        .memory
        .filter(|(_, total)| *total > 0)
        .map(|(used, total)| used as f32 * 100.0 / total as f32)
}

/// Carga media del último minuto repartida entre los núcleos.
pub fn load_per_core(stats: &Stats) -> Option<f32> {
    match (stats.load, stats.cores) {
        (Some(load), Some(cores)) if cores > 0 => Some(load[0] / cores as f32),
        _ => None,
    }
}

// ----------------------------------------------------------------------
// Discos
// ----------------------------------------------------------------------

/// Sistemas de ficheros que no son discos que se llenen con el uso: los de
/// memoria, los de solo lectura por naturaleza (las imágenes de snap están
/// siempre al 100 %) y las capas de los contenedores.
const NOT_DISKS: &[&str] = &[
    "tmpfs", "devtmpfs", "ramfs", "squashfs", "erofs", "cramfs", "romfs", "iso9660", "udf",
    "overlay", "aufs", "efivarfs", "autofs", "proc", "sysfs", "devpts", "cgroup", "cgroup2",
    "nsfs", "fuse", "fusectl",
];

/// `true` si merece vigilar cuánto se llena este sistema de ficheros.
pub fn is_real_disk(disk: &Disk) -> bool {
    if disk.total == 0 {
        return false;
    }
    // La raíz importa siempre, sea lo que sea (en un contenedor es una capa).
    if disk.mount == "/" {
        return true;
    }
    if disk.read_only {
        return false;
    }
    if disk.kind.is_empty() {
        // El servidor no dijo el tipo (no es Linux): por el nombre del
        // dispositivo, que en los de memoria no es una ruta.
        return disk.filesystem.contains('/') && !disk.filesystem.starts_with("/dev/loop");
    }
    // `fuse.algo` son montajes de usuario (sshfs, lxcfs…); `fuseblk`, discos.
    !NOT_DISKS.contains(&disk.kind.as_str()) && !disk.kind.starts_with("fuse.")
}

/// El disco más lleno del equipo, sin contar lo que no son discos.
pub fn fullest_disk(stats: &Stats) -> Option<&Disk> {
    stats
        .disks
        .iter()
        .filter(|d| is_real_disk(d))
        .max_by(|a, b| a.used_percent().total_cmp(&b.used_percent()))
}

// ----------------------------------------------------------------------
// Tendencia
// ----------------------------------------------------------------------

/// Puntos que guarda la minigráfica, como mucho: si las muestras llegan más
/// seguidas, se funden (se promedian) las que caen en el mismo tramo.
pub const TREND_POINTS: u32 = 240;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrendPoint {
    pub at: Instant,
    /// Uso de CPU, en porcentaje.
    pub cpu: Option<f32>,
    /// Carga media del último minuto.
    pub load: Option<f32>,
}

/// Últimos minutos de CPU y carga de un equipo.
#[derive(Debug, Clone, Default)]
pub struct Trend {
    points: VecDeque<TrendPoint>,
    /// Muestras fundidas en el último punto.
    merged: u32,
}

impl Trend {
    pub fn push(&mut self, at: Instant, cpu: Option<f32>, load: Option<f32>, window: Duration) {
        let step = window / TREND_POINTS;
        let mean = |old: Option<f32>, new: Option<f32>, count: u32| match (old, new) {
            (Some(old), Some(new)) => Some((old * count as f32 + new) / (count + 1) as f32),
            (old, new) => new.or(old),
        };
        match self.points.back_mut() {
            Some(last) if at.saturating_duration_since(last.at) < step => {
                last.cpu = mean(last.cpu, cpu, self.merged);
                last.load = mean(last.load, load, self.merged);
                self.merged += 1;
            }
            _ => {
                self.points.push_back(TrendPoint { at, cpu, load });
                self.merged = 1;
            }
        }
        self.prune(at, window);
    }

    /// Olvida lo que ya queda fuera de la ventana.
    pub fn prune(&mut self, now: Instant, window: Duration) {
        while self
            .points
            .front()
            .is_some_and(|p| now.saturating_duration_since(p.at) > window)
        {
            self.points.pop_front();
        }
    }

    pub fn points(&self) -> impl ExactSizeIterator<Item = &TrendPoint> + '_ {
        self.points.iter()
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
}

// ----------------------------------------------------------------------
// Estado de cada equipo
// ----------------------------------------------------------------------

/// Estado de la conexión con un equipo.
#[derive(Debug, Clone, PartialEq)]
pub enum Link {
    /// A la espera de turno para conectar.
    Waiting,
    /// Conectando; con el último mensaje de progreso.
    Connecting(String),
    Online,
    /// Sin conexión, con el motivo: se vuelve a intentar sola.
    Down(String),
    /// Hace falta una contraseña (o passphrase) que no está en el baúl. No
    /// se reintenta hasta que el usuario lo pida.
    NeedsCredential,
    /// El servidor rechaza el acceso, o hay que confirmar su clave: tampoco
    /// se reintenta sola (insistir puede bloquear la cuenta).
    Rejected(String),
}

impl Link {
    /// `true` si está esperando a que el usuario pida reintentar.
    pub fn needs_user(&self) -> bool {
        matches!(self, Link::NeedsCredential | Link::Rejected(_))
    }
}

/// Un equipo del panel, con lo último que se sabe de él.
#[derive(Debug, Clone)]
pub struct HostRow {
    pub id: Uuid,
    pub name: String,
    /// `usuario@host:puerto`.
    pub target: String,
    pub link: Link,
    /// Intentos de conexión fallidos seguidos.
    pub failures: u32,
    /// Cuándo toca el siguiente intento, si está sin conexión.
    pub retry_at: Option<DateTime<Local>>,
    /// Paso del intento de reconexión en curso, si lo hay.
    pub step: Option<String>,
    /// Conectado, pero la consulta no da datos: por qué.
    pub probe_error: Option<String>,
    pub latency: Option<Duration>,
    /// Última muestra (se conserva aunque se pierda la conexión).
    pub stats: Option<Stats>,
    /// Cuándo llegó la última muestra.
    pub updated: Option<DateTime<Local>>,
    updated_at: Option<Instant>,
    pub trend: Trend,
    pub alerts: Alerts,
}

impl HostRow {
    pub(crate) fn new(id: Uuid, name: String, target: String) -> Self {
        Self {
            id,
            name,
            target,
            link: Link::Waiting,
            failures: 0,
            retry_at: None,
            step: None,
            probe_error: None,
            latency: None,
            stats: None,
            updated: None,
            updated_at: None,
            trend: Trend::default(),
            alerts: Alerts::default(),
        }
    }

    /// Estado en pocas palabras.
    pub fn state_label(&self) -> &'static str {
        match &self.link {
            Link::Waiting => tr("en cola"),
            Link::Connecting(_) => tr("conectando"),
            Link::Online if self.probe_error.is_some() => tr("sin datos"),
            Link::Online => tr("en línea"),
            Link::Down(_) => tr("sin conexión"),
            Link::NeedsCredential => tr("falta credencial"),
            Link::Rejected(_) => tr("acceso rechazado"),
        }
    }

    /// Para ordenar por estado: primero lo que va peor.
    fn state_rank(&self) -> u8 {
        match &self.link {
            Link::Down(_) => 0,
            Link::Rejected(_) => 1,
            Link::NeedsCredential => 2,
            Link::Online if self.probe_error.is_some() => 3,
            Link::Online if self.alerts.any() => 4,
            Link::Connecting(_) => 5,
            Link::Waiting => 6,
            Link::Online => 7,
        }
    }

    /// Algo no va bien: sin conexión, sin datos o con alguna alerta.
    pub fn has_problem(&self) -> bool {
        self.alerts.any()
            || self.probe_error.is_some()
            || matches!(
                self.link,
                Link::Down(_) | Link::NeedsCredential | Link::Rejected(_)
            )
    }

    /// Los datos son viejos: hace más de tres intervalos que no llega una
    /// muestra (o no hay conexión).
    pub fn is_stale(&self, now: Instant, interval: u32) -> bool {
        let limit = Duration::from_secs(u64::from(interval) * 3 + 5);
        self.link != Link::Online
            || self
                .updated_at
                .is_none_or(|at| now.saturating_duration_since(at) > limit)
    }

    fn matches(&self, needle: &str) -> bool {
        needle.is_empty()
            || self.name.to_lowercase().contains(needle)
            || self.target.to_lowercase().contains(needle)
            || self.state_label().to_lowercase().contains(needle)
            || self
                .alerts
                .active()
                .any(|k| k.label().to_lowercase() == needle)
    }
}

/// Una alerta que saltó o se resolvió, para el historial y los avisos.
#[derive(Debug, Clone, PartialEq)]
pub struct AlertEvent {
    pub at: DateTime<Local>,
    pub host: Uuid,
    pub name: String,
    pub kind: AlertKind,
    pub raised: bool,
    pub detail: String,
}

/// Entradas que guarda el historial de alertas.
pub const HISTORY_MAX: usize = 200;
/// Avisos sin recoger que se guardan, como mucho.
const NEWS_MAX: usize = 64;

/// Columnas por las que se puede ordenar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Name,
    State,
    Latency,
    Cpu,
    Load,
    Memory,
    Disk,
    Uptime,
    Updated,
}

/// Lo que la interfaz pinta: se calcula en segundo plano.
pub struct State {
    pub hosts: Vec<HostRow>,
    /// Alertas recientes, de la más antigua a la más nueva.
    pub history: VecDeque<AlertEvent>,
    /// Sube cada vez que cambia algo de lo que se ve.
    pub revision: u64,
    settings: DashboardSettings,
    /// Alertas de las que aún no se ha avisado en la aplicación.
    news: Vec<AlertEvent>,
    /// Secretos pendientes de guardar en el baúl.
    secrets: Vec<(String, String)>,
}

impl State {
    fn new(hosts: Vec<HostRow>, settings: DashboardSettings) -> Self {
        Self {
            hosts,
            history: VecDeque::new(),
            revision: 0,
            settings,
            news: vec![],
            secrets: vec![],
        }
    }

    pub fn settings(&self) -> &DashboardSettings {
        &self.settings
    }

    fn record(&mut self, index: usize, change: AlertChange) {
        let host = &self.hosts[index];
        let event = AlertEvent {
            at: Local::now(),
            host: host.id,
            name: host.name.clone(),
            kind: change.kind,
            raised: change.raised,
            detail: change.detail,
        };
        if self.settings.notify {
            if self.news.len() >= NEWS_MAX {
                self.news.remove(0);
            }
            self.news.push(event.clone());
        }
        if self.history.len() >= HISTORY_MAX {
            self.history.pop_front();
        }
        self.history.push_back(event);
    }

    /// Cambia el estado de la conexión de un equipo.
    fn set_link(
        &mut self,
        index: usize,
        link: Link,
        failures: u32,
        retry_at: Option<DateTime<Local>>,
    ) {
        let limits = self.settings.thresholds.clone();
        let host = &mut self.hosts[index];
        let reach = match &link {
            Link::Online => Some(Reach::Online),
            // Una conexión que se cae no es aún un equipo inalcanzable: lo
            // es cuando falla el intento de volver a conectar.
            Link::Down(error) if failures > 0 => Some(Reach::Unreachable(error)),
            Link::NeedsCredential => Some(Reach::NoAccess(tr(
                "falta una contraseña o passphrase que no está en el baúl",
            ))),
            Link::Rejected(error) => Some(Reach::NoAccess(error)),
            _ => None,
        };
        let changes = reach
            .map(|reach| host.alerts.observe_link(&limits, reach))
            .unwrap_or_default();
        if link != Link::Online {
            host.latency = None;
            host.probe_error = None;
        }
        host.link = link;
        host.failures = failures;
        host.retry_at = retry_at;
        host.step = None;
        for change in changes {
            self.record(index, change);
        }
    }

    /// Anota una muestra recién llegada. `first` es la primera de una
    /// consulta recién lanzada, que aún no puede traer el uso de CPU (sale
    /// de comparar dos muestras).
    fn sample(&mut self, index: usize, mut stats: Stats, now: Instant, first: bool) {
        let window = self.settings.trend_window();
        let limits = self.settings.thresholds.clone();
        let host = &mut self.hosts[index];
        host.trend
            .push(now, stats.cpu, stats.load.map(|l| l[0]), window);
        let changes = host.alerts.observe(&limits, &stats);
        if first && stats.cpu.is_none() {
            // Mientras llega la siguiente se sigue enseñando el último
            // conocido, en vez de dejar la celda en blanco un momento cada
            // vez que se reconecta o le toca el turno.
            stats.cpu = host.stats.as_ref().and_then(|s| s.cpu);
        }
        host.stats = Some(stats);
        host.updated = Some(Local::now());
        host.updated_at = Some(now);
        host.probe_error = None;
        for change in changes {
            self.record(index, change);
        }
    }

    fn configure(&mut self, settings: DashboardSettings) {
        for host in &mut self.hosts {
            host.alerts.retune(&settings.thresholds);
        }
        if !settings.notify {
            self.news.clear();
        }
        self.settings = settings;
    }

    /// Equipos con alguna alerta activa.
    pub fn alerting(&self) -> usize {
        self.hosts.iter().filter(|h| h.alerts.any()).count()
    }

    /// Equipos conectados.
    pub fn online(&self) -> usize {
        self.hosts.iter().filter(|h| h.link == Link::Online).count()
    }

    /// Filas que se muestran, por orden: las que casan con el filtro,
    /// ordenadas por la columna elegida. Lo que no tiene dato va al final,
    /// se ordene hacia donde se ordene.
    pub fn order(
        &self,
        filter: &str,
        only_problems: bool,
        column: Column,
        descending: bool,
    ) -> Vec<usize> {
        let needle = filter.trim().to_lowercase();
        let mut rows: Vec<usize> = (0..self.hosts.len())
            .filter(|i| {
                let host = &self.hosts[*i];
                host.matches(&needle) && (!only_problems || host.has_problem())
            })
            .collect();
        let number = |host: &HostRow| -> Option<f64> {
            let stats = host.stats.as_ref();
            match column {
                Column::Latency => host.latency.map(|l| l.as_secs_f64()),
                Column::Cpu => stats.and_then(|s| s.cpu).map(f64::from),
                Column::Load => stats.and_then(|s| s.load).map(|l| f64::from(l[0])),
                Column::Memory => stats.and_then(memory_percent).map(f64::from),
                Column::Disk => stats
                    .and_then(fullest_disk)
                    .map(|d| f64::from(d.used_percent())),
                Column::Uptime => stats.and_then(|s| s.uptime_secs).map(|u| u as f64),
                Column::Updated => host.updated.map(|u| u.timestamp_millis() as f64),
                Column::State => Some(f64::from(host.state_rank())),
                Column::Name => Some(0.0),
            }
        };
        let name = |host: &HostRow| host.name.to_lowercase();
        rows.sort_by(|a, b| {
            let (a, b) = (&self.hosts[*a], &self.hosts[*b]);
            let by_value = match (number(a), number(b)) {
                (Some(x), Some(y)) if descending => y.total_cmp(&x),
                (Some(x), Some(y)) => x.total_cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            };
            by_value.then_with(|| {
                if column == Column::Name && descending {
                    name(b).cmp(&name(a))
                } else {
                    name(a).cmp(&name(b))
                }
            })
        });
        rows
    }
}

/// Resume en una línea las alertas recién ocurridas, para el aviso de la
/// aplicación. Devuelve el texto y si es una mala noticia.
pub fn summarize(news: &[AlertEvent]) -> Option<(String, bool)> {
    let raised: Vec<&AlertEvent> = news.iter().filter(|e| e.raised).collect();
    let bad = !raised.is_empty();
    let events: Vec<&AlertEvent> = if bad { raised } else { news.iter().collect() };
    match events[..] {
        [] => None,
        [one] => {
            let (name, detail) = (&one.name, &one.detail);
            let text = if bad {
                trf!("Alerta: {name} — {detail}", name, detail)
            } else {
                trf!("Resuelto: {name} — {detail}", name, detail)
            };
            Some((text, bad))
        }
        _ => {
            let mut list: Vec<String> = events
                .iter()
                .take(4)
                .map(|e| format!("{} ({})", e.name, e.kind.label()))
                .collect();
            if events.len() > list.len() {
                list.push(trf!("y {n} más", n = events.len() - list.len()));
            }
            let (n, list) = (events.len(), list.join(", "));
            let text = if bad {
                trf!("{n} alertas nuevas: {list}", n, list)
            } else {
                trf!("{n} alertas resueltas: {list}", n, list)
            };
            Some((text, bad))
        }
    }
}

// ----------------------------------------------------------------------
// Conexiones
// ----------------------------------------------------------------------

/// Un equipo que vigilar: cómo se llama y cómo llegar a él.
pub struct HostJob {
    pub id: Uuid,
    pub name: String,
    /// `usuario@host:puerto`, para mostrarlo.
    pub target: String,
    pub endpoint: Endpoint,
}

/// Ajustes internos; los de fábrica son los de la aplicación y las pruebas
/// los acortan.
#[derive(Clone)]
pub struct Options {
    /// Espera antes de cada reintento de conexión; el último vale para
    /// todos los siguientes.
    pub retry_delays: Vec<Duration>,
    /// Lo que puede tardar un equipo en conectar (sin contar lo que tarde
    /// el usuario en contestar a una pregunta).
    pub connect_timeout: Duration,
    /// Lo que puede tardar en contestar al sondeo de latencia antes de dar
    /// la conexión por perdida.
    pub ping_timeout: Duration,
    /// Equipos conectando a la vez, como mucho (aunque quepan más
    /// conexiones abiertas): un bastión compartido rechaza las ráfagas.
    pub connecting_at_once: usize,
    /// ¿Merece reintentar tras este error? Los de autenticación, no:
    /// insistir no lo arregla y puede bloquear la cuenta.
    pub retryable: fn(&str) -> bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            retry_delays: [2, 5, 10, 20, 30, 60, 120]
                .into_iter()
                .map(Duration::from_secs)
                .collect(),
            connect_timeout: Duration::from_secs(30),
            ping_timeout: Duration::from_secs(20),
            connecting_at_once: 4,
            retryable: |_| true,
        }
    }
}

impl Options {
    /// Espera tras `failures` intentos fallidos seguidos.
    pub fn retry_delay(&self, failures: u32) -> Duration {
        let last = self.retry_delays.len().saturating_sub(1);
        self.retry_delays
            .get((failures as usize).min(last))
            .copied()
            .unwrap_or(Duration::from_secs(60))
    }
}

/// Lo que comparten las tareas de los equipos y el panel.
struct Shared {
    state: Mutex<State>,
    /// Turnos para tener una conexión abierta.
    open: Arc<Semaphore>,
    /// Turnos para estar conectando.
    connecting: Semaphore,
    /// Equipos a la espera de turno: si los hay, los conectados ceden el
    /// suyo en cuanto tienen datos.
    waiting: AtomicUsize,
    /// Segundos entre muestras; al cambiar, se relanza la consulta.
    interval: watch::Sender<u32>,
    stop: watch::Sender<bool>,
    /// Hay algo nuevo que pintar.
    changed: Notify,
    /// La ventana está a la vista: si no, solo se despierta a la interfaz
    /// por lo que tiene que atender igualmente (avisos, secretos).
    watched: AtomicBool,
    options: Options,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Cambia el estado y avisa a la interfaz.
    fn update<R>(&self, apply: impl FnOnce(&mut State) -> R) -> R {
        let result = {
            let mut state = self.lock();
            state.revision += 1;
            apply(&mut state)
        };
        self.changed.notify_one();
        result
    }

    fn set_link(&self, index: usize, link: Link) {
        self.update(|state| {
            let failures = match link {
                Link::Online => 0,
                _ => state.hosts[index].failures,
            };
            state.set_link(index, link, failures, None)
        });
    }

    /// Mensaje de progreso de un intento de conexión. En los reintentos
    /// automáticos (`quiet`) el equipo sigue figurando como sin conexión.
    fn progress(&self, index: usize, text: String, quiet: bool) {
        self.update(|state| {
            let host = &mut state.hosts[index];
            if quiet {
                host.step = Some(text);
            } else {
                host.link = Link::Connecting(text);
            }
        });
    }

    /// Espera el turno para tener una conexión abierta.
    async fn turn(&self, index: usize) -> OwnedSemaphorePermit {
        if let Ok(permit) = self.open.clone().try_acquire_owned() {
            return permit;
        }
        self.set_link(index, Link::Waiting);
        // Aunque la tarea se abandone a media espera, la cuenta se deshace.
        struct Queued<'a>(&'a AtomicUsize);
        impl Drop for Queued<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let _queued = Queued(&self.waiting);
        self.open
            .clone()
            .acquire_owned()
            .await
            .expect("el semáforo no se cierra")
    }
}

/// Lo que cada tarea comparte con el panel sobre su equipo.
struct Slot {
    /// Cómo llegar al equipo, con los secretos que se conozcan.
    endpoint: Mutex<Endpoint>,
    /// Peticiones de reintentar ya: cuántas van, y si la última permite
    /// preguntar al usuario lo que haga falta.
    retry: watch::Sender<(u64, bool)>,
}

impl Slot {
    fn endpoint(&self) -> MutexGuard<'_, Endpoint> {
        self.endpoint.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Como mucho, cada cuánto se despierta a la interfaz por datos nuevos: la
/// primera novedad se pinta al momento y las que lleguen justo después
/// esperan a ir juntas (con muchos equipos, las muestras llegan a goteo).
const REPAINT_EVERY: Duration = Duration::from_millis(500);

/// El panel en marcha: las conexiones con los equipos de una carpeta y lo
/// que se sabe de cada uno. Al soltarlo se cierran todas.
pub struct Dashboard {
    shared: Arc<Shared>,
    slots: Vec<Arc<Slot>>,
}

impl Dashboard {
    /// Empieza a vigilar los equipos. `prompter` se usa para lo que haya
    /// que preguntar al usuario (contraseñas que falten, claves de
    /// servidores desconocidos) en el primer intento de cada equipo y
    /// cuando pida reintentar; las reconexiones automáticas no preguntan.
    pub fn start(
        rt: &tokio::runtime::Handle,
        jobs: Vec<HostJob>,
        prompter: Prompter,
        settings: &DashboardSettings,
        options: Options,
    ) -> Self {
        let mut settings = settings.clone();
        settings.sanitize();
        let hosts = jobs
            .iter()
            .map(|job| HostRow::new(job.id, job.name.clone(), job.target.clone()))
            .collect();
        let max_connections = settings.max_connections as usize;
        let shared = Arc::new(Shared {
            open: Arc::new(Semaphore::new(max_connections)),
            connecting: Semaphore::new(options.connecting_at_once.clamp(1, max_connections)),
            waiting: AtomicUsize::new(0),
            interval: watch::channel(settings.interval).0,
            stop: watch::channel(false).0,
            changed: Notify::new(),
            watched: AtomicBool::new(true),
            state: Mutex::new(State::new(hosts, settings)),
            options,
        });
        // Quien despierta a la interfaz: junta las novedades que lleguen
        // muy seguidas en un solo repintado.
        let waker: Waker = prompter.waker();
        rt.spawn({
            let shared = shared.clone();
            let mut stop = shared.stop.subscribe();
            async move {
                loop {
                    tokio::select! {
                        _ = stop.wait_for(|stop| *stop) => break,
                        _ = shared.changed.notified() => {}
                    }
                    let pending = {
                        let state = shared.lock();
                        !state.news.is_empty() || !state.secrets.is_empty()
                    };
                    if pending || shared.watched.load(Ordering::Relaxed) {
                        waker();
                    }
                    tokio::time::sleep(REPAINT_EVERY).await;
                }
            }
        });
        let slots: Vec<Arc<Slot>> = jobs
            .into_iter()
            .map(|job| {
                Arc::new(Slot {
                    endpoint: Mutex::new(job.endpoint),
                    retry: watch::channel((0, false)).0,
                })
            })
            .collect();
        for (index, slot) in slots.iter().enumerate() {
            rt.spawn(host_task(
                index,
                shared.clone(),
                slot.clone(),
                prompter.clone(),
            ));
        }
        Self { shared, slots }
    }

    /// El estado, para pintarlo. Hay que soltarlo pronto: mientras se tiene
    /// cogido, las muestras que lleguen esperan.
    pub fn state(&self) -> MutexGuard<'_, State> {
        self.shared.lock()
    }

    /// Número que cambia cuando hay algo nuevo que pintar.
    pub fn revision(&self) -> u64 {
        self.shared.lock().revision
    }

    /// Dice si la ventana está a la vista. Mientras no lo esté (plegada), la
    /// llegada de datos no hace repintar la aplicación; las alertas, sí.
    pub fn set_watched(&self, watched: bool) {
        self.shared.watched.store(watched, Ordering::Relaxed);
    }

    /// Aplica las preferencias. El intervalo y los umbrales tienen efecto
    /// al momento; el tope de conexiones solo al arrancar el panel.
    pub fn configure(&self, settings: &DashboardSettings) {
        let mut settings = settings.clone();
        settings.sanitize();
        self.shared.interval.send_if_modified(|current| {
            std::mem::replace(current, settings.interval) != settings.interval
        });
        self.shared.update(|state| state.configure(settings));
    }

    /// Alertas que han saltado o se han resuelto desde la última llamada.
    pub fn take_news(&self) -> Vec<AlertEvent> {
        std::mem::take(&mut self.shared.lock().news)
    }

    /// Secretos que el usuario pidió guardar en el baúl, ya validados.
    pub fn take_secrets(&self) -> Vec<(String, String)> {
        std::mem::take(&mut self.shared.lock().secrets)
    }

    /// Vuelve a intentar ya la conexión de un equipo que está sin ella.
    /// `endpoint` sustituye a lo que se sabía de él (por si el baúl tiene
    /// ahora la credencial que faltaba). Con `ask` se puede preguntar al
    /// usuario lo que haga falta; sin él, si falta algo, se queda como está.
    pub fn retry(&self, id: Uuid, endpoint: Option<Endpoint>, ask: bool) {
        let index = {
            let state = self.shared.lock();
            state.hosts.iter().position(|h| h.id == id).filter(|i| {
                matches!(state.hosts[*i].link, Link::Down(_)) || state.hosts[*i].link.needs_user()
            })
        };
        if let Some(slot) = index.and_then(|i| self.slots.get(i)) {
            if let Some(endpoint) = endpoint {
                *slot.endpoint() = endpoint;
            }
            slot.retry.send_modify(|(count, asking)| {
                *count += 1;
                *asking = ask;
            });
        }
    }

    /// Olvida los secretos que se tenían de los equipos (al bloquearse el
    /// baúl). Las conexiones abiertas siguen; una reconexión que necesite
    /// una contraseña quedará en «falta credencial».
    pub fn forget_credentials(&self) {
        for slot in &self.slots {
            let mut endpoint = slot.endpoint();
            let mut hop = Some(&mut *endpoint);
            while let Some(current) = hop {
                current.creds = Default::default();
                hop = current.via.as_deref_mut();
            }
        }
    }

    pub fn clear_history(&self) {
        self.shared.update(|state| state.history.clear());
    }
}

impl Drop for Dashboard {
    fn drop(&mut self) {
        self.shared.stop.send_replace(true);
    }
}

/// Cierra una conexión con el servidor de forma ordenada.
async fn close(handle: Option<SshHandle>) {
    if let Some(handle) = handle {
        let goodbye = handle.disconnect(Disconnect::ByApplication, "", "es");
        let _ = tokio::time::timeout(Duration::from_secs(3), goodbye).await;
    }
}

/// Todo lo que se hace con un equipo mientras el panel está abierto.
async fn host_task(index: usize, shared: Arc<Shared>, slot: Arc<Slot>, prompter: Prompter) {
    let mut stop = shared.stop.subscribe();
    let mut conn = None;
    tokio::select! {
        _ = stop.wait_for(|stop| *stop) => {}
        _ = host_loop(index, &shared, &slot, &prompter, &mut conn) => {}
    }
    // Se cierre como se cierre el panel, la conexión no se queda abierta.
    close(conn.take()).await;
}

/// Lo que se guarda, como mucho, del texto de un error.
const MAX_ERROR: usize = 300;

/// Preguntas a las que se contestó que no (o no se pudo preguntar).
#[derive(Debug, Clone, Copy, Default)]
struct Declined {
    secret: bool,
    host_key: bool,
}

/// Cómo acaba una conexión.
enum End {
    /// Cede su turno a otro equipo que espera.
    Rotate,
    Failed {
        error: String,
        declined: Declined,
    },
}

async fn host_loop(
    index: usize,
    shared: &Shared,
    slot: &Slot,
    prompter: &Prompter,
    conn: &mut Option<SshHandle>,
) {
    let mut retry = slot.retry.subscribe();
    // El primer intento puede preguntar al usuario; los automáticos, no.
    let mut interactive = true;
    // El equipo figura como sin conexión: los pasos del reintento no
    // cambian su estado hasta que conecte.
    let mut quiet = false;
    let mut failures = 0u32;
    loop {
        let turn = shared.turn(index).await;
        let mut healthy = false;
        let end = session(
            index,
            shared,
            slot,
            prompter,
            conn,
            interactive,
            quiet,
            &mut healthy,
        )
        .await;
        close(conn.take()).await;
        interactive = false;
        let (error, declined) = match end {
            End::Rotate => {
                // Antes de soltar el turno, para que nunca figuren más
                // conexiones que turnos.
                shared.set_link(index, Link::Waiting);
                drop(turn);
                quiet = false;
                failures = 0;
                continue;
            }
            End::Failed { error, declined } => (brief(&error, MAX_ERROR), declined),
        };
        // Una conexión que llegó a funcionar y se cae empieza de cero; la
        // que no llegó es un intento fallido más.
        failures = if healthy { 0 } else { failures + 1 };
        let link = if declined.secret {
            Link::NeedsCredential
        } else if declined.host_key {
            Link::Rejected(tr("hay que confirmar la clave del servidor").into())
        } else if !(shared.options.retryable)(&error) {
            Link::Rejected(error)
        } else {
            Link::Down(error)
        };
        // Las peticiones de reintento anteriores a este momento no cuentan.
        retry.mark_unchanged();
        if link.needs_user() {
            shared.update(|state| state.set_link(index, link, failures, None));
            drop(turn);
            if retry.changed().await.is_err() {
                return;
            }
            interactive = retry.borrow().1;
            quiet = false;
            failures = 0;
            continue;
        }
        let delay = shared.options.retry_delay(failures);
        let retry_at = Local::now() + delay;
        shared.update(|state| state.set_link(index, link, failures, Some(retry_at)));
        drop(turn);
        quiet = true;
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            changed = retry.changed() => {
                if changed.is_err() {
                    return;
                }
                // Lo ha pedido el usuario: que vea cómo va.
                interactive = retry.borrow().1;
                quiet = false;
            }
        }
    }
}

/// Recuerda en el equipo los secretos recién validados que el usuario pidió
/// guardar: la próxima reconexión no tendrá que preguntarlos.
fn remember(endpoint: &mut Endpoint, saved: &[(String, String)]) {
    let mut hop = Some(endpoint);
    while let Some(current) = hop {
        for (key, value) in saved {
            if *key == current.session.password_key() {
                current.creds.password = Some(value.clone());
            } else if *key == current.session.passphrase_key() {
                current.creds.passphrase = Some(value.clone());
            }
        }
        hop = current.via.as_deref_mut();
    }
}

/// Conecta con el equipo. Las preguntas de la conexión (contraseña, clave
/// del servidor) se pasan al usuario solo si `interactive`; si no, se
/// contestan que no y se anota en lo que se devuelve.
async fn connect(
    index: usize,
    shared: &Shared,
    endpoint: &Endpoint,
    prompter: &Prompter,
    interactive: bool,
    quiet: bool,
    gate: SemaphorePermit<'_>,
) -> (Result<Connected>, Declined) {
    // Un `Prompter` propio, para ver las preguntas antes que el usuario.
    let (tx, rx) = std::sync::mpsc::channel();
    let asked = Arc::new(Notify::new());
    let local = Prompter::new(tx, {
        let asked = asked.clone();
        Arc::new(move || asked.notify_one())
    });
    let status = |text: String| shared.progress(index, text, quiet);
    let connecting = crate::ssh::connect_endpoint(endpoint, &local, &status);
    tokio::pin!(connecting);
    let mut gate = Some(gate);
    let mut declined = Declined::default();
    let mut deadline = tokio::time::Instant::now() + shared.options.connect_timeout;
    loop {
        tokio::select! {
            result = &mut connecting => return (result, declined),
            _ = asked.notified() => {
                while let Ok(request) = rx.try_recv() {
                    if interactive {
                        // Quien espera al usuario no ocupa turno de conexión.
                        gate.take();
                        shared.progress(index, tr("Esperando tu respuesta…").into(), quiet);
                    }
                    match request {
                        PromptRequest::HostKey { host, port, algorithm, fingerprint, reply } => {
                            let accepted = interactive
                                && prompter.confirm_host_key(&host, port, algorithm, fingerprint).await;
                            declined.host_key |= !accepted;
                            let _ = reply.send(accepted);
                        }
                        PromptRequest::Secret { title, message, echo, allow_save, reply } => {
                            let answer = if interactive {
                                prompter.secret(&title, &message, echo, allow_save).await
                            } else {
                                None
                            };
                            declined.secret |= answer.is_none();
                            let _ = reply.send(answer);
                        }
                    }
                    // El tiempo que tarde el usuario no cuenta.
                    deadline = tokio::time::Instant::now() + shared.options.connect_timeout;
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                let error = anyhow!("{}", tr("el equipo no terminó de conectar a tiempo"));
                return (Err(error), declined);
            }
        }
    }
}

async fn next_sample(sampler: &mut Option<Sampler>) -> Result<Stats> {
    match sampler {
        Some(sampler) => sampler.next().await,
        None => std::future::pending().await,
    }
}

/// Una conexión con el equipo, de principio a fin: conecta y, mientras
/// dure, recoge las muestras y mide la latencia.
#[allow(clippy::too_many_arguments)]
async fn session(
    index: usize,
    shared: &Shared,
    slot: &Slot,
    prompter: &Prompter,
    conn: &mut Option<SshHandle>,
    interactive: bool,
    quiet: bool,
    healthy: &mut bool,
) -> End {
    let lost = |error: &str| End::Failed {
        error: error.to_string(),
        declined: Declined::default(),
    };
    let gate = shared
        .connecting
        .acquire()
        .await
        .expect("el semáforo no se cierra");
    shared.progress(index, tr("Conectando…").into(), quiet);
    let endpoint = slot.endpoint().clone();
    let (result, declined) =
        connect(index, shared, &endpoint, prompter, interactive, quiet, gate).await;
    let connected = match result {
        Ok(connected) => connected,
        Err(e) => {
            return End::Failed {
                error: format!("{e:#}"),
                declined,
            };
        }
    };
    let handle = connected.handle;
    *conn = Some(handle.clone());
    if !connected.to_save.is_empty() {
        remember(&mut slot.endpoint(), &connected.to_save);
        shared.update(|state| state.secrets.extend(connected.to_save));
    }
    shared.set_link(index, Link::Online);

    let mut interval = shared.interval.subscribe();
    let mut every = Duration::from_secs(u64::from(*interval.borrow_and_update()));
    // La latencia se mide con cada muestra, y al menos una vez por minuto.
    let pinger = |every: Duration| {
        let mut ping = tokio::time::interval(every.min(Duration::from_secs(60)));
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ping
    };
    let mut ping = pinger(every);
    let mut sampler: Option<Sampler> = None;
    // Cuándo (volver a) lanzar la consulta, y cuántas veces ha fallado.
    let mut probe_at = tokio::time::Instant::now();
    let mut probe_failures = 0u32;
    let mut samples = 0u32;
    loop {
        let probe_error = tokio::select! {
            _ = tokio::time::sleep_until(probe_at), if sampler.is_none() => {
                let command = crate::monitor::probe_command(every.as_secs() as u32, Probe::Light);
                let start = Sampler::start(handle.clone(), command);
                match tokio::time::timeout(shared.options.connect_timeout, start).await {
                    Ok(Ok(started)) => {
                        sampler = Some(started);
                        None
                    }
                    Ok(Err(e)) => Some(format!("{e:#}")),
                    // Ni siquiera contesta: la conexión no está para más.
                    Err(_) => return lost(tr("el servidor dejó de contestar")),
                }
            }
            sample = next_sample(&mut sampler), if sampler.is_some() => match sample {
                Ok(stats) => {
                    samples += 1;
                    probe_failures = 0;
                    *healthy = true;
                    let first = samples == 1;
                    shared.update(|state| state.sample(index, stats, Instant::now(), first));
                    // Con más equipos que conexiones permitidas, se cede el
                    // turno en cuanto hay datos (dos muestras: la segunda
                    // trae el uso de CPU).
                    if samples >= 2 && shared.waiting.load(Ordering::SeqCst) > 0 {
                        if let Some(sampler) = sampler.take() {
                            sampler.close().await;
                        }
                        return End::Rotate;
                    }
                    None
                }
                Err(e) => {
                    // (Sin dejar su canal abierto: se abrirá otro.)
                    if let Some(sampler) = sampler.take() {
                        sampler.close().await;
                    }
                    Some(format!("{e:#}"))
                }
            },
            _ = ping.tick() => {
                let began = Instant::now();
                let answered = tokio::time::timeout(shared.options.ping_timeout, handle.send_ping())
                    .await
                    .is_ok_and(|sent| sent.is_ok());
                if !answered || handle.is_closed() {
                    return lost(tr("se perdió la conexión con el servidor"));
                }
                let latency = began.elapsed();
                *healthy = true;
                shared.update(|state| state.hosts[index].latency = Some(latency));
                None
            }
            changed = interval.changed() => {
                if changed.is_err() {
                    return lost(tr("el panel se cerró"));
                }
                // Otro intervalo: se relanza la consulta por la misma conexión.
                every = Duration::from_secs(u64::from(*interval.borrow_and_update()));
                ping = pinger(every);
                if let Some(sampler) = sampler.take() {
                    sampler.close().await;
                }
                probe_at = tokio::time::Instant::now();
                None
            }
        };
        if let Some(error) = probe_error {
            // Si lo que ha fallado es la conexión, se reconecta; si solo la
            // consulta (el servidor no deja ejecutar, no hay shell POSIX),
            // se insiste por la misma conexión, cada vez más de tarde en
            // tarde.
            if handle.is_closed() {
                return lost(tr("se perdió la conexión con el servidor"));
            }
            let error = brief(&error, MAX_ERROR);
            shared.update(|state| state.hosts[index].probe_error = Some(error));
            probe_at = tokio::time::Instant::now() + shared.options.retry_delay(probe_failures);
            probe_failures += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(cpu: f32) -> Stats {
        Stats {
            cpu: Some(cpu),
            cores: Some(4),
            load: Some([0.5, 0.4, 0.3]),
            memory: Some((4_000_000, 16_000_000)),
            uptime_secs: Some(3600),
            disks: vec![disk("/", "ext4", 60)],
            ..Default::default()
        }
    }

    fn disk(mount: &str, kind: &str, percent: u64) -> Disk {
        Disk {
            mount: mount.into(),
            filesystem: "/dev/sda1".into(),
            total: 1000,
            used: percent * 10,
            available: (100 - percent) * 10,
            kind: kind.into(),
            read_only: false,
        }
    }

    fn kinds(changes: &[AlertChange]) -> Vec<(AlertKind, bool)> {
        changes.iter().map(|c| (c.kind, c.raised)).collect()
    }

    #[test]
    fn sustained_alerts_need_several_samples_both_ways() {
        let limits = Thresholds::default();
        let mut alerts = Alerts::default();
        // Dos muestras altas no bastan, y una baja en medio reinicia la cuenta.
        assert!(alerts.observe(&limits, &stats(95.0)).is_empty());
        assert!(alerts.observe(&limits, &stats(99.0)).is_empty());
        assert!(alerts.observe(&limits, &stats(10.0)).is_empty());
        assert!(alerts.observe(&limits, &stats(95.0)).is_empty());
        assert!(alerts.observe(&limits, &stats(90.0)).is_empty());
        // La tercera seguida, sí; y solo avisa una vez.
        let changes = alerts.observe(&limits, &stats(97.4));
        assert_eq!(kinds(&changes), vec![(AlertKind::Cpu, true)]);
        assert!(changes[0].detail.contains("CPU al 97 %"), "{changes:?}");
        assert!(changes[0].detail.contains("3 muestras"), "{changes:?}");
        assert!(alerts.is_active(AlertKind::Cpu) && alerts.any());
        for _ in 0..5 {
            assert!(alerts.observe(&limits, &stats(100.0)).is_empty());
        }
        // Resolverse también pide tres seguidas por debajo.
        assert!(alerts.observe(&limits, &stats(20.0)).is_empty());
        assert!(alerts.observe(&limits, &stats(95.0)).is_empty());
        assert!(alerts.observe(&limits, &stats(20.0)).is_empty());
        assert!(alerts.observe(&limits, &stats(89.9)).is_empty());
        let changes = alerts.observe(&limits, &stats(30.0));
        assert_eq!(kinds(&changes), vec![(AlertKind::Cpu, false)]);
        assert!(!alerts.any());
        // Una muestra sin dato de CPU (la primera) ni suma ni resta.
        let mut alerts = Alerts::default();
        alerts.observe(&limits, &stats(95.0));
        alerts.observe(&limits, &stats(95.0));
        let unknown = Stats {
            cpu: None,
            ..stats(0.0)
        };
        assert!(alerts.observe(&limits, &unknown).is_empty());
        assert_eq!(
            kinds(&alerts.observe(&limits, &stats(95.0))),
            vec![(AlertKind::Cpu, true)]
        );

        // La carga se mide por núcleo: 8 con 4 núcleos es el doble.
        let loaded = |load: f32, cores: Option<u32>| Stats {
            load: Some([load, 1.0, 1.0]),
            cores,
            ..stats(5.0)
        };
        let mut alerts = Alerts::default();
        assert!(alerts.observe(&limits, &loaded(7.9, Some(4))).is_empty());
        for _ in 0..2 {
            assert!(alerts.observe(&limits, &loaded(8.0, Some(4))).is_empty());
        }
        let changes = alerts.observe(&limits, &loaded(9.5, Some(4)));
        assert_eq!(kinds(&changes), vec![(AlertKind::Load, true)]);
        assert!(
            changes[0].detail.contains("carga 9,50 con 4 núcleo"),
            "{changes:?}"
        );
        // Sin saber los núcleos no se puede juzgar la carga.
        let mut alerts = Alerts::default();
        for _ in 0..10 {
            assert!(alerts.observe(&limits, &loaded(50.0, None)).is_empty());
        }
        // Con «1 muestra» salta a la primera.
        let quick = Thresholds {
            sustained: 1,
            ..Default::default()
        };
        let mut alerts = Alerts::default();
        assert_eq!(
            kinds(&alerts.observe(&quick, &stats(91.0))),
            vec![(AlertKind::Cpu, true)]
        );
        assert_eq!(
            kinds(&alerts.observe(&quick, &stats(50.0))),
            vec![(AlertKind::Cpu, false)]
        );
    }

    #[test]
    fn level_alerts_fire_at_once_and_recover_with_a_margin() {
        let limits = Thresholds::default();
        let memory = |percent: u64| Stats {
            memory: Some((percent * 1000, 100_000)),
            ..stats(5.0)
        };
        let mut alerts = Alerts::default();
        assert!(alerts.observe(&limits, &memory(89)).is_empty());
        let changes = alerts.observe(&limits, &memory(90));
        assert_eq!(kinds(&changes), vec![(AlertKind::Memory, true)]);
        assert!(changes[0].detail.contains("memoria al 90 %"), "{changes:?}");
        // Bailar alrededor del umbral no avisa una y otra vez…
        for percent in [89, 91, 89, 95, 88] {
            assert!(alerts.observe(&limits, &memory(percent)).is_empty());
        }
        // …hasta que baja con margen.
        assert_eq!(
            kinds(&alerts.observe(&limits, &memory(87))),
            vec![(AlertKind::Memory, false)]
        );

        // El disco que cuenta es el más lleno de los de verdad.
        let disks = |list: Vec<Disk>| Stats {
            disks: list,
            ..stats(5.0)
        };
        let mut alerts = Alerts::default();
        let snap = Disk {
            read_only: true,
            ..disk("/snap/core/1", "squashfs", 100)
        };
        let quiet = disks(vec![
            disk("/", "ext4", 40),
            snap.clone(),
            disk("/run", "tmpfs", 99),
        ]);
        assert!(alerts.observe(&limits, &quiet).is_empty());
        let full = disks(vec![disk("/", "ext4", 40), snap, disk("/var", "xfs", 93)]);
        let changes = alerts.observe(&limits, &full);
        assert_eq!(kinds(&changes), vec![(AlertKind::Disk, true)]);
        assert!(
            changes[0].detail.contains("disco /var al 93 %"),
            "{changes:?}"
        );
        assert!(alerts.observe(&limits, &full).is_empty());
        assert_eq!(
            kinds(&alerts.observe(&limits, &quiet)),
            vec![(AlertKind::Disk, false)]
        );
        // Varias cosas a la vez: cada una avisa por su lado.
        let mut alerts = Alerts::default();
        let both = Stats {
            memory: Some((99, 100)),
            ..full.clone()
        };
        assert_eq!(
            kinds(&alerts.observe(&limits, &both)),
            vec![(AlertKind::Memory, true), (AlertKind::Disk, true)]
        );
        // Un equipo que no da algún dato no dispara ni rompe nada.
        let mut alerts = Alerts::default();
        assert!(alerts.observe(&limits, &Stats::default()).is_empty());
    }

    #[test]
    fn switched_off_thresholds_withdraw_their_alerts_quietly() {
        let limits = Thresholds::default();
        let mut alerts = Alerts::default();
        let bad = Stats {
            memory: Some((99, 100)),
            disks: vec![disk("/", "ext4", 99)],
            ..stats(5.0)
        };
        assert_eq!(alerts.observe(&limits, &bad).len(), 2);
        assert_eq!(
            kinds(&alerts.observe_link(&limits, Reach::Unreachable("Connection refused"))),
            vec![(AlertKind::Unreachable, true)]
        );
        assert_eq!(alerts.active().count(), 3);
        let off = Thresholds {
            memory: 0.0,
            disk: 0.0,
            unreachable: false,
            ..Default::default()
        };
        // Ni «resuelto» ni nada: simplemente dejan de contar.
        assert!(alerts.observe(&off, &bad).is_empty());
        assert!(
            alerts
                .observe_link(&off, Reach::Unreachable("x"))
                .is_empty()
        );
        assert!(alerts.observe_link(&off, Reach::NoAccess("x")).is_empty());
        assert!(!alerts.any());
        // Al volver a vigilarlos, saltan de nuevo si siguen mal.
        assert_eq!(alerts.observe(&limits, &bad).len(), 2);
    }

    fn state(names: &[&str]) -> State {
        let hosts = names
            .iter()
            .map(|name| {
                HostRow::new(
                    Uuid::new_v4(),
                    name.to_string(),
                    format!("root@{name}.interno:22"),
                )
            })
            .collect();
        State::new(hosts, DashboardSettings::default())
    }

    #[test]
    fn transitions_are_recorded_once_and_not_per_sample() {
        let mut state = state(&["web01", "db01"]);
        let now = Instant::now();
        // Una conexión que se cae no avisa hasta que falla el reintento.
        state.set_link(0, Link::Online, 0, None);
        state.sample(0, stats(10.0), now, false);
        state.set_link(0, Link::Down("se perdió la conexión".into()), 0, None);
        assert!(state.history.is_empty());
        assert_eq!(state.hosts[0].latency, None);
        assert!(state.hosts[0].stats.is_some(), "se conserva lo último");
        state.set_link(0, Link::Down("Connection refused".into()), 1, None);
        state.set_link(0, Link::Down("Connection refused".into()), 2, None);
        state.set_link(0, Link::Down("Connection refused".into()), 3, None);
        assert_eq!(state.history.len(), 1);
        let event = &state.history[0];
        assert_eq!(
            (event.name.as_str(), event.kind, event.raised),
            ("web01", AlertKind::Unreachable, true)
        );
        assert!(event.detail.contains("Connection refused"), "{event:?}");
        assert!(state.hosts[0].has_problem());
        assert_eq!(state.alerting(), 1);
        // Los pasos intermedios del reintento no lo dan por resuelto.
        state.hosts[0].step = Some("Conectando…".into());
        assert_eq!(state.history.len(), 1);
        state.set_link(0, Link::Online, 3, None);
        assert_eq!(state.history.len(), 2);
        assert!(!state.history[1].raised);
        assert!(!state.hosts[0].alerts.any());

        // Muestras: avisa al pasar de bien a mal y al revés, no cada vez.
        state.set_link(1, Link::Online, 0, None);
        let full = Stats {
            disks: vec![disk("/", "ext4", 95)],
            ..stats(10.0)
        };
        for _ in 0..6 {
            state.sample(1, full.clone(), now, false);
        }
        assert_eq!(state.history.len(), 3);
        for _ in 0..6 {
            state.sample(1, stats(10.0), now, false);
        }
        assert_eq!(state.history.len(), 4);
        let news = std::mem::take(&mut state.news);
        assert_eq!(news.len(), 4);
        assert_eq!(
            news.iter().map(|e| e.raised).collect::<Vec<_>>(),
            vec![true, false, true, false]
        );
        assert_eq!(state.hosts[1].trend.len(), 1, "muestras del mismo instante");

        // La primera muestra tras reconectar no trae el uso de CPU: se sigue
        // viendo el anterior, sin contarlo dos veces para las alertas ni
        // para la gráfica.
        let no_cpu = Stats {
            cpu: None,
            ..stats(0.0)
        };
        state.sample(1, stats(42.0), now + Duration::from_secs(60), false);
        state.sample(1, no_cpu.clone(), now + Duration::from_secs(120), true);
        assert_eq!(state.hosts[1].stats.as_ref().unwrap().cpu, Some(42.0));
        assert_eq!(state.hosts[1].trend.points().last().unwrap().cpu, None);
        // En las siguientes, si no viene es que el equipo no lo da.
        state.sample(1, no_cpu, now + Duration::from_secs(180), false);
        assert_eq!(state.hosts[1].stats.as_ref().unwrap().cpu, None);
        assert_eq!(state.history.len(), 4);

        // Falta la credencial: ya no es «inalcanzable», sino «sin acceso»
        // (se avisa: mientras tanto el equipo no se vigila).
        state.set_link(0, Link::Down("timeout".into()), 1, None);
        assert!(state.hosts[0].alerts.is_active(AlertKind::Unreachable));
        state.set_link(0, Link::NeedsCredential, 0, None);
        state.set_link(0, Link::NeedsCredential, 0, None);
        assert_eq!(
            state.hosts[0].alerts.active().collect::<Vec<_>>(),
            vec![AlertKind::Access]
        );
        assert!(state.hosts[0].has_problem());
        assert_eq!(state.history.len(), 6);
        assert!(state.history[5].raised && state.history[5].detail.contains("baúl"));
        // Mientras se reintenta no cambia nada; al entrar, se da por resuelta.
        state.set_link(0, Link::Connecting("Conectando…".into()), 0, None);
        assert_eq!(state.history.len(), 6);
        state.set_link(0, Link::Online, 0, None);
        assert_eq!(state.history.len(), 7);
        assert_eq!(
            (state.history[6].kind, state.history[6].raised),
            (AlertKind::Access, false)
        );
        // Lo mismo si el servidor rechaza el acceso; y si después deja de
        // responder, pasa a ser «inalcanzable» sin dar lo otro por resuelto.
        state.set_link(0, Link::Rejected("clave rechazada".into()), 1, None);
        assert_eq!(state.history.len(), 8);
        assert!(state.history[7].detail.contains("clave rechazada"));
        state.set_link(0, Link::Down("timeout".into()), 1, None);
        assert_eq!(state.history.len(), 9);
        assert_eq!(
            state.hosts[0].alerts.active().collect::<Vec<_>>(),
            vec![AlertKind::Unreachable]
        );
        state.set_link(0, Link::Online, 0, None);
        assert_eq!(state.history.len(), 10);
        assert!(!state.hosts[0].alerts.any());
        state.news.clear();

        // Sin avisos en la aplicación, el historial sigue.
        let mut quiet = state.settings.clone();
        quiet.notify = false;
        state.configure(quiet);
        assert!(state.news.is_empty());
        state.set_link(1, Link::Down("x".into()), 1, None);
        assert!(state.news.is_empty());
        assert_eq!(state.history.len(), 11);
        // Y el historial no crece sin límite.
        for round in 0..HISTORY_MAX {
            let link = if round % 2 == 0 {
                Link::Online
            } else {
                Link::Down("x".into())
            };
            state.set_link(1, link, 1, None);
        }
        assert_eq!(state.history.len(), HISTORY_MAX);
    }

    #[test]
    fn news_are_summarized_in_one_notice() {
        let event = |name: &str, kind, raised, detail: &str| AlertEvent {
            at: Local::now(),
            host: Uuid::new_v4(),
            name: name.into(),
            kind,
            raised,
            detail: detail.into(),
        };
        assert_eq!(summarize(&[]), None);
        let one = [event("web01", AlertKind::Cpu, true, "CPU al 97 %")];
        assert_eq!(
            summarize(&one),
            Some(("Alerta: web01 — CPU al 97 %".into(), true))
        );
        let back = [event(
            "web01",
            AlertKind::Unreachable,
            false,
            "vuelve a responder",
        )];
        assert_eq!(
            summarize(&back),
            Some(("Resuelto: web01 — vuelve a responder".into(), false))
        );
        // Con buenas y malas noticias, mandan las malas.
        let mut many: Vec<AlertEvent> = (1..=6)
            .map(|i| event(&format!("srv{i}"), AlertKind::Disk, true, "disco"))
            .collect();
        many.push(back[0].clone());
        let (text, bad) = summarize(&many).unwrap();
        assert!(bad);
        assert_eq!(
            text,
            "6 alertas nuevas: srv1 (disco), srv2 (disco), srv3 (disco), srv4 (disco), y 2 más"
        );
        let (text, bad) = summarize(&[back[0].clone(), back[0].clone()]).unwrap();
        assert!(!bad);
        assert!(text.starts_with("2 alertas resueltas: web01 (inalcanzable)"));
    }

    #[test]
    fn only_real_disks_count_as_the_fullest() {
        let mut list = vec![
            disk("/", "ext4", 40),
            disk("/boot/efi", "vfat", 5),
            disk("/dev/shm", "tmpfs", 99),
            disk("/var/lib/docker/overlay2/x/merged", "overlay", 98),
            disk("/mnt/remoto", "fuse.sshfs", 97),
            disk("/mnt/usb", "fuseblk", 70),
            disk("/snap/core/1", "squashfs", 100),
        ];
        list.push(Disk {
            read_only: true,
            ..disk("/mnt/cd", "ext4", 100)
        });
        list.push(Disk {
            total: 0,
            used: 0,
            available: 0,
            ..disk("/proc", "proc", 0)
        });
        let stats = Stats {
            disks: list,
            ..Default::default()
        };
        assert_eq!(fullest_disk(&stats).unwrap().mount, "/mnt/usb");
        // En un contenedor la raíz es una capa, y aun así cuenta.
        let container = Stats {
            disks: vec![disk("/", "overlay", 88), disk("/dev/shm", "tmpfs", 99)],
            ..Default::default()
        };
        assert_eq!(fullest_disk(&container).unwrap().mount, "/");
        // Sin tipos (no es Linux) se mira el nombre del dispositivo.
        let named = |mount: &str, device: &str, percent| Disk {
            filesystem: device.into(),
            ..disk(mount, "", percent)
        };
        let bsd = Stats {
            disks: vec![
                named("/", "/dev/ada0p2", 30),
                named("/tmp", "tmpfs", 99),
                named("/dev", "devfs", 100),
                named("/datos", "zroot/datos", 60),
                named("/snap/x", "/dev/loop3", 100),
            ],
            ..Default::default()
        };
        assert_eq!(fullest_disk(&bsd).unwrap().mount, "/datos");
        assert_eq!(fullest_disk(&Stats::default()), None);
    }

    #[test]
    fn trend_keeps_a_window_and_merges_dense_samples() {
        let start = Instant::now();
        let at = |secs: u64| start + Duration::from_secs(secs);
        let window = Duration::from_secs(15 * 60);
        let mut trend = Trend::default();
        assert!(trend.is_empty());
        // Una muestra cada 10 s durante 20 minutos: quedan los últimos 15.
        for i in 0..=120 {
            trend.push(at(i * 10), Some(i as f32), Some(1.0), window);
        }
        assert_eq!(trend.len(), 91);
        let points: Vec<&TrendPoint> = trend.points().collect();
        assert_eq!(points[0].at, at(300));
        assert_eq!(points[90].cpu, Some(120.0));
        // Muestras muy seguidas para la ventana pedida se promedian: la
        // gráfica nunca guarda más de `TREND_POINTS` puntos (y pico).
        let window = Duration::from_secs(240 * 60);
        let mut trend = Trend::default();
        for i in 0..20_000u64 {
            let cpu = if i % 2 == 0 { 20.0 } else { 40.0 };
            trend.push(at(i * 2), Some(cpu), None, window);
        }
        assert!(trend.len() <= TREND_POINTS as usize + 1, "{}", trend.len());
        let last = trend.points().nth(trend.len() - 2).unwrap();
        assert!((last.cpu.unwrap() - 30.0).abs() < 1.0, "{last:?}");
        assert_eq!(last.load, None);
        // Los huecos sin dato no se inventan.
        let mut trend = Trend::default();
        trend.push(at(0), None, Some(2.0), window);
        trend.push(at(1), Some(50.0), None, window);
        let only = trend.points().next().unwrap();
        assert_eq!((only.cpu, only.load), (Some(50.0), Some(2.0)));
        trend.prune(at(240 * 60 + 5), window);
        assert!(trend.is_empty());
    }

    #[test]
    fn rows_sort_and_filter() {
        let mut state = state(&["web02", "Web01", "db01", "cache"]);
        let now = Instant::now();
        let with = |cpu: f32, memory: u64, disk_percent: u64, uptime: u64| Stats {
            memory: Some((memory, 100)),
            disks: vec![disk("/", "ext4", disk_percent)],
            uptime_secs: Some(uptime),
            ..stats(cpu)
        };
        state.set_link(0, Link::Online, 0, None);
        state.sample(0, with(50.0, 30, 95, 500), now, false);
        state.hosts[0].latency = Some(Duration::from_millis(40));
        state.set_link(1, Link::Online, 0, None);
        state.sample(1, with(5.0, 80, 20, 900), now, false);
        state.hosts[1].latency = Some(Duration::from_millis(3));
        state.set_link(2, Link::Down("Connection refused".into()), 2, None);
        // «cache» sigue en cola, sin datos.
        let names = |rows: Vec<usize>| -> Vec<&str> {
            rows.into_iter()
                .map(|i| state.hosts[i].name.as_str())
                .collect()
        };
        let order = |column, descending| names(state.order("", false, column, descending));
        assert_eq!(
            order(Column::Name, false),
            ["cache", "db01", "Web01", "web02"]
        );
        assert_eq!(
            order(Column::Name, true),
            ["web02", "Web01", "db01", "cache"]
        );
        // Lo que no tiene dato va siempre al final.
        assert_eq!(
            order(Column::Cpu, true),
            ["web02", "Web01", "cache", "db01"]
        );
        assert_eq!(
            order(Column::Cpu, false),
            ["Web01", "web02", "cache", "db01"]
        );
        assert_eq!(
            order(Column::Memory, true),
            ["Web01", "web02", "cache", "db01"]
        );
        assert_eq!(
            order(Column::Disk, true),
            ["web02", "Web01", "cache", "db01"]
        );
        assert_eq!(
            order(Column::Latency, false),
            ["Web01", "web02", "cache", "db01"]
        );
        assert_eq!(
            order(Column::Uptime, true),
            ["Web01", "web02", "cache", "db01"]
        );
        // Por estado: primero lo que va peor (web02 tiene el disco lleno).
        assert_eq!(
            order(Column::State, false),
            ["db01", "web02", "cache", "Web01"]
        );
        // Filtro por nombre, dirección o estado; y «solo con problemas».
        let filtered = |filter, problems| names(state.order(filter, problems, Column::Name, false));
        assert_eq!(filtered(" WEB ", false), ["Web01", "web02"]);
        assert_eq!(filtered("db01.interno", false), ["db01"]);
        assert_eq!(filtered("sin conexión", false), ["db01"]);
        assert_eq!(filtered("disco", false), ["web02"]);
        assert_eq!(filtered("", true), ["db01", "web02"]);
        assert!(filtered("nada", false).is_empty());
        assert_eq!((state.online(), state.alerting()), (2, 2));
        // Datos viejos: sin conexión, o sin muestras desde hace rato.
        assert!(!state.hosts[0].is_stale(now + Duration::from_secs(20), 10));
        assert!(state.hosts[0].is_stale(now + Duration::from_secs(40), 10));
        assert!(state.hosts[2].is_stale(now, 10) && state.hosts[3].is_stale(now, 10));
    }

    #[test]
    fn settings_load_old_files_and_are_validated() {
        // Un fichero de antes de que existiera el panel, o con campos de menos.
        let settings: DashboardSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings, DashboardSettings::default());
        let mut settings: DashboardSettings = serde_json::from_str(
            r#"{"intervalo":0,"conexiones":100000,"tendencia":"inventada","minutos_tendencia":0,
                "umbrales":{"cpu":250,"carga":-3,"muestras":0,"disco":80}}"#,
        )
        .unwrap();
        settings.sanitize();
        assert_eq!(settings.interval, DashboardSettings::MIN_INTERVAL);
        assert_eq!(settings.max_connections, DashboardSettings::MAX_CONNECTIONS);
        assert_eq!(settings.trend, TrendMetric::Cpu);
        assert_eq!(settings.trend_minutes, 1);
        let limits = &settings.thresholds;
        assert_eq!((limits.cpu, limits.load, limits.sustained), (100.0, 0.0, 1));
        assert_eq!((limits.memory, limits.disk), (90.0, 80.0));
        assert!(limits.unreachable && settings.notify);
        // Se guarda con nombres legibles y vuelve igual.
        settings.trend = TrendMetric::Load;
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains(r#""tendencia":"carga""#), "{json}");
        assert!(json.contains(r#""umbrales":{"cpu":100.0"#), "{json}");
        assert_eq!(
            serde_json::from_str::<DashboardSettings>(&json).unwrap(),
            settings
        );
        let nan = Thresholds {
            memory: f32::NAN,
            ..Default::default()
        };
        let mut fixed = nan.clone();
        fixed.sanitize();
        assert_eq!(fixed, Thresholds::default());
    }

    #[test]
    fn outside_texts_are_cut_short() {
        assert_eq!(brief("corto", 10), "corto");
        assert_eq!(brief("justo", 5), "justo");
        assert_eq!(brief("demasiado largo", 9), "demasiado…");
        // Sin partir letras de varios bytes.
        assert_eq!(brief("añoñá", 3), "año…");
        let mut alerts = Alerts::default();
        let long = Disk {
            mount: format!("/{}", "x".repeat(5000)),
            ..disk("/", "ext4", 99)
        };
        let stats = Stats {
            disks: vec![long],
            ..Default::default()
        };
        let changes = alerts.observe(&Thresholds::default(), &stats);
        assert!(changes[0].detail.chars().count() < 120, "{changes:?}");
    }

    #[test]
    fn retries_wait_longer_each_time() {
        let options = Options::default();
        let delays: Vec<u64> = (0..10).map(|n| options.retry_delay(n).as_secs()).collect();
        assert_eq!(delays, [2, 5, 10, 20, 30, 60, 120, 120, 120, 120]);
        let none = Options {
            retry_delays: vec![],
            ..Default::default()
        };
        assert_eq!(none.retry_delay(3), Duration::from_secs(60));
    }

    #[test]
    fn saved_secrets_are_remembered_for_the_next_reconnection() {
        use crate::model::Session;
        let hop = Session::default();
        let target = Session::default();
        let mut endpoint = Endpoint::through(
            [(hop.clone(), Default::default())],
            target.clone(),
            Default::default(),
        );
        remember(
            &mut endpoint,
            &[
                (target.passphrase_key(), "frase".into()),
                (hop.password_key(), "clave".into()),
                ("otra/password".into(), "x".into()),
            ],
        );
        assert_eq!(endpoint.creds.passphrase.as_deref(), Some("frase"));
        assert_eq!(endpoint.creds.password, None);
        let via = endpoint.via.as_ref().unwrap();
        assert_eq!(via.creds.password.as_deref(), Some("clave"));
    }
}
