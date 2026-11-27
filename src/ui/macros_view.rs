//! Ventana de gestión de macros de terminal.

use uuid::Uuid;

use crate::macros::{Macro, MacroStore};

pub struct MacrosWindow {
    selected: Option<Uuid>,
    name: String,
    text: String,
    message: Option<(String, bool)>,
}

/// Lo que la ventana pide a la aplicación.
pub enum MacroRequest {
    None,
    /// Ejecutar esta macro en el terminal activo.
    Run(Uuid),
}

impl MacrosWindow {
    pub fn new(store: &MacroStore, select: Option<Uuid>) -> Self {
        let mut window = Self {
            selected: None,
            name: String::new(),
            text: String::new(),
            message: None,
        };
        if let Some(m) = select
            .and_then(|id| store.macros.iter().find(|m| m.id == id))
            .or(store.macros.first())
        {
            window.load(m);
        }
        window
    }

    fn load(&mut self, m: &Macro) {
        self.selected = Some(m.id);
        self.name = m.name.clone();
        self.text = m.text.clone();
    }

    /// Devuelve si la ventana sigue abierta y lo que haya que hacer.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        store: &mut MacroStore,
        can_run: bool,
    ) -> (bool, MacroRequest) {
        let mut open = true;
        let mut request = MacroRequest::None;
        let mut changed = false;
        egui::Window::new("Macros")
            .id(egui::Id::unique("macros_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos([260.0, 70.0])
            .show(ctx, |ui| {
                ui.set_width(660.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(660.0, 340.0),
                    egui::Layout::left_to_right(egui::Align::TOP),
                    |ui| {
                        ui.vertical(|ui| {
                            ui.set_width(200.0);
                            ui.strong("Macros guardadas");
                            let mut pick = None;
                            egui::ScrollArea::vertical()
                                .id_salt("macro_list")
                                .max_height(260.0)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    if store.macros.is_empty() {
                                        ui.weak("Aún no hay macros.");
                                    }
                                    for m in &store.macros {
                                        if ui
                                            .selectable_label(self.selected == Some(m.id), &m.name)
                                            .clicked()
                                        {
                                            pick = Some(m.clone());
                                        }
                                    }
                                });
                            if let Some(m) = pick {
                                self.load(&m);
                                self.message = None;
                            }
                            if ui.button("➕ Nueva macro").clicked() {
                                self.selected = None;
                                self.name = "Nueva macro".into();
                                self.text.clear();
                                self.message = None;
                            }
                        });
                        ui.separator();
                        ui.vertical(|ui| {
                            ui.set_width(430.0);
                            ui.horizontal(|ui| {
                                ui.label("Nombre");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.name).desired_width(340.0),
                                );
                            });
                            ui.add(
                                egui::TextEdit::multiline(&mut self.text)
                                    .font(egui::TextStyle::Monospace)
                                    .desired_rows(10)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("Órdenes a enviar, una por línea"),
                            );
                            ui.weak(
                                "Cada salto de línea es Intro. Teclas especiales: \\e (Escape), \
                             \\t (tabulador), \\x03 (Ctrl+C), \\x04 (Ctrl+D)…",
                            );
                            ui.horizontal(|ui| {
                                let valid = !self.name.trim().is_empty();
                                if ui
                                    .add_enabled(valid, egui::Button::new("Guardar"))
                                    .clicked()
                                {
                                    match self.selected {
                                        Some(id) => {
                                            if let Some(m) =
                                                store.macros.iter_mut().find(|m| m.id == id)
                                            {
                                                m.name = self.name.trim().to_string();
                                                m.text = self.text.clone();
                                            }
                                        }
                                        None => {
                                            self.selected =
                                                Some(store.add(&self.name, self.text.clone()));
                                        }
                                    }
                                    changed = true;
                                    self.message = Some(("Macro guardada".into(), false));
                                }
                                if ui
                                    .add_enabled(
                                        self.selected.is_some() && can_run,
                                        egui::Button::new("▶ Ejecutar"),
                                    )
                                    .on_hover_text("Envía la macro guardada al terminal activo")
                                    .on_disabled_hover_text(
                                        "Guarda la macro y abre un terminal conectado",
                                    )
                                    .clicked()
                                    && let Some(id) = self.selected
                                {
                                    request = MacroRequest::Run(id);
                                }
                                if ui
                                    .add_enabled(
                                        self.selected.is_some(),
                                        egui::Button::new("🗑 Eliminar"),
                                    )
                                    .clicked()
                                    && let Some(id) = self.selected.take()
                                {
                                    store.remove(id);
                                    changed = true;
                                    self.name.clear();
                                    self.text.clear();
                                    if let Some(first) = store.macros.first().cloned() {
                                        self.load(&first);
                                    }
                                    self.message = Some(("Macro eliminada".into(), false));
                                }
                            });
                            if let Some((msg, is_error)) = &self.message {
                                let color = if *is_error {
                                    ui.visuals().error_fg_color
                                } else {
                                    crate::theme::ok_color()
                                };
                                ui.colored_label(color, msg);
                            }
                        });
                    },
                );
            });
        if changed && let Err(e) = store.save(&crate::paths::macros_file()) {
            self.message = Some((format!("No se pudieron guardar las macros: {e:#}"), true));
        }
        (open, request)
    }
}
