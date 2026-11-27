//! Estado central del servidor X: clientes, recursos, árbol de ventanas,
//! reparto de eventos y composición de la pantalla.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{Notify, mpsc};

use super::draw::{Gc, Image, Rect};
use super::font::Font;
use super::keymap::Keymap;
use super::proto::*;
use crate::desktop::DesktopEvent;

/// Identificador de cliente; el 0 es el propio servidor.
pub type ClientId = u32;

pub const ROOT: Xid = 0x100;
pub const DEFAULT_COLORMAP: Xid = 0x101;
pub const ROOT_VISUAL: Xid = 0x102;
/// Ventana interna (nunca visible) con la que el servidor hace de dueño y
/// de solicitante de selecciones para el portapapeles.
pub const SERVER_WINDOW: Xid = 0x103;
/// Bits de identificador que cada cliente puede usar a su gusto.
pub const RESOURCE_MASK: u32 = 0x001f_ffff;
pub const MAX_CLIENTS: u32 = 255;

// Átomos predefinidos que usa el propio servidor.
pub const A_PRIMARY: Xid = 1;
pub const A_ATOM: Xid = 4;
pub const A_FONT: Xid = 18;
pub const A_STRING: Xid = 31;
pub const A_WM_HINTS: Xid = 35;
pub const A_WM_NAME: Xid = 39;
pub const A_WM_NORMAL_HINTS: Xid = 40;

const PREDEFINED_ATOMS: &[&str] = &[
    "PRIMARY",
    "SECONDARY",
    "ARC",
    "ATOM",
    "BITMAP",
    "CARDINAL",
    "COLORMAP",
    "CURSOR",
    "CUT_BUFFER0",
    "CUT_BUFFER1",
    "CUT_BUFFER2",
    "CUT_BUFFER3",
    "CUT_BUFFER4",
    "CUT_BUFFER5",
    "CUT_BUFFER6",
    "CUT_BUFFER7",
    "DRAWABLE",
    "FONT",
    "INTEGER",
    "PIXMAP",
    "POINT",
    "RECTANGLE",
    "RESOURCE_MANAGER",
    "RGB_COLOR_MAP",
    "RGB_BEST_MAP",
    "RGB_BLUE_MAP",
    "RGB_DEFAULT_MAP",
    "RGB_GRAY_MAP",
    "RGB_GREEN_MAP",
    "RGB_RED_MAP",
    "STRING",
    "VISUALID",
    "WINDOW",
    "WM_COMMAND",
    "WM_HINTS",
    "WM_CLIENT_MACHINE",
    "WM_ICON_NAME",
    "WM_ICON_SIZE",
    "WM_NAME",
    "WM_NORMAL_HINTS",
    "WM_SIZE_HINTS",
    "WM_ZOOM_HINTS",
    "MIN_SPACE",
    "NORM_SPACE",
    "MAX_SPACE",
    "END_SPACE",
    "SUPERSCRIPT_X",
    "SUPERSCRIPT_Y",
    "SUBSCRIPT_X",
    "SUBSCRIPT_Y",
    "UNDERLINE_POSITION",
    "UNDERLINE_THICKNESS",
    "STRIKEOUT_ASCENT",
    "STRIKEOUT_DESCENT",
    "ITALIC_ANGLE",
    "X_HEIGHT",
    "QUAD_WIDTH",
    "WEIGHT",
    "POINT_SIZE",
    "RESOLUTION",
    "COPYRIGHT",
    "NOTICE",
    "FONT_NAME",
    "FAMILY_NAME",
    "FULL_NAME",
    "CAP_HEIGHT",
    "WM_CLASS",
    "WM_TRANSIENT_FOR",
];

/// Tabla de átomos: nombre ↔ número.
pub struct Atoms {
    names: Vec<String>,
    by_name: HashMap<String, Xid>,
}

impl Atoms {
    fn new() -> Self {
        let mut atoms = Atoms {
            names: vec![],
            by_name: HashMap::new(),
        };
        for name in PREDEFINED_ATOMS {
            atoms.intern(name);
        }
        atoms
    }

    pub fn intern(&mut self, name: &str) -> Xid {
        if let Some(id) = self.by_name.get(name) {
            return *id;
        }
        self.names.push(name.to_string());
        let id = self.names.len() as Xid;
        self.by_name.insert(name.to_string(), id);
        id
    }

    pub fn find(&self, name: &str) -> Option<Xid> {
        self.by_name.get(name).copied()
    }

    pub fn name(&self, atom: Xid) -> Option<&str> {
        self.names
            .get((atom as usize).checked_sub(1)?)
            .map(String::as_str)
    }

    pub fn valid(&self, atom: Xid) -> bool {
        atom >= 1 && atom as usize <= self.names.len()
    }
}

/// Átomos que el servidor necesita tener a mano.
pub struct KnownAtoms {
    pub wm_protocols: Xid,
    pub wm_delete_window: Xid,
    pub wm_take_focus: Xid,
    pub clipboard: Xid,
    pub utf8_string: Xid,
    pub targets: Xid,
    pub text: Xid,
    pub timestamp: Xid,
    pub incr: Xid,
    pub net_wm_name: Xid,
    pub motif_wm_hints: Xid,
    /// Propiedad donde los clientes dejan el texto que se les pide.
    pub transfer: Xid,
}

pub struct Client {
    pub id: ClientId,
    /// El cliente habla en *big-endian*.
    pub be: bool,
    /// Número de secuencia de la última petición procesada.
    pub seq: u16,
    /// Bytes pendientes de enviar (respuestas, eventos y errores, en orden).
    pub out: Vec<u8>,
    pub tx: mpsc::UnboundedSender<Vec<u8>>,
    /// Despierta a la tarea del cliente para que cierre la conexión.
    pub kill: Arc<Notify>,
    pub big_requests: bool,
    /// Ventanas ajenas que deben sobrevivir a este cliente (`ChangeSaveSet`).
    pub save_set: Vec<Xid>,
    /// Ya se le envió un aviso de movimiento y aún no ha preguntado.
    pub motion_hint_sent: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Background {
    None,
    ParentRelative,
    Pixel(u32),
    Pixmap(Arc<Image>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    pub name: Xid,
    pub kind: Xid,
    pub format: u8,
    /// Datos en orden *little-endian* canónico.
    pub data: Vec<u8>,
}

/// Captura pasiva de botón o de tecla sobre una ventana.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassiveGrab {
    pub client: ClientId,
    /// `true`: de tecla (`detail` es un código de tecla); `false`: de botón.
    pub key: bool,
    /// 0 = cualquiera.
    pub detail: u8,
    /// `S_ANY_MODIFIER` = cualquiera.
    pub modifiers: u16,
    pub owner_events: bool,
    pub mask: u32,
    pub sync: bool,
}

pub struct Window {
    pub id: Xid,
    pub owner: ClientId,
    pub parent: Xid,
    /// Hijas, de la más baja a la más alta.
    pub children: Vec<Xid>,
    /// Esquina exterior (del borde) respecto al origen de la madre.
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub bw: u32,
    pub depth: u8,
    pub input_only: bool,
    pub mapped: bool,
    pub bg: Background,
    pub border_pixel: u32,
    pub bit_gravity: u8,
    pub win_gravity: u8,
    pub backing_store: u8,
    pub backing_planes: u32,
    pub backing_pixel: u32,
    pub override_redirect: bool,
    pub save_under: bool,
    pub do_not_propagate: u32,
    pub colormap: Xid,
    pub cursor: Xid,
    /// Máscara de eventos de cada cliente.
    pub masks: Vec<(ClientId, u32)>,
    pub props: Vec<Property>,
    /// Contenido (las ventanas `InputOnly` no tienen).
    pub image: Option<Image>,
    pub grabs: Vec<PassiveGrab>,
    /// El gestor integrado ya eligió su posición inicial.
    pub placed: bool,
    /// Barra de título ya dibujada: `(ancho, activa, imagen)`.
    pub title_cache: Option<(i32, bool, Arc<Image>)>,
}

impl Window {
    pub fn mask_of(&self, client: ClientId) -> u32 {
        self.masks
            .iter()
            .find(|(c, _)| *c == client)
            .map_or(0, |(_, m)| *m)
    }

