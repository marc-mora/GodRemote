//! Ventana general de ajustes.

use crate::theme::Settings;

/// Lo que la ventana pide a la aplicación.
#[derive(Default)]
pub struct SettingsRequest {
    /// Abrir los ajustes de la barra de información del servidor.
    pub open_server_bar: bool,
    /// Abrir esta carpeta en el explorador de archivos.
    pub open_folder: Option<std::path::PathBuf>,
}

#[derive(Default)]
pub struct SettingsWindow;

impl SettingsWindow {
    /// Devuelve (sigue abierta, han cambiado los ajustes, peticiones).
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        settings: &mut Settings,
    ) -> (bool, bool, SettingsRequest) {
        let mut open = true;
        let before = settings.clone();
        let mut request = SettingsRequest::default();
        egui::Window::new("Ajustes")
            .id(egui::Id::unique("settings_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos([280.0, 60.0])
            .show(ctx, |ui| {
                ui.set_width(520.0);
                rdp_section(ui, settings);
                ui.separator();
                ui.strong("Conexión");
                ui.horizontal(|ui| {
                    ui.checkbox(
                        &mut settings.reconnect,
                        "Reconectar los terminales que pierdan la conexión, hasta",
                    );
                    ui.add_enabled(
                        settings.reconnect,
                        egui::DragValue::new(&mut settings.reconnect_attempts)
                            .range(1..=50)
                            .suffix(" intentos"),
                    );
                })
                .response
                .on_hover_text(
                    "Con esperas cada vez más largas. No se reintenta si lo que falla es la \
                     contraseña o la clave, ni si cierras tú la sesión",
                );
                ui.checkbox(
                    &mut settings.restore_tabs,
                    "Al arrancar, reabrir las pestañas que había al cerrar",
                );
                ui.separator();
                log_section(ui, settings, &mut request);
                ui.separator();
                ui.strong("Barra de información del servidor");
                ui.horizontal(|ui| {
                    ui.checkbox(
                        &mut settings.server_bar.enabled,
                        "Mostrarla bajo los terminales SSH",
                    );
                    if ui.button("Elegir qué muestra…").clicked() {
                        request.open_server_bar = true;
                    }
                });
            });
        (open, *settings != before, request)
    }
}

/// Registro de las sesiones de terminal en ficheros.
fn log_section(ui: &mut egui::Ui, settings: &mut Settings, request: &mut SettingsRequest) {
    ui.strong("Registro de sesiones");
    ui.checkbox(
        &mut settings.log.always,
        "Guardar siempre en un fichero lo que muestran los terminales",
    )
    .on_hover_text("Sin marcar, cada terminal se registra solo si pulsas su botón «Registrar»");
    ui.checkbox(
        &mut settings.log.timestamps,
        "Anteponer la hora a cada línea",
    );
    ui.horizontal(|ui| {
        ui.label("Carpeta");
        ui.add(
            egui::TextEdit::singleline(&mut settings.log.folder)
                .desired_width(300.0)
                .hint_text(crate::paths::logs_dir().display().to_string()),
        );
        if ui.button("📂").on_hover_text("Elegir carpeta").clicked()
            && let Some(dir) = rfd::FileDialog::new()
                .set_title("Carpeta de los registros")
                .pick_folder()
        {
            settings.log.folder = dir.display().to_string();
        }
        if ui
            .button("Abrir")
            .on_hover_text("Ver los registros guardados")
            .clicked()
        {
            request.open_folder = Some(settings.log.dir());
        }
    });
    ui.weak(
        "Los registros son texto sin colores ni secuencias de escape. Contienen todo lo que \
         aparece en pantalla: cuidado con las contraseñas que se muestren.",
    );
}

/// Cliente RDP y, ligada a él, la versión de la librería SSH.
fn rdp_section(ui: &mut egui::Ui, settings: &mut Settings) {
    ui.strong("Escritorio remoto (RDP) y librería SSH");
    let edition = if crate::HAS_EMBEDDED_RDP {
        "Edición completa"
    } else {
        "Edición SSH reforzada"
    };
    ui.label(format!(
        "{edition} · librería SSH russh {}",
        crate::RUSSH_VERSION
    ));
    if crate::HAS_EMBEDDED_RDP {
        ui.checkbox(
            &mut settings.rdp_embedded,
            "Usar el cliente RDP integrado (en una pestaña)",
        )
        .on_hover_text(
            "Desmarcado, todas las sesiones RDP se abren con el cliente externo del sistema",
        );
        if !settings.rdp_embedded {
            ui.weak(format!(
                "Las sesiones RDP se abrirán con {}.",
                crate::rdp::client_description()
            ));
        }
        ui.weak(
            "El cliente RDP integrado obliga hoy a compilar con una versión de la librería SSH \
             anterior a la última (le faltan varios endurecimientos frente a servidores \
             maliciosos). Eso se decide al compilar: este interruptor elige qué cliente RDP se \
             usa, pero no cambia la librería. Si prefieres la librería SSH más reciente, usa la \
             edición «SSH reforzada» (GodRemote-ssh-reforzado), que no trae RDP integrado.",
        );
    } else {
        let mut off = false;
        ui.add_enabled(
            false,
            egui::Checkbox::new(&mut off, "Usar el cliente RDP integrado (en una pestaña)"),
        );
        ui.weak(format!(
            "Esta edición lleva la librería SSH más reciente y, a cambio, no incluye el cliente \
             RDP integrado: las sesiones RDP se abren con {}. Si lo necesitas en una pestaña \
             (o RDP a través de saltos SSH), usa la edición completa (GodRemote).",
            crate::rdp::client_description()
        ));
    }
}
