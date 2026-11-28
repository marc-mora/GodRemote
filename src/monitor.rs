//! Información del servidor (CPU, memoria, disco, red…) para la barra que se
//! muestra bajo los terminales SSH.
//!
//! Se abre un canal aparte sobre la misma conexión SSH y se deja en él un
//! pequeño bucle de shell que imprime los contadores del sistema cada pocos
//! segundos. Los porcentajes y velocidades se calculan aquí, comparando cada
//! muestra con la anterior. No toca la shell del usuario.

use std::collections::HashMap;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use russh::ChannelMsg;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::ssh::{SshHandle, Waker};

/// Datos que se pueden mostrar en la barra.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BarItem {
    /// Nombre de la sesión en GodRemote.
    Name,
    /// Nombre que el servidor dice tener.
    Host,
    /// `usuario@host:puerto` con el que se conectó.
    Target,
    Ip,
    Os,
    Kernel,
    Uptime,
    Load,
    Cpu,
    Memory,
    Swap,
    Disk,
    Network,
    Users,
    Processes,
    Time,
}

impl BarItem {
    pub const ALL: [BarItem; 16] = [
        BarItem::Name,
        BarItem::Host,
        BarItem::Target,
        BarItem::Ip,
        BarItem::Os,
        BarItem::Kernel,
        BarItem::Uptime,
        BarItem::Load,
        BarItem::Cpu,
        BarItem::Memory,
        BarItem::Swap,
        BarItem::Disk,
        BarItem::Network,
        BarItem::Users,
        BarItem::Processes,
        BarItem::Time,
    ];

    /// Nombre con el que se guarda en las preferencias.
    pub fn key(self) -> &'static str {
        match self {
            BarItem::Name => "nombre",
            BarItem::Host => "equipo",
            BarItem::Target => "destino",
            BarItem::Ip => "ip",
            BarItem::Os => "sistema",
            BarItem::Kernel => "kernel",
            BarItem::Uptime => "encendido",
            BarItem::Load => "carga",
            BarItem::Cpu => "cpu",
            BarItem::Memory => "memoria",
            BarItem::Swap => "swap",
            BarItem::Disk => "disco",
            BarItem::Network => "red",
            BarItem::Users => "usuarios",
            BarItem::Processes => "procesos",
            BarItem::Time => "hora",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|i| i.key() == key)
    }

    /// Nombre visible en la ventana de ajustes.
    pub fn label(self) -> &'static str {
        match self {
            BarItem::Name => "Nombre de la sesión",
            BarItem::Host => "Nombre del servidor (hostname)",
            BarItem::Target => "Usuario y dirección de conexión",
            BarItem::Ip => "Direcciones IP del servidor",
            BarItem::Os => "Sistema operativo",
            BarItem::Kernel => "Kernel y arquitectura",
            BarItem::Uptime => "Tiempo encendido",
            BarItem::Load => "Carga media (1, 5 y 15 min)",
            BarItem::Cpu => "Uso de CPU",
            BarItem::Memory => "Memoria",
            BarItem::Swap => "Intercambio (swap)",
            BarItem::Disk => "Espacio libre en disco",
            BarItem::Network => "Tráfico de red",
            BarItem::Users => "Usuarios conectados",
            BarItem::Processes => "Procesos",
            BarItem::Time => "Fecha y hora del servidor",
        }
    }
}

fn items_to_json<S: Serializer>(items: &[BarItem], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(items.iter().map(|i| i.key()))
}

/// Lee la lista de elementos pasando por alto los nombres desconocidos (de
/// otra versión), en lugar de descartar todas las preferencias.
fn items_from_json<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<BarItem>, D::Error> {
    let keys = Vec::<String>::deserialize(d)?;
    Ok(keys.iter().filter_map(|k| BarItem::from_key(k)).collect())
}

/// Preferencias de la barra de información del servidor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerBar {
    #[serde(rename = "visible")]
    pub enabled: bool,
    /// Segundos entre consultas.
    #[serde(rename = "intervalo")]
    pub interval: u32,
    /// Elementos visibles, en el orden en que se muestran.
    #[serde(
        rename = "elementos",
        serialize_with = "items_to_json",
        deserialize_with = "items_from_json"
    )]
    pub items: Vec<BarItem>,
    /// Rutas cuyo espacio libre se muestra.
    #[serde(rename = "discos")]
    pub disks: Vec<String>,
    /// Dibujar una barrita de uso junto a CPU, memoria y disco.
    #[serde(rename = "barras")]
    pub gauges: bool,
    /// Porcentaje a partir del cual un valor se pinta de aviso.
    #[serde(rename = "aviso")]
    pub warn: f32,
    /// Porcentaje a partir del cual un valor se pinta de error.
    #[serde(rename = "critico")]
    pub critical: f32,
}

impl Default for ServerBar {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: 5,
            items: vec![
                BarItem::Host,
                BarItem::Os,
                BarItem::Uptime,
                BarItem::Cpu,
                BarItem::Memory,
                BarItem::Disk,
                BarItem::Network,
            ],
            disks: vec!["/".into()],
            gauges: true,
            warn: 75.0,
            critical: 90.0,
        }
    }
}