    pub fn all_masks(&self) -> u32 {
        self.masks.iter().fold(0, |acc, (_, m)| acc | m)
    }

    pub fn prop(&self, name: Xid) -> Option<&Property> {
        self.props.iter().find(|p| p.name == name)
    }

    /// Rectángulo exterior (con borde) en coordenadas de la madre.
    pub fn outer(&self) -> Rect {
        Rect::new(
            self.x,
            self.y,
            (self.w + 2 * self.bw) as i32,
            (self.h + 2 * self.bw) as i32,
        )
    }
}

pub struct Pixmap {
    pub owner: ClientId,
    /// Tamaño pedido (el búfer puede ser menor si era desmesurado).
    pub width: u32,
    pub height: u32,
    pub image: Arc<Image>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    None,
    PointerRoot,
    Window(Xid),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub window: Xid,
    pub client: ClientId,
    pub time: u32,
}

/// Captura activa del puntero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerGrab {
    pub client: ClientId,
    pub window: Xid,
    pub owner_events: bool,
    pub mask: u32,
    /// Nació de pulsar un botón: termina al soltarlos todos.
    pub by_button: bool,
    /// Captura pasiva síncrona: el evento se puede «repetir».
    pub replay: Option<(u8, Xid)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardGrab {
    pub client: ClientId,
    pub window: Xid,
    pub owner_events: bool,
    /// Código de la tecla que la activó (captura pasiva): termina al soltarla.
    pub by_key: Option<u8>,
}

/// Arrastre en curso de una ventana por su decoración.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WmDrag {
    Move {
        window: Xid,
        dx: i32,
        dy: i32,
    },
    Resize {
        window: Xid,
        /// Bordes que se mueven: izquierdo, derecho, inferior.
        left: bool,
        right: bool,
        bottom: bool,
        start: (i32, i32),
        geometry: (i32, i32, i32, i32),
    },
    /// Botón de cerrar pulsado: cierra si se suelta encima.
    Close {
        window: Xid,
    },
}

pub struct Server {
    pub width: u32,
    pub height: u32,
    pub clients: HashMap<ClientId, Client>,
    pub windows: HashMap<Xid, Window>,
    pub pixmaps: HashMap<Xid, Pixmap>,
    pub gcs: HashMap<Xid, (ClientId, Arc<Gc>)>,
    pub fonts: HashMap<Xid, (ClientId, Font)>,
    pub cursors: HashMap<Xid, ClientId>,
    pub colormaps: HashMap<Xid, ClientId>,
    pub atoms: Atoms,
    pub known: KnownAtoms,
    pub default_font: Font,
    /// Fuente de las barras de título del gestor integrado.
    pub title_font: Font,
    pub keymap: Keymap,
    pub selections: HashMap<Xid, Selection>,
    pub focus: Focus,
    pub focus_revert: u8,
    pub pointer: (i32, i32),
    /// Botones pulsados (bits de estado 8–12).
    pub buttons: u16,
    /// Ventana más profunda bajo el puntero.
    pub pointer_window: Xid,
    pub grab: Option<PointerGrab>,
    pub keyboard_grab: Option<KeyboardGrab>,
    /// Cliente que hace de gestor de ventanas (redirige la raíz).
    pub wm_client: Option<ClientId>,
    pub wm_drag: Option<WmDrag>,
    /// Siguiente hueco de la colocación en cascada.
    pub cascade: u32,
    /// Zonas de pantalla que hay que recomponer.
    pub damage: Vec<Rect>,
    /// El tamaño de la pantalla cambió: hay que rehacer el búfer compartido.
    pub resized: bool,
    /// Texto del portapapeles local que el servidor ofrece a los clientes.
    pub clipboard: Option<String>,
    /// Último texto recibido de un cliente (para no hacerse eco).
    pub clipboard_from_client: Option<String>,
    /// Sincronizar también PRIMARY (lo seleccionado con el ratón).
    pub sync_primary: bool,
    /// Avisos pendientes para la interfaz.
    pub desktop_events: Vec<DesktopEvent>,
    /// Peticiones sin implementar ya anotadas (para no repetir el aviso).
    pub unimplemented: Vec<u8>,
    start: Instant,
    next_client: ClientId,
}

/// Destino de la composición: una zona de pantalla en píxeles 0xRRGGBB.
pub struct Target<'a> {
    pub rect: Rect,
    pub data: &'a mut [u32],
}

impl Target<'_> {
    pub fn fill(&mut self, r: Rect, pixel: u32) {
        let r = r.intersect(&self.rect);
        let w = self.rect.width() as usize;
        for y in r.y0..r.y1 {
            let start = (y - self.rect.y0) as usize * w + (r.x0 - self.rect.x0) as usize;
            self.data[start..start + r.width() as usize].fill(pixel);
        }
    }

    /// Copia la parte `clip` de una imagen cuyo origen está en `(ox, oy)`.
    pub fn blit(&mut self, img: &Image, ox: i32, oy: i32, clip: Rect) {
        let r = clip
            .intersect(&self.rect)
            .intersect(&img.bounds().translate(ox, oy));
        let w = self.rect.width() as usize;
        for y in r.y0..r.y1 {
            let start = (y - self.rect.y0) as usize * w + (r.x0 - self.rect.x0) as usize;
            self.data[start..start + r.width() as usize].copy_from_slice(img.row(
                y - oy,
                r.x0 - ox,
                r.x1 - ox,
            ));
        }
    }
}

