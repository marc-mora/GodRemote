//! Ventana de snippets: crear, editar, asignarles un atajo y lanzarlos.

use egui::{Event, Key};
use uuid::Uuid;

use crate::macros::{Macro, MacroStore, Shortcut, is_modifier_key};

pub struct MacrosWindow {
    selected: Option<Uuid>,
    name: String,
    text: String,
    shortcut: Option<Shortcut>,
    /// Esperando a que el usuario pulse la combinación del atajo.
    capturing: bool,
    message: Option<(String, bool)>,
}

/// Lo que la ventana pide a la aplicación.
pub enum MacroRequest {
    None,
    /// Enviar este snippet al terminal activo.
    Run(Uuid),
}

impl MacrosWindow {
    pub fn new(store: &MacroStore, select: Option<Uuid>) -> Self {
        let mut window = Self::blank();
        if let Some(m) = select
            .and_then(|id| store.macros.iter().find(|m| m.id == id))
            .or(store.macros.first())
        {
            window.load(m);
        }
        window
    }

    /// Ventana lista para escribir un snippet nuevo.
    pub fn new_snippet() -> Self {
        let mut window = Self::blank();
        window.name = "Nuevo snippet".into();
        window
    }

    fn blank() -> Self {
        Self {
            selected: None,
            name: String::new(),
            text: String::new(),
            shortcut: None,
            capturing: false,
            message: None,
        }
    }

    fn load(&mut self, m: &Macro) {
        self.selected = Some(m.id);
        self.name = m.name.clone();
        self.text = m.text.clone();
        self.shortcut = m.shortcut;
        self.capturing = false;
    }