pub const MAX_DISKS: usize = 8;

/// Una ruta de disco se inserta en la orden que se ejecuta en el servidor,
/// así que solo se admiten rutas absolutas con caracteres inofensivos.
pub fn valid_disk(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 200
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-'))
}

/// Separa una lista de rutas escrita por el usuario (comas o espacios).
/// Devuelve las válidas y las rechazadas.
pub fn parse_disks(text: &str) -> (Vec<String>, Vec<String>) {
    let mut good: Vec<String> = vec![];
    let mut bad = vec![];
    for part in text
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|p| !p.is_empty())
    {
        if !valid_disk(part) {
            bad.push(part.to_string());
        } else if !good.iter().any(|g| g == part) && good.len() < MAX_DISKS {
            good.push(part.to_string());
        }
    }
    (good, bad)
}

impl ServerBar {
    pub fn sanitize(&mut self) {
        self.interval = self.interval.clamp(1, 3600);
        self.disks.retain(|d| valid_disk(d));
        self.disks.dedup();
        self.disks.truncate(MAX_DISKS);
        if self.disks.is_empty() {
            self.disks.push("/".into());
        }
        let mut seen = vec![];
        self.items.retain(|i| {
            let new = !seen.contains(i);
            seen.push(*i);
            new
        });
        let fix = |v: f32, default: f32| {
            if v.is_finite() {
                v.clamp(1.0, 100.0)
            } else {
                default
            }
        };
        self.warn = fix(self.warn, 75.0);
        self.critical = fix(self.critical, 90.0).max(self.warn);
    }

    /// Lo que obliga a relanzar la consulta si cambia.
    fn probe(&self) -> (u32, Vec<String>) {
        (self.interval, self.disks.clone())
    }
}

/// Qué se pregunta al servidor en cada muestra.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe<'a> {
    /// Todo lo que puede mostrar la barra, con el espacio de estas rutas.
    Full(&'a [String]),
    /// Lo justo para vigilar muchos equipos a la vez (el panel de estado):
    /// tiempo encendido, carga, CPU, memoria y el espacio de todos los
    /// sistemas de ficheros montados, con su tipo para poder descartar los
    /// que no son discos. Casi todo sale de `/proc` sin lanzar programas.
    Light,
}

/// Orden que se deja ejecutándose en el servidor. Va en una sola línea y sin
/// comillas simples ni barras invertidas para que la entienda cualquier shell
/// de entrada (bash, zsh, fish, csh…), que solo tiene que lanzar `sh`.
pub fn command(interval: u32, disks: &[String]) -> String {
    probe_command(interval, Probe::Full(disks))
}

