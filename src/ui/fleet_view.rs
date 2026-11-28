//! Ventana para ejecutar una orden en todos los equipos SSH de una carpeta
//! y ver, de cada uno, lo que contesta.

use std::collections::BTreeSet;

use uuid::Uuid;

use crate::fleet::{FleetRun, HostState};
use crate::i18n::{decimal, tr};
use crate::macros::MacroStore;
use crate::model::{Protocol, Tree};
use crate::theme::{error_color, ok_color, warn_color};
use crate::trf;

/// Lo que la ventana pide a la aplicación.
pub enum FleetRequest {
    None,
    /// Lanzar la orden en estos equipos.
    Start {
        sessions: Vec<Uuid>,
        command: String,
        parallel: usize,
        timeout_secs: u64,
    },
}

pub struct FleetWindow {
    /// Carpeta cuyos equipos se listan; `None` son todos.
    folder: Option<Uuid>,
    /// Equipos desmarcados de la lista.
    skipped: BTreeSet<Uuid>,
    command: String,
    parallel: usize,
    timeout_secs: u64,
    pub run: Option<FleetRun>,
    /// Equipo cuya salida se está viendo.
    shown: Option<Uuid>,
    /// Se ha pulsado «Ejecutar» y falta el «sí».
    confirming: bool,
    message: Option<(String, bool)>,
}

impl FleetWindow {
    pub fn new(folder: Option<Uuid>) -> Self {
        Self {
            folder,
            skipped: BTreeSet::new(),
            command: String::new(),
            parallel: 4,
            timeout_secs: 60,
            run: None,
            shown: None,
            confirming: false,
            message: None,
        }
    }

    /// Equipos SSH de la carpeta elegida (con sus subcarpetas).
    fn hosts(&self, tree: &Tree) -> Vec<(Uuid, String)> {
        tree.session_paths()
            .into_iter()
            .filter(|(id, _)| {
                tree.session(*id)
                    .is_some_and(|s| s.protocol == Protocol::Ssh)
                    && self.folder.is_none_or(|folder| tree.is_inside(folder, *id))
            })
            .collect()
    }

