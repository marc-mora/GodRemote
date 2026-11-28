//! Barra de información del servidor (bajo los terminales SSH) y su ventana
//! de ajustes.

use egui::{Color32, FontId, RichText};

use super::util;
use crate::i18n::tr;
use crate::model::Session;
use crate::monitor::{
    BarItem, Monitor, ServerBar, Stats, format_kb, format_rate, format_uptime, parse_disks,
};
use crate::theme;
use crate::trf;

/// Un dato de la barra: etiqueta tenue, valor y, si es un porcentaje, su
/// barrita de uso.
struct Chip {
    label: String,
    value: String,
    percent: Option<f32>,
    hover: String,
}

impl Chip {
    fn new(label: &str, value: impl Into<String>) -> Self {
        Self {
            label: label.to_string(),
            value: value.into(),
            percent: None,
            hover: String::new(),
        }
    }

    fn percent(mut self, percent: f32) -> Self {
        self.percent = Some(percent.clamp(0.0, 100.0));
        self
    }

    fn hover(mut self, text: impl Into<String>) -> Self {
        self.hover = text.into();
        self
    }
}

/// Con la coma (o el punto) decimal del idioma en uso.
fn decimal(value: f32) -> String {
    crate::i18n::decimal(f64::from(value), 2)
}

/// Datos que se muestran para un elemento. Vacío si el servidor no lo da.
fn chips(item: BarItem, session: &Session, stats: Option<&Stats>) -> Vec<Chip> {
    // Lo que no depende del servidor se muestra desde el primer momento.
    match item {
        BarItem::Name => return vec![Chip::new("", session.display_name()).hover(tr("Sesión"))],
        BarItem::Target => {
            return vec![
                Chip::new("", session.target()).hover(tr("Usuario y dirección de conexión")),
            ];
        }
        _ => {}
    }
    let Some(s) = stats else {
        return vec![];
    };
    let one = |chip: Chip| vec![chip];
    match item {
        BarItem::Name | BarItem::Target => vec![],
        BarItem::Host if !s.host.is_empty() => {
            one(Chip::new("", s.host.clone()).hover(tr("Nombre del servidor")))
        }
        BarItem::Ip if !s.ips.is_empty() => {
            one(Chip::new("IP", s.ips[0].clone()).hover(s.ips.join("\n")))
        }
        BarItem::Os if !s.os.is_empty() || !s.kernel.is_empty() => {
            let name = if s.os.is_empty() { &s.kernel } else { &s.os };
            one(Chip::new("", name.clone()).hover(format!("{} · {}", s.kernel, s.arch)))
        }
        BarItem::Kernel if !s.kernel.is_empty() => one(Chip::new(
            "Kernel",
            format!("{} {}", s.kernel, s.arch).trim().to_string(),
        )),
        BarItem::Uptime => s
            .uptime_secs
            .map(|u| Chip::new(tr("Encendido"), format_uptime(u)))
            .into_iter()
            .collect(),
        BarItem::Load => s
            .load
            .map(|l| {
                let chip = Chip::new(
                    tr("Carga"),
                    format!("{} {} {}", decimal(l[0]), decimal(l[1]), decimal(l[2])),
                )
                .hover(tr("Carga media en 1, 5 y 15 minutos"));
                // Con los núcleos se sabe cuánta carga es mucha.
                match s.cores {
                    Some(cores) => Chip {
                        percent: Some((l[0] * 100.0 / cores as f32).clamp(0.0, 100.0)),
                        ..chip
                    },
                    None => chip,
                }
            })
            .into_iter()
            .collect(),
        BarItem::Cpu => {
            let cores = s
                .cores
                .map(|c| trf!("{c} núcleo(s)", c))
                .unwrap_or_default();
            match s.cpu {
                Some(cpu) => one(Chip::new("CPU", util::percent(cpu))
                    .percent(cpu)
                    .hover(cores)),
                // Hace falta una segunda muestra para saber el uso.
                None if s.memory.is_some() => one(Chip::new("CPU", "…").hover(cores)),
                None => vec![],
            }
        }
        BarItem::Memory => s
            .memory
            .map(|(used, total)| {
                let percent = used as f32 * 100.0 / total.max(1) as f32;
                Chip::new("Mem", format!("{} / {}", format_kb(used), format_kb(total)))
                    .percent(percent)
                    .hover(trf!(
                        "Memoria en uso: {uso}\nDisponible: {libre}",
                        uso = util::percent(percent),
                        libre = format_kb(total.saturating_sub(used))
                    ))
            })
            .into_iter()
            .collect(),
        BarItem::Swap => s
            .swap
            .map(|(used, total)| {
                let percent = used as f32 * 100.0 / total.max(1) as f32;
                Chip::new(
                    "Swap",
                    format!("{} / {}", format_kb(used), format_kb(total)),
                )
                .percent(percent)
                .hover(trf!(
                    "Intercambio en uso: {uso}",
                    uso = util::percent(percent)
                ))
            })
            .into_iter()
            .collect(),
        BarItem::Disk => s
            .disks
            .iter()
            .map(|d| {
                Chip::new(
                    &trf!("Disco {disco}", disco = d.mount),
                    trf!("{espacio} libres", espacio = format_kb(d.available)),
                )
                .percent(d.used_percent())
                .hover(trf!(
                    "{sistema}\nTotal: {total}\nUsado: {usado} ({uso})",
                    sistema = d.filesystem,
                    total = format_kb(d.total),
                    usado = format_kb(d.used),
                    uso = util::percent(d.used_percent())
                ))
            })
            .collect(),
        BarItem::Network => match s.network {
            Some((rx, tx)) => one(Chip::new(
                tr("Red"),
                format!("⬇ {}  ⬆ {}", format_rate(rx), format_rate(tx)),
            )
            .hover(tr(
                "Tráfico recibido y enviado (sin contar interfaces virtuales)",
            ))),
            None if s.memory.is_some() => one(Chip::new(tr("Red"), "…")),
            None => vec![],
        },
        BarItem::Users => s
            .users
            .map(|u| {
                Chip::new(tr("Usuarios"), u.to_string()).hover(tr("Sesiones de usuario abiertas"))
            })
            .into_iter()
            .collect(),
        BarItem::Processes => s
            .processes
            .map(|p| Chip::new(tr("Procesos"), p.to_string()))
            .into_iter()
            .collect(),
        BarItem::Time if !s.time.is_empty() => {
            // «2026-11-27 16:20:01 UTC»: en la barra, solo la hora.
            let short = s.time.split_once(' ').map(|(_, t)| t).unwrap_or(&s.time);
            one(Chip::new(tr("Hora"), short)
                .hover(trf!("Hora del servidor: {hora}", hora = s.time)))
        }
        _ => vec![],
    }
}