/// Como [`command`], eligiendo qué se pregunta.
pub fn probe_command(interval: u32, probe: Probe) -> String {
    let interval = interval.clamp(1, 3600);
    let mut script: Vec<String> = vec![
        "export LC_ALL=C".into(),
        "s=1".into(),
        "while :".into(),
        "do echo @@begin".into(),
    ];
    if matches!(probe, Probe::Full(_)) {
        script.extend([
            r#"echo "host=$(hostname 2>/dev/null || uname -n)""#.to_string(),
            r#"echo "kernel=$(uname -sr)""#.into(),
            r#"echo "arch=$(uname -m)""#.into(),
            r#"if [ -r /etc/os-release ]; then ( . /etc/os-release; echo "os=$PRETTY_NAME" ); fi"#
                .into(),
            r#"echo "time=$(date "+%Y-%m-%d %H:%M:%S %Z")""#.into(),
        ]);
    }
    script.extend([
        r#"if [ -r /proc/uptime ]; then read up rest < /proc/uptime; echo "uptime=$up"; fi"#
            .to_string(),
        r#"if [ -r /proc/loadavg ]; then read l < /proc/loadavg; echo "load=$l"; fi"#.into(),
        r#"if [ -r /proc/stat ]; then read l < /proc/stat; echo "cpu=$l"; fi"#.into(),
        r#"echo "ncpu=$(getconf _NPROCESSORS_ONLN 2>/dev/null)""#.into(),
        r#"if [ -r /proc/meminfo ]; then while read k v u; do case $k in MemTotal:|MemAvailable:|MemFree:|Buffers:|Cached:|SwapTotal:|SwapFree:) echo "mem=$k$v";; esac; done < /proc/meminfo; fi"#.into(),
    ]);
    match probe {
        Probe::Full(disks) => {
            let disks: Vec<&str> = disks
                .iter()
                .map(String::as_str)
                .filter(|d| valid_disk(d))
                .collect();
            let disks = if disks.is_empty() {
                "/".to_string()
            } else {
                disks.join(" ")
            };
            script.extend([
                r#"if [ -r /proc/net/dev ]; then while read l; do echo "net=$l"; done < /proc/net/dev; fi"#
                    .to_string(),
                format!(r#"df -Pk {disks} 2>/dev/null | while read l; do echo "disk=$l"; done"#),
                r#"echo "users=$(who 2>/dev/null | wc -l)""#.into(),
                r#"echo "procs=$(ls -d /proc/[0-9]* 2>/dev/null | wc -l)""#.into(),
                r#"echo "ip=$(hostname -I 2>/dev/null)""#.into(),
            ]);
        }
        Probe::Light => script.extend([
            // Tipo y opciones de cada sistema de ficheros, y su punto de
            // montaje al final (puede llevar espacios).
            r#"if [ -r /proc/mounts ]; then while read -r d m t o r; do echo "mount=$t $o $m"; done < /proc/mounts; fi"#
                .to_string(),
            r#"df -Pk 2>/dev/null | while read l; do echo "disk=$l"; done"#.into(),
        ]),
    }
    script.extend([
        "echo @@end".to_string(),
        "sleep $s".into(),
        format!("s={interval}"),
        "done".into(),
    ]);
    let script = script.join("; ");
    debug_assert!(!script.contains('\'') && !script.contains('\\'));
    format!("exec sh -c '{script}'")
}

/// Espacio de un sistema de ficheros, en kB.
#[derive(Debug, Clone, PartialEq)]
pub struct Disk {
    pub mount: String,
    pub filesystem: String,
    pub total: u64,
    pub used: u64,
    pub available: u64,
    /// Tipo de sistema de ficheros (`ext4`, `tmpfs`…); vacío si el servidor
    /// no lo dijo (solo lo pregunta la consulta ligera).
    pub kind: String,
    /// Montado en solo lectura.
    pub read_only: bool,
}

impl Disk {
    /// Porcentaje ocupado (como lo calcula `df`: sobre lo usable).
    pub fn used_percent(&self) -> f32 {
        let usable = self.used + self.available;
        if usable == 0 {
            0.0
        } else {
            self.used as f32 * 100.0 / usable as f32
        }
    }
}

/// Estado del servidor en un momento dado, ya calculado.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    pub host: String,
    pub os: String,
    pub kernel: String,
    pub arch: String,
    pub time: String,
    pub ips: Vec<String>,
    pub uptime_secs: Option<u64>,
    pub load: Option<[f32; 3]>,
    /// Uso de CPU en porcentaje (desde la muestra anterior).
    pub cpu: Option<f32>,
    pub cores: Option<u32>,
    /// Memoria (usada, total), en kB.
    pub memory: Option<(u64, u64)>,
    /// Intercambio (usado, total), en kB.
    pub swap: Option<(u64, u64)>,
    pub disks: Vec<Disk>,
    /// Tráfico (bajada, subida), en bytes por segundo.
    pub network: Option<(f64, f64)>,
    pub users: Option<u32>,
    pub processes: Option<u32>,
}

/// Contadores tal como los da el servidor.
#[derive(Debug, Clone, Default)]
struct Raw {
    stats: Stats,
    uptime: Option<f64>,
    /// Tiempos de CPU (total, inactivo).
    cpu: Option<(u64, u64)>,
    /// Bytes (recibidos, enviados) sumando las interfaces reales.
    net: Option<(u64, u64)>,
    mem_total: Option<u64>,
    mem_available: Option<u64>,
    mem_free: Option<u64>,
    buffers: Option<u64>,
    cached: Option<u64>,
    swap_total: Option<u64>,
    swap_free: Option<u64>,
    /// Punto de montaje → (tipo, solo lectura), si el servidor lo dijo.
    mounts: HashMap<String, (String, bool)>,
}

/// Sistemas de ficheros que se anotan de cada muestra, como mucho: un
/// servidor no debe poder llenar la memoria con una lista interminable.
const MAX_MOUNTS: usize = 4096;

