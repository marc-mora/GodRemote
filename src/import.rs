//! Importación de equipos desde otras herramientas: el `config` de OpenSSH,
//! las sesiones de PuTTY (exportadas a un `.reg`) y las de MobaXterm
//! (`.mxtsessions` o `MobaXterm.ini`).
//!
//! Solo se leen los datos de conexión (nombre, host, puerto, usuario, clave
//! y saltos). Las contraseñas no se importan: ninguna de estas herramientas
//! las guarda de forma que se puedan leer.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use uuid::Uuid;

use crate::model::{AuthMethod, Folder, FolderDefaults, Node, Protocol, Session};

/// De dónde se importa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    OpenSsh,
    Putty,
    MobaXterm,
}

impl Source {
    pub const ALL: [Source; 3] = [Source::OpenSsh, Source::Putty, Source::MobaXterm];

    pub fn label(self) -> &'static str {
        match self {
            Source::OpenSsh => "OpenSSH (~/.ssh/config)",
            Source::Putty => "PuTTY (fichero .reg)",
            Source::MobaXterm => "MobaXterm (.mxtsessions o MobaXterm.ini)",
        }
    }

    /// Nombre de la carpeta donde quedan los equipos importados.
    pub fn folder_name(self) -> &'static str {
        match self {
            Source::OpenSsh => "Importado de OpenSSH",
            Source::Putty => "Importado de PuTTY",
            Source::MobaXterm => "Importado de MobaXterm",
        }
    }

    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Source::OpenSsh => &[],
            Source::Putty => &["reg"],
            Source::MobaXterm => &["mxtsessions", "ini"],
        }
    }
}

/// Resultado de una importación.
#[derive(Debug, Default)]
pub struct Imported {
    /// Equipos y carpetas, listos para colgar del árbol.
    pub nodes: Vec<Node>,
    /// Cosas que no se han podido importar o conviene revisar.
    pub warnings: Vec<String>,
}

impl Imported {
    /// Número de equipos importados.
    pub fn count(&self) -> usize {
        fn count(nodes: &[Node]) -> usize {
            nodes
                .iter()
                .map(|n| match n {
                    Node::Session(_) => 1,
                    Node::Folder(f) => count(&f.children),
                })
                .sum()
        }
        count(&self.nodes)
    }

    /// Todo lo importado dentro de una carpeta con el nombre dado.
    pub fn into_folder(self, name: &str) -> Folder {
        Folder {
            id: Uuid::new_v4(),
            name: name.to_string(),
            defaults: FolderDefaults::default(),
            children: self.nodes,
        }
    }
}

/// Lee un fichero de texto en UTF-8, UTF-16 (como exporta el registro de
/// Windows) o, si no, tomando cada byte como un carácter.
fn read_text(path: &Path) -> Result<String> {
    let meta =
        std::fs::metadata(path).with_context(|| format!("No se pudo leer {}", path.display()))?;
    if !meta.is_file() {
        bail!("{} no es un fichero", path.display());
    }
    if meta.len() > 32 * 1024 * 1024 {
        bail!("{} es demasiado grande", path.display());
    }
    let bytes =
        std::fs::read(path).with_context(|| format!("No se pudo leer {}", path.display()))?;
    Ok(decode_text(&bytes))
}

