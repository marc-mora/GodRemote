//! Modelo de datos: árbol jerárquico de carpetas y sesiones.

use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    Ssh,
    Sftp,
    Rdp,
    Vnc,
    Telnet,
    Serial,
}

impl Protocol {
    pub const ALL: [Protocol; 6] = [
        Protocol::Ssh,
        Protocol::Sftp,
        Protocol::Rdp,
        Protocol::Vnc,
        Protocol::Telnet,
        Protocol::Serial,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Protocol::Ssh => "SSH",
            Protocol::Sftp => "SFTP",
            Protocol::Rdp => "RDP",
            Protocol::Vnc => "VNC",
            Protocol::Telnet => "Telnet",
            Protocol::Serial => "Serie",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Ssh | Protocol::Sftp => 22,
            Protocol::Rdp => 3389,
            Protocol::Vnc => 5900,
            Protocol::Telnet => 23,
            // El puerto serie no usa puerto TCP.
            Protocol::Serial => 0,
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Protocol::Ssh => "💻",
            Protocol::Sftp => "📂",
            Protocol::Rdp => "🖥",
            Protocol::Vnc => "📺",
            Protocol::Telnet => "📞",
            Protocol::Serial => "🔌",
        }
    }

    /// Protocolos que viajan sobre SSH (usan usuario, clave, salto…).
    pub fn is_ssh(self) -> bool {
        matches!(self, Protocol::Ssh | Protocol::Sftp)
    }

    /// Protocolos que se conectan a un host y puerto TCP.
    pub fn uses_network(self) -> bool {
        self != Protocol::Serial
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMethod {
    /// Contraseña (guardada en el baúl o pedida al conectar).
    Password,
    /// Clave privada en disco.
    Key,
    /// Agente SSH (ssh-agent / Pageant).
    Agent,
}

impl AuthMethod {
    pub const ALL: [AuthMethod; 3] = [AuthMethod::Password, AuthMethod::Key, AuthMethod::Agent];

    pub fn label(self) -> &'static str {
        match self {
            AuthMethod::Password => "Contraseña",
            AuthMethod::Key => "Clave privada",
            AuthMethod::Agent => "Agente SSH",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RdpOptions {
    pub domain: String,
    pub fullscreen: bool,
    pub width: u32,
    pub height: u32,
    /// Redirigir el portapapeles.
    pub clipboard: bool,
    /// Usar el cliente integrado (en una pestaña) en vez del externo.
    pub embedded: bool,
    /// Con el cliente integrado: usar el tamaño de la pestaña al conectar.
    pub fit_window: bool,
}

impl Default for RdpOptions {
    fn default() -> Self {
        Self {
            domain: String::new(),
            fullscreen: false,
            width: 1600,
            height: 900,
            clipboard: true,
            embedded: true,
            fit_window: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TunnelKind {
    /// `-L`: un puerto local lleva a un destino visto desde el servidor.
    Local,
    /// `-R`: un puerto del servidor lleva a un destino visto desde aquí.
    Remote,
    /// `-D`: proxy SOCKS5 local que sale por el servidor.
    Dynamic,
}

impl TunnelKind {
    pub const ALL: [TunnelKind; 3] = [TunnelKind::Local, TunnelKind::Remote, TunnelKind::Dynamic];

    pub fn label(self) -> &'static str {
        match self {
            TunnelKind::Local => "Local (-L)",
            TunnelKind::Remote => "Remoto (-R)",
            TunnelKind::Dynamic => "Dinámico SOCKS (-D)",
        }
    }
}

/// Reenvío de puertos asociado a una sesión SSH.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tunnel {
    pub kind: TunnelKind,
    /// Dirección de escucha (local para `-L`/`-D`, del servidor para `-R`).
    pub listen_host: String,
    pub listen_port: u16,
    /// Destino (no se usa en túneles dinámicos).
    pub target_host: String,
    pub target_port: u16,
}

impl Default for Tunnel {
    fn default() -> Self {
        Self {
            kind: TunnelKind::Local,
            listen_host: "127.0.0.1".into(),
            listen_port: 8080,
            target_host: "127.0.0.1".into(),
            target_port: 80,
        }
    }
}

impl Tunnel {
    /// Descripción corta, al estilo de OpenSSH.
    pub fn describe(&self) -> String {
        match self.kind {
            TunnelKind::Local => format!(
                "-L {}:{} hacia {}:{}",
                self.listen_host, self.listen_port, self.target_host, self.target_port
            ),
            TunnelKind::Remote => format!(
                "-R {}:{} hacia {}:{}",
                self.listen_host, self.listen_port, self.target_host, self.target_port
            ),
            TunnelKind::Dynamic => {
                format!("-D {}:{} (SOCKS5)", self.listen_host, self.listen_port)
            }
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.listen_host.trim().is_empty() {
            return Err("Túnel: falta la dirección de escucha".into());
        }
        if self.listen_port == 0 {
            return Err("Túnel: el puerto de escucha no es válido".into());
        }
        if self.kind != TunnelKind::Dynamic {
            if self.target_host.trim().is_empty() {
                return Err("Túnel: falta el host de destino".into());
            }
            if self.target_port == 0 {
                return Err("Túnel: el puerto de destino no es válido".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SerialParity {
    None,
    Odd,
    Even,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SerialFlow {
    None,
    Software,
    Hardware,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SerialOptions {
    /// Dispositivo: `COM3`, `/dev/ttyUSB0`…
    pub device: String,
    pub baud: u32,
    pub data_bits: u8,
    pub parity: SerialParity,
    pub stop_bits: u8,
    pub flow: SerialFlow,
}

impl Default for SerialOptions {
    fn default() -> Self {
        Self {
            device: String::new(),
            baud: 115_200,
            data_bits: 8,
            parity: SerialParity::None,
            stop_bits: 1,
            flow: SerialFlow::None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub id: Uuid,
    pub name: String,
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
    /// Ruta a la clave privada (si `auth == Key`).
    pub key_path: String,
    /// Sesión SSH por la que saltar para llegar a esta (bastión).
    pub jump: Option<Uuid>,
    /// Reenvíos de puertos que se abren al conectar por SSH.
    pub tunnels: Vec<Tunnel>,
    /// Reenviar las aplicaciones gráficas X11 al servidor X local.
    pub x11_forwarding: bool,
    pub rdp: RdpOptions,
    pub serial: SerialOptions,
    pub notes: String,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: String::new(),
            protocol: Protocol::Ssh,
            host: String::new(),
            port: 22,
            username: String::new(),
            auth: AuthMethod::Password,
            key_path: String::new(),
            jump: None,
            tunnels: vec![],
            x11_forwarding: false,
            rdp: RdpOptions::default(),
            serial: SerialOptions::default(),
            notes: String::new(),
        }
    }
}

impl Session {
    pub fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            if self.protocol == Protocol::Serial {
                self.serial.device.clone()
            } else if self.username.is_empty() || !self.protocol.is_ssh() {
                self.host.clone()
            } else {
                format!("{}@{}", self.username, self.host)
            }
        } else {
            self.name.clone()
        }
    }

    /// Clave del baúl para la contraseña de esta sesión.
    pub fn password_key(&self) -> String {
        format!("{}/password", self.id)
    }

    /// Clave del baúl para la passphrase de la clave privada.
    pub fn passphrase_key(&self) -> String {
        format!("{}/passphrase", self.id)
    }

    /// Destino legible: `usuario@host:puerto` o el dispositivo serie.
    pub fn target(&self) -> String {
        match self.protocol {
            Protocol::Serial => format!("{} · {} baudios", self.serial.device, self.serial.baud),
            p if p.is_ssh() || (p == Protocol::Rdp && !self.username.is_empty()) => {
                format!("{}@{}:{}", self.username, self.host, self.port)
            }
            _ => format!("{}:{}", self.host, self.port),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.protocol == Protocol::Serial {
            if self.serial.device.trim().is_empty() {
                return Err("Indica el dispositivo serie (COM3, /dev/ttyUSB0…)".into());
            }
            if self.serial.baud == 0 {
                return Err("La velocidad no es válida".into());
            }
            if !(5..=8).contains(&self.serial.data_bits) {
                return Err("Los bits de datos deben estar entre 5 y 8".into());
            }
            if !(1..=2).contains(&self.serial.stop_bits) {
                return Err("Los bits de parada deben ser 1 o 2".into());
            }
            return Ok(());
        }
        if self.host.trim().is_empty() {
            return Err("El host no puede estar vacío".into());
        }
        if self.port == 0 {
            return Err("El puerto no es válido".into());
        }
        if self.protocol.is_ssh() {
            if self.username.trim().is_empty() {
                return Err("El usuario no puede estar vacío".into());
            }
            if self.auth == AuthMethod::Key && self.key_path.trim().is_empty() {
                return Err("Indica la ruta de la clave privada".into());
            }
            if self.jump == Some(self.id) {
                return Err("Una sesión no puede saltar a través de sí misma".into());
            }
            for t in &self.tunnels {
                t.validate()?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub children: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Node {
    Folder(Folder),
    Session(Session),
}

impl Node {
    pub fn id(&self) -> Uuid {
        match self {
            Node::Folder(f) => f.id,
            Node::Session(s) => s.id,
        }
    }

    pub fn name(&self) -> String {
        match self {
            Node::Folder(f) => f.name.clone(),
            Node::Session(s) => s.display_name(),
        }
    }
}

/// Árbol completo de equipos. La raíz es una lista de nodos.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tree {
    #[serde(default)]
    pub nodes: Vec<Node>,
}

impl Tree {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read(path) {
            Ok(data) => Ok(serde_json::from_slice(&data)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let data = serde_json::to_vec_pretty(self)?;
        crate::paths::write_private(path, &data)?;
        Ok(())
    }

    /// Hijos de la carpeta `parent` (o la raíz si es `None`).
    fn children_mut(&mut self, parent: Option<Uuid>) -> Option<&mut Vec<Node>> {
        fn find(nodes: &mut [Node], id: Uuid) -> Option<&mut Vec<Node>> {
            for n in nodes.iter_mut() {
                if let Node::Folder(f) = n {
                    if f.id == id {
                        return Some(&mut f.children);
                    }
                    if let Some(found) = find(&mut f.children, id) {
                        return Some(found);
                    }
                }
            }
            None
        }
        match parent {
            None => Some(&mut self.nodes),
            Some(id) => find(&mut self.nodes, id),
        }
    }

    /// Inserta un nodo dentro de `parent` (raíz si es `None` o no existe).
    pub fn insert(&mut self, parent: Option<Uuid>, node: Node) {
        if self.children_mut(parent).is_none() {
            self.nodes.push(node);
            return;
        }
        if let Some(children) = self.children_mut(parent) {
            children.push(node);
        }
    }

    pub fn add_folder(&mut self, parent: Option<Uuid>, name: &str) -> Uuid {
        let id = Uuid::new_v4();
        self.insert(
            parent,
            Node::Folder(Folder {
                id,
                name: name.to_string(),
                children: vec![],
            }),
        );
        id
    }

    /// Elimina un nodo (y todo su contenido) y lo devuelve.
    pub fn remove(&mut self, id: Uuid) -> Option<Node> {
        fn rm(nodes: &mut Vec<Node>, id: Uuid) -> Option<Node> {
            if let Some(pos) = nodes.iter().position(|n| n.id() == id) {
                return Some(nodes.remove(pos));
            }
            for n in nodes.iter_mut() {
                if let Node::Folder(f) = n
                    && let Some(found) = rm(&mut f.children, id)
                {
                    return Some(found);
                }
            }
            None
        }
        rm(&mut self.nodes, id)
    }

    pub fn find(&self, id: Uuid) -> Option<&Node> {
        fn go(nodes: &[Node], id: Uuid) -> Option<&Node> {
            for n in nodes {
                if n.id() == id {
                    return Some(n);
                }
                if let Node::Folder(f) = n
                    && let Some(found) = go(&f.children, id)
                {
                    return Some(found);
                }
            }
            None
        }
        go(&self.nodes, id)
    }

    pub fn find_mut(&mut self, id: Uuid) -> Option<&mut Node> {
        fn go(nodes: &mut [Node], id: Uuid) -> Option<&mut Node> {
            for n in nodes {
                if n.id() == id {
                    return Some(n);
                }
                if let Node::Folder(f) = n
                    && let Some(found) = go(&mut f.children, id)
                {
                    return Some(found);
                }
            }
            None
        }
        go(&mut self.nodes, id)
    }

    pub fn session(&self, id: Uuid) -> Option<&Session> {
        match self.find(id)? {
            Node::Session(s) => Some(s),
            Node::Folder(_) => None,
        }
    }

    /// Crea o actualiza una sesión (por id). Si es nueva, va dentro de `parent`.
    pub fn upsert_session(&mut self, parent: Option<Uuid>, session: Session) {
        if let Some(Node::Session(existing)) = self.find_mut(session.id) {
            *existing = session;
        } else {
            self.insert(parent, Node::Session(session));
        }
    }

    /// `true` si `candidate` es `ancestor` o está dentro de él.
    pub fn is_inside(&self, ancestor: Uuid, candidate: Uuid) -> bool {
        fn contains(node: &Node, id: Uuid) -> bool {
            if node.id() == id {
                return true;
            }
            match node {
                Node::Folder(f) => f.children.iter().any(|c| contains(c, id)),
                Node::Session(_) => false,
            }
        }
        self.find(ancestor).is_some_and(|n| contains(n, candidate))
    }

    /// Mueve un nodo a otra carpeta (`None` = raíz). No permite mover una
    /// carpeta dentro de sí misma.
    pub fn move_node(&mut self, id: Uuid, new_parent: Option<Uuid>) -> bool {
        if let Some(parent) = new_parent {
            if self.is_inside(id, parent) {
                return false;
            }
            if !matches!(self.find(parent), Some(Node::Folder(_))) {
                return false;
            }
        }
        match self.remove(id) {
            Some(node) => {
                self.insert(new_parent, node);
                true
            }
            None => false,
        }
    }

    /// Carpeta que contiene el nodo (`None` si está en la raíz o no existe).
    pub fn parent_of(&self, id: Uuid) -> Option<Uuid> {
        fn go(nodes: &[Node], id: Uuid, parent: Option<Uuid>) -> Option<Option<Uuid>> {
            for n in nodes {
                if n.id() == id {
                    return Some(parent);
                }
                if let Node::Folder(f) = n
                    && let Some(found) = go(&f.children, id, Some(f.id))
                {
                    return Some(found);
                }
            }
            None
        }
        go(&self.nodes, id, None).flatten()
    }

    /// Todos los ids de sesión contenidos en un nodo (recursivo).
    pub fn session_ids(node: &Node) -> Vec<Uuid> {
        match node {
            Node::Session(s) => vec![s.id],
            Node::Folder(f) => f.children.iter().flat_map(Tree::session_ids).collect(),
        }
    }

    /// Sesiones SSH del árbol (id y nombre), candidatas a servir de salto.
    pub fn ssh_sessions(&self) -> Vec<(Uuid, String)> {
        fn go(nodes: &[Node], out: &mut Vec<(Uuid, String)>) {
            for n in nodes {
                match n {
                    Node::Folder(f) => go(&f.children, out),
                    Node::Session(s) if s.protocol.is_ssh() => {
                        out.push((s.id, format!("{} ({})", s.display_name(), s.target())))
                    }
                    Node::Session(_) => {}
                }
            }
        }
        let mut out = vec![];
        go(&self.nodes, &mut out);
        out
    }

    /// Lista plana de carpetas con su ruta legible ("Clientes / ACME").
    pub fn folder_paths(&self) -> Vec<(Uuid, String)> {
        fn go(nodes: &[Node], prefix: &str, out: &mut Vec<(Uuid, String)>) {
            for n in nodes {
                if let Node::Folder(f) = n {
                    let path = if prefix.is_empty() {
                        f.name.clone()
                    } else {
                        format!("{prefix} / {}", f.name)
                    };
                    out.push((f.id, path.clone()));
                    go(&f.children, &path, out);
                }
            }
        }
        let mut out = vec![];
        go(&self.nodes, "", &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(name: &str) -> Session {
        Session {
            name: name.into(),
            host: "example.org".into(),
            username: "root".into(),
            ..Default::default()
        }
    }

    #[test]
    fn hierarchy_roundtrip() {
        let mut tree = Tree::default();
        let prod = tree.add_folder(None, "Producción");
        let web = tree.add_folder(Some(prod), "Web");
        let s = session("web-01");
        let sid = s.id;
        tree.upsert_session(Some(web), s);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.json");
        tree.save(&path).unwrap();
        let loaded = Tree::load(&path).unwrap();
        assert_eq!(tree, loaded);
        assert_eq!(loaded.session(sid).unwrap().name, "web-01");
        assert!(loaded.is_inside(prod, sid));
        assert_eq!(loaded.parent_of(sid), Some(web));
        assert_eq!(loaded.parent_of(prod), None);
        assert_eq!(loaded.folder_paths()[1].1, "Producción / Web");
    }

    #[test]
    fn move_and_remove() {
        let mut tree = Tree::default();
        let a = tree.add_folder(None, "A");
        let b = tree.add_folder(Some(a), "B");
        let s = session("x");
        let sid = s.id;
        tree.upsert_session(Some(b), s);

        // No se puede mover una carpeta dentro de sí misma.
        assert!(!tree.move_node(a, Some(b)));
        assert!(tree.move_node(sid, None));
        assert!(!tree.is_inside(a, sid));
        assert!(tree.move_node(sid, Some(a)));
        assert!(tree.is_inside(a, sid));

        let removed = tree.remove(a).unwrap();
        assert_eq!(Tree::session_ids(&removed), vec![sid]);
        assert!(tree.nodes.is_empty());
    }

    #[test]
    fn upsert_updates_in_place() {
        let mut tree = Tree::default();
        let mut s = session("uno");
        tree.upsert_session(None, s.clone());
        s.name = "dos".into();
        tree.upsert_session(None, s.clone());
        assert_eq!(tree.nodes.len(), 1);
        assert_eq!(tree.session(s.id).unwrap().name, "dos");
    }

    #[test]
    fn old_files_still_load() {
        // Fichero de la versión 0.1: sin túneles, salto ni opciones nuevas.
        let json = r#"{"nodes":[{"type":"session",
            "id":"cad9943e-cd96-428c-a085-db0329898fd8","name":"viejo",
            "protocol":"Rdp","host":"h","port":3389,"username":"u","auth":"Password",
            "key_path":"","rdp":{"domain":"D","fullscreen":false,"width":800,
            "height":600,"clipboard":true},"notes":""}]}"#;
        let tree: Tree = serde_json::from_str(json).unwrap();
        let Node::Session(s) = &tree.nodes[0] else {
            panic!("no es una sesión")
        };
        assert_eq!(s.rdp.width, 800);
        assert!(s.rdp.embedded);
        assert!(s.tunnels.is_empty() && s.jump.is_none() && !s.x11_forwarding);
        assert_eq!(s.serial.baud, 115_200);
    }

    #[test]
    fn validation_per_protocol() {
        let mut s = session("x");
        assert!(s.validate().is_ok());
        s.jump = Some(s.id);
        assert!(s.validate().is_err());
        s.jump = None;
        s.tunnels.push(Tunnel {
            listen_port: 0,
            ..Default::default()
        });
        assert!(s.validate().is_err());
        s.tunnels[0] = Tunnel {
            kind: TunnelKind::Dynamic,
            listen_port: 1080,
            target_host: String::new(),
            target_port: 0,
            ..Default::default()
        };
        assert!(s.validate().is_ok());
        assert_eq!(s.tunnels[0].describe(), "-D 127.0.0.1:1080 (SOCKS5)");

        let mut serial = Session {
            protocol: Protocol::Serial,
            ..Default::default()
        };
        assert!(serial.validate().is_err());
        serial.serial.device = "/dev/ttyUSB0".into();
        assert!(serial.validate().is_ok());
        assert_eq!(serial.display_name(), "/dev/ttyUSB0");

        let vnc = Session {
            protocol: Protocol::Vnc,
            host: "pc".into(),
            port: 5900,
            ..Default::default()
        };
        assert!(vnc.validate().is_ok());
        assert_eq!(vnc.target(), "pc:5900");
    }

    #[test]
    fn missing_file_is_empty_tree() {
        let dir = tempfile::tempdir().unwrap();
        let tree = Tree::load(&dir.path().join("nope.json")).unwrap();
        assert!(tree.nodes.is_empty());
    }
}
