//! Editor de una carpeta: su nombre y los valores que da a los equipos que
//! contiene (usuario, autenticación, saltos, fondo del terminal…).

use uuid::Uuid;

use super::backdrop_view::BackdropEditor;
use crate::backdrop::Backdrop;
use crate::i18n::tr;
use crate::keys::KeyInfo;
use crate::model::{AuthMethod, Folder, FolderDefaults, Node, Tree};
use crate::trf;
use crate::vault::Vault;

const FIELD_WIDTH: f32 = 320.0;

pub struct FolderEditor {
    /// Carpeta que se edita; `None` si es nueva.
    id: Option<Uuid>,
    parent: Option<Uuid>,
    name: String,
    username: String,
    define_auth: bool,
    auth: AuthMethod,
    key_path: String,
    password: String,
    passphrase: String,
    define_jumps: bool,
    jumps: Vec<Uuid>,
    hide_bar: bool,
    color: Option<crate::theme::Rgb>,
    define_backdrop: bool,
    backdrop: Backdrop,
    backdrop_ui: BackdropEditor,
    keys: Vec<KeyInfo>,
    error: Option<String>,
    focused: bool,
}

pub enum FolderResult {
    Open,
    Cancelled,
    /// Guardada: identificador de la carpeta.
    Saved(Uuid),
}

impl FolderEditor {
    pub fn new(parent: Option<Uuid>) -> Self {
        Self::build(None, parent, String::new(), &FolderDefaults::default())
    }

    pub fn edit(folder: &Folder, parent: Option<Uuid>) -> Self {
        Self::build(
            Some(folder.id),
            parent,
            folder.name.clone(),
            &folder.defaults,
        )
    }

    fn build(id: Option<Uuid>, parent: Option<Uuid>, name: String, d: &FolderDefaults) -> Self {
        Self {
            id,
            parent,
            name,
            username: d.username.clone(),
            define_auth: d.auth.is_some(),
            auth: d.auth.unwrap_or(AuthMethod::Key),
            key_path: d.key_path.clone(),
            password: String::new(),
            passphrase: String::new(),
            define_jumps: d.jumps.is_some(),
            jumps: d.jumps.clone().unwrap_or_default(),
            hide_bar: d.server_info == Some(false),
            color: d.color,
            define_backdrop: d.backdrop.is_some(),
            backdrop: d.backdrop.clone().unwrap_or_default(),
            backdrop_ui: BackdropEditor::default(),
            keys: crate::keys::list(),
            error: None,
            focused: false,
        }
    }