/// `/proc/mounts` escribe los espacios y otros caracteres raros de las
/// rutas como `\040` (barra invertida y tres cifras en octal).
fn unescape_mount(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let octal = bytes.get(i + 1..i + 4).filter(|d| {
            bytes[i] == b'\\' && d[0] <= b'3' && d.iter().all(|c| (b'0'..=b'7').contains(c))
        });
        match octal {
            Some(d) => {
                out.push((d[0] - b'0') * 64 + (d[1] - b'0') * 8 + (d[2] - b'0'));
                i += 4;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Interfaces que no se suman al tráfico: la de bucle local y las virtuales,
/// cuyo tráfico ya pasa por una interfaz real.
fn skip_interface(name: &str) -> bool {
    name == "lo"
        || ["veth", "docker", "br-", "virbr", "cni", "flannel", "cali"]
            .iter()
            .any(|p| name.starts_with(p))
}

impl Raw {
    fn line(&mut self, key: &str, value: &str) {
        let value = value.trim();
        let s = &mut self.stats;
        match key {
            "host" => s.host = value.to_string(),
            "kernel" => s.kernel = value.to_string(),
            "arch" => s.arch = value.to_string(),
            "os" => s.os = value.to_string(),
            "time" => s.time = value.to_string(),
            "ip" => {
                s.ips = value.split_whitespace().map(str::to_string).collect();
            }
            "uptime" => {
                self.uptime = value.parse().ok();
                s.uptime_secs = self.uptime.map(|u| u as u64);
            }
            "load" => {
                let v: Vec<f32> = value
                    .split_whitespace()
                    .take(3)
                    .filter_map(|p| p.parse().ok())
                    .collect();
                if let [a, b, c] = v[..] {
                    s.load = Some([a, b, c]);
                }
            }
            "cpu" => {
                // cpu  user nice system idle iowait irq softirq steal …
                let v: Vec<u64> = value
                    .split_whitespace()
                    .skip(1)
                    .take(8)
                    .filter_map(|p| p.parse().ok())
                    .collect();
                if v.len() >= 4 {
                    let idle = v[3] + v.get(4).copied().unwrap_or(0);
                    self.cpu = Some((v.iter().sum(), idle));
                }
            }
            "ncpu" => s.cores = value.parse().ok().filter(|n| *n > 0),
            "mem" => {
                if let Some((name, amount)) = value.split_once(':') {
                    let amount = amount.trim().parse().ok();
                    match name {
                        "MemTotal" => self.mem_total = amount,
                        "MemAvailable" => self.mem_available = amount,
                        "MemFree" => self.mem_free = amount,
                        "Buffers" => self.buffers = amount,
                        "Cached" => self.cached = amount,
                        "SwapTotal" => self.swap_total = amount,
                        "SwapFree" => self.swap_free = amount,
                        _ => {}
                    }
                }
            }
            "net" => {
                // «  eth0: rx_bytes … (8 campos) tx_bytes …»; las dos líneas
                // de cabecera no tienen esa forma.
                if let Some((name, rest)) = value.split_once(':') {
                    let v: Vec<u64> = rest
                        .split_whitespace()
                        .filter_map(|p| p.parse().ok())
                        .collect();
                    if v.len() >= 9 && !skip_interface(name.trim()) {
                        let (rx, tx) = self.net.unwrap_or((0, 0));
                        self.net = Some((rx + v[0], tx + v[8]));
                    }
                }
            }
            "disk" => {
                // Filesystem 1024-blocks Used Available Capacity Mounted on
                let f: Vec<&str> = value.split_whitespace().collect();
                if f.len() >= 6
                    && s.disks.len() < MAX_MOUNTS
                    && let (Ok(total), Ok(used), Ok(available)) =
                        (f[1].parse(), f[2].parse(), f[3].parse())
                {
                    let mount = f[5..].join(" ");
                    if !s.disks.iter().any(|d| d.mount == mount) {
                        s.disks.push(Disk {
                            mount,
                            filesystem: f[0].to_string(),
                            total,
                            used,
                            available,
                            kind: String::new(),
                            read_only: false,
                        });
                    }
                }
            }
            "mount" => {
                // «tipo opciones punto de montaje»
                let mut parts = value.splitn(3, ' ');
                if let (Some(kind), Some(options), Some(mount)) =
                    (parts.next(), parts.next(), parts.next())
                    && self.mounts.len() < MAX_MOUNTS
                {
                    let read_only = options.split(',').any(|o| o == "ro");
                    // Si hay varios en el mismo sitio, el que se ve es el último.
                    self.mounts
                        .insert(unescape_mount(mount), (kind.to_string(), read_only));
                }
            }
            "users" => s.users = value.parse().ok(),
            "procs" => s.processes = value.parse().ok().filter(|n| *n > 0),
            _ => {}
        }
    }

    /// Cierra la muestra: calcula lo que depende de la muestra anterior.
    fn finish(mut self, previous: Option<&(Raw, Instant)>, now: Instant) -> (Raw, Stats) {
        if let Some(total) = self.mem_total.filter(|t| *t > 0) {
            let available = self.mem_available.unwrap_or_else(|| {
                self.mem_free.unwrap_or(0) + self.buffers.unwrap_or(0) + self.cached.unwrap_or(0)
            });
            self.stats.memory = Some((total.saturating_sub(available), total));
        }
        if let Some(total) = self.swap_total.filter(|t| *t > 0) {
            self.stats.swap = Some((total.saturating_sub(self.swap_free.unwrap_or(0)), total));
        }
        for disk in &mut self.stats.disks {
            if let Some((kind, read_only)) = self.mounts.get(&disk.mount) {
                disk.kind = kind.clone();
                disk.read_only = *read_only;
            }
        }
        if let Some((prev, at)) = previous {
            if let (Some((total, idle)), Some((ptotal, pidle))) = (self.cpu, prev.cpu) {
                let dt = total.saturating_sub(ptotal);
                if dt > 0 {
                    let busy = dt.saturating_sub(idle.saturating_sub(pidle));
                    self.stats.cpu = Some((busy as f32 * 100.0 / dt as f32).clamp(0.0, 100.0));
                }
            }
            // El reloj del servidor es más fiel que el nuestro, que incluye
            // el retardo de la red.
            let elapsed = match (self.uptime, prev.uptime) {
                (Some(a), Some(b)) if a > b => a - b,
                _ => now.duration_since(*at).as_secs_f64(),
            };
            if let (Some((rx, tx)), Some((prx, ptx))) = (self.net, prev.net)
                && elapsed > 0.05
                && rx >= prx
                && tx >= ptx
            {
                self.stats.network =
                    Some(((rx - prx) as f64 / elapsed, (tx - ptx) as f64 / elapsed));
            }
        }
        let stats = self.stats.clone();
        (self, stats)
    }
}

/// Convierte las líneas que llegan del servidor en muestras.
#[derive(Default)]
pub struct Collector {
    current: Option<Raw>,
    previous: Option<(Raw, Instant)>,
}

impl Collector {
    /// Procesa una línea; devuelve una muestra cuando se completa.
    pub fn feed(&mut self, line: &str) -> Option<Stats> {
        self.feed_at(line, Instant::now())
    }

    fn feed_at(&mut self, line: &str, now: Instant) -> Option<Stats> {
        let line = line.trim_end_matches(['\r', '\n']);
        match line {
            "@@begin" => {
                self.current = Some(Raw::default());
                None
            }
            "@@end" => {
                let raw = self.current.take()?;
                let (raw, stats) = raw.finish(self.previous.as_ref(), now);
                self.previous = Some((raw, now));
                Some(stats)
            }
            _ => {
                // Lo que llegue fuera de un bloque (avisos del servidor,
                // mensajes del perfil) se ignora.
                if let (Some(raw), Some((key, value))) = (&mut self.current, line.split_once('=')) {
                    raw.line(key, value);
                }
                None
            }
        }
    }
}

enum Event {
    Stats(Box<Stats>),
    Failed(String),
}

/// Consulta en marcha sobre una conexión SSH. Al soltarla se detiene.
pub struct Monitor {
    events: std::sync::mpsc::Receiver<Event>,
    _stop: tokio::sync::oneshot::Sender<()>,
    probe: (u32, Vec<String>),
    /// Última muestra recibida.
    pub stats: Option<Stats>,
    /// Motivo por el que no hay datos, si la consulta falló.
    pub error: Option<String>,
}

impl Monitor {
    /// Recoge las muestras pendientes.
    pub fn poll(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Stats(stats) => {
                    self.stats = Some(*stats);
                    self.error = None;
                }
                Event::Failed(error) => self.error = Some(error),
            }
        }
    }

    /// `true` si las preferencias han cambiado de forma que hay que
    /// relanzar la consulta.
    pub fn is_stale(&self, config: &ServerBar) -> bool {
        self.probe != config.probe()
    }
}

/// Tope de datos sin un salto de línea: un servidor que no habla el
/// protocolo esperado no debe llenar la memoria.
const MAX_PENDING: usize = 256 * 1024;

/// La consulta ejecutándose en un canal propio de una conexión SSH: entrega
/// las muestras una a una, a quien las vaya pidiendo.
pub struct Sampler {
    channel: russh::Channel<russh::client::Msg>,
    collector: Collector,
    pending: Vec<u8>,
    samples: u64,
}

impl Sampler {
    /// Abre el canal y lanza en él la orden (la de [`command`] o la de
    /// [`probe_command`]).
    pub async fn start(handle: SshHandle, command: String) -> Result<Self> {
        let channel = handle
            .channel_open_session()
            .await
            .context("El servidor no dejó abrir otro canal")?;
        // El canal no necesita la referencia a la conexión: si el terminal se
        // cierra, esta consulta no debe mantenerla viva.
        drop(handle);
        channel
            .exec(true, command)
            .await
            .context("No se pudo lanzar la consulta")?;
        Ok(Self {
            channel,
            collector: Collector::default(),
            pending: vec![],
            samples: 0,
        })
    }

    /// Espera a la siguiente muestra. Da error si el servidor corta o no
    /// habla el protocolo esperado. Se puede abandonar la espera y volver a
    /// llamar: no se pierde nada de lo ya recibido.
    pub async fn next(&mut self) -> Result<Stats> {
        loop {
            while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.pending.drain(..=end).collect();
                if let Some(stats) = self.collector.feed(&String::from_utf8_lossy(&line)) {
                    self.samples += 1;
                    return Ok(stats);
                }
            }
            if self.pending.len() > MAX_PENDING {
                let _ = self.channel.close().await;
                bail!("El servidor respondió algo inesperado");
            }
            match self.channel.wait().await {
                Some(ChannelMsg::Data { data }) => self.pending.extend_from_slice(&data),
                Some(ChannelMsg::Failure) => bail!("El servidor no permite ejecutar órdenes"),
                Some(ChannelMsg::Close) | None => break,
                _ => {}
            }
        }
        if self.samples == 0 {
            bail!("El servidor no devolvió datos (hace falta una shell POSIX)");
        }
        bail!("La consulta se interrumpió")
    }

    /// Cierra el canal: la orden deja de ejecutarse en el servidor.
    pub async fn close(self) {
        let _ = self.channel.close().await;
    }
}

