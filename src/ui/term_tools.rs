//! Herramientas de los terminales que viven fuera de cada pestaña: los
//! disparadores, el aviso de fin de orden larga, las grabaciones y el
//! historial de órdenes. Aquí está su enganche con la aplicación (qué se
//! mira en cada fotograma, sus ventanas y su menú).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::cast_view::PlayerWindow;
use super::history_view::{HistoryRequest, HistoryWindow};
use super::palette_view::{PaletteAction, PaletteItem};
use super::triggers_view::TriggersWindow;
use super::{App, Tab, TabKind};
use crate::cmd_history::{HistoryStore, LOCAL_KEY};
use crate::model::{Session, Tree};
use crate::session_log::LogSettings;
use crate::shell_marks::{Finished, human_duration};
use crate::terminal::{Signal, TermInput, TerminalView};
use crate::triggers::{Compiled, Fired, Scope, TriggerSettings};

/// Grabaciones recientes que se ofrecen en el menú.
const RECENT_CASTS: usize = 8;
/// Cada cuánto se vuelve a mirar la carpeta de las grabaciones.
const RECENT_TTL: Duration = Duration::from_secs(2);
/// Ficheros de la carpeta que se miran como mucho para buscarlas.
const MAX_SCANNED: usize = 20_000;
/// El historial de órdenes se guarda, como mucho, cada este tiempo.
const HISTORY_SAVE_EVERY: Duration = Duration::from_secs(2);
/// Órdenes del historial que se ofrecen en la paleta.
const PALETTE_HISTORY: usize = 40;

/// Marca de una pestaña en la que ha saltado un disparador (o ha terminado
/// una orden larga) mientras no se miraba.
pub(super) struct TabAlert {
    /// Qué ha pasado, para mostrarlo al pasar el ratón.
    pub text: String,
    /// Con campana.
    pub bell: bool,
}

impl Tab {
    /// Signo que precede al título: la campana del terminal o la marca de
    /// un disparador.
    pub(super) fn mark(&self) -> &'static str {
        match &self.alert {
            _ if self.rang => "🔔 ",
            Some(alert) if alert.bell => "🔔 ",
            Some(_) => "❗ ",
            None => "",
        }
    }
}

pub(super) struct TermTools {
    /// Reglas preparadas para aplicarlas y los ajustes de los que salieron.
    rules: Arc<Compiled>,
    applied: TriggerSettings,
    triggers_window: Option<TriggersWindow>,
    /// Reproductores de grabaciones abiertos.
    players: Vec<PlayerWindow>,
    next_player: u64,
    /// Últimas grabaciones de la carpeta de registros y cuándo se miró.
    recent: Option<(Instant, Vec<PathBuf>)>,
    /// Órdenes ejecutadas en cada equipo.
    history: HistoryStore,
    /// Cuándo se guardó el historial por última vez.
    history_saved: Instant,
    /// El último intento de guardarlo falló (para avisar una sola vez).
    history_failed: bool,
    history_window: Option<HistoryWindow>,
    /// Se ha pedido abrir el historial desde la cabecera de un terminal.
    pub open_history: bool,
}

impl TermTools {
    pub(super) fn new(settings: &crate::theme::Settings, tree: &Tree) -> Self {
        let mut history = HistoryStore::load(&crate::cmd_history::history_file());
        // El historial de los equipos que ya no existen se olvida (salvo
        // que el árbol haya llegado vacío: quizá no se pudo leer).
        if !tree.nodes.is_empty() {
            history.retain_hosts(|key| host_exists(key, tree));
        }
        Self {
            history,
            history_saved: Instant::now(),
            history_failed: false,
            history_window: None,
            open_history: false,
            rules: Arc::new(Compiled::new(&settings.triggers)),
            applied: settings.triggers.clone(),
            triggers_window: None,
            players: vec![],
            next_player: 1,
            recent: None,
        }
    }
}

impl Drop for TermTools {
    fn drop(&mut self) {
        // Lo último que se haya ejecutado, antes de salir.
        if self.history.is_dirty() {
            let _ = self.history.save(&crate::cmd_history::history_file());
        }
    }
}