    /// Valores que quedan definidos con lo que hay en el formulario.
    fn defaults(&self) -> FolderDefaults {
        let mut backdrop = self.backdrop.clone();
        backdrop.sanitize();
        FolderDefaults {
            username: self.username.trim().to_string(),
            auth: self.define_auth.then_some(self.auth),
            key_path: if self.define_auth && self.auth == AuthMethod::Key {
                self.key_path.trim().to_string()
            } else {
                String::new()
            },
            jumps: self.define_jumps.then(|| self.jumps.clone()),
            backdrop: self.define_backdrop.then_some(backdrop),
            server_info: self.hide_bar.then_some(false),
            color: self.color,
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        tree: &mut Tree,
        vault: &mut Option<Vault>,
    ) -> FolderResult {
        let mut result = FolderResult::Open;
        let mut open = true;
        let title = if self.id.is_some() {
            tr("Editar carpeta")
        } else {
            tr("Nueva carpeta")
        };
        let max_height = (ctx.content_rect().height() - 150.0).max(240.0);
        egui::Window::new(title)
            .id(egui::Id::unique("folder_editor"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_width(470.0);
                egui::ScrollArea::vertical()
                    .max_height(max_height)
                    .show(ui, |ui| self.form(ui, tree, vault.as_ref()));
                ui.add_space(6.0);
                if let Some(error) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.separator();
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter))
                    && ui.memory(|m| m.focused().is_none());
                ui.horizontal(|ui| {
                    if ui.button(tr("Guardar")).clicked() || enter {
                        match self.commit(tree, vault) {
                            Ok(id) => result = FolderResult::Saved(id),
                            Err(e) => self.error = Some(e),
                        }
                    }
                    if ui.button(tr("Cancelar")).clicked() {
                        result = FolderResult::Cancelled;
                    }
                });
            });
        if !open {
            result = FolderResult::Cancelled;
        }
        result
    }

    fn form(&mut self, ui: &mut egui::Ui, tree: &Tree, vault: Option<&Vault>) {
        egui::Grid::new("folder_grid")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label(tr("Nombre"));
                let name =
                    ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(FIELD_WIDTH));
                if !self.focused {
                    name.request_focus();
                    self.focused = true;
                }
                ui.end_row();
            });
        ui.add_space(4.0);
        let defined = !self.defaults().is_empty();
        egui::CollapsingHeader::new(tr("Valores para los equipos de esta carpeta"))
            .id_salt("folder_defaults")
            .default_open(defined)
            .show(ui, |ui| {
                ui.weak(tr(
                    "Lo que definas aquí lo usan los equipos de la carpeta y de sus \
                     subcarpetas. El usuario y el fondo se aplican a los que no tengan el \
                     suyo; la autenticación y los saltos, a los que marquen que los heredan \
                     (los equipos nuevos lo marcan solos).",
                ));
                ui.add_space(4.0);
                egui::Grid::new("folder_defaults_grid")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label(tr("Usuario"));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.username)
                                .hint_text(tr("sin definir"))
                                .desired_width(FIELD_WIDTH),
                        );
                        ui.end_row();
                    });
                self.auth_ui(ui, vault);
                ui.checkbox(
                    &mut self.define_jumps,
                    tr("Definir los saltos (bastiones SSH)"),
                );
                if self.define_jumps {
                    ui.indent("folder_jumps", |ui| {
                        if self.jumps.is_empty() {
                            ui.weak(tr("Sin saltos: conexión directa."));
                        }
                        super::editor::jump_list_ui(ui, tree, &mut self.jumps, None, "folder");
                    });
                }
                ui.checkbox(
                    &mut self.hide_bar,
                    tr("Sin barra de información del servidor en estos equipos"),
                );
                super::util::tab_color_ui(
                    ui,
                    tr("Marcar con un color las pestañas de estos equipos"),
                    &mut self.color,
                );
                ui.checkbox(
                    &mut self.define_backdrop,
                    tr("Fondo del terminal para estos equipos"),
                )
                .on_hover_text(tr("Lo usan los equipos que no tengan un fondo propio"));
                if self.define_backdrop {
                    ui.indent("folder_backdrop", |ui| {
                        ui.set_max_width(430.0);
                        self.backdrop_ui.show(ui, &mut self.backdrop, "folder");
                    });
                }
            });
    }

    fn auth_ui(&mut self, ui: &mut egui::Ui, vault: Option<&Vault>) {
        ui.checkbox(&mut self.define_auth, tr("Definir la autenticación SSH"));
        if !self.define_auth {
            return;
        }
        ui.indent("folder_auth", |ui| {
            ui.horizontal(|ui| {
                for method in AuthMethod::ALL {
                    ui.selectable_value(&mut self.auth, method, tr(method.label()));
                }
            });
            let stored = |suffix: &str| {
                self.id
                    .is_some_and(|id| vault.is_some_and(|v| v.contains(&format!("{id}/{suffix}"))))
            };
            match self.auth {
                AuthMethod::Password => {
                    let hint = if stored("password") {
                        tr("guardada en el baúl (escribe para cambiarla)")
                    } else {
                        tr("se pedirá al conectar si se deja vacía")
                    };
                    ui.add(
                        egui::TextEdit::singleline(&mut self.password)
                            .password(true)
                            .hint_text(hint)
                            .desired_width(FIELD_WIDTH),
                    );
                    if vault.is_none() {
                        ui.weak(tr("Baúl bloqueado: la contraseña no se guardará."));
                    }
                }
                AuthMethod::Key => {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.key_path)
                                .hint_text("~/.ssh/id_ed25519")
                                .desired_width(FIELD_WIDTH - 86.0),
                        );
                        egui::ComboBox::from_id_salt("folder_key_picker")
                            .selected_text("…")
                            .width(24.0)
                            .show_ui(ui, |ui| {
                                if self.keys.is_empty() {
                                    ui.weak(tr("No se han encontrado claves"));
                                }
                                for k in &self.keys {
                                    let label = super::editor::key_label(k);
                                    if ui.selectable_label(false, label).clicked() {
                                        self.key_path = k.path.display().to_string();
                                    }
                                }
                            });
                        if ui
                            .button("📂")
                            .on_hover_text(tr("Elegir el fichero de la clave"))
                            .clicked()
                            && let Some(path) = rfd::FileDialog::new()
                                .set_title(tr("Clave privada SSH"))
                                .pick_file()
                        {
                            self.key_path = path.display().to_string();
                        }
                    });
                    let hint = if stored("passphrase") {
                        tr("passphrase guardada en el baúl")
                    } else {
                        tr("passphrase (si la clave está cifrada)")
                    };
                    ui.add(
                        egui::TextEdit::singleline(&mut self.passphrase)
                            .password(true)
                            .hint_text(hint)
                            .desired_width(FIELD_WIDTH),
                    );
                    if vault.is_none() {
                        ui.weak(tr("Baúl bloqueado: la passphrase no se guardará."));
                    }
                }
                AuthMethod::Agent => {
                    ui.weak(tr("Se usarán las claves cargadas en ssh-agent / Pageant."));
                }
            }
        });
    }

    /// Valida y guarda la carpeta (y sus secretos en el baúl).
    fn commit(&mut self, tree: &mut Tree, vault: &mut Option<Vault>) -> Result<Uuid, String> {
        let name = self.name.trim().to_string();
        if name.is_empty() {
            return Err(tr("La carpeta necesita un nombre").into());
        }
        let defaults = self.defaults();
        if defaults.auth == Some(AuthMethod::Key) && defaults.key_path.is_empty() {
            return Err(tr("Indica la ruta de la clave privada").into());
        }
        if let Some(jumps) = &defaults.jumps {
            for id in jumps {
                match tree.session(*id) {
                    Some(hop) if hop.protocol.is_ssh() => {}
                    Some(hop) => {
                        return Err(trf!(
                            "El salto «{name}» no es una sesión SSH",
                            name = hop.display_name()
                        ));
                    }
                    None => return Err(tr("Uno de los saltos ya no existe").into()),
                }
            }
        }
        let id = match self.id {
            Some(id) => id,
            None => tree.add_folder(self.parent, &name),
        };
        let Some(Node::Folder(folder)) = tree.find_mut(id) else {
            return Err(tr("La carpeta ya no existe").into());
        };
        folder.name = name;
        folder.defaults = defaults.clone();
        self.id = Some(id);

        if let Some(vault) = vault {
            let password = format!("{id}/password");
            let passphrase = format!("{id}/passphrase");
            let result = (|| -> anyhow::Result<()> {
                match defaults.auth {
                    Some(AuthMethod::Password) => {
                        if !self.password.is_empty() {
                            vault.set(&password, &self.password)?;
                        }
                        vault.remove(&passphrase)?;
                    }
                    Some(AuthMethod::Key) => {
                        if !self.passphrase.is_empty() {
                            vault.set(&passphrase, &self.passphrase)?;
                        }
                        vault.remove(&password)?;
                    }
                    _ => {
                        vault.remove(&password)?;
                        vault.remove(&passphrase)?;
                    }
                }
                Ok(())
            })();
            result.map_err(|e| {
                trf!(
                    "No se pudo actualizar el baúl: {error}",
                    error = format!("{e:#}")
                )
            })?;
        }
        Ok(id)
    }
}