fn decode_text(bytes: &[u8]) -> String {
    let utf16 = |data: &[u8], big_endian: bool| {
        let units: Vec<u16> = data
            .chunks_exact(2)
            .map(|c| {
                if big_endian {
                    u16::from_be_bytes([c[0], c[1]])
                } else {
                    u16::from_le_bytes([c[0], c[1]])
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xff, 0xfe, rest @ ..] => utf16(rest, false),
        [0xfe, 0xff, rest @ ..] => utf16(rest, true),
        [0xef, 0xbb, 0xbf, rest @ ..] => String::from_utf8_lossy(rest).to_string(),
        _ => match std::str::from_utf8(bytes) {
            Ok(text) => text.to_string(),
            // Ficheros antiguos en la página de códigos de Windows.
            Err(_) => bytes.iter().map(|b| *b as char).collect(),
        },
    }
}

/// Importa del fichero indicado según su origen.
pub fn import_file(source: Source, path: &Path) -> Result<Imported> {
    let text = read_text(path)?;
    let imported = match source {
        Source::OpenSsh => openssh(&text, default_key().as_deref()),
        Source::Putty => putty(&text),
        Source::MobaXterm => mobaxterm(&text),
    };
    if imported.count() == 0 {
        bail!(
            "No se ha encontrado ningún equipo en {} (¿es un fichero de {}?)",
            path.display(),
            source.label()
        );
    }
    Ok(imported)
}

/// Clave privada que OpenSSH usaría por defecto, si existe.
fn default_key() -> Option<String> {
    ["id_ed25519", "id_ecdsa", "id_rsa"]
        .iter()
        .find(|name| crate::paths::home_dir().join(".ssh").join(name).is_file())
        .map(|name| format!("~/.ssh/{name}"))
}

fn ssh_session(name: &str, host: &str, port: u16, user: &str, key: &str) -> Session {
    let key = key.trim();
    Session {
        name: name.trim().to_string(),
        protocol: Protocol::Ssh,
        host: host.trim().to_string(),
        port,
        username: user.trim().to_string(),
        auth: if key.is_empty() {
            AuthMethod::Password
        } else {
            AuthMethod::Key
        },
        key_path: key.to_string(),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// OpenSSH
// ---------------------------------------------------------------------------

/// Opciones de un bloque `Host` que interesan.
#[derive(Debug, Clone, Default)]
struct SshOptions {
    host_name: Option<String>,
    user: Option<String>,
    port: Option<u16>,
    identity: Option<String>,
    proxy_jump: Option<String>,
}

impl SshOptions {
    fn set(&mut self, keyword: &str, value: &str) {
        // En ssh_config manda la primera aparición de cada opción.
        match keyword {
            "hostname" if self.host_name.is_none() => self.host_name = Some(value.to_string()),
            "user" if self.user.is_none() => self.user = Some(value.to_string()),
            "port" if self.port.is_none() => self.port = value.parse().ok(),
            "identityfile" if self.identity.is_none() => self.identity = Some(value.to_string()),
            "proxyjump" if self.proxy_jump.is_none() => self.proxy_jump = Some(value.to_string()),
            _ => {}
        }
    }

    /// Completa lo que falte con los valores generales (`Host *`).
    fn or(mut self, defaults: &SshOptions) -> Self {
        self.user = self.user.or_else(|| defaults.user.clone());
        self.port = self.port.or(defaults.port);
        self.identity = self.identity.or_else(|| defaults.identity.clone());
        self.proxy_jump = self.proxy_jump.or_else(|| defaults.proxy_jump.clone());
        self
    }
}

/// Separa una línea de ssh_config en palabra clave y valor.
fn ssh_line(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let split = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let keyword = line[..split].to_lowercase();
    let value = line[split..]
        .trim_start_matches(|c: char| c.is_whitespace() || c == '=')
        .trim();
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    (!value.is_empty()).then(|| (keyword, value.to_string()))
}

/// `usuario@host:puerto` de un salto que no es un alias del fichero.
fn jump_spec(spec: &str) -> (String, String, u16) {
    let spec = spec.trim().trim_start_matches("ssh://");
    let (user, rest) = match spec.rsplit_once('@') {
        Some((user, rest)) => (user.to_string(), rest),
        None => (String::new(), spec),
    };
    match rest.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') && port.parse::<u16>().is_ok() => {
            (user, host.to_string(), port.parse().unwrap_or(22))
        }
        _ => (user, rest.to_string(), 22),
    }
}

/// Equipos de un `config` de OpenSSH. `default_key` es la clave que se
/// asigna a los que no indican `IdentityFile`.
pub fn openssh(text: &str, default_key: Option<&str>) -> Imported {
    let mut out = Imported::default();
    let mut defaults = SshOptions::default();
    // (alias, opciones) en el orden del fichero.
    let mut blocks: Vec<(Vec<String>, SshOptions)> = vec![];
    // Bloque en curso: None antes del primer `Host` (opciones generales).
    let mut current: Option<(Vec<String>, SshOptions)> = None;
    let mut skipping = false;
    for line in text.lines() {
        let Some((keyword, value)) = ssh_line(line) else {
            continue;
        };
        match keyword.as_str() {
            "host" => {
                blocks.extend(current.take());
                skipping = false;
                let patterns: Vec<String> = value.split_whitespace().map(str::to_string).collect();
                current = Some((patterns, SshOptions::default()));
            }
            "match" => {
                blocks.extend(current.take());
                skipping = true;
                out.warnings
                    .push("Los bloques «Match» no se importan".to_string());
            }
            "include" => out.warnings.push(format!(
                "No se han seguido los ficheros incluidos con «Include {value}»"
            )),
            _ if skipping => {}
            _ => match &mut current {
                Some((_, options)) => options.set(&keyword, &value),
                None => defaults.set(&keyword, &value),
            },
        }
    }
    blocks.extend(current.take());

    // Los bloques con comodines dan valores generales, no equipos.
    let is_pattern = |p: &str| p.contains(['*', '?', '!']);
    for (patterns, options) in &blocks {
        if patterns.iter().any(|p| p == "*") {
            for (keyword, value) in [
                ("user", options.user.clone()),
                ("port", options.port.map(|p| p.to_string())),
                ("identityfile", options.identity.clone()),
                ("proxyjump", options.proxy_jump.clone()),
            ] {
                if let Some(value) = value {
                    defaults.set(keyword, &value);
                }
            }
        }
    }

    let mut sessions: Vec<Session> = vec![];
    let mut jumps_of: BTreeMap<Uuid, String> = BTreeMap::new();
    for (patterns, options) in blocks {
        let options = options.or(&defaults);
        for alias in patterns.iter().filter(|p| !is_pattern(p)) {
            if sessions.iter().any(|s| s.name == *alias) {
                continue;
            }
            let host = options
                .host_name
                .clone()
                .unwrap_or_else(|| alias.clone())
                .replace("%h", alias);
            let key = options
                .identity
                .clone()
                .or_else(|| default_key.map(str::to_string))
                .unwrap_or_default();
            let session = ssh_session(
                alias,
                &host,
                options.port.unwrap_or(22),
                options.user.as_deref().unwrap_or_default(),
                &key,
            );
            if let Some(jump) = options
                .proxy_jump
                .as_ref()
                .filter(|j| !j.eq_ignore_ascii_case("none"))
            {
                jumps_of.insert(session.id, jump.clone());
            }
            sessions.push(session);
        }
    }

    // Los saltos se enlazan con los equipos importados; los que no son un
    // alias del fichero se crean como equipos nuevos.
    let mut extra: Vec<Session> = vec![];
    for (id, list) in &jumps_of {
        let mut jumps = vec![];
        for spec in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let known = sessions
                .iter()
                .chain(&extra)
                .find(|s| s.name == spec)
                .map(|s| s.id);
            let hop = match known {
                Some(hop) => hop,
                None => {
                    let (user, host, port) = jump_spec(spec);
                    let hop = ssh_session(spec, &host, port, &user, default_key.unwrap_or(""));
                    let id = hop.id;
                    extra.push(hop);
                    id
                }
            };
            if hop != *id && !jumps.contains(&hop) {
                jumps.push(hop);
            }
        }
        if let Some(session) = sessions.iter_mut().find(|s| s.id == *id) {
            session.jumps = jumps;
        }
    }
    for session in &sessions {
        if session.username.is_empty() {
            out.warnings.push(format!(
                "«{}» no indica usuario: complétalo antes de conectar",
                session.name
            ));
        }
    }
    out.nodes = sessions
        .into_iter()
        .chain(extra)
        .map(Node::Session)
        .collect();
    out
}

// ---------------------------------------------------------------------------
// PuTTY
// ---------------------------------------------------------------------------

/// Valor de una línea `"Nombre"=…` de un fichero `.reg`.
enum RegValue {
    Text(String),
    Number(u32),
}

fn reg_line(line: &str) -> Option<(String, RegValue)> {
    let line = line.trim();
    let rest = line.strip_prefix('"')?;
    let end = rest.find("\"=")?;
    let name = rest[..end].to_string();
    let value = &rest[end + 2..];
    if let Some(hex) = value.strip_prefix("dword:") {
        return Some((
            name,
            RegValue::Number(u32::from_str_radix(hex.trim(), 16).ok()?),
        ));
    }
    let text = value.strip_prefix('"')?.strip_suffix('"')?;
    // En un .reg, la barra invertida y las comillas van escapadas.
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            out.extend(chars.next());
        } else {
            out.push(c);
        }
    }
    Some((name, RegValue::Text(out)))
}