/// Clave del historial de órdenes de una pestaña: la de su equipo si está
/// en el árbol, la común de los terminales locales, o ninguna (una conexión
/// rápida, que no se guarda en ningún sitio).
fn history_key(session: Option<&Session>, tree: &Tree) -> Option<String> {
    match session {
        None => Some(LOCAL_KEY.to_string()),
        Some(session) => tree
            .session(session.id)
            .map(|session| session.id.to_string()),
    }
}

/// El equipo de esa clave sigue existiendo.
fn host_exists(key: &str, tree: &Tree) -> bool {
    key == LOCAL_KEY || uuid::Uuid::parse_str(key).is_ok_and(|id| tree.session(id).is_some())
}

/// Las grabaciones (`.cast`) más recientes de una carpeta, de la más nueva
/// a la más antigua.
fn recent_casts(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = entries
        .take(MAX_SCANNED)
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case(crate::cast::EXTENSION))
        })
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            meta.is_file().then(|| {
                (
                    meta.modified().unwrap_or(std::time::UNIX_EPOCH),
                    entry.path(),
                )
            })
        })
        .collect();
    found.sort_by(|a, b| b.cmp(a));
    found
        .into_iter()
        .take(RECENT_CASTS)
        .map(|(_, path)| path)
        .collect()
}

/// Botones de la cabecera de un terminal: empezar o terminar su grabación
/// y abrir el historial de órdenes de su equipo.
pub(super) fn header_buttons(
    ui: &mut egui::Ui,
    term: &mut TerminalView,
    title: &str,
    settings: &LogSettings,
    notice: &mut Option<(String, bool)>,
    tools: &mut TermTools,
) {
    if ui
        .small_button("🕘")
        .on_hover_text("Historial de órdenes de este equipo")
        .clicked()
    {
        tools.open_history = true;
    }
    let (label, hover) = match &term.cast {
        Some(cast) => (
            egui::RichText::new("🎬 Grabando").color(ui.visuals().error_fg_color),
            format!(
                "Grabando esta sesión, con sus tiempos, en\n{}\nPulsa para terminar la grabación.",
                cast.path().display()
            ),
        ),
        None => (
            egui::RichText::new("🎬 Grabar"),
            "Grabar esta sesión con sus tiempos (asciicast, .cast) para reproducirla después"
                .to_string(),
        ),
    };
    if ui.small_button(label).on_hover_text(hover).clicked() {
        *notice = Some(toggle_cast(term, title, settings));
    }
}

/// Empieza a grabar un terminal o, si ya se graba, termina. Devuelve el
/// aviso que corresponde.
fn toggle_cast(term: &mut TerminalView, title: &str, settings: &LogSettings) -> (String, bool) {
    match term.stop_cast() {
        Some(path) => (
            format!(
                "Grabación guardada en {} (se reproduce desde «Herramientas»)",
                path.display()
            ),
            false,
        ),
        None => match term.start_cast(&settings.dir(), title) {
            Ok(path) => (format!("Grabando en {}", path.display()), false),
            Err(e) => (format!("No se pudo grabar la sesión: {e:#}"), true),
        },
    }
}

/// Texto del aviso de un disparador y si es un problema.
fn describe_fired(tab: &str, fired: &Fired) -> (String, bool) {
    if fired.paused {
        return (
            format!(
                "{tab}: el disparador «{}» se ha disparado demasiadas veces seguidas y queda \
                 en pausa en esta pestaña",
                fired.name
            ),
            true,
        );
    }
    let text = match (&fired.send, fired.notify) {
        (Some(_), _) => format!(
            "⚠ {tab}: el disparador «{}» ha contestado a «{}»",
            fired.name, fired.line
        ),
        (None, _) => format!("🔔 {tab}: {} — «{}»", fired.name, fired.line),
    };
    (text, false)
}

