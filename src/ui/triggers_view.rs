//! Ventana de disparadores: reglas que reaccionan a lo que aparece en los
//! terminales, y el aviso de fin de orden larga.

use uuid::Uuid;

use crate::i18n::tr;
use crate::model::Tree;
use crate::theme::{error_color, ok_color, warn_color};
use crate::trf;
use crate::triggers::{
    Line, LongCommand, MAX_COOLDOWN, MAX_PATTERN_CHARS, MAX_RULES, MAX_SEND_CHARS,
    MIN_SEND_COOLDOWN, Pattern, Scope, Trigger, TriggerSettings,
};

pub struct TriggersWindow {
    /// Regla guardada que se está editando; `None` es una nueva.
    selected: Option<Uuid>,
    draft: Trigger,
    /// Se ha marcado «Enviar un texto» y falta confirmarlo.
    confirming_send: bool,
    /// Línea con la que probar el texto de la regla.
    sample: String,
    message: Option<(String, bool)>,
}

/// Texto del ámbito de una regla.
fn scope_label(scope: &Scope, tree: &Tree) -> String {
    match scope {
        Scope::All => tr("Todos los terminales").to_string(),
        Scope::Folder(id) => tree
            .folder_paths()
            .into_iter()
            .find(|(folder, _)| folder == id)
            .map_or(tr("(carpeta eliminada)").to_string(), |(_, path)| {
                format!("📁 {path}")
            }),
        Scope::Session(id) => tree
            .session_paths()
            .into_iter()
            .find(|(session, _)| session == id)
            .map_or(tr("(equipo eliminado)").to_string(), |(_, path)| path),
    }
}

impl TriggersWindow {
    pub fn new(settings: &TriggerSettings) -> Self {
        let mut window = Self {
            selected: None,
            draft: Trigger::default(),
            confirming_send: false,
            sample: String::new(),
            message: None,
        };
        if let Some(first) = settings.rules.first() {
            window.load(first);
        }
        window
    }

    fn load(&mut self, rule: &Trigger) {
        self.selected = Some(rule.id);
        self.draft = rule.clone();
        self.confirming_send = false;
        self.message = None;
    }

    fn blank(&mut self) {
        self.selected = None;
        self.draft = Trigger::default();
        self.confirming_send = false;
        self.message = None;
    }

    /// Guarda la regla en edición. Devuelve `true` si se guardó.
    fn save(&mut self, settings: &mut TriggerSettings) -> bool {
        if let Err(problem) = self.draft.validate() {
            self.message = Some((problem, true));
            return false;
        }
        self.draft.name = self.draft.name.trim().to_string();
        self.draft.pattern = self.draft.pattern.trim().to_string();
        match settings.rules.iter().position(|r| r.id == self.draft.id) {
            Some(index) => {
                // Si está activa se decide en la lista, no aquí.
                self.draft.enabled = settings.rules[index].enabled;
                settings.rules[index] = self.draft.clone();
            }
            None if settings.rules.len() >= MAX_RULES => {
                self.message = Some((trf!("No caben más de {max} reglas", max = MAX_RULES), true));
                return false;
            }
            None => settings.rules.push(self.draft.clone()),
        }
        self.selected = Some(self.draft.id);
        self.message = Some((tr("Regla guardada").into(), false));
        true
    }