/// Deshace la codificación `%XX` de los nombres de sesión de PuTTY.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = text.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    // PuTTY guarda los nombres en la página de códigos del sistema.
    match String::from_utf8(out) {
        Ok(text) => text,
        Err(e) => e.into_bytes().iter().map(|b| *b as char).collect(),
    }
}

/// Sesiones de PuTTY a partir de la exportación del registro
/// (`HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions`).
pub fn putty(text: &str) -> Imported {
    let mut out = Imported::default();
    let mut current: Option<(String, BTreeMap<String, RegValue>)> = None;
    let mut sections = vec![];
    for line in text.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            sections.extend(current.take());
            let lower = header.to_lowercase();
            if let Some(at) = lower.find("\\putty\\sessions\\") {
                let name = &header[at + "\\putty\\sessions\\".len()..];
                current = Some((percent_decode(name), BTreeMap::new()));
            }
        } else if let (Some((_, values)), Some((name, value))) = (&mut current, reg_line(line)) {
            values.insert(name, value);
        }
    }
    sections.extend(current.take());

    for (name, values) in sections {
        let text = |key: &str| match values.get(key) {
            Some(RegValue::Text(t)) => t.trim().to_string(),
            _ => String::new(),
        };
        let number = |key: &str| match values.get(key) {
            Some(RegValue::Number(n)) => Some(*n),
            _ => None,
        };
        let protocol = text("Protocol").to_lowercase();
        if protocol == "serial" {
            let mut session = Session {
                name: name.clone(),
                protocol: Protocol::Serial,
                ..Default::default()
            };
            session.serial.device = text("SerialLine");
            if let Some(speed) = number("SerialSpeed") {
                session.serial.baud = speed;
            }
            if !session.serial.device.is_empty() {
                out.nodes.push(Node::Session(session));
            }
            continue;
        }
        // «Default Settings» y las sesiones sin host no son equipos.
        let host_field = text("HostName");
        if host_field.is_empty() {
            continue;
        }
        // PuTTY admite «usuario@host» en el campo del host.
        let (user_in_host, host) = match host_field.rsplit_once('@') {
            Some((user, host)) => (user.to_string(), host.to_string()),
            None => (String::new(), host_field.clone()),
        };
        let user = if text("UserName").is_empty() {
            user_in_host
        } else {
            text("UserName")
        };
        let mut session = match protocol.as_str() {
            "ssh" | "" => ssh_session(&name, &host, 22, &user, &text("PublicKeyFile")),
            "telnet" => Session {
                name: name.clone(),
                protocol: Protocol::Telnet,
                host: host.clone(),
                port: 23,
                ..Default::default()
            },
            other => {
                out.warnings.push(format!(
                    "«{name}» usa el protocolo «{other}», que GodRemote no tiene"
                ));
                continue;
            }
        };
        if let Some(port) = number("PortNumber").and_then(|p| u16::try_from(p).ok())
            && port != 0
        {
            session.port = port;
        }
        if number("ProxyMethod").unwrap_or(0) != 0 && !text("ProxyHost").is_empty() {
            out.warnings.push(format!(
                "«{name}» pasa por el proxy {}: configúralo como salto si es SSH",
                text("ProxyHost")
            ));
        }
        out.nodes.push(Node::Session(session));
    }
    out
}