fn level_color(config: &ServerBar, percent: f32) -> Option<Color32> {
    if percent >= config.critical {
        Some(theme::error_color())
    } else if percent >= config.warn {
        Some(theme::warn_color())
    } else {
        None
    }
}

/// Alto que ocupa la barra con el tema activo.
pub fn height(ui: &egui::Ui) -> f32 {
    let font = FontId::proportional(theme::server_bar_font());
    ui.fonts_mut(|f| f.row_height(&font)) + 8.0
}

/// Dibuja la barra. Devuelve `true` si se pulsa el botón de ajustes.
pub fn show(
    ui: &mut egui::Ui,
    config: &ServerBar,
    session: &Session,
    monitor: Option<&Monitor>,
) -> bool {
    let font = FontId::proportional(theme::server_bar_font());
    let stats = monitor.and_then(|m| m.stats.as_ref());
    let mut open_settings = false;
    let rect = ui.max_rect();
    ui.painter()
        .rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(6.0, 0.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if ui
                .add(
                    egui::Button::new(RichText::new("⚙").font(font.clone()))
                        .frame(false)
                        .small(),
                )
                .on_hover_text(tr("Elegir qué se muestra en esta barra"))
                .clicked()
            {
                open_settings = true;
            }
            egui::ScrollArea::horizontal()
                .id_salt("server_bar_scroll")
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        let mut first = true;
                        for item in &config.items {
                            for chip in chips(*item, session, stats) {
                                if !first {
                                    ui.label(RichText::new("·").font(font.clone()).weak());
                                }
                                first = false;
                                chip_ui(ui, config, &font, &chip);
                            }
                        }
                        let note = match monitor {
                            Some(m) if m.stats.is_none() => match &m.error {
                                Some(error) => Some(trf!("Sin datos del servidor: {error}", error)),
                                None => Some(tr("Consultando el servidor…").to_string()),
                            },
                            Some(m) => m
                                .error
                                .as_ref()
                                .map(|error| trf!("Datos sin actualizar: {error}", error)),
                            // Conectando o desconectado: ya lo dice la
                            // cabecera de la pestaña.
                            None => None,
                        };
                        if let Some(note) = note {
                            if !first {
                                ui.label(RichText::new("·").font(font.clone()).weak());
                            }
                            ui.label(RichText::new(note).font(font.clone()).weak());
                        }
                    });
                });
        },
    );
    open_settings
}

