//! Sincronización de la configuración entre los equipos del usuario.
//!
//! El usuario elige una carpeta que él ya replica (Dropbox, Syncthing, una
//! unidad de red…) y una contraseña. GodRemote guarda ahí **un fichero
//! cifrado** con toda la configuración que se comparte (equipos, snippets,
//! túneles, temas, ajustes y el baúl, este último tal cual: ya va cifrado
//! con su contraseña maestra) y lo mantiene al día en los dos sentidos.
//!
//! Cómo se decide qué gana:
//!
//! - Cada escritura es una *instantánea* con un identificador al azar, un
//!   número de revisión y la lista de instantáneas de las que desciende.
//!   Nunca se miran las horas: un reloj mal puesto no cambia nada.
//! - Este equipo recuerda (en `sync-estado.json`) lo acordado con las últimas
//!   instantáneas que leyó o escribió. Al sincronizar busca la más reciente
//!   de la que desciende lo que hay en la carpeta y hace con ella una fusión
//!   a tres bandas, elemento a elemento: si solo cambió un lado, gana ese;
//!   si cambiaron los dos lo mismo de forma distinta, es un conflicto y se
//!   pregunta. Los borrados se propagan porque se conoce la base.
//! - Si dos equipos escriben a la vez y el servicio se queda con uno, el
//!   otro lo nota (lo que hay no desciende de lo que escribió), retrocede a
//!   la base anterior y vuelve a enviar sus cambios: no se pierde nada.
//! - Sin base conocida (primera vez, carpeta reiniciada) la fusión es una
//!   unión: nada se borra y lo que difiere se pregunta.
//!
//! Todo lo que llega de la carpeta es entrada no fiable, aunque descifre
//! bien: tiene límites de tamaño y de número de elementos, solo se escriben
//! los ficheros de configuración de siempre y antes de tocar nada se guarda
//! una copia de la configuración local.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::macros::{Macro, MacroStore};
use crate::model::{Folder, FolderDefaults, Node, Session, Tree};
use crate::theme::{Settings, Theme, ThemeStore};
use crate::tunnel_manager::{SavedTunnel, TunnelStore};

// ----------------------------------------------------------------------
// Ficheros cifrados
// ----------------------------------------------------------------------

/// Para qué es un fichero cifrado. Entra en los datos autenticados: un
/// fichero de un tipo no se puede hacer pasar por otro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealKind {
    /// La configuración sincronizada entre los equipos de un usuario.
    Sync,
    /// Una carpeta de equipos publicada para los compañeros.
    Share,
}

impl SealKind {
    fn tag(self) -> &'static str {
        match self {
            SealKind::Sync => "sincronizacion",
            SealKind::Share => "carpeta",
        }
    }
}

const SEAL_VERSION: u32 = 1;
// Los mismos costes que el baúl.
const KDF_M_COST: u32 = 64 * 1024; // 64 MiB
const KDF_T_COST: u32 = 3;
const KDF_P_COST: u32 = 1;
// Topes para lo que diga un fichero ajeno: derivar la clave no debe poder
// usarse para agotar la memoria o el procesador.
const MAX_M_COST: u32 = 1024 * 1024; // 1 GiB
const MAX_T_COST: u32 = 16;
const MAX_P_COST: u32 = 8;
/// Tamaño máximo de un fichero cifrado en disco.
pub const MAX_SEALED: u64 = 128 * 1024 * 1024;
/// Tamaño máximo de su contenido una vez descomprimido.
pub const MAX_PLAIN: u64 = 64 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct SealedFile {
    #[serde(rename = "godremote_cifrado")]
    version: u32,
    #[serde(rename = "tipo")]
    kind: String,
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
    salt: String,
    /// Deriva de la contraseña: distingue «contraseña equivocada» de
    /// «fichero dañado».
    #[serde(rename = "comprobacion")]
    check: String,
    nonce: String,
    #[serde(rename = "datos")]
    data: String,
}

/// Sal y costes con los que se deriva la clave de un fichero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealParams {
    salt: [u8; 16],
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
}

impl SealParams {
    /// Sal nueva con los costes de fábrica.
    pub fn fresh() -> Self {
        let mut salt = [0u8; 16];
        rand::fill(&mut salt);
        Self {
            salt,
            m_cost: KDF_M_COST,
            t_cost: KDF_T_COST,
            p_cost: KDF_P_COST,
        }
    }

    /// La sal de un fichero que ya se pudo abrir, para volver a escribirlo
    /// sin que los demás tengan que derivar otra clave; o una nueva si no la
    /// hay o rebaja los costes de fábrica.
    pub fn reuse(previous: Option<&SealParams>) -> Self {
        previous
            .filter(|p| p.strong_enough())
            .cloned()
            .unwrap_or_else(Self::fresh)
    }

    fn strong_enough(&self) -> bool {
        self.m_cost >= KDF_M_COST && self.t_cost >= KDF_T_COST && self.p_cost >= KDF_P_COST
    }

    fn acceptable(&self) -> bool {
        (8..=MAX_M_COST).contains(&self.m_cost)
            && (1..=MAX_T_COST).contains(&self.t_cost)
            && (1..=MAX_P_COST).contains(&self.p_cost)
            && self.m_cost >= 8 * self.p_cost
    }
}

/// Por qué no se ha podido abrir un fichero cifrado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealError {
    /// Vacío o cortado: seguramente se está escribiendo todavía.
    Incomplete,
    WrongPassword,
    /// Dañado, manipulado o de otra cosa.
    Corrupt,
    /// Escrito por una versión más reciente.
    Newer,
    TooBig,
    /// No se pudo cifrar o derivar la clave (no debería ocurrir).
    Internal(String),
}

impl std::fmt::Display for SealError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SealError::Incomplete => {
                write!(f, "El fichero está incompleto (quizá se está copiando)")
            }
            SealError::WrongPassword => write!(f, "La contraseña no es la de este fichero"),
            SealError::Corrupt => write!(
                f,
                "El fichero está dañado o no es de GodRemote (no se ha tocado nada)"
            ),
            SealError::Newer => write!(
                f,
                "El fichero es de una versión más reciente de GodRemote: actualiza este equipo"
            ),
            SealError::TooBig => write!(f, "El fichero es demasiado grande"),
            SealError::Internal(e) => write!(f, "No se pudo cifrar: {e}"),
        }
    }
}

impl std::error::Error for SealError {}

/// Clave y comprobación derivadas de una contraseña.
type Material = Zeroizing<[u8; 48]>;

/// Una contraseña y las claves ya derivadas de ella. La contraseña vive solo
/// en memoria y se borra al soltar el llavero.
pub struct Keyring {
    password: Zeroizing<String>,
    cache: Vec<(SealParams, Material)>,
}

impl Keyring {
    pub fn new(password: &str) -> Self {
        Self {
            password: Zeroizing::new(password.to_string()),
            cache: vec![],
        }
    }

    pub fn password(&self) -> &str {
        &self.password
    }

    fn material(&mut self, params: &SealParams) -> Result<Material, SealError> {
        if let Some((_, material)) = self.cache.iter().find(|(p, _)| p == params) {
            return Ok(material.clone());
        }
        if !params.acceptable() {
            return Err(SealError::Corrupt);
        }
        let kdf = Params::new(params.m_cost, params.t_cost, params.p_cost, Some(48))
            .map_err(|_| SealError::Corrupt)?;
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, kdf);
        let mut material = Zeroizing::new([0u8; 48]);
        argon
            .hash_password_into(self.password.as_bytes(), &params.salt, material.as_mut())
            .map_err(|e| SealError::Internal(e.to_string()))?;
        // Unas pocas sales distintas como mucho: lo normal es una sola.
        if self.cache.len() >= 8 {
            self.cache.remove(0);
        }
        self.cache.push((params.clone(), material.clone()));
        Ok(material)
    }
}

fn aad(kind: SealKind, params: &SealParams, check: &str) -> Vec<u8> {
    format!(
        "godremote-cifrado-v{SEAL_VERSION}|{}|{}|{}|{}|{}|{check}",
        kind.tag(),
        params.m_cost,
        params.t_cost,
        params.p_cost,
        B64.encode(params.salt),
    )
    .into_bytes()
}

/// Cifra `plain` (comprimido antes) con la contraseña del llavero.
pub fn seal(
    kind: SealKind,
    keys: &mut Keyring,
    params: &SealParams,
    plain: &[u8],
) -> Result<Vec<u8>, SealError> {
    let material = keys.material(params)?;
    let internal = |e: &dyn std::fmt::Display| SealError::Internal(e.to_string());
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(plain).map_err(|e| internal(&e))?;
    let packed = Zeroizing::new(encoder.finish().map_err(|e| internal(&e))?);
    let mut nonce = [0u8; 24];
    rand::fill(&mut nonce);
    let check = B64.encode(&material[32..]);
    let cipher = XChaCha20Poly1305::new_from_slice(&material[..32])
        .map_err(|_| internal(&"clave no válida"))?;
    let data = cipher
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: &packed,
                aad: &aad(kind, params, &check),
            },
        )
        .map_err(|_| internal(&"fallo al cifrar"))?;
    let file = SealedFile {
        version: SEAL_VERSION,
        kind: kind.tag().to_string(),
        m_cost: params.m_cost,
        t_cost: params.t_cost,
        p_cost: params.p_cost,
        salt: B64.encode(params.salt),
        check,
        nonce: B64.encode(nonce),
        data: B64.encode(data),
    };
    serde_json::to_vec(&file).map_err(|e| internal(&e))
}

/// Abre un fichero cifrado. Devuelve el contenido y la sal con la que se
/// cifró.
pub fn open(
    kind: SealKind,
    keys: &mut Keyring,
    raw: &[u8],
) -> Result<(Zeroizing<Vec<u8>>, SealParams), SealError> {
    if raw.iter().all(|b| b.is_ascii_whitespace() || *b == 0) {
        return Err(SealError::Incomplete);
    }
    let file: SealedFile = serde_json::from_slice(raw).map_err(|e| {
        if e.is_eof() {
            SealError::Incomplete
        } else {
            SealError::Corrupt
        }
    })?;
    if file.version > SEAL_VERSION {
        return Err(SealError::Newer);
    }
    if file.version == 0 || file.kind != kind.tag() {
        return Err(SealError::Corrupt);
    }
    let fixed = |text: &str, len: usize| -> Result<Vec<u8>, SealError> {
        B64.decode(text)
            .ok()
            .filter(|bytes| bytes.len() == len)
            .ok_or(SealError::Corrupt)
    };
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&fixed(&file.salt, 16)?);
    let mut nonce = [0u8; 24];
    nonce.copy_from_slice(&fixed(&file.nonce, 24)?);
    let check = fixed(&file.check, 16)?;
    let params = SealParams {
        salt,
        m_cost: file.m_cost,
        t_cost: file.t_cost,
        p_cost: file.p_cost,
    };
    if !params.acceptable() {
        return Err(SealError::Corrupt);
    }
    let data = B64.decode(&file.data).map_err(|_| SealError::Corrupt)?;
    let material = keys.material(&params)?;
    // Comparación sin atajos: no debe depender de cuántos bytes coinciden.
    let differ = material[32..]
        .iter()
        .zip(&check)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b));
    if differ != 0 {
        return Err(SealError::WrongPassword);
    }
    let cipher =
        XChaCha20Poly1305::new_from_slice(&material[..32]).map_err(|_| SealError::Corrupt)?;
    let packed = Zeroizing::new(
        cipher
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &data,
                    aad: &aad(kind, &params, &file.check),
                },
            )
            .map_err(|_| SealError::Corrupt)?,
    );
    let mut plain = Zeroizing::new(Vec::new());
    flate2::read::ZlibDecoder::new(&packed[..])
        .take(MAX_PLAIN + 1)
        .read_to_end(&mut plain)
        .map_err(|_| SealError::Corrupt)?;
    if plain.len() as u64 > MAX_PLAIN {
        return Err(SealError::TooBig);
    }
    Ok((plain, params))
}

/// Lee un fichero sin pasar de `max` bytes.
pub fn read_limited(path: &Path, max: u64) -> std::io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::other("no es un fichero"));
    }
    if meta.len() > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "es demasiado grande",
        ));
    }
    let mut data = Vec::with_capacity(meta.len() as usize);
    file.take(max + 1).read_to_end(&mut data)?;
    if data.len() as u64 > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "es demasiado grande",
        ));
    }
    Ok(data)
}