/// Texto del aviso de fin de orden larga.
fn describe_done(tab: &str, done: &Finished) -> String {
    let time = human_duration(done.duration);
    match (done.exact, done.exit_code) {
        (true, Some(code)) if code != 0 => {
            format!("{tab}: la orden ha terminado con error (código {code}) tras {time}")
        }
        (true, _) => format!("{tab}: la orden ha terminado tras {time}"),
        (false, _) => {
            format!("{tab}: parece que la orden ha terminado (o espera una respuesta) tras {time}")
        }
    }
}

impl App {
    /// Lo que hay que mirar en cada fotograma, una vez procesados los
    /// eventos de las pestañas: qué disparadores saltan, qué órdenes largas
    /// han terminado, las grabaciones automáticas y las órdenes que hay que
    /// anotar en el historial.
    pub(super) fn term_tools_poll(&mut self, ctx: &egui::Context) {
        let tools = &mut self.term_tools;
        if tools.applied != self.settings.triggers {
            tools.applied = self.settings.triggers.clone();
            tools.rules = Arc::new(Compiled::new(&tools.applied));
        }
        let rules = tools.rules.clone();
        let record_always = self.settings.log.record_always;
        let cast_dir = self.settings.log.dir();
        let now = chrono::Utc::now().timestamp();
        let tiles = self.split_tiles();
        let history = &self.settings.history;
        let store = &mut self.term_tools.history;
        let split_visible = tiles.len() >= 2 && tiles.contains(&self.active);
        let focused = ctx.input(|i| i.focused);
        let tree = &self.tree;
        let mut notices: Vec<(String, bool)> = vec![];
        let mut attention = false;
        let mut wake: Option<Duration> = None;
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            // Se está mirando: la pestaña se ve y la ventana está delante.
            let watched =
                focused && (index == self.active || (split_visible && tiles.contains(&index)));
            if watched {
                tab.alert = None;
            }
            let TabKind::Terminal(term) = &mut tab.kind else {
                continue;
            };
            // Grabación: la automática de los terminales nuevos, y los
            // problemas de las que están en curso.
            if !std::mem::replace(&mut term.cast_offered, true)
                && record_always
                && term.cast.is_none()
                && let Err(e) = term.start_cast(&cast_dir, &tab.title)
            {
                notices.push((format!("No se pudo grabar la sesión: {e:#}"), true));
            }
            if let Some(error) = term.cast.as_mut().and_then(|c| c.error.take()) {
                // Disco lleno, carpeta borrada…: se deja de grabar.
                notices.push((error, true));
                term.cast = None;
            }
            // Historial: se siguen las órdenes de los equipos que lo quieran.
            let history_key =
                history_key(tab.session.as_ref(), tree).filter(|key| history.records(key));
            term.set_command_capture(history_key.is_some());
            let session = tab.session.as_ref().map(|s| s.id);
            let fired = term.run_triggers(&rules, &|scope| match scope {
                Scope::All => true,
                Scope::Session(id) => session == Some(*id),
                Scope::Folder(id) => session.is_some_and(|s| tree.is_inside(*id, s)),
            });
            for fired in fired {
                let (text, is_error) = describe_fired(&tab.title, &fired);
                if !watched {
                    tab.alert = Some(TabAlert {
                        text: text.clone(),
                        bell: fired.bell,
                    });
                }
                attention |= fired.bell;
                notices.push((text, is_error));
            }
            for signal in std::mem::take(&mut term.signals) {
                match signal {
                    // De lo que se está mirando no hace falta avisar.
                    Signal::CommandDone(done) if !watched => {
                        let text = describe_done(&tab.title, &done);
                        let bell = rules.long_command.bell;
                        tab.alert = Some(TabAlert {
                            text: text.clone(),
                            bell,
                        });
                        attention |= bell;
                        notices.push((text, false));
                    }
                    Signal::CommandDone(_) => {}
                    Signal::Command(command) => {
                        if let Some(key) = &history_key {
                            let name = match &tab.session {
                                Some(session) => session.display_name(),
                                None => "Terminales locales".to_string(),
                            };
                            store.record(key, &name, &command, now, history.max_per_host);
                        }
                    }
                }
            }
            if let Some(time) = term.next_wake() {
                wake = Some(wake.map_or(time, |w| w.min(time)));
            }
        }
        // Solo cabe un aviso: el último, y antes que nada los problemas.
        notices.sort_by_key(|(_, is_error)| *is_error);
        if let Some((text, is_error)) = notices.pop() {
            self.notify(text, is_error);
        }
        if attention && !focused {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Informational,
            ));
        }
        if let Some(time) = wake {
            // Sin salida nueva no habría otro fotograma en el que mirarlo.
            ctx.request_repaint_after(time + Duration::from_millis(15));
        }
        // El historial se guarda poco después de cada orden, no en cada una.
        let tools = &mut self.term_tools;
        if tools.history.is_dirty() {
            if tools.history_saved.elapsed() >= HISTORY_SAVE_EVERY {
                tools.history_saved = Instant::now();
                let result = tools.history.save(&crate::cmd_history::history_file());
                // Si no se puede escribir se seguirá intentando, pero se
                // dice una sola vez.
                let already_failed = std::mem::replace(&mut tools.history_failed, result.is_err());
                if let Err(e) = result {
                    if !already_failed {
                        self.notify(
                            format!("No se pudo guardar el historial de órdenes: {e:#}"),
                            true,
                        );
                    }
                    ctx.request_repaint_after(HISTORY_SAVE_EVERY);
                }
            } else {
                ctx.request_repaint_after(HISTORY_SAVE_EVERY);
            }
        }
    }

    /// Ventanas de estas herramientas.
    pub(super) fn term_tools_windows(&mut self, ctx: &egui::Context) {
        if let Some(window) = &mut self.term_tools.triggers_window {
            let (open, changed) = window.show(ctx, &mut self.settings.triggers, &self.tree);
            if changed && let Err(e) = self.settings.save(&crate::paths::settings_file()) {
                self.notify(
                    format!("No se pudieron guardar las preferencias: {e:#}"),
                    true,
                );
            }
            if !open {
                self.term_tools.triggers_window = None;
                self.focus_active_terminal();
            }
        }
        let before = self.term_tools.players.len();
        self.term_tools
            .players
            .retain_mut(|player| player.show(ctx));
        if self.term_tools.players.len() != before {
            self.focus_active_terminal();
        }
        if std::mem::take(&mut self.term_tools.open_history) {
            self.open_history_window();
        }
        self.history_window_ui(ctx);
    }

    /// Clave y nombre del equipo de la pestaña activa, para su historial.
    fn active_history_host(&self) -> Option<String> {
        let tab = self.tabs.get(self.active)?;
        matches!(tab.kind, TabKind::Terminal(_))
            .then(|| history_key(tab.session.as_ref(), &self.tree))
            .flatten()
    }

    /// Abre el historial de órdenes por el equipo de la pestaña activa.
    pub(super) fn open_history_window(&mut self) {
        let host = self
            .active_history_host()
            .or_else(|| {
                self.term_tools
                    .history
                    .hosts()
                    .next()
                    .map(|(key, _)| key.to_string())
            })
            .unwrap_or_else(|| LOCAL_KEY.to_string());
        match &mut self.term_tools.history_window {
            Some(window) => window.set_host(host),
            None => self.term_tools.history_window = Some(HistoryWindow::new(host)),
        }
    }

    fn history_window_ui(&mut self, ctx: &egui::Context) {
        let Some(mut window) = self.term_tools.history_window.take() else {
            return;
        };
        // Equipos que se pueden elegir: los que tienen historial y el de la
        // pestaña activa, con el nombre que tienen ahora en el árbol.
        let paths = self.tree.session_paths();
        let name_of = |key: &str, stored: &str| -> String {
            if key == LOCAL_KEY {
                return "Terminales locales".to_string();
            }
            paths
                .iter()
                .find(|(id, _)| id.to_string() == key)
                .map(|(_, path)| path.clone())
                .unwrap_or_else(|| format!("{stored} (eliminado)"))
        };
        let mut hosts: Vec<(String, String)> = self
            .term_tools
            .history
            .hosts()
            .map(|(key, stored)| (key.to_string(), name_of(key, stored)))
            .collect();
        if let Some(key) = self.active_history_host()
            && !hosts.iter().any(|(k, _)| *k == key)
        {
            let name = name_of(&key, "");
            hosts.push((key, name));
        }
        hosts.sort_by_key(|(_, name)| name.to_lowercase());
        let target = match self.tabs.get(self.active) {
            Some(tab) if matches!(&tab.kind, TabKind::Terminal(t) if t.is_connected()) => {
                Some(tab.title.clone())
            }
            _ => None,
        };
        let (open, changed, request) = window.show(
            ctx,
            &mut self.term_tools.history,
            &mut self.settings.history,
            &hosts,
            target.as_deref(),
        );
        if changed {
            self.settings.history.sanitize();
            if let Err(e) = self.settings.save(&crate::paths::settings_file()) {
                self.notify(
                    format!("No se pudieron guardar las preferencias: {e:#}"),
                    true,
                );
            }
        }
        // Enviada una orden, la ventana se quita de en medio: lo que sigue
        // pasa en el terminal.
        let sent = match request {
            HistoryRequest::None => false,
            HistoryRequest::Type(command) => {
                self.type_in_terminal(&command, false);
                true
            }
            HistoryRequest::Run(command) => {
                self.type_in_terminal(&command, true);
                true
            }
        };
        if open && !sent {
            self.term_tools.history_window = Some(window);
        } else {
            self.focus_active_terminal();
        }
    }

    /// Escribe una orden en el terminal activo, como si se pegara, y si se
    /// pide pulsa Intro.
    pub(super) fn type_in_terminal(&mut self, command: &str, run: bool) {
        let Some(term) = self.active_terminal() else {
            return self.notify("Abre un terminal conectado para enviarle la orden", true);
        };
        term.apply_input(&TermInput::Paste(command.to_string()));
        if run {
            term.apply_input(&TermInput::Key(egui::Key::Enter, egui::Modifiers::NONE));
        }
        term.request_focus();
    }

    /// Las últimas órdenes del equipo de la pestaña activa, para la paleta:
    /// elegir una la deja escrita en el terminal.
    pub(super) fn history_palette_items(&self, items: &mut Vec<PaletteItem>) {
        let Some(key) = self.active_history_host() else {
            return;
        };
        for entry in self
            .term_tools
            .history
            .entries(&key)
            .iter()
            .take(PALETTE_HISTORY)
        {
            let mut label: String = entry.command.chars().take(90).collect();
            if label.len() < entry.command.len() {
                label.push('…');
            }
            items.push(PaletteItem {
                kind: "Historial",
                label,
                detail: match entry.count {
                    0 | 1 => String::new(),
                    n => format!("×{n}"),
                },
                action: PaletteAction::TypeCommand(entry.command.clone()),
            });
        }
    }

    /// Abre una grabación en un reproductor.
    pub(super) fn open_cast(&mut self, path: &Path) {
        if self.term_tools.players.iter().any(|p| p.path() == path) {
            return;
        }
        let id = self.term_tools.next_player;
        self.term_tools.next_player += 1;
        match PlayerWindow::open(path, id) {
            Ok(player) => self.term_tools.players.push(player),
            Err(e) => self.notify(format!("{e:#}"), true),
        }
    }

    /// Pide un fichero `.cast` y lo abre en un reproductor.
    pub(super) fn pick_cast(&mut self) {
        let dir = self.settings.log.dir();
        let mut dialog = rfd::FileDialog::new()
            .set_title("Reproducir una grabación")
            .add_filter(
                "Grabaciones de terminal (asciicast)",
                &[crate::cast::EXTENSION],
            )
            .add_filter("Todos los ficheros", &["*"]);
        if dir.is_dir() {
            dialog = dialog.set_directory(&dir);
        }
        if let Some(path) = dialog.pick_file() {
            self.open_cast(&path);
        }
    }

    /// Empieza o termina la grabación del terminal de la pestaña activa.
    pub(super) fn toggle_active_cast(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let TabKind::Terminal(term) = &mut tab.kind else {
            return;
        };
        let (text, is_error) = toggle_cast(term, &tab.title, &self.settings.log);
        self.term_tools.recent = None;
        self.notify(text, is_error);
    }

    /// `Some(true)` si el terminal de la pestaña activa se está grabando;
    /// `None` si la pestaña activa no es un terminal.
    pub(super) fn active_cast(&self) -> Option<bool> {
        match self.tabs.get(self.active).map(|t| &t.kind) {
            Some(TabKind::Terminal(term)) => Some(term.cast.is_some()),
            _ => None,
        }
    }

    pub(super) fn open_triggers_window(&mut self) {
        self.term_tools.triggers_window = Some(TriggersWindow::new(&self.settings.triggers));
    }

    /// Menú de la barra de herramientas.
    pub(super) fn term_tools_menu(&mut self, ui: &mut egui::Ui) {
        enum Chosen {
            Triggers,
            History,
            ToggleCast,
            PickCast,
            Play(PathBuf),
            Folder,
        }
        let rules = self
            .settings
            .triggers
            .rules
            .iter()
            .filter(|rule| rule.enabled)
            .count();
        let recording = self.active_cast();
        let mut chosen = None;
        let menu = ui.menu_button("🛠 Herramientas", |ui| {
            ui.set_min_width(300.0);
            let triggers = match rules {
                0 => "🔔 Disparadores…".to_string(),
                1 => "🔔 Disparadores… (1 activo)".to_string(),
                n => format!("🔔 Disparadores… ({n} activos)"),
            };
            if ui
                .button(triggers)
                .on_hover_text(
                    "Avisar o contestar cuando aparezca un texto en un terminal, y avisar \
                     cuando termine una orden larga",
                )
                .clicked()
            {
                chosen = Some(Chosen::Triggers);
            }
            if ui
                .button("🕘 Historial de órdenes…")
                .on_hover_text(
                    "Las órdenes ejecutadas en cada equipo, para buscarlas y volver a \
                     enviarlas",
                )
                .clicked()
            {
                chosen = Some(Chosen::History);
            }
            ui.separator();
            let label = if recording == Some(true) {
                "⏹ Terminar la grabación de esta sesión"
            } else {
                "🎬 Grabar esta sesión (.cast)"
            };
            if ui
                .add_enabled(recording.is_some(), egui::Button::new(label))
                .on_disabled_hover_text("Abre un terminal")
                .clicked()
            {
                chosen = Some(Chosen::ToggleCast);
            }
            if ui.button("▶ Reproducir una grabación…").clicked() {
                chosen = Some(Chosen::PickCast);
            }
            // Las últimas de la carpeta de registros, a un clic.
            let dir = self.settings.log.dir();
            let stale = self
                .term_tools
                .recent
                .as_ref()
                .is_none_or(|(at, _)| at.elapsed() > RECENT_TTL);
            if stale {
                self.term_tools.recent = Some((Instant::now(), recent_casts(&dir)));
            }
            let recent = self
                .term_tools
                .recent
                .as_ref()
                .map(|(_, list)| list.as_slice())
                .unwrap_or_default();
            if !recent.is_empty() {
                ui.weak("Últimas grabaciones");
            }
            for path in recent {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if ui
                    .add(egui::Button::new(format!("▶ {name}")).truncate())
                    .on_hover_text(path.display().to_string())
                    .clicked()
                {
                    chosen = Some(Chosen::Play(path.clone()));
                }
            }
            if ui
                .button("📂 Abrir la carpeta de las grabaciones")
                .clicked()
            {
                chosen = Some(Chosen::Folder);
            }
            if chosen.is_some() {
                ui.close();
            }
        });
        menu.response
            .on_hover_text("Disparadores, historial de órdenes y grabaciones de los terminales");
        match chosen {
            None => {}
            Some(Chosen::Triggers) => self.open_triggers_window(),
            Some(Chosen::History) => self.open_history_window(),
            Some(Chosen::ToggleCast) => self.toggle_active_cast(),
            Some(Chosen::PickCast) => self.pick_cast(),
            Some(Chosen::Play(path)) => self.open_cast(&path),
            Some(Chosen::Folder) => {
                let dir = self.settings.log.dir();
                let _ = std::fs::create_dir_all(&dir);
                if let Err(e) = crate::paths::open_with_system(&dir) {
                    self.notify(format!("No se pudo abrir {}: {e}", dir.display()), true);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_say_what_happened() {
        let fired = Fired {
            rule: uuid::Uuid::new_v4(),
            name: "Disco".into(),
            line: "ERROR: disco lleno".into(),
            notify: true,
            bell: false,
            send: None,
            paused: false,
        };
        assert_eq!(
            describe_fired("web-01", &fired),
            ("🔔 web-01: Disco — «ERROR: disco lleno»".to_string(), false)
        );
        // Que una regla ha escrito en el terminal se dice siempre.
        let sent = Fired {
            send: Some(b"y\r".to_vec()),
            notify: false,
            ..fired.clone()
        };
        let (text, is_error) = describe_fired("web-01", &sent);
        assert!(text.contains("ha contestado") && !is_error, "{text}");
        let paused = Fired {
            paused: true,
            ..fired
        };
        let (text, is_error) = describe_fired("web-01", &paused);
        assert!(text.contains("en pausa") && is_error, "{text}");

        let done = |secs, exit_code, exact| Finished {
            duration: Duration::from_secs(secs),
            exit_code,
            exact,
        };
        assert_eq!(
            describe_done("bd", &done(125, Some(0), true)),
            "bd: la orden ha terminado tras 2 min 5 s"
        );
        assert_eq!(
            describe_done("bd", &done(40, Some(2), true)),
            "bd: la orden ha terminado con error (código 2) tras 40 s"
        );
        assert_eq!(
            describe_done("bd", &done(40, None, true)),
            "bd: la orden ha terminado tras 40 s"
        );
        assert!(describe_done("bd", &done(40, None, false)).contains("parece"));
    }

    #[test]
    fn history_keys_follow_the_tree() {
        let mut tree = Tree::default();
        let saved = Session {
            name: "web".into(),
            ..Default::default()
        };
        tree.insert(None, crate::model::Node::Session(saved.clone()));
        // Un equipo del árbol tiene su historial; los terminales locales
        // comparten uno; una conexión rápida no tiene dónde guardarlo.
        assert_eq!(history_key(Some(&saved), &tree), Some(saved.id.to_string()));
        assert_eq!(history_key(None, &tree).as_deref(), Some(LOCAL_KEY));
        let quick = Session::default();
        assert_eq!(history_key(Some(&quick), &tree), None);
        // Al borrar un equipo, su historial deja de tener dueño.
        assert!(host_exists(LOCAL_KEY, &tree));
        assert!(host_exists(&saved.id.to_string(), &tree));
        assert!(!host_exists(&quick.id.to_string(), &tree));
        assert!(!host_exists("basura", &tree));
        tree.remove(saved.id);
        assert!(!host_exists(&saved.id.to_string(), &tree));
    }

    #[test]
    fn recent_recordings_are_listed_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        assert!(recent_casts(&dir.path().join("no-existe")).is_empty());
        assert!(recent_casts(dir.path()).is_empty());
        let now = std::time::SystemTime::now();
        for n in 0..RECENT_CASTS + 3 {
            let path = dir.path().join(format!("sesion-{n:02}.cast"));
            let file = std::fs::File::create(&path).unwrap();
            file.set_modified(now - Duration::from_secs(1000 - n as u64 * 10))
                .unwrap();
        }
        // Lo que no es una grabación no cuenta.
        std::fs::write(dir.path().join("sesion.log"), "texto").unwrap();
        std::fs::create_dir(dir.path().join("carpeta.cast")).unwrap();
        std::fs::write(dir.path().join("MAYUS.CAST"), "{}").unwrap();
        let names: Vec<String> = recent_casts(dir.path())
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names.len(), RECENT_CASTS);
        assert_eq!(names[0], "MAYUS.CAST");
        assert_eq!(names[1], "sesion-10.cast");
        assert_eq!(names[2], "sesion-09.cast");
    }
}
