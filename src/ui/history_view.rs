//! Ventana del historial de órdenes: las que se han ejecutado en cada
//! equipo, con buscador, para volver a enviarlas o copiarlas.

use crate::cmd_history::{
    HistorySettings, HistoryStore, LOCAL_KEY, MAX_PER_HOST, MIN_PER_HOST, ago,
};
use crate::theme::warn_color;

/// Lo que la ventana pide a la aplicación.
pub enum HistoryRequest {
    None,
    /// Escribir la orden en el terminal activo, sin ejecutarla.
    Type(String),
    /// Escribirla y pulsar Intro.
    Run(String),
}

pub struct HistoryWindow {
    /// Clave del equipo cuyo historial se muestra.
    host: String,
    query: String,
    /// Posición elegida en la lista (tras filtrar).
    selected: usize,
    /// El buscador ya tiene el foco.
    focused: bool,
    /// Hay que desplazar la lista hasta la elegida.
    reveal: bool,
    /// Se ha pedido borrarlo todo y falta el «sí».
    confirming_clear: bool,
}

impl HistoryWindow {
    pub fn new(host: String) -> Self {
        Self {
            host,
            query: String::new(),
            selected: 0,
            focused: false,
            reveal: false,
            confirming_clear: false,
        }
    }

    /// Muestra otro equipo.
    pub fn set_host(&mut self, host: String) {
        if host != self.host {
            self.host = host;
            self.selected = 0;
            self.reveal = true;
        }
        self.focused = false;
    }

