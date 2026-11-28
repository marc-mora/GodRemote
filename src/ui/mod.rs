//! Interfaz gráfica: árbol de equipos, pestañas y diálogos.

pub mod auth_extras;
pub mod backdrop_view;
pub mod cast_view;
pub mod dashboard_view;
pub mod desktop_view;
pub mod editor;
pub mod fleet_view;
pub mod folder_editor;
pub mod history_view;
pub mod keys_view;
pub mod macros_view;
pub mod palette_view;
pub mod probe;
pub mod server_bar;
pub mod settings_view;
pub mod sftp_view;
mod term_tools;
pub mod themes_view;
pub mod transfer_view;
pub mod triggers_view;
pub mod tunnels_view;
pub mod util;
pub mod vault_view;
pub mod xwindows_view;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use egui::{Color32, RichText};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::desktop::{DesktopBackend, KeyCode};
use crate::macros::MacroStore;
use crate::model::{AuthMethod, Node, Protocol, Session, Tree};
use crate::sftp::SftpSource;
use crate::ssh::{Credentials, Endpoint, PromptRequest, Prompter, SecretAnswer, SshHandle, Waker};
use crate::terminal::{
    ConnState, INITIAL_COLS, INITIAL_ROWS, TermBackend, TermInput, TerminalView,
};
use crate::theme::{Settings, ThemeStore};
use crate::theme::{ok_color, warn_color};
use crate::tunnel_manager::{RunState, RunningTunnel, TunnelStore};
use crate::tunnels::TunnelStatus;
use crate::vault::Vault;
use desktop_view::{DesktopState, DesktopView};
use editor::{EditorResult, SessionEditor};
use fleet_view::{FleetRequest, FleetWindow};
use folder_editor::{FolderEditor, FolderResult};
use keys_view::KeysWindow;
use macros_view::{MacroRequest, MacrosWindow};
use palette_view::{Command, Palette, PaletteAction, PaletteItem, PaletteResult};
use server_bar::ServerBarWindow;
use settings_view::{ProxyVault, SettingsWindow};
use sftp_view::{SftpState, SftpView};
use themes_view::ThemesWindow;
use transfer_view::{TransferRequest, TransferWindow};
use tunnels_view::{TunnelRequest, TunnelView, TunnelsWindow};

enum TabKind {
    Terminal(Box<TerminalView>),
    Sftp(Box<SftpView>),
    /// Escritorio remoto integrado (RDP o VNC).
    Desktop(Box<DesktopView>),
}

struct Tab {
    id: u64,
    title: String,
    /// Sesión de origen (para reconectar o abrir SFTP); `None` = shell local.
    session: Option<Session>,
    kind: TabKind,
    /// Reconexión automática en marcha (solo terminales).
    reconnect: Option<Reconnect>,
    /// Pestaña de terminal de la que procede (una vista SFTP abierta
    /// sobre su conexión), para seguir su carpeta.
    follows: Option<u64>,
    /// Ha recibido salida mientras no se veía.
    unseen: bool,
    /// Ha sonado la campana mientras no se veía.
    rang: bool,
    /// El usuario le ha puesto nombre: se conserva al reabrirla.
    renamed: bool,
    /// Es la pantalla del servidor X integrado.
    x_server: bool,
    /// Ha saltado un disparador (o ha terminado una orden larga) sin verse.
    alert: Option<term_tools::TabAlert>,
}

/// Pestañas que se han pedido cerrar y tienen sesiones conectadas: se
/// pregunta antes.
struct PendingClose {
    ids: Vec<u64>,
    /// Títulos de las que siguen conectadas.
    live: Vec<String>,
    /// El botón «Cancelar» ya tiene el foco.
    focused: bool,
}

/// Acción pedida desde el menú contextual de una pestaña.
enum TabAction {
    Rename(usize),
    Duplicate(usize),
    MoveFirst(usize),
    MoveLast(usize),
    Close(usize),
    CloseOthers(usize),
    CloseRight(usize),
}

/// Reconexión automática de un terminal cuya conexión se cayó.
struct Reconnect {
    /// Intentos hechos hasta ahora.
    attempt: u32,
    /// Cuándo toca el siguiente intento; `None` si hay uno en curso.
    next: Option<Instant>,
}

/// Un túnel del gestor en marcha y, si su conexión se cayó, sus reintentos.
struct TunnelRun {
    running: RunningTunnel,
    retry: Option<Reconnect>,
}

/// Espera antes de cada intento de reconexión, en segundos.
const RECONNECT_DELAYS: [u64; 6] = [2, 5, 10, 20, 30, 60];

fn reconnect_delay(attempt: u32) -> Duration {
    Duration::from_secs(RECONNECT_DELAYS[(attempt as usize).min(RECONNECT_DELAYS.len() - 1)])
}

/// Un corte de red merece reintentar. Un fallo de autenticación o de
/// confianza en el servidor no: repetirlo no lo arregla y varios intentos
/// fallidos seguidos pueden bloquear la cuenta.
fn worth_retrying(error: &str) -> bool {
    let error = error.to_lowercase();
    ![
        "autenticación",
        "cancelad",
        "contraseña",
        "clave",
        "agente",
        "certificado",
        "known_hosts",
        "rechaz",
    ]
    .iter()
    .any(|word| error.contains(word))
}

/// Pestañas abiertas, para reabrirlas al arrancar.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct SavedTabs {
    #[serde(default, rename = "pestanas")]
    tabs: Vec<SavedTab>,
    #[serde(default, rename = "activa")]
    active: usize,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct SavedTab {
    /// Sesión del árbol; `None` es un terminal local.
    #[serde(rename = "sesion")]
    session: Option<Uuid>,
    /// `terminal`, `sftp` o `escritorio`.
    #[serde(rename = "tipo")]
    kind: String,
    /// Nombre que le puso el usuario, si la renombró.
    #[serde(default, rename = "titulo", skip_serializing_if = "Option::is_none")]
    title: Option<String>,
}

enum VaultDialog {
    Unlock {
        password: String,
        error: Option<String>,
        focus: bool,
    },
    Create {
        password: String,
        confirm: String,
        error: Option<String>,
    },
    Change {
        password: String,
        confirm: String,
        error: Option<String>,
    },
}

enum ActivePrompt {
    HostKey {
        host: String,
        port: u16,
        algorithm: String,
        fingerprint: String,
        reply: Option<oneshot::Sender<bool>>,
    },
    Secret {
        title: String,
        message: String,
        echo: bool,
        allow_save: bool,
        value: String,
        save: bool,
        focused: bool,
        reply: Option<oneshot::Sender<Option<SecretAnswer>>>,
    },
}

impl ActivePrompt {
    /// La conexión que preguntaba ya no existe (p. ej. se cerró la pestaña).
    fn abandoned(&self) -> bool {
        match self {
            ActivePrompt::HostKey { reply, .. } => reply.as_ref().is_none_or(|r| r.is_closed()),
            ActivePrompt::Secret { reply, .. } => reply.as_ref().is_none_or(|r| r.is_closed()),
        }
    }
}

enum TreeAction {
    Select(Uuid),
    Connect(Uuid),
    OpenAs(Uuid, Protocol),
    /// Abrir una sesión RDP con el cliente integrado (`true`) o el externo.
    OpenRdp(Uuid, bool),
    Edit(Uuid),
    Duplicate(Uuid),
    Delete(Uuid),
    NewSession(Option<Uuid>),
    NewFolder(Option<Uuid>),
    RenameFolder(Uuid),
    /// Ejecutar una orden en los equipos de una carpeta.
    RunOnFolder(Uuid),
    /// Abrir el panel de estado de los equipos de una carpeta.
    Dashboard(Uuid),
    Move(Uuid, Option<Uuid>),
}

struct RdpPrompt {
    session: Session,
    password: String,
    save: bool,
    focused: bool,
}

pub struct App {
    rt: tokio::runtime::Runtime,
    waker: Waker,
    tree: Tree,
    vault: Option<Vault>,
    vault_dialog: Option<VaultDialog>,
    tabs: Vec<Tab>,
    active: usize,
    next_tab_id: u64,
    prompter: Prompter,
    prompt_rx: Receiver<PromptRequest>,
    prompts: VecDeque<ActivePrompt>,
    notice_tx: Sender<(String, bool)>,
    notice_rx: Receiver<(String, bool)>,
    selected: Option<Uuid>,
    filter: String,
    editor: Option<SessionEditor>,
    folder_editor: Option<FolderEditor>,
    confirm_delete: Option<Uuid>,
    rdp_prompt: Option<RdpPrompt>,
    keys_window: Option<KeysWindow>,
    themes: ThemeStore,
    settings: Settings,
    /// Modo claro u oscuro del sistema, para el tema automático.
    system_theme: crate::system_theme::Watcher,
    /// Nombre del tema que está aplicado (`None`: hay que volver a aplicarlo).
    applied_theme: Option<String>,
    /// Prueba de arranque (captura y cierre), si se ha pedido.
    probe: Option<probe::StartupProbe>,
    /// Aviso de versión nueva.
    updates: crate::update_check::UpdateCheck,
    themes_window: Option<ThemesWindow>,
    server_bar_window: Option<ServerBarWindow>,
    settings_window: Option<SettingsWindow>,
    transfer_window: Option<TransferWindow>,
    macros: MacroStore,
    macros_window: Option<MacrosWindow>,
    /// Macro recién grabada, a la espera de nombre.
    recorded_macro: Option<RecordedMacro>,
    snippet_prompt: Option<SnippetPrompt>,
    /// Espacio disponible para el contenido de una pestaña (en puntos).
    content_size: egui::Vec2,
    toast: Option<(String, bool, Instant)>,
    quick_connect: String,
    broadcast: Broadcast,
    split: SplitView,
    /// Pestaña cuyo título se está editando: identificador, texto y si el
    /// campo ya tiene el foco.
    tab_rename: Option<(u64, String, bool)>,
    /// Pestaña que se está arrastrando para cambiarla de sitio.
    tab_drag: Option<u64>,
    /// Pestañas guardadas en disco la última vez.
    saved_tabs: SavedTabs,
    /// Pestañas por reabrir al arrancar (cuando el baúl ya esté resuelto).
    pending_restore: Option<SavedTabs>,
    /// Túneles guardados del gestor y los que están en marcha.
    tunnel_store: TunnelStore,
    tunnel_runs: HashMap<Uuid, TunnelRun>,
    tunnels_window: Option<TunnelsWindow>,
    /// Faltan por iniciar los túneles marcados para arrancar solos.
    pending_autostart: bool,
    /// Paleta de órdenes, si está abierta.
    palette: Option<Palette>,
    /// Ventana para ejecutar una orden en varios equipos.
    fleet_window: Option<FleetWindow>,
    /// Panel de estado de los equipos de una carpeta.
    dashboard_window: Option<dashboard_view::DashboardWindow>,
    /// Visor de lo guardado en el baúl.
    vault_window: Option<vault_view::VaultWindow>,
    /// Última vez que el usuario tocó el teclado o el ratón.
    last_activity: Instant,
    /// Proxy de salida que está en vigor ahora mismo.
    applied_proxy: Option<crate::proxy::ProxyConfig>,
    pending_close: Option<PendingClose>,
    /// Se ha pedido salir con sesiones vivas; `Some(true)` cuando el botón
    /// «Cancelar» de la pregunta ya tiene el foco.
    pending_quit: Option<bool>,
    /// El usuario ha confirmado que quiere salir.
    quit_confirmed: bool,
    /// Servidor X integrado (arranca con la primera aplicación gráfica).
    xhost: Arc<crate::xhost::XHost>,
    /// Ventanas nativas de sus aplicaciones, si van sueltas.
    x_windows: xwindows_view::XWindowsView,
    /// Tamaño que pide la pestaña del servidor X y desde cuándo: se aplica
    /// cuando deja de cambiar.
    x_resize: Option<((u32, u32), Instant)>,
    /// Disparadores y demás herramientas de los terminales.
    term_tools: term_tools::TermTools,
}

/// Vista dividida: varios terminales a la vez, en mosaico.
struct SplitView {
    enabled: bool,
    /// Mostrar todos los terminales abiertos (también los que se abran).
    all: bool,
    /// Pestañas elegidas, por identificador (si no son todos).
    chosen: std::collections::BTreeSet<u64>,
    /// Columnas del mosaico; 0 = las que mejor cuadren.
    columns: usize,
}

impl Default for SplitView {
    fn default() -> Self {
        Self {
            enabled: false,
            all: true,
            chosen: Default::default(),
            columns: 0,
        }
    }
}

impl SplitView {
    fn includes(&self, tab: u64) -> bool {
        self.enabled && (self.all || self.chosen.contains(&tab))
    }
}

/// Columnas y filas de un mosaico de `count` terminales. `columns` es la
/// preferencia del usuario (0 = automático).
fn split_grid(count: usize, columns: usize) -> (usize, usize) {
    let count = count.max(1);
    let columns = if columns == 0 {
        (count as f64).sqrt().ceil() as usize
    } else {
        columns
    }
    .clamp(1, count);
    (columns, count.div_ceil(columns))
}

/// Como máximo, estos terminales a la vez: más no caben con un tamaño útil.
const MAX_TILES: usize = 16;

/// Difusión: lo que se escribe en un terminal se repite en otros.
struct Broadcast {
    enabled: bool,
    /// A todos los terminales abiertos (también a los que se abran después).
    all: bool,
    /// Pestañas elegidas, por identificador (si no es a todos).
    chosen: std::collections::BTreeSet<u64>,
}

impl Default for Broadcast {
    fn default() -> Self {
        Self {
            enabled: false,
            all: true,
            chosen: Default::default(),
        }
    }
}

impl Broadcast {
    /// `true` si la pestaña participa en la difusión.
    fn includes(&self, tab: u64) -> bool {
        self.enabled && (self.all || self.chosen.contains(&tab))
    }
}

/// Snippet a la espera de los valores de sus preguntas.
struct SnippetPrompt {
    id: Uuid,
    name: String,
    /// Pregunta → respuesta (empieza con el valor por defecto).
    answers: Vec<(String, String)>,
    focused: bool,
}

