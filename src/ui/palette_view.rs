//! Paleta de órdenes: una caja de búsqueda para saltar a un equipo, una
//! pestaña, un snippet, un túnel o una acción sin usar el ratón.

use uuid::Uuid;

/// Acciones de la aplicación que se pueden lanzar desde la paleta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    LocalTerminal,
    NewSession,
    NewFolder,
    Keys,
    Snippets,
    Tunnels,
    Themes,
    Settings,
    Transfer,
    VaultLock,
    VaultUnlock,
    ToggleSplit,
    ToggleBroadcast,
    SearchTerminal,
    CloseTab,
    NextTab,
    PreviousTab,
    XServer,
    RunOnFolder,
    VaultContents,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    SwitchTab(u64),
    Connect(Uuid),
    RunSnippet(Uuid),
    StartTunnel(Uuid),
    StopTunnel(Uuid),
    Command(Command),
}

pub struct PaletteItem {
    /// Tipo de resultado, a la izquierda: «Pestaña», «Equipo»…
    pub kind: &'static str,
    pub label: String,
    /// Texto secundario (destino, atajo…); también se busca en él.
    pub detail: String,
    pub action: PaletteAction,
}

pub enum PaletteResult {
    Open,
    Closed,
    Chosen(PaletteAction),
}

/// Resultados que se muestran como mucho.
const MAX_SHOWN: usize = 80;

#[derive(Default)]
pub struct Palette {
    query: String,
    selected: usize,
    focused: bool,
    /// Hay que desplazar la lista hasta el elegido (se movió con el teclado).
    reveal: bool,
}

impl Palette {
    pub fn show(&mut self, ctx: &egui::Context, items: &[PaletteItem]) -> PaletteResult {
        let texts: Vec<String> = items
            .iter()
            .map(|item| format!("{} {} {}", item.label, item.detail, item.kind))
            .collect();
        let mut matches = crate::palette::rank(&self.query, texts.iter().map(String::as_str));
        matches.truncate(MAX_SHOWN);
        if self.selected >= matches.len() {
            self.selected = matches.len().saturating_sub(1);
        }

        // Las flechas mueven la selección; Intro la lanza.
        let (down, up, enter, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        if down && self.selected + 1 < matches.len() {
            self.selected += 1;
            self.reveal = true;
        }
        if up && self.selected > 0 {
            self.selected -= 1;
            self.reveal = true;
        }
        if escape {
            return PaletteResult::Closed;
        }
        let mut chosen = enter.then(|| matches.get(self.selected).copied()).flatten();

        let modal = egui::Modal::new(egui::Id::unique("command_palette")).show(ctx, |ui| {
            ui.set_width(620.0);
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .desired_width(f32::INFINITY)
                    .hint_text("Equipo, pestaña, snippet, túnel o acción…"),
            );
            if !self.focused {
                edit.request_focus();
                self.focused = true;
            }
            if edit.changed() {
                self.selected = 0;
                self.reveal = true;
            }
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .max_height(380.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    if matches.is_empty() {
                        ui.weak("Nada coincide con lo escrito.");
                    }
                    for (row, index) in matches.iter().enumerate() {
                        let item = &items[*index];
                        let response = ui
                            .horizontal(|ui| {
                                ui.add_sized(
                                    [70.0, 18.0],
                                    egui::Label::new(egui::RichText::new(item.kind).weak().small()),
                                );
                                let label = ui.selectable_label(
                                    row == self.selected,
                                    egui::RichText::new(&item.label),
                                );
                                if !item.detail.is_empty() {
                                    ui.add(
                                        egui::Label::new(egui::RichText::new(&item.detail).weak())
                                            .truncate(),
                                    );
                                }
                                label
                            })
                            .inner;
                        if response.clicked() {
                            chosen = Some(*index);
                        }
                        if row == self.selected && self.reveal {
                            response.scroll_to_me(None);
                        }
                    }
                });
            self.reveal = false;
            ui.add_space(2.0);
            ui.weak("Flechas para moverse · Intro para abrir · Esc para cerrar");
        });
        if let Some(index) = chosen {
            return PaletteResult::Chosen(items[index].action.clone());
        }
        if modal.should_close() {
            return PaletteResult::Closed;
        }
        PaletteResult::Open
    }
}
