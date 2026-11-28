//! Ventana general de ajustes.

use crate::highlight::{Highlight, HlColor, MAX_RULES, Rule};
use crate::theme::{Rgb, Settings};

/// Lo que la ventana pide a la aplicación.
#[derive(Default)]
pub struct SettingsRequest {
    /// Abrir los ajustes de la barra de información del servidor.
    pub open_server_bar: bool,
    /// Abrir esta carpeta en el explorador de archivos.
    pub open_folder: Option<std::path::PathBuf>,
    /// Abrir la ventana de importación y copias de seguridad.
    pub open_transfer: bool,
    /// Arrancar el servidor X integrado y mostrar su pestaña.
    pub open_x_server: bool,
    /// Guardar en el baúl esta contraseña del proxy (vacía: borrarla).
    pub proxy_password: Option<String>,
}

/// Lo que la ventana necesita saber del baúl para la contraseña del proxy.
#[derive(Clone, Copy)]
pub struct ProxyVault {
    /// El baúl está abierto.
    pub open: bool,
    /// Ya hay una contraseña del proxy guardada.
    pub stored: bool,
}

#[derive(Default)]
pub struct SettingsWindow {
    /// Contraseña del proxy que se está escribiendo.
    proxy_password: String,
}

impl SettingsWindow {
    /// Devuelve (sigue abierta, han cambiado los ajustes, peticiones).
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        settings: &mut Settings,
        vault: ProxyVault,
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
                ui.set_width(540.0);
                let max_height = (ctx.content_rect().height() - 140.0).max(240.0);
                egui::ScrollArea::vertical()
                    .max_height(max_height)
                    .min_scrolled_height(max_height)
                    .show(ui, |ui| {
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
                        ui.checkbox(
                            &mut settings.activity_notice,
                            "Destacar las pestañas de terminal con salida nueva sin ver",
                        )
                        .on_hover_text(
                            "La pestaña se resalta cuando su terminal recibe algo mientras miras \
                     otra, y lleva una campana si la ha hecho sonar",
                        );
                        super::auth_extras::agent_choice_ui(ui, &mut settings.agent);
                        ui.separator();
                        proxy_section(ui, settings, vault, &mut self.proxy_password, &mut request);
                        ui.separator();
                        local_shell_section(ui, settings);
                        ui.separator();
                        ui.strong("Teclado");
                        ui.checkbox(
                            &mut settings.app_shortcuts,
                            "Atajos de la aplicación (paleta de órdenes y pestañas)",
                        );
                        ui.weak(
                            "Ctrl+Mayús+P abre la paleta de órdenes · Ctrl+Tab y Ctrl+Mayús+Tab \
                             (o Ctrl+AvPág y Ctrl+RePág) cambian de pestaña · Ctrl+Mayús+T abre \
                             un terminal local · Ctrl+Mayús+W cierra la pestaña · Ctrl+Mayús+F \
                             busca en el terminal.",
                        );
                        ui.separator();
                        ui.strong("Protecciones");
                        ui.checkbox(
                            &mut settings.confirm_paste,
                            "Preguntar antes de pegar varias líneas en un terminal",
                        )
                        .on_hover_text(
                            "En una shell cada línea se ejecuta según llega: el aviso enseña \
                             lo que hay en el portapapeles antes de enviarlo",
                        );
                        ui.checkbox(
                            &mut settings.confirm_close,
                            "Preguntar antes de cerrar pestañas con sesiones conectadas",
                        )
                        .on_hover_text(
                            "También al salir de GodRemote con sesiones conectadas o túneles \
                             en marcha",
                        );
                        ui.horizontal(|ui| {
                            let mut auto = settings.vault_autolock > 0;
                            if ui
                                .checkbox(&mut auto, "Bloquear el baúl tras")
                                .on_hover_text(
                                    "Sin teclado ni ratón en GodRemote durante ese tiempo, el \
                                     baúl se bloquea. Las sesiones abiertas siguen; para \
                                     conectar o reconectar con una contraseña guardada habrá \
                                     que desbloquearlo",
                                )
                                .changed()
                            {
                                settings.vault_autolock = if auto { 15 } else { 0 };
                            }
                            ui.add_enabled(
                                auto,
                                egui::DragValue::new(&mut settings.vault_autolock)
                                    .range(1..=1440)
                                    .suffix(" min sin actividad"),
                            );
                        });
                        ui.separator();
                        ui.strong("SFTP");
                        ui.horizontal(|ui| {
                            ui.label("Editor de los ficheros remotos");
                            ui.add(
                        egui::TextEdit::singleline(&mut settings.editor)
                            .desired_width(250.0)
                            .hint_text("el del sistema"),
                    )
                    .on_hover_text(
                        "Programa con el que se abren los ficheros remotos al pulsar «Editar» \
                         en SFTP: por ejemplo «notepad», «code -w» o la ruta de un programa. \
                         Vacío, se usa el que el sistema tenga asociado a cada tipo de fichero",
                    );
                        });
                        ui.separator();
                        log_section(ui, settings, &mut request);
                        ui.separator();
                        ui.strong("Importar y copias de seguridad");
                        if ui
                    .button("Importar equipos o hacer una copia…")
                    .on_hover_text(
                        "Importar de OpenSSH, PuTTY o MobaXterm, y guardar o restaurar toda la \
                         configuración en un solo fichero",
                    )
                    .clicked()
                {
                    request.open_transfer = true;
                }
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
                        ui.separator();
                        x_server_section(ui, settings, &mut request);
                        ui.separator();
                        highlight_section(ui, &mut settings.highlight);
                    });
            });
        (open, *settings != before, request)
    }
}