struct RecordedMacro {
    name: String,
    bytes: Vec<u8>,
    focused: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: tokio::runtime::Runtime) -> Self {
        let themes = ThemeStore::load(&crate::paths::themes_dir());
        let mut settings = Settings::load(&crate::paths::settings_file());
        settings.fix_themes(|name| themes.exists(name));
        let ctx = cc.egui_ctx.clone();
        let waker: Waker = Arc::new(move || ctx.request_repaint());
        // Se pregunta ya al sistema si está en claro u oscuro, para no
        // arrancar con un tema y cambiar al momento.
        let system_theme = crate::system_theme::Watcher::start(waker.clone());
        let system_dark = if settings.theme_auto {
            system_theme.dark()
        } else {
            None
        };
        let applied_theme = settings.active_theme(system_dark).to_string();
        if let Some(theme) = themes.get(&applied_theme) {
            theme.apply(&cc.egui_ctx);
        }
        crate::highlight::set(&settings.highlight);
        crate::terminal::set_paste_guard(settings.confirm_paste);
        // El doble clic se mide entre fotogramas: con el margen habitual de
        // los sistemas (medio segundo) no se pierde si uno tarda en pintarse.
        cc.egui_ctx
            .options_mut(|o| o.input_options.max_double_click_delay = 0.5);
        // Con la prueba de arranque no se consulta nada fuera.
        let updates = crate::update_check::UpdateCheck::start(
            rt.handle(),
            settings.check_updates && std::env::var_os(probe::ENV).is_none(),
            waker.clone(),
        );
        let xhost = crate::xhost::XHost::new(rt.handle().clone(), waker.clone());
        xhost.configure(settings.x_server.embedded, settings.x_server.listen_local);
        crate::xhost::install(xhost.clone());
        let (prompt_tx, prompt_rx) = std::sync::mpsc::channel();
        let (notice_tx, notice_rx) = std::sync::mpsc::channel();

        let mut toast = None;
        let tree = match Tree::load(&crate::paths::sessions_file()) {
            Ok(t) => t,
            Err(e) => {
                toast = Some((
                    format!("No se pudieron leer las sesiones: {e:#}"),
                    true,
                    Instant::now(),
                ));
                Tree::default()
            }
        };
        let saved_tabs: SavedTabs = std::fs::read(crate::paths::state_file())
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        let restore_tabs = settings.restore_tabs && !saved_tabs.tabs.is_empty();
        let vault_dialog = if Vault::exists(&crate::paths::vault_file()) {
            VaultDialog::Unlock {
                password: String::new(),
                error: None,
                focus: true,
            }
        } else {
            VaultDialog::Create {
                password: String::new(),
                confirm: String::new(),
                error: None,
            }
        };

        Self {
            rt,
            term_tools: term_tools::TermTools::new(&settings, &tree),
            prompter: Prompter::new(prompt_tx, waker.clone()),
            waker,
            tree,
            vault: None,
            vault_dialog: Some(vault_dialog),
            tabs: vec![],
            active: 0,
            next_tab_id: 1,
            prompt_rx,
            prompts: VecDeque::new(),
            notice_tx,
            notice_rx,
            selected: None,
            filter: String::new(),
            editor: None,
            folder_editor: None,
            confirm_delete: None,
            rdp_prompt: None,
            keys_window: None,
            themes,
            settings,
            system_theme,
            applied_theme: Some(applied_theme),
            probe: probe::StartupProbe::from_env(),
            updates,
            themes_window: None,
            server_bar_window: None,
            settings_window: None,
            transfer_window: None,
            macros: MacroStore::load(&crate::paths::macros_file()).unwrap_or_default(),
            macros_window: None,
            recorded_macro: None,
            snippet_prompt: None,
            content_size: egui::vec2(1024.0, 700.0),
            toast,
            quick_connect: String::new(),
            broadcast: Broadcast::default(),
            split: SplitView::default(),
            tab_rename: None,
            tab_drag: None,
            pending_restore: restore_tabs.then(|| saved_tabs.clone()),
            saved_tabs,
            tunnel_store: TunnelStore::load(&crate::paths::tunnels_file()),
            tunnel_runs: HashMap::new(),
            tunnels_window: None,
            pending_autostart: true,
            palette: None,
            fleet_window: None,
            dashboard_window: None,
            vault_window: None,
            last_activity: Instant::now(),
            applied_proxy: None,
            pending_close: None,
            pending_quit: None,
            quit_confirmed: false,
            xhost,
            x_windows: Default::default(),
            x_resize: None,
        }
    }

    fn notify(&mut self, msg: impl Into<String>, is_error: bool) {
        self.toast = Some((msg.into(), is_error, Instant::now()));
    }

    fn save_tree(&mut self) {
        if let Err(e) = self.tree.save(&crate::paths::sessions_file()) {
            self.notify(format!("No se pudieron guardar las sesiones: {e:#}"), true);
        }
    }

    fn creds_for(&self, session: &Session) -> Credentials {
        match &self.vault {
            Some(v) => Credentials {
                password: v.get(&session.password_key()).map(str::to_string),
                passphrase: v.get(&session.passphrase_key()).map(str::to_string),
                totp: crate::totp::stored(session, v),
            },
            None => Credentials::default(),
        }
    }

    fn store_secrets(&mut self, secrets: Vec<(String, String)>) {
        if secrets.is_empty() {
            return;
        }
        let Some(vault) = &mut self.vault else {
            return;
        };
        let mut error = None;
        for (key, value) in secrets {
            if let Err(e) = vault.set(&key, &value) {
                error = Some(format!("No se pudo guardar en el baúl: {e:#}"));
            }
        }
        match error {
            Some(e) => self.notify(e, true),
            None => self.notify("Credencial guardada en el baúl", false),
        }
    }

    fn push_tab(&mut self, title: String, session: Option<Session>, kind: TabKind) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(Tab {
            id,
            title,
            session,
            kind,
            reconnect: None,
            follows: None,
            unseen: false,
            rang: false,
            renamed: false,
            x_server: false,
            alert: None,
        });
        self.active = self.tabs.len() - 1;
    }

    /// Cadena de saltos SSH de una sesión, con los secretos de cada salto.
    /// `None` si la conexión es directa.
    fn route_for(&self, session: &Session) -> Result<Option<Endpoint>, String> {
        let hops = self.tree.jump_path(session)?;
        Ok(Endpoint::route(hops.into_iter().map(|hop| {
            let creds = self.creds_for(&hop);
            (hop, creds)
        })))
    }

    /// Reúne lo necesario para conectar por SSH: secretos de la sesión y de
    /// cada salto (bastión) de su cadena.
    fn endpoint_for(&self, session: &Session) -> Result<Endpoint, String> {
        Ok(Endpoint {
            session: session.clone(),
            creds: self.creds_for(session),
            via: self.route_for(session)?.map(Box::new),
        })
    }

    /// Crea el terminal de una sesión según su protocolo.
    fn new_terminal(&self, session: &Session) -> Result<TerminalView, String> {
        let mut view = TerminalView::new(self.terminal_backend(session)?);
        view.set_backdrop(session.backdrop.as_ref());
        // Órdenes al conectar: solo por SSH (en Telnet o serie se
        // escribirían en la pregunta del usuario o de la contraseña).
        if session.protocol == Protocol::Ssh && !session.on_connect.trim().is_empty() {
            view.set_on_connect(on_connect_bytes(session));
        }
        view.log = self.automatic_log(&session.display_name(), &session.target());
        Ok(view)
    }

    /// Abre la conexión de un terminal según el protocolo de la sesión.
    fn terminal_backend(&self, session: &Session) -> Result<TermBackend, String> {
        Ok(match session.protocol {
            Protocol::Telnet => crate::telnet::spawn_via(
                self.rt.handle(),
                session.host.trim().to_string(),
                session.port,
                self.route_for(session)?,
                self.prompter.clone(),
                INITIAL_COLS,
                INITIAL_ROWS,
            ),
            Protocol::Serial => crate::serial::spawn(&session.serial, self.waker.clone())
                .map_err(|e| format!("{e:#}"))?,
            _ => crate::ssh::spawn_shell(
                self.rt.handle(),
                self.endpoint_for(session)?,
                self.prompter.clone(),
                INITIAL_COLS,
                INITIAL_ROWS,
            ),
        })
    }

    /// Vuelve a conectar el terminal de una pestaña, conservando su
    /// historial.
    fn restart_terminal(&mut self, index: usize, message: &str) -> Result<(), String> {
        let Some(session) = self.tabs.get(index).and_then(|t| t.session.clone()) else {
            return Ok(());
        };
        let backend = self.terminal_backend(&session)?;
        if let Some(TabKind::Terminal(term)) = self.tabs.get_mut(index).map(|t| &mut t.kind) {
            term.restart(backend, message);
        }
        Ok(())
    }

    /// Atiende al servidor X integrado: abre su pestaña cuando arranca, la
    /// quita si se para y ajusta su pantalla al espacio de la pestaña.
    fn sync_x_server(&mut self, ctx: &egui::Context) {
        // (Cada aplicación en su ventana, si se quiere y aquí se puede.)
        self.xhost.set_windows(
            self.settings.x_server.windows && xwindows_view::XWindowsView::available(ctx),
        );
        self.xhost.set_initial_size(
            self.content_size.x.round().max(0.0) as u32,
            (self.content_size.y - 30.0).round().max(0.0) as u32,
        );
        if let Some(backend) = self.xhost.take_backend() {
            // Una pantalla anterior ya no sirve.
            self.tabs.retain(|t| !t.x_server);
            let mut view = DesktopView::new(backend, true);
            view.clipboard_on_focus = true;
            self.push_tab("Servidor X".into(), None, TabKind::Desktop(Box::new(view)));
            if let Some(tab) = self.tabs.last_mut() {
                tab.x_server = true;
            }
            self.x_resize = None;
        }
        let Some(server) = self.xhost.running() else {
            // Parado (desde los ajustes): su pestaña ya no muestra nada.
            if let Some(index) = self.tabs.iter().position(|t| t.x_server) {
                self.close_tab(index);
            }
            return;
        };
        // Con las ventanas sueltas, cada una va en su ventana del sistema
        // (esté o no a la vista la pestaña) y la pantalla X es el escritorio.
        if server.rootless().is_some() {
            if let Some(Tab {
                kind: TabKind::Desktop(view),
                ..
            }) = self.tabs.iter_mut().find(|t| t.x_server)
            {
                self.x_windows.show(ctx, &server, view);
            }
            self.x_resize = None;
            return;
        }
        // La pantalla sigue al tamaño de la pestaña cuando deja de cambiar.
        let Some(Tab {
            kind: TabKind::Desktop(view),
            x_server: true,
            ..
        }) = self.tabs.get(self.active)
        else {
            return;
        };
        let wanted = (
            view.last_area.x.round() as u32,
            view.last_area.y.round() as u32,
        );
        if wanted.0 < 64 || wanted.1 < 64 || view.remote_size().is_none_or(|size| size == wanted) {
            self.x_resize = None;
            return;
        }
        match self.x_resize {
            Some((size, since)) if size == wanted => {
                if since.elapsed() >= Duration::from_millis(250) {
                    server.resize(wanted.0, wanted.1);
                    self.x_resize = None;
                } else {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
            }
            _ => {
                self.x_resize = Some((wanted, Instant::now()));
                ctx.request_repaint_after(Duration::from_millis(260));
            }
        }
    }

    /// Inicia un túnel del gestor sobre una conexión SSH propia.
    fn start_tunnel(&mut self, id: Uuid) -> Result<(), String> {
        let saved = self
            .tunnel_store
            .get(id)
            .cloned()
            .ok_or("El túnel ya no existe")?;
        saved.validate()?;
        let session = saved
            .session
            .and_then(|session| self.tree.resolved_session(session))
            .filter(|session| session.protocol.is_ssh())
            .ok_or("El equipo SSH del túnel ya no existe")?;
        let endpoint = self.endpoint_for(&session)?;
        let running = crate::tunnel_manager::spawn(
            self.rt.handle(),
            endpoint,
            saved.tunnel,
            self.prompter.clone(),
        );
        self.tunnel_runs.insert(
            id,
            TunnelRun {
                running,
                retry: None,
            },
        );
        Ok(())
    }

    /// Atiende a los túneles del gestor: su estado, los secretos que haya
    /// que guardar, el arranque automático y la reconexión tras una caída.
    fn poll_tunnels(&mut self, ctx: &egui::Context, secrets: &mut Vec<(String, String)>) {
        // Con el baúl aún sin abrir se pedirían contraseñas guardadas.
        if self.pending_autostart && self.vault_dialog.is_none() {
            self.pending_autostart = false;
            let ids: Vec<Uuid> = self
                .tunnel_store
                .tunnels
                .iter()
                .filter(|t| t.autostart)
                .map(|t| t.id)
                .collect();
            for id in ids {
                if let Err(e) = self.start_tunnel(id) {
                    let name = self
                        .tunnel_store
                        .get(id)
                        .map(|t| t.display_name())
                        .unwrap_or_default();
                    self.notify(format!("Túnel «{name}»: {e}"), true);
                }
            }
        }
        let now = Instant::now();
        let enabled = self.settings.reconnect;
        let max = self.settings.reconnect_attempts;
        let mut due = vec![];
        for (id, run) in &mut self.tunnel_runs {
            run.running.poll();
            secrets.append(&mut run.running.secrets_to_save);
            match &run.running.state {
                RunState::Connecting(_) => {}
                RunState::Active => run.retry = None,
                RunState::Failed(error) if enabled && worth_retrying(error) => {
                    match &mut run.retry {
                        // Solo se reintenta lo que llegó a funcionar.
                        None if run.running.was_active => {
                            run.retry = Some(Reconnect {
                                attempt: 0,
                                next: Some(now + reconnect_delay(0)),
                            });
                        }
                        None => {}
                        Some(r) => match r.next {
                            Some(at) if now >= at => due.push((*id, r.attempt + 1)),
                            Some(_) => ctx.request_repaint_after(Duration::from_millis(500)),
                            None if r.attempt >= max => {}
                            None => r.next = Some(now + reconnect_delay(r.attempt)),
                        },
                    }
                }
                RunState::Failed(_) | RunState::Stopped => run.retry = None,
            }
        }
        for (id, attempt) in due {
            match self.start_tunnel(id) {
                Ok(()) => {
                    if let Some(run) = self.tunnel_runs.get_mut(&id) {
                        run.retry = Some(Reconnect {
                            attempt,
                            next: None,
                        });
                    }
                }
                Err(_) => {
                    if let Some(run) = self.tunnel_runs.get_mut(&id) {
                        run.retry = None;
                    }
                }
            }
        }
    }

    /// Estado de cada túnel del gestor, para su ventana.
    fn tunnel_views(&self) -> HashMap<Uuid, TunnelView> {
        let now = Instant::now();
        let max = self.settings.reconnect_attempts;
        self.tunnel_runs
            .iter()
            .map(|(id, run)| {
                let retry = run.retry.as_ref().and_then(|r| match r.next {
                    Some(at) => Some(format!(
                        "Conexión perdida. Reintento {} de {max} en {} s…",
                        r.attempt + 1,
                        at.saturating_duration_since(now).as_secs() + 1
                    )),
                    None if matches!(run.running.state, RunState::Connecting(_)) => {
                        Some(format!("Reconectando… intento {} de {max}", r.attempt))
                    }
                    None => None,
                });
                (
                    *id,
                    TunnelView {
                        state: run.running.state.clone(),
                        retry,
                    },
                )
            })
            .collect()
    }

    fn tunnels_window_ui(&mut self, ctx: &egui::Context) {
        let Some(mut window) = self.tunnels_window.take() else {
            return;
        };
        let sessions = self.tree.ssh_sessions();
        let views = self.tunnel_views();
        let (open, requests) = window.show(ctx, &mut self.tunnel_store, &sessions, &views);
        for request in requests {
            match request {
                TunnelRequest::Start(id) => {
                    if let Err(e) = self.start_tunnel(id) {
                        self.notify(e, true);
                    }
                }
                TunnelRequest::Stop(id) => self.stop_tunnel(id),
                TunnelRequest::Removed(id) => {
                    self.tunnel_runs.remove(&id);
                }
            }
        }
        if open {
            self.tunnels_window = Some(window);
        } else {
            self.focus_active_terminal();
        }
    }

    /// Marca las pestañas de terminal que han recibido salida (o en las
    /// que ha sonado la campana) mientras no se veían.
    fn note_background_activity(&mut self) {
        let tiles = self.split_tiles();
        let split_visible = tiles.len() >= 2 && tiles.contains(&self.active);
        let notify = self.settings.activity_notice;
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            let TabKind::Terminal(term) = &mut tab.kind else {
                continue;
            };
            let (output, bell) = term.take_activity();
            let visible = index == self.active || (split_visible && tiles.contains(&index));
            if visible || !notify {
                tab.unseen = false;
                tab.rang = false;
            } else {
                tab.unseen |= output;
                tab.rang |= bell;
            }
        }
    }

    /// Pasa a las vistas SFTP el editor de los ajustes y, a las abiertas
    /// desde un terminal, la carpeta en la que está ese terminal.
    fn sync_sftp_views(&mut self) {
        let dirs: Vec<(u64, String)> = self
            .tabs
            .iter()
            .filter_map(|tab| match &tab.kind {
                TabKind::Terminal(t) => t.working_dir().map(|dir| (tab.id, dir)),
                _ => None,
            })
            .collect();
        for tab in &mut self.tabs {
            let TabKind::Sftp(view) = &mut tab.kind else {
                continue;
            };
            if view.editor != self.settings.editor {
                view.editor = self.settings.editor.clone();
            }
            if let Some(source) = tab.follows
                && let Some((_, dir)) = dirs.iter().find(|(id, _)| *id == source)
            {
                view.follow_dir(dir);
            }
        }
    }

    /// Reconexión automática: los terminales que han perdido la conexión se
    /// reintentan con esperas crecientes.
    fn auto_reconnect(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let enabled = self.settings.reconnect;
        let max = self.settings.reconnect_attempts;
        let mut due = vec![];
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            let TabKind::Terminal(term) = &tab.kind else {
                continue;
            };
            match &term.state {
                ConnState::Connecting => {}
                ConnState::Connected => tab.reconnect = None,
                ConnState::Closed(Some(error))
                    if enabled && tab.session.is_some() && worth_retrying(error) =>
                {
                    match &mut tab.reconnect {
                        // Solo se reintenta lo que llegó a estar conectado.
                        None if term.was_connected => {
                            tab.reconnect = Some(Reconnect {
                                attempt: 0,
                                next: Some(now + reconnect_delay(0)),
                            });
                        }
                        None => {}
                        Some(r) => match r.next {
                            Some(at) if now >= at => due.push((index, r.attempt + 1)),
                            // La cuenta atrás de la cabecera va por segundos.
                            Some(_) => ctx.request_repaint_after(Duration::from_millis(500)),
                            // El intento en curso ha fallado.
                            None if r.attempt >= max => {}
                            None => r.next = Some(now + reconnect_delay(r.attempt)),
                        },
                    }
                }
                ConnState::Closed(_) => tab.reconnect = None,
            }
        }
        for (index, attempt) in due {
            let message = format!("[Reconectando… intento {attempt} de {max}]");
            match self.restart_terminal(index, &message) {
                Ok(()) => {
                    self.tabs[index].reconnect = Some(Reconnect {
                        attempt,
                        next: None,
                    });
                }
                Err(e) => {
                    self.tabs[index].reconnect = None;
                    self.notify(e, true);
                }
            }
        }
    }

    /// Pestañas que se pueden reabrir: las de sesiones del árbol y los
    /// terminales locales.
    fn tabs_snapshot(&self) -> SavedTabs {
        let mut active = 0;
        let mut tabs = vec![];
        for (index, tab) in self.tabs.iter().enumerate() {
            let session = match &tab.session {
                Some(s) if self.tree.session(s.id).is_some() => Some(s.id),
                Some(_) => continue,
                None => None,
            };
            let kind = match &tab.kind {
                TabKind::Terminal(_) => "terminal",
                TabKind::Sftp(_) if session.is_some() => "sftp",
                TabKind::Desktop(_) if session.is_some() => "escritorio",
                _ => continue,
            };
            if index == self.active {
                active = tabs.len();
            }
            tabs.push(SavedTab {
                session,
                kind: kind.to_string(),
                title: tab.renamed.then(|| tab.title.clone()),
            });
        }
        SavedTabs { tabs, active }
    }

    /// Guarda las pestañas abiertas cuando cambian y, al arrancar, reabre
    /// las de la última vez si así se ha pedido.
    fn sync_saved_tabs(&mut self) {
        if let Some(saved) = self.pending_restore.take() {
            // Con el baúl aún sin abrir se pedirían contraseñas guardadas.
            if self.vault_dialog.is_some() {
                self.pending_restore = Some(saved);
                return;
            }
            for tab in &saved.tabs {
                let before = self.tabs.len();
                match (
                    tab.session.and_then(|id| self.tree.resolved_session(id)),
                    tab.kind.as_str(),
                ) {
                    (None, "terminal") if tab.session.is_none() => self.open_local_terminal(),
                    (Some(session), "terminal") => self.open_terminal(session),
                    (Some(session), "sftp") => self.open_sftp(session, None),
                    (Some(session), "escritorio") => self.connect(session, None),
                    _ => {}
                }
                // La pestaña recupera el nombre que se le hubiera puesto.
                if self.tabs.len() > before
                    && let (Some(title), Some(opened)) = (&tab.title, self.tabs.last_mut())
                {
                    opened.title = title.clone();
                    opened.renamed = true;
                }
            }
            if !self.tabs.is_empty() {
                self.active = saved.active.min(self.tabs.len() - 1);
            }
            return;
        }
        let snapshot = self.tabs_snapshot();
        if snapshot != self.saved_tabs {
            if let Ok(data) = serde_json::to_vec_pretty(&snapshot) {
                let _ = crate::paths::write_private(&crate::paths::state_file(), &data);
            }
            self.saved_tabs = snapshot;
        }
    }

    /// Registro de sesión para un terminal nuevo, si los ajustes piden
    /// registrar siempre.
    fn automatic_log(&self, name: &str, target: &str) -> Option<crate::session_log::SessionLog> {
        if !self.settings.log.always {
            return None;
        }
        match crate::session_log::SessionLog::start(
            &self.settings.log.dir(),
            name,
            target,
            self.settings.log.timestamps,
        ) {
            Ok(log) => Some(log),
            Err(e) => {
                let _ = self
                    .notice_tx
                    .send((format!("No se pudo registrar la sesión: {e:#}"), true));
                None
            }
        }
    }

    fn open_terminal(&mut self, session: Session) {
        match self.new_terminal(&session) {
            Ok(view) => self.push_tab(
                session.display_name(),
                Some(session),
                TabKind::Terminal(Box::new(view)),
            ),
            Err(e) => self.notify(e, true),
        }
    }

    /// Abre un terminal local con la shell de los ajustes.
    fn open_local_terminal(&mut self) {
        let command = self.settings.local_shell.clone();
        self.open_local_shell("Terminal local", &command);
    }

    /// Abre un terminal local con la shell indicada (vacía: la habitual).
    fn open_local_shell(&mut self, title: &str, command: &str) {
        match crate::terminal::spawn_local_command(
            command,
            self.waker.clone(),
            INITIAL_COLS,
            INITIAL_ROWS,
        ) {
            Ok(backend) => {
                let mut view = TerminalView::new(backend);
                view.log = self.automatic_log(title, "");
                self.push_tab(title.into(), None, TabKind::Terminal(Box::new(view)))
            }
            Err(e) => self.notify(
                format!("No se pudo abrir la shell local «{command}»: {e:#}"),
                true,
            ),
        }
    }

    /// Abre SFTP sobre la conexión de un terminal y enlaza la vista con él
    /// para que pueda seguir su carpeta.
    fn open_sftp_for_terminal(&mut self, session: Session, handle: Option<SshHandle>, tab: u64) {
        let before = self.tabs.len();
        let linked = handle.is_some();
        self.open_sftp(session, handle);
        if linked
            && self.tabs.len() > before
            && let Some(new) = self.tabs.last_mut()
        {
            new.follows = Some(tab);
            if let TabKind::Sftp(view) = &mut new.kind {
                view.linked = true;
            }
        }
    }

    fn open_sftp(&mut self, session: Session, shared: Option<SshHandle>) {
        let source = match shared {
            Some(handle) => SftpSource::Existing(handle),
            None => match self.endpoint_for(&session) {
                Ok(endpoint) => SftpSource::Connect(Box::new(endpoint)),
                Err(e) => return self.notify(e, true),
            },
        };
        let client = crate::sftp::spawn(self.rt.handle(), source, self.prompter.clone());
        self.push_tab(
            format!("{} (SFTP)", session.display_name()),
            Some(session),
            TabKind::Sftp(Box::new(SftpView::new(client))),
        );
    }

    /// Tamaño (en píxeles) que se pedirá para un escritorio RDP integrado.
    #[cfg(feature = "rdp-integrado")]
    fn desktop_size(&self, session: &Session) -> (u16, u16) {
        if session.rdp.fit_window {
            // El alto descuenta la barra de la pestaña.
            let w = self.content_size.x.clamp(640.0, 7680.0);
            let h = (self.content_size.y - 34.0).clamp(480.0, 4320.0);
            (w as u16, h as u16)
        } else {
            (
                session.rdp.width.clamp(640, 7680) as u16,
                session.rdp.height.clamp(480, 4320) as u16,
            )
        }
    }

    /// Crea la vista de un escritorio remoto integrado (RDP o VNC).
    fn new_desktop(&self, session: &Session) -> Result<DesktopView, String> {
        let password = self
            .vault
            .as_ref()
            .and_then(|v| v.get(&session.password_key()).map(str::to_string));
        let via = self.route_for(session)?;
        let backend: DesktopBackend = match session.protocol {
            Protocol::Vnc => crate::vnc::spawn_via(
                self.rt.handle(),
                session.clone(),
                password,
                via,
                self.prompter.clone(),
            ),
            #[cfg(feature = "rdp-integrado")]
            _ => crate::rdp_client::spawn_via(
                self.rt.handle(),
                session.clone(),
                password,
                self.desktop_size(session),
                via,
                self.prompter.clone(),
            ),
            #[cfg(not(feature = "rdp-integrado"))]
            _ => {
                let _ = (password, via);
                return Err("Esta edición no incluye el cliente RDP integrado".into());
            }
        };
        let clipboard = session.protocol == Protocol::Vnc || session.rdp.clipboard;
        Ok(DesktopView::new(backend, clipboard))
    }

    fn open_desktop(&mut self, session: Session) {
        match self.new_desktop(&session) {
            Ok(view) => self.push_tab(
                session.display_name(),
                Some(session),
                TabKind::Desktop(Box::new(view)),
            ),
            Err(e) => self.notify(e, true),
        }
    }

    fn open_rdp(&mut self, session: Session) {
        if !session.jumps.is_empty() {
            return self.notify(
                "Los saltos solo funcionan con el cliente RDP integrado: \
                 ábrelo integrado o quita los saltos de la sesión",
                true,
            );
        }
        let stored = self
            .vault
            .as_ref()
            .and_then(|v| v.get(&session.password_key()).map(str::to_string));
        if stored.is_some() || cfg!(windows) {
            // En Windows, mstsc gestiona él mismo las credenciales.
            self.launch_rdp(&session, stored.as_deref());
        } else {
            self.rdp_prompt = Some(RdpPrompt {
                session,
                password: String::new(),
                save: self.vault.is_some(),
                focused: false,
            });
        }
    }

    fn launch_rdp(&mut self, session: &Session, password: Option<&str>) {
        match crate::rdp::launch(session, password) {
            Ok((mut child, log)) => {
                self.notify(
                    format!("Cliente RDP lanzado para {}", session.display_name()),
                    false,
                );
                let name = session.display_name();
                let tx = self.notice_tx.clone();
                let waker = self.waker.clone();
                std::thread::spawn(move || {
                    let status = child.wait();
                    let failed = !status.as_ref().is_ok_and(|s| s.success());
                    let msg = if failed {
                        let detail = log
                            .and_then(|p| std::fs::read_to_string(p).ok())
                            .map(|text| rdp_error_summary(&text))
                            .unwrap_or_default();
                        format!("RDP «{name}» terminó con error. {detail}")
                    } else {
                        format!("Sesión RDP «{name}» finalizada")
                    };
                    let _ = tx.send((msg, failed));
                    waker();
                });
            }
            Err(e) => self.notify(format!("{e:#}"), true),
        }
    }

    /// Vuelve a leer la configuración del disco (tras restaurar una copia
    /// de seguridad). El baúl queda bloqueado: puede ser otro.
    fn reload_config(&mut self, ctx: &egui::Context) {
        match Tree::load(&crate::paths::sessions_file()) {
            Ok(tree) => self.tree = tree,
            Err(e) => self.notify(format!("No se pudieron leer las sesiones: {e:#}"), true),
        }
        self.themes = ThemeStore::load(&crate::paths::themes_dir());
        self.settings = Settings::load(&crate::paths::settings_file());
        self.settings.fix_themes(|name| self.themes.exists(name));
        // Puede ser otro tema, o el mismo nombre con otros colores.
        self.applied_theme = None;
        self.sync_theme(ctx);
        crate::highlight::set(&self.settings.highlight);
        crate::terminal::set_paste_guard(self.settings.confirm_paste);
        self.xhost.configure(
            self.settings.x_server.embedded,
            self.settings.x_server.listen_local,
        );
        self.macros = MacroStore::load(&crate::paths::macros_file()).unwrap_or_default();
        // Los túneles en marcha eran de la configuración anterior.
        self.tunnel_runs.clear();
        self.tunnel_store = TunnelStore::load(&crate::paths::tunnels_file());
        self.tunnels_window = None;
        self.dashboard_window = None;
        self.vault = None;
        self.selected = None;
        self.editor = None;
        self.folder_editor = None;
        self.themes_window = None;
        self.macros_window = None;
        if Vault::exists(&crate::paths::vault_file()) {
            self.vault_dialog = Some(VaultDialog::Unlock {
                password: String::new(),
                error: None,
                focus: true,
            });
        }
    }

    /// `true` si las sesiones RDP pueden abrirse con el cliente integrado:
    /// la edición lo incluye y los ajustes no lo desactivan.
    fn embedded_rdp_enabled(&self) -> bool {
        crate::HAS_EMBEDDED_RDP && self.settings.rdp_embedded
    }

    /// Abre una sesión con el protocolo indicado (o el suyo propio).
    fn connect(&mut self, session: Session, protocol: Option<Protocol>) {
        match protocol.unwrap_or(session.protocol) {
            Protocol::Ssh | Protocol::Telnet | Protocol::Serial => self.open_terminal(session),
            Protocol::Sftp => self.open_sftp(session, None),
            Protocol::Rdp if session.rdp.embedded && self.embedded_rdp_enabled() => {
                self.open_desktop(session)
            }
            Protocol::Rdp => self.open_rdp(session),
            Protocol::Vnc => self.open_desktop(session),
        }
    }

    fn apply_tree_actions(&mut self, actions: Vec<TreeAction>) {
        for action in actions {
            match action {
                TreeAction::Select(id) => self.selected = Some(id),
                TreeAction::Connect(id) => {
                    if let Some(s) = self.tree.resolved_session(id) {
                        self.connect(s, None);
                    }
                }
                TreeAction::OpenAs(id, protocol) => {
                    if let Some(s) = self.tree.resolved_session(id) {
                        self.connect(s, Some(protocol));
                    }
                }
                TreeAction::OpenRdp(id, embedded) => {
                    if embedded && !self.embedded_rdp_enabled() {
                        self.notify(
                            if crate::HAS_EMBEDDED_RDP {
                                "El cliente RDP integrado está desactivado en Ajustes"
                            } else {
                                "Esta edición no incluye el cliente RDP integrado"
                            },
                            true,
                        );
                    } else if let Some(mut s) = self.tree.resolved_session(id) {
                        s.rdp.embedded = embedded;
                        self.connect(s, None);
                    }
                }
                TreeAction::Edit(id) => {
                    if let Some(s) = self.tree.session(id).cloned() {
                        let parent = self.tree.parent_of(id);
                        self.editor = Some(SessionEditor::edit(s, parent));
                    }
                }
                TreeAction::Duplicate(id) => {
                    if let Some(original) = self.tree.session(id).cloned() {
                        let mut copy = original.clone();
                        copy.id = Uuid::new_v4();
                        copy.name = format!("{} (copia)", original.display_name());
                        if let Some(vault) = &mut self.vault {
                            for (from, to) in [
                                (original.password_key(), copy.password_key()),
                                (original.passphrase_key(), copy.passphrase_key()),
                                (original.totp_key(), copy.totp_key()),
                            ] {
                                if let Some(secret) = vault.get(&from).map(str::to_string) {
                                    let _ = vault.set(&to, &secret);
                                }
                            }
                        }
                        let parent = self.tree.parent_of(id);
                        self.tree.upsert_session(parent, copy);
                        self.save_tree();
                    }
                }
                TreeAction::Delete(id) => self.confirm_delete = Some(id),
                TreeAction::NewSession(parent) => self.editor = Some(self.new_session(parent)),
                TreeAction::NewFolder(parent) => {
                    self.folder_editor = Some(FolderEditor::new(parent));
                }
                TreeAction::RenameFolder(id) => {
                    if let Some(Node::Folder(f)) = self.tree.find(id) {
                        self.folder_editor = Some(FolderEditor::edit(f, self.tree.parent_of(id)));
                    }
                }
                TreeAction::RunOnFolder(id) => {
                    self.fleet_window = Some(FleetWindow::new(Some(id)));
                }
                TreeAction::Dashboard(id) => self.open_dashboard(Some(id)),
                TreeAction::Move(id, parent) => {
                    if self.tree.move_node(id, parent) {
                        self.save_tree();
                    } else {
                        self.notify("No se puede mover una carpeta dentro de sí misma", true);
                    }
                }
            }
        }
    }

    /// Carpeta donde crear cosas nuevas según la selección actual.
    fn target_folder(&self) -> Option<Uuid> {
        let id = self.selected?;
        match self.tree.find(id)? {
            Node::Folder(f) => Some(f.id),
            Node::Session(_) => self.tree.parent_of(id),
        }
    }

    // ------------------------------------------------------------------
    // Paneles
    // ------------------------------------------------------------------

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("GodRemote").strong().size(16.0));
            ui.separator();
            if ui
                .button("➕ Sesión")
                .on_hover_text("Nueva sesión SSH / SFTP / RDP")
                .clicked()
            {
                self.editor = Some(self.new_session(self.target_folder()));
            }
            if ui
                .button("📁 Carpeta")
                .on_hover_text("Nueva carpeta de equipos")
                .clicked()
            {
                self.folder_editor = Some(FolderEditor::new(self.target_folder()));
            }
            if ui.button("💻 Terminal local").clicked() {
                self.open_local_terminal();
            }
            // Las demás shells del equipo, para una sola pestaña.
            let mut chosen = None;
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let menu = ui.menu_button("   ", |ui| {
                    for shell in crate::shells::detect() {
                        if ui
                            .button(&shell.name)
                            .on_hover_text(&shell.command)
                            .clicked()
                        {
                            chosen = Some(shell);
                            ui.close();
                        }
                    }
                });
                // La flecha se dibuja: no todas las fuentes traen el signo.
                let center = menu.response.rect.center();
                ui.painter().add(egui::Shape::convex_polygon(
                    vec![
                        center + egui::vec2(-4.0, -2.0),
                        center + egui::vec2(4.0, -2.0),
                        center + egui::vec2(0.0, 3.0),
                    ],
                    ui.visuals().text_color(),
                    egui::Stroke::NONE,
                ));
                menu.response
                    .on_hover_text("Abrir un terminal local con otra shell");
            });
            if let Some(shell) = chosen {
                self.open_local_shell(&shell.name, &shell.command);
            }
            if ui
                .button("🔑 Claves")
                .on_hover_text("Gestionar claves SSH")
                .clicked()
            {
                self.keys_window = Some(KeysWindow::default());
            }
            self.macros_menu(ui);
            self.term_tools_menu(ui);
            let active = self
                .tunnel_runs
                .values()
                .filter(|run| run.running.state == RunState::Active)
                .count();
            let failed = self
                .tunnel_runs
                .values()
                .any(|run| matches!(run.running.state, RunState::Failed(_)));
            let mut label = RichText::new(match active {
                0 => "🔀 Túneles".to_string(),
                n => format!("🔀 Túneles ({n})"),
            });
            if failed {
                label = label.color(ui.visuals().error_fg_color);
            } else if active > 0 {
                label = label.color(ok_color());
            }
            if ui
                .button(label)
                .on_hover_text("Túneles SSH que se inician y se detienen sin abrir un terminal")
                .clicked()
            {
                self.tunnels_window = Some(TunnelsWindow::new(&self.tunnel_store));
            }
            if ui
                .button("🎨 Temas")
                .on_hover_text("Cambiar o personalizar el aspecto")
                .clicked()
            {
                self.open_themes_window();
            }
            if ui
                .button("⚙ Ajustes")
                .on_hover_text(
                    "Cliente RDP, barra de información del servidor y otras preferencias",
                )
                .clicked()
            {
                self.settings_window = Some(SettingsWindow::default());
            }
            let vault_exists = Vault::exists(&crate::paths::vault_file());
            ui.menu_button("🔐 Baúl", |ui| {
                if self.vault.is_some() {
                    if ui.button("Bloquear").clicked() {
                        self.vault = None;
                        ui.close();
                    }
                    if ui.button("Ver lo guardado…").clicked() {
                        self.vault_window = Some(Default::default());
                        ui.close();
                    }
                    if ui.button("Cambiar contraseña maestra…").clicked() {
                        self.vault_dialog = Some(VaultDialog::Change {
                            password: String::new(),
                            confirm: String::new(),
                            error: None,
                        });
                        ui.close();
                    }
                } else if vault_exists {
                    if ui.button("Desbloquear…").clicked() {
                        self.vault_dialog = Some(VaultDialog::Unlock {
                            password: String::new(),
                            error: None,
                            focus: true,
                        });
                        ui.close();
                    }
                } else if ui.button("Crear baúl…").clicked() {
                    self.vault_dialog = Some(VaultDialog::Create {
                        password: String::new(),
                        confirm: String::new(),
                        error: None,
                    });
                    ui.close();
                }
            });
        });
    }

    fn macros_menu(&mut self, ui: &mut egui::Ui) {
        let can_run = self.active_terminal().is_some();
        let recording = self.active_terminal().is_some_and(|t| t.is_recording());
        let mut run = None;
        let mut toggle_recording = false;
        let mut manage = false;
        let mut create = false;
        ui.menu_button("⚡ Snippets", |ui| {
            ui.set_min_width(260.0);
            if self.macros.macros.is_empty() {
                ui.weak("Aún no hay snippets");
            }
            for m in &self.macros.macros {
                let mut button = egui::Button::new(format!("▶ {}", m.name));
                if let Some(shortcut) = &m.shortcut {
                    button = button.shortcut_text(shortcut.to_string());
                }
                if ui
                    .add_enabled(can_run, button)
                    .on_disabled_hover_text("Abre un terminal conectado")
                    .clicked()
                {
                    run = Some(m.id);
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("➕ Nuevo snippet…").clicked() {
                create = true;
                ui.close();
            }
            let label = if recording {
                "⏹ Detener y guardar…"
            } else {
                "⏺ Grabar tecleando en el terminal activo"
            };
            if ui
                .add_enabled(can_run, egui::Button::new(label))
                .on_disabled_hover_text("Abre un terminal conectado")
                .clicked()
            {
                toggle_recording = true;
                ui.close();
            }
            if ui.button("Gestionar snippets y atajos…").clicked() {
                manage = true;
                ui.close();
            }
        })
        .response
        .on_hover_text("Textos y órdenes que se envían al terminal desde aquí o con un atajo");
        if let Some(id) = run {
            self.run_macro(id);
        }
        if toggle_recording {
            let mut finished = None;
            if let Some(term) = self.active_terminal() {
                if term.is_recording() {
                    finished = Some(term.stop_recording());
                } else {
                    term.start_recording();
                    term.request_focus();
                }
            }
            if let Some(bytes) = finished {
                self.finish_recording(bytes);
            }
        }
        if manage {
            self.macros_window = Some(MacrosWindow::new(&self.macros, None));
        }
        if create {
            self.macros_window = Some(MacrosWindow::new_snippet());
        }
    }

    fn recorded_macro_ui(&mut self, ctx: &egui::Context) {
        let Some(recorded) = &mut self.recorded_macro else {
            return;
        };
        let mut save = false;
        let mut close = false;
        egui::Modal::new(egui::Id::unique("recorded_macro")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading("Guardar snippet");
            ui.label("Nombre de lo que acabas de grabar:");
            let resp =
                ui.add(egui::TextEdit::singleline(&mut recorded.name).desired_width(f32::INFINITY));
            if !recorded.focused {
                resp.request_focus();
                recorded.focused = true;
            }
            ui.add(
                egui::Label::new(
                    RichText::new(crate::macros::encode(&recorded.bytes))
                        .monospace()
                        .weak(),
                )
                .truncate(),
            );
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let valid = !recorded.name.trim().is_empty();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(valid, egui::Button::new("Guardar"))
                    .clicked()
                    || (enter && valid)
                {
                    save = true;
                }
                if ui.button("Descartar").clicked()
                    || ui.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    close = true;
                }
            });
        });
        if save {
            if let Some(recorded) = self.recorded_macro.take() {
                let id = self
                    .macros
                    .add(&recorded.name, crate::macros::encode(&recorded.bytes));
                match self.macros.save(&crate::paths::macros_file()) {
                    // Se abre la ventana de snippets con el nuevo, por si se
                    // le quiere retocar o asignar un atajo.
                    Ok(()) => {
                        self.macros_window = Some(MacrosWindow::new(&self.macros, Some(id)));
                    }
                    Err(e) => self.notify(format!("No se pudo guardar el snippet: {e:#}"), true),
                }
            }
        } else if close {
            self.recorded_macro = None;
            self.focus_active_terminal();
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        if let Some((_, is_error, at)) = &self.toast {
            let ttl = if *is_error { 15 } else { 6 };
            if at.elapsed() > Duration::from_secs(ttl) {
                self.toast = None;
            } else {
                ui.ctx().request_repaint_after(Duration::from_secs(1));
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Primero el estado del baúl (derecha); el aviso ocupa el resto.
            match &self.vault {
                Some(v) => ui.colored_label(
                    ok_color(),
                    format!("🔓 Baúl abierto · {} secreto(s)", v.len()),
                ),
                None => ui.colored_label(warn_color(), "🔒 Baúl bloqueado"),
            };
            if self.settings.check_updates
                && let Some(version) = self.updates.newer()
                && ui
                    .link(format!("⬆ Versión {version} disponible"))
                    .on_hover_text("Abre la página de descargas en el navegador")
                    .clicked()
            {
                // (Si el navegador no se abre, la dirección sigue a la vista.)
                let _ = crate::paths::open_url(crate::update_check::RELEASES_URL);
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                if let Some((msg, is_error, _)) = &self.toast {
                    let color = if *is_error {
                        ui.visuals().error_fg_color
                    } else {
                        ok_color()
                    };
                    ui.add(egui::Label::new(RichText::new(msg).color(color)).truncate())
                        .on_hover_text(msg);
                }
            });
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text("🔍 Buscar equipo…")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(4.0);
        let mut actions = vec![];
        let folders = self.tree.folder_paths();
        egui::ScrollArea::vertical()
            .id_salt("tree_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.tree.nodes.is_empty() {
                    ui.add_space(12.0);
                    ui.weak("Aún no hay equipos.");
                    ui.weak("Crea una sesión con «➕ Sesión».");
                } else if self.filter.trim().is_empty() {
                    tree_ui(ui, &self.tree.nodes, self.selected, &folders, &mut actions);
                } else {
                    let needle = self.filter.trim().to_lowercase();
                    let mut matches = vec![];
                    collect_matches(&self.tree.nodes, &needle, &mut matches);
                    if matches.is_empty() {
                        ui.weak("Sin resultados.");
                    }
                    for s in matches {
                        session_row(ui, s, self.selected, &folders, &mut actions);
                    }
                }
                // Clic derecho en el hueco libre: crear en la raíz.
                let rest = ui.allocate_response(ui.available_size(), egui::Sense::click());
                rest.context_menu(|ui| {
                    if ui.button("➕ Nueva sesión").clicked() {
                        actions.push(TreeAction::NewSession(None));
                        ui.close();
                    }
                    if ui.button("📁 Nueva carpeta").clicked() {
                        actions.push(TreeAction::NewFolder(None));
                        ui.close();
                    }
                });
            });
        self.apply_tree_actions(actions);
    }

    /// Pestañas (por índice) que forman el mosaico de la vista dividida.
    fn split_tiles(&self) -> Vec<usize> {
        self.tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| {
                matches!(tab.kind, TabKind::Terminal(_)) && self.split.includes(tab.id)
            })
            .map(|(index, _)| index)
            .take(MAX_TILES)
            .collect()
    }

    /// Botón de la barra de pestañas que abre las opciones de la vista
    /// dividida.
    fn split_button(&mut self, ui: &mut egui::Ui) {
        use egui::containers::menu::{MenuButton, MenuConfig};
        let terminals: Vec<(u64, String)> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| matches!(tab.kind, TabKind::Terminal(_)))
            .map(|(i, tab)| (tab.id, format!("{}. {}", i + 1, tab.title)))
            .collect();
        let label = if self.split.enabled {
            RichText::new("⊞ Dividida").strong()
        } else {
            RichText::new("⊞ Dividir")
        };
        let config =
            MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        let was_enabled = self.split.enabled;
        let split = &mut self.split;
        let (response, _) = MenuButton::new(label).config(config).ui(ui, |ui| {
            ui.set_min_width(280.0);
            ui.set_max_width(340.0);
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            ui.checkbox(&mut split.enabled, "Ver varios terminales a la vez");
            ui.weak(
                "Los terminales se reparten en mosaico. Un clic en uno le da el teclado; \
                 doble clic en su título lo deja a pantalla completa.",
            );
            ui.add_enabled_ui(split.enabled, |ui| {
                ui.radio_value(&mut split.all, true, "Todos los terminales abiertos");
                ui.radio_value(&mut split.all, false, "Solo los que elija:");
                ui.add_enabled_ui(!split.all, |ui| {
                    ui.indent("split_tabs", |ui| {
                        for (id, title) in &terminals {
                            let mut on = split.chosen.contains(id);
                            if ui.checkbox(&mut on, title).changed() {
                                if on {
                                    split.chosen.insert(*id);
                                } else {
                                    split.chosen.remove(id);
                                }
                            }
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.label("Columnas");
                    ui.selectable_value(&mut split.columns, 0, "Auto");
                    for n in 1..=4 {
                        ui.selectable_value(&mut split.columns, n, n.to_string());
                    }
                });
            });
        });
        response.on_hover_text("Ver varios terminales a la vez, en mosaico");
        if self.split.enabled != was_enabled {
            self.focus_active_terminal();
        }
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut close = None;
        ui.horizontal(|ui| {
            self.split_button(ui);
            // En el mosaico los terminales no llevan su cabecera completa:
            // la difusión se maneja desde aquí.
            let tiles = self.split_tiles();
            if tiles.len() >= 2 && tiles.contains(&self.active) {
                let terminals: Vec<(u64, String, bool)> = self
                    .tabs
                    .iter()
                    .enumerate()
                    .filter_map(|(i, tab)| match &tab.kind {
                        TabKind::Terminal(t) => Some((
                            tab.id,
                            format!("{}. {}", i + 1, tab.title),
                            t.is_connected(),
                        )),
                        _ => None,
                    })
                    .collect();
                let current = self.tabs[self.active].id;
                broadcast_button(ui, &mut self.broadcast, &terminals, current);
            }
            ui.separator();
            self.tab_strip(ui, &mut close);
        });
        if let Some(i) = close {
            self.request_close(&[i]);
        }
    }

    fn tab_strip(&mut self, ui: &mut egui::Ui, close: &mut Option<usize>) {
        let mut activate = None;
        let mut action = None;
        // Fin de la edición de un título: guardar (true) o descartar.
        let mut rename_end = None;
        // Posición de cada pestaña en pantalla, para reordenar arrastrando.
        let mut rects: Vec<egui::Rect> = Vec::with_capacity(self.tabs.len());
        egui::ScrollArea::horizontal()
            .id_salt("tab_bar")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (i, tab) in self.tabs.iter().enumerate() {
                        let (icon, color) = match &tab.kind {
                            TabKind::Terminal(t) => (
                                tab.session.as_ref().map_or("💻", |s| s.protocol.icon()),
                                match t.state {
                                    ConnState::Connecting => warn_color(),
                                    ConnState::Connected => ok_color(),
                                    ConnState::Closed(_) => ui.visuals().error_fg_color,
                                },
                            ),
                            TabKind::Sftp(s) => (
                                "📂",
                                match s.state {
                                    SftpState::Connecting(_) => warn_color(),
                                    SftpState::Ready => ok_color(),
                                    SftpState::Closed(_) => ui.visuals().error_fg_color,
                                },
                            ),
                            TabKind::Desktop(d) => (
                                tab.session.as_ref().map_or("🖥", |s| s.protocol.icon()),
                                match d.state {
                                    DesktopState::Connecting(_) => warn_color(),
                                    DesktopState::Connected => ok_color(),
                                    DesktopState::Closed(_) => ui.visuals().error_fg_color,
                                },
                            ),
                        };
                        let scope = ui.scope(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            status_dot(ui, color);
                            // Título en edición: un campo de texto en su sitio.
                            if let Some((id, text, focused)) = &mut self.tab_rename
                                && *id == tab.id
                            {
                                let mut field = egui::TextEdit::singleline(text)
                                    .desired_width(150.0)
                                    .show(ui);
                                if !*focused {
                                    // Con todo el nombre seleccionado:
                                    // escribir lo sustituye.
                                    field.response.request_focus();
                                    let end = egui::text::CCursor::new(text.chars().count());
                                    field.state.cursor.set_char_range(Some(
                                        egui::text::CCursorRange::two(
                                            egui::text::CCursor::new(0),
                                            end,
                                        ),
                                    ));
                                    field.state.store(ui.ctx(), field.response.id);
                                    *focused = true;
                                } else if field.response.lost_focus() {
                                    let (escape, enter, tab, pointer) = ui.input(|i| {
                                        (
                                            i.key_pressed(egui::Key::Escape),
                                            i.key_pressed(egui::Key::Enter),
                                            i.key_pressed(egui::Key::Tab),
                                            i.pointer.any_pressed()
                                                || i.pointer.any_released()
                                                || i.pointer.any_down(),
                                        )
                                    });
                                    // La tecla que cierra la edición es del
                                    // campo: no debe llegar al terminal.
                                    ui.input_mut(|i| {
                                        i.events.retain(|event| {
                                            !matches!(
                                                event,
                                                egui::Event::Key {
                                                    key: egui::Key::Enter | egui::Key::Escape,
                                                    pressed: true,
                                                    ..
                                                }
                                            )
                                        })
                                    });
                                    if escape {
                                        // Escape lo deja como estaba.
                                        rename_end = Some(false);
                                    } else if enter || tab || pointer {
                                        // Intro o un clic fuera lo guardan.
                                        rename_end = Some(true);
                                    } else {
                                        // Otro control ha tomado el foco sin
                                        // que el usuario hiciera nada: se
                                        // sigue editando.
                                        field.response.request_focus();
                                    }
                                }
                                return;
                            }
                            let in_group = matches!(tab.kind, TabKind::Terminal(_))
                                && self.broadcast.includes(tab.id);
                            let bell = tab.mark();
                            let mut label = RichText::new(format!("{bell}{icon} {}", tab.title));
                            if in_group {
                                label = label.color(warn_color());
                            } else if tab.unseen || tab.rang || tab.alert.is_some() {
                                // Actividad sin ver: destacado.
                                label = label.strong().color(ui.visuals().hyperlink_color);
                            }
                            let resp = ui
                                .selectable_label(self.active == i, label)
                                .interact(egui::Sense::click_and_drag());
                            let hover = if in_group {
                                "Recibe lo que se escriba con la difusión activa"
                            } else if let Some(alert) = &tab.alert {
                                alert.text.as_str()
                            } else if tab.rang {
                                "Ha sonado la campana en este terminal"
                            } else if tab.unseen {
                                "Hay salida nueva sin ver"
                            } else {
                                "Arrastra para cambiarla de sitio; doble clic para renombrarla"
                            };
                            let resp = resp.on_hover_text(hover);
                            if resp.double_clicked() {
                                action = Some(TabAction::Rename(i));
                            } else if resp.clicked() {
                                activate = Some(i);
                            }
                            if resp.drag_started() {
                                self.tab_drag = Some(tab.id);
                            }
                            resp.context_menu(|ui| {
                                let last = self.tabs.len() - 1;
                                let mut item = |ui: &mut egui::Ui, text: &str, a: TabAction| {
                                    if ui.button(text).clicked() {
                                        action = Some(a);
                                        ui.close();
                                    }
                                };
                                item(ui, "✏ Renombrar…", TabAction::Rename(i));
                                item(ui, "Duplicar", TabAction::Duplicate(i));
                                ui.separator();
                                ui.add_enabled_ui(i > 0, |ui| {
                                    item(ui, "Mover al principio", TabAction::MoveFirst(i));
                                });
                                ui.add_enabled_ui(i < last, |ui| {
                                    item(ui, "Mover al final", TabAction::MoveLast(i));
                                });
                                ui.separator();
                                item(ui, "Cerrar", TabAction::Close(i));
                                ui.add_enabled_ui(last > 0, |ui| {
                                    item(ui, "Cerrar las demás", TabAction::CloseOthers(i));
                                });
                                ui.add_enabled_ui(i < last, |ui| {
                                    item(ui, "Cerrar las de la derecha", TabAction::CloseRight(i));
                                });
                            });
                            if resp.middle_clicked()
                                || ui
                                    .small_button("×")
                                    .on_hover_text("Cerrar pestaña")
                                    .clicked()
                            {
                                *close = Some(i);
                            }
                        });
                        rects.push(scope.response.rect);
                        // Color de la carpeta o del equipo, bajo la pestaña.
                        if let Some(color) = tab.session.as_ref().and_then(|s| s.color) {
                            let r = scope.response.rect;
                            ui.painter().rect_filled(
                                egui::Rect::from_min_max(
                                    egui::pos2(r.left(), r.bottom() - 1.0),
                                    egui::pos2(r.right(), r.bottom() + 2.0),
                                ),
                                1.0,
                                color.color32(),
                            );
                        }
                        ui.separator();
                    }
                });
            });
        if let Some(i) = activate {
            self.active = i;
            self.focus_active_terminal();
        }
        if let Some(keep) = rename_end {
            self.finish_tab_rename(keep);
        }
        self.drag_tab(ui, &rects);
        if let Some(action) = action {
            self.apply_tab_action(action);
        }
    }

    /// Termina la edición del título de una pestaña, guardándolo o no.
    fn finish_tab_rename(&mut self, keep: bool) {
        let Some((id, text, _)) = self.tab_rename.take() else {
            return;
        };
        let title = text.trim();
        if keep
            && !title.is_empty()
            && let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id)
        {
            tab.title = title.to_string();
            tab.renamed = true;
        }
        self.focus_active_terminal();
    }

    /// Reordena las pestañas mientras se arrastra una: cambia de sitio
    /// cuando el puntero pasa de la mitad de la vecina.
    fn drag_tab(&mut self, ui: &egui::Ui, rects: &[egui::Rect]) {
        let Some(id) = self.tab_drag else {
            return;
        };
        if !ui.input(|i| i.pointer.primary_down()) {
            self.tab_drag = None;
            return;
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        let (Some(from), Some(pointer)) = (
            self.tabs.iter().position(|t| t.id == id),
            ui.input(|i| i.pointer.latest_pos()),
        ) else {
            return;
        };
        let target = rects.iter().enumerate().find_map(|(i, rect)| {
            let passed = if i > from {
                pointer.x > rect.center().x
            } else {
                pointer.x < rect.center().x
            };
            (i != from && rect.x_range().contains(pointer.x) && passed).then_some(i)
        });
        if let Some(to) = target {
            self.move_tab(from, to);
        }
    }

    /// Mueve una pestaña de posición sin cambiar cuál es la activa.
    fn move_tab(&mut self, from: usize, to: usize) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let active = self.tabs.get(self.active).map(|t| t.id);
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        if let Some(index) = active.and_then(|id| self.tabs.iter().position(|t| t.id == id)) {
            self.active = index;
        }
    }

    fn apply_tab_action(&mut self, action: TabAction) {
        match action {
            TabAction::Rename(i) => {
                if let Some(tab) = self.tabs.get(i) {
                    self.tab_rename = Some((tab.id, tab.title.clone(), false));
                }
            }
            TabAction::Duplicate(i) => {
                let Some(tab) = self.tabs.get(i) else {
                    return;
                };
                match (tab.session.clone(), &tab.kind) {
                    // La pantalla del servidor X es única.
                    (None, TabKind::Desktop(_)) => {}
                    (None, _) => self.open_local_terminal(),
                    (Some(session), TabKind::Terminal(_)) => self.open_terminal(session),
                    (Some(session), TabKind::Sftp(_)) => self.open_sftp(session, None),
                    (Some(session), TabKind::Desktop(_)) => self.open_desktop(session),
                }
            }
            TabAction::MoveFirst(i) => self.move_tab(i, 0),
            TabAction::MoveLast(i) => self.move_tab(i, self.tabs.len().saturating_sub(1)),
            TabAction::Close(i) => self.request_close(&[i]),
            TabAction::CloseOthers(i) => {
                let others: Vec<usize> = (0..self.tabs.len()).filter(|j| *j != i).collect();
                self.request_close(&others);
            }
            TabAction::CloseRight(i) => {
                let right: Vec<usize> = (i + 1..self.tabs.len()).collect();
                self.request_close(&right);
            }
        }
    }

    /// Todo lo que se puede abrir desde la paleta de órdenes: primero las
    /// pestañas abiertas, luego las acciones y por último los equipos, los
    /// snippets y los túneles.
    fn palette_items(&self) -> Vec<PaletteItem> {
        let mut items = vec![];
        for (index, tab) in self.tabs.iter().enumerate() {
            if index == self.active {
                continue;
            }
            items.push(PaletteItem {
                kind: "Pestaña",
                label: tab.title.clone(),
                detail: tab.session.as_ref().map(|s| s.target()).unwrap_or_default(),
                action: PaletteAction::SwitchTab(tab.id),
            });
        }
        let has_terminal = matches!(
            self.tabs.get(self.active).map(|t| &t.kind),
            Some(TabKind::Terminal(_))
        );
        let mut command = |label: &str, detail: &str, command: Command| {
            items.push(PaletteItem {
                kind: "Acción",
                label: label.to_string(),
                detail: detail.to_string(),
                action: PaletteAction::Command(command),
            });
        };
        command(
            "Abrir un terminal local",
            "Ctrl+Mayús+T",
            Command::LocalTerminal,
        );
        if !self.tabs.is_empty() {
            command("Cerrar la pestaña", "Ctrl+Mayús+W", Command::CloseTab);
        }
        if has_terminal {
            command(
                "Buscar en el terminal",
                "Ctrl+Mayús+F",
                Command::SearchTerminal,
            );
            command(
                if self.broadcast.enabled {
                    "Desactivar la difusión"
                } else {
                    "Activar la difusión: escribir en varios terminales"
                },
                "",
                Command::ToggleBroadcast,
            );
        }
        command(
            if self.split.enabled {
                "Quitar la vista dividida"
            } else {
                "Vista dividida: varios terminales a la vez"
            },
            "",
            Command::ToggleSplit,
        );
        command("Nuevo equipo…", "", Command::NewSession);
        command("Nueva carpeta…", "", Command::NewFolder);
        command(
            "Ejecutar una orden en los equipos de una carpeta…",
            "",
            Command::RunOnFolder,
        );
        command(
            "Panel de estado de los equipos de una carpeta",
            "CPU, memoria, disco y alertas",
            Command::Dashboard,
        );
        command("Túneles", "gestor de túneles", Command::Tunnels);
        command("Snippets y atajos", "", Command::Snippets);
        command(
            "Disparadores",
            "avisar o contestar cuando aparezca un texto",
            Command::Triggers,
        );
        if let Some(recording) = self.active_cast() {
            command(
                if recording {
                    "Terminar la grabación de esta sesión"
                } else {
                    "Grabar esta sesión"
                },
                "asciicast, .cast",
                Command::ToggleCast,
            );
        }
        command(
            "Reproducir una grabación…",
            "asciicast, .cast",
            Command::PlayCast,
        );
        command(
            "Historial de órdenes",
            "las ejecutadas en cada equipo",
            Command::History,
        );
        command("Claves SSH", "", Command::Keys);
        command("Temas", "aspecto, colores y letra", Command::Themes);
        command("Ajustes", "preferencias", Command::Settings);
        command(
            "Importar equipos o hacer una copia de seguridad",
            "",
            Command::Transfer,
        );
        if self.settings.x_server.embedded {
            command(
                "Servidor X: abrir su pestaña",
                "aplicaciones gráficas",
                Command::XServer,
            );
        }
        if self.vault.is_some() {
            command(
                "Bloquear el baúl",
                "contraseñas guardadas",
                Command::VaultLock,
            );
            command("Ver lo guardado en el baúl", "", Command::VaultContents);
        } else if Vault::exists(&crate::paths::vault_file()) {
            command(
                "Desbloquear el baúl…",
                "contraseñas guardadas",
                Command::VaultUnlock,
            );
        }
        for (id, path) in self.tree.session_paths() {
            let Some(session) = self.tree.session(id) else {
                continue;
            };
            items.push(PaletteItem {
                kind: "Equipo",
                label: path,
                detail: format!("{} · {}", session.protocol.label(), session.target()),
                action: PaletteAction::Connect(id),
            });
        }
        if has_terminal {
            for snippet in &self.macros.macros {
                items.push(PaletteItem {
                    kind: "Snippet",
                    label: snippet.name.clone(),
                    detail: snippet.shortcut.map(|s| s.to_string()).unwrap_or_default(),
                    action: PaletteAction::RunSnippet(snippet.id),
                });
            }
        }
        for tunnel in &self.tunnel_store.tunnels {
            let running = self
                .tunnel_runs
                .get(&tunnel.id)
                .is_some_and(|run| run.running.is_running());
            items.push(PaletteItem {
                kind: "Túnel",
                label: format!(
                    "{} {}",
                    if running { "Detener" } else { "Iniciar" },
                    tunnel.display_name()
                ),
                detail: tunnel.tunnel.describe(),
                action: if running {
                    PaletteAction::StopTunnel(tunnel.id)
                } else {
                    PaletteAction::StartTunnel(tunnel.id)
                },
            });
        }
        self.history_palette_items(&mut items);
        items
    }

    fn palette_ui(&mut self, ctx: &egui::Context) {
        let Some(mut palette) = self.palette.take() else {
            return;
        };
        let items = self.palette_items();
        match palette.show(ctx, &items) {
            PaletteResult::Open => self.palette = Some(palette),
            PaletteResult::Closed => self.focus_active_terminal(),
            PaletteResult::Chosen(action) => {
                self.focus_active_terminal();
                self.run_palette_action(action);
            }
        }
    }

    fn run_palette_action(&mut self, action: PaletteAction) {
        match action {
            PaletteAction::SwitchTab(id) => {
                if let Some(index) = self.tabs.iter().position(|t| t.id == id) {
                    self.active = index;
                    self.focus_active_terminal();
                }
            }
            PaletteAction::Connect(id) => {
                if let Some(session) = self.tree.resolved_session(id) {
                    self.selected = Some(id);
                    self.connect(session, None);
                }
            }
            PaletteAction::RunSnippet(id) => self.run_macro(id),
            PaletteAction::StartTunnel(id) => {
                if let Err(e) = self.start_tunnel(id) {
                    self.notify(e, true);
                }
            }
            PaletteAction::StopTunnel(id) => self.stop_tunnel(id),
            PaletteAction::TypeCommand(command) => self.type_in_terminal(&command, false),
            PaletteAction::Command(command) => self.run_command(command),
        }
    }

    /// Ejecuta una acción de la aplicación (desde la paleta o un atajo).
    fn run_command(&mut self, command: Command) {
        match command {
            Command::LocalTerminal => self.open_local_terminal(),
            Command::NewSession => self.editor = Some(self.new_session(self.target_folder())),
            Command::NewFolder => {
                self.folder_editor = Some(FolderEditor::new(self.target_folder()));
            }
            Command::Keys => self.keys_window = Some(KeysWindow::default()),
            Command::Snippets => {
                self.macros_window = Some(MacrosWindow::new(&self.macros, None));
            }
            Command::Triggers => self.open_triggers_window(),
            Command::ToggleCast => self.toggle_active_cast(),
            Command::PlayCast => self.pick_cast(),
            Command::History => self.open_history_window(),
            Command::Tunnels => {
                self.tunnels_window = Some(TunnelsWindow::new(&self.tunnel_store));
            }
            Command::Themes => {
                self.open_themes_window();
            }
            Command::Settings => self.settings_window = Some(SettingsWindow::default()),
            Command::Transfer => self.transfer_window = Some(TransferWindow::default()),
            Command::VaultLock => self.vault = None,
            Command::VaultUnlock => {
                self.vault_dialog = Some(VaultDialog::Unlock {
                    password: String::new(),
                    error: None,
                    focus: true,
                });
            }
            Command::RunOnFolder => {
                self.fleet_window = Some(FleetWindow::new(self.target_folder()));
            }
            Command::Dashboard => self.open_dashboard(self.target_folder()),
            Command::VaultContents => {
                if self.vault.is_some() {
                    self.vault_window = Some(Default::default());
                }
            }
            Command::ToggleSplit => self.split.enabled = !self.split.enabled,
            Command::ToggleBroadcast => self.broadcast.enabled = !self.broadcast.enabled,
            Command::SearchTerminal => {
                if let Some(TabKind::Terminal(term)) =
                    self.tabs.get_mut(self.active).map(|t| &mut t.kind)
                {
                    term.open_search();
                }
            }
            Command::CloseTab => {
                if !self.tabs.is_empty() {
                    self.request_close(&[self.active]);
                }
            }
            Command::NextTab | Command::PreviousTab => {
                let count = self.tabs.len();
                if count > 1 {
                    self.active = if command == Command::NextTab {
                        (self.active + 1) % count
                    } else {
                        (self.active + count - 1) % count
                    };
                    self.focus_active_terminal();
                }
            }
            Command::XServer => {
                self.xhost.server();
                if let Some(index) = self.tabs.iter().position(|t| t.x_server) {
                    self.active = index;
                }
            }
        }
    }

    /// Modo del sistema (`Some(true)`: oscuro) si el tema lo sigue y se ha
    /// podido averiguar. (No se usa lo que diga la librería de ventanas: como
    /// la ventana lleva el modo del tema, podría devolvernos el nuestro.)
    fn system_dark(&self) -> Option<bool> {
        if self.settings.theme_auto {
            self.system_theme.dark()
        } else {
            None
        }
    }

    /// Nombre del tema que toca ahora.
    fn active_theme(&self) -> &str {
        self.settings.active_theme(self.system_dark())
    }

    fn open_themes_window(&mut self) {
        self.themes_window = Some(ThemesWindow::new(self.active_theme()));
    }

    /// Aplica el tema que toca si no es el que está puesto. Se llama en
    /// cada fotograma: con el tema automático, además, vuelve a preguntar
    /// al sistema de vez en cuando.
    fn sync_theme(&mut self, ctx: &egui::Context) {
        if self.settings.theme_auto {
            self.system_theme.refresh();
            // Aunque no pase nada en la ventana, se mira cada cierto tiempo.
            ctx.request_repaint_after(crate::system_theme::IDLE_INTERVAL);
        }
        let wanted = self.active_theme();
        if self.applied_theme.as_deref() != Some(wanted) {
            let wanted = wanted.to_string();
            if let Some(theme) = self.themes.get(&wanted) {
                theme.apply(ctx);
            }
            self.applied_theme = Some(wanted);
        }
    }

    /// Pone en vigor el proxy de salida de los ajustes, con la contraseña
    /// que haya en el baúl. Se llama en cada fotograma: solo actúa si algo
    /// ha cambiado (los ajustes, o el baúl al abrirse o bloquearse).
    fn sync_proxy(&mut self) {
        let wanted = self
            .settings
            .proxy
            .is_active()
            .then(|| crate::proxy::ProxyConfig {
                settings: self.settings.proxy.clone(),
                password: self
                    .vault
                    .as_ref()
                    .and_then(|v| v.get(crate::proxy::PASSWORD_KEY))
                    .map(str::to_string),
            });
        if wanted != self.applied_proxy {
            crate::proxy::set(wanted.clone());
            self.applied_proxy = wanted;
        }
    }

    /// Bloquea el baúl tras un rato sin actividad y muestra su visor.
    fn vault_guard(&mut self, ctx: &egui::Context) {
        self.sync_proxy();
        let active = ctx.input(|i| {
            !i.events.is_empty() || i.pointer.any_down() || i.pointer.delta() != egui::Vec2::ZERO
        });
        if active {
            self.last_activity = Instant::now();
        }
        let minutes = self.settings.vault_autolock;
        if minutes > 0 && self.vault.is_some() {
            let limit = Duration::from_secs(u64::from(minutes) * 60);
            let idle = self.last_activity.elapsed();
            if idle >= limit {
                self.vault = None;
                self.vault_window = None;
                self.notify("Baúl bloqueado por inactividad", false);
                // El estado ya se ha pintado en este fotograma: hace falta
                // otro para que se vea el cambio.
                ctx.request_repaint();
            } else {
                // Sin actividad no habría otro fotograma en el que mirarlo.
                ctx.request_repaint_after((limit - idle).min(Duration::from_secs(30)));
            }
        }
        if let Some(window) = &mut self.vault_window {
            let open = match &mut self.vault {
                Some(vault) => window.show(ctx, vault, &self.tree),
                None => false,
            };
            if !open {
                self.vault_window = None;
                self.focus_active_terminal();
            }
        }
    }

    fn fleet_window_ui(&mut self, ctx: &egui::Context) {
        let Some(mut window) = self.fleet_window.take() else {
            return;
        };
        let (open, request) = window.show(ctx, &self.tree, &self.macros);
        if let FleetRequest::Start {
            sessions,
            command,
            parallel,
            timeout_secs,
        } = request
        {
            let now = chrono::Local::now();
            let mut jobs = vec![];
            let mut problem = None;
            for id in sessions {
                let Some(session) = self.tree.resolved_session(id) else {
                    continue;
                };
                match self.endpoint_for(&session) {
                    Ok(endpoint) => {
                        let context = crate::macros::SnippetContext {
                            host: session.host.trim().to_string(),
                            port: session.port.to_string(),
                            user: session.username.trim().to_string(),
                            name: session.display_name(),
                        };
                        jobs.push(crate::fleet::HostJob {
                            id,
                            name: session.display_name(),
                            endpoint,
                            command: crate::macros::expand(&command, &context, &[], now),
                        });
                    }
                    Err(e) => problem = Some(format!("{}: {e}", session.display_name())),
                }
            }
            if let Some(problem) = problem {
                self.notify(problem, true);
            }
            if !jobs.is_empty() {
                window.run = Some(crate::fleet::run(
                    self.rt.handle(),
                    jobs,
                    self.prompter.clone(),
                    parallel,
                    Duration::from_secs(timeout_secs),
                ));
            }
        }
        if open {
            self.fleet_window = Some(window);
        } else {
            self.focus_active_terminal();
        }
    }

    /// Detiene un túnel del gestor.
    fn stop_tunnel(&mut self, id: Uuid) {
        if let Some(run) = self.tunnel_runs.get_mut(&id) {
            run.running.stop();
            run.retry = None;
            // Un reintento pendiente no deja nada que parar.
            if !run.running.is_running() {
                run.running.state = RunState::Stopped;
            }
        }
    }

    /// Cerrar esta pestaña interrumpe algo: una sesión remota conectada, un
    /// escritorio o aplicaciones gráficas abiertas.
    fn tab_is_live(&self, tab: &Tab) -> bool {
        match &tab.kind {
            TabKind::Terminal(term) => tab.session.is_some() && term.is_connected(),
            TabKind::Desktop(_) if tab.x_server => self
                .xhost
                .running()
                .is_some_and(|server| server.client_count() > 0),
            TabKind::Desktop(view) => view.state == DesktopState::Connected,
            TabKind::Sftp(view) => view.is_transferring(),
        }
    }

    /// Cierra las pestañas indicadas; si alguna tiene una sesión viva y los
    /// ajustes lo piden, pregunta antes.
    fn request_close(&mut self, indices: &[usize]) {
        let tabs: Vec<&Tab> = indices.iter().filter_map(|i| self.tabs.get(*i)).collect();
        let ids: Vec<u64> = tabs.iter().map(|t| t.id).collect();
        let live: Vec<String> = tabs
            .iter()
            .filter(|t| self.tab_is_live(t))
            .map(|t| t.title.clone())
            .collect();
        if live.is_empty() || !self.settings.confirm_close {
            self.close_tabs(&ids);
        } else {
            self.pending_close = Some(PendingClose {
                ids,
                live,
                focused: false,
            });
        }
    }

    fn close_tabs(&mut self, ids: &[u64]) {
        for id in ids {
            if let Some(index) = self.tabs.iter().position(|t| t.id == *id) {
                self.close_tab(index);
            }
        }
    }

    /// Pregunta antes de cerrar pestañas con sesiones vivas. El foco va a
    /// «Cancelar»: una tecla pulsada de más no cierra nada (ni llega al
    /// terminal).
    fn close_confirm_ui(&mut self, ctx: &egui::Context) {
        let Some(pending) = &mut self.pending_close else {
            return;
        };
        let mut decision = None;
        egui::Modal::new(egui::Id::unique("close_confirm")).show(ctx, |ui| {
            ui.set_width(400.0);
            match (pending.ids.len(), pending.live.as_slice()) {
                (1, [title]) => {
                    ui.heading(format!("¿Cerrar «{title}»?"));
                    ui.label(
                        "Tiene una sesión conectada o transferencias a medias: lo que esté en \
                         marcha se interrumpirá.",
                    );
                }
                (total, live) => {
                    ui.heading(format!("¿Cerrar {total} pestañas?"));
                    ui.label(format!(
                        "{} de ellas tienen sesiones conectadas o transferencias a medias; lo \
                         que esté en marcha se interrumpirá:",
                        live.len()
                    ));
                    for title in live.iter().take(8) {
                        ui.label(format!("  • {title}"));
                    }
                    if live.len() > 8 {
                        ui.weak(format!("  … y {} más", live.len() - 8));
                    }
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Cerrar").clicked() {
                    decision = Some(true);
                }
                let cancel = ui.button("Cancelar");
                if !pending.focused {
                    cancel.request_focus();
                    pending.focused = true;
                }
                if cancel.clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    decision = Some(false);
                }
            });
        });
        if let Some(close) = decision {
            let pending = self.pending_close.take();
            if close && let Some(pending) = pending {
                self.close_tabs(&pending.ids);
            }
            self.focus_active_terminal();
        }
    }

    /// Sesiones conectadas y túneles en marcha: lo que se perdería al salir.
    fn live_counts(&self) -> (usize, usize) {
        let sessions = self.tabs.iter().filter(|t| self.tab_is_live(t)).count();
        let tunnels = self
            .tunnel_runs
            .values()
            .filter(|run| run.running.is_running())
            .count();
        (sessions, tunnels)
    }

    /// Al pedir el cierre de la ventana con sesiones vivas, pregunta antes.
    fn quit_guard(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested())
            && !self.quit_confirmed
            && self.settings.confirm_close
        {
            let (sessions, tunnels) = self.live_counts();
            if sessions + tunnels > 0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.pending_quit.get_or_insert(false);
            }
        }
        if self.pending_quit.is_none() {
            return;
        }
        let (sessions, tunnels) = self.live_counts();
        let Some(focused) = &mut self.pending_quit else {
            return;
        };
        let mut decision = None;
        egui::Modal::new(egui::Id::unique("quit_confirm")).show(ctx, |ui| {
            ui.set_width(400.0);
            ui.heading("¿Salir de GodRemote?");
            let mut parts = vec![];
            match sessions {
                0 => {}
                1 => parts.push("1 pestaña con una sesión o transferencias en curso".to_string()),
                n => parts.push(format!(
                    "{n} pestañas con sesiones o transferencias en curso"
                )),
            }
            match tunnels {
                0 => {}
                1 => parts.push("1 túnel en marcha".to_string()),
                n => parts.push(format!("{n} túneles en marcha")),
            }
            ui.label(format!("Hay {}: se cerrarán.", parts.join(" y ")));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Salir").clicked() {
                    decision = Some(true);
                }
                let cancel = ui.button("Cancelar");
                if !*focused {
                    cancel.request_focus();
                    *focused = true;
                }
                if cancel.clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    decision = Some(false);
                }
            });
        });
        match decision {
            Some(true) => {
                self.pending_quit = None;
                self.quit_confirmed = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Some(false) => {
                self.pending_quit = None;
                self.focus_active_terminal();
            }
            None => {}
        }
    }

    /// Cierra una pestaña y deja el teclado en la que quede activa.
    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let active = self.tabs.get(self.active).map(|t| t.id);
        let closed = self.tabs.remove(index);
        if closed.x_server {
            // Cerrar su pestaña para el servidor X (y sus aplicaciones);
            // volverá a arrancar con la siguiente.
            self.xhost.stop();
        }
        self.broadcast.chosen.remove(&closed.id);
        self.split.chosen.remove(&closed.id);
        if self
            .tab_rename
            .as_ref()
            .is_some_and(|(id, ..)| *id == closed.id)
        {
            self.tab_rename = None;
        }
        // Si se cierra otra, la activa sigue siendo la misma.
        if let Some(index) = active
            .filter(|id| *id != closed.id)
            .and_then(|id| self.tabs.iter().position(|t| t.id == id))
        {
            self.active = index;
            self.focus_active_terminal();
            return;
        }
        if self.active >= index && self.active > 0 {
            self.active -= 1;
        }
        self.focus_active_terminal();
    }

    /// Vista dividida: los terminales de `tiles` repartidos en mosaico.
    fn split_view_ui(&mut self, ui: &mut egui::Ui, tiles: &[usize]) {
        let area = ui.available_rect_before_wrap();
        let (columns, rows) = split_grid(tiles.len(), self.split.columns);
        let gap = 4.0;
        let tile = egui::vec2(
            (area.width() - gap * (columns - 1) as f32) / columns as f32,
            (area.height() - gap * (rows - 1) as f32) / rows as f32,
        );
        let receivers = self
            .tabs
            .iter()
            .filter(|t| matches!(t.kind, TabKind::Terminal(_)) && self.broadcast.includes(t.id))
            .count();
        let accent = ui.visuals().selection.bg_fill;
        let mut mirrored: Option<(u64, Vec<TermInput>)> = None;
        let mut focused = None;
        let mut maximize = None;
        let mut reconnect = None;
        let mut close = None;
        let mut open_bar_settings = false;
        for (n, &index) in tiles.iter().enumerate() {
            let rect = egui::Rect::from_min_size(
                area.min
                    + egui::vec2(
                        (n % columns) as f32 * (tile.x + gap),
                        (n / columns) as f32 * (tile.y + gap),
                    ),
                tile,
            );
            let is_active = index == self.active;
            let Some(tab) = self.tabs.get_mut(index) else {
                continue;
            };
            let tab_id = tab.id;
            let TabKind::Terminal(term) = &mut tab.kind else {
                continue;
            };
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                ui.push_id(("tile", tab_id), |ui| {
                    // Cabecera compacta: estado, nombre y botones.
                    ui.horizontal(|ui| {
                        let color = match term.state {
                            ConnState::Connecting => warn_color(),
                            ConnState::Connected => ok_color(),
                            ConnState::Closed(_) => ui.visuals().error_fg_color,
                        };
                        status_dot(ui, color);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("×").on_hover_text("Cerrar").clicked() {
                                close = Some(index);
                            }
                            if ui
                                .small_button("⛶")
                                .on_hover_text("Ver solo este terminal")
                                .clicked()
                            {
                                maximize = Some(index);
                            }
                            if matches!(term.state, ConnState::Closed(_))
                                && tab.session.is_some()
                                && ui.small_button("⟳").on_hover_text("Reconectar").clicked()
                            {
                                reconnect = Some(index);
                            }
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    let mut title = RichText::new(&tab.title);
                                    if is_active {
                                        title = title.strong();
                                    }
                                    let label = ui.add(
                                        egui::Label::new(title)
                                            .truncate()
                                            .sense(egui::Sense::click()),
                                    );
                                    if label.double_clicked() {
                                        maximize = Some(index);
                                    } else if label.clicked() {
                                        term.request_focus();
                                        focused = Some(index);
                                    }
                                },
                            );
                        });
                    });
                    // Terminal y, si cabe, la barra de información.
                    let body = ui.available_rect_before_wrap();
                    let bar = tab
                        .session
                        .as_ref()
                        .filter(|s| wants_server_bar(&self.settings.server_bar, s))
                        .filter(|_| body.height() > 200.0);
                    let bar_height = match bar {
                        Some(_) => server_bar::height(ui),
                        None => 0.0,
                    };
                    let mut term_rect = body;
                    term_rect.max.y -= bar_height;
                    // Con la difusión activa, el terminal que tenga el
                    // teclado anota lo que se escribe para los demás.
                    let sending = self.broadcast.includes(tab_id) && receivers > 1;
                    term.mirror = sending.then(Vec::new);
                    ui.scope_builder(egui::UiBuilder::new().max_rect(term_rect), |ui| term.ui(ui));
                    if let Some(inputs) = term.mirror.take().filter(|i| !i.is_empty()) {
                        mirrored = Some((tab_id, inputs));
                    }
                    if term.has_focus && !is_active {
                        focused = Some(index);
                    }
                    // Marco: el de aviso si se escribe para varios; si no,
                    // el del terminal que tiene el teclado.
                    let frame = if sending {
                        Some(warn_color())
                    } else if is_active {
                        Some(accent)
                    } else {
                        None
                    };
                    if let Some(color) = frame {
                        ui.painter().rect_stroke(
                            term_rect,
                            0.0,
                            egui::Stroke::new(2.0, color),
                            egui::StrokeKind::Inside,
                        );
                    }
                    if let Some(session) = bar {
                        let mut bar_rect = body;
                        bar_rect.min.y = term_rect.max.y;
                        ui.scope_builder(egui::UiBuilder::new().max_rect(bar_rect), |ui| {
                            if server_bar::show(
                                ui,
                                &self.settings.server_bar,
                                session,
                                term.monitor.as_ref(),
                            ) {
                                open_bar_settings = true;
                            }
                        });
                    }
                });
            });
        }
        if let Some((source, inputs)) = mirrored {
            self.mirror_to_group(source, &inputs);
        }
        if let Some(index) = focused {
            self.active = index;
        }
        if let Some(index) = reconnect {
            match self.restart_terminal(index, "[Reconectando…]") {
                Ok(()) => self.tabs[index].reconnect = None,
                Err(e) => self.notify(e, true),
            }
        }
        if let Some(index) = maximize {
            self.active = index;
            self.split.enabled = false;
            self.focus_active_terminal();
        }
        if let Some(index) = close {
            self.close_tab(index);
        }
        if open_bar_settings {
            self.server_bar_window = Some(ServerBarWindow::new(&self.settings.server_bar));
        }
    }

    fn focus_active_terminal(&mut self) {
        match self.tabs.get_mut(self.active).map(|t| &mut t.kind) {
            Some(TabKind::Terminal(t)) => t.request_focus(),
            Some(TabKind::Desktop(d)) => d.request_focus(),
            _ => {}
        }
    }

    /// Terminal conectado de la pestaña activa, si lo hay.
    fn active_terminal(&mut self) -> Option<&mut TerminalView> {
        match self.tabs.get_mut(self.active).map(|t| &mut t.kind) {
            Some(TabKind::Terminal(t)) if t.is_connected() => Some(t),
            _ => None,
        }
    }

    /// Repite en los demás terminales del grupo de difusión lo que se ha
    /// introducido en la pestaña `source`.
    fn mirror_to_group(&mut self, source: u64, inputs: &[TermInput]) {
        if inputs.is_empty() || !self.broadcast.includes(source) {
            return;
        }
        for tab in &mut self.tabs {
            if tab.id == source || !self.broadcast.includes(tab.id) {
                continue;
            }
            if let TabKind::Terminal(term) = &mut tab.kind {
                for input in inputs {
                    term.apply_input(input);
                }
            }
        }
    }

    /// Lanza un snippet. Si tiene preguntas (`{{?…}}`) se piden antes sus
    /// valores.
    fn run_macro(&mut self, id: Uuid) {
        if self.active_terminal().is_none() {
            return self.notify("Abre un terminal conectado para lanzar el snippet", true);
        }
        let Some(snippet) = self.macros.macros.iter().find(|m| m.id == id) else {
            return;
        };
        let questions = crate::macros::questions(&snippet.text);
        if questions.is_empty() {
            self.launch_snippet(id, &[]);
        } else {
            self.snippet_prompt = Some(SnippetPrompt {
                id,
                name: snippet.name.clone(),
                answers: questions,
                focused: false,
            });
        }
    }

    /// Envía un snippet al terminal activo y, con la difusión activa, a los
    /// demás terminales del grupo. Las variables se sustituyen con los
    /// datos de cada terminal.
    fn launch_snippet(&mut self, id: Uuid, answers: &[(String, String)]) {
        let Some(text) = self
            .macros
            .macros
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.text.clone())
        else {
            return;
        };
        let now = chrono::Local::now();
        let Some(source) = self.tabs.get(self.active).map(|t| t.id) else {
            return;
        };
        let broadcasting = self.broadcast.includes(source);
        for tab in &mut self.tabs {
            let is_source = tab.id == source;
            if !is_source && !(broadcasting && self.broadcast.includes(tab.id)) {
                continue;
            }
            let context = snippet_context(tab);
            let TabKind::Terminal(term) = &mut tab.kind else {
                continue;
            };
            let expanded = crate::macros::expand(&text, &context, answers, now);
            let bytes = crate::macros::decode(&expanded);
            if is_source {
                term.send_macro(bytes);
                term.request_focus();
            } else {
                term.apply_input(&TermInput::Bytes(bytes));
            }
        }
    }

    /// Diálogo que pide los valores de las preguntas de un snippet.
    fn snippet_prompt_ui(&mut self, ctx: &egui::Context) {
        let Some(prompt) = &mut self.snippet_prompt else {
            return;
        };
        let mut launch = false;
        let mut cancel = false;
        egui::Modal::new(egui::Id::unique("snippet_prompt")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading(format!("Lanzar «{}»", prompt.name));
            egui::Grid::new("snippet_prompt_grid")
                .num_columns(2)
                .show(ui, |ui| {
                    for (i, (label, value)) in prompt.answers.iter_mut().enumerate() {
                        ui.label(label.as_str());
                        let response =
                            ui.add(egui::TextEdit::singleline(value).desired_width(280.0));
                        if i == 0 && !prompt.focused {
                            response.request_focus();
                            prompt.focused = true;
                        }
                        ui.end_row();
                    }
                });
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.horizontal(|ui| {
                if ui.button("▶ Lanzar").clicked() || enter {
                    launch = true;
                }
                if ui.button("Cancelar").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    cancel = true;
                }
            });
        });
        if launch {
            if let Some(prompt) = self.snippet_prompt.take() {
                self.launch_snippet(prompt.id, &prompt.answers);
            }
            self.focus_active_terminal();
        } else if cancel {
            self.snippet_prompt = None;
            self.focus_active_terminal();
        }
    }

    /// Lanza el snippet cuyo atajo se acaba de pulsar. Solo con el foco en
    /// un terminal conectado: en un diálogo o un campo de texto las teclas
    /// son de quien las está usando.
    fn dispatch_shortcuts(&mut self, ctx: &egui::Context) {
        if self.macros_window.is_some()
            || self.recorded_macro.is_some()
            || self.snippet_prompt.is_some()
        {
            return;
        }
        // Los atajos de la aplicación valen con el foco en cualquier parte,
        // salvo con una pregunta en pantalla.
        let asking = self.palette.is_some()
            || self.pending_close.is_some()
            || self.pending_quit.is_some()
            || self.vault_dialog.is_some()
            || !self.prompts.is_empty();
        if self.settings.app_shortcuts
            && !asking
            && let Some(shortcut) = ctx.input_mut(|i| crate::macros::app_shortcut(&mut i.events))
        {
            use crate::macros::AppShortcut;
            self.run_command(match shortcut {
                AppShortcut::Palette => {
                    self.palette = Some(Palette::default());
                    return;
                }
                AppShortcut::NextTab => Command::NextTab,
                AppShortcut::PreviousTab => Command::PreviousTab,
                AppShortcut::NewTerminal => Command::LocalTerminal,
                AppShortcut::CloseTab => Command::CloseTab,
            });
            return;
        }
        let in_terminal = matches!(
            self.tabs.get(self.active).map(|t| &t.kind),
            Some(TabKind::Terminal(t)) if t.is_connected() && t.has_focus
        );
        if !in_terminal {
            return;
        }
        let triggered = ctx.input_mut(|i| self.macros.triggered(&mut i.events));
        if let Some(id) = triggered {
            self.run_macro(id);
        }
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.18);
            ui.label(RichText::new("GodRemote").size(34.0).strong());
            ui.weak("Gestor de conexiones remotas · SSH · SFTP · RDP · VNC · Telnet · Serie");
            ui.add_space(24.0);
            ui.label("Conexión rápida");
            let mut go = None;
            ui.allocate_ui(egui::vec2(520.0, 28.0), |ui| {
                ui.horizontal(|ui| {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.quick_connect)
                            .hint_text("usuario@host[:puerto]")
                            .desired_width(300.0),
                    );
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        go = Some(Protocol::Ssh);
                    }
                    for p in Protocol::ALL {
                        if p != Protocol::Serial && ui.button(p.label()).clicked() {
                            go = Some(p);
                        }
                    }
                });
            });
            if let Some(protocol) = go {
                match util::parse_quick_connect(&self.quick_connect, protocol) {
                    Ok(session) => self.connect(session, None),
                    Err(e) => self.notify(e, true),
                }
            }
            ui.add_space(24.0);
            ui.weak("Doble clic en un equipo del árbol para conectar.");
            ui.weak("Clic derecho para editar, mover, duplicar o abrir SFTP.");
        });
    }

    fn active_tab_ui(&mut self, ui: &mut egui::Ui) {
        let mut open_sftp: Option<(Session, Option<SshHandle>)> = None;
        let mut reconnect = false;
        let mut recorded: Option<Vec<u8>> = None;
        let mut open_bar_settings = false;
        // Terminales abiertos (para el menú de difusión) y cuántos reciben
        // ahora mismo lo que se escriba.
        let terminals: Vec<(u64, String, bool)> = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(i, tab)| match &tab.kind {
                // Con el número de pestaña, para distinguir títulos iguales.
                TabKind::Terminal(t) => Some((
                    tab.id,
                    format!("{}. {}", i + 1, tab.title),
                    t.is_connected(),
                )),
                _ => None,
            })
            .collect();
        let mut mirrored: Vec<TermInput> = vec![];
        let mut notice: Option<(String, bool)> = None;
        let mut cancel_reconnect = false;
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        // Cada pestaña tiene su propio espacio de ids (foco, scroll…).
        let tab_id = tab.id;
        ui.push_id(tab_id, |ui| match &mut tab.kind {
            TabKind::Terminal(term) => {
                ui.horizontal(|ui| {
                    match &term.state {
                        ConnState::Connecting => {
                            ui.spinner();
                            ui.label("Conectando…");
                        }
                        ConnState::Connected => {
                            status_dot(ui, ok_color());
                            ui.colored_label(ok_color(), "Conectado");
                        }
                        ConnState::Closed(_) => {
                            status_dot(ui, ui.visuals().error_fg_color);
                            ui.colored_label(ui.visuals().error_fg_color, "Desconectado");
                            if let Some(r) = &tab.reconnect {
                                let max = self.settings.reconnect_attempts;
                                match r.next {
                                    Some(at) => {
                                        let secs =
                                            at.saturating_duration_since(Instant::now()).as_secs()
                                                + 1;
                                        ui.colored_label(
                                            warn_color(),
                                            format!(
                                                "Reconectando en {secs} s (intento {} de {max})",
                                                r.attempt + 1
                                            ),
                                        );
                                        if ui.small_button("Cancelar").clicked() {
                                            cancel_reconnect = true;
                                        }
                                    }
                                    None => {
                                        ui.weak("No se pudo reconectar");
                                    }
                                }
                            }
                            if tab.session.is_some() && ui.button("⟳ Reconectar").clicked() {
                                reconnect = true;
                            }
                        }
                    }
                    if let Some(s) = &tab.session {
                        ui.weak(s.target());
                        if s.protocol.is_ssh()
                            && term.state == ConnState::Connected
                            && ui
                                .button("📂 SFTP")
                                .on_hover_text("Explorar ficheros sobre esta misma conexión")
                                .clicked()
                        {
                            open_sftp = Some((s.clone(), term.ssh.clone()));
                        }
                        if !term.tunnels.is_empty() {
                            let active = term
                                .tunnels
                                .iter()
                                .filter(|t| matches!(t, Some(TunnelStatus::Active)))
                                .count();
                            let all_ok = active == term.tunnels.len();
                            let color = if all_ok { ok_color() } else { warn_color() };
                            let details: Vec<String> = term
                                .tunnels
                                .iter()
                                .enumerate()
                                .map(|(i, status)| {
                                    let what =
                                        s.tunnels.get(i).map(|t| t.describe()).unwrap_or_default();
                                    match status {
                                        Some(TunnelStatus::Active) => format!("Activo: {what}"),
                                        Some(TunnelStatus::Failed(e)) => {
                                            format!("Error: {what}: {e}")
                                        }
                                        None => format!("Pendiente: {what}"),
                                    }
                                })
                                .collect();
                            ui.colored_label(
                                color,
                                format!("Túneles {active}/{}", term.tunnels.len()),
                            )
                            .on_hover_text(details.join("\n"));
                        }
                    }
                    if term.is_recording() {
                        status_dot(ui, ui.visuals().error_fg_color);
                        ui.label("Grabando snippet");
                        if ui.button("⏹ Detener").clicked() {
                            recorded = Some(term.stop_recording());
                        }
                    }
                    broadcast_button(ui, &mut self.broadcast, &terminals, tab_id);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("A+")
                            .on_hover_text("Letra más grande")
                            .clicked()
                        {
                            term.font_delta = (term.font_delta + 1.0).min(24.0);
                        }
                        if ui
                            .small_button("A−")
                            .on_hover_text("Letra más pequeña")
                            .clicked()
                        {
                            term.font_delta = (term.font_delta - 1.0).max(-8.0);
                        }
                        if ui
                            .small_button("🔍")
                            .on_hover_text("Buscar en el terminal y su historial (Ctrl+Mayús+F)")
                            .clicked()
                        {
                            term.open_search();
                        }
                        // Registro de la sesión en un fichero.
                        let (label, hover) = match &term.log {
                            Some(log) => (
                                RichText::new("⏺ Registrando").color(ui.visuals().error_fg_color),
                                format!(
                                    "Guardando lo que muestra este terminal en\n{}\n\
                                     Pulsa para dejar de registrar.",
                                    log.path().display()
                                ),
                            ),
                            None => (
                                RichText::new("⏺ Registrar"),
                                "Guardar en un fichero de texto lo que muestre este terminal"
                                    .to_string(),
                            ),
                        };
                        if ui.small_button(label).on_hover_text(hover).clicked() {
                            match term.log.take() {
                                Some(log) => {
                                    notice = Some((
                                        format!("Registro guardado en {}", log.path().display()),
                                        false,
                                    ));
                                }
                                None => {
                                    let target = tab
                                        .session
                                        .as_ref()
                                        .map(|s| s.target())
                                        .unwrap_or_default();
                                    match crate::session_log::SessionLog::start(
                                        &self.settings.log.dir(),
                                        &tab.title,
                                        &target,
                                        self.settings.log.timestamps,
                                    ) {
                                        Ok(log) => {
                                            notice = Some((
                                                format!("Registrando en {}", log.path().display()),
                                                false,
                                            ));
                                            term.log = Some(log);
                                        }
                                        Err(e) => notice = Some((format!("{e:#}"), true)),
                                    }
                                }
                            }
                        }
                        if let Some(error) = term.log.as_mut().and_then(|l| l.error.take()) {
                            // Disco lleno, carpeta borrada…: se deja de registrar.
                            notice = Some((error, true));
                            term.log = None;
                        }
                        // Grabación de la sesión (.cast) e historial de órdenes.
                        term_tools::header_buttons(
                            ui,
                            term,
                            &tab.title,
                            &self.settings.log,
                            &mut notice,
                            &mut self.term_tools,
                        );
                    });
                });
                // La barra de información del servidor ocupa una franja fija
                // bajo el terminal.
                let bar = tab
                    .session
                    .as_ref()
                    .filter(|s| wants_server_bar(&self.settings.server_bar, s));
                let area = ui.available_rect_before_wrap();
                let bar_height = match bar {
                    Some(_) => server_bar::height(ui).min(area.height() / 3.0),
                    None => 0.0,
                };
                let mut term_rect = area;
                term_rect.max.y -= bar_height;
                // Con la difusión activa, lo que se escriba aquí se anota para
                // repetirlo en los demás terminales del grupo.
                let receivers = terminals
                    .iter()
                    .filter(|(id, ..)| self.broadcast.includes(*id))
                    .count();
                let sending = self.broadcast.includes(tab_id) && receivers > 1;
                term.mirror = sending.then(Vec::new);
                // Sin diálogos abiertos, el teclado es del terminal.
                ui.scope_builder(egui::UiBuilder::new().max_rect(term_rect), |ui| term.ui(ui));
                mirrored = term.mirror.take().unwrap_or_default();
                if sending {
                    // Un marco bien visible: escribir en varios servidores a
                    // la vez no debe pasar inadvertido.
                    ui.painter().rect_stroke(
                        term_rect,
                        0.0,
                        egui::Stroke::new(2.0, warn_color()),
                        egui::StrokeKind::Inside,
                    );
                }
                if let Some(session) = bar {
                    let mut bar_rect = area;
                    bar_rect.min.y = term_rect.max.y;
                    ui.scope_builder(egui::UiBuilder::new().max_rect(bar_rect), |ui| {
                        if server_bar::show(
                            ui,
                            &self.settings.server_bar,
                            session,
                            term.monitor.as_ref(),
                        ) {
                            open_bar_settings = true;
                        }
                    });
                }
            }
            TabKind::Sftp(view) => view.ui(ui),
            TabKind::Desktop(view) if tab.x_server => {
                ui.horizontal(|ui| {
                    status_dot(ui, ok_color());
                    ui.colored_label(ok_color(), "Servidor X integrado");
                    let clients = self.xhost.running().map_or(0, |s| s.client_count());
                    ui.weak(match clients {
                        0 => "sin aplicaciones".to_string(),
                        1 => "1 aplicación".to_string(),
                        n => format!("{n} aplicaciones"),
                    });
                    if let Some((w, h)) = view.remote_size() {
                        ui.weak(format!("· {w}×{h}"));
                    }
                    if let Some(display) = self.xhost.local_display() {
                        ui.weak("· programas locales:");
                        if ui
                            .button(format!("DISPLAY={display}"))
                            .on_hover_text("Copiar «export DISPLAY=…» al portapapeles")
                            .clicked()
                        {
                            ui.ctx().copy_text(format!("export DISPLAY={display}"));
                            notice = Some(("Copiado al portapapeles".into(), false));
                        }
                    } else if let Some(error) = self.xhost.local_error() {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                    if clients == 0 {
                        ui.weak(
                            "· lanza una aplicación gráfica desde un terminal SSH con el \
                             reenvío X11 activado",
                        );
                    }
                });
                if self.xhost.running().is_some_and(|s| s.rootless().is_some()) {
                    xwindows_view::placeholder(ui, self.x_windows.count());
                } else {
                    egui::Frame::NONE.show(ui, |ui| view.ui(ui));
                }
            }
            TabKind::Desktop(view) => {
                ui.horizontal(|ui| {
                    match &view.state {
                        DesktopState::Connecting(_) => {
                            ui.spinner();
                            ui.label("Conectando…");
                        }
                        DesktopState::Connected => {
                            status_dot(ui, ok_color());
                            ui.colored_label(ok_color(), "Conectado");
                        }
                        DesktopState::Closed(_) => {
                            status_dot(ui, ui.visuals().error_fg_color);
                            ui.colored_label(ui.visuals().error_fg_color, "Desconectado");
                            if tab.session.is_some() && ui.button("⟳ Reconectar").clicked() {
                                reconnect = true;
                            }
                        }
                    }
                    if let Some(s) = &tab.session {
                        ui.weak(format!("{} · {}", s.protocol.label(), s.target()));
                    }
                    if let Some((w, h)) = view.remote_size() {
                        ui.weak(format!("{w}×{h}"));
                        if ui
                            .button("Ctrl+Alt+Supr")
                            .on_hover_text("Envía Ctrl+Alt+Supr al equipo remoto")
                            .clicked()
                        {
                            view.send_combo(&[KeyCode::Control, KeyCode::Alt, KeyCode::Delete]);
                            view.request_focus();
                        }
                        if ui
                            .button("⊞ Win")
                            .on_hover_text("Pulsa la tecla Windows en el equipo remoto")
                            .clicked()
                        {
                            view.send_combo(&[KeyCode::Meta]);
                            view.request_focus();
                        }
                    }
                });
                egui::Frame::NONE.show(ui, |ui| view.ui(ui));
            }
        });

        self.mirror_to_group(tab_id, &mirrored);
        if let Some((message, is_error)) = notice {
            self.notify(message, is_error);
        }

        if reconnect && let Some(session) = self.tabs[self.active].session.clone() {
            if matches!(self.tabs[self.active].kind, TabKind::Desktop(_)) {
                match self.new_desktop(&session) {
                    Ok(view) => self.tabs[self.active].kind = TabKind::Desktop(Box::new(view)),
                    Err(e) => self.notify(e, true),
                }
            } else {
                let active = self.active;
                match self.restart_terminal(active, "[Reconectando…]") {
                    Ok(()) => {
                        self.tabs[active].reconnect = None;
                        self.focus_active_terminal();
                    }
                    Err(e) => self.notify(e, true),
                }
            }
        }
        if cancel_reconnect && let Some(tab) = self.tabs.get_mut(self.active) {
            // Se deja como un intento agotado: no vuelve a programarse.
            tab.reconnect = Some(Reconnect {
                attempt: u32::MAX,
                next: None,
            });
        }
        if let Some((session, handle)) = open_sftp {
            self.open_sftp_for_terminal(session, handle, tab_id);
        }
        if let Some(bytes) = recorded {
            self.finish_recording(bytes);
        }
        if open_bar_settings {
            self.server_bar_window = Some(ServerBarWindow::new(&self.settings.server_bar));
        }
    }

    fn finish_recording(&mut self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            self.notify("No se tecleó nada: no se ha guardado ningún snippet", true);
        } else {
            self.recorded_macro = Some(RecordedMacro {
                name: String::new(),
                bytes,
                focused: false,
            });
        }
    }

    // ------------------------------------------------------------------
    // Diálogos
    // ------------------------------------------------------------------

    fn vault_dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.vault_dialog else {
            return;
        };
        let path = crate::paths::vault_file();
        let mut close = false;
        let mut opened: Option<Vault> = None;
        let mut changed = false;
        egui::Modal::new(egui::Id::unique("vault_dialog")).show(ctx, |ui| {
            ui.set_width(400.0);
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            match dialog {
                VaultDialog::Unlock {
                    password,
                    error,
                    focus,
                } => {
                    ui.heading("🔐 Desbloquear baúl");
                    ui.label(
                        "Introduce la contraseña maestra para usar las credenciales guardadas.",
                    );
                    let resp = ui.add(
                        egui::TextEdit::singleline(password)
                            .password(true)
                            .desired_width(f32::INFINITY),
                    );
                    // El foco se pide una sola vez (y tras cada intento fallido).
                    if *focus {
                        resp.request_focus();
                        *focus = false;
                    }
                    if let Some(e) = error {
                        ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Desbloquear").clicked() || enter {
                            match Vault::unlock(&path, password) {
                                Ok(v) => opened = Some(v),
                                Err(e) => {
                                    *error = Some(format!("{e:#}"));
                                    password.clear();
                                    *focus = true;
                                }
                            }
                        }
                        if ui.button("Continuar sin baúl").clicked() {
                            close = true;
                        }
                    });
                }
                VaultDialog::Create {
                    password,
                    confirm,
                    error,
                } => {
                    ui.heading("🔐 Crear baúl de credenciales");
                    ui.label(
                        "Elige una contraseña maestra. Con ella se cifran las contraseñas y \
                         passphrases guardadas (Argon2id + XChaCha20-Poly1305).",
                    );
                    ui.colored_label(
                        warn_color(),
                        "Si la olvidas, los secretos guardados no se pueden recuperar.",
                    );
                    ui.add(
                        egui::TextEdit::singleline(password)
                            .password(true)
                            .hint_text("contraseña maestra")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add(
                        egui::TextEdit::singleline(confirm)
                            .password(true)
                            .hint_text("repetir")
                            .desired_width(f32::INFINITY),
                    );
                    if let Some(e) = error {
                        ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Crear baúl").clicked() || enter {
                            if password.is_empty() {
                                *error = Some("La contraseña no puede estar vacía".into());
                            } else if password != confirm {
                                *error = Some("Las contraseñas no coinciden".into());
                            } else {
                                match Vault::create(&path, password) {
                                    Ok(v) => opened = Some(v),
                                    Err(e) => *error = Some(format!("{e:#}")),
                                }
                            }
                        }
                        if ui.button("Ahora no").clicked() {
                            close = true;
                        }
                    });
                }
                VaultDialog::Change {
                    password,
                    confirm,
                    error,
                } => {
                    ui.heading("Cambiar contraseña maestra");
                    ui.add(
                        egui::TextEdit::singleline(password)
                            .password(true)
                            .hint_text("nueva contraseña maestra")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add(
                        egui::TextEdit::singleline(confirm)
                            .password(true)
                            .hint_text("repetir")
                            .desired_width(f32::INFINITY),
                    );
                    if let Some(e) = error {
                        ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Cambiar").clicked() || enter {
                            if password.is_empty() {
                                *error = Some("La contraseña no puede estar vacía".into());
                            } else if password != confirm {
                                *error = Some("Las contraseñas no coinciden".into());
                            } else {
                                match self.vault.as_mut().map(|v| v.change_master(password)) {
                                    Some(Ok(())) => changed = true,
                                    Some(Err(e)) => *error = Some(format!("{e:#}")),
                                    None => *error = Some("El baúl está bloqueado".into()),
                                }
                            }
                        }
                        if ui.button("Cancelar").clicked() {
                            close = true;
                        }
                    });
                }
            }
        });
        if let Some(v) = opened {
            self.vault = Some(v);
            self.vault_dialog = None;
            self.notify("Baúl desbloqueado", false);
        } else if changed {
            self.vault_dialog = None;
            self.notify("Contraseña maestra cambiada", false);
        } else if close {
            self.vault_dialog = None;
        }
    }

    fn prompt_ui(&mut self, ctx: &egui::Context) {
        while self.prompts.front().is_some_and(ActivePrompt::abandoned) {
            self.prompts.pop_front();
        }
        let vault_open = self.vault.is_some();
        let Some(prompt) = self.prompts.front_mut() else {
            return;
        };
        let mut done = false;
        egui::Modal::new(egui::Id::unique("ssh_prompt")).show(ctx, |ui| {
            ui.set_width(460.0);
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
            match prompt {
                ActivePrompt::HostKey {
                    host,
                    port,
                    algorithm,
                    fingerprint,
                    reply,
                } => {
                    ui.heading("Host desconocido");
                    ui.label(format!(
                        "No se puede verificar la autenticidad de {host}:{port}. \
                         Es la primera vez que te conectas a este servidor."
                    ));
                    ui.add_space(4.0);
                    let is_certificate = algorithm.starts_with("certificado");
                    if is_certificate {
                        ui.label("Huella del certificado TLS:");
                    } else {
                        ui.label(format!("Huella de la clave {algorithm}:"));
                    }
                    ui.label(RichText::new(fingerprint.as_str()).monospace().strong());
                    ui.add_space(4.0);
                    ui.weak(if is_certificate {
                        "Comprueba la huella con el administrador del servidor. Si confías, \
                         se recordará y la conexión se rechazará si cambia."
                    } else {
                        "Comprueba la huella con el administrador del servidor. Si confías, \
                         se guardará en known_hosts y se avisará si cambia."
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Confiar y conectar").clicked() {
                            if let Some(r) = reply.take() {
                                let _ = r.send(true);
                            }
                            done = true;
                        }
                        if ui.button("Cancelar").clicked() || escape {
                            if let Some(r) = reply.take() {
                                let _ = r.send(false);
                            }
                            done = true;
                        }
                    });
                }
                ActivePrompt::Secret {
                    title,
                    message,
                    echo,
                    allow_save,
                    value,
                    save,
                    focused,
                    reply,
                } => {
                    ui.heading(title.as_str());
                    ui.label(message.as_str());
                    let resp = ui.add(
                        egui::TextEdit::singleline(value)
                            .password(!*echo)
                            .desired_width(f32::INFINITY),
                    );
                    if !*focused {
                        resp.request_focus();
                        *focused = true;
                    }
                    if *allow_save {
                        if vault_open {
                            ui.checkbox(save, "Guardar en el baúl cifrado");
                        } else {
                            ui.weak("Baúl bloqueado: no se guardará.");
                        }
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Aceptar").clicked() || enter {
                            if let Some(r) = reply.take() {
                                let _ = r.send(Some(SecretAnswer {
                                    value: std::mem::take(value),
                                    save: *save && *allow_save && vault_open,
                                }));
                            }
                            done = true;
                        }
                        if ui.button("Cancelar").clicked() || escape {
                            if let Some(r) = reply.take() {
                                let _ = r.send(None);
                            }
                            done = true;
                        }
                    });
                }
            }
        });
        if done {
            self.prompts.pop_front();
            self.focus_active_terminal();
        }
    }

    /// Editor de una sesión nueva en la carpeta indicada. Si la carpeta
    /// define la autenticación o los saltos, la sesión nace heredándolos.
    fn new_session(&self, parent: Option<Uuid>) -> SessionEditor {
        let mut editor = SessionEditor::new(parent);
        let inherited = self.tree.inherited(parent);
        editor.session.inherit_auth = inherited.auth.is_some();
        editor.session.inherit_jumps = inherited.jumps.is_some();
        editor
    }

    fn confirm_delete_ui(&mut self, ctx: &egui::Context) {
        let Some(id) = self.confirm_delete else {
            return;
        };
        let Some(node) = self.tree.find(id) else {
            self.confirm_delete = None;
            return;
        };
        let name = node.name();
        let session_ids = Tree::session_ids(node);
        let is_folder = matches!(node, Node::Folder(_));
        let mut close = false;
        let mut delete = false;
        egui::Modal::new(egui::Id::unique("confirm_delete")).show(ctx, |ui| {
            ui.set_width(400.0);
            ui.heading("Eliminar");
            if is_folder {
                ui.label(format!(
                    "Se eliminará la carpeta «{name}» con las {} sesión(es) que contiene \
                     y sus credenciales guardadas.",
                    session_ids.len()
                ));
            } else {
                ui.label(format!(
                    "Se eliminará la sesión «{name}» y sus credenciales guardadas."
                ));
            }
            ui.weak("Esta acción no se puede deshacer.");
            ui.horizontal(|ui| {
                if ui
                    .button(RichText::new("Eliminar").color(ui.visuals().error_fg_color))
                    .clicked()
                {
                    delete = true;
                }
                if ui.button("Cancelar").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    close = true;
                }
            });
        });
        if delete {
            self.tree.remove(id);
            if let Some(vault) = &mut self.vault {
                for sid in session_ids {
                    let _ = vault.remove_prefix(&format!("{sid}/"));
                }
            }
            self.save_tree();
            self.confirm_delete = None;
            if self.selected == Some(id) {
                self.selected = None;
            }
        } else if close {
            self.confirm_delete = None;
        }
    }

    fn rdp_prompt_ui(&mut self, ctx: &egui::Context) {
        let vault_open = self.vault.is_some();
        let Some(prompt) = &mut self.rdp_prompt else {
            return;
        };
        let mut close = false;
        let mut launch = false;
        egui::Modal::new(egui::Id::unique("rdp_prompt")).show(ctx, |ui| {
            ui.set_width(400.0);
            ui.heading("Escritorio remoto");
            ui.label(format!(
                "Contraseña de {} en {}",
                if prompt.session.username.is_empty() {
                    "(usuario)"
                } else {
                    &prompt.session.username
                },
                prompt.session.host
            ));
            let resp = ui.add(
                egui::TextEdit::singleline(&mut prompt.password)
                    .password(true)
                    .desired_width(f32::INFINITY),
            );
            if !prompt.focused {
                resp.request_focus();
                prompt.focused = true;
            }
            if vault_open {
                ui.checkbox(&mut prompt.save, "Guardar en el baúl cifrado");
            } else {
                ui.weak("Baúl bloqueado: no se guardará.");
            }
            ui.weak(format!("Cliente: {}", crate::rdp::client_description()));
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let valid = !prompt.password.is_empty();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(valid, egui::Button::new("Conectar"))
                    .clicked()
                    || (enter && valid)
                {
                    launch = true;
                }
                if ui.button("Cancelar").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    close = true;
                }
            });
        });
        if launch {
            if let Some(prompt) = self.rdp_prompt.take() {
                // Solo se guarda si la sesión pertenece al árbol.
                if prompt.save && vault_open && self.tree.session(prompt.session.id).is_some() {
                    self.store_secrets(vec![(
                        prompt.session.password_key(),
                        prompt.password.clone(),
                    )]);
                }
                self.launch_rdp(&prompt.session, Some(&prompt.password));
            }
        } else if close {
            self.rdp_prompt = None;
        }
    }
}