    /// Devuelve si la ventana sigue abierta y lo que haya que hacer.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        tree: &Tree,
        macros: &MacroStore,
    ) -> (bool, FleetRequest) {
        let mut open = true;
        let mut request = FleetRequest::None;
        let hosts = self.hosts(tree);
        let running = self.run.as_ref().is_some_and(|run| !run.is_finished());
        egui::Window::new(tr("Ejecutar en varios equipos"))
            .id(egui::Id::unique("fleet_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos([230.0, 60.0])
            .show(ctx, |ui| {
                ui.set_width(860.0);
                ui.horizontal(|ui| {
                    ui.label(tr("Equipos de"));
                    let folders = tree.folder_paths();
                    let current = match self.folder {
                        None => tr("todas las carpetas").to_string(),
                        Some(id) => folders
                            .iter()
                            .find(|(fid, _)| *fid == id)
                            .map(|(_, path)| path.clone())
                            .unwrap_or_else(|| tr("(carpeta eliminada)").into()),
                    };
                    ui.add_enabled_ui(!running, |ui| {
                        egui::ComboBox::from_id_salt("fleet_folder")
                            .selected_text(current)
                            .width(300.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.folder,
                                    None,
                                    tr("todas las carpetas"),
                                );
                                for (id, path) in &folders {
                                    ui.selectable_value(&mut self.folder, Some(*id), path);
                                }
                            });
                    });
                    ui.separator();
                    ui.label(tr("A la vez"));
                    ui.add_enabled(
                        !running,
                        egui::DragValue::new(&mut self.parallel).range(1..=32),
                    );
                    ui.label(tr("Tiempo máximo"));
                    ui.add_enabled(
                        !running,
                        egui::DragValue::new(&mut self.timeout_secs)
                            .range(2..=3600)
                            .suffix(" s"),
                    )
                    .on_hover_text(tr("Lo que cada equipo puede tardar en conectar y terminar"));
                });
                ui.separator();
                ui.allocate_ui_with_layout(
                    egui::vec2(860.0, 430.0),
                    egui::Layout::left_to_right(egui::Align::TOP),
                    |ui| {
                        ui.vertical(|ui| {
                            ui.set_width(300.0);
                            self.host_list(ui, &hosts, running);
                        });
                        ui.separator();
                        ui.vertical(|ui| {
                            ui.set_width(540.0);
                            request = self.command_panel(ui, &hosts, macros, running);
                            ui.separator();
                            self.output_panel(ui);
                        });
                    },
                );
            });
        (open, request)
    }

    fn host_list(&mut self, ui: &mut egui::Ui, hosts: &[(Uuid, String)], running: bool) {
        ui.horizontal(|ui| {
            ui.strong(match &self.run {
                Some(run) => trf!("Resultado ({n})", n = run.hosts.len()),
                None => trf!("Equipos SSH ({n})", n = hosts.len()),
            });
            if self.run.is_none() {
                if ui.small_button(tr("Todos")).clicked() {
                    self.skipped.clear();
                }
                if ui.small_button(tr("Ninguno")).clicked() {
                    self.skipped = hosts.iter().map(|(id, _)| *id).collect();
                }
            } else if !running && ui.small_button(tr("Elegir de nuevo")).clicked() {
                self.run = None;
                self.shown = None;
            }
        });
        egui::ScrollArea::vertical()
            .id_salt("fleet_hosts")
            .max_height(400.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if hosts.is_empty() {
                    ui.weak(tr("No hay equipos SSH en esta carpeta."));
                }
                match &self.run {
                    // Con una ejecución hecha (o en curso): su estado.
                    Some(run) => {
                        for host in &run.hosts {
                            ui.horizontal(|ui| {
                                let (color, text) = match &host.state {
                                    HostState::Waiting => {
                                        (ui.visuals().weak_text_color(), tr("en cola").to_string())
                                    }
                                    HostState::Connecting(_) => {
                                        (warn_color(), tr("conectando").to_string())
                                    }
                                    HostState::Running => {
                                        (warn_color(), tr("en marcha").to_string())
                                    }
                                    HostState::Done(Some(0)) => {
                                        (ok_color(), tr("bien").to_string())
                                    }
                                    HostState::Done(Some(code)) => {
                                        (error_color(), trf!("código {code}", code))
                                    }
                                    HostState::Done(None) => {
                                        (ok_color(), tr("terminado").to_string())
                                    }
                                    HostState::Failed(_) => {
                                        (error_color(), tr("error").to_string())
                                    }
                                    HostState::Cancelled => (
                                        ui.visuals().weak_text_color(),
                                        tr("cancelado").to_string(),
                                    ),
                                };
                                super::status_dot(ui, color);
                                let label = ui
                                    .selectable_label(self.shown == Some(host.id), &host.name)
                                    .on_hover_text(match &host.state {
                                        HostState::Failed(error) => error.clone(),
                                        HostState::Connecting(step) => step.clone(),
                                        _ => text.clone(),
                                    });
                                if label.clicked() {
                                    self.shown = Some(host.id);
                                }
                                ui.weak(format!(
                                    "{text} · {} s",
                                    decimal(host.elapsed.as_secs_f64(), 1)
                                ));
                            });
                        }
                    }
                    // Antes de ejecutar: en cuáles se va a lanzar.
                    None => {
                        for (id, path) in hosts {
                            let mut included = !self.skipped.contains(id);
                            if ui.checkbox(&mut included, path).changed() {
                                if included {
                                    self.skipped.remove(id);
                                } else {
                                    self.skipped.insert(*id);
                                }
                            }
                        }
                    }
                }
            });
    }

    fn command_panel(
        &mut self,
        ui: &mut egui::Ui,
        hosts: &[(Uuid, String)],
        macros: &MacroStore,
        running: bool,
    ) -> FleetRequest {
        let mut request = FleetRequest::None;
        ui.horizontal(|ui| {
            ui.strong(tr("Orden"));
            ui.add_enabled_ui(!running && !macros.macros.is_empty(), |ui| {
                ui.menu_button(tr("Usar un snippet"), |ui| {
                    for snippet in &macros.macros {
                        if ui.button(&snippet.name).clicked() {
                            self.command = snippet.text.trim_end().to_string();
                            ui.close();
                        }
                    }
                });
            });
        });
        ui.add_enabled(
            !running,
            egui::TextEdit::multiline(&mut self.command)
                .font(egui::TextStyle::Monospace)
                .desired_rows(4)
                .desired_width(f32::INFINITY)
                .hint_text(tr("por ejemplo: uptime; df -h /")),
        );
        ui.weak(tr(
            "Se ejecuta sin terminal, como «ssh equipo 'orden'»: no sirve para órdenes que \
             preguntan algo. Admite las variables de los snippets ({{host}}, {{usuario}}, \
             {{nombre}}, {{fecha}}…), que se sustituyen en cada equipo.",
        ));
        let chosen: Vec<Uuid> = hosts
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| !self.skipped.contains(id))
            .collect();
        ui.horizontal(|ui| {
            if running {
                if ui.button(tr("⏹ Cancelar")).clicked()
                    && let Some(run) = &self.run
                {
                    run.cancel();
                }
            } else if self.confirming {
                ui.colored_label(
                    warn_color(),
                    trf!("Se ejecutará en {n} equipos. ¿Seguro?", n = chosen.len()),
                );
                if ui.button(tr("Sí, ejecutar")).clicked() {
                    self.confirming = false;
                    request = self.start(&chosen);
                }
                if ui.button(tr("No")).clicked() {
                    self.confirming = false;
                }
            } else {
                let ready = !chosen.is_empty() && !self.command.trim().is_empty();
                let label = match chosen.len() {
                    1 => tr("▶ Ejecutar en 1 equipo").to_string(),
                    n => trf!("▶ Ejecutar en {n} equipos", n),
                };
                if ui.add_enabled(ready, egui::Button::new(label)).clicked() {
                    // Con varios equipos, una pregunta más: es fácil
                    // lanzar algo donde no se quería.
                    if chosen.len() > 1 {
                        self.confirming = true;
                    } else {
                        request = self.start(&chosen);
                    }
                }
            }
            if let Some(run) = &self.run {
                let (ok, failed, pending) = run.summary();
                ui.colored_label(ok_color(), trf!("{ok} bien", ok));
                if failed > 0 {
                    ui.colored_label(error_color(), trf!("{failed} con error", failed));
                }
                if pending > 0 {
                    ui.colored_label(warn_color(), trf!("{pending} sin terminar", pending));
                }
            }
        });
        if let Some((message, is_error)) = &self.message {
            let color = if *is_error { error_color() } else { ok_color() };
            ui.colored_label(color, message);
        }
        request
    }

    fn start(&mut self, chosen: &[Uuid]) -> FleetRequest {
        self.message = None;
        self.shown = None;
        FleetRequest::Start {
            sessions: chosen.to_vec(),
            command: self.command.trim().to_string(),
            parallel: self.parallel,
            timeout_secs: self.timeout_secs,
        }
    }

    fn output_panel(&mut self, ui: &mut egui::Ui) {
        let Some(run) = &self.run else {
            ui.weak(tr("Aquí aparecerá lo que conteste cada equipo."));
            return;
        };
        // Sin elegir ninguno se enseña el primero que tenga algo que decir.
        let shown = self
            .shown
            .and_then(|id| run.hosts.iter().find(|h| h.id == id))
            .or_else(|| run.hosts.iter().find(|h| !h.output.is_empty()))
            .or(run.hosts.first());
        let Some(host) = shown else {
            return;
        };
        let mut save = false;
        ui.horizontal(|ui| {
            ui.strong(&host.name);
            if ui.button(tr("Copiar su salida")).clicked() {
                ui.ctx().copy_text(host.output.clone());
            }
            if ui
                .button(tr("Copiar todo"))
                .on_hover_text(tr(
                    "La salida de todos los equipos, con una cabecera por equipo",
                ))
                .clicked()
            {
                ui.ctx().copy_text(run.report());
            }
            if ui.button(tr("Guardar todo…")).clicked() {
                save = true;
            }
        });
        if let HostState::Failed(error) = &host.state {
            ui.colored_label(error_color(), error);
        }
        egui::ScrollArea::both()
            .id_salt("fleet_output")
            .max_height(210.0)
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(&host.output).monospace())
                        .wrap_mode(egui::TextWrapMode::Extend),
                );
                if host.truncated {
                    ui.colored_label(warn_color(), tr("[salida recortada: era demasiado larga]"));
                }
            });
        if save
            && let Some(path) = rfd::FileDialog::new()
                .set_title(tr("Guardar la salida de todos los equipos"))
                .set_file_name(tr("salida-equipos.txt"))
                .save_file()
        {
            self.message = Some(match std::fs::write(&path, run.report()) {
                Ok(()) => (trf!("Guardado en {ruta}", ruta = path.display()), false),
                Err(error) => (trf!("No se pudo guardar: {error}", error), true),
            });
        }
    }
}