/// Color de fondo del escritorio.
const DESKTOP_COLOR: u32 = 0x2b3a4a;

impl Server {
    pub fn new(width: u32, height: u32) -> Self {
        let mut atoms = Atoms::new();
        let known = KnownAtoms {
            wm_protocols: atoms.intern("WM_PROTOCOLS"),
            wm_delete_window: atoms.intern("WM_DELETE_WINDOW"),
            wm_take_focus: atoms.intern("WM_TAKE_FOCUS"),
            clipboard: atoms.intern("CLIPBOARD"),
            utf8_string: atoms.intern("UTF8_STRING"),
            targets: atoms.intern("TARGETS"),
            text: atoms.intern("TEXT"),
            timestamp: atoms.intern("TIMESTAMP"),
            incr: atoms.intern("INCR"),
            net_wm_name: atoms.intern("_NET_WM_NAME"),
            motif_wm_hints: atoms.intern("_MOTIF_WM_HINTS"),
            transfer: atoms.intern("GODREMOTE_SELECTION"),
        };
        let mut server = Server {
            width,
            height,
            clients: HashMap::new(),
            windows: HashMap::new(),
            pixmaps: HashMap::new(),
            gcs: HashMap::new(),
            fonts: HashMap::new(),
            cursors: HashMap::new(),
            colormaps: HashMap::new(),
            atoms,
            known,
            default_font: super::font::open("fixed").expect("fuente por defecto"),
            title_font: super::font::open("-*-helvetica-bold-r-*-*-14-*-*-*-*-*-iso10646-1")
                .expect("fuente de los títulos"),
            keymap: Keymap::new(),
            selections: HashMap::new(),
            focus: Focus::PointerRoot,
            focus_revert: 1,
            pointer: (0, 0),
            buttons: 0,
            pointer_window: ROOT,
            grab: None,
            keyboard_grab: None,
            wm_client: None,
            wm_drag: None,
            cascade: 0,
            damage: vec![],
            resized: false,
            clipboard: None,
            clipboard_from_client: None,
            sync_primary: true,
            desktop_events: vec![],
            unimplemented: vec![],
            start: Instant::now(),
            next_client: 1,
        };
        let mut root = server.blank_window(ROOT, 0, 0, 24, false);
        (root.w, root.h) = (width, height);
        root.mapped = true;
        root.bg = Background::Pixel(DESKTOP_COLOR);
        let mut image = Image::new(width, height, 24);
        image.fill(image.bounds(), DESKTOP_COLOR);
        root.image = Some(image);
        server.windows.insert(ROOT, root);
        let mut internal = server.blank_window(SERVER_WINDOW, 0, ROOT, 0, true);
        (internal.x, internal.y, internal.w, internal.h) = (-10, -10, 1, 1);
        server.windows.insert(SERVER_WINDOW, internal);
        server.damage_screen(Rect::new(0, 0, width as i32, height as i32));
        server
    }

    pub fn blank_window(
        &self,
        id: Xid,
        owner: ClientId,
        parent: Xid,
        depth: u8,
        input_only: bool,
    ) -> Window {
        Window {
            id,
            owner,
            parent,
            children: vec![],
            x: 0,
            y: 0,
            w: 1,
            h: 1,
            bw: 0,
            depth,
            input_only,
            mapped: false,
            bg: Background::None,
            border_pixel: 0,
            bit_gravity: 0,
            win_gravity: 1,
            backing_store: 0,
            backing_planes: u32::MAX,
            backing_pixel: 0,
            override_redirect: false,
            save_under: false,
            do_not_propagate: 0,
            colormap: DEFAULT_COLORMAP,
            cursor: 0,
            masks: vec![],
            props: vec![],
            image: None,
            grabs: vec![],
            placed: false,
            title_cache: None,
        }
    }

    /// Milisegundos desde que arrancó el servidor (nunca 0: es «CurrentTime»).
    pub fn now(&self) -> u32 {
        (self.start.elapsed().as_millis() as u32).max(1)
    }

    // --- Clientes -----------------------------------------------------------

    pub fn add_client(
        &mut self,
        be: bool,
        tx: mpsc::UnboundedSender<Vec<u8>>,
        kill: Arc<Notify>,
    ) -> Option<ClientId> {
        if self.clients.len() as u32 >= MAX_CLIENTS {
            return None;
        }
        // Busca un identificador libre (se reutilizan los de clientes idos).
        let mut id = self.next_client;
        while self.clients.contains_key(&id) {
            id = id % MAX_CLIENTS + 1;
        }
        self.next_client = id % MAX_CLIENTS + 1;
        self.clients.insert(
            id,
            Client {
                id,
                be,
                seq: 0,
                out: vec![],
                tx,
                kill,
                big_requests: false,
                save_set: vec![],
                motion_hint_sent: false,
            },
        );
        Some(id)
    }

    /// Base de los identificadores de recurso de un cliente.
    pub fn resource_base(client: ClientId) -> u32 {
        client << 21
    }

    pub fn owner_of(id: Xid) -> ClientId {
        id >> 21
    }

    /// Comprueba que un cliente puede crear un recurso con ese identificador.
    pub fn check_new_id(&self, client: ClientId, id: Xid) -> XResult {
        let used = self.windows.contains_key(&id)
            || self.pixmaps.contains_key(&id)
            || self.gcs.contains_key(&id)
            || self.fonts.contains_key(&id)
            || self.cursors.contains_key(&id)
            || self.colormaps.contains_key(&id);
        if Self::owner_of(id) != client || used {
            return Err(XError::new(BAD_ID_CHOICE, id));
        }
        Ok(())
    }

    /// Envía los bytes pendientes de cada cliente a su tarea de escritura.
    pub fn flush(&mut self) {
        for client in self.clients.values_mut() {
            if !client.out.is_empty() {
                let _ = client.tx.send(std::mem::take(&mut client.out));
            }
        }
    }