/// Resume la salida de FreeRDP: las primeras líneas de error, sin prefijos.
fn rdp_error_summary(log: &str) -> String {
    let mut out: Vec<&str> = vec![];
    for line in log.lines().filter(|l| l.contains("[ERROR]")) {
        // Formato: "[hora] [pid] [ERROR][módulo] - [función]: mensaje"
        let msg = line.rsplit_once("]: ").map_or(line, |(_, m)| m).trim();
        if !msg.is_empty() && !out.contains(&msg) {
            out.push(msg);
        }
        if out.len() == 2 {
            break;
        }
    }
    out.join(" · ")
}

/// Punto de estado (verde / ámbar / rojo).
fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

fn collect_matches<'a>(nodes: &'a [Node], needle: &str, out: &mut Vec<&'a Session>) {
    for n in nodes {
        match n {
            Node::Folder(f) => collect_matches(&f.children, needle, out),
            Node::Session(s) => {
                if s.display_name().to_lowercase().contains(needle)
                    || s.host.to_lowercase().contains(needle)
                    || s.username.to_lowercase().contains(needle)
                {
                    out.push(s);
                }
            }
        }
    }
}

fn move_menu(
    ui: &mut egui::Ui,
    id: Uuid,
    folders: &[(Uuid, String)],
    actions: &mut Vec<TreeAction>,
) {
    ui.menu_button("Mover a", |ui| {
        if ui.button("(raíz)").clicked() {
            actions.push(TreeAction::Move(id, None));
            ui.close();
        }
        for (fid, path) in folders {
            if *fid != id && ui.button(path).clicked() {
                actions.push(TreeAction::Move(id, Some(*fid)));
                ui.close();
            }
        }
    });
}