// ---------------------------------------------------------------------------
// MobaXterm
// ---------------------------------------------------------------------------

/// Deshace los caracteres que MobaXterm escribe con nombre en sus sesiones.
fn moba_unescape(text: &str) -> String {
    text.replace("__PTVIRG__", ";")
        .replace("__DBLQUO__", "\"")
        .replace("__PIPE__", "|")
        .replace("__PERCENT__", "%")
        .replace("__DIEZE__", "#")
        .replace("_ProfileDir_", "%USERPROFILE%")
        .replace("_MyDocuments_", "%USERPROFILE%\\Documents")
}

/// Inserta un nodo en la ruta de carpetas indicada, creándolas si faltan.
fn insert_at(nodes: &mut Vec<Node>, path: &[&str], node: Node) {
    let Some((first, rest)) = path.split_first() else {
        nodes.push(node);
        return;
    };
    let index = nodes
        .iter()
        .position(|n| matches!(n, Node::Folder(f) if f.name == *first));
    let index = index.unwrap_or_else(|| {
        nodes.push(Node::Folder(Folder {
            id: Uuid::new_v4(),
            name: first.to_string(),
            defaults: FolderDefaults::default(),
            children: vec![],
        }));
        nodes.len() - 1
    });
    if let Node::Folder(folder) = &mut nodes[index] {
        insert_at(&mut folder.children, rest, node);
    }
}