fn chip_ui(ui: &mut egui::Ui, config: &ServerBar, font: &FontId, chip: &Chip) {
    let response = ui
        .horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if !chip.label.is_empty() {
                ui.label(RichText::new(&chip.label).font(font.clone()).weak());
            }
            let level = chip.percent.and_then(|p| level_color(config, p));
            let mut value = RichText::new(&chip.value).font(font.clone());
            if let Some(color) = level {
                value = value.color(color);
            }
            ui.label(value);
            if config.gauges
                && let Some(percent) = chip.percent
            {
                let size = egui::vec2(34.0, (font.size * 0.45).clamp(4.0, 10.0));
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(rect, 2.0, ui.visuals().widgets.inactive.bg_fill);
                let mut fill = rect;
                fill.set_width(rect.width() * percent / 100.0);
                painter.rect_filled(fill, 2.0, level.unwrap_or_else(theme::ok_color));
            }
        })
        .response;
    if !chip.hover.is_empty() {
        response.on_hover_text(&chip.hover);
    }
}

/// Ventana de ajustes de la barra.
pub struct ServerBarWindow {
    disks: String,
    rejected: Vec<String>,
}

impl ServerBarWindow {
    pub fn new(config: &ServerBar) -> Self {
        Self {
            disks: config.disks.join(", "),
            rejected: vec![],
        }
    }