fn session_row(
    ui: &mut egui::Ui,
    s: &Session,
    selected: Option<Uuid>,
    folders: &[(Uuid, String)],
    actions: &mut Vec<TreeAction>,
) {
    let label = format!("{} {}", s.protocol.icon(), s.display_name());
    let resp = ui
        .selectable_label(selected == Some(s.id), label)
        .on_hover_text(format!(
            "{} · {}{}:{}{}",
            s.protocol.label(),
            if s.username.is_empty() {
                String::new()
            } else {
                format!("{}@", s.username)
            },
            s.host,
            s.port,
            if s.protocol != Protocol::Rdp && s.auth != AuthMethod::Password {
                format!(" · {}", s.auth.label())
            } else {
                String::new()
            }
        ));
    if resp.double_clicked() {
        actions.push(TreeAction::Connect(s.id));
    } else if resp.clicked() {
        actions.push(TreeAction::Select(s.id));
    }
    resp.context_menu(|ui| {
        if ui.button("Conectar").clicked() {
            actions.push(TreeAction::Connect(s.id));
            ui.close();
        }
        match s.protocol {
            Protocol::Ssh => {
                if ui.button("📂 Abrir SFTP").clicked() {
                    actions.push(TreeAction::OpenAs(s.id, Protocol::Sftp));
                    ui.close();
                }
            }
            Protocol::Sftp => {
                if ui.button("💻 Abrir terminal SSH").clicked() {
                    actions.push(TreeAction::OpenAs(s.id, Protocol::Ssh));
                    ui.close();
                }
            }
            Protocol::Rdp => {
                let (label, embedded) = if s.rdp.embedded {
                    ("🖥 Abrir con el cliente externo", false)
                } else {
                    ("🖥 Abrir integrado en una pestaña", true)
                };
                if ui.button(label).clicked() {
                    actions.push(TreeAction::OpenRdp(s.id, embedded));
                    ui.close();
                }
            }
            Protocol::Vnc | Protocol::Telnet | Protocol::Serial => {}
        }
        ui.separator();
        if ui.button("✏ Editar…").clicked() {
            actions.push(TreeAction::Edit(s.id));
            ui.close();
        }
        if ui.button("Duplicar").clicked() {
            actions.push(TreeAction::Duplicate(s.id));
            ui.close();
        }
        move_menu(ui, s.id, folders, actions);
        ui.separator();
        if ui.button("🗑 Eliminar…").clicked() {
            actions.push(TreeAction::Delete(s.id));
            ui.close();
        }
    });
}

