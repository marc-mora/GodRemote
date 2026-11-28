//! Ventana del gestor de túneles: túneles SSH guardados que se arrancan y
//! se paran sin abrir un terminal.

use std::collections::HashMap;

use uuid::Uuid;

use crate::i18n::tr;
use crate::model::TunnelKind;
use crate::theme::{error_color, ok_color, warn_color};
use crate::trf;
use crate::tunnel_manager::{RunState, SavedTunnel, TunnelStore};

/// Estado de un túnel, tal como lo ve la ventana.
#[derive(Clone)]
pub struct TunnelView {
    pub state: RunState,
    /// Aviso de la reconexión automática, si está en ello.
    pub retry: Option<String>,
}

/// Lo que la ventana pide a la aplicación.
pub enum TunnelRequest {
    Start(Uuid),
    Stop(Uuid),
    /// Se ha eliminado: hay que pararlo si estaba en marcha.
    Removed(Uuid),
}

pub struct TunnelsWindow {
    /// Túnel guardado que se está editando; `None` es uno nuevo.
    selected: Option<Uuid>,
    draft: SavedTunnel,
    message: Option<(String, bool)>,
}

fn state_color(ui: &egui::Ui, view: Option<&TunnelView>) -> egui::Color32 {
    match view.map(|v| &v.state) {
        Some(RunState::Active) => ok_color(),
        Some(RunState::Connecting(_)) => warn_color(),
        Some(RunState::Failed(_)) => error_color(),
        Some(RunState::Stopped) | None => ui.visuals().weak_text_color(),
    }
}

fn is_running(view: Option<&TunnelView>) -> bool {
    matches!(
        view.map(|v| &v.state),
        Some(RunState::Active | RunState::Connecting(_))
    ) || view.is_some_and(|v| v.retry.is_some())
}

impl TunnelsWindow {
    pub fn new(store: &TunnelStore) -> Self {
        let mut window = Self {
            selected: None,
            draft: SavedTunnel::default(),
            message: None,
        };
        if let Some(first) = store.tunnels.first() {
            window.load(first);
        }
        window
    }

    fn load(&mut self, tunnel: &SavedTunnel) {
        self.selected = Some(tunnel.id);
        self.draft = tunnel.clone();
        self.message = None;
    }

    fn blank(&mut self) {
        self.selected = None;
        self.draft = SavedTunnel::default();
        self.message = None;
    }

    /// Guarda el túnel en edición. Devuelve `true` si se guardó.
    fn save(&mut self, store: &mut TunnelStore) -> bool {
        if let Err(e) = self.draft.validate() {
            self.message = Some((e, true));
            return false;
        }
        // Dos túneles no pueden escuchar en el mismo sitio de este equipo.
        let clash = store.tunnels.iter().find(|other| {
            other.id != self.draft.id
                && other.tunnel.kind != TunnelKind::Remote
                && self.draft.tunnel.kind != TunnelKind::Remote
                && other.tunnel.listen_port == self.draft.tunnel.listen_port
                && other.tunnel.listen_host.trim() == self.draft.tunnel.listen_host.trim()
        });
        if let Some(other) = clash {
            self.message = Some((
                trf!(
                    "«{nombre}» ya escucha en {host}:{puerto}",
                    nombre = other.display_name(),
                    host = other.tunnel.listen_host.trim(),
                    puerto = other.tunnel.listen_port
                ),
                true,
            ));
            return false;
        }
        self.draft.name = self.draft.name.trim().to_string();
        store.upsert(self.draft.clone());
        self.selected = Some(self.draft.id);
        if let Err(e) = store.save(&crate::paths::tunnels_file()) {
            self.message = Some((
                trf!("No se pudo guardar: {error}", error = format!("{e:#}")),
                true,
            ));
            return false;
        }
        self.message = Some((tr("Túnel guardado").into(), false));
        true
    }