/// Sesiones de MobaXterm: las secciones `[Bookmarks]`, `[Bookmarks_1]`… de
/// un `.mxtsessions` o de `MobaXterm.ini`. Cada una es una carpeta
/// (`SubRep=Carpeta\Subcarpeta`) y cada línea una sesión:
/// `Nombre=#icono#tipo%host%puerto%usuario%…`.
pub fn mobaxterm(text: &str) -> Imported {
    let mut out = Imported::default();
    let mut in_bookmarks = false;
    let mut folder: Vec<String> = vec![];
    for line in text.lines() {
        let line = line.trim();
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_bookmarks = section.to_lowercase().starts_with("bookmarks");
            folder.clear();
            continue;
        }
        if !in_bookmarks {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name == "SubRep" {
            folder = value
                .split('\\')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect();
            continue;
        }
        if name == "ImgNum" {
            continue;
        }
        // #icono#tipo%campo1%campo2…  (después de un segundo # van los
        // ajustes del terminal, que no interesan).
        let Some(rest) = value.strip_prefix('#') else {
            continue;
        };
        let Some((_, rest)) = rest.split_once('#') else {
            continue;
        };
        let rest = rest.split('#').next().unwrap_or_default();
        let fields: Vec<String> = rest.split('%').map(moba_unescape).collect();
        let field = |i: usize| fields.get(i).map(|f| f.trim()).unwrap_or_default();
        let port = |default: u16| {
            field(2)
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .unwrap_or(default)
        };
        let name = moba_unescape(name.trim());
        let host = field(1).to_string();
        let network = |protocol: Protocol, default_port: u16, user: &str| Session {
            name: name.clone(),
            protocol,
            host: host.clone(),
            port: port(default_port),
            username: user.to_string(),
            ..Default::default()
        };
        let session = match field(0) {
            // SSH: host, puerto, usuario… y la clave privada en el campo 14.
            "0" if !host.is_empty() => {
                let key = field(14);
                let key = if key.contains(['\\', '/']) { key } else { "" };
                let mut session = ssh_session(&name, &host, port(22), field(3), key);
                // Pasarela SSH (campos 8, 9 y 10): se anota para revisarla.
                if !field(8).is_empty() {
                    out.warnings.push(format!(
                        "«{name}» pasa por la pasarela SSH {}: añádela como salto",
                        field(8)
                    ));
                }
                session.x11_forwarding = false;
                Some(session)
            }
            "1" if !host.is_empty() => Some(network(Protocol::Telnet, 23, "")),
            "4" if !host.is_empty() => Some(network(Protocol::Rdp, 3389, field(3))),
            "5" if !host.is_empty() => Some(network(Protocol::Vnc, 5900, "")),
            "7" if !host.is_empty() => {
                let mut session = ssh_session(&name, &host, port(22), field(3), "");
                session.protocol = Protocol::Sftp;
                Some(session)
            }
            kind => {
                let what = match kind {
                    "2" => "Rsh",
                    "3" => "XDMCP",
                    "6" => "FTP",
                    "8" => "puerto serie",
                    "9" => "explorador de archivos",
                    "10" => "shell local",
                    "11" => "navegador",
                    "12" => "Mosh",
                    "13" => "AWS S3",
                    "14" => "WSL",
                    _ => "un tipo desconocido",
                };
                out.warnings
                    .push(format!("«{name}» es de tipo {what}: no se ha importado"));
                None
            }
        };
        if let Some(session) = session {
            let path: Vec<&str> = folder.iter().map(String::as_str).collect();
            insert_at(&mut out.nodes, &path, Node::Session(session));
        }
    }
    out
}