async fn run(
    handle: SshHandle,
    command: String,
    tx: &std::sync::mpsc::Sender<Event>,
    waker: &Waker,
    mut stop: tokio::sync::oneshot::Receiver<()>,
) -> Result<()> {
    let mut sampler = Sampler::start(handle, command).await?;
    loop {
        tokio::select! {
            _ = &mut stop => {
                sampler.close().await;
                return Ok(());
            }
            stats = sampler.next() => {
                if tx.send(Event::Stats(Box::new(stats?))).is_err() {
                    sampler.close().await;
                    return Ok(());
                }
                waker();
            }
        }
    }
}

/// Empieza a consultar el estado del servidor por la conexión indicada.
pub fn spawn(
    rt: &tokio::runtime::Handle,
    handle: SshHandle,
    config: &ServerBar,
    waker: Waker,
) -> Monitor {
    let (tx, rx) = std::sync::mpsc::channel();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let command = command(config.interval, &config.disks);
    rt.spawn(async move {
        if let Err(e) = run(handle, command, &tx, &waker, stop_rx).await {
            let _ = tx.send(Event::Failed(format!("{e:#}")));
            waker();
        }
    });
    Monitor {
        events: rx,
        _stop: stop_tx,
        probe: config.probe(),
        stats: None,
        error: None,
    }
}