fn tree_ui(
    ui: &mut egui::Ui,
    nodes: &[Node],
    selected: Option<Uuid>,
    folders: &[(Uuid, String)],
    actions: &mut Vec<TreeAction>,
) {
    for node in nodes {
        match node {
            Node::Folder(f) => {
                let title = if selected == Some(f.id) {
                    RichText::new(format!("📁 {}", f.name)).strong()
                } else {
                    RichText::new(format!("📁 {}", f.name))
                };
                let header = egui::CollapsingHeader::new(title)
                    .id_salt(f.id)
                    .default_open(true)
                    .show(ui, |ui| {
                        if f.children.is_empty() {
                            ui.weak("(vacía)");
                        }
                        tree_ui(ui, &f.children, selected, folders, actions);
                    });
                if header.header_response.clicked() {
                    actions.push(TreeAction::Select(f.id));
                }
                header.header_response.context_menu(|ui| {
                    if ui.button("➕ Nueva sesión aquí").clicked() {
                        actions.push(TreeAction::NewSession(Some(f.id)));
                        ui.close();
                    }
                    if ui.button("📁 Nueva subcarpeta").clicked() {
                        actions.push(TreeAction::NewFolder(Some(f.id)));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("✏ Editar carpeta…").clicked() {
                        actions.push(TreeAction::RenameFolder(f.id));
                        ui.close();
                    }
                    if ui
                        .button("▶ Ejecutar una orden en sus equipos…")
                        .on_hover_text(
                            "Lanza la misma orden en todos sus equipos SSH y recoge lo que \
                             contesta cada uno",
                        )
                        .clicked()
                    {
                        actions.push(TreeAction::RunOnFolder(f.id));
                        ui.close();
                    }
                    if ui
                        .button("📊 Panel de estado")
                        .on_hover_text(
                            "Vigila a la vez todos sus equipos SSH: conexión, CPU, memoria, \
                             disco y alertas",
                        )
                        .clicked()
                    {
                        actions.push(TreeAction::Dashboard(f.id));
                        ui.close();
                    }
                    move_menu(ui, f.id, folders, actions);
                    ui.separator();
                    if ui.button("🗑 Eliminar…").clicked() {
                        actions.push(TreeAction::Delete(f.id));
                        ui.close();
                    }
                });
            }
            Node::Session(s) => session_row(ui, s, selected, folders, actions),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Los atajos de los snippets se miran antes de que el terminal vea
        // las teclas.
        self.dispatch_shortcuts(ui.ctx());
        // Antes de abrir ninguna conexión en este fotograma.
        self.sync_proxy();
        crate::agent::set_choice(self.settings.agent);
        self.sync_theme(ui.ctx());
        // El idioma de los ajustes (solo cuesta una comparación).
        crate::i18n::set(crate::i18n::resolve(self.settings.language));
        if let Some(probe) = &mut self.probe {
            probe.tick(ui.ctx());
        }

        // --- Eventos de fondo ---------------------------------------------
        while let Ok(req) = self.prompt_rx.try_recv() {
            self.prompts.push_back(match req {
                PromptRequest::HostKey {
                    host,
                    port,
                    algorithm,
                    fingerprint,
                    reply,
                } => ActivePrompt::HostKey {
                    host,
                    port,
                    algorithm,
                    fingerprint,
                    reply: Some(reply),
                },
                PromptRequest::Secret {
                    title,
                    message,
                    echo,
                    allow_save,
                    reply,
                } => ActivePrompt::Secret {
                    title,
                    message,
                    echo,
                    allow_save,
                    value: String::new(),
                    save: allow_save && self.vault.is_some(),
                    focused: false,
                    reply: Some(reply),
                },
            });
        }
        while let Ok((msg, is_error)) = self.notice_rx.try_recv() {
            self.notify(msg, is_error);
        }
        // Las pestañas en segundo plano también procesan sus eventos.
        let mut secrets = vec![];
        let mut notices = vec![];
        for tab in &mut self.tabs {
            match &mut tab.kind {
                TabKind::Terminal(t) => {
                    t.poll();
                    secrets.append(&mut t.secrets_to_save);
                    notices.append(&mut t.notices);
                    if t.is_waiting_login() {
                        ui.ctx().request_repaint_after(Duration::from_millis(120));
                    }
                    // Consulta del estado del servidor para la barra.
                    let bar = &self.settings.server_bar;
                    let wanted = tab
                        .session
                        .as_ref()
                        .is_some_and(|s| wants_server_bar(bar, s));
                    match (&t.ssh, wanted) {
                        (Some(handle), true) => {
                            if t.monitor.as_ref().is_none_or(|m| m.is_stale(bar)) {
                                let mut monitor = crate::monitor::spawn(
                                    self.rt.handle(),
                                    handle.clone(),
                                    bar,
                                    self.waker.clone(),
                                );
                                // Mientras llega la primera muestra se
                                // conserva la última conocida.
                                monitor.stats = t.monitor.take().and_then(|m| m.stats);
                                t.monitor = Some(monitor);
                            }
                        }
                        _ => t.monitor = None,
                    }
                    if let Some(monitor) = &mut t.monitor {
                        monitor.poll();
                    }
                }
                TabKind::Sftp(s) => {
                    s.poll();
                    secrets.append(&mut s.secrets_to_save);
                }
                TabKind::Desktop(d) => {
                    d.poll();
                    secrets.append(&mut d.secrets_to_save);
                }
            }
        }
        if let Some(notice) = notices.pop() {
            self.notify(notice, true);
        }
        self.poll_tunnels(ui.ctx(), &mut secrets);
        if let Some(run) = self.fleet_window.as_mut().and_then(|w| w.run.as_mut()) {
            run.poll();
            secrets.append(&mut run.secrets_to_save);
            if !run.is_finished() {
                // Los segundos de cada equipo siguen corriendo.
                ui.ctx().request_repaint_after(Duration::from_millis(250));
            }
        }
        self.sync_x_server(ui.ctx());
        // Solo se guardan secretos de sesiones que existen en el árbol.
        secrets.retain(|(key, _)| {
            key.split('/')
                .next()
                .and_then(|id| Uuid::parse_str(id).ok())
                .is_some_and(|id| self.tree.find(id).is_some())
        });
        self.store_secrets(secrets);
        self.term_tools_poll(ui.ctx());
        self.note_background_activity();
        self.sync_sftp_views();
        self.auto_reconnect(ui.ctx());
        self.sync_saved_tabs();

        // --- Disposición --------------------------------------------------
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(3.0);
            self.toolbar(ui);
            ui.add_space(3.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(250.0)
            .size_range(160.0..=480.0)
            .show(ui, |ui| self.sidebar(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            if self.tabs.is_empty() {
                self.welcome(ui);
            } else {
                self.tab_bar(ui);
                ui.separator();
                self.content_size = ui.available_size();
                // Mosaico, si la pestaña activa es uno de sus terminales y
                // hay al menos dos; si no, la pestaña activa a solas.
                let tiles = self.split_tiles();
                if tiles.len() >= 2 && tiles.contains(&self.active) {
                    self.split_view_ui(ui, &tiles);
                } else {
                    // El color de la carpeta o del equipo, bien a la vista
                    // sobre lo que se está escribiendo.
                    if let Some(color) = self
                        .tabs
                        .get(self.active)
                        .and_then(|t| t.session.as_ref())
                        .and_then(|s| s.color)
                    {
                        let (band, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 3.0),
                            egui::Sense::hover(),
                        );
                        ui.painter().rect_filled(band, 1.0, color.color32());
                    }
                    self.active_tab_ui(ui);
                }
            }
        });

        // --- Ventanas y diálogos ------------------------------------------
        let ctx = ui.ctx().clone();
        if let Some(editor) = &mut self.editor {
            editor.embedded_rdp = crate::HAS_EMBEDDED_RDP && self.settings.rdp_embedded;
            match editor.show(&ctx, &mut self.tree, &mut self.vault) {
                EditorResult::Open => {}
                EditorResult::Cancelled => self.editor = None,
                EditorResult::Saved { connect } => {
                    let saved = editor.session.clone();
                    self.selected = Some(saved.id);
                    self.editor = None;
                    self.save_tree();
                    // Se usa tal como se conecta: con lo heredado de su carpeta.
                    let session = self.tree.resolved_session(saved.id).unwrap_or(saved);
                    // Las pestañas ya abiertas de este equipo toman al
                    // momento su nuevo fondo y su barra de información.
                    for tab in &mut self.tabs {
                        let Some(open) = tab.session.as_mut().filter(|s| s.id == session.id) else {
                            continue;
                        };
                        open.server_info = session.server_info;
                        open.backdrop = session.backdrop.clone();
                        if let TabKind::Terminal(term) = &mut tab.kind {
                            term.set_backdrop(session.backdrop.as_ref());
                        }
                    }
                    if connect {
                        self.connect(session, None);
                    }
                }
            }
        }
        if let Some(win) = &mut self.keys_window
            && !win.show(&ctx)
        {
            self.keys_window = None;
            self.focus_active_terminal();
        }
        if self.themes_window.is_some() {
            let system_dark = self.system_dark();
            if let Some(win) = &mut self.themes_window
                && !win.show(&ctx, &mut self.themes, &mut self.settings, system_dark)
            {
                self.themes_window = None;
                self.focus_active_terminal();
            }
            // La ventana aplica por su cuenta lo que se elige o se edita en
            // ella: aquí solo se anota cuál toca, para no pisarle la vista
            // previa de lo que está sin guardar.
            self.applied_theme = Some(self.settings.active_theme(system_dark).to_string());
        }
        if let Some(win) = &mut self.settings_window {
            let proxy_vault = ProxyVault {
                open: self.vault.is_some(),
                stored: self
                    .vault
                    .as_ref()
                    .is_some_and(|v| v.contains(crate::proxy::PASSWORD_KEY)),
            };
            let (open, changed, request) = win.show(&ctx, &mut self.settings, proxy_vault);
            if let Some(password) = &request.proxy_password
                && let Some(vault) = &mut self.vault
            {
                let result = if password.is_empty() {
                    vault.remove(crate::proxy::PASSWORD_KEY)
                } else {
                    vault.set(crate::proxy::PASSWORD_KEY, password)
                };
                match result {
                    Ok(()) if password.is_empty() => {
                        self.notify("Contraseña del proxy borrada del baúl", false)
                    }
                    Ok(()) => self.notify("Contraseña del proxy guardada en el baúl", false),
                    Err(e) => self.notify(format!("No se pudo guardar: {e:#}"), true),
                }
            }
            if changed {
                self.settings.highlight.sanitize();
                crate::highlight::set(&self.settings.highlight);
                crate::terminal::set_paste_guard(self.settings.confirm_paste);
                self.xhost.configure(
                    self.settings.x_server.embedded,
                    self.settings.x_server.listen_local,
                );
            }
            if request.open_x_server && self.settings.x_server.embedded {
                // Si ya está en marcha, se enseña su pestaña.
                self.xhost.server();
                if let Some(index) = self.tabs.iter().position(|t| t.x_server) {
                    self.active = index;
                }
            }
            if changed && let Err(e) = self.settings.save(&crate::paths::settings_file()) {
                self.notify(
                    format!("No se pudieron guardar las preferencias: {e:#}"),
                    true,
                );
            }
            if request.open_server_bar {
                self.server_bar_window = Some(ServerBarWindow::new(&self.settings.server_bar));
            }
            if request.open_transfer {
                self.transfer_window = Some(TransferWindow::default());
            }
            if let Some(folder) = request.open_folder {
                let _ = std::fs::create_dir_all(&folder);
                if let Err(e) = crate::paths::open_with_system(&folder) {
                    self.notify(format!("No se pudo abrir {}: {e}", folder.display()), true);
                }
            }
            if !open {
                self.settings_window = None;
                self.focus_active_terminal();
            }
        }
        if let Some(win) = &mut self.transfer_window {
            let (open, request) = win.show(&ctx);
            match request {
                TransferRequest::None => {}
                TransferRequest::Imported(folder) => {
                    let id = folder.id;
                    self.tree.insert(None, Node::Folder(*folder));
                    self.selected = Some(id);
                    self.save_tree();
                }
                TransferRequest::Restored => self.reload_config(&ctx),
            }
            if !open {
                self.transfer_window = None;
                self.focus_active_terminal();
            }
        }
        if let Some(win) = &mut self.server_bar_window {
            let (open, changed) = win.show(&ctx, &mut self.settings.server_bar);
            if changed {
                self.settings.server_bar.sanitize();
                if let Err(e) = self.settings.save(&crate::paths::settings_file()) {
                    self.notify(
                        format!("No se pudieron guardar las preferencias: {e:#}"),
                        true,
                    );
                }
            }
            if !open {
                self.server_bar_window = None;
                self.focus_active_terminal();
            }
        }
        if let Some(win) = &mut self.macros_window {
            let can_run = matches!(
                self.tabs.get(self.active).map(|t| &t.kind),
                Some(TabKind::Terminal(t)) if t.is_connected()
            );
            let (open, request) = win.show(&ctx, &mut self.macros, can_run);
            if !open {
                self.macros_window = None;
                self.focus_active_terminal();
            }
            if let MacroRequest::Run(id) = request {
                self.run_macro(id);
            }
        }
        self.tunnels_window_ui(&ctx);
        self.term_tools_windows(&ctx);
        self.vault_guard(&ctx);
        self.fleet_window_ui(&ctx);
        self.dashboard_ui(&ctx);
        self.palette_ui(&ctx);
        self.close_confirm_ui(&ctx);
        self.quit_guard(&ctx);
        self.recorded_macro_ui(&ctx);
        self.snippet_prompt_ui(&ctx);
        if let Some(editor) = &mut self.folder_editor {
            match editor.show(&ctx, &mut self.tree, &mut self.vault) {
                FolderResult::Open => {}
                FolderResult::Cancelled => self.folder_editor = None,
                FolderResult::Saved(id) => {
                    self.selected = Some(id);
                    self.folder_editor = None;
                    self.save_tree();
                }
            }
        }
        self.confirm_delete_ui(&ctx);
        self.rdp_prompt_ui(&ctx);
        self.vault_dialog_ui(&ctx);
        self.prompt_ui(&ctx);
    }
}

/// Datos de una pestaña para las variables de los snippets.
/// Bytes que se envían al conectar a un equipo: sus órdenes, con las
/// variables de los snippets ya sustituidas y un Intro al final.
fn on_connect_bytes(session: &Session) -> Vec<u8> {
    let context = crate::macros::SnippetContext {
        host: session.host.trim().to_string(),
        port: session.port.to_string(),
        user: session.username.trim().to_string(),
        name: session.display_name(),
    };
    let mut text = session.on_connect.trim_end().to_string();
    text.push('\n');
    let expanded = crate::macros::expand(&text, &context, &[], chrono::Local::now());
    crate::macros::decode(&expanded)
}

fn snippet_context(tab: &Tab) -> crate::macros::SnippetContext {
    match &tab.session {
        Some(session) => crate::macros::SnippetContext {
            host: session.host.trim().to_string(),
            port: session.port.to_string(),
            user: session.username.trim().to_string(),
            name: session.display_name(),
        },
        // Terminal local.
        None => crate::macros::SnippetContext {
            host: "localhost".into(),
            port: String::new(),
            user: ["USER", "USERNAME", "LOGNAME"]
                .iter()
                .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
                .or_else(|| {
                    crate::paths::home_dir()
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                })
                .unwrap_or_default(),
            name: tab.title.clone(),
        },
    }
}

/// Botón de la cabecera del terminal que abre el menú de difusión.
fn broadcast_button(
    ui: &mut egui::Ui,
    broadcast: &mut Broadcast,
    terminals: &[(u64, String, bool)],
    current: u64,
) {
    use egui::containers::menu::{MenuButton, MenuConfig};
    let receivers = terminals
        .iter()
        .filter(|(id, ..)| broadcast.includes(*id))
        .count();
    let label = if !broadcast.enabled {
        RichText::new("📡 Difusión")
    } else if broadcast.includes(current) && receivers > 1 {
        RichText::new(format!("📡 Escribiendo en {receivers} terminales"))
            .color(warn_color())
            .strong()
    } else if broadcast.includes(current) {
        RichText::new("📡 Difusión: no hay más terminales").color(warn_color())
    } else {
        RichText::new("📡 Difusión activa (este terminal no)").color(warn_color())
    };
    // El menú no se cierra al marcar casillas: se suelen marcar varias.
    let config = MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
    let (response, _) = MenuButton::new(label).config(config).ui(ui, |ui| {
        ui.set_min_width(300.0);
        ui.set_max_width(340.0);
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
        ui.checkbox(
            &mut broadcast.enabled,
            "Escribir a la vez en varios terminales",
        );
        ui.weak(
            "Lo que teclees, pegues o lances con un snippet en uno de ellos se envía también a \
             los demás.",
        );
        ui.add_enabled_ui(broadcast.enabled, |ui| {
            ui.radio_value(&mut broadcast.all, true, "En todos los terminales abiertos");
            ui.radio_value(&mut broadcast.all, false, "Solo en los que elija:");
            ui.add_enabled_ui(!broadcast.all, |ui| {
                ui.indent("broadcast_tabs", |ui| {
                    for (id, title, connected) in terminals {
                        let mut on = broadcast.chosen.contains(id);
                        let mut text = title.clone();
                        if *id == current {
                            text.push_str(" (este)");
                        }
                        if !connected {
                            text.push_str(" · sin conexión");
                        }
                        if ui.checkbox(&mut on, text).changed() {
                            if on {
                                broadcast.chosen.insert(*id);
                            } else {
                                broadcast.chosen.remove(id);
                            }
                        }
                    }
                    ui.horizontal(|ui| {
                        if ui.small_button("Todos").clicked() {
                            broadcast.chosen = terminals.iter().map(|(id, ..)| *id).collect();
                        }
                        if ui.small_button("Ninguno").clicked() {
                            broadcast.chosen.clear();
                        }
                    });
                });
            });
        });
    });
    response.on_hover_text("Enviar lo que escribes a varios terminales a la vez");
}

/// `true` si el terminal de esta sesión lleva la barra de información del
/// servidor: solo terminales SSH, y solo si ni las preferencias ni la propia
/// sesión la desactivan.
fn wants_server_bar(config: &crate::monitor::ServerBar, session: &Session) -> bool {
    config.enabled && session.protocol == Protocol::Ssh && session.server_info
}

#[cfg(test)]
mod tests {
    use super::{rdp_error_summary, reconnect_delay, split_grid, worth_retrying};

    #[test]
    fn split_view_grid() {
        // Automático: lo más cuadrado posible.
        assert_eq!(split_grid(2, 0), (2, 1));
        assert_eq!(split_grid(3, 0), (2, 2));
        assert_eq!(split_grid(4, 0), (2, 2));
        assert_eq!(split_grid(5, 0), (3, 2));
        assert_eq!(split_grid(9, 0), (3, 3));
        assert_eq!(split_grid(10, 0), (4, 3));
        // Con las columnas que pida el usuario (nunca más que terminales).
        assert_eq!(split_grid(4, 1), (1, 4));
        assert_eq!(split_grid(5, 4), (4, 2));
        assert_eq!(split_grid(2, 4), (2, 1));
        assert_eq!(split_grid(0, 0), (1, 1));
    }

    #[test]
    fn reconnection_policy() {
        // Cortes de red: se reintenta, cada vez con más espera.
        assert!(worth_retrying("Connection reset by peer"));
        assert!(worth_retrying(
            "Tiempo de espera agotado conectando a 10.0.0.5:22"
        ));
        assert!(worth_retrying(
            "El último salto no pudo abrir la conexión con x:22: refused"
        ));
        assert!(reconnect_delay(0) < reconnect_delay(3));
        assert_eq!(reconnect_delay(99), reconnect_delay(5));
        // Autenticación o confianza: insistir puede bloquear la cuenta.
        assert!(!worth_retrying(
            "Autenticación fallida: contraseña incorrecta"
        ));
        assert!(!worth_retrying("Autenticación cancelada"));
        assert!(!worth_retrying(
            "El servidor rechazó la clave /home/u/.ssh/id"
        ));
        assert!(!worth_retrying("Clave de host rechazada por el usuario"));
        assert!(!worth_retrying(
            "Ninguna clave del agente fue aceptada por el servidor"
        ));
    }

    #[test]
    fn freerdp_log_summary() {
        let log = "[12:36:09:851] [1:2] [WARN][com.freerdp.client.x11] - [f]: aviso\n\
                   [12:36:09:872] [1:2] [ERROR][com.freerdp.core] - [get_next_addrinfo]: ERRCONNECT_CONNECT_FAILED [0x00020006]\n\
                   [12:36:09:872] [1:2] [ERROR][com.freerdp.core] - [get_next_addrinfo]: ERRCONNECT_CONNECT_FAILED [0x00020006]\n\
                   [12:36:09:872] [1:2] [ERROR][com.freerdp.core.nego] - [nego_connect]: Failed to connect\n\
                   [12:36:09:873] [1:2] [ERROR][x] - [y]: tercero\n";
        assert_eq!(
            rdp_error_summary(log),
            "ERRCONNECT_CONNECT_FAILED [0x00020006] · Failed to connect"
        );
        assert_eq!(rdp_error_summary("sin errores"), "");
    }
}