    /// Devuelve si la ventana sigue abierta y si han cambiado los ajustes.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        settings: &mut TriggerSettings,
        tree: &Tree,
    ) -> (bool, bool) {
        let mut open = true;
        let before = settings.clone();
        egui::Window::new(tr("Disparadores"))
            .id(egui::Id::unique("triggers_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos([240.0, 60.0])
            .show(ctx, |ui| {
                ui.set_width(800.0);
                ui.weak(tr(
                    "Reglas que reaccionan cuando en la salida de un terminal aparece un texto: \
                     avisan y marcan la pestaña o, si lo pides expresamente, contestan por ti.",
                ));
                ui.add_space(4.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(800.0, 390.0),
                    egui::Layout::left_to_right(egui::Align::TOP),
                    |ui| {
                        ui.vertical(|ui| {
                            ui.set_width(290.0);
                            self.rule_list(ui, settings);
                        });
                        ui.separator();
                        ui.vertical(|ui| {
                            ui.set_width(490.0);
                            self.editor(ui, settings, tree);
                        });
                    },
                );
                ui.separator();
                long_command_section(ui, &mut settings.long_command);
            });
        (open, *settings != before)
    }

    fn rule_list(&mut self, ui: &mut egui::Ui, settings: &mut TriggerSettings) {
        ui.strong(trf!("Reglas ({n})", n = settings.rules.len()));
        let mut pick = None;
        egui::ScrollArea::vertical()
            .id_salt("trigger_list")
            .max_height(330.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if settings.rules.is_empty() {
                    ui.weak(tr("Aún no hay reglas."));
                }
                for rule in &mut settings.rules {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut rule.enabled, "")
                            .on_hover_text(tr("Regla activa"));
                        let mut name = egui::RichText::new(rule.display_name());
                        if !rule.enabled {
                            name = name.weak();
                        }
                        let label = ui
                            .add(
                                egui::Button::selectable(self.selected == Some(rule.id), name)
                                    .truncate(),
                            )
                            .on_hover_text(trf!(
                                "Cuando aparezca «{texto}»",
                                texto = rule.pattern.trim()
                            ));
                        if label.clicked() {
                            pick = Some(rule.clone());
                        }
                    });
                    // Qué hace, bien a la vista: sobre todo si escribe.
                    ui.horizontal(|ui| {
                        ui.add_space(26.0);
                        if rule.sends() {
                            ui.label(
                                egui::RichText::new(tr("⚠ ENVÍA TEXTO"))
                                    .strong()
                                    .color(warn_color()),
                            )
                            .on_hover_text(trf!(
                                "Escribe en el terminal sin preguntar:\n{texto}",
                                texto = rule.send.as_deref().unwrap_or_default()
                            ));
                        }
                        let mut what = vec![];
                        if rule.notify {
                            what.push(if rule.bell {
                                tr("avisa 🔔")
                            } else {
                                tr("avisa")
                            });
                        }
                        if !matches!(rule.scope, Scope::All) {
                            what.push(tr("ámbito limitado"));
                        }
                        ui.weak(what.join(" · "));
                    });
                    ui.add_space(2.0);
                }
            });
        if let Some(rule) = pick {
            self.load(&rule);
        }
        if ui
            .add_enabled(
                settings.rules.len() < MAX_RULES,
                egui::Button::new(tr("➕ Nueva regla")),
            )
            .clicked()
        {
            self.blank();
        }
    }

    fn editor(&mut self, ui: &mut egui::Ui, settings: &mut TriggerSettings, tree: &Tree) {
        let draft = &mut self.draft;
        egui::Grid::new("trigger_editor")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label(tr("Nombre"));
                ui.add(
                    egui::TextEdit::singleline(&mut draft.name)
                        .desired_width(380.0)
                        .hint_text(tr("opcional")),
                );
                ui.end_row();

                ui.label(tr("Cuando aparezca"));
                ui.add(
                    egui::TextEdit::singleline(&mut draft.pattern)
                        .font(egui::TextStyle::Monospace)
                        .char_limit(MAX_PATTERN_CHARS)
                        .desired_width(380.0)
                        .hint_text(tr("texto que dispara la regla")),
                );
                ui.end_row();

                ui.label("");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut draft.wildcards, tr("Comodines"))
                        .on_hover_text(tr(
                            "* vale por cualquier texto y ? por un carácter cualquiera; \\* y \
                             \\? son los signos tal cual",
                        ));
                    ui.checkbox(&mut draft.case_sensitive, tr("Distinguir mayúsculas"));
                });
                ui.end_row();

                ui.label(tr("En"));
                egui::ComboBox::from_id_salt("trigger_scope")
                    .selected_text(scope_label(&draft.scope, tree))
                    .width(380.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut draft.scope,
                            Scope::All,
                            tr("Todos los terminales"),
                        );
                        for (id, path) in tree.folder_paths() {
                            ui.selectable_value(
                                &mut draft.scope,
                                Scope::Folder(id),
                                format!("📁 {path}"),
                            );
                        }
                        for (id, path) in tree.session_paths() {
                            ui.selectable_value(&mut draft.scope, Scope::Session(id), path);
                        }
                    });
                ui.end_row();
            });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut draft.notify, tr("Avisar"))
                .on_hover_text(tr(
                    "Un aviso en la barra de estado y una marca en la pestaña",
                ));
            ui.add_enabled(
                draft.notify,
                egui::Checkbox::new(&mut draft.bell, tr("con campana 🔔")),
            )
            .on_hover_text(tr(
                "La pestaña lleva la campana y, si GodRemote no está en primer plano, pide \
                 atención en la barra de tareas",
            ));
        });
        // Enviar texto: hay que pedirlo y confirmarlo.
        let mut sends = draft.send.is_some();
        let asked = ui
            .checkbox(
                &mut sends,
                egui::RichText::new(tr("⚠ Enviar un texto al terminal")).color(warn_color()),
            )
            .on_hover_text(tr("La regla escribe en el terminal sin preguntar"))
            .changed();
        if asked {
            if sends {
                self.confirming_send = true;
            } else {
                draft.send = None;
                self.confirming_send = false;
            }
        }
        if self.confirming_send {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.colored_label(
                    warn_color(),
                    tr(
                        "Esta regla escribirá en el terminal por su cuenta cada vez que el texto \
                     aparezca en la línea donde espera el cursor.",
                    ),
                );
                ui.label(tr(
                    "Lo recibirá cualquier servidor o programa que escriba ese texto, no solo \
                     el que tienes en mente. No pongas contraseñas: se guardan sin cifrar con \
                     los ajustes.",
                ));
                ui.horizontal(|ui| {
                    if ui.button(tr("Sí, que envíe texto")).clicked() {
                        draft.send = Some(String::new());
                        draft.cooldown_secs = draft.cooldown_secs.max(MIN_SEND_COOLDOWN);
                        self.confirming_send = false;
                    }
                    if ui.button(tr("No")).clicked() {
                        self.confirming_send = false;
                    }
                });
            });
        }
        if let Some(text) = &mut draft.send {
            ui.add(
                egui::TextEdit::multiline(text)
                    .font(egui::TextStyle::Monospace)
                    .char_limit(MAX_SEND_CHARS)
                    .desired_rows(2)
                    .desired_width(f32::INFINITY)
                    .hint_text(tr("Respuesta; cada salto de línea es Intro")),
            );
            ui.weak(tr(
                "Como en los snippets: un salto de línea es Intro; \\e, \\t y \\x03 son teclas \
                 especiales. Solo se envía si el texto está en la línea donde espera el cursor \
                 y lleva un momento quieta; nunca a lo que pasa de largo en un listado ni a una \
                 aplicación a pantalla completa.",
            ));
        }
        ui.horizontal(|ui| {
            ui.label(tr("No volver a dispararse en"));
            let min = if draft.send.is_some() {
                MIN_SEND_COOLDOWN
            } else {
                0
            };
            ui.add(
                egui::DragValue::new(&mut draft.cooldown_secs)
                    .range(min..=MAX_COOLDOWN)
                    .suffix(" s"),
            )
            .on_hover_text(tr("Enfriamiento: evita que la regla se dispare en bucle"));
            ui.weak(tr("en el mismo terminal"));
        });
        ui.horizontal(|ui| {
            ui.label(tr("Probar con"));
            ui.add(
                egui::TextEdit::singleline(&mut self.sample)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(280.0)
                    .hint_text(tr("una línea de ejemplo")),
            );
            if !self.sample.is_empty() {
                match Pattern::new(&draft.pattern, draft.wildcards, draft.case_sensitive) {
                    Some(pattern) if pattern.is_match(&Line::new(&self.sample)) => {
                        ui.colored_label(ok_color(), tr("✔ se dispara"))
                    }
                    Some(_) => ui.weak(tr("no se dispara")),
                    None => ui.weak(tr("falta el texto")),
                };
            }
        });
        ui.add_space(4.0);
        let mut save = false;
        let mut remove = false;
        ui.horizontal(|ui| {
            save = ui
                .add_enabled(!self.confirming_send, egui::Button::new(tr("Guardar")))
                .clicked();
            remove = ui
                .add_enabled(self.selected.is_some(), egui::Button::new(tr("🗑 Eliminar")))
                .clicked();
            if let Some((message, is_error)) = &self.message {
                let color = if *is_error { error_color() } else { ok_color() };
                ui.colored_label(color, message);
            }
        });
        if save {
            self.save(settings);
        }
        if remove && let Some(id) = self.selected {
            settings.rules.retain(|rule| rule.id != id);
            match settings.rules.first().cloned() {
                Some(first) => self.load(&first),
                None => self.blank(),
            }
            self.message = Some((tr("Regla eliminada").into(), false));
        }
    }
}

/// El aviso incorporado de fin de orden larga.
fn long_command_section(ui: &mut egui::Ui, long: &mut LongCommand) {
    ui.strong(tr("Órdenes largas"));
    ui.horizontal(|ui| {
        ui.checkbox(
            &mut long.enabled,
            tr("Avisar cuando termine una orden que dure más de"),
        );
        ui.add_enabled(
            long.enabled,
            egui::DragValue::new(&mut long.seconds)
                .range(LongCommand::MIN_SECONDS..=LongCommand::MAX_SECONDS)
                .suffix(" s"),
        );
        ui.label(tr("en una pestaña que no estoy mirando"));
        ui.add_enabled(
            long.enabled,
            egui::Checkbox::new(&mut long.bell, tr("con campana 🔔")),
        );
    });
    ui.weak(tr(
        "Si la shell marca sus órdenes (integración de shell, OSC 133: fish, PowerShell en \
         Windows Terminal, o bash y zsh con la integración de kitty, WezTerm, iTerm2…) el aviso \
         es exacto y dice si terminó con error. Si no, se deduce: se pulsó Intro, hubo salida \
         durante ese tiempo y después silencio con el cursor en algo que parece un indicador \
         o una pregunta.",
    ));
}