    /// Desconecta a un cliente y libera todo lo suyo.
    pub fn remove_client(&mut self, id: ClientId) {
        let Some(client) = self.clients.remove(&id) else {
            return;
        };
        client.kill.notify_one();
        // Las ventanas ajenas de su conjunto de salvación vuelven a la raíz
        // (así sobreviven a la muerte de un gestor de ventanas).
        for win in client.save_set {
            if self.windows.contains_key(&win) && Self::owner_of(win) != id {
                let (ax, ay) = self.abs_origin(win);
                let bw = self.windows[&win].bw as i32;
                if self.windows[&win].parent != ROOT {
                    let _ = self.reparent_window(win, ROOT, ax - bw, ay - bw);
                }
                self.map_window(0, win);
            }
        }
        if self.wm_client == Some(id) {
            self.wm_client = None;
            self.damage_screen(Rect::new(0, 0, self.width as i32, self.height as i32));
        }
        if self.grab.as_ref().is_some_and(|g| g.client == id) {
            self.grab = None;
        }
        if self.keyboard_grab.as_ref().is_some_and(|g| g.client == id) {
            self.keyboard_grab = None;
        }
        // Ventanas de primer nivel del cliente (las hijas caen con ellas).
        let mut owned: Vec<Xid> = self
            .windows
            .values()
            .filter(|w| w.owner == id)
            .map(|w| w.id)
            .collect();
        owned.sort_unstable();
        for win in owned {
            if self.windows.contains_key(&win) {
                self.destroy_window(win);
            }
        }
        for w in self.windows.values_mut() {
            w.masks.retain(|(c, _)| *c != id);
            w.grabs.retain(|g| g.client != id);
        }
        self.pixmaps.retain(|_, p| p.owner != id);
        self.gcs.retain(|_, (c, _)| *c != id);
        self.fonts.retain(|_, (c, _)| *c != id);
        self.cursors.retain(|_, c| *c != id);
        self.colormaps.retain(|_, c| *c != id);
        self.selections.retain(|_, s| s.client != id);
        self.refresh_pointer_window();
    }

    // --- Envío de eventos ---------------------------------------------------

    /// Entrega un evento a un cliente (el 0 es el propio servidor).
    pub fn send(&mut self, client: ClientId, ev: Event) {
        if client == 0 {
            self.internal_event(ev);
            return;
        }
        if let Some(c) = self.clients.get_mut(&client) {
            let bytes = ev.encode(c.be, c.seq);
            c.out.extend_from_slice(&bytes);
        }
    }

    /// Entrega un evento a todos los clientes que seleccionaron `mask` en la
    /// ventana. Devuelve si alguno lo recibió.
    pub fn deliver(&mut self, window: Xid, mask: u32, ev: Event) -> bool {
        let Some(w) = self.windows.get(&window) else {
            return false;
        };
        let targets: Vec<ClientId> = w
            .masks
            .iter()
            .filter(|(_, m)| m & mask != 0)
            .map(|(c, _)| *c)
            .collect();
        for client in &targets {
            self.send(*client, ev);
        }
        !targets.is_empty()
    }

    /// Evento de estructura: a la ventana (`StructureNotify`) y a su madre
    /// (`SubstructureNotify`). `make` recibe la ventana «event».
    pub fn notify_structure(&mut self, window: Xid, make: impl Fn(Xid) -> Event) {
        let parent = self.windows.get(&window).map_or(0, |w| w.parent);
        self.deliver(window, M_STRUCTURE_NOTIFY, make(window));
        if parent != 0 {
            self.deliver(parent, M_SUBSTRUCTURE_NOTIFY, make(parent));
        }
    }

    // --- Geometría del árbol ------------------------------------------------

    pub fn win(&self, id: Xid) -> XResult<&Window> {
        self.windows.get(&id).ok_or(XError::new(BAD_WINDOW, id))
    }

    pub fn win_mut(&mut self, id: Xid) -> XResult<&mut Window> {
        self.windows.get_mut(&id).ok_or(XError::new(BAD_WINDOW, id))
    }

    /// Origen del interior de una ventana en coordenadas de pantalla.
    pub fn abs_origin(&self, id: Xid) -> (i32, i32) {
        let (mut x, mut y) = (0, 0);
        let mut cur = id;
        while let Some(w) = self.windows.get(&cur) {
            if cur == ROOT {
                break;
            }
            x += w.x + w.bw as i32;
            y += w.y + w.bw as i32;
            cur = w.parent;
        }
        (x, y)
    }

    /// Mapeada ella y todas sus antecesoras.
    pub fn viewable(&self, id: Xid) -> bool {
        let mut cur = id;
        loop {
            let Some(w) = self.windows.get(&cur) else {
                return false;
            };
            if !w.mapped {
                return false;
            }
            if cur == ROOT {
                return true;
            }
            cur = w.parent;
        }
    }

    /// `a` es antecesora estricta de `b`.
    pub fn is_ancestor(&self, a: Xid, b: Xid) -> bool {
        let mut cur = b;
        while let Some(w) = self.windows.get(&cur) {
            if w.parent == a {
                return true;
            }
            if cur == ROOT {
                return false;
            }
            cur = w.parent;
        }
        false
    }

    /// Cadena de ventanas desde `id` hasta la raíz (ambas incluidas).
    pub fn ancestry(&self, id: Xid) -> Vec<Xid> {
        let mut out = vec![];
        let mut cur = id;
        while let Some(w) = self.windows.get(&cur) {
            out.push(cur);
            if cur == ROOT {
                break;
            }
            cur = w.parent;
        }
        out
    }

    /// Ventana de primer nivel (hija de la raíz) que contiene a `id`.
    pub fn toplevel_of(&self, id: Xid) -> Option<Xid> {
        let chain = self.ancestry(id);
        (chain.len() >= 2 && chain.last() == Some(&ROOT)).then(|| chain[chain.len() - 2])
    }

    /// Hija de `parent` que es `descendant` o la contiene (0 si no hay).
    pub fn child_towards(&self, parent: Xid, descendant: Xid) -> Xid {
        let mut cur = descendant;
        while let Some(w) = self.windows.get(&cur) {
            if w.parent == parent {
                return cur;
            }
            if cur == ROOT {
                break;
            }
            cur = w.parent;
        }
        0
    }