/// Escribe un fichero en una carpeta compartida de forma atómica: primero a
/// un temporal junto a él (con `tag` en el nombre, para no pisarse con otro
/// equipo) y después se renombra. Quien lo lea ve el anterior o el nuevo,
/// nunca uno a medias.
pub fn write_atomic(path: &Path, data: &[u8], tag: &str) -> std::io::Result<()> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| std::io::Error::other("ruta sin nombre de fichero"))?;
    let tmp = path.with_file_name(format!(".{name}.{tag}.tmp"));
    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(data)?;
        // Hay unidades de red que no saben volcar a disco: no es motivo
        // para no sincronizar.
        let _ = file.sync_all();
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Huella de un contenido (SHA-256 en hexadecimal).
pub fn fingerprint(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ----------------------------------------------------------------------
// Elementos: la configuración vista como «clave → valor JSON»
// ----------------------------------------------------------------------

/// La configuración que se sincroniza, elemento a elemento:
///
/// - `c/<id>`: una carpeta del árbol (sin sus hijos) y `s/<id>`: un equipo;
///   los dos llevan en `@en` la carpeta que los contiene.
/// - `orden/<id>` (o `orden/raiz`): el orden de los hijos de una carpeta.
/// - `m/<id>`: un snippet; `t/<id>`: un túnel del gestor; `orden/m` y
///   `orden/t`, su orden.
/// - `tema/<nombre>`: un tema del usuario.
/// - `a/<campo>`: un ajuste.
/// - `baul`: el baúl, tal cual está en disco (cifrado con su contraseña).
pub type Items = BTreeMap<String, Value>;

/// Campo de un nodo del árbol con la carpeta que lo contiene.
const PARENT: &str = "@en";
const ROOT_ORDER: &str = "orden/raiz";
const MACRO_ORDER: &str = "orden/m";
const TUNNEL_ORDER: &str = "orden/t";
const VAULT: &str = "baul";
/// Profundidad máxima del árbol que se reconstruye.
const MAX_DEPTH: usize = 64;
/// Elementos que se admiten en una instantánea ajena.
const MAX_ITEMS: usize = 50_000;
const MAX_KEY: usize = 200;
const MAX_VAULT: u64 = 16 * 1024 * 1024;
const FORMAT: u32 = 1;
/// Instantáneas de las que se recuerda lo acordado.
const MAX_HISTORY: usize = 8;
const MAX_LINEAGE: usize = 128;
const MAX_DEVICES: usize = 32;
/// Copias previas a una sincronización que se conservan.
const MAX_SAFETY_COPIES: usize = 10;
const SAFETY_PREFIX: &str = "antes-de-sincronizar-";

/// Nombre del fichero de la carpeta de sincronización.
pub const SYNC_FILE: &str = "godremote-sync.json";

/// Ajustes que son de cada equipo y no se sincronizan: rutas y programas
/// locales, el servidor X (depende del sistema), el proxy (depende de la
/// red) y el agente SSH de Windows.
pub const LOCAL_SETTINGS: [&str; 6] = [
    "registro",
    "editor",
    "shell_local",
    "servidor_x",
    "proxy",
    "agente",
];

/// Un valor tipado como JSON. Pasa por texto para que los decimales queden
/// igual que al releerlos de un fichero.
fn json<T: Serialize>(value: &T) -> Result<Value, SyncError> {
    serde_json::to_string(value)
        .and_then(|text| serde_json::from_str(&text))
        .map_err(|e| SyncError::Local(format!("no se pudo preparar un elemento: {e}")))
}

/// Huella de un valor (o de su ausencia), para atar una decisión a lo que
/// se decidió.
fn digest(value: Option<&Value>) -> String {
    match value {
        // Sin `preserve_order`, serde_json ordena las claves: el texto es
        // siempre el mismo para el mismo valor.
        Some(v) => fingerprint(v.to_string().as_bytes()),
        None => "-".to_string(),
    }
}

/// Carpeta del árbol, sin sus hijos.
#[derive(Serialize, Deserialize)]
struct FolderItem {
    id: Uuid,
    name: String,
    #[serde(default, skip_serializing_if = "FolderDefaults::is_empty")]
    defaults: FolderDefaults,
}

fn order_key(parent: Option<Uuid>) -> String {
    match parent {
        Some(id) => format!("orden/{id}"),
        None => ROOT_ORDER.to_string(),
    }
}

/// Identificador de una clave `prefijo/<uuid>`.
fn key_id(key: &str, prefix: &str) -> Option<Uuid> {
    key.strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('/'))
        .and_then(|id| Uuid::parse_str(id).ok())
}

fn is_node(key: &str) -> bool {
    key_id(key, "c").is_some() || key_id(key, "s").is_some()
}

fn parent_of(value: &Value) -> Option<Uuid> {
    value
        .get(PARENT)
        .and_then(Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
}

fn set_parent(value: &mut Value, parent: Option<Uuid>) {
    if let Some(object) = value.as_object_mut() {
        object.insert(
            PARENT.to_string(),
            parent.map_or(Value::Null, |id| Value::String(id.to_string())),
        );
    }
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn to_list(ids: Vec<String>) -> Value {
    Value::Array(ids.into_iter().map(Value::String).collect())
}

/// Claves de los ajustes que conoce esta versión.
fn known_settings() -> BTreeSet<String> {
    serde_json::to_value(Settings::default())
        .ok()
        .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect()))
        .unwrap_or_default()
}

/// Los ajustes de fábrica como elementos: la base cuando no hay otra, para
/// que un equipo recién instalado reciba los ajustes de los demás en vez de
/// plantear un conflicto por cada uno.
fn default_settings_items() -> Items {
    let mut items = Items::new();
    if let Ok(Value::Object(object)) = json(&Settings::default()) {
        for (key, value) in object {
            if !LOCAL_SETTINGS.contains(&key.as_str()) {
                items.insert(format!("a/{key}"), value);
            }
        }
    }
    items
}

/// El valor tal como lo escribiría esta versión; `None` si no lo entiende
/// (otra versión, otro tipo de elemento): entonces se conserva sin tocarlo.
fn roundtrip(key: &str, value: &Value) -> Option<Value> {
    if let Some(id) = key_id(key, "c") {
        let item: FolderItem = serde_json::from_value(value.clone()).ok()?;
        (item.id == id).then(|| json(&item).ok()).flatten()
    } else if let Some(id) = key_id(key, "s") {
        let session: Session = serde_json::from_value(value.clone()).ok()?;
        (session.id == id).then(|| json(&session).ok()).flatten()
    } else if let Some(id) = key_id(key, "m") {
        let item: Macro = serde_json::from_value(value.clone()).ok()?;
        (item.id == id).then(|| json(&item).ok()).flatten()
    } else if let Some(id) = key_id(key, "t") {
        let item: SavedTunnel = serde_json::from_value(value.clone()).ok()?;
        (item.id == id).then(|| json(&item).ok()).flatten()
    } else if let Some(name) = key.strip_prefix("tema/") {
        let theme = Theme::from_json(&value.to_string()).ok()?;
        (theme.name == name).then(|| json(&theme).ok()).flatten()
    } else if let Some(setting) = key.strip_prefix("a/") {
        (known_settings().contains(setting) && !LOCAL_SETTINGS.contains(&setting))
            .then(|| value.clone())
    } else if key == VAULT {
        value
            .as_str()
            .filter(|text| crate::vault::looks_valid(text.as_bytes()))
            .map(|_| value.clone())
    } else if key.starts_with("orden/") {
        value
            .as_array()
            .filter(|list| list.iter().all(Value::is_string))
            .map(|_| value.clone())
    } else {
        None
    }
}

/// Lo que hay en la carpeta de datos, como elementos.
struct LocalRead {
    items: Items,
    /// Algún tema no se pudo leer: no se dan por borrados los que falten.
    themes_incomplete: bool,
}

fn local_error(what: &str, e: impl std::fmt::Display) -> SyncError {
    SyncError::Local(format!(
        "{what} no se puede leer ({e}); no se sincroniza para no propagar el fallo"
    ))
}

/// Carpetas de la raíz que no se sincronizan: las compartidas a las que
/// está suscrito este equipo (son de quien las publica).
fn excluded_roots(dir: &Path) -> BTreeSet<Uuid> {
    crate::share::subscribed_roots(dir)
}

fn tree_items(tree: &Tree, excluded: &BTreeSet<Uuid>, items: &mut Items) -> Result<(), SyncError> {
    fn go(
        nodes: &[Node],
        parent: Option<Uuid>,
        excluded: &BTreeSet<Uuid>,
        items: &mut Items,
    ) -> Result<(), SyncError> {
        let mut order = vec![];
        for node in nodes {
            if parent.is_none() && excluded.contains(&node.id()) {
                continue;
            }
            let (key, mut value) = match node {
                Node::Folder(f) => (
                    format!("c/{}", f.id),
                    json(&FolderItem {
                        id: f.id,
                        name: f.name.clone(),
                        defaults: f.defaults.clone(),
                    })?,
                ),
                Node::Session(s) => (format!("s/{}", s.id), json(s)?),
            };
            set_parent(&mut value, parent);
            let id = node.id();
            if items.contains_key(&format!("c/{id}")) || items.contains_key(&format!("s/{id}")) {
                return Err(SyncError::Local(format!(
                    "sessions.json tiene dos elementos con el mismo identificador ({id}); \
                     no se sincroniza"
                )));
            }
            items.insert(key, value);
            order.push(id.to_string());
            if let Node::Folder(f) = node {
                go(&f.children, Some(f.id), excluded, items)?;
            }
        }
        if !order.is_empty() {
            items.insert(order_key(parent), to_list(order));
        }
        Ok(())
    }
    go(&tree.nodes, None, excluded, items)
}