/// Tamaño legible a partir de kB (base 1024).
pub fn format_kb(kb: u64) -> String {
    let mut value = kb as f64;
    let mut unit = "kB";
    for next in ["MB", "GB", "TB", "PB"] {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }
    if value >= 100.0 || unit == "kB" {
        format!("{value:.0} {unit}")
    } else {
        format!("{value:.1} {unit}").replace('.', ",")
    }
}

/// Velocidad legible a partir de bytes por segundo.
pub fn format_rate(bytes_per_sec: f64) -> String {
    let mut value = bytes_per_sec.max(0.0);
    let mut unit = "B/s";
    for next in ["kB/s", "MB/s", "GB/s"] {
        if value < 1000.0 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }
    if value >= 100.0 || unit == "B/s" {
        format!("{value:.0} {unit}")
    } else {
        format!("{value:.1} {unit}").replace('.', ",")
    }
}

/// Tiempo encendido: «3 d 4 h», «4 h 12 min», «12 min».
pub fn format_uptime(secs: u64) -> String {
    let (days, hours, minutes) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    if days > 0 {
        format!("{days} d {hours} h")
    } else if hours > 0 {
        format!("{hours} h {minutes} min")
    } else {
        format!("{minutes} min")
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const FIRST: &str = "\
bienvenido al servidor (esto lo imprime el perfil)
@@begin
host=web01
kernel=Linux 6.8.0-45-generic
arch=x86_64
os=Ubuntu 24.04.1 LTS
time=2026-11-27 16:20:01 UTC
uptime=1000.00
load=0.52 0.40 0.31 2/345 9999
cpu=cpu  1000 0 500 8000 500 0 0 0 0 0
ncpu=4
mem=MemTotal:16000000
mem=MemFree:1000000
mem=MemAvailable:12000000
mem=Buffers:500000
mem=Cached:3000000
mem=SwapTotal:2000000
mem=SwapFree:1500000
net=Inter-|   Receive                                                |  Transmit
net= face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
net=    lo: 9999999   100    0    0    0     0          0         0 9999999   100    0    0    0     0       0          0
net=  eth0: 1000000   900    0    0    0     0          0         0  500000   800    0    0    0     0       0          0
net=veth12: 7777777   900    0    0    0     0          0         0 7777777   800    0    0    0     0       0          0
disk=Filesystem     1024-blocks     Used Available Capacity Mounted on
disk=/dev/sda1        100000000 60000000  35000000      64% /
disk=/dev/sdb1         50000000 45000000   2500000      95% /mnt/datos copia
users=3
procs=212
ip=10.0.0.5 172.17.0.1
@@end
";

    fn feed(collector: &mut Collector, text: &str, at: Instant) -> Vec<Stats> {
        text.lines()
            .filter_map(|l| collector.feed_at(l, at))
            .collect()
    }

    #[test]
    fn samples_are_parsed_and_rates_derived() {
        let start = Instant::now();
        let mut c = Collector::default();
        let got = feed(&mut c, FIRST, start);
        assert_eq!(got.len(), 1);
        let s = &got[0];
        assert_eq!(s.host, "web01");
        assert_eq!(s.os, "Ubuntu 24.04.1 LTS");
        assert_eq!(s.kernel, "Linux 6.8.0-45-generic");
        assert_eq!(s.ips, vec!["10.0.0.5", "172.17.0.1"]);
        assert_eq!(s.uptime_secs, Some(1000));
        assert_eq!(s.load, Some([0.52, 0.40, 0.31]));
        assert_eq!(s.cores, Some(4));
        assert_eq!(s.memory, Some((4_000_000, 16_000_000)));
        assert_eq!(s.swap, Some((500_000, 2_000_000)));
        assert_eq!((s.users, s.processes), (Some(3), Some(212)));
        assert_eq!(s.disks.len(), 2);
        assert_eq!(s.disks[0].mount, "/");
        assert_eq!(s.disks[0].available, 35_000_000);
        assert_eq!(s.disks[1].mount, "/mnt/datos copia");
        assert!((s.disks[1].used_percent() - 94.7).abs() < 0.1);
        // La primera muestra no tiene con qué compararse.
        assert_eq!((s.cpu, s.network), (None, None));

        // Segunda muestra, 5 s después según el reloj del servidor: 2000
        // tics de CPU, de ellos 1500 inactivos; 1 MB recibido y 512 kB
        // enviados por eth0 (lo y veth no cuentan).
        let second = FIRST
            .replace("uptime=1000.00", "uptime=1005.00")
            .replace(
                "cpu=cpu  1000 0 500 8000 500",
                "cpu=cpu  1400 0 600 9400 600",
            )
            .replace("eth0: 1000000", "eth0: 2048576")
            .replace("0  500000   800", "0 1024288   800");
        let got = feed(&mut c, &second, start + Duration::from_secs(60));
        let s = &got[0];
        assert_eq!(s.cpu, Some(25.0));
        let (rx, tx) = s.network.unwrap();
        assert!((rx - 209_715.2).abs() < 0.1, "{rx}");
        assert!((tx - 104_857.6).abs() < 0.1, "{tx}");

        // Un contador que retrocede (interfaz reiniciada) no da una
        // velocidad absurda.
        let third = second.replace("eth0: 2048576", "eth0: 5");
        let got = feed(&mut c, &third, start + Duration::from_secs(120));
        assert_eq!(got[0].network, None);
    }

    #[test]
    fn partial_systems_degrade_gracefully() {
        // Un sistema sin /proc (BSD, macOS) solo da lo básico.
        let mut c = Collector::default();
        let text = "@@begin\nhost=mac\nkernel=Darwin 24.1.0\nncpu=8\nusers=1\nprocs=0\n\
                    disk=/dev/disk3s1 488245288 230000000 250000000 48% /\nip=\n@@end\n";
        let got = feed(&mut c, text, Instant::now());
        let s = &got[0];
        assert_eq!(s.host, "mac");
        assert_eq!(
            (s.memory, s.cpu, s.load, s.uptime_secs),
            (None, None, None, None)
        );
        assert_eq!(s.processes, None);
        assert!(s.ips.is_empty() && s.disks.len() == 1);
        // Un bloque sin cerrar no produce nada, y `@@end` suelto tampoco.
        let mut c = Collector::default();
        assert!(c.feed("@@end").is_none());
        assert!(c.feed("@@begin").is_none());
        assert!(c.feed("host=a").is_none());
        assert!(c.feed("@@begin").is_none());
        assert_eq!(c.feed("@@end\r\n").unwrap().host, "");
    }

    #[test]
    fn command_is_safe_for_any_login_shell() {
        let cmd = command(7, &["/".into(), "/var/lib".into(), "/x; rm -rf ~".into()]);
        assert!(cmd.starts_with("exec sh -c '") && cmd.ends_with('\''));
        let body = &cmd["exec sh -c '".len()..cmd.len() - 1];
        // Ni comillas simples ni barras invertidas ni saltos de línea.
        assert!(!body.contains(['\'', '\\', '\n']), "{body}");
        assert!(body.contains("df -Pk / /var/lib 2>/dev/null"), "{body}");
        assert!(!body.contains("rm -rf"));
        assert!(body.contains("sleep $s; s=7; done"));
        // Sin rutas válidas se consulta la raíz.
        assert!(command(0, &["malo".into()]).contains("df -Pk / 2>/dev/null"));
        assert!(command(0, &[]).contains("s=1; done"));
    }

    #[test]
    fn the_command_really_runs_in_a_posix_shell() {
        // La misma orden que se manda al servidor, ejecutada aquí.
        if !std::path::Path::new("/bin/sh").exists() {
            return;
        }
        let cmd = command(1, &["/".into()]).replace("while :;", "for once in 1;");
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&cmd)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mut c = Collector::default();
        let text = String::from_utf8_lossy(&out.stdout);
        let got: Vec<Stats> = text.lines().filter_map(|l| c.feed(l)).collect();
        assert_eq!(got.len(), 1, "{text}");
        assert!(!got[0].host.is_empty(), "{text}");
        assert!(!got[0].kernel.is_empty(), "{text}");
        if std::path::Path::new("/proc/meminfo").exists() {
            assert!(got[0].memory.is_some(), "{text}");
            assert!(got[0].disks.iter().any(|d| d.mount == "/"), "{text}");
        }
    }

    #[test]
    fn light_probe_asks_less_and_tells_filesystem_types() {
        let cmd = probe_command(9, Probe::Light);
        let body = &cmd["exec sh -c '".len()..cmd.len() - 1];
        assert!(!body.contains(['\'', '\\', '\n']), "{body}");
        // Sin los programas que solo interesan a la barra.
        for word in ["hostname", "uname", "who", "ls -d", "os-release", "date"] {
            assert!(!body.contains(word), "{word}: {body}");
        }
        assert!(body.contains("df -Pk 2>/dev/null"), "{body}");
        assert!(body.contains("sleep $s; s=9; done"));
        // La de la barra no cambia: es la misma de siempre.
        assert_eq!(
            probe_command(7, Probe::Full(&["/".into()])),
            command(7, &["/".into()])
        );
        assert!(!command(7, &[]).contains("/proc/mounts"));

        // Los tipos llegan aparte y se casan con los discos por su punto de
        // montaje, lleve espacios escritos tal cual o al modo de /proc.
        let text = "@@begin\n\
                    mount=ext4 rw,relatime /\n\
                    mount=squashfs ro,nodev,relatime /snap/core/1\n\
                    mount=xfs rw /mnt/datos\\040copia\n\
                    mount=tmpfs rw /mnt/otro sitio\n\
                    mount=ext4 rw,errors=remount-ro /srv\n\
                    disk=Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                    disk=/dev/sda1 100 60 40 60% /\n\
                    disk=/dev/loop0 50 50 0 100% /snap/core/1\n\
                    disk=/dev/sdb1 100 10 90 10% /mnt/datos copia\n\
                    disk=tmpfs 100 0 100 0% /mnt/otro sitio\n\
                    disk=/dev/sdc1 100 10 90 10% /srv\n\
                    disk=/dev/sdd1 100 10 90 10% /sin/tipo\n\
                    @@end\n";
        let mut c = Collector::default();
        let got = feed(&mut c, text, Instant::now());
        let kinds: Vec<(&str, &str, bool)> = got[0]
            .disks
            .iter()
            .map(|d| (d.mount.as_str(), d.kind.as_str(), d.read_only))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("/", "ext4", false),
                ("/snap/core/1", "squashfs", true),
                ("/mnt/datos copia", "xfs", false),
                ("/mnt/otro sitio", "tmpfs", false),
                ("/srv", "ext4", false),
                ("/sin/tipo", "", false),
            ]
        );
        assert_eq!(unescape_mount("a\\134b\\011c\\04"), "a\\b\tc\\04");
    }

    #[test]
    fn the_light_probe_really_runs_in_a_posix_shell() {
        if !std::path::Path::new("/bin/sh").exists() {
            return;
        }
        let cmd = probe_command(1, Probe::Light).replace("while :;", "for once in 1;");
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&cmd)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mut c = Collector::default();
        let text = String::from_utf8_lossy(&out.stdout);
        let got: Vec<Stats> = text.lines().filter_map(|l| c.feed(l)).collect();
        assert_eq!(got.len(), 1, "{text}");
        if std::path::Path::new("/proc/mounts").exists() {
            assert!(got[0].memory.is_some() && got[0].load.is_some(), "{text}");
            let root = got[0].disks.iter().find(|d| d.mount == "/");
            assert!(root.is_some_and(|d| !d.kind.is_empty()), "{text}");
        }
    }

    #[test]
    fn settings_are_validated() {
        let mut bar: ServerBar = serde_json::from_str(
            r#"{"intervalo":0,"elementos":["cpu","inventado","cpu","red"],
                "discos":["/","$(reboot)","/srv/datos"],"aviso":95,"critico":50}"#,
        )
        .unwrap();
        bar.sanitize();
        assert_eq!(bar.interval, 1);
        assert_eq!(bar.items, vec![BarItem::Cpu, BarItem::Network]);
        assert_eq!(bar.disks, vec!["/", "/srv/datos"]);
        assert_eq!((bar.warn, bar.critical), (95.0, 95.0));
        assert!(bar.enabled && bar.gauges);
        // Se guarda con nombres legibles.
        let json = serde_json::to_string(&bar).unwrap();
        assert!(json.contains(r#""elementos":["cpu","red"]"#), "{json}");
        assert_eq!(serde_json::from_str::<ServerBar>(&json).unwrap(), bar);

        let (good, bad) = parse_disks("/, /home;/var/log  `id` /home");
        assert_eq!(good, vec!["/", "/home", "/var/log"]);
        assert_eq!(bad, vec!["`id`"]);
        for item in BarItem::ALL {
            assert_eq!(BarItem::from_key(item.key()), Some(item));
        }
    }

    #[test]
    fn readable_units() {
        assert_eq!(format_kb(512), "512 kB");
        assert_eq!(format_kb(16_000_000), "15,3 GB");
        assert_eq!(format_kb(350 * 1024 * 1024), "350 GB");
        assert_eq!(format_rate(0.0), "0 B/s");
        assert_eq!(format_rate(209_715.2), "205 kB/s");
        assert_eq!(format_rate(3.5 * 1024.0 * 1024.0), "3,5 MB/s");
        assert_eq!(format_uptime(59), "0 min");
        assert_eq!(format_uptime(4 * 3600 + 12 * 60), "4 h 12 min");
        assert_eq!(format_uptime(3 * 86_400 + 4 * 3600 + 5), "3 d 4 h");
    }
}