/// Proxy por el que salen las conexiones.
fn proxy_section(
    ui: &mut egui::Ui,
    settings: &mut Settings,
    vault: ProxyVault,
    password: &mut String,
    request: &mut SettingsRequest,
) {
    use crate::proxy::ProxyKind;
    ui.strong("Proxy de salida");
    let proxy = &mut settings.proxy;
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("proxy_kind")
            .selected_text(proxy.kind.label())
            .width(130.0)
            .show_ui(ui, |ui| {
                for kind in ProxyKind::ALL {
                    ui.selectable_value(&mut proxy.kind, kind, kind.label());
                }
            });
        ui.add_enabled_ui(proxy.kind != ProxyKind::None, |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut proxy.host)
                    .desired_width(200.0)
                    .hint_text("servidor del proxy"),
            );
            ui.add(egui::DragValue::new(&mut proxy.port).range(1..=65535));
        });
    });
    if proxy.kind != ProxyKind::None {
        ui.horizontal(|ui| {
            ui.label("Usuario");
            ui.add(
                egui::TextEdit::singleline(&mut proxy.username)
                    .desired_width(120.0)
                    .hint_text("opcional"),
            );
            ui.label("Contraseña");
            ui.add_enabled(
                vault.open,
                egui::TextEdit::singleline(password)
                    .password(true)
                    .desired_width(120.0)
                    .hint_text(if vault.stored {
                        "guardada"
                    } else {
                        "sin guardar"
                    }),
            )
            .on_disabled_hover_text("Se guarda en el baúl: desbloquéalo para cambiarla");
            if ui
                .add_enabled(
                    vault.open && !password.is_empty(),
                    egui::Button::new("Guardar"),
                )
                .clicked()
            {
                request.proxy_password = Some(std::mem::take(password));
            }
            if vault.open && vault.stored && ui.button("Quitar").clicked() {
                request.proxy_password = Some(String::new());
            }
        });
        ui.horizontal(|ui| {
            ui.label("Sin proxy para");
            ui.add(
                egui::TextEdit::singleline(&mut proxy.exceptions)
                    .desired_width(330.0)
                    .hint_text("por ejemplo: .interno, 10., 192.168."),
            )
            .on_hover_text(
                "Separados por comas: un nombre exacto, un final de dominio (.interno) o un \
                 comienzo de dirección (10.)",
            );
        });
        if !proxy.username.trim().is_empty() && !vault.open {
            ui.colored_label(
                crate::theme::warn_color(),
                "Con el baúl bloqueado no se puede entregar la contraseña al proxy.",
            );
        }
    }
    ui.weak(
        "Por él pasa la primera conexión hacia cada equipo (SSH, SFTP, Telnet, VNC y RDP \
         integrado; también el primer salto y los túneles del gestor). Las direcciones de \
         este mismo equipo nunca pasan por el proxy, y cada equipo puede pedir conexión \
         directa en su ficha.",
    );
}

/// Shell que abren los terminales locales.
fn local_shell_section(ui: &mut egui::Ui, settings: &mut Settings) {
    ui.strong("Terminal local");
    ui.horizontal(|ui| {
        ui.label("Shell");
        ui.add(
            egui::TextEdit::singleline(&mut settings.local_shell)
                .desired_width(280.0)
                .hint_text("la habitual del sistema"),
        )
        .on_hover_text(
            "Programa y argumentos, por ejemplo «pwsh.exe -NoLogo» o «wsl.exe -d Ubuntu». \
             Una ruta con espacios va entre comillas",
        );
        ui.menu_button("Elegir…", |ui| {
            if ui.button("La habitual del sistema").clicked() {
                settings.local_shell.clear();
                ui.close();
            }
            for shell in crate::shells::detect() {
                if ui
                    .button(&shell.name)
                    .on_hover_text(&shell.command)
                    .clicked()
                {
                    settings.local_shell = shell.command;
                    ui.close();
                }
            }
        });
    });
    ui.weak(
        "Es la que abre el botón «Terminal local»; su flecha permite abrir cualquiera de las \
         otras para una sola pestaña.",
    );
}