fn read_local(dir: &Path) -> Result<LocalRead, SyncError> {
    let mut items = Items::new();
    let tree =
        Tree::load(&dir.join("sessions.json")).map_err(|e| local_error("sessions.json", e))?;
    tree_items(&tree, &excluded_roots(dir), &mut items)?;

    let macros =
        MacroStore::load(&dir.join("macros.json")).map_err(|e| local_error("macros.json", e))?;
    let mut order = vec![];
    for item in &macros.macros {
        if items
            .insert(format!("m/{}", item.id), json(item)?)
            .is_none()
        {
            order.push(item.id.to_string());
        }
    }
    if !order.is_empty() {
        items.insert(MACRO_ORDER.to_string(), to_list(order));
    }

    // `TunnelStore::load` devuelve una lista vacía si el fichero está mal:
    // aquí eso se confundiría con «los he borrado todos».
    let tunnels: TunnelStore = match std::fs::read(dir.join("tunnels.json")) {
        Ok(data) => serde_json::from_slice(&data).map_err(|e| local_error("tunnels.json", e))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => TunnelStore::default(),
        Err(e) => return Err(local_error("tunnels.json", e)),
    };
    let mut order = vec![];
    for item in &tunnels.tunnels {
        if items
            .insert(format!("t/{}", item.id), json(item)?)
            .is_none()
        {
            order.push(item.id.to_string());
        }
    }
    if !order.is_empty() {
        items.insert(TUNNEL_ORDER.to_string(), to_list(order));
    }

    let themes = ThemeStore::load(&dir.join("themes"));
    for (name, builtin) in themes.names() {
        if let (false, Some(theme)) = (builtin, themes.get(&name)) {
            items.insert(format!("tema/{name}"), json(theme)?);
        }
    }

    let settings_file = dir.join("settings.json");
    match std::fs::read(&settings_file) {
        Ok(data) => {
            // `Settings::load` vuelve a los de fábrica si no lo entiende.
            let valid = serde_json::from_slice::<Value>(&data)
                .ok()
                .is_some_and(|v| serde_json::from_value::<Settings>(v).is_ok());
            if !valid {
                return Err(local_error("settings.json", "no es válido"));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(local_error("settings.json", e)),
    }
    if let Value::Object(object) = json(&Settings::load(&settings_file))? {
        for (key, value) in object {
            if !LOCAL_SETTINGS.contains(&key.as_str()) {
                items.insert(format!("a/{key}"), value);
            }
        }
    }

    match read_limited(&dir.join("vault.json"), MAX_VAULT) {
        Ok(data) => {
            let text = String::from_utf8(data)
                .ok()
                .filter(|text| crate::vault::looks_valid(text.as_bytes()))
                .ok_or_else(|| local_error("vault.json", "no es un baúl válido"))?;
            items.insert(VAULT.to_string(), Value::String(text));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(local_error("vault.json", e)),
    }
    Ok(LocalRead {
        items,
        themes_incomplete: !themes.load_errors.is_empty(),
    })
}

/// Completa lo local con lo que esta versión no sabe representar, tomado
/// de la base: elementos y campos de una versión más reciente no se dan por
/// borrados solo porque aquí no se entiendan.
fn overlay(local: &mut Items, base: &Items, themes_incomplete: bool) {
    for (key, base_value) in base {
        let known = roundtrip(key, base_value);
        match local.get_mut(key) {
            None => {
                // El baúl no se puede borrar desde la aplicación: si falta
                // el fichero, no es un borrado que haya que propagar.
                let keep = known.is_none()
                    || key == VAULT
                    || (themes_incomplete && key.starts_with("tema/"));
                if keep {
                    local.insert(key.clone(), base_value.clone());
                }
            }
            Some(Value::Object(mine)) => {
                let (Some(Value::Object(known)), Value::Object(theirs)) = (&known, base_value)
                else {
                    continue;
                };
                for (field, value) in theirs {
                    if !known.contains_key(field) && !mine.contains_key(field) {
                        mine.insert(field.clone(), value.clone());
                    }
                }
            }
            Some(_) => {}
        }
    }
}

/// Deja el árbol de `items` en un estado que se puede reconstruir: sin
/// padres que no existen, sin ciclos, sin más profundidad de la cuenta y
/// con el orden de los hijos al día. Devuelve cuántos nodos ha tenido que
/// recolocar en la raíz.
fn normalize_tree(items: &mut Items) -> usize {
    let folders: BTreeSet<Uuid> = items.keys().filter_map(|k| key_id(k, "c")).collect();
    let mut moved = 0;
    // Padres que ya no existen (su carpeta se borró en otro equipo).
    for (key, value) in items.iter_mut() {
        if !is_node(key) {
            continue;
        }
        let dangling = match value.get(PARENT) {
            None | Some(Value::Null) => false,
            Some(_) => parent_of(value).is_none_or(|p| !folders.contains(&p)),
        };
        if dangling {
            set_parent(value, None);
            moved += 1;
        } else if value.get(PARENT).is_none() {
            set_parent(value, None);
        }
    }
    // Ciclos (A dentro de B en un equipo y B dentro de A en otro) y cadenas
    // demasiado largas: se corta por la carpeta de identificador más bajo.
    loop {
        let parents: BTreeMap<Uuid, Option<Uuid>> = folders
            .iter()
            .map(|id| (*id, items.get(&format!("c/{id}")).and_then(parent_of)))
            .collect();
        let mut cut = None;
        'search: for start in &folders {
            let mut path = vec![*start];
            let mut current = parents[start];
            while let Some(id) = current {
                if let Some(position) = path.iter().position(|p| *p == id) {
                    cut = path[position..].iter().min().copied();
                    break 'search;
                }
                path.push(id);
                if path.len() > MAX_DEPTH {
                    cut = Some(*start);
                    break 'search;
                }
                current = parents.get(&id).copied().flatten();
            }
        }
        let Some(id) = cut else {
            break;
        };
        if let Some(value) = items.get_mut(&format!("c/{id}")) {
            set_parent(value, None);
            moved += 1;
        }
    }
    // Orden de los hijos de cada carpeta: el que haya, sin los que ya no
    // están y con los nuevos al final (por nombre).
    let mut children: BTreeMap<Option<Uuid>, Vec<(String, String)>> = BTreeMap::new();
    for (key, value) in items.iter() {
        let Some(id) = key_id(key, "c").or_else(|| key_id(key, "s")) else {
            continue;
        };
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase();
        children
            .entry(parent_of(value))
            .or_default()
            .push((name, id.to_string()));
    }
    let stale: Vec<String> = items
        .keys()
        .filter(|k| k.as_str() == ROOT_ORDER || key_id(k, "orden").is_some())
        .filter(|k| !children.contains_key(&key_id(k, "orden")))
        .cloned()
        .collect();
    for key in stale {
        items.remove(&key);
    }
    for (parent, mut members) in children {
        members.sort();
        let key = order_key(parent);
        let order = normalized_order(string_list(items.get(&key)), members);
        items.insert(key, to_list(order));
    }
    for (prefix, key) in [("m", MACRO_ORDER), ("t", TUNNEL_ORDER)] {
        let mut members: Vec<(String, String)> = items
            .iter()
            .filter_map(|(k, v)| {
                let id = key_id(k, prefix)?;
                let name = ["name", "nombre"]
                    .iter()
                    .find_map(|f| v.get(*f).and_then(Value::as_str))
                    .unwrap_or_default()
                    .to_lowercase();
                Some((name, id.to_string()))
            })
            .collect();
        members.sort();
        if members.is_empty() {
            items.remove(key);
        } else {
            let order = normalized_order(string_list(items.get(key)), members);
            items.insert(key.to_string(), to_list(order));
        }
    }
    moved
}

/// `wanted` sin repetidos ni ausentes, y los `members` que falten al final.
fn normalized_order(wanted: Vec<String>, members: Vec<(String, String)>) -> Vec<String> {
    let valid: BTreeSet<&String> = members.iter().map(|(_, id)| id).collect();
    let mut seen = BTreeSet::new();
    let mut order: Vec<String> = wanted
        .into_iter()
        .filter(|id| valid.contains(id) && seen.insert(id.clone()))
        .collect();
    for (_, id) in &members {
        if seen.insert(id.clone()) {
            order.push(id.clone());
        }
    }
    order
}

/// El árbol que describen los elementos (ya normalizados). Lo que esta
/// versión no entiende se deja fuera.
fn build_tree(items: &Items) -> Tree {
    fn children(items: &Items, parent: Option<Uuid>, depth: usize) -> Vec<Node> {
        let mut nodes = vec![];
        if depth > MAX_DEPTH + 1 {
            return nodes;
        }
        for id in string_list(items.get(&order_key(parent))) {
            if let Some(value) = items.get(&format!("c/{id}")) {
                if let Ok(item) = serde_json::from_value::<FolderItem>(value.clone()) {
                    nodes.push(Node::Folder(Folder {
                        id: item.id,
                        name: item.name,
                        defaults: item.defaults,
                        children: children(items, Some(item.id), depth + 1),
                    }));
                }
            } else if let Some(value) = items.get(&format!("s/{id}"))
                && let Ok(session) = serde_json::from_value::<Session>(value.clone())
            {
                nodes.push(Node::Session(session));
            }
        }
        nodes
    }
    let mut tree = Tree {
        nodes: children(items, None, 0),
    };
    tree.migrate();
    tree
}

/// Qué ficheros locales ha cambiado una sincronización.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Applied {
    pub tree: bool,
    pub macros: bool,
    pub tunnels: bool,
    pub themes: bool,
    pub settings: bool,
    pub vault: bool,
}

impl Applied {
    pub fn any(&self) -> bool {
        self.tree || self.macros || self.tunnels || self.themes || self.settings || self.vault
    }
}

fn differs(before: &Items, after: &Items, belongs: impl Fn(&str) -> bool) -> bool {
    let pick = |items: &Items| -> Vec<(String, Value)> {
        items
            .iter()
            .filter(|(k, _)| belongs(k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    };
    pick(before) != pick(after)
}

fn is_tree_key(key: &str) -> bool {
    is_node(key)
        || key == ROOT_ORDER
        || (key.starts_with("orden/") && key != MACRO_ORDER && key != TUNNEL_ORDER)
}

/// Lleva a la carpeta de datos lo que ha cambiado entre `before` y `after`.
/// Solo escribe los ficheros de configuración de siempre.
fn apply_local(
    dir: &Path,
    before: &Items,
    after: &Items,
    notes: &mut Vec<String>,
) -> Result<Applied, SyncError> {
    let io = |what: &str, e: &dyn std::fmt::Display| {
        SyncError::Local(format!("no se pudo escribir {what}: {e}"))
    };
    let mut applied = Applied::default();

    // Los temas, antes que los ajustes que los nombran.
    if differs(before, after, |k| k.starts_with("tema/")) {
        let themes_dir = dir.join("themes");
        let mut store = ThemeStore::load(&themes_dir);
        for (key, value) in after.iter().filter(|(k, _)| k.starts_with("tema/")) {
            if before.get(key) == Some(value) {
                continue;
            }
            let name = &key["tema/".len()..];
            match Theme::from_json(&value.to_string()) {
                Ok(theme) if theme.name == name && !store.is_builtin(name) => {
                    let previous = store.exists(name).then_some(name);
                    store
                        .save_user(theme, previous)
                        .map_err(|e| io("un tema", &format!("{e:#}")))?;
                    applied.themes = true;
                }
                _ => notes.push(format!(
                    "El tema «{name}» no se puede usar en esta versión y se ha dejado como está"
                )),
            }
        }
        for key in before.keys().filter(|k| k.starts_with("tema/")) {
            let name = &key["tema/".len()..];
            if !after.contains_key(key) && store.exists(name) && !store.is_builtin(name) {
                store
                    .delete_user(name)
                    .map_err(|e| io("un tema", &format!("{e:#}")))?;
                applied.themes = true;
            }
        }
    }

    if differs(before, after, is_tree_key) {
        let file = dir.join("sessions.json");
        let mut tree = build_tree(after);
        // Las carpetas compartidas no se sincronizan: siguen donde estaban.
        let excluded = excluded_roots(dir);
        if !excluded.is_empty() {
            let current = Tree::load(&file).map_err(|e| local_error("sessions.json", e))?;
            tree.nodes.extend(
                current
                    .nodes
                    .into_iter()
                    .filter(|n| excluded.contains(&n.id())),
            );
        }
        tree.save(&file)
            .map_err(|e| io("sessions.json", &format!("{e:#}")))?;
        applied.tree = true;
    }

    for (prefix, order, name) in [
        ("m", MACRO_ORDER, "macros.json"),
        ("t", TUNNEL_ORDER, "tunnels.json"),
    ] {
        if !differs(before, after, |k| key_id(k, prefix).is_some() || k == order) {
            continue;
        }
        let ids = string_list(after.get(order));
        let values = ids
            .iter()
            .filter_map(|id| after.get(&format!("{prefix}/{id}")));
        if prefix == "m" {
            let store = MacroStore {
                macros: values
                    .filter_map(|v| serde_json::from_value::<Macro>(v.clone()).ok())
                    .collect(),
            };
            store
                .save(&dir.join(name))
                .map_err(|e| io(name, &format!("{e:#}")))?;
            applied.macros = true;
        } else {
            let store = TunnelStore {
                tunnels: values
                    .filter_map(|v| serde_json::from_value::<SavedTunnel>(v.clone()).ok())
                    .collect(),
            };
            store
                .save(&dir.join(name))
                .map_err(|e| io(name, &format!("{e:#}")))?;
            applied.tunnels = true;
        }
    }

    if differs(before, after, |k| k.starts_with("a/")) {
        let file = dir.join("settings.json");
        let known = known_settings();
        let Value::Object(mut object) = json(&Settings::load(&file))? else {
            return Err(SyncError::Local("los ajustes no son un objeto".into()));
        };
        let mut rejected = vec![];
        for (key, value) in after.iter().filter(|(k, _)| k.starts_with("a/")) {
            let name = &key["a/".len()..];
            if !known.contains(name)
                || LOCAL_SETTINGS.contains(&name)
                || before.get(key) == Some(value)
            {
                continue;
            }
            // Un valor que esta versión no entiende no debe dejar los
            // ajustes sin poder leerse.
            let previous = object.insert(name.to_string(), value.clone());
            if serde_json::from_value::<Settings>(Value::Object(object.clone())).is_err() {
                match previous {
                    Some(previous) => object.insert(name.to_string(), previous),
                    None => object.remove(name),
                };
                rejected.push(name.to_string());
            }
        }
        if !rejected.is_empty() {
            notes.push(format!(
                "Ajustes que esta versión no entiende y se han dejado como estaban: {}",
                rejected.join(", ")
            ));
        }
        let settings: Settings = serde_json::from_value(Value::Object(object))
            .map_err(|e| SyncError::Local(format!("los ajustes recibidos no son válidos: {e}")))?;
        settings
            .save(&file)
            .map_err(|e| io("settings.json", &format!("{e:#}")))?;
        applied.settings = true;
    }

    if let Some(Value::String(text)) = after.get(VAULT)
        && before.get(VAULT) != after.get(VAULT)
    {
        if crate::vault::looks_valid(text.as_bytes()) {
            crate::paths::write_private(&dir.join("vault.json"), text.as_bytes())
                .map_err(|e| io("vault.json", &e))?;
            applied.vault = true;
        } else {
            notes.push("El baúl recibido no es válido y se ha conservado el de este equipo".into());
        }
    }
    Ok(applied)
}

// ----------------------------------------------------------------------
// Fusión a tres bandas
// ----------------------------------------------------------------------

/// Con qué quedarse en un conflicto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    /// Lo de este equipo.
    #[serde(rename = "mio")]
    Mine,
    /// Lo que hay en la carpeta de sincronización.
    #[serde(rename = "carpeta")]
    Theirs,
}

/// Decisión del usuario sobre un conflicto. Vale solo mientras los dos
/// lados sigan como estaban cuando se tomó.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    #[serde(rename = "lado")]
    pub side: Side,
    #[serde(rename = "mio")]
    pub mine: String,
    #[serde(rename = "carpeta")]
    pub theirs: String,
}

/// Un elemento que ha cambiado en los dos lados de forma distinta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    #[serde(rename = "clave")]
    pub key: String,
    /// Qué es, para el usuario: «Equipo «web-01»», «Ajuste «Tema»»…
    #[serde(rename = "titulo")]
    pub title: String,
    /// En qué difieren los dos lados.
    #[serde(rename = "detalle")]
    pub detail: String,
    #[serde(rename = "mio")]
    pub mine: String,
    #[serde(rename = "carpeta")]
    pub theirs: String,
}