    /// Devuelve si la ventana sigue abierta y lo que haya que hacer.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        store: &mut TunnelStore,
        sessions: &[(Uuid, String)],
        states: &HashMap<Uuid, TunnelView>,
    ) -> (bool, Vec<TunnelRequest>) {
        let mut open = true;
        let mut requests = vec![];
        egui::Window::new(tr("Túneles"))
            .id(egui::Id::unique("tunnels_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos([250.0, 70.0])
            .show(ctx, |ui| {
                ui.set_width(790.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(790.0, 360.0),
                    egui::Layout::left_to_right(egui::Align::TOP),
                    |ui| {
                        ui.vertical(|ui| {
                            ui.set_width(280.0);
                            self.list(ui, store, states, &mut requests);
                        });
                        ui.separator();
                        ui.vertical(|ui| {
                            ui.set_width(490.0);
                            self.editor(ui, store, sessions, states, &mut requests);
                        });
                    },
                );
                ui.separator();
                ui.weak(tr(
                    "Cada túnel abre su propia conexión SSH con el equipo elegido (pasando por \
                     sus saltos, si los tiene) y sigue en marcha aunque no haya ningún terminal \
                     abierto. Los cambios en un túnel en marcha se aplican al volver a iniciarlo.",
                ));
            });
        (open, requests)
    }

    fn list(
        &mut self,
        ui: &mut egui::Ui,
        store: &TunnelStore,
        states: &HashMap<Uuid, TunnelView>,
        requests: &mut Vec<TunnelRequest>,
    ) {
        ui.strong(tr("Túneles guardados"));
        let mut pick = None;
        egui::ScrollArea::vertical()
            .id_salt("tunnel_list")
            .max_height(310.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                if store.tunnels.is_empty() {
                    ui.weak(tr("Aún no hay túneles."));
                }
                for tunnel in &store.tunnels {
                    let view = states.get(&tunnel.id);
                    ui.horizontal(|ui| {
                        super::status_dot(ui, state_color(ui, view));
                        let running = is_running(view);
                        let (icon, hint) = if running {
                            ("⏹", tr("Detener"))
                        } else {
                            ("▶", tr("Iniciar"))
                        };
                        if ui.small_button(icon).on_hover_text(hint).clicked() {
                            requests.push(if running {
                                TunnelRequest::Stop(tunnel.id)
                            } else {
                                TunnelRequest::Start(tunnel.id)
                            });
                        }
                        let label = ui
                            .selectable_label(
                                self.selected == Some(tunnel.id),
                                tunnel.display_name(),
                            )
                            .on_hover_text(tunnel.tunnel.describe());
                        if label.clicked() {
                            pick = Some(tunnel.clone());
                        }
                    });
                }
            });
        if let Some(tunnel) = pick {
            self.load(&tunnel);
        }
        if ui.button(tr("➕ Nuevo túnel")).clicked() {
            self.blank();
        }
    }

    fn editor(
        &mut self,
        ui: &mut egui::Ui,
        store: &mut TunnelStore,
        sessions: &[(Uuid, String)],
        states: &HashMap<Uuid, TunnelView>,
        requests: &mut Vec<TunnelRequest>,
    ) {
        let draft = &mut self.draft;
        egui::Grid::new("tunnel_editor")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                ui.label(tr("Nombre"));
                ui.add(
                    egui::TextEdit::singleline(&mut draft.name)
                        .desired_width(280.0)
                        .hint_text(tr("opcional; por ejemplo «BD de producción»")),
                );
                ui.end_row();

                ui.label(tr("Equipo SSH"));
                let current = draft
                    .session
                    .and_then(|id| sessions.iter().find(|(sid, _)| *sid == id))
                    .map(|(_, label)| label.as_str());
                let text = match (current, draft.session) {
                    (Some(label), _) => label,
                    (None, Some(_)) => tr("(el equipo ya no existe)"),
                    (None, None) => tr("Elige un equipo…"),
                };
                egui::ComboBox::from_id_salt("tunnel_session")
                    .selected_text(text)
                    .width(288.0)
                    .show_ui(ui, |ui| {
                        if sessions.is_empty() {
                            ui.weak(tr("No hay equipos SSH guardados"));
                        }
                        for (id, label) in sessions {
                            ui.selectable_value(&mut draft.session, Some(*id), label);
                        }
                    });
                ui.end_row();

                ui.label(tr("Tipo"));
                egui::ComboBox::from_id_salt("tunnel_kind_standalone")
                    .selected_text(tr(draft.tunnel.kind.label()))
                    .width(190.0)
                    .show_ui(ui, |ui| {
                        for kind in TunnelKind::ALL {
                            ui.selectable_value(&mut draft.tunnel.kind, kind, tr(kind.label()));
                        }
                    });
                ui.end_row();

                ui.label(match draft.tunnel.kind {
                    TunnelKind::Remote => tr("Escucha en el servidor"),
                    _ => tr("Escucha en este equipo"),
                });
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.tunnel.listen_host)
                            .desired_width(180.0)
                            .hint_text("127.0.0.1"),
                    )
                    .on_hover_text(tr(
                        "127.0.0.1 solo deja entrar desde el propio equipo; 0.0.0.0 lo abre a \
                         toda su red",
                    ));
                    ui.add(egui::DragValue::new(&mut draft.tunnel.listen_port).range(1..=65535));
                });
                ui.end_row();

                if draft.tunnel.kind != TunnelKind::Dynamic {
                    ui.label(match draft.tunnel.kind {
                        TunnelKind::Remote => tr("Destino, visto desde aquí"),
                        _ => tr("Destino, visto desde el servidor"),
                    });
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut draft.tunnel.target_host)
                                .desired_width(180.0)
                                .hint_text("host"),
                        );
                        ui.add(
                            egui::DragValue::new(&mut draft.tunnel.target_port).range(1..=65535),
                        );
                    });
                    ui.end_row();
                }

                ui.label("");
                ui.checkbox(&mut draft.autostart, tr("Iniciarlo al abrir GodRemote"));
                ui.end_row();
            });
        ui.add_space(2.0);
        ui.weak(match draft.tunnel.kind {
            TunnelKind::Local => tr(
                "Local: lo que se conecte al puerto de este equipo sale por el servidor hacia \
                 el destino.",
            ),
            TunnelKind::Remote => tr(
                "Remoto: lo que se conecte al puerto del servidor llega, a través de este \
                 equipo, al destino.",
            ),
            TunnelKind::Dynamic => tr(
                "Dinámico: un proxy SOCKS5 en este equipo; cada programa que lo use sale a la \
                 red por el servidor.",
            ),
        });
        ui.add_space(6.0);

        let view = self.selected.and_then(|id| states.get(&id));
        let running = is_running(view);
        ui.horizontal(|ui| {
            if ui.button(tr("Guardar")).clicked() {
                self.save(store);
            }
            let saved = self.selected.is_some();
            if running {
                if ui.button(tr("⏹ Detener")).clicked()
                    && let Some(id) = self.selected
                {
                    requests.push(TunnelRequest::Stop(id));
                }
            } else if ui
                .add_enabled(saved, egui::Button::new(tr("▶ Iniciar")))
                .on_disabled_hover_text(tr("Guarda el túnel para poder iniciarlo"))
                .clicked()
                && let Some(id) = self.selected
            {
                // Se inicia lo que se ve: primero se guardan los cambios.
                if store.get(id) == Some(&self.draft) || self.save(store) {
                    self.message = None;
                    requests.push(TunnelRequest::Start(id));
                }
            }
            if ui
                .add_enabled(saved, egui::Button::new(tr("🗑 Eliminar")))
                .clicked()
                && let Some(id) = self.selected
            {
                store.remove(id);
                requests.push(TunnelRequest::Removed(id));
                match store.save(&crate::paths::tunnels_file()) {
                    Ok(()) => {
                        match store.tunnels.first().cloned() {
                            Some(first) => self.load(&first),
                            None => self.blank(),
                        }
                        self.message = Some((tr("Túnel eliminado").into(), false));
                    }
                    Err(e) => {
                        self.message = Some((
                            trf!("No se pudo guardar: {error}", error = format!("{e:#}")),
                            true,
                        ))
                    }
                }
            }
        });

        let view = self.selected.and_then(|id| states.get(&id));
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            super::status_dot(ui, state_color(ui, view));
            match view {
                Some(TunnelView {
                    retry: Some(retry), ..
                }) => ui.colored_label(warn_color(), retry),
                Some(TunnelView { state, .. }) => match state {
                    RunState::Active => ui.colored_label(
                        ok_color(),
                        trf!(
                            "En marcha: {tunel}",
                            tunel = self.draft_saved(store).tunnel.describe()
                        ),
                    ),
                    RunState::Connecting(text) => ui.colored_label(warn_color(), text),
                    RunState::Failed(error) => ui.colored_label(error_color(), error),
                    RunState::Stopped => ui.weak(tr("Detenido")),
                },
                None => ui.weak(tr("Detenido")),
            };
        });
        if let Some((message, is_error)) = &self.message {
            let color = if *is_error { error_color() } else { ok_color() };
            ui.colored_label(color, message);
        }
    }

    /// El túnel seleccionado tal como está guardado (lo que está en marcha),
    /// o el borrador si aún no se ha guardado.
    fn draft_saved<'a>(&'a self, store: &'a TunnelStore) -> &'a SavedTunnel {
        self.selected
            .and_then(|id| store.get(id))
            .unwrap_or(&self.draft)
    }
}
