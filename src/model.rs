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

    /// `true` si la sesión se abre en un terminal (y no en un explorador
    /// de ficheros o un escritorio remoto).
    pub fn has_terminal(self) -> bool {
        matches!(self, Protocol::Ssh | Protocol::Telnet | Protocol::Serial)
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
    /// Saltos (bastiones SSH) por los que pasar para llegar a este equipo,
    /// en orden: el primero es el más cercano a nosotros.
    pub jumps: Vec<Uuid>,
    /// Formato antiguo (un único salto). Solo se lee; al cargar pasa a
    /// `jumps`.
    #[doc(hidden)]
    #[serde(default, rename = "jump", skip_serializing)]
    pub legacy_jump: Option<Uuid>,
    /// Usar la autenticación (método y clave) que defina su carpeta.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub inherit_auth: bool,
    /// Usar los saltos que defina su carpeta, en lugar de los propios.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub inherit_jumps: bool,
    /// De quién son los secretos del baúl que usa: su carpeta, cuando
    /// hereda de ella la autenticación. Se calcula al conectar.
    #[serde(skip)]
    pub secret_owner: Option<Uuid>,
    /// Reenvíos de puertos que se abren al conectar por SSH.
    pub tunnels: Vec<Tunnel>,
    /// Reenviar las aplicaciones gráficas X11 al servidor X local.
    pub x11_forwarding: bool,
    pub rdp: RdpOptions,
    pub serial: SerialOptions,
    /// Mostrar bajo el terminal la barra con el estado del servidor (abre
    /// un canal SSH aparte para consultarlo).
    pub server_info: bool,
    /// Fondo del terminal propio de este equipo; sin él se usa el del tema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backdrop: Option<crate::backdrop::Backdrop>,
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
            jumps: vec![],
            legacy_jump: None,
            inherit_auth: false,
            inherit_jumps: false,
            secret_owner: None,
            tunnels: vec![],
            x11_forwarding: false,
            rdp: RdpOptions::default(),
            serial: SerialOptions::default(),
            server_info: true,
            backdrop: None,
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

    /// Clave del baúl para la contraseña de esta sesión (o la de la
    /// carpeta de la que hereda la autenticación).
    pub fn password_key(&self) -> String {
        format!("{}/password", self.secret_owner.unwrap_or(self.id))
    }

    /// Clave del baúl para la passphrase de la clave privada.
    pub fn passphrase_key(&self) -> String {
        format!("{}/passphrase", self.secret_owner.unwrap_or(self.id))
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
            for t in &self.tunnels {
                t.validate()?;
            }
        }
        if self.jumps.contains(&self.id) {
            return Err("Un equipo no puede saltar a través de sí mismo".into());
        }
        let mut seen = self.jumps.clone();
        seen.sort();
        seen.dedup();
        if seen.len() != self.jumps.len() {
            return Err("Hay un salto repetido en la lista".into());
        }
        Ok(())
    }

    /// Pasa los datos de formatos antiguos al actual.
    fn migrate(&mut self) {
        if let Some(jump) = self.legacy_jump.take()
            && self.jumps.is_empty()
        {
            self.jumps.push(jump);
        }
        if let Some(backdrop) = &mut self.backdrop {
            backdrop.sanitize();
        }
    }
}

/// Valores que una carpeta da a los equipos que contiene. Los heredan
/// también los de sus subcarpetas, salvo que una más cercana defina otros.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FolderDefaults {
    /// Usuario de los equipos que no indiquen el suyo.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub username: String,
    /// Autenticación de los equipos que la hereden.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthMethod>,
    /// Clave privada, si la autenticación es por clave.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub key_path: String,
    /// Saltos de los equipos que los hereden.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jumps: Option<Vec<Uuid>>,
    /// Fondo del terminal de los equipos que no tengan uno propio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backdrop: Option<crate::backdrop::Backdrop>,
    /// `Some(false)`: sin barra de información en estos equipos.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_info: Option<bool>,
}