impl Conflict {
    pub fn is_vault(&self) -> bool {
        self.key == VAULT
    }

    /// La decisión de quedarse con un lado de este conflicto.
    pub fn decide(&self, side: Side) -> Decision {
        Decision {
            side,
            mine: self.mine.clone(),
            theirs: self.theirs.clone(),
        }
    }
}

/// Resultado de una fusión.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Merge {
    /// Cómo debe quedar este equipo.
    pub local: Items,
    /// Cómo debe quedar la carpeta. Difiere de `local` solo en los
    /// elementos en conflicto: cada lado conserva lo suyo hasta que se
    /// decida.
    pub remote: Items,
    /// Lo acordado si la carpeta queda como `remote`.
    pub agreed: Items,
    /// Lo acordado respecto a lo que había en la carpeta (por si la
    /// escritura no llega o la pisa otro equipo).
    pub agreed_before: Items,
    pub conflicts: Vec<Conflict>,
    /// Elementos que cambian en este equipo y en la carpeta.
    pub pulled: usize,
    pub pushed: usize,
    /// Nodos del árbol que se han tenido que dejar en la raíz.
    pub moved_to_root: usize,
}

enum One {
    Merged(Option<Value>),
    /// Campos que chocan (vacío: el elemento entero).
    Conflict(Vec<String>),
}

fn merge_one(
    base: Option<&Value>,
    local: Option<&Value>,
    remote: Option<&Value>,
    side: Option<Side>,
) -> One {
    if local == remote || remote == base {
        return One::Merged(local.cloned());
    }
    if local == base {
        return One::Merged(remote.cloned());
    }
    // Han cambiado los dos. Si son objetos, campo a campo: dos cambios en
    // campos distintos del mismo equipo no chocan.
    if let (Some(Value::Object(mine)), Some(Value::Object(theirs))) = (local, remote)
        && base.is_none_or(Value::is_object)
    {
        let empty = Map::new();
        let origin = base.and_then(Value::as_object).unwrap_or(&empty);
        let fields: BTreeSet<&String> = mine.keys().chain(theirs.keys()).collect();
        let mut merged = Map::new();
        let mut clashes = vec![];
        for field in fields {
            let (b, l, r) = (origin.get(field), mine.get(field), theirs.get(field));
            let value = if l == r || r == b {
                l
            } else if l == b {
                r
            } else {
                clashes.push(field.clone());
                match side {
                    Some(Side::Theirs) => r,
                    _ => l,
                }
            };
            if let Some(value) = value {
                merged.insert(field.clone(), value.clone());
            }
        }
        return if clashes.is_empty() || side.is_some() {
            One::Merged(Some(Value::Object(merged)))
        } else {
            One::Conflict(clashes)
        };
    }
    match side {
        Some(Side::Mine) => One::Merged(local.cloned()),
        Some(Side::Theirs) => One::Merged(remote.cloned()),
        None => One::Conflict(vec![]),
    }
}

/// El orden de los hijos nunca es un conflicto: si lo cambiaron los dos, se
/// conserva el de este equipo y lo nuevo del otro va al final.
fn merge_order(
    base: Option<&Value>,
    local: Option<&Value>,
    remote: Option<&Value>,
) -> Option<Value> {
    if local == remote || remote == base {
        return local.cloned();
    }
    if local == base || local.is_none() {
        return remote.cloned();
    }
    let mut order = string_list(local);
    for id in string_list(remote) {
        if !order.contains(&id) {
            order.push(id);
        }
    }
    Some(to_list(order))
}

/// Nombre legible de un campo de un equipo o de un ajuste.
fn field_label(field: &str) -> &str {
    match field {
        PARENT => "carpeta",
        "name" | "nombre" => "nombre",
        "port" => "puerto",
        "username" => "usuario",
        "auth" => "autenticación",
        "key_path" => "clave privada",
        "cert_path" => "certificado",
        "protocol" => "protocolo",
        "jumps" => "saltos",
        "tunnels" => "túneles",
        "notes" => "notas",
        "on_connect" => "órdenes al conectar",
        "defaults" => "valores para sus equipos",
        "text" => "texto",
        "atajo" => "atajo",
        "equipo" => "equipo",
        "tunel" => "túnel",
        "autoarranque" => "arranque automático",
        "backdrop" => "fondo",
        "color" => "color",
        other => other,
    }
}

fn short(value: Option<&Value>) -> Option<String> {
    let text = match value? {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "sí".to_string(),
        Value::Bool(false) => "no".to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => "(nada)".to_string(),
        _ => return None,
    };
    let clean: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    Some(if clean.chars().count() > 40 {
        format!("{}…", clean.chars().take(40).collect::<String>())
    } else {
        clean
    })
}

fn conflict_title(key: &str, value: Option<&Value>) -> String {
    let named = |fields: &[&str]| -> String {
        fields
            .iter()
            .find_map(|f| value?.get(*f)?.as_str().filter(|s| !s.trim().is_empty()))
            .unwrap_or("(sin nombre)")
            .chars()
            .filter(|c| !c.is_control())
            .take(80)
            .collect()
    };
    if key_id(key, "s").is_some() {
        format!("Equipo «{}»", named(&["name", "host"]))
    } else if key_id(key, "c").is_some() {
        format!("Carpeta «{}»", named(&["name"]))
    } else if key_id(key, "m").is_some() {
        format!("Snippet «{}»", named(&["name"]))
    } else if key_id(key, "t").is_some() {
        format!("Túnel «{}»", named(&["nombre"]))
    } else if let Some(name) = key.strip_prefix("tema/") {
        format!("Tema «{name}»")
    } else if let Some(name) = key.strip_prefix("a/") {
        format!("Ajuste «{}»", name.replace('_', " "))
    } else if key == VAULT {
        "Baúl de credenciales".to_string()
    } else {
        format!("Elemento «{key}»")
    }
}

fn conflict_detail(
    key: &str,
    local: Option<&Value>,
    remote: Option<&Value>,
    fields: &[String],
) -> String {
    if key == VAULT {
        return "Se han guardado o cambiado credenciales en los dos lados.".to_string();
    }
    match (local, remote) {
        (None, Some(_)) => "Borrado en este equipo y modificado en la carpeta.".to_string(),
        (Some(_), None) => "Modificado en este equipo y borrado en la carpeta.".to_string(),
        _ if fields.is_empty() => match (short(local), short(remote)) {
            (Some(mine), Some(theirs)) => format!("Aquí: {mine} · En la carpeta: {theirs}"),
            _ => "Distinto en los dos lados.".to_string(),
        },
        _ => fields
            .iter()
            .map(|field| {
                let label = field_label(field);
                // La carpeta de destino es un identificador: no dice nada.
                let values = (field != PARENT)
                    .then(|| {
                        Some((
                            short(local.and_then(|v| v.get(field)))?,
                            short(remote.and_then(|v| v.get(field)))?,
                        ))
                    })
                    .flatten();
                match values {
                    Some((mine, theirs)) => {
                        format!("{label}: aquí «{mine}», en la carpeta «{theirs}»")
                    }
                    None => format!("{label}: distinto en los dos lados"),
                }
            })
            .collect::<Vec<_>>()
            .join(" · "),
    }
}

/// Fusión a tres bandas, elemento a elemento. `base` es lo último acordado
/// entre este equipo y la carpeta; `decisions`, lo que el usuario ha
/// contestado sobre conflictos anteriores.
pub fn merge(
    base: &Items,
    local: &Items,
    remote: &Items,
    decisions: &BTreeMap<String, Decision>,
) -> Merge {
    let mut out = Merge::default();
    let keys: BTreeSet<&String> = base
        .keys()
        .chain(local.keys())
        .chain(remote.keys())
        .collect();
    let mut clashing = BTreeSet::new();
    for key in keys {
        let (b, l, r) = (base.get(key), local.get(key), remote.get(key));
        let merged = if key.starts_with("orden/") {
            One::Merged(merge_order(b, l, r))
        } else {
            let side = decisions
                .get(key)
                .filter(|d| d.mine == digest(l) && d.theirs == digest(r))
                .map(|d| d.side);
            merge_one(b, l, r, side)
        };
        match merged {
            One::Merged(value) => {
                if let Some(value) = value {
                    out.local.insert(key.clone(), value.clone());
                    out.remote.insert(key.clone(), value);
                }
            }
            One::Conflict(fields) => {
                clashing.insert(key.clone());
                if let Some(l) = l {
                    out.local.insert(key.clone(), l.clone());
                }
                if let Some(r) = r {
                    out.remote.insert(key.clone(), r.clone());
                }
                out.conflicts.push(Conflict {
                    key: key.clone(),
                    title: conflict_title(key, l.or(r)),
                    detail: conflict_detail(key, l, r, &fields),
                    mine: digest(l),
                    theirs: digest(r),
                });
            }
        }
    }
    out.moved_to_root = normalize_tree(&mut out.local);
    normalize_tree(&mut out.remote);
    // Lo acordado: lo fusionado, salvo en los conflictos, donde sigue
    // valiendo la base hasta que se resuelvan.
    out.agreed = out.local.clone();
    out.agreed_before = remote.clone();
    for key in &clashing {
        for agreed in [&mut out.agreed, &mut out.agreed_before] {
            match base.get(key) {
                Some(value) => agreed.insert(key.clone(), value.clone()),
                None => agreed.remove(key),
            };
        }
    }
    let changed = |before: &Items, after: &Items| {
        before
            .keys()
            .chain(after.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|k| !k.starts_with("orden/") && before.get(*k) != after.get(*k))
            .count()
    };
    out.pulled = changed(local, &out.local);
    out.pushed = changed(remote, &out.remote);
    out
}

/// Cómo combinar dos baúles secreto a secreto.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct VaultPlan {
    /// Secretos que hay que guardar en el baúl de este equipo.
    pub set: BTreeMap<String, String>,
    /// Secretos que hay que quitar (se borraron en el otro lado).
    pub remove: Vec<String>,
    /// Secretos con valores distintos en los dos lados (solo sus nombres).
    pub clashes: Vec<String>,
}

impl Drop for VaultPlan {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        for value in self.set.values_mut() {
            value.zeroize();
        }
    }
}

/// Fusión a tres bandas de los secretos de dos baúles. Sin `base` es una
/// unión: nada se borra. En los que chocan gana `prefer`.
pub fn plan_vault_merge(
    base: Option<&BTreeMap<String, String>>,
    mine: &BTreeMap<String, String>,
    theirs: &BTreeMap<String, String>,
    prefer: Side,
) -> VaultPlan {
    let mut plan = VaultPlan::default();
    let keys: BTreeSet<&String> = mine.keys().chain(theirs.keys()).collect();
    for key in keys {
        let (l, r) = (mine.get(key), theirs.get(key));
        let b = base.and_then(|b| b.get(key));
        if l == r || (base.is_some() && r == b) {
            continue;
        }
        let take_theirs = if (base.is_some() && l == b) || (base.is_none() && l.is_none()) {
            true
        } else if base.is_none() && r.is_none() {
            false
        } else {
            plan.clashes.push(key.clone());
            prefer == Side::Theirs
        };
        if take_theirs {
            match r {
                Some(value) => {
                    plan.set.insert(key.clone(), value.clone());
                }
                None => plan.remove.push(key.clone()),
            }
        }
    }
    plan
}

// ----------------------------------------------------------------------
// Instantáneas, configuración y estado
// ----------------------------------------------------------------------

/// Un equipo que ha escrito en la carpeta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    #[serde(rename = "nombre")]
    pub name: String,
    /// Cuándo escribió por última vez (segundos Unix, según su reloj: solo
    /// se enseña, no decide nada).
    #[serde(rename = "visto")]
    pub seen: i64,
    #[serde(rename = "revision")]
    pub revision: u64,
}