    /// Ventana visible más profunda que contiene el punto de pantalla.
    pub fn window_at(&self, x: i32, y: i32) -> Xid {
        let mut cur = ROOT;
        let (mut ox, mut oy) = (0, 0);
        'descend: loop {
            let w = &self.windows[&cur];
            for child in w.children.iter().rev() {
                let c = &self.windows[child];
                if c.mapped && c.outer().translate(ox, oy).contains(x, y) {
                    ox += c.x + c.bw as i32;
                    oy += c.y + c.bw as i32;
                    cur = *child;
                    continue 'descend;
                }
            }
            return cur;
        }
    }

    // --- Daños y composición ------------------------------------------------

    pub fn screen_rect(&self) -> Rect {
        Rect::new(0, 0, self.width as i32, self.height as i32)
    }

    /// Anota una zona de pantalla para recomponer.
    pub fn damage_screen(&mut self, r: Rect) {
        let r = r.intersect(&self.screen_rect());
        if r.is_empty() {
            return;
        }
        // Se funde con una zona que la toque; si se acumulan muchas, todas.
        for d in &mut self.damage {
            if !d.intersect(&r).is_empty() {
                *d = d.union(&r);
                return;
            }
        }
        self.damage.push(r);
        if self.damage.len() > 12 {
            let all = self.damage.iter().fold(Rect::EMPTY, |acc, d| acc.union(d));
            self.damage = vec![all];
        }
    }

    /// Zona visible (en pantalla) de un rectángulo del interior de una
    /// ventana: recortada por todas sus antecesoras. Vacía si no se ve.
    pub fn visible_rect(&self, id: Xid, r: Rect) -> Rect {
        let mut cur = id;
        let mut r = r;
        loop {
            let Some(w) = self.windows.get(&cur) else {
                return Rect::EMPTY;
            };
            if !w.mapped {
                return Rect::EMPTY;
            }
            r = r.intersect(&Rect::new(0, 0, w.w as i32, w.h as i32));
            if cur == ROOT || r.is_empty() {
                return r;
            }
            r = r.translate(w.x + w.bw as i32, w.y + w.bw as i32);
            cur = w.parent;
        }
    }

    /// Anota como dañada una zona del interior de una ventana.
    pub fn damage_window(&mut self, id: Xid, r: Rect) {
        let visible = self.visible_rect(id, r);
        self.damage_screen(visible);
    }

    /// Anota toda la zona que ocupa una ventana, con borde y decoración.
    pub fn damage_frame(&mut self, id: Xid) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        let parent = w.parent;
        let rect = if parent == ROOT {
            self.frame_rect(w)
        } else {
            w.outer()
        };
        if parent == ROOT {
            self.damage_screen(rect);
        } else {
            self.damage_window(parent, rect);
        }
    }

    /// Compone una zona de la pantalla en `out` (`rect.width() * height`).
    pub fn compose(&self, rect: Rect, out: &mut [u32]) {
        let mut target = Target { rect, data: out };
        self.paint_tree(ROOT, 0, 0, rect, &mut target);
    }

    /// Compone una ventana con sus descendientes (para `GetImage`): `rect`
    /// va en coordenadas del interior de la ventana.
    pub fn compose_window(&self, id: Xid, rect: Rect, out: &mut [u32]) {
        let mut target = Target { rect, data: out };
        self.paint_tree(id, 0, 0, rect, &mut target);
    }

    fn paint_tree(&self, id: Xid, ox: i32, oy: i32, clip: Rect, target: &mut Target) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        let inner = Rect::new(ox, oy, w.w as i32, w.h as i32);
        if w.bw > 0 && !w.input_only {
            let bw = w.bw as i32;
            let outer = Rect {
                x0: inner.x0 - bw,
                y0: inner.y0 - bw,
                x1: inner.x1 + bw,
                y1: inner.y1 + bw,
            };
            for part in outer.subtract(&inner) {
                target.fill(part.intersect(&clip), w.border_pixel);
            }
        }
        let visible = inner.intersect(&clip);
        if visible.is_empty() {
            return;
        }
        if let Some(img) = &w.image {
            target.blit(img, ox, oy, visible);
        }
        for child in &w.children {
            let Some(c) = self.windows.get(child) else {
                continue;
            };
            if !c.mapped {
                continue;
            }
            let (cx, cy) = (ox + c.x + c.bw as i32, oy + c.y + c.bw as i32);
            if id == ROOT && self.decorated(c) {
                self.paint_decoration(c, visible, target);
            }
            self.paint_tree(*child, cx, cy, visible, target);
        }
    }

    // --- Fondo y exposición -------------------------------------------------

    /// Fondo efectivo de una ventana y origen de su mosaico (resuelve
    /// `ParentRelative` subiendo por el árbol).
    fn effective_background(&self, id: Xid) -> (Background, i32, i32) {
        let (mut ox, mut oy) = (0, 0);
        let mut cur = id;
        loop {
            let Some(w) = self.windows.get(&cur) else {
                return (Background::None, 0, 0);
            };
            if w.bg != Background::ParentRelative || cur == ROOT {
                return (w.bg.clone(), ox, oy);
            }
            ox -= w.x + w.bw as i32;
            oy -= w.y + w.bw as i32;
            cur = w.parent;
        }
    }

    /// Pinta el fondo en una zona del búfer de la ventana.
    pub fn paint_background(&mut self, id: Xid, r: Rect) {
        let (bg, ox, oy) = self.effective_background(id);
        let Some(img) = self.windows.get_mut(&id).and_then(|w| w.image.as_mut()) else {
            return;
        };
        match bg {
            Background::Pixel(p) => img.fill(r, p),
            Background::Pixmap(tile) => img.fill_tiled(r, &tile, ox, oy),
            Background::None | Background::ParentRelative => return,
        }
        self.damage_window(id, r);
    }

    /// Borra a fondo unas zonas y avisa al cliente con `Expose`.
    pub fn expose(&mut self, id: Xid, rects: &[Rect]) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        if w.input_only {
            return;
        }
        let bounds = Rect::new(0, 0, w.w as i32, w.h as i32);
        let rects: Vec<Rect> = rects
            .iter()
            .map(|r| r.intersect(&bounds))
            .filter(|r| !r.is_empty())
            .collect();
        for (i, r) in rects.iter().enumerate() {
            self.paint_background(id, *r);
            let ev = Ev::new(EXPOSE, 0)
                .u32(id)
                .u16(r.x0 as u16)
                .u16(r.y0 as u16)
                .u16(r.width() as u16)
                .u16(r.height() as u16)
                .u16((rects.len() - 1 - i) as u16)
                .done();
            self.deliver(id, M_EXPOSURE, ev);
        }
    }

    /// Expone entera una ventana recién visible y todas sus hijas mapeadas.
    pub fn expose_tree(&mut self, id: Xid) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        if !w.mapped {
            return;
        }
        let whole = Rect::new(0, 0, w.w as i32, w.h as i32);
        let children = w.children.clone();
        if !w.input_only {
            // «Sin obstrucción»: el contenido se guarda siempre entero.
            let ev = Ev::new(VISIBILITY_NOTIFY, 0).u32(id).u8(0).done();
            self.deliver(id, M_VISIBILITY_CHANGE, ev);
            self.expose(id, &[whole]);
        }
        for child in children {
            self.expose_tree(child);
        }
    }

    // --- Operaciones sobre ventanas -----------------------------------------

    /// Cliente (distinto de `requester`) que redirige la subestructura de
    /// una ventana.
    pub fn redirector(&self, parent: Xid, requester: ClientId) -> Option<ClientId> {
        self.windows
            .get(&parent)?
            .masks
            .iter()
            .find_map(|(c, m)| (m & M_SUBSTRUCTURE_REDIRECT != 0 && *c != requester).then_some(*c))
    }

    /// `MapWindow`, con redirección al gestor de ventanas si lo hay.
    pub fn map_window(&mut self, requester: ClientId, id: Xid) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        if w.mapped || id == ROOT {
            return;
        }
        let (parent, redirect) = (w.parent, !w.override_redirect);
        if redirect && let Some(wm) = self.redirector(parent, requester) {
            let ev = Ev::new(MAP_REQUEST, 0).u32(parent).u32(id).done();
            self.send(wm, ev);
            return;
        }
        if parent == ROOT {
            self.wm_before_map(id);
        }
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
        w.mapped = true;
        let override_redirect = w.override_redirect;
        self.notify_structure(id, |event| {
            Ev::new(MAP_NOTIFY, 0)
                .u32(event)
                .u32(id)
                .u8(override_redirect as u8)
                .done()
        });
        if self.viewable(id) {
            self.expose_tree(id);
            self.damage_frame(id);
        }
        if parent == ROOT {
            self.wm_after_map(id);
        }
        self.refresh_pointer_window();
    }

    pub fn unmap_window(&mut self, id: Xid, from_configure: bool) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        if !w.mapped || id == ROOT {
            return;
        }
        self.damage_frame(id);
        if let Some(w) = self.windows.get_mut(&id) {
            w.mapped = false;
        }
        self.notify_structure(id, |event| {
            Ev::new(UNMAP_NOTIFY, 0)
                .u32(event)
                .u32(id)
                .u8(from_configure as u8)
                .done()
        });
        self.after_hidden(id);
    }

    /// Una ventana dejó de verse (o de existir): foco, capturas y puntero.
    fn after_hidden(&mut self, id: Xid) {
        let gone = |server: &Server, w: Xid| !server.viewable(w);
        if self.grab.as_ref().is_some_and(|g| gone(self, g.window)) {
            self.grab = None;
        }
        if self
            .keyboard_grab
            .as_ref()
            .is_some_and(|g| gone(self, g.window))
        {
            self.keyboard_grab = None;
        }
        if let Some(drag) = self.wm_drag {
            let window = match drag {
                WmDrag::Move { window, .. }
                | WmDrag::Resize { window, .. }
                | WmDrag::Close { window } => window,
            };
            if gone(self, window) {
                self.wm_drag = None;
            }
        }
        if let Focus::Window(f) = self.focus
            && gone(self, f)
        {
            self.wm_focus_lost(id);
        }
        self.refresh_pointer_window();
    }

    pub fn destroy_window(&mut self, id: Xid) {
        if id == ROOT || id == SERVER_WINDOW || !self.windows.contains_key(&id) {
            return;
        }
        self.unmap_window(id, false);
        self.destroy_recursive(id);
        self.after_hidden(id);
    }

    fn destroy_recursive(&mut self, id: Xid) {
        let children = self.windows.get(&id).map_or(vec![], |w| w.children.clone());
        for child in children {
            self.destroy_recursive(child);
        }
        self.notify_structure(id, |event| {
            Ev::new(DESTROY_NOTIFY, 0).u32(event).u32(id).done()
        });
        let Some(w) = self.windows.remove(&id) else {
            return;
        };
        if let Some(parent) = self.windows.get_mut(&w.parent) {
            parent.children.retain(|c| *c != id);
        }
        self.selections.retain(|_, s| s.window != id);
        for client in self.clients.values_mut() {
            client.save_set.retain(|s| *s != id);
        }
        if self.pointer_window == id {
            self.pointer_window = ROOT;
        }
    }

    /// Cambia posición, tamaño y borde (sin redirección ni apilamiento).
    pub fn move_resize(&mut self, id: Xid, x: i32, y: i32, w: u32, h: u32, bw: u32) {
        let Some(win) = self.windows.get(&id) else {
            return;
        };
        let (w, h) = (w.clamp(1, 32_767), h.clamp(1, 32_767));
        let unchanged = (win.x, win.y, win.w, win.h, win.bw) == (x, y, w, h, bw);
        if unchanged {
            return;
        }
        let resized = (win.w, win.h) != (w, h);
        let (old_w, old_h) = (win.w as i32, win.h as i32);
        let gravity = win.bit_gravity;
        self.damage_frame(id);
        let win = self.windows.get_mut(&id).expect("ventana");
        (win.x, win.y, win.bw) = (x, y, bw);
        if resized {
            (win.w, win.h) = (w, h);
            win.title_cache = None;
            if let Some(img) = &mut win.image {
                // Gravedad de bits «noroeste»: el contenido se queda donde
                // estaba. Con cualquier otra se redibuja todo.
                *img = img.resized(w, h, 0, 0);
            }
        }
        self.damage_frame(id);
        if resized && self.viewable(id) {
            let whole = Rect::new(0, 0, w as i32, h as i32);
            if gravity == 1 {
                let kept = Rect::new(0, 0, old_w, old_h);
                let fresh = whole.subtract(&kept);
                self.expose(id, &fresh);
            } else {
                self.expose(id, &[whole]);
            }
        }
    }

    /// Envía el `ConfigureNotify` con la geometría actual.
    pub fn notify_configure(&mut self, id: Xid) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        let above = self
            .windows
            .get(&w.parent)
            .and_then(|p| {
                let i = p.children.iter().position(|c| *c == id)?;
                i.checked_sub(1).map(|i| p.children[i])
            })
            .unwrap_or(0);
        let (x, y, ww, wh, bw, or) = (w.x, w.y, w.w, w.h, w.bw, w.override_redirect);
        self.notify_structure(id, |event| {
            Ev::new(CONFIGURE_NOTIFY, 0)
                .u32(event)
                .u32(id)
                .u32(above)
                .i16(x as i16)
                .i16(y as i16)
                .u16(ww as u16)
                .u16(wh as u16)
                .u16(bw as u16)
                .u8(or as u8)
                .done()
        });
    }

    /// Cambia el orden de apilamiento. `mode`: 0 encima, 1 debajo, 2 encima
    /// si la tapan, 3 debajo si tapa, 4 lo contrario.
    pub fn restack(&mut self, id: Xid, sibling: Option<Xid>, mode: u8) {
        let Some(parent) = self.windows.get(&id).map(|w| w.parent) else {
            return;
        };
        let Some(p) = self.windows.get_mut(&parent) else {
            return;
        };
        let Some(pos) = p.children.iter().position(|c| *c == id) else {
            return;
        };
        let top = pos + 1 == p.children.len();
        let above = match mode {
            0 | 2 => true,
            1 | 3 => false,
            _ => !top,
        };
        p.children.remove(pos);
        let index = match sibling.and_then(|s| p.children.iter().position(|c| *c == s)) {
            Some(i) if above => i + 1,
            Some(i) => i,
            None if above => p.children.len(),
            None => 0,
        };
        p.children.insert(index, id);
        if index != pos {
            self.damage_frame(id);
        }
    }

    pub fn reparent_window(&mut self, id: Xid, new_parent: Xid, x: i32, y: i32) -> XResult {
        if id == ROOT || id == new_parent || self.is_ancestor(id, new_parent) {
            return Err(XError::new(BAD_MATCH, id));
        }
        self.win(new_parent)?;
        let was_mapped = self.win(id)?.mapped;
        self.unmap_window(id, false);
        let old_parent = self.win(id)?.parent;
        if let Some(p) = self.windows.get_mut(&old_parent) {
            p.children.retain(|c| *c != id);
        }
        let w = self.win_mut(id)?;
        (w.parent, w.x, w.y) = (new_parent, x, y);
        w.title_cache = None;
        let override_redirect = w.override_redirect;
        self.win_mut(new_parent)?.children.push(id);
        let make = |event: Xid| {
            Ev::new(REPARENT_NOTIFY, 0)
                .u32(event)
                .u32(id)
                .u32(new_parent)
                .i16(x as i16)
                .i16(y as i16)
                .u8(override_redirect as u8)
                .done()
        };
        self.deliver(id, M_STRUCTURE_NOTIFY, make(id));
        self.deliver(old_parent, M_SUBSTRUCTURE_NOTIFY, make(old_parent));
        if new_parent != old_parent {
            self.deliver(new_parent, M_SUBSTRUCTURE_NOTIFY, make(new_parent));
        }
        if was_mapped {
            let owner = self.win(id)?.owner;
            self.map_window(owner, id);
        }
        Ok(())
    }

    // --- Propiedades --------------------------------------------------------

    /// Cambia una propiedad y avisa con `PropertyNotify`.
    pub fn set_property(&mut self, window: Xid, prop: Property) {
        let name = prop.name;
        let Some(w) = self.windows.get_mut(&window) else {
            return;
        };
        match w.props.iter_mut().find(|p| p.name == name) {
            Some(existing) => *existing = prop,
            None => w.props.push(prop),
        }
        self.property_changed(window, name, false);
    }

    pub fn delete_property(&mut self, window: Xid, name: Xid) {
        let Some(w) = self.windows.get_mut(&window) else {
            return;
        };
        let before = w.props.len();
        w.props.retain(|p| p.name != name);
        if w.props.len() != before {
            self.property_changed(window, name, true);
        }
    }

    pub fn property_changed(&mut self, window: Xid, name: Xid, deleted: bool) {
        let ev = Ev::new(PROPERTY_NOTIFY, 0)
            .u32(window)
            .u32(name)
            .u32(self.now())
            .u8(deleted as u8)
            .done();
        self.deliver(window, M_PROPERTY_CHANGE, ev);
        // El título de la decoración sale de estas propiedades.
        if name == A_WM_NAME || name == self.known.net_wm_name || name == self.known.motif_wm_hints
        {
            if let Some(w) = self.windows.get_mut(&window) {
                w.title_cache = None;
            }
            self.damage_frame(window);
        }
    }

    /// Texto de una propiedad de cadena (Latin-1 o UTF-8).
    pub fn text_property(&self, window: Xid, name: Xid) -> Option<String> {
        let prop = self.windows.get(&window)?.prop(name)?;
        if prop.format != 8 {
            return None;
        }
        Some(if prop.kind == self.known.utf8_string {
            String::from_utf8_lossy(&prop.data).into_owned()
        } else {
            prop.data.iter().map(|b| *b as char).collect()
        })
    }

    /// Valores de 32 bits de una propiedad.
    pub fn property_u32s(&self, window: Xid, name: Xid) -> Vec<u32> {
        self.windows
            .get(&window)
            .and_then(|w| w.prop(name))
            .filter(|p| p.format == 32)
            .map(|p| {
                p.data
                    .chunks_exact(4)
                    .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect()
            })
            .unwrap_or_default()
    }

    // --- Foco ---------------------------------------------------------------

    /// Cambia el foco del teclado generando `FocusOut`/`FocusIn`.
    pub fn set_focus(&mut self, new: Focus) {
        let old = self.focus;
        let new = match new {
            Focus::Window(w) if !self.viewable(w) => return,
            other => other,
        };
        if old == new {
            return;
        }
        self.focus = new;
        let event =
            |code: u8, detail: u8, window: Xid| Ev::new(code, detail).u32(window).u8(0).done();
        // Detalles: 0 antecesora, 1 virtual, 2 inferior, 3 no lineal,
        // 4 no lineal virtual, 6 PointerRoot, 7 ninguna.
        let root_detail = |f: Focus| if f == Focus::None { 7 } else { 6 };
        match (old, new) {
            (Focus::Window(a), Focus::Window(b)) => {
                let (chain_a, chain_b) = (self.ancestry(a), self.ancestry(b));
                if chain_a.contains(&b) {
                    // El foco sube a una antecesora.
                    self.deliver(a, M_FOCUS_CHANGE, event(FOCUS_OUT, 0, a));
                    for w in chain_a.iter().skip(1).take_while(|w| **w != b) {
                        self.deliver(*w, M_FOCUS_CHANGE, event(FOCUS_OUT, 1, *w));
                    }
                    self.deliver(b, M_FOCUS_CHANGE, event(FOCUS_IN, 2, b));
                } else if chain_b.contains(&a) {
                    // El foco baja a una descendiente.
                    self.deliver(a, M_FOCUS_CHANGE, event(FOCUS_OUT, 2, a));
                    let between: Vec<Xid> = chain_b
                        .iter()
                        .skip(1)
                        .take_while(|w| **w != a)
                        .copied()
                        .collect();
                    for w in between.iter().rev() {
                        self.deliver(*w, M_FOCUS_CHANGE, event(FOCUS_IN, 1, *w));
                    }
                    self.deliver(b, M_FOCUS_CHANGE, event(FOCUS_IN, 0, b));
                } else {
                    let common = chain_a
                        .iter()
                        .find(|w| chain_b.contains(w))
                        .copied()
                        .unwrap_or(ROOT);
                    self.deliver(a, M_FOCUS_CHANGE, event(FOCUS_OUT, 3, a));
                    for w in chain_a.iter().skip(1).take_while(|w| **w != common) {
                        self.deliver(*w, M_FOCUS_CHANGE, event(FOCUS_OUT, 4, *w));
                    }
                    let down: Vec<Xid> = chain_b
                        .iter()
                        .skip(1)
                        .take_while(|w| **w != common)
                        .copied()
                        .collect();
                    for w in down.iter().rev() {
                        self.deliver(*w, M_FOCUS_CHANGE, event(FOCUS_IN, 4, *w));
                    }
                    self.deliver(b, M_FOCUS_CHANGE, event(FOCUS_IN, 3, b));
                }
            }
            (Focus::Window(a), other) => {
                self.deliver(a, M_FOCUS_CHANGE, event(FOCUS_OUT, 3, a));
                for w in self.ancestry(a).iter().skip(1) {
                    if *w != ROOT {
                        self.deliver(*w, M_FOCUS_CHANGE, event(FOCUS_OUT, 4, *w));
                    }
                }
                self.deliver(
                    ROOT,
                    M_FOCUS_CHANGE,
                    event(FOCUS_IN, root_detail(other), ROOT),
                );
            }
            (other, Focus::Window(b)) => {
                self.deliver(
                    ROOT,
                    M_FOCUS_CHANGE,
                    event(FOCUS_OUT, root_detail(other), ROOT),
                );
                let chain = self.ancestry(b);
                for w in chain.iter().skip(1).rev() {
                    if *w != ROOT {
                        self.deliver(*w, M_FOCUS_CHANGE, event(FOCUS_IN, 4, *w));
                    }
                }
                self.deliver(b, M_FOCUS_CHANGE, event(FOCUS_IN, 3, b));
            }
            (a, b) => {
                self.deliver(ROOT, M_FOCUS_CHANGE, event(FOCUS_OUT, root_detail(a), ROOT));
                self.deliver(ROOT, M_FOCUS_CHANGE, event(FOCUS_IN, root_detail(b), ROOT));
            }
        }
        // La barra de título activa cambia de color.
        for w in [old, new] {
            if let Focus::Window(w) = w
                && let Some(top) = self.toplevel_of(w)
            {
                if let Some(win) = self.windows.get_mut(&top) {
                    win.title_cache = None;
                }
                self.damage_frame(top);
            }
        }
    }

    // --- Selecciones y portapapeles -----------------------------------------

    /// Cambia el dueño de una selección (0 = nadie), avisando al anterior.
    pub fn set_selection_owner(&mut self, selection: Xid, window: Xid, client: ClientId) {
        let time = self.now();
        if let Some(old) = self.selections.get(&selection).copied()
            && (old.window != window || old.client != client)
        {
            let ev = Ev::new(SELECTION_CLEAR, 0)
                .u32(time)
                .u32(old.window)
                .u32(selection)
                .done();
            self.send(old.client, ev);
        }
        if window == 0 {
            self.selections.remove(&selection);
            return;
        }
        self.selections.insert(
            selection,
            Selection {
                window,
                client,
                time,
            },
        );
        // Un cliente tomó el portapapeles: se le pide el texto.
        let wanted =
            selection == self.known.clipboard || (selection == A_PRIMARY && self.sync_primary);
        if client != 0 && wanted {
            let ev = Ev::new(SELECTION_REQUEST, 0)
                .u32(time)
                .u32(window)
                .u32(SERVER_WINDOW)
                .u32(selection)
                .u32(self.known.utf8_string)
                .u32(self.known.transfer)
                .done();
            self.send(client, ev);
        }
    }

    /// El portapapeles local tiene este texto: el servidor pasa a ser el
    /// dueño de CLIPBOARD y PRIMARY y contestará él las conversiones.
    pub fn offer_clipboard(&mut self, text: String) {
        // Si es el mismo texto que acaba de dar un cliente que sigue siendo
        // el dueño, no hay nada que ofrecer: se le quitaría la selección.
        let client_owns = self
            .selections
            .get(&self.known.clipboard)
            .is_some_and(|s| s.client != 0);
        if client_owns && self.clipboard_from_client.as_deref() == Some(&text) {
            return;
        }
        self.clipboard = Some(text);
        for selection in [self.known.clipboard, A_PRIMARY] {
            self.set_selection_owner(selection, SERVER_WINDOW, 0);
        }
    }

    /// Contesta una conversión de una selección que posee el servidor.
    /// Devuelve la propiedad donde dejó el resultado (0 si la rechaza).
    pub fn convert_own_selection(&mut self, requestor: Xid, target: Xid, property: Xid) -> Xid {
        let Some(text) = self.clipboard.clone() else {
            return 0;
        };
        let property = if property == 0 { target } else { property };
        let k = &self.known;
        let prop = if target == k.targets {
            let targets = [k.targets, k.utf8_string, A_STRING, k.text, k.timestamp];
            Property {
                name: property,
                kind: A_ATOM,
                format: 32,
                data: targets.iter().flat_map(|a| a.to_le_bytes()).collect(),
            }
        } else if target == k.utf8_string || target == k.text {
            Property {
                name: property,
                kind: k.utf8_string,
                format: 8,
                data: text.into_bytes(),
            }
        } else if target == A_STRING {
            Property {
                name: property,
                kind: A_STRING,
                format: 8,
                data: text
                    .chars()
                    .map(|c| if (c as u32) < 256 { c as u8 } else { b'?' })
                    .collect(),
            }
        } else if target == k.timestamp {
            Property {
                name: property,
                kind: 19, // INTEGER
                format: 32,
                data: self.now().to_le_bytes().to_vec(),
            }
        } else {
            return 0;
        };
        self.set_property(requestor, prop);
        property
    }

    /// Eventos dirigidos al propio servidor (su ventana interna).
    fn internal_event(&mut self, ev: Event) {
        if ev.code() != SELECTION_NOTIFY {
            return;
        }
        // SelectionNotify: time, requestor, selection, target, property.
        let (requestor, property) = (ev.u32_at(8), ev.u32_at(20));
        if requestor != SERVER_WINDOW || property == 0 {
            return;
        }
        let Some(prop) = self
            .windows
            .get(&SERVER_WINDOW)
            .and_then(|w| w.prop(property))
            .cloned()
        else {
            return;
        };
        // Borrar la propiedad es el acuse de recibo para el dueño.
        self.delete_property(SERVER_WINDOW, property);
        if prop.format != 8 || prop.kind == self.known.incr {
            return;
        }
        let text = if prop.kind == A_STRING {
            prop.data.iter().map(|b| *b as char).collect()
        } else {
            String::from_utf8_lossy(&prop.data).into_owned()
        };
        if !text.is_empty() && self.clipboard_from_client.as_deref() != Some(&text) {
            self.clipboard_from_client = Some(text.clone());
            self.desktop_events.push(DesktopEvent::ClipboardText(text));
        }
    }

    // --- Tamaño de pantalla -------------------------------------------------

    /// Cambia el tamaño de la pantalla raíz.
    pub fn resize_screen(&mut self, width: u32, height: u32) {
        let (width, height) = (width.clamp(64, 16_384), height.clamp(64, 16_384));
        if (width, height) == (self.width, self.height) {
            return;
        }
        (self.width, self.height) = (width, height);
        if let Some(root) = self.windows.get_mut(&ROOT) {
            (root.w, root.h) = (width, height);
            if let Some(img) = &mut root.image {
                *img = img.resized(width, height, 0, 0);
            }
        }
        self.resized = true;
        self.damage.clear();
        let whole = self.screen_rect();
        self.expose(ROOT, &[whole]);
        self.damage_screen(whole);
        self.notify_configure(ROOT);
        self.wm_keep_on_screen();
    }
}

/// Invierte el orden de bytes de los datos de una propiedad de 16 o 32 bits.
pub fn swap_property_data(data: &mut [u8], format: u8) {
    let unit = match format {
        16 => 2,
        32 => 4,
        _ => return,
    };
    for chunk in data.chunks_exact_mut(unit) {
        chunk.reverse();
    }
}