/// Dónde se muestran las aplicaciones gráficas (X11).
fn x_server_section(ui: &mut egui::Ui, settings: &mut Settings, request: &mut SettingsRequest) {
    ui.strong("Aplicaciones gráficas (X11)");
    let x = &mut settings.x_server;
    ui.radio_value(
        &mut x.embedded,
        true,
        "Servidor X integrado de GodRemote, en una pestaña",
    )
    .on_hover_text("No hace falta instalar nada: VcXsrv, Xming ni XQuartz");
    ui.radio_value(
        &mut x.embedded,
        false,
        "Servidor X del sistema (el escritorio de Linux, VcXsrv, Xming, XQuartz…)",
    );
    ui.add_enabled_ui(x.embedded, |ui| {
        ui.horizontal(|ui| {
            ui.checkbox(
                &mut x.listen_local,
                "Aceptar también programas de este equipo",
            )
            .on_hover_text(
                "El servidor escucha en 127.0.0.1 y cualquier programa de este equipo puede \
                 conectarse sin contraseña (por ejemplo, desde WSL). La pestaña «Servidor X» \
                 indica el valor de DISPLAY",
            );
            if ui
                .button("Abrir su pestaña")
                .on_hover_text("Arranca el servidor X integrado y muestra su pantalla")
                .clicked()
            {
                request.open_x_server = true;
            }
        });
    });
    ui.weak(
        "El servidor integrado se usa con las sesiones SSH que tengan marcado «Reenviar \
         aplicaciones gráficas (X11)» y arranca con la primera aplicación. Entiende el \
         protocolo X11 básico (xterm, xclock, editores y herramientas clásicas); las \
         aplicaciones que exigen extensiones modernas (RENDER, XKB, GLX: GTK 3, Qt 5, \
         navegadores) necesitan el servidor X del sistema. El cambio afecta a las \
         conexiones nuevas.",
    );
}

/// Palabras resaltadas y enlaces del terminal.
fn highlight_section(ui: &mut egui::Ui, highlight: &mut Highlight) {
    ui.strong("Terminal: enlaces y palabras resaltadas");
    ui.checkbox(
        &mut highlight.links,
        "Reconocer las direcciones web (se abren con Ctrl+clic o con el botón derecho)",
    );
    ui.checkbox(
        &mut highlight.enabled,
        "Resaltar estas palabras con su color",
    )
    .on_hover_text(
        "Solo palabras enteras, sin distinguir mayúsculas, y solo en el texto que el servidor \
         no haya coloreado ya",
    );
    let mut remove = None;
    ui.add_enabled_ui(highlight.enabled, |ui| {
        for (index, rule) in highlight.rules.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt(("highlight_color", index))
                    .selected_text(rule.color.label())
                    .width(86.0)
                    .show_ui(ui, |ui| {
                        for (color, label) in HlColor::THEMED {
                            ui.selectable_value(&mut rule.color, color, label);
                        }
                        let custom = matches!(rule.color, HlColor::Custom(_));
                        if ui.selectable_label(custom, "Propio").clicked() && !custom {
                            rule.color = HlColor::Custom(Rgb::hex(0xff8800));
                        }
                    });
                // Muestra del color: los del tema, tal como se verán.
                match &mut rule.color {
                    HlColor::Custom(rgb) => {
                        ui.color_edit_button_srgb(&mut rgb.0);
                    }
                    themed => {
                        let color = themed.resolve(&crate::theme::terminal_palette());
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                        ui.painter().rect_filled(rect.shrink(2.0), 3.0, color);
                    }
                }
                ui.add(
                    egui::TextEdit::singleline(&mut rule.words)
                        .desired_width(330.0)
                        .hint_text("palabras o frases separadas por comas"),
                );
                if ui.button("🗑").on_hover_text("Quitar esta regla").clicked() {
                    remove = Some(index);
                }
            });
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    highlight.rules.len() < MAX_RULES,
                    egui::Button::new("➕ Añadir regla"),
                )
                .clicked()
            {
                highlight.rules.push(Rule::new("", HlColor::Cyan));
            }
            if ui
                .button("Restablecer")
                .on_hover_text("Vuelve a las reglas de fábrica: errores, avisos y aciertos")
                .clicked()
            {
                highlight.rules = Highlight::default_rules();
            }
        });
    });
    if let Some(index) = remove {
        highlight.rules.remove(index);
    }
    ui.weak(
        "Si dos reglas chocan gana la que va antes. Los colores con nombre salen de la paleta \
         ANSI del tema, así que cambian con él.",
    );
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
    ui.checkbox(
        &mut settings.log.record_always,
        "Grabar siempre las sesiones con sus tiempos, para reproducirlas (.cast)",
    )
    .on_hover_text(
        "Formato asciicast, el de asciinema, en la misma carpeta. Sin marcar, cada terminal \
         se graba solo si pulsas su botón «Grabar»",
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
        "Los registros son texto sin colores ni secuencias de escape; las grabaciones lo \
         guardan todo, con sus tiempos, y se reproducen desde «Herramientas». Unos y otras \
         contienen todo lo que aparece en pantalla (no lo que se teclea sin eco): cuidado \
         con las contraseñas que se muestren.",
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