/// Lo que se guarda (cifrado) en la carpeta de sincronización.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(rename = "formato")]
    pub format: u32,
    pub id: Uuid,
    #[serde(rename = "revision")]
    pub revision: u64,
    /// Equipo que la escribió.
    #[serde(rename = "dispositivo")]
    pub device: Uuid,
    /// Instantáneas de las que desciende, de la más reciente a la más
    /// antigua.
    #[serde(default, rename = "antecesores")]
    pub ancestors: Vec<Uuid>,
    #[serde(default, rename = "dispositivos")]
    pub devices: BTreeMap<Uuid, DeviceInfo>,
    #[serde(default, rename = "elementos")]
    pub items: Items,
}

fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .take(60)
        .collect::<String>()
        .trim()
        .to_string()
}

impl Snapshot {
    /// Comprueba y recorta lo que llega de la carpeta.
    fn validate(mut self) -> Result<Self, SyncError> {
        if self.format > FORMAT {
            return Err(SyncError::Seal(SealError::Newer));
        }
        if self.format == 0 {
            return Err(SyncError::Invalid("formato desconocido".into()));
        }
        if self.items.len() > MAX_ITEMS {
            return Err(SyncError::Invalid(format!(
                "tiene {} elementos (el máximo es {MAX_ITEMS})",
                self.items.len()
            )));
        }
        if self.items.keys().any(|k| k.len() > MAX_KEY || k.is_empty()) {
            return Err(SyncError::Invalid("tiene claves no admitidas".into()));
        }
        self.ancestors.truncate(MAX_LINEAGE);
        trim_devices(&mut self.devices);
        for device in self.devices.values_mut() {
            device.name = clean_name(&device.name);
        }
        Ok(self)
    }
}

/// Deja como mucho `MAX_DEVICES`, los de revisión más alta.
fn trim_devices(devices: &mut BTreeMap<Uuid, DeviceInfo>) {
    while devices.len() > MAX_DEVICES {
        let Some(oldest) = devices
            .iter()
            .min_by_key(|(id, d)| (d.revision, **id))
            .map(|(id, _)| *id)
        else {
            break;
        };
        devices.remove(&oldest);
    }
}

/// Preferencias de la sincronización en este equipo (`sync.json`). No se
/// sincronizan: la carpeta puede estar en otra ruta en cada uno.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncConfig {
    #[serde(rename = "activa")]
    pub enabled: bool,
    #[serde(rename = "carpeta")]
    pub folder: String,
    /// Cada cuántos minutos se sincroniza sin que nadie lo pida.
    #[serde(rename = "minutos")]
    pub minutes: u32,
    /// Identificador de este equipo.
    #[serde(rename = "dispositivo")]
    pub device: Uuid,
    #[serde(rename = "nombre")]
    pub name: String,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            folder: String::new(),
            minutes: 5,
            device: Uuid::new_v4(),
            name: default_device_name(),
        }
    }
}

/// Nombre de este equipo, para enseñarlo en los demás.
pub fn default_device_name() -> String {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .filter_map(|var| std::env::var(var).ok())
        .chain(std::fs::read_to_string("/etc/hostname").ok())
        .map(|name| clean_name(&name))
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| "Equipo".to_string())
}

impl SyncConfig {
    pub fn file(dir: &Path) -> PathBuf {
        dir.join("sync.json")
    }

    pub fn load(dir: &Path) -> Self {
        let mut config: Self = std::fs::read(Self::file(dir))
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        config.sanitize();
        config
    }

    pub fn sanitize(&mut self) {
        self.minutes = self.minutes.clamp(1, 24 * 60);
        self.name = clean_name(&self.name);
        if self.name.is_empty() {
            self.name = default_device_name();
        }
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let data = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        crate::paths::write_private(&Self::file(dir), &data)
    }

    /// La carpeta de sincronización, con `~` expandido.
    pub fn folder_path(&self) -> PathBuf {
        crate::paths::expand_tilde(&self.folder)
    }
}

/// Lo acordado con una instantánea de la carpeta.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct BaseEntry {
    id: Uuid,
    #[serde(rename = "elementos")]
    items: Items,
}

/// Lo que este equipo recuerda entre sincronizaciones (`sync-estado.json`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncState {
    /// Lo acordado con las últimas instantáneas, de la más reciente a la
    /// más antigua.
    #[serde(rename = "bases")]
    history: Vec<BaseEntry>,
    /// Instantáneas cuyo contenido ya está incorporado en este equipo.
    #[serde(rename = "linaje")]
    lineage: Vec<Uuid>,
    /// Respuestas del usuario a conflictos, a la espera de aplicarse.
    #[serde(rename = "decisiones")]
    pub decisions: BTreeMap<String, Decision>,
    /// Última sincronización completada (segundos Unix; solo se enseña).
    #[serde(rename = "ultima")]
    pub last_sync: Option<i64>,
    #[serde(rename = "revision")]
    pub revision: u64,
    #[serde(rename = "dispositivos")]
    pub devices: BTreeMap<Uuid, DeviceInfo>,
    /// Conflictos pendientes tras la última sincronización.
    #[serde(rename = "conflictos")]
    pub conflicts: Vec<Conflict>,
}

impl SyncState {
    pub fn file(dir: &Path) -> PathBuf {
        dir.join("sync-estado.json")
    }

    pub fn load(dir: &Path) -> Self {
        read_limited(&Self::file(dir), 4 * MAX_PLAIN)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let data = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        crate::paths::write_private(&Self::file(dir), &data)
    }

    /// Anota con qué quedarse en un conflicto; se aplica en la siguiente
    /// sincronización.
    pub fn decide(&mut self, conflict: &Conflict, side: Side) {
        self.decisions
            .insert(conflict.key.clone(), conflict.decide(side));
    }

    /// Lo acordado con la instantánea más reciente de la que desciende
    /// `remote`, si se recuerda alguna.
    fn base_for(&self, remote: &Snapshot) -> Option<&Items> {
        self.history
            .iter()
            .find(|entry| entry.id == remote.id || remote.ancestors.contains(&entry.id))
            .map(|entry| &entry.items)
    }

    fn remember(&mut self, id: Uuid, items: Items) {
        self.history.retain(|entry| entry.id != id);
        self.history.insert(0, BaseEntry { id, items });
        self.history.truncate(MAX_HISTORY);
        self.lineage.retain(|known| *known != id);
        self.lineage.insert(0, id);
        self.lineage.truncate(MAX_LINEAGE);
    }

    /// Tras combinar en el baúl de este equipo los secretos del de la
    /// carpeta (`theirs`), el conflicto del baúl queda decidido: vale el de
    /// este equipo, que ya lo tiene todo.
    pub fn keep_merged_vault(&mut self, dir: &Path, theirs: &str) -> std::io::Result<()> {
        let mine = String::from_utf8(read_limited(&dir.join("vault.json"), MAX_VAULT)?)
            .map_err(std::io::Error::other)?;
        self.decisions.insert(
            VAULT.to_string(),
            Decision {
                side: Side::Mine,
                mine: digest(Some(&Value::String(mine))),
                theirs: digest(Some(&Value::String(theirs.to_string()))),
            },
        );
        Ok(())
    }

    /// El contenido del baúl acordado por última vez (cifrado, como en
    /// disco), para combinar dos baúles secreto a secreto.
    pub fn vault_base(&self) -> Option<&str> {
        self.history
            .first()
            .and_then(|entry| entry.items.get(VAULT))
            .and_then(Value::as_str)
    }
}

/// Por qué no se ha podido sincronizar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncError {
    /// La carpeta no está (unidad sin montar, sin conexión…): se reintenta.
    Unavailable(String),
    Seal(SealError),
    /// Lo que hay en la carpeta descifra pero no es admisible.
    Invalid(String),
    /// La configuración de este equipo no se puede leer o escribir.
    Local(String),
    /// La carpeta cambió entre que se leyó y se iba a escribir.
    Changed,
    Io(String),
}

impl SyncError {
    /// `true` si lo normal es que se arregle solo al reintentar.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            SyncError::Unavailable(_)
                | SyncError::Changed
                | SyncError::Io(_)
                | SyncError::Seal(SealError::Incomplete)
        )
    }
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Unavailable(e) => {
                write!(f, "La carpeta de sincronización no está disponible ({e})")
            }
            SyncError::Seal(SealError::WrongPassword) => {
                write!(
                    f,
                    "La contraseña de sincronización no es la de esta carpeta"
                )
            }
            SyncError::Seal(e) => write!(f, "{e}"),
            SyncError::Invalid(e) => write!(
                f,
                "Lo que hay en la carpeta no es válido ({e}); no se ha tocado nada"
            ),
            SyncError::Local(e) => write!(f, "{e}"),
            SyncError::Changed => write!(
                f,
                "Otro equipo ha escrito en la carpeta a la vez; se vuelve a intentar"
            ),
            SyncError::Io(e) => write!(f, "No se pudo escribir en la carpeta ({e})"),
        }
    }
}

impl std::error::Error for SyncError {}

impl From<SealError> for SyncError {
    fn from(e: SealError) -> Self {
        SyncError::Seal(e)
    }
}

/// Lo leído de la carpeta de sincronización.
#[derive(Debug, Clone, Default)]
pub struct Remote {
    /// `None`: todavía no hay nada en la carpeta.
    pub snapshot: Option<Snapshot>,
    /// Huella del fichero leído, para no pisar lo que llegue después.
    pub fingerprint: Option<String>,
    /// Sal con la que estaba cifrado (se reutiliza al escribir).
    pub params: Option<SealParams>,
    /// Copias «en conflicto» que ha dejado el servicio que replica la
    /// carpeta. No hacen falta: cada equipo reenvía lo suyo.
    pub copies: Vec<String>,
}

fn folder_error(folder: &Path, e: &std::io::Error) -> SyncError {
    SyncError::Unavailable(format!("{}: {e}", folder.display()))
}

/// Ficheros de la carpeta que parecen copias del nuestro hechas por el
/// servicio que la replica («copia en conflicto», `.sync-conflict-`…).
fn conflict_copies(folder: &Path) -> Vec<String> {
    let stem = SYNC_FILE.trim_end_matches(".json");
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|name| name.starts_with(stem) && name != SYNC_FILE)
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Lee y descifra lo que hay en la carpeta. No toca nada.
pub fn read_folder(folder: &Path, keys: &mut Keyring) -> Result<Remote, SyncError> {
    match std::fs::metadata(folder) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(SyncError::Unavailable("no es una carpeta".into())),
        Err(e) => return Err(folder_error(folder, &e)),
    }
    let copies = conflict_copies(folder);
    let raw = match read_limited(&folder.join(SYNC_FILE), MAX_SEALED) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Remote {
                copies,
                ..Default::default()
            });
        }
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            return Err(SyncError::Seal(SealError::TooBig));
        }
        Err(e) => return Err(folder_error(folder, &e)),
    };
    let (plain, params) = open(SealKind::Sync, keys, &raw)?;
    let snapshot: Snapshot = serde_json::from_slice(&plain)
        .map_err(|e| SyncError::Invalid(format!("no se entiende: {e}")))?;
    Ok(Remote {
        snapshot: Some(snapshot.validate()?),
        fingerprint: Some(fingerprint(&raw)),
        params: Some(params),
        copies,
    })
}

/// Qué debe haber en la carpeta para poder escribir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// Lo mismo que se leyó (la huella del fichero, o que no hubiera).
    Unchanged(Option<String>),
    /// Da igual: se sustituye lo que haya.
    Anything,
}