    /// Mientras se espera el atajo, la siguiente pulsación es para él y no
    /// para los campos de texto.
    fn capture(&mut self, ctx: &egui::Context) {
        enum Pressed {
            Key(Key, egui::Modifiers),
            /// Ctrl+C, Ctrl+V o Ctrl+X: el sistema los entrega ya convertidos.
            Clipboard,
        }
        let pressed = ctx.input_mut(|i| {
            let mut pressed = None;
            i.events.retain(|event| match event {
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    // Ctrl, Alt o Mayús sueltos: se sigue esperando la tecla.
                    if !is_modifier_key(*key) {
                        pressed.get_or_insert(Pressed::Key(*key, *modifiers));
                    }
                    false
                }
                Event::Copy | Event::Cut | Event::Paste(_) => {
                    pressed.get_or_insert(Pressed::Clipboard);
                    false
                }
                Event::Text(_) => false,
                _ => true,
            });
            pressed
        });
        match pressed {
            None => {}
            Some(Pressed::Key(Key::Escape, modifiers)) if modifiers.is_none() => {
                self.capturing = false;
                self.message = None;
            }
            Some(Pressed::Clipboard) => {
                self.message = Some((
                    "Ctrl+C, Ctrl+V y Ctrl+X están reservados para copiar, pegar y cortar".into(),
                    true,
                ));
            }
            Some(Pressed::Key(key, modifiers)) => {
                let shortcut = Shortcut::new(key, modifiers);
                match shortcut.validate() {
                    Ok(()) => {
                        self.shortcut = Some(shortcut);
                        self.capturing = false;
                        self.message = None;
                    }
                    Err(e) => self.message = Some((e, true)),
                }
            }
        }
    }

    /// Guarda el snippet en edición. Devuelve `true` si se guardó.
    fn save(&mut self, store: &mut MacroStore) -> bool {
        if let Some(shortcut) = &self.shortcut
            && let Some(other) = store.shortcut_owner(shortcut, self.selected)
        {
            self.message = Some((
                format!("El atajo {shortcut} ya lo usa «{}»", other.name),
                true,
            ));
            return false;
        }
        let id = match self.selected {
            Some(id) => id,
            None => store.add(&self.name, String::new()),
        };
        if let Some(m) = store.macros.iter_mut().find(|m| m.id == id) {
            m.name = self.name.trim().to_string();
            m.text = self.text.clone();
            m.shortcut = self.shortcut;
        }
        self.selected = Some(id);
        self.message = Some(("Snippet guardado".into(), false));
        true
    }

    /// Devuelve si la ventana sigue abierta y lo que haya que hacer.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        store: &mut MacroStore,
        can_run: bool,
    ) -> (bool, MacroRequest) {
        if self.capturing {
            self.capture(ctx);
        }
        let mut open = true;
        let mut request = MacroRequest::None;
        let mut changed = false;
        egui::Window::new("Snippets")
            .id(egui::Id::unique("macros_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos([260.0, 70.0])
            .show(ctx, |ui| {
                ui.set_width(700.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(700.0, 420.0),
                    egui::Layout::left_to_right(egui::Align::TOP),
                    |ui| {
                        ui.vertical(|ui| {
                            ui.set_width(230.0);
                            ui.strong("Snippets guardados");
                            let mut pick = None;
                            egui::ScrollArea::vertical()
                                .id_salt("macro_list")
                                .max_height(340.0)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    if store.macros.is_empty() {
                                        ui.weak("Aún no hay snippets.");
                                    }
                                    for m in &store.macros {
                                        let mut button = egui::Button::selectable(
                                            self.selected == Some(m.id),
                                            m.name.as_str(),
                                        );
                                        if let Some(shortcut) = &m.shortcut {
                                            button = button.shortcut_text(shortcut.to_string());
                                        }
                                        if ui.add_sized([222.0, 0.0], button).clicked() {
                                            pick = Some(m.clone());
                                        }
                                    }
                                });
                            if let Some(m) = pick {
                                self.load(&m);
                                self.message = None;
                            }
                            if ui.button("➕ Nuevo snippet").clicked() {
                                *self = Self::new_snippet();
                            }
                        });
                        ui.separator();
                        ui.vertical(|ui| {
                            ui.set_width(440.0);
                            ui.horizontal(|ui| {
                                ui.label("Nombre");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.name).desired_width(350.0),
                                );
                            });
                            ui.add(
                                egui::TextEdit::multiline(&mut self.text)
                                    .font(egui::TextStyle::Monospace)
                                    .desired_rows(9)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("Texto u órdenes a enviar, una por línea"),
                            );
                            ui.weak(
                                "Cada salto de línea es Intro: sin salto al final, el texto se \
                                 escribe y queda a la espera. Teclas especiales: \\e (Escape), \
                                 \\t (tabulador), \\x03 (Ctrl+C), \\x04 (Ctrl+D)…",
                            );
                            ui.weak(
                                "Variables: {{host}}, {{usuario}}, {{puerto}}, {{nombre}}, \
                                 {{fecha}}, {{hora}}. Con {{?Pregunta}} o {{?Pregunta=valor}} se \
                                 pide un dato al lanzarlo.",
                            );
                            ui.add_space(4.0);
                            self.shortcut_row(ui);
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                let valid = !self.name.trim().is_empty();
                                if ui
                                    .add_enabled(valid, egui::Button::new("Guardar"))
                                    .clicked()
                                {
                                    changed |= self.save(store);
                                }
                                if ui
                                    .add_enabled(
                                        self.selected.is_some() && can_run,
                                        egui::Button::new("▶ Lanzar"),
                                    )
                                    .on_hover_text("Envía el snippet guardado al terminal activo")
                                    .on_disabled_hover_text(
                                        "Guarda el snippet y abre un terminal conectado",
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
                                    *self = Self::blank();
                                    if let Some(first) = store.macros.first().cloned() {
                                        self.load(&first);
                                    }
                                    self.message = Some(("Snippet eliminado".into(), false));
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
            self.message = Some((format!("No se pudieron guardar los snippets: {e:#}"), true));
        }
        (open, request)
    }

    /// Fila del atajo de teclado: el actual y los botones para cambiarlo.
    fn shortcut_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Atajo");
            if self.capturing {
                ui.colored_label(
                    crate::theme::warn_color(),
                    "Pulsa la combinación de teclas… (Esc para cancelar)",
                );
                if ui.button("Cancelar").clicked() {
                    self.capturing = false;
                }
                return;
            }
            match &self.shortcut {
                Some(shortcut) => {
                    ui.label(
                        egui::RichText::new(shortcut.to_string())
                            .monospace()
                            .strong(),
                    );
                }
                None => {
                    ui.weak("sin atajo");
                }
            }
            if ui
                .button(if self.shortcut.is_some() {
                    "Cambiar…"
                } else {
                    "Asignar…"
                })
                .on_hover_text(
                    "Elige la combinación que lanzará este snippet en el terminal activo: con \
                     Ctrl o Alt, o una tecla de función (F1–F12)",
                )
                .clicked()
            {
                self.capturing = true;
                self.message = None;
                // El foco deja los campos de texto: la combinación no debe
                // escribirse en ellos.
                ui.memory_mut(|m| m.request_focus(egui::Id::NULL));
            }
            if self.shortcut.is_some() && ui.button("Quitar").clicked() {
                self.shortcut = None;
            }
        });
        ui.weak(
            "El atajo funciona con el foco en un terminal. Ctrl+letra suele tener ya un uso en \
             la shell; Ctrl+Alt+letra, Ctrl+Mayús+letra y F1–F12 no chocan con nada.",
        );
    }
}