impl FolderDefaults {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Lo que un equipo recibiría de sus carpetas, y de cuál: sirve al editor
/// para explicarlo. Cada dato lleva el nombre de la carpeta que lo define.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inherited {
    pub username: Option<(String, String)>,
    /// (carpeta, método, clave privada).
    pub auth: Option<(String, AuthMethod, String)>,
    pub jumps: Option<(String, Vec<Uuid>)>,
    pub backdrop: Option<String>,
    /// Carpeta que desactiva la barra de información.
    pub server_info_off: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "FolderDefaults::is_empty")]
    pub defaults: FolderDefaults,
    #[serde(default)]
    pub children: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
// Los árboles son pequeños: no compensa añadir una indirección por sesión.
#[allow(clippy::large_enum_variant)]
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

/// Máximo de saltos encadenados para llegar a un equipo.
pub const MAX_JUMPS: usize = 8;

/// Árbol completo de equipos. La raíz es una lista de nodos.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tree {
    #[serde(default)]
    pub nodes: Vec<Node>,
}

impl Tree {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read(path) {
            Ok(data) => {
                let mut tree: Tree = serde_json::from_slice(&data)?;
                tree.migrate();
                Ok(tree)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Pasa las sesiones de formatos antiguos al actual.
    pub fn migrate(&mut self) {
        fn go(nodes: &mut [Node]) {
            for n in nodes {
                match n {
                    Node::Folder(f) => {
                        if let Some(backdrop) = &mut f.defaults.backdrop {
                            backdrop.sanitize();
                        }
                        go(&mut f.children)
                    }
                    Node::Session(s) => s.migrate(),
                }
            }
        }
        go(&mut self.nodes);
    }

    /// Camino completo de saltos para llegar a `session`, en orden (el
    /// primero es el más cercano a nosotros).
    ///
    /// Se recorren los saltos de la sesión en el orden indicado y, antes de
    /// cada uno, los que ese salto necesite a su vez. Un equipo que ya está
    /// en el camino no se repite.
    pub fn jump_path(&self, session: &Session) -> Result<Vec<Session>, String> {
        fn expand(
            tree: &Tree,
            session: &Session,
            stack: &mut Vec<Uuid>,
            path: &mut Vec<Session>,
        ) -> Result<(), String> {
            if stack.contains(&session.id) {
                return Err(format!(
                    "Los saltos forman un bucle (pasan dos veces por «{}»)",
                    session.display_name()
                ));
            }
            stack.push(session.id);
            for hop_id in &session.jumps {
                // El salto, con lo que herede de su carpeta.
                let hop = tree.resolved_session(*hop_id).ok_or_else(|| {
                    format!(
                        "Un salto de «{}» ya no existe; edita la sesión",
                        session.display_name()
                    )
                })?;
                if !hop.protocol.is_ssh() {
                    return Err(format!(
                        "El salto «{}» no es una sesión SSH",
                        hop.display_name()
                    ));
                }
                if path.iter().any(|s| s.id == hop.id) {
                    continue;
                }
                // Primero, los saltos que este salto necesita a su vez.
                expand(tree, &hop, stack, path)?;
                path.push(hop);
                if path.len() > MAX_JUMPS {
                    return Err(format!(
                        "Demasiados saltos encadenados (máximo {MAX_JUMPS})"
                    ));
                }
            }
            stack.pop();
            Ok(())
        }
        let mut path = vec![];
        expand(self, session, &mut vec![], &mut path)?;
        Ok(path)
    }

    /// Carpetas que contienen a `parent` (incluida), de la más cercana a
    /// la raíz.
    pub fn ancestors(&self, parent: Option<Uuid>) -> Vec<&Folder> {
        let mut chain = vec![];
        let mut current = parent;
        while let Some(id) = current {
            let Some(Node::Folder(folder)) = self.find(id) else {
                break;
            };
            chain.push(folder);
            current = self.parent_of(id);
            // Un árbol dañado no debe dejar esto dando vueltas.
            if chain.len() > 64 {
                break;
            }
        }
        chain
    }

    /// Lo que heredaría un equipo situado en la carpeta `parent`.
    pub fn inherited(&self, parent: Option<Uuid>) -> Inherited {
        let mut out = Inherited::default();
        for folder in self.ancestors(parent) {
            let d = &folder.defaults;
            let name = folder.name.clone();
            if out.username.is_none() && !d.username.trim().is_empty() {
                out.username = Some((name.clone(), d.username.trim().to_string()));
            }
            if out.auth.is_none()
                && let Some(auth) = d.auth
            {
                out.auth = Some((name.clone(), auth, d.key_path.clone()));
            }
            if out.jumps.is_none()
                && let Some(jumps) = &d.jumps
            {
                out.jumps = Some((name.clone(), jumps.clone()));
            }
            if out.backdrop.is_none() && d.backdrop.is_some() {
                out.backdrop = Some(name.clone());
            }
            if out.server_info_off.is_none() && d.server_info == Some(false) {
                out.server_info_off = Some(name);
            }
        }
        out
    }

    /// La sesión tal como se conecta: con lo que hereda de las carpetas
    /// que la contienen. `parent` es la carpeta donde está (o va a estar).
    pub fn resolve(&self, session: &Session, parent: Option<Uuid>) -> Session {
        let mut out = session.clone();
        let mut username_done = !out.username.trim().is_empty();
        let mut auth_done = !out.inherit_auth;
        let mut jumps_done = !out.inherit_jumps;
        let mut backdrop_done = out.backdrop.is_some();
        for folder in self.ancestors(parent) {
            let d = &folder.defaults;
            if !username_done && !d.username.trim().is_empty() {
                out.username = d.username.trim().to_string();
                username_done = true;
            }
            if !auth_done && let Some(auth) = d.auth {
                out.auth = auth;
                out.key_path = d.key_path.clone();
                // Sus secretos son los de la carpeta, comunes a todos los
                // equipos que heredan de ella.
                out.secret_owner = Some(folder.id);
                auth_done = true;
            }
            if !jumps_done && let Some(jumps) = &d.jumps {
                // Un equipo no puede saltar a través de sí mismo.
                out.jumps = jumps.iter().copied().filter(|j| *j != out.id).collect();
                jumps_done = true;
            }
            if !backdrop_done && let Some(backdrop) = &d.backdrop {
                out.backdrop = Some(backdrop.clone());
                backdrop_done = true;
            }
            if d.server_info == Some(false) {
                out.server_info = false;
            }
        }
        out
    }

    /// Una sesión del árbol, lista para conectar (véase [`Tree::resolve`]).
    pub fn resolved_session(&self, id: Uuid) -> Option<Session> {
        let session = self.session(id)?;
        Some(self.resolve(session, self.parent_of(id)))
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
                defaults: FolderDefaults::default(),
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
        assert!(s.tunnels.is_empty() && s.jumps.is_empty() && !s.x11_forwarding);
        assert_eq!(s.serial.baud, 115_200);
    }

    fn names(path: &[Session]) -> Vec<&str> {
        path.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn folders_give_values_to_their_sessions() {
        let mut tree = Tree::default();
        let bastion = session("bastion");
        tree.upsert_session(None, bastion.clone());
        let prod = tree.add_folder(None, "Producción");
        let web = tree.add_folder(Some(prod), "Web");
        if let Some(Node::Folder(f)) = tree.find_mut(prod) {
            f.defaults = FolderDefaults {
                username: " deploy ".into(),
                auth: Some(AuthMethod::Key),
                key_path: "~/.ssh/prod".into(),
                jumps: Some(vec![bastion.id]),
                backdrop: Some(crate::backdrop::Backdrop {
                    kind: crate::backdrop::BackdropKind::Text,
                    text: "PRODUCCIÓN".into(),
                    ..Default::default()
                }),
                server_info: None,
            };
        }
        if let Some(Node::Folder(f)) = tree.find_mut(web) {
            f.defaults.username = "www".into();
            f.defaults.server_info = Some(false);
        }

        // Un equipo de la subcarpeta, que hereda todo lo que puede.
        let mut one = session("web-01");
        one.username.clear();
        one.inherit_auth = true;
        one.inherit_jumps = true;
        tree.upsert_session(Some(web), one.clone());
        let r = tree.resolved_session(one.id).unwrap();
        // El usuario, de la carpeta más cercana; lo demás, de la de arriba.
        assert_eq!(r.username, "www");
        assert_eq!(
            (r.auth, r.key_path.as_str()),
            (AuthMethod::Key, "~/.ssh/prod")
        );
        assert_eq!(r.jumps, vec![bastion.id]);
        assert_eq!(r.backdrop.as_ref().unwrap().text, "PRODUCCIÓN");
        assert!(!r.server_info);
        // Sus secretos son los de la carpeta que define la autenticación.
        assert_eq!(r.password_key(), format!("{prod}/password"));
        assert_eq!(r.passphrase_key(), format!("{prod}/passphrase"));
        // Y el camino de saltos sale de lo heredado.
        assert_eq!(names(&tree.jump_path(&r).unwrap()), ["bastion"]);
        // La sesión guardada no cambia.
        assert_eq!(tree.session(one.id).unwrap(), &one);

        // Lo que el equipo define por sí mismo manda sobre la carpeta.
        let mut own = session("web-02");
        own.username = "root".into();
        own.auth = AuthMethod::Password;
        own.backdrop = Some(Default::default());
        tree.upsert_session(Some(prod), own.clone());
        let r = tree.resolved_session(own.id).unwrap();
        assert_eq!(
            (r.username.as_str(), r.auth),
            ("root", AuthMethod::Password)
        );
        assert!(r.jumps.is_empty() && r.server_info);
        assert_eq!(r.backdrop, Some(Default::default()));
        assert_eq!(r.password_key(), format!("{}/password", own.id));

        // Fuera de las carpetas no se hereda nada, aunque se pida.
        let mut loose = session("suelto");
        loose.inherit_auth = true;
        loose.inherit_jumps = true;
        loose.jumps = vec![bastion.id];
        tree.upsert_session(None, loose.clone());
        let r = tree.resolved_session(loose.id).unwrap();
        assert_eq!((r.auth, r.jumps.clone()), (loose.auth, vec![bastion.id]));
        assert_eq!(r.secret_owner, None);

        // Lo que explica el editor: qué carpeta da cada cosa.
        let i = tree.inherited(Some(web));
        assert_eq!(i.username, Some(("Web".into(), "www".into())));
        assert_eq!(i.auth.as_ref().map(|a| a.0.as_str()), Some("Producción"));
        assert_eq!(i.jumps.as_ref().map(|j| j.0.as_str()), Some("Producción"));
        assert_eq!(i.backdrop.as_deref(), Some("Producción"));
        assert_eq!(i.server_info_off.as_deref(), Some("Web"));
        assert_eq!(tree.inherited(None), Inherited::default());

        // Un salto dentro de una carpeta también hereda (aquí, el usuario).
        let mut hop = session("salto-interno");
        hop.username.clear();
        tree.upsert_session(Some(prod), hop.clone());
        let mut target = session("destino");
        target.jumps = vec![hop.id];
        tree.upsert_session(None, target.clone());
        let path = tree.jump_path(&target).unwrap();
        assert_eq!(path[0].username, "deploy");

        // Los saltos de la carpeta no hacen saltar a un equipo por sí mismo.
        let mut in_prod = bastion.clone();
        in_prod.id = Uuid::new_v4();
        in_prod.inherit_jumps = true;
        tree.upsert_session(Some(prod), in_prod.clone());
        if let Some(Node::Folder(f)) = tree.find_mut(prod) {
            f.defaults.jumps = Some(vec![in_prod.id, bastion.id]);
        }
        assert_eq!(
            tree.resolved_session(in_prod.id).unwrap().jumps,
            vec![bastion.id]
        );

        // Se guarda y se recupera; una carpeta sin valores no escribe nada.
        let json = serde_json::to_string(&tree).unwrap();
        assert!(
            json.contains("\"defaults\":{\"username\":\" deploy \""),
            "{json}"
        );
        assert_eq!(json.matches("\"defaults\"").count(), 2, "{json}");
        assert!(json.contains("\"inherit_auth\":true"));
        assert!(!json.contains("secret_owner"));
        assert_eq!(serde_json::from_str::<Tree>(&json).unwrap(), tree);
    }

    #[test]
    fn jump_path_with_several_hops() {
        let mut tree = Tree::default();
        let (a, b, c) = (session("a"), session("b"), session("c"));
        let mut target = session("destino");
        target.jumps = vec![a.id, b.id, c.id];
        for s in [&a, &b, &c, &target] {
            tree.upsert_session(None, s.clone());
        }
        // Tres saltos, en el orden indicado.
        assert_eq!(names(&tree.jump_path(&target).unwrap()), ["a", "b", "c"]);
        // Sin saltos: conexión directa.
        assert!(tree.jump_path(&a).unwrap().is_empty());

        // Un salto que a su vez necesita otro: se intercala delante.
        let mut inner = session("interno");
        let outer = session("externo");
        inner.jumps = vec![outer.id];
        let mut via_inner = session("via-interno");
        via_inner.jumps = vec![inner.id];
        for s in [&inner, &outer, &via_inner] {
            tree.upsert_session(None, s.clone());
        }
        assert_eq!(
            names(&tree.jump_path(&via_inner).unwrap()),
            ["externo", "interno"]
        );
        // Si ya figura en la lista, no se repite.
        via_inner.jumps = vec![outer.id, inner.id];
        assert_eq!(
            names(&tree.jump_path(&via_inner).unwrap()),
            ["externo", "interno"]
        );
        via_inner.jumps = vec![a.id, inner.id, b.id];
        assert_eq!(
            names(&tree.jump_path(&via_inner).unwrap()),
            ["a", "externo", "interno", "b"]
        );
    }

    #[test]
    fn jump_path_errors() {
        let mut tree = Tree::default();
        let mut a = session("a");
        let mut b = session("b");
        // Bucle: a salta por b y b por a.
        a.jumps = vec![b.id];
        b.jumps = vec![a.id];
        tree.upsert_session(None, a.clone());
        tree.upsert_session(None, b.clone());
        let err = tree.jump_path(&a).unwrap_err();
        assert!(err.contains("bucle"), "{err}");

        // Salto eliminado.
        let mut orphan = session("huerfano");
        orphan.jumps = vec![Uuid::new_v4()];
        let err = tree.jump_path(&orphan).unwrap_err();
        assert!(err.contains("ya no existe"), "{err}");

        // Un salto tiene que ser SSH.
        let rdp = Session {
            name: "win".into(),
            protocol: Protocol::Rdp,
            host: "w".into(),
            ..Default::default()
        };
        tree.upsert_session(None, rdp.clone());
        let mut bad = session("mal");
        bad.jumps = vec![rdp.id];
        let err = tree.jump_path(&bad).unwrap_err();
        assert!(err.contains("no es una sesión SSH"), "{err}");

        // Demasiados saltos.
        let mut long = session("largo");
        for i in 0..=MAX_JUMPS {
            let hop = session(&format!("h{i}"));
            long.jumps.push(hop.id);
            tree.upsert_session(None, hop);
        }
        let err = tree.jump_path(&long).unwrap_err();
        assert!(err.contains("Demasiados saltos"), "{err}");
        // Justo en el límite sí vale.
        long.jumps.pop();
        assert_eq!(tree.jump_path(&long).unwrap().len(), MAX_JUMPS);
    }

    #[test]
    fn single_jump_from_old_files_is_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.json");
        // Formato 0.2.0: campo `jump` con un único salto.
        std::fs::write(
            &path,
            r#"{"nodes":[
              {"type":"session","id":"11111111-1111-4111-8111-111111111111","name":"bastion",
               "protocol":"Ssh","host":"b","port":22,"username":"u","auth":"Agent","key_path":"","notes":""},
              {"type":"session","id":"22222222-2222-4222-8222-222222222222","name":"destino",
               "protocol":"Ssh","host":"d","port":22,"username":"u","auth":"Agent","key_path":"",
               "jump":"11111111-1111-4111-8111-111111111111","notes":""}]}"#,
        )
        .unwrap();
        let tree = Tree::load(&path).unwrap();
        let target = tree
            .session(Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap())
            .unwrap();
        assert_eq!(target.jumps.len(), 1);
        assert_eq!(names(&tree.jump_path(target).unwrap()), ["bastion"]);
        // Al guardar ya solo queda el formato nuevo.
        tree.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("\"jumps\"") && !text.contains("\"jump\""),
            "{text}"
        );
    }

    #[test]
    fn validation_per_protocol() {
        let mut s = session("x");
        assert!(s.validate().is_ok());
        s.jumps = vec![s.id];
        assert!(s.validate().is_err());
        let other = Uuid::new_v4();
        s.jumps = vec![other, other];
        assert!(s.validate().is_err());
        s.jumps = vec![other, Uuid::new_v4()];
        assert!(s.validate().is_ok());
        s.jumps.clear();
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