/// Cifra y escribe una instantánea en la carpeta. Devuelve la huella del
/// fichero escrito.
pub fn write_folder(
    folder: &Path,
    snapshot: &Snapshot,
    keys: &mut Keyring,
    params: Option<&SealParams>,
    expect: &Expect,
) -> Result<String, SyncError> {
    if !folder.is_dir() {
        return Err(SyncError::Unavailable(format!(
            "{} no existe",
            folder.display()
        )));
    }
    let plain =
        Zeroizing::new(serde_json::to_vec(snapshot).map_err(|e| SyncError::Io(e.to_string()))?);
    if plain.len() as u64 > MAX_PLAIN {
        return Err(SyncError::Local(
            "la configuración es demasiado grande para sincronizarla".into(),
        ));
    }
    // La misma sal que había, si no rebaja los costes: así los demás
    // equipos no tienen que derivar otra clave.
    let params = SealParams::reuse(params);
    let sealed = seal(SealKind::Sync, keys, &params, &plain)?;
    let path = folder.join(SYNC_FILE);
    if let Expect::Unchanged(expected) = expect {
        let current = match read_limited(&path, MAX_SEALED) {
            Ok(raw) => Some(fingerprint(&raw)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(folder_error(folder, &e)),
        };
        if current != *expected {
            return Err(SyncError::Changed);
        }
    }
    write_atomic(&path, &sealed, &snapshot.device.to_string())
        .map_err(|e| SyncError::Io(e.to_string()))?;
    Ok(fingerprint(&sealed))
}

/// Resultado de fusionar lo local con lo leído de la carpeta.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    /// Instantánea que hay que escribir en la carpeta, si algo ha cambiado.
    pub write: Option<Snapshot>,
    /// Ficheros locales que se han cambiado.
    pub applied: Applied,
    pub conflicts: Vec<Conflict>,
    pub notes: Vec<String>,
    /// Elementos traídos de la carpeta y enviados a ella.
    pub pulled: usize,
    pub pushed: usize,
    /// Copia de la configuración local hecha antes de cambiarla.
    pub safety_copy: Option<PathBuf>,
    /// El baúl de la carpeta, si está en conflicto con el de este equipo.
    pub their_vault: Option<String>,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Guarda la configuración local antes de cambiarla, como hace la
/// restauración de copias, y poda las copias antiguas.
pub fn safety_copy(dir: &Path) -> Result<Option<PathBuf>, SyncError> {
    let Ok(backup) = crate::backup::Backup::create(dir, false) else {
        // Todavía no hay nada que guardar.
        return Ok(None);
    };
    // Con un contador, por si se hacen dos en el mismo instante.
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let path = dir.join(format!(
        "{SAFETY_PREFIX}{}-{:04}.json",
        chrono::Local::now().format("%Y%m%d-%H%M%S%.3f"),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 10_000
    ));
    backup.write(&path).map_err(|e| {
        SyncError::Local(format!(
            "no se pudo guardar la copia previa ({e:#}); no se ha cambiado nada"
        ))
    })?;
    let mut copies: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with(SAFETY_PREFIX) && n.ends_with(".json"))
                })
                .collect()
        })
        .unwrap_or_default();
    copies.sort();
    let extra = copies.len().saturating_sub(MAX_SAFETY_COPIES);
    for old in copies.into_iter().take(extra) {
        let _ = std::fs::remove_file(old);
    }
    Ok(Some(path))
}