    /// Devuelve (sigue abierta, han cambiado los ajustes).
    pub fn show(&mut self, ctx: &egui::Context, config: &mut ServerBar) -> (bool, bool) {
        let mut open = true;
        let before = config.clone();
        egui::Window::new(tr("Barra de información del servidor"))
            .id(egui::Id::unique("server_bar_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos([300.0, 70.0])
            .show(ctx, |ui| {
                ui.set_width(460.0);
                ui.checkbox(
                    &mut config.enabled,
                    tr("Mostrar la barra bajo los terminales SSH"),
                );
                ui.weak(tr(
                    "Los datos se piden por un canal aparte de la misma conexión, sin escribir \
                     nada en tu terminal. Hace falta que el servidor tenga una shell POSIX; el \
                     uso de CPU, memoria y red sale de /proc (Linux). Cada equipo puede \
                     desactivarla en su ficha.",
                ));
                ui.add_space(6.0);
                ui.add_enabled_ui(config.enabled, |ui| {
                    egui::Grid::new("server_bar_grid")
                        .num_columns(2)
                        .show(ui, |ui| {
                            ui.label(tr("Actualizar cada"));
                            ui.add(
                                egui::DragValue::new(&mut config.interval)
                                    .range(1..=3600)
                                    .suffix(" s"),
                            );
                            ui.end_row();
                            ui.label(tr("Discos"));
                            let response = ui
                                .add(
                                    egui::TextEdit::singleline(&mut self.disks)
                                        .desired_width(260.0)
                                        .hint_text("/, /home, /var"),
                                )
                                .on_hover_text(tr(
                                    "Rutas del servidor cuyo espacio libre se muestra, \
                                     separadas por comas",
                                ));
                            if response.changed() {
                                let (good, bad) = parse_disks(&self.disks);
                                self.rejected = bad;
                                config.disks = if good.is_empty() {
                                    vec!["/".into()]
                                } else {
                                    good
                                };
                            }
                            ui.end_row();
                            ui.label(tr("Barritas de uso"));
                            ui.checkbox(&mut config.gauges, tr("Junto a CPU, memoria y disco"));
                            ui.end_row();
                            ui.label(tr("Color de aviso desde"));
                            ui.add(
                                egui::Slider::new(&mut config.warn, 1.0..=100.0)
                                    .suffix(tr(" %"))
                                    .max_decimals(0),
                            );
                            ui.end_row();
                            ui.label(tr("Color de error desde"));
                            ui.add(
                                egui::Slider::new(&mut config.critical, 1.0..=100.0)
                                    .suffix(tr(" %"))
                                    .max_decimals(0),
                            );
                            ui.end_row();
                        });
                    if !self.rejected.is_empty() {
                        ui.colored_label(
                            theme::error_color(),
                            trf!(
                                "Rutas no admitidas (solo letras, números y / _ . -): {rutas}",
                                rutas = self.rejected.join(" ")
                            ),
                        );
                    }
                    ui.add_space(6.0);
                    ui.strong(tr("Qué se muestra, y en qué orden"));
                    items_ui(ui, config);
                });
                ui.add_space(6.0);
                if ui.button(tr("Restablecer")).clicked() {
                    *config = ServerBar::default();
                    *self = Self::new(config);
                }
            });
        config.critical = config.critical.max(config.warn);
        (open, *config != before)
    }
}

/// Lista de elementos: primero los visibles, en su orden, y después el resto.
fn items_ui(ui: &mut egui::Ui, config: &mut ServerBar) {
    enum Action {
        Hide(usize),
        Show(BarItem),
        Up(usize),
        Down(usize),
    }
    let mut action = None;
    let count = config.items.len();
    egui::ScrollArea::vertical()
        .id_salt("server_bar_items")
        .max_height(250.0)
        .show(ui, |ui| {
            egui::Grid::new("server_bar_items_grid")
                .num_columns(2)
                .show(ui, |ui| {
                    for (i, item) in config.items.iter().enumerate() {
                        let mut visible = true;
                        if ui.checkbox(&mut visible, tr(item.label())).changed() {
                            action = Some(Action::Hide(i));
                        }
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(i > 0, egui::Button::new("⬆").small())
                                .on_hover_text(tr("Antes"))
                                .clicked()
                            {
                                action = Some(Action::Up(i));
                            }
                            if ui
                                .add_enabled(i + 1 < count, egui::Button::new("⬇").small())
                                .on_hover_text(tr("Después"))
                                .clicked()
                            {
                                action = Some(Action::Down(i));
                            }
                        });
                        ui.end_row();
                    }
                    for item in BarItem::ALL {
                        if config.items.contains(&item) {
                            continue;
                        }
                        let mut visible = false;
                        if ui.checkbox(&mut visible, tr(item.label())).changed() {
                            action = Some(Action::Show(item));
                        }
                        ui.end_row();
                    }
                });
        });
    match action {
        Some(Action::Hide(i)) => {
            config.items.remove(i);
        }
        Some(Action::Show(item)) => config.items.push(item),
        Some(Action::Up(i)) => config.items.swap(i, i - 1),
        Some(Action::Down(i)) => config.items.swap(i, i + 1),
        None => {}
    }
}
