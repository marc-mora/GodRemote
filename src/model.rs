//! Modelo de datos: árbol jerárquico de carpetas y sesiones.

use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    Ssh,
    Sftp,
    Rdp,
}

impl Protocol {
    pub const ALL: [Protocol; 3] = [Protocol::Ssh, Protocol::Sftp, Protocol::Rdp];

    pub fn label(self) -> &'static str {
        match self {
            Protocol::Ssh => "SSH",
            Protocol::Sftp => "SFTP",
            Protocol::Rdp => "RDP",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Ssh | Protocol::Sftp => 22,
            Protocol::Rdp => 3389,
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Protocol::Ssh => "🖥",
            Protocol::Sftp => "📂",
            Protocol::Rdp => "🪟",
        }
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
}

impl Default for RdpOptions {
    fn default() -> Self {
        Self {
            domain: String::new(),
            fullscreen: false,
            width: 1600,
            height: 900,
            clipboard: true,
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
    pub rdp: RdpOptions,
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
            rdp: RdpOptions::default(),
            notes: String::new(),
        }
    }
}

impl Session {
    pub fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            if self.username.is_empty() {
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

    pub fn validate(&self) -> Result<(), String> {
        if self.host.trim().is_empty() {
            return Err("El host no puede estar vacío".into());
        }
        if self.port == 0 {
            return Err("El puerto no es válido".into());
        }
        if self.protocol != Protocol::Rdp && self.username.trim().is_empty() {
            return Err("El usuario no puede estar vacío".into());
        }
        if self.protocol != Protocol::Rdp
            && self.auth == AuthMethod::Key
            && self.key_path.trim().is_empty()
        {
            return Err("Indica la ruta de la clave privada".into());
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
        fn find(nodes: &mut Vec<Node>, id: Uuid) -> Option<&mut Vec<Node>> {
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

    /// Todos los ids de sesión contenidos en un nodo (recursivo).
    pub fn session_ids(node: &Node) -> Vec<Uuid> {
        match node {
            Node::Session(s) => vec![s.id],
            Node::Folder(f) => f.children.iter().flat_map(Tree::session_ids).collect(),
        }
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
    fn missing_file_is_empty_tree() {
        let dir = tempfile::tempdir().unwrap();
        let tree = Tree::load(&dir.path().join("nope.json")).unwrap();
        assert!(tree.nodes.is_empty());
    }
}