/// Exporta a un fichero `.reg` las sesiones de PuTTY guardadas en este
/// equipo (solo en Windows) y las importa.
pub fn putty_from_registry() -> Result<Imported> {
    if !cfg!(windows) {
        bail!("Las sesiones de PuTTY solo se pueden leer directamente en Windows");
    }
    let file = std::env::temp_dir().join(format!("godremote-putty-{}.reg", Uuid::new_v4()));
    let status = std::process::Command::new("reg")
        .args(["export", r"HKCU\Software\SimonTatham\PuTTY\Sessions"])
        .arg(&file)
        .arg("/y")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("No se pudo ejecutar «reg export»")?;
    if !status.success() {
        bail!("No hay sesiones de PuTTY guardadas en este equipo");
    }
    let result = import_file(Source::Putty, &file);
    let _ = std::fs::remove_file(&file);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sessions(nodes: &[Node]) -> Vec<&Session> {
        let mut out = vec![];
        for node in nodes {
            match node {
                Node::Session(s) => out.push(s),
                Node::Folder(f) => out.extend(sessions(&f.children)),
            }
        }
        out
    }

    #[test]
    fn openssh_config() {
        let text = r#"
# Valores generales
Host *
    User admin
    IdentityFile ~/.ssh/id_general

Host bastion
    HostName bastion.ejemplo.net
    Port 2222
    User salto
    IdentityFile ~/.ssh/bastion

Host web-01 web-02
    HostName %h.interno
    ProxyJump bastion

Host db
    HostName=10.0.0.9
    User "postgres"
    ProxyJump bastion,otro@externo.ejemplo.net:2200

Host *.lab !malo
    User nadie

Match host x
    User fantasma

Include conf.d/*
"#;
        let imported = openssh(text, Some("~/.ssh/id_ed25519"));
        let all = sessions(&imported.nodes);
        let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "bastion",
                "web-01",
                "web-02",
                "db",
                "otro@externo.ejemplo.net:2200"
            ]
        );
        let get = |name: &str| *all.iter().find(|s| s.name == name).unwrap();
        let bastion = get("bastion");
        assert_eq!(
            (
                bastion.host.as_str(),
                bastion.port,
                bastion.username.as_str()
            ),
            ("bastion.ejemplo.net", 2222, "salto")
        );
        assert_eq!(
            (bastion.auth, bastion.key_path.as_str()),
            (AuthMethod::Key, "~/.ssh/bastion")
        );
        // Varios alias en un bloque, %h, y los valores generales de «Host *».
        let web = get("web-02");
        assert_eq!((web.host.as_str(), web.port), ("web-02.interno", 22));
        assert_eq!(
            (web.username.as_str(), web.key_path.as_str()),
            ("admin", "~/.ssh/id_general")
        );
        assert_eq!(web.jumps, vec![bastion.id]);
        // Un salto que no es un alias se crea como equipo.
        let db = get("db");
        let extra = get("otro@externo.ejemplo.net:2200");
        assert_eq!(db.username, "postgres");
        assert_eq!(db.jumps, vec![bastion.id, extra.id]);
        assert_eq!(
            (extra.username.as_str(), extra.host.as_str(), extra.port),
            ("otro", "externo.ejemplo.net", 2200)
        );
        // Comodines, Match e Include no dan equipos, pero se avisa.
        assert!(imported.warnings.iter().any(|w| w.contains("Match")));
        assert!(
            imported
                .warnings
                .iter()
                .any(|w| w.contains("Include conf.d/*"))
        );

        // Sin IdentityFile ni clave por defecto: contraseña; sin usuario, aviso.
        let plain = openssh("Host solo\n  HostName 1.2.3.4\n", None);
        let s = sessions(&plain.nodes)[0];
        assert_eq!((s.auth, s.username.as_str()), (AuthMethod::Password, ""));
        assert!(plain.warnings[0].contains("no indica usuario"));
        assert_eq!(openssh("# vacío\n", None).count(), 0);
    }

    #[test]
    fn putty_registry_export() {
        let text = r#"Windows Registry Editor Version 5.00

[HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions]

[HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions\Default%20Settings]
"HostName"=""
"Protocol"="ssh"

[HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions\Servidor%20web%20(prod)]
"HostName"="web.ejemplo.net"
"PortNumber"=dword:00000898
"Protocol"="ssh"
"UserName"="deploy"
"PublicKeyFile"="C:\\Users\\yo\\claves\\web.ppk"

[HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions\router]
"HostName"="admin@192.168.1.1"
"PortNumber"=dword:00000017
"Protocol"="telnet"

[HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions\con-proxy]
"HostName"="root@10.1.1.1"
"Protocol"="ssh"
"ProxyMethod"=dword:00000002
"ProxyHost"="proxy.ejemplo.net"

[HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions\consola]
"Protocol"="serial"
"SerialLine"="COM3"
"SerialSpeed"=dword:0001c200

[HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions\viejo]
"HostName"="10.9.9.9"
"Protocol"="rlogin"
"#;
        let imported = putty(text);
        let all = sessions(&imported.nodes);
        assert_eq!(all.len(), 4);
        let web = all
            .iter()
            .find(|s| s.name == "Servidor web (prod)")
            .unwrap();
        assert_eq!((web.host.as_str(), web.port), ("web.ejemplo.net", 2200));
        assert_eq!(web.username, "deploy");
        assert_eq!(
            (web.auth, web.key_path.as_str()),
            (AuthMethod::Key, r"C:\Users\yo\claves\web.ppk")
        );
        let router = all.iter().find(|s| s.name == "router").unwrap();
        assert_eq!(
            (router.protocol, router.host.as_str(), router.port),
            (Protocol::Telnet, "192.168.1.1", 23)
        );
        // usuario@host en el campo del host; sin clave → contraseña.
        let proxied = all.iter().find(|s| s.name == "con-proxy").unwrap();
        assert_eq!(
            (proxied.username.as_str(), proxied.host.as_str()),
            ("root", "10.1.1.1")
        );
        assert_eq!((proxied.auth, proxied.port), (AuthMethod::Password, 22));
        let serial = all.iter().find(|s| s.name == "consola").unwrap();
        assert_eq!(
            (
                serial.protocol,
                serial.serial.device.as_str(),
                serial.serial.baud
            ),
            (Protocol::Serial, "COM3", 115_200)
        );
        assert!(
            imported
                .warnings
                .iter()
                .any(|w| w.contains("proxy.ejemplo.net"))
        );
        assert!(imported.warnings.iter().any(|w| w.contains("rlogin")));

        // Lo mismo, tal como lo exporta regedit: UTF-16 con marca de orden.
        let mut utf16 = vec![0xff, 0xfe];
        utf16.extend(text.encode_utf16().flat_map(|u| u.to_le_bytes()));
        assert_eq!(putty(&decode_text(&utf16)).count(), 4);
    }

    #[test]
    fn mobaxterm_sessions() {
        let text = "[Bookmarks]\r\nSubRep=\r\nImgNum=42\r\n\
            suelto=#109#0%10.0.0.1%22%root%%-1%-1%%%%%0%0%0%%%-1%0%0%0%%1080%%0%0%1#MobaFont%10%0%0%-1%15%236,236,236%30,30,30%180,180,192%0%-1%0%%xterm%-1%-1%_Std_Colors_0_%80%24%0%1%-1%<none>%%0%0%-1#0# #-1\r\n\
            \r\n[Bookmarks_1]\r\nSubRep=Clientes\\Acme\r\nImgNum=41\r\n\
            web __DIEZE__1=#109#0%web.acme.net%2222%deploy%%-1%-1%%jump.acme.net%22%salto%0%0%0%_ProfileDir_\\claves\\acme.ppk%%-1%0%0%0%%1080%%0%0%1#MobaFont#0# #-1\r\n\
            escritorio=#91#4%win.acme.net%3389%ACME\\admin%0%0%0%0%-1%0%0%0%0%0%0%-1#MobaFont#0# #-1\r\n\
            vnc=#128#5%10.2.2.2%5901%-1%0%%%0%0%%#MobaFont#0# #-1\r\n\
            ficheros=#140#7%sftp.acme.net%22%sube%-1%0%%0%%%0#MobaFont#0# #-1\r\n\
            consola=#131#8%-2%115200%3%0%0%1%2%COM3#MobaFont#0# #-1\r\n\
            [Otra]\r\nnada=#1#0%no.importar%22%x\r\n";
        let imported = mobaxterm(text);
        assert_eq!(imported.count(), 5);
        // La raíz y la carpeta anidada Clientes\Acme.
        let Node::Session(loose) = &imported.nodes[0] else {
            panic!("se esperaba una sesión en la raíz");
        };
        assert_eq!(
            (
                loose.name.as_str(),
                loose.host.as_str(),
                loose.username.as_str()
            ),
            ("suelto", "10.0.0.1", "root")
        );
        assert_eq!(loose.auth, AuthMethod::Password);
        let Node::Folder(clients) = &imported.nodes[1] else {
            panic!("se esperaba la carpeta Clientes");
        };
        let Node::Folder(acme) = &clients.children[0] else {
            panic!("se esperaba la carpeta Acme");
        };
        assert_eq!(
            (clients.name.as_str(), acme.name.as_str()),
            ("Clientes", "Acme")
        );
        let all = sessions(&acme.children);
        let web = all.iter().find(|s| s.name == "web #1").unwrap();
        assert_eq!(
            (web.host.as_str(), web.port, web.username.as_str()),
            ("web.acme.net", 2222, "deploy")
        );
        assert_eq!(
            (web.auth, web.key_path.as_str()),
            (AuthMethod::Key, r"%USERPROFILE%\claves\acme.ppk")
        );
        let rdp = all.iter().find(|s| s.name == "escritorio").unwrap();
        assert_eq!(
            (rdp.protocol, rdp.port, rdp.username.as_str()),
            (Protocol::Rdp, 3389, r"ACME\admin")
        );
        let vnc = all.iter().find(|s| s.name == "vnc").unwrap();
        assert_eq!((vnc.protocol, vnc.port), (Protocol::Vnc, 5901));
        let sftp = all.iter().find(|s| s.name == "ficheros").unwrap();
        assert_eq!(
            (sftp.protocol, sftp.username.as_str()),
            (Protocol::Sftp, "sube")
        );
        // Lo que no se importa se dice.
        assert!(
            imported
                .warnings
                .iter()
                .any(|w| w.contains("jump.acme.net"))
        );
        assert!(
            imported
                .warnings
                .iter()
                .any(|w| w.contains("consola") && w.contains("serie"))
        );
    }

    #[test]
    fn files_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        std::fs::write(&config, "Host uno\n  HostName 1.1.1.1\n  User u\n").unwrap();
        let imported = import_file(Source::OpenSsh, &config).unwrap();
        assert_eq!(imported.count(), 1);
        let folder = imported.into_folder(Source::OpenSsh.folder_name());
        assert_eq!(
            (folder.name.as_str(), folder.children.len()),
            ("Importado de OpenSSH", 1)
        );
        // Un fichero que no es lo que se dice, o que no existe.
        let e = import_file(Source::Putty, &config).unwrap_err();
        assert!(format!("{e:#}").contains("ningún equipo"), "{e:#}");
        assert!(import_file(Source::MobaXterm, &dir.path().join("no-existe")).is_err());
    }
}