    /// Dibuja la ventana.
    ///
    /// `hosts` son los equipos que se pueden elegir (clave y nombre) y
    /// `target`, el título del terminal conectado que recibiría una orden.
    /// Devuelve si sigue abierta, si han cambiado los ajustes y qué hacer.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        store: &mut HistoryStore,
        settings: &mut HistorySettings,
        hosts: &[(String, String)],
        target: Option<&str>,
    ) -> (bool, bool, HistoryRequest) {
        let mut open = true;
        let mut close = false;
        let mut request = HistoryRequest::None;
        let before = settings.clone();
        egui::Window::new("Historial de órdenes")
            .id(egui::Id::unique("history_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos([300.0, 70.0])
            .show(ctx, |ui| {
                ui.set_width(700.0);
                self.host_row(ui, settings, hosts);
                request = self.list(ui, store, target, &mut close);
                ui.separator();
                self.footer(ui, store, settings);
            });
        (open && !close, *settings != before, request)
    }

    fn host_row(
        &mut self,
        ui: &mut egui::Ui,
        settings: &mut HistorySettings,
        hosts: &[(String, String)],
    ) {
        ui.horizontal(|ui| {
            ui.label("Equipo");
            let current = hosts
                .iter()
                .find(|(key, _)| *key == self.host)
                .map_or("(sin historial)", |(_, name)| name.as_str());
            let mut chosen = None;
            egui::ComboBox::from_id_salt("history_host")
                .selected_text(current)
                .width(300.0)
                .show_ui(ui, |ui| {
                    for (key, name) in hosts {
                        if ui.selectable_label(*key == self.host, name).clicked() {
                            chosen = Some(key.clone());
                        }
                    }
                });
            if let Some(key) = chosen {
                self.set_host(key);
            }
            // Por equipo: dejar de recordar las de este.
            let mut remembered = !settings.is_excluded(&self.host);
            if ui
                .add_enabled(
                    settings.enabled,
                    egui::Checkbox::new(&mut remembered, "Recordar las órdenes de este equipo"),
                )
                .on_hover_text(
                    "Desmarcado, lo que ejecutes en este equipo no se anota (lo ya anotado \
                     sigue ahí hasta que lo borres)",
                )
                .changed()
            {
                settings.set_excluded(&self.host, !remembered);
            }
        });
    }

    fn list(
        &mut self,
        ui: &mut egui::Ui,
        store: &mut HistoryStore,
        target: Option<&str>,
        close: &mut bool,
    ) -> HistoryRequest {
        let mut request = HistoryRequest::None;
        let total = store.entries(&self.host).len();
        let found: Vec<(String, u32, i64)> = store
            .search(&self.host, &self.query)
            .into_iter()
            .map(|e| (e.command.clone(), e.count, e.last))
            .collect();
        if self.selected >= found.len() {
            self.selected = found.len().saturating_sub(1);
        }
        let mut remove = None;
        ui.horizontal(|ui| {
            ui.label("🔍");
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .desired_width(470.0)
                    .hint_text("Buscar entre sus órdenes…"),
            );
            if !self.focused {
                edit.request_focus();
                self.focused = true;
            }
            if edit.changed() {
                self.selected = 0;
                self.reveal = true;
            }
            // Con el foco en el buscador, las flechas recorren la lista e
            // Intro escribe la orden elegida (con Mayús, además la ejecuta).
            if edit.has_focus() || edit.lost_focus() {
                let (down, up, enter, shift, escape) = ui.input(|i| {
                    (
                        i.key_pressed(egui::Key::ArrowDown),
                        i.key_pressed(egui::Key::ArrowUp),
                        i.key_pressed(egui::Key::Enter),
                        i.modifiers.shift,
                        i.key_pressed(egui::Key::Escape),
                    )
                });
                if down && self.selected + 1 < found.len() {
                    self.selected += 1;
                    self.reveal = true;
                }
                if up && self.selected > 0 {
                    self.selected -= 1;
                    self.reveal = true;
                }
                if enter && let Some((command, ..)) = found.get(self.selected) {
                    request = if shift {
                        HistoryRequest::Run(command.clone())
                    } else {
                        HistoryRequest::Type(command.clone())
                    };
                }
                if escape {
                    *close = true;
                }
            }
            ui.weak(match (found.len(), total) {
                (_, 0) => "sin órdenes".to_string(),
                (shown, total) if shown == total => format!("{total} órdenes"),
                (shown, total) => format!("{shown} de {total}"),
            });
        });
        let now = chrono::Utc::now().timestamp();
        egui::ScrollArea::vertical()
            .id_salt("history_list")
            .max_height(320.0)
            .min_scrolled_height(320.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if total == 0 {
                    ui.add_space(8.0);
                    ui.weak("Aún no se ha anotado ninguna orden de este equipo.");
                } else if found.is_empty() {
                    ui.add_space(8.0);
                    ui.weak("Nada coincide con lo buscado.");
                }
                for (row, (command, count, last)) in found.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .small_button("🗑")
                                .on_hover_text("Borrar esta orden del historial")
                                .clicked()
                            {
                                remove = Some(command.clone());
                            }
                            let times = match count {
                                0 | 1 => String::new(),
                                n => format!("×{n} · "),
                            };
                            ui.weak(format!("{times}{}", ago(*last, now)));
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    let label = ui
                                        .add(
                                            egui::Button::selectable(
                                                row == self.selected,
                                                egui::RichText::new(command).monospace(),
                                            )
                                            .truncate(),
                                        )
                                        .on_hover_text(command);
                                    if label.double_clicked() {
                                        request = HistoryRequest::Type(command.clone());
                                    }
                                    if label.clicked() {
                                        self.selected = row;
                                    }
                                    if row == self.selected && self.reveal {
                                        label.scroll_to_me(None);
                                    }
                                },
                            );
                        });
                    });
                }
            });
        self.reveal = false;
        if let Some(command) = remove {
            store.remove(&self.host, &command);
        }
        let chosen = found.get(self.selected).map(|(command, ..)| command);
        ui.horizontal(|ui| {
            let can_send = chosen.is_some() && target.is_some();
            if ui
                .add_enabled(can_send, egui::Button::new("⌨ Escribir en el terminal"))
                .on_hover_text("La deja escrita, sin ejecutarla (Intro, o doble clic)")
                .on_disabled_hover_text("Elige una orden y abre un terminal conectado")
                .clicked()
                && let Some(command) = chosen
            {
                request = HistoryRequest::Type(command.clone());
            }
            if ui
                .add_enabled(can_send, egui::Button::new("▶ Ejecutar"))
                .on_hover_text("La escribe y pulsa Intro (Mayús+Intro)")
                .on_disabled_hover_text("Elige una orden y abre un terminal conectado")
                .clicked()
                && let Some(command) = chosen
            {
                request = HistoryRequest::Run(command.clone());
            }
            if ui
                .add_enabled(chosen.is_some(), egui::Button::new("📋 Copiar"))
                .clicked()
                && let Some(command) = chosen
            {
                ui.ctx().copy_text(command.clone());
            }
            match target {
                Some(title) => ui.weak(format!("se envía a «{title}»")),
                None => ui.weak("no hay un terminal conectado a la vista"),
            };
        });
        request
    }

    fn footer(
        &mut self,
        ui: &mut egui::Ui,
        store: &mut HistoryStore,
        settings: &mut HistorySettings,
    ) {
        ui.horizontal(|ui| {
            ui.checkbox(&mut settings.enabled, "Recordar las órdenes que ejecuto")
                .on_hover_text("En todos los equipos, salvo los que se excluyan uno a uno");
            ui.label("hasta");
            ui.add_enabled(
                settings.enabled,
                egui::DragValue::new(&mut settings.max_per_host).range(MIN_PER_HOST..=MAX_PER_HOST),
            );
            ui.label("por equipo");
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !store.entries(&self.host).is_empty(),
                    egui::Button::new("🗑 Borrar el historial de este equipo"),
                )
                .clicked()
            {
                store.clear_host(&self.host);
                self.selected = 0;
            }
            if self.confirming_clear {
                ui.colored_label(warn_color(), "¿Borrar el de todos los equipos?");
                if ui.button("Sí, borrar todo").clicked() {
                    store.clear();
                    self.selected = 0;
                    self.confirming_clear = false;
                }
                if ui.button("No").clicked() {
                    self.confirming_clear = false;
                }
            } else if ui
                .add_enabled(!store.is_empty(), egui::Button::new("Borrar todo…"))
                .clicked()
            {
                self.confirming_clear = true;
            }
        });
        let local = if self.host == LOCAL_KEY {
            " Los terminales locales comparten un mismo historial."
        } else {
            ""
        };
        ui.weak(format!(
            "Se anota lo que el terminal muestra al pulsar Intro, sin el indicador de la shell: \
             una contraseña tecleada sin eco no se guarda, y tampoco las respuestas a preguntas \
             («Usuario:», «¿Seguro? [s/N]») ni las órdenes que empiecen por un espacio. Si la \
             shell marca sus órdenes (OSC 133) solo se anotan las suyas; si no, también lo que \
             se escriba a otros programas con indicador propio, como mysql o python.{local}"
        ));
    }
}