/// Fusiona la configuración de la carpeta de datos `dir` con lo leído de la
/// carpeta de sincronización: aplica en este equipo lo que haya que traer
/// (tras guardar una copia) y devuelve lo que haya que escribir.
///
/// Con `remote.snapshot == None` (carpeta vacía, o se quiere reescribir) se
/// envía lo de este equipo tal cual.
pub fn merge_local(
    dir: &Path,
    config: &SyncConfig,
    state: &mut SyncState,
    remote: &Remote,
) -> Result<Outcome, SyncError> {
    let read = read_local(dir)?;
    let mut before = state.clone();
    let mut outcome = Outcome::default();
    let mut local = read.items;
    // Lo que hay que escribir en la carpeta y lo acordado si llega.
    let mut to_write: Option<(Items, Items)> = None;
    let mut ancestors: Vec<Uuid> = vec![];
    let mut devices = state.devices.clone();
    match &remote.snapshot {
        Some(snapshot) => {
            let base = state
                .base_for(snapshot)
                .cloned()
                .unwrap_or_else(default_settings_items);
            overlay(&mut local, &base, read.themes_incomplete);
            let merged = merge(&base, &local, &snapshot.items, &state.decisions);
            if merged.local != local {
                outcome.safety_copy = safety_copy(dir)?;
                outcome.applied = apply_local(dir, &local, &merged.local, &mut outcome.notes)?;
            }
            if merged.moved_to_root > 0 {
                outcome.notes.push(format!(
                    "{} elemento(s) se han dejado en la raíz: su carpeta se borró o se movió \
                     en otro equipo",
                    merged.moved_to_root
                ));
            }
            if merged.conflicts.iter().any(Conflict::is_vault) {
                outcome.their_vault = snapshot
                    .items
                    .get(VAULT)
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            outcome.pulled = merged.pulled;
            outcome.pushed = merged.pushed;
            outcome.conflicts = merged.conflicts;
            state.remember(snapshot.id, merged.agreed_before);
            state.revision = state.revision.max(snapshot.revision);
            ancestors.push(snapshot.id);
            ancestors.extend(&snapshot.ancestors);
            devices = snapshot.devices.clone();
            if merged.remote != snapshot.items {
                to_write = Some((merged.remote, merged.agreed));
            }
        }
        None => {
            normalize_tree(&mut local);
            outcome.pushed = local.keys().filter(|k| !k.starts_with("orden/")).count();
            to_write = Some((local.clone(), local));
        }
    }
    if let Some((items, agreed)) = to_write {
        let id = Uuid::new_v4();
        // Desciende de lo que había en la carpeta y de todo lo que este
        // equipo ya tenía incorporado.
        for known in &state.lineage {
            if !ancestors.contains(known) {
                ancestors.push(*known);
            }
        }
        ancestors.truncate(MAX_LINEAGE);
        state.revision += 1;
        devices.insert(
            config.device,
            DeviceInfo {
                name: clean_name(&config.name),
                seen: now(),
                revision: state.revision,
            },
        );
        trim_devices(&mut devices);
        state.remember(id, agreed);
        outcome.write = Some(Snapshot {
            format: FORMAT,
            id,
            revision: state.revision,
            device: config.device,
            ancestors,
            devices: devices.clone(),
            items,
        });
    }
    state.devices = devices;
    state.decisions.clear();
    state.conflicts = outcome.conflicts.clone();
    // Si no ha cambiado nada no se reescribe el estado (puede ser grande):
    // la hora de la última sincronización se pone al día en la siguiente.
    before.last_sync = state.last_sync;
    if *state != before || state.last_sync.is_none() {
        state.last_sync = Some(now());
        state
            .save(dir)
            .map_err(|e| SyncError::Local(format!("no se pudo guardar el estado: {e}")))?;
    }
    state.last_sync = Some(now());
    Ok(outcome)
}

/// Una sincronización completa, de una vez: leer la carpeta, fusionar y
/// escribir. (La interfaz hace los tres pasos por separado para no parar
/// la ventana mientras se lee o se escribe.)
pub fn sync_once(
    dir: &Path,
    config: &SyncConfig,
    state: &mut SyncState,
    keys: &mut Keyring,
) -> Result<Outcome, SyncError> {
    let folder = config.folder_path();
    let remote = read_folder(&folder, keys)?;
    let outcome = merge_local(dir, config, state, &remote)?;
    if let Some(snapshot) = &outcome.write {
        write_folder(
            &folder,
            snapshot,
            keys,
            remote.params.as_ref(),
            &Expect::Unchanged(remote.fingerprint.clone()),
        )?;
    }
    Ok(outcome)
}

/// Tamaño y fecha de un fichero: lo que cuesta poco mirar para saber si ha
/// cambiado. (La fecha solo se compara consigo misma.)
pub type Stamp = Option<(u64, std::time::SystemTime)>;

pub fn stamp(path: &Path) -> Stamp {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// Tamaños y fechas de los ficheros locales que se sincronizan, para notar
/// que algo ha cambiado sin leerlos enteros.
pub fn local_stamp(dir: &Path) -> Vec<(String, Stamp)> {
    let mut stamps: Vec<(String, Stamp)> = [
        "sessions.json",
        "macros.json",
        "tunnels.json",
        "settings.json",
        "vault.json",
    ]
    .iter()
    .map(|name| (name.to_string(), stamp(&dir.join(name))))
    .collect();
    let mut themes: Vec<(String, Stamp)> = std::fs::read_dir(dir.join("themes"))
        .map(|entries| {
            entries
                .flatten()
                .map(|e| (format!("themes/{:?}", e.file_name()), stamp(&e.path())))
                .collect()
        })
        .unwrap_or_default();
    themes.sort_by(|a, b| a.0.cmp(&b.0));
    stamps.extend(themes);
    stamps
}

/// Borra de la carpeta las copias «en conflicto» que dejó el servicio que
/// la replica. Devuelve cuántas.
pub fn remove_conflict_copies(folder: &Path) -> usize {
    conflict_copies(folder)
        .iter()
        .filter(|name| std::fs::remove_file(folder.join(name)).is_ok())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str) -> Session {
        Session {
            name: name.into(),
            host: format!("{name}.example"),
            username: "root".into(),
            ..Default::default()
        }
    }

    fn items(tree: &Tree) -> Items {
        let mut items = Items::new();
        tree_items(tree, &BTreeSet::new(), &mut items).unwrap();
        items
    }

    fn none() -> BTreeMap<String, Decision> {
        BTreeMap::new()
    }

    fn edit(tree: &mut Tree, id: Uuid, change: impl FnOnce(&mut Session)) {
        match tree.find_mut(id) {
            Some(Node::Session(s)) => change(s),
            _ => panic!("no está el equipo"),
        }
    }

    fn names(tree: &Tree) -> Vec<String> {
        tree.session_paths().into_iter().map(|(_, p)| p).collect()
    }

    // --- Formato cifrado -------------------------------------------------

    #[test]
    fn sealed_files_roundtrip_and_show_nothing() {
        let mut keys = Keyring::new("contraseña de sincronización");
        let params = SealParams::fresh();
        let plain = br#"{"equipo":"web-01.interno","usuario":"administrador"}"#;
        let sealed = seal(SealKind::Sync, &mut keys, &params, plain).unwrap();
        let text = String::from_utf8(sealed.clone()).unwrap();
        for secret in ["web-01", "administrador", "equipo", "contraseña"] {
            assert!(!text.contains(secret), "{secret} se lee en el fichero");
        }
        let (opened, used) = open(SealKind::Sync, &mut keys, &sealed).unwrap();
        assert_eq!(&opened[..], plain);
        assert_eq!(used, params);
        // Cada escritura lleva su nonce: dos cifrados del mismo texto no se
        // parecen.
        let again = seal(SealKind::Sync, &mut keys, &params, plain).unwrap();
        assert_ne!(again, sealed);

        // Contraseña equivocada: se distingue de un fichero dañado.
        let mut wrong = Keyring::new("otra");
        assert_eq!(
            open(SealKind::Sync, &mut wrong, &sealed).unwrap_err(),
            SealError::WrongPassword
        );
        // Un fichero de carpeta compartida no vale como sincronización.
        assert_eq!(
            open(SealKind::Share, &mut keys, &sealed).unwrap_err(),
            SealError::Corrupt
        );
        let share = seal(SealKind::Share, &mut keys, &params, plain).unwrap();
        assert!(open(SealKind::Share, &mut keys, &share).is_ok());
        assert!(open(SealKind::Sync, &mut keys, &share).is_err());
    }

    #[test]
    fn truncated_or_tampered_files_are_rejected() {
        let mut keys = Keyring::new("clave");
        let params = SealParams::fresh();
        let sealed = seal(SealKind::Sync, &mut keys, &params, b"contenido de prueba").unwrap();
        // A medio escribir: vacío, con ceros o cortado por cualquier sitio.
        for partial in [&b""[..], &[0u8; 64][..], &b"  \n"[..]] {
            assert_eq!(
                open(SealKind::Sync, &mut keys, partial).unwrap_err(),
                SealError::Incomplete
            );
        }
        for cut in [1, 10, sealed.len() / 2, sealed.len() - 1] {
            assert_eq!(
                open(SealKind::Sync, &mut keys, &sealed[..cut]).unwrap_err(),
                SealError::Incomplete,
                "cortado en {cut}"
            );
        }
        assert_eq!(
            open(SealKind::Sync, &mut keys, b"esto no es nuestro").unwrap_err(),
            SealError::Corrupt
        );

        // Manipulado: cualquier campo que se toque se nota.
        let tamper = |change: &dyn Fn(&mut SealedFile)| {
            let mut file: SealedFile = serde_json::from_slice(&sealed).unwrap();
            change(&mut file);
            serde_json::to_vec(&file).unwrap()
        };
        let flip = |text: &str| {
            let mut bytes = B64.decode(text).unwrap();
            bytes[0] ^= 1;
            B64.encode(bytes)
        };
        let data = tamper(&|f| f.data = flip(&f.data));
        assert_eq!(
            open(SealKind::Sync, &mut keys, &data).unwrap_err(),
            SealError::Corrupt
        );
        let nonce = tamper(&|f| f.nonce = flip(&f.nonce));
        assert_eq!(
            open(SealKind::Sync, &mut keys, &nonce).unwrap_err(),
            SealError::Corrupt
        );
        // Otra sal u otra comprobación: la clave ya no es la del fichero.
        let salt = tamper(&|f| f.salt = flip(&f.salt));
        assert_eq!(
            open(SealKind::Sync, &mut keys, &salt).unwrap_err(),
            SealError::WrongPassword
        );
        let check = tamper(&|f| f.check = flip(&f.check));
        assert_eq!(
            open(SealKind::Sync, &mut keys, &check).unwrap_err(),
            SealError::WrongPassword
        );
        // Truncar el texto cifrado (base64 válido, pero más corto).
        let short = tamper(&|f| {
            let bytes = B64.decode(&f.data).unwrap();
            f.data = B64.encode(&bytes[..bytes.len() - 5]);
        });
        assert_eq!(
            open(SealKind::Sync, &mut keys, &short).unwrap_err(),
            SealError::Corrupt
        );
        // Versión futura, y costes con los que no se intenta ni derivar.
        let newer = tamper(&|f| f.version = 9);
        assert_eq!(
            open(SealKind::Sync, &mut keys, &newer).unwrap_err(),
            SealError::Newer
        );
        for (m, t, p) in [
            (u32::MAX, 3, 1),
            (65536, 1000, 1),
            (65536, 3, 200),
            (0, 3, 1),
        ] {
            let greedy = tamper(&|f| (f.m_cost, f.t_cost, f.p_cost) = (m, t, p));
            let started = std::time::Instant::now();
            assert_eq!(
                open(SealKind::Sync, &mut keys, &greedy).unwrap_err(),
                SealError::Corrupt
            );
            assert!(started.elapsed() < std::time::Duration::from_secs(2));
        }
    }

    #[test]
    fn oversized_contents_are_refused_when_opening() {
        // Mucho más grande descomprimido que en disco: no se expande entero.
        let mut keys = Keyring::new("clave");
        let big = vec![b' '; MAX_PLAIN as usize + 1];
        let sealed = seal(SealKind::Sync, &mut keys, &SealParams::fresh(), &big).unwrap();
        assert!(sealed.len() < 1024 * 1024);
        assert_eq!(
            open(SealKind::Sync, &mut keys, &sealed).unwrap_err(),
            SealError::TooBig
        );
    }

    // --- Elementos -------------------------------------------------------

    #[test]
    fn the_tree_survives_the_trip_through_items() {
        let mut tree = Tree::default();
        let loose = host("suelto");
        tree.upsert_session(None, loose.clone());
        let prod = tree.add_folder(None, "Producción");
        let web = tree.add_folder(Some(prod), "Web");
        if let Some(Node::Folder(f)) = tree.find_mut(prod) {
            f.defaults.username = "deploy".into();
            f.defaults.jumps = Some(vec![loose.id]);
        }
        for name in ["zeta", "alfa", "medio"] {
            tree.upsert_session(Some(web), host(name));
        }
        tree.add_folder(Some(prod), "Vacía");
        let mut flat = items(&tree);
        assert_eq!(normalize_tree(&mut flat), 0);
        assert_eq!(flat, items(&tree), "normalizar no cambia un árbol sano");
        // Mismo árbol, con el mismo orden (que no es el alfabético).
        assert_eq!(build_tree(&flat), tree);
        assert_eq!(
            names(&build_tree(&flat))[1..],
            [
                "Producción / Web / zeta",
                "Producción / Web / alfa",
                "Producción / Web / medio"
            ]
        );
        // Dos nodos con el mismo identificador no se sincronizan.
        let mut twice = tree.clone();
        twice.nodes.push(Node::Session(loose));
        let mut out = Items::new();
        assert!(matches!(
            tree_items(&twice, &BTreeSet::new(), &mut out),
            Err(SyncError::Local(_))
        ));
    }

    #[test]
    fn broken_trees_are_repaired_not_lost() {
        let mut tree = Tree::default();
        let a = tree.add_folder(None, "A");
        let b = tree.add_folder(None, "B");
        let s = host("dentro");
        tree.upsert_session(Some(a), s.clone());
        let mut flat = items(&tree);
        // Un ciclo (A dentro de B y B dentro de A) y un padre que no existe.
        set_parent(flat.get_mut(&format!("c/{a}")).unwrap(), Some(b));
        set_parent(flat.get_mut(&format!("c/{b}")).unwrap(), Some(a));
        let orphan = host("huérfano");
        let mut value = json(&orphan).unwrap();
        set_parent(&mut value, Some(Uuid::new_v4()));
        flat.insert(format!("s/{}", orphan.id), value);
        assert_eq!(normalize_tree(&mut flat), 2);
        let rebuilt = build_tree(&flat);
        // Están todos: las dos carpetas (una dentro de la otra) y los dos
        // equipos.
        assert!(rebuilt.find(a).is_some() && rebuilt.find(b).is_some());
        assert!(rebuilt.is_inside(a, s.id));
        assert_eq!(rebuilt.parent_of(orphan.id), None);
        assert!(rebuilt.session(orphan.id).is_some());
        assert_eq!(rebuilt.session_paths().len(), 2);
        // Y lo reparado ya no cambia.
        assert_eq!(items(&rebuilt), flat);

        // Una cadena de carpetas más larga de la cuenta se corta.
        let mut deep = Items::new();
        let ids: Vec<Uuid> = (0..MAX_DEPTH + 20).map(|_| Uuid::new_v4()).collect();
        for (index, id) in ids.iter().enumerate() {
            let mut value = json(&FolderItem {
                id: *id,
                name: format!("n{index}"),
                defaults: Default::default(),
            })
            .unwrap();
            set_parent(&mut value, index.checked_sub(1).map(|i| ids[i]));
            deep.insert(format!("c/{id}"), value);
        }
        assert!(normalize_tree(&mut deep) >= 1);
        let rebuilt = build_tree(&deep);
        assert_eq!(rebuilt.folder_paths().len(), ids.len());
    }

    // --- Fusión a tres bandas -------------------------------------------

    #[test]
    fn changes_on_one_side_win_and_deletions_propagate() {
        let mut base = Tree::default();
        let folder = base.add_folder(None, "Producción");
        let (a, b, c, d) = (host("a"), host("b"), host("c"), host("d"));
        for s in [&a, &b, &c, &d] {
            base.upsert_session(Some(folder), s.clone());
        }
        // Aquí: se renombra «a», se borra «b» y se añade «nuevo-aquí».
        let mut local = base.clone();
        edit(&mut local, a.id, |s| s.name = "a-renombrado".into());
        local.remove(b.id);
        local.upsert_session(None, host("nuevo-aquí"));
        // En la carpeta: cambia el puerto de «c», se borra «d», se añade
        // «nuevo-allí».
        let mut remote = base.clone();
        edit(&mut remote, c.id, |s| s.port = 2222);
        remote.remove(d.id);
        remote.upsert_session(Some(folder), host("nuevo-allí"));

        let merged = merge(&items(&base), &items(&local), &items(&remote), &none());
        assert!(merged.conflicts.is_empty(), "{:?}", merged.conflicts);
        assert_eq!(merged.local, merged.remote);
        assert_eq!(merged.agreed, merged.local);
        let tree = build_tree(&merged.local);
        assert_eq!(
            names(&tree),
            [
                "Producción / a-renombrado",
                "Producción / c",
                "Producción / nuevo-allí",
                "nuevo-aquí"
            ]
        );
        assert_eq!(tree.session(c.id).unwrap().port, 2222);
        // Tres elementos cambian en cada lado (cambio, borrado y alta).
        assert_eq!((merged.pulled, merged.pushed), (3, 3));

        // Los borrados no resucitan: un equipo que aún tiene «b» y «d» sin
        // tocar (los conoce por la base) los pierde al fusionar.
        let stale = merge(&items(&base), &items(&base), &merged.remote, &none());
        assert!(stale.conflicts.is_empty());
        let tree = build_tree(&stale.local);
        assert!(tree.session(b.id).is_none() && tree.session(d.id).is_none());
        assert_eq!(stale.pushed, 0);
        // Y con lo acordado como base, otra vuelta no cambia nada.
        let again = merge(&merged.agreed, &merged.local, &merged.remote, &none());
        assert_eq!((again.pulled, again.pushed), (0, 0));
        assert_eq!(again.local, merged.local);
    }

    #[test]
    fn the_same_change_on_both_sides_is_not_a_conflict() {
        let mut base = Tree::default();
        let a = host("a");
        let b = host("b");
        base.upsert_session(None, a.clone());
        base.upsert_session(None, b.clone());
        let mut both = base.clone();
        edit(&mut both, a.id, |s| s.host = "nuevo.example".into());
        both.remove(b.id);
        let merged = merge(&items(&base), &items(&both), &items(&both), &none());
        assert!(merged.conflicts.is_empty());
        assert_eq!((merged.pulled, merged.pushed), (0, 0));
        assert_eq!(build_tree(&merged.local), both);
    }

    #[test]
    fn different_fields_of_one_host_merge_and_the_same_field_conflicts() {
        let mut base = Tree::default();
        let a = host("a");
        base.upsert_session(None, a.clone());
        let mut local = base.clone();
        edit(&mut local, a.id, |s| {
            s.notes = "apuntado aquí".into();
            s.port = 2200;
        });
        let mut remote = base.clone();
        edit(&mut remote, a.id, |s| s.username = "deploy".into());
        // Campos distintos: se quedan los dos cambios.
        let merged = merge(&items(&base), &items(&local), &items(&remote), &none());
        assert!(merged.conflicts.is_empty());
        let s = build_tree(&merged.local).session(a.id).unwrap().clone();
        assert_eq!(
            (s.notes.as_str(), s.port, s.username.as_str()),
            ("apuntado aquí", 2200, "deploy")
        );
        assert_eq!(merged.local, merged.remote);

        // El mismo campo con valores distintos: conflicto, y nadie pierde
        // lo suyo mientras no se decida.
        edit(&mut remote, a.id, |s| s.port = 2222);
        let (b, l, r) = (items(&base), items(&local), items(&remote));
        let merged = merge(&b, &l, &r, &none());
        assert_eq!(merged.conflicts.len(), 1);
        let conflict = &merged.conflicts[0];
        assert_eq!(conflict.title, "Equipo «a»");
        assert_eq!(conflict.detail, "puerto: aquí «2200», en la carpeta «2222»");
        assert_eq!(merged.local, l);
        assert_eq!(merged.remote, r);
        assert_eq!((merged.pulled, merged.pushed), (0, 0));
        // Lo acordado sigue siendo la base: el conflicto vuelve a salir la
        // próxima vez, no se resuelve solo.
        assert_eq!(merged.agreed, b);
        assert_eq!(merged.agreed_before, b);
        let again = merge(&merged.agreed, &merged.local, &merged.remote, &none());
        assert_eq!(again.conflicts, merged.conflicts);

        // «Lo mío»: mi puerto, y aun así entra el usuario del otro lado.
        let mut decisions = BTreeMap::new();
        decisions.insert(conflict.key.clone(), conflict.decide(Side::Mine));
        let mine = merge(&b, &l, &r, &decisions);
        assert!(mine.conflicts.is_empty());
        let s = build_tree(&mine.local).session(a.id).unwrap().clone();
        assert_eq!((s.port, s.username.as_str()), (2200, "deploy"));
        assert_eq!(mine.local, mine.remote);
        // «Lo de la carpeta».
        decisions.insert(conflict.key.clone(), conflict.decide(Side::Theirs));
        let theirs = merge(&b, &l, &r, &decisions);
        let s = build_tree(&theirs.local).session(a.id).unwrap().clone();
        assert_eq!((s.port, s.notes.as_str()), (2222, "apuntado aquí"));
        // Una decisión tomada sobre otros valores ya no vale.
        edit(&mut remote, a.id, |s| s.port = 2223);
        let stale = merge(&b, &l, &items(&remote), &decisions);
        assert_eq!(stale.conflicts.len(), 1);
        assert_ne!(stale.conflicts[0].theirs, conflict.theirs);
    }

    #[test]
    fn deleting_what_the_other_side_changed_is_a_conflict() {
        let mut base = Tree::default();
        let a = host("a");
        let b = host("b");
        base.upsert_session(None, a.clone());
        base.upsert_session(None, b.clone());
        // Aquí se borra «a» y se cambia «b»; en la carpeta, al revés.
        let mut local = base.clone();
        local.remove(a.id);
        edit(&mut local, b.id, |s| s.notes = "importante".into());
        let mut remote = base.clone();
        edit(&mut remote, a.id, |s| s.host = "otro.example".into());
        remote.remove(b.id);
        let (bi, l, r) = (items(&base), items(&local), items(&remote));
        let merged = merge(&bi, &l, &r, &none());
        assert_eq!(merged.conflicts.len(), 2);
        let details: Vec<&str> = merged.conflicts.iter().map(|c| c.detail.as_str()).collect();
        assert!(details.contains(&"Borrado en este equipo y modificado en la carpeta."));
        assert!(details.contains(&"Modificado en este equipo y borrado en la carpeta."));
        // Nada se pierde entretanto en ningún lado.
        assert!(build_tree(&merged.local).session(b.id).is_some());
        assert!(build_tree(&merged.remote).session(a.id).is_some());

        // Me quedo con lo mío en los dos: «a» desaparece y «b» se conserva.
        let decisions: BTreeMap<String, Decision> = merged
            .conflicts
            .iter()
            .map(|c| (c.key.clone(), c.decide(Side::Mine)))
            .collect();
        let mine = merge(&bi, &l, &r, &decisions);
        assert!(mine.conflicts.is_empty());
        assert_eq!(names(&build_tree(&mine.remote)), ["b"]);
        // Con lo de la carpeta: vuelve «a» (cambiado) y se va «b».
        let decisions: BTreeMap<String, Decision> = merged
            .conflicts
            .iter()
            .map(|c| (c.key.clone(), c.decide(Side::Theirs)))
            .collect();
        let theirs = merge(&bi, &l, &r, &decisions);
        let tree = build_tree(&theirs.local);
        assert_eq!(names(&tree), ["a"]);
        assert_eq!(tree.session(a.id).unwrap().host, "otro.example");
    }

    #[test]
    fn moving_hosts_between_folders() {
        let mut base = Tree::default();
        let one = base.add_folder(None, "Uno");
        let two = base.add_folder(None, "Dos");
        let three = base.add_folder(None, "Tres");
        let a = host("a");
        let b = host("b");
        base.upsert_session(Some(one), a.clone());
        base.upsert_session(Some(one), b.clone());

        // Aquí se mueve «a» a Dos; en la carpeta se le cambia el nombre.
        let mut local = base.clone();
        assert!(local.move_node(a.id, Some(two)));
        let mut remote = base.clone();
        edit(&mut remote, a.id, |s| s.name = "a2".into());
        let merged = merge(&items(&base), &items(&local), &items(&remote), &none());
        assert!(merged.conflicts.is_empty(), "{:?}", merged.conflicts);
        let tree = build_tree(&merged.local);
        assert_eq!(names(&tree), ["Uno / b", "Dos / a2"]);
        assert_eq!(merged.local, merged.remote);

        // Movido a carpetas distintas en cada lado: se pregunta.
        assert!(remote.move_node(a.id, Some(three)));
        let (bi, l, r) = (items(&base), items(&local), items(&remote));
        let merged = merge(&bi, &l, &r, &none());
        assert_eq!(merged.conflicts.len(), 1);
        assert_eq!(
            merged.conflicts[0].detail,
            "carpeta: distinto en los dos lados"
        );
        assert_eq!(build_tree(&merged.local).parent_of(a.id), Some(two));
        assert_eq!(build_tree(&merged.remote).parent_of(a.id), Some(three));
        let mut decisions = BTreeMap::new();
        decisions.insert(
            merged.conflicts[0].key.clone(),
            merged.conflicts[0].decide(Side::Theirs),
        );
        let theirs = merge(&bi, &l, &r, &decisions);
        let tree = build_tree(&theirs.local);
        assert_eq!(tree.parent_of(a.id), Some(three));
        assert_eq!(tree.session(a.id).unwrap().name, "a2");

        // Aquí se mueve «b» a Dos y en la carpeta se borra Dos (vacía): el
        // equipo no se pierde, queda en la raíz.
        let mut local = base.clone();
        assert!(local.move_node(b.id, Some(two)));
        let mut remote = base.clone();
        remote.remove(two);
        let merged = merge(&items(&base), &items(&local), &items(&remote), &none());
        assert!(merged.conflicts.is_empty());
        assert_eq!(merged.moved_to_root, 1);
        let tree = build_tree(&merged.local);
        assert!(tree.find(two).is_none());
        assert_eq!(tree.parent_of(b.id), None);
        assert!(tree.session(b.id).is_some());
        assert_eq!(merged.local, merged.remote);

        // Una carpeta dentro de otra en un lado y al revés en el otro.
        let mut local = base.clone();
        assert!(local.move_node(one, Some(two)));
        let mut remote = base.clone();
        assert!(remote.move_node(two, Some(one)));
        let merged = merge(&items(&base), &items(&local), &items(&remote), &none());
        let tree = build_tree(&merged.local);
        assert_eq!(tree.session_paths().len(), 2);
        assert_eq!(tree.folder_paths().len(), 3);
        assert_eq!(merged.local, merged.remote);
    }

    #[test]
    fn order_of_children_never_conflicts() {
        let mut base = Tree::default();
        let hosts: Vec<Session> = ["a", "b", "c"].iter().map(|n| host(n)).collect();
        for s in &hosts {
            base.upsert_session(None, s.clone());
        }
        // Aquí se invierte el orden y se añade «x»; en la carpeta se añade
        // «y».
        let mut local = base.clone();
        local.nodes.reverse();
        local.upsert_session(None, host("x"));
        let mut remote = base.clone();
        remote.upsert_session(None, host("y"));
        let merged = merge(&items(&base), &items(&local), &items(&remote), &none());
        assert!(merged.conflicts.is_empty());
        assert_eq!(names(&build_tree(&merged.local)), ["c", "b", "a", "x", "y"]);
        assert_eq!(merged.local, merged.remote);
        // Solo cambia el orden en la carpeta: se adopta.
        let mut remote = base.clone();
        remote.nodes.swap(0, 2);
        let merged = merge(&items(&base), &items(&base), &items(&remote), &none());
        assert_eq!(names(&build_tree(&merged.local)), ["c", "b", "a"]);
    }

    #[test]
    fn without_a_known_base_nothing_is_deleted() {
        // Dos equipos que no se conocen: cada uno con lo suyo y uno en
        // común (restaurado de la misma copia) que difiere.
        let shared = host("común");
        let mut local = Tree::default();
        local.upsert_session(None, shared.clone());
        local.upsert_session(None, host("solo-aquí"));
        let mut remote = Tree::default();
        remote.upsert_session(None, shared.clone());
        remote.upsert_session(None, host("solo-allí"));
        edit(&mut remote, shared.id, |s| s.port = 2222);
        let base = default_settings_items();
        let mut l = items(&local);
        let mut r = items(&remote);
        // Ajustes: aquí los de fábrica; allí se cambió uno. Y uno distinto
        // en los dos lados.
        l.extend(default_settings_items());
        r.extend(default_settings_items());
        r.insert("a/reintentos".into(), Value::from(9));
        l.insert("a/confirmar_pegado".into(), Value::from(false));
        l.insert("a/bloqueo_baul".into(), Value::from(5));
        r.insert("a/bloqueo_baul".into(), Value::from(60));
        let merged = merge(&base, &l, &r, &none());
        let tree = build_tree(&merged.local);
        assert_eq!(names(&tree), ["común", "solo-aquí", "solo-allí"]);
        assert_eq!(names(&build_tree(&merged.remote)), names(&tree));
        // Lo que difiere se pregunta; lo de fábrica cede ante lo cambiado.
        let keys: Vec<&str> = merged.conflicts.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, ["a/bloqueo_baul", &format!("s/{}", shared.id)]);
        assert_eq!(merged.conflicts[0].title, "Ajuste «bloqueo baul»");
        assert_eq!(merged.conflicts[0].detail, "Aquí: 5 · En la carpeta: 60");
        assert_eq!(merged.local["a/reintentos"], Value::from(9));
        assert_eq!(merged.remote["a/confirmar_pegado"], Value::from(false));
    }

    #[test]
    fn what_this_version_does_not_understand_is_kept() {
        let mut tree = Tree::default();
        let a = host("a");
        tree.upsert_session(None, a.clone());
        let known = items(&tree);
        // La carpeta trae cosas de una versión posterior: un campo nuevo en
        // un equipo, un equipo con un protocolo que aquí no existe, un tipo
        // de elemento nuevo y un ajuste nuevo.
        let mut base = known.clone();
        base.get_mut(&format!("s/{}", a.id))
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("campo_futuro".into(), Value::from(7));
        let alien = Uuid::new_v4();
        let mut alien_value = json(&host("marciano")).unwrap();
        alien_value["protocol"] = Value::from("Mosh");
        alien_value["id"] = Value::from(alien.to_string());
        set_parent(&mut alien_value, None);
        base.insert(format!("s/{alien}"), alien_value);
        base.insert("x/cosa".into(), serde_json::json!({"dato": 1}));
        base.insert("a/ajuste_futuro".into(), Value::from(true));
        base.insert("a/editor".into(), Value::from("de otra versión"));
        normalize_tree(&mut base);

        // Lo local, leído con esta versión, no tiene nada de eso…
        let mut local = known.clone();
        assert!(!local.contains_key("x/cosa"));
        // …pero al completarlo con la base no se da por borrado.
        overlay(&mut local, &base, false);
        normalize_tree(&mut local);
        let merged = merge(&base, &local, &base, &none());
        assert_eq!((merged.pulled, merged.pushed), (0, 0), "{merged:?}");
        assert_eq!(merged.remote, base);
        // Y un cambio local de verdad conserva el campo desconocido.
        let mut edited = tree.clone();
        edit(&mut edited, a.id, |s| s.port = 2022);
        let mut local = items(&edited);
        overlay(&mut local, &base, false);
        let merged = merge(&base, &local, &base, &none());
        let pushed = &merged.remote[&format!("s/{}", a.id)];
        assert_eq!(pushed["port"], Value::from(2022));
        assert_eq!(pushed["campo_futuro"], Value::from(7));
        assert_eq!(merged.pushed, 1);
        // El árbol local se reconstruye sin lo que no se entiende.
        assert_eq!(names(&build_tree(&merged.local)), ["a"]);
    }

    #[test]
    fn vaults_merge_secret_by_secret() {
        let map = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let base = map(&[("a", "1"), ("b", "2"), ("c", "3"), ("d", "4")]);
        // Aquí: cambia a, se borra b, se añade x. Allí: cambia c, se borra
        // d, se añade y. Y «z» se añade en los dos con valores distintos.
        let mine = map(&[
            ("a", "1bis"),
            ("c", "3"),
            ("d", "4"),
            ("x", "9"),
            ("z", "m"),
        ]);
        let theirs = map(&[
            ("a", "1"),
            ("b", "2"),
            ("c", "3bis"),
            ("y", "8"),
            ("z", "t"),
        ]);
        let plan = plan_vault_merge(Some(&base), &mine, &theirs, Side::Mine);
        assert_eq!(plan.set, map(&[("c", "3bis"), ("y", "8")]));
        assert_eq!(plan.remove, ["d"]);
        assert_eq!(plan.clashes, ["z"]);
        let plan = plan_vault_merge(Some(&base), &mine, &theirs, Side::Theirs);
        assert_eq!(plan.set, map(&[("c", "3bis"), ("y", "8"), ("z", "t")]));
        // Sin base no se borra nada y lo que difiere choca.
        let plan = plan_vault_merge(None, &mine, &theirs, Side::Mine);
        assert_eq!(plan.set, map(&[("b", "2"), ("y", "8")]));
        assert!(plan.remove.is_empty());
        assert_eq!(plan.clashes, ["a", "c", "z"]);
        // Iguales: nada que hacer.
        assert_eq!(
            plan_vault_merge(Some(&base), &mine, &mine, Side::Theirs),
            VaultPlan::default()
        );
    }

    #[test]
    fn snapshots_from_the_folder_are_bounded() {
        let snapshot = Snapshot {
            format: FORMAT,
            id: Uuid::new_v4(),
            revision: 1,
            device: Uuid::new_v4(),
            ancestors: (0..MAX_LINEAGE + 50).map(|_| Uuid::new_v4()).collect(),
            devices: (0..MAX_DEVICES as u64 + 10)
                .map(|n| {
                    (
                        Uuid::new_v4(),
                        DeviceInfo {
                            name: format!("equipo\u{7}\n{n}{}", "x".repeat(500)),
                            seen: 0,
                            revision: n,
                        },
                    )
                })
                .collect(),
            items: Items::new(),
        };
        let ok = snapshot.clone().validate().unwrap();
        assert_eq!(ok.ancestors.len(), MAX_LINEAGE);
        assert_eq!(ok.devices.len(), MAX_DEVICES);
        assert!(
            ok.devices.values().all(|d| d.name.chars().count() <= 60
                && !d.name.contains('\n')
                && d.revision >= 10)
        );
        let mut newer = snapshot.clone();
        newer.format = FORMAT + 1;
        assert_eq!(
            newer.validate().unwrap_err(),
            SyncError::Seal(SealError::Newer)
        );
        let mut crowded = snapshot.clone();
        crowded.items = (0..=MAX_ITEMS)
            .map(|n| (format!("x/{n}"), Value::Null))
            .collect();
        assert!(matches!(crowded.validate(), Err(SyncError::Invalid(_))));
        let mut long_key = snapshot;
        long_key.items.insert("k".repeat(MAX_KEY + 1), Value::Null);
        assert!(matches!(long_key.validate(), Err(SyncError::Invalid(_))));
    }
}
