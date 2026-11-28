//! Ventana del panel de estado: todos los equipos SSH de una carpeta a la
//! vez, con sus alertas y su tendencia.
//!
//! Aquí solo se pinta: los datos los recoge y los calcula
//! [`crate::dashboard`] en segundo plano, y es él quien pide repintar cuando
//! llega algo nuevo.

use std::time::{Duration, Instant};

use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke};
use uuid::Uuid;

use super::util::percent;
use crate::dashboard::{
    AlertEvent, AlertKind, Column, Dashboard, DashboardSettings, HostJob, HostRow, Link, Options,
    State, Thresholds, TrendMetric, TrendPoint, brief, fullest_disk, is_real_disk, load_per_core,
    memory_percent, summarize,
};
use crate::i18n::tr;
use crate::model::{Protocol, Tree};
use crate::monitor::{format_kb, format_uptime};
use crate::theme::{error_color, ok_color, warn_color};
use crate::{mark, trf};

/// Lo que la ventana pide a la aplicación.
pub enum DashboardRequest {
    None,
    /// Arrancar (o volver a arrancar) el panel con los equipos de la carpeta.
    Start,
    /// Abrir el terminal de un equipo.
    Open(Uuid),
    /// Reintentar ya la conexión de un equipo, preguntando lo que haga falta.
    Retry(Uuid),
}

/// Lo que se elige en la ventana: filtro, orden, qué está desplegado.
struct View {
    filter: String,
    only_problems: bool,
    sort: Column,
    descending: bool,
    selected: Option<Uuid>,
    show_settings: bool,
    show_history: bool,
    /// Orden de las filas ya calculado, y para qué estado se calculó.
    order: Option<(OrderKey, Vec<usize>)>,
}

type OrderKey = (u64, String, bool, Column, bool);

pub struct DashboardWindow {
    /// Carpeta cuyos equipos se vigilan; `None` son todos.
    folder: Option<Uuid>,
    /// El panel en marcha.
    run: Option<Dashboard>,
    /// Hay que (volver a) arrancarlo.
    start: bool,
    /// El baúl estaba abierto la última vez que se miró.
    vault_open: bool,
    /// Tope de conexiones con el que arrancó (cambiarlo pide recargar).
    started_connections: u32,
    /// Alertas de las que aún no se ha avisado, y desde cuándo esperan.
    news: Vec<AlertEvent>,
    news_since: Option<Instant>,
    view: View,
}

/// Lo que se espera antes de avisar de una alerta, por si llegan más: al
/// abrir el panel, o si se cae la red, saltan varias casi a la vez y el
/// aviso de la aplicación solo enseña uno.
const NEWS_DELAY: Duration = Duration::from_millis(1200);

impl DashboardWindow {
    pub fn new(folder: Option<Uuid>) -> Self {
        Self {
            folder,
            run: None,
            start: true,
            vault_open: false,
            started_connections: 0,
            news: vec![],
            news_since: None,
            view: View {
                filter: String::new(),
                only_problems: false,
                sort: Column::Name,
                descending: false,
                selected: None,
                show_settings: false,
                show_history: true,
                order: None,
            },
        }
    }

    /// Equipos SSH de la carpeta elegida (con sus subcarpetas).
    fn hosts(&self, tree: &Tree) -> Vec<Uuid> {
        tree.session_paths()
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| {
                tree.session(*id)
                    .is_some_and(|s| s.protocol == Protocol::Ssh)
                    && self.folder.is_none_or(|folder| tree.is_inside(folder, *id))
            })
            .collect()
    }

    /// Devuelve si la ventana sigue abierta, si han cambiado las
    /// preferencias y lo que haya que hacer.
    fn show(
        &mut self,
        ctx: &egui::Context,
        tree: &Tree,
        settings: &mut DashboardSettings,
    ) -> (bool, bool, DashboardRequest) {
        let mut open = true;
        let before = settings.clone();
        let mut request = DashboardRequest::None;
        if std::mem::take(&mut self.start) {
            request = DashboardRequest::Start;
        }
        let mut clear_history = false;
        let folders = tree.folder_paths();
        let folder_name = match self.folder {
            None => tr("todas las carpetas").to_string(),
            Some(id) => folders
                .iter()
                .find(|(fid, _)| *fid == id)
                .map(|(_, path)| path.clone())
                .unwrap_or_else(|| tr("(carpeta eliminada)").into()),
        };
        // El estado se coge una vez por fotograma y se suelta al acabar de
        // pintar: mientras tanto, lo que llegue espera.
        let state = self.run.as_ref().map(|run| run.state());
        let view = &mut self.view;
        let folder = &mut self.folder;
        let restart_needed =
            self.run.is_some() && self.started_connections != settings.max_connections;
        let shown = egui::Window::new(tr("📊 Panel de estado"))
            .id(egui::Id::unique("dashboard_window"))
            .open(&mut open)
            .collapsible(true)
            .resizable(true)
            .default_pos([210.0, 56.0])
            .default_size([1040.0, 500.0])
            .min_size([980.0, 260.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(tr("Equipos de"));
                    let chosen = *folder;
                    egui::ComboBox::from_id_salt("dashboard_folder")
                        .selected_text(&folder_name)
                        .width(210.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(folder, None, tr("todas las carpetas"));
                            for (id, path) in &folders {
                                ui.selectable_value(folder, Some(*id), path);
                            }
                        });
                    if *folder != chosen {
                        request = DashboardRequest::Start;
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut view.filter)
                            .hint_text(tr("🔍 Filtrar por nombre, dirección o estado…"))
                            .desired_width(250.0),
                    );
                    ui.checkbox(&mut view.only_problems, tr("Solo con problemas"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button("🔄")
                            .on_hover_text(tr(
                                "Recargar: vuelve a leer los equipos de la carpeta y abre \
                                 las conexiones de nuevo",
                            ))
                            .clicked()
                        {
                            request = DashboardRequest::Start;
                        }
                        ui.toggle_value(&mut view.show_settings, tr("⚙ Refresco y alertas"))
                            .on_hover_text(tr(
                                "Cada cuánto se actualiza y cuándo salta una alerta",
                            ));
                    });
                });
                if view.show_settings {
                    ui.separator();
                    settings_ui(ui, settings, restart_needed);
                }
                ui.separator();
                // (Aunque no haya tabla, la ventana conserva su tamaño.)
                let Some(state) = &state else {
                    ui.weak(tr("Abriendo las conexiones…"));
                    ui.allocate_space(ui.available_size());
                    return;
                };
                if state.hosts.is_empty() {
                    ui.weak(tr("No hay equipos SSH en esta carpeta."));
                    ui.allocate_space(ui.available_size());
                    return;
                }
                // Abajo, el historial; la tabla se queda con el resto.
                egui::Panel::bottom("dashboard_history")
                    .resizable(false)
                    .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(0, 4)))
                    .show(ui, |ui| {
                        clear_history = history(ui, state, &mut view.show_history);
                    });
                egui::CentralPanel::no_frame().show(ui, |ui| {
                    if let Some(wanted) = table(ui, state, view, settings) {
                        request = wanted;
                    }
                });
            });
        drop(state);
        if let Some(run) = &self.run {
            // Plegada (o cerrada), los datos nuevos no hacen repintar.
            run.set_watched(shown.is_some_and(|window| window.inner.is_some()));
            if clear_history {
                run.clear_history();
            }
        }
        (open, *settings != before, request)
    }
}

/// Resumen de un vistazo: cuántos equipos están en línea y cuántos tienen
/// alguna alerta. Para una fila que se rellena de derecha a izquierda: se
/// añade al revés de como se lee.
fn summary(ui: &mut egui::Ui, state: &State) {
    let total = state.hosts.len();
    let online = state.online();
    let alerting = state.alerting();
    if alerting > 0 {
        ui.colored_label(
            error_color(),
            match alerting {
                1 => tr("1 equipo con alertas").to_string(),
                n => trf!("{n} equipos con alertas", n),
            },
        );
        ui.weak("·");
    }
    let color = if online == total {
        ok_color()
    } else {
        warn_color()
    };
    ui.colored_label(color, trf!("{online} de {total} en línea", online, total));
}

/// Casilla con su valor para un umbral que, a cero, no se vigila.
fn threshold_ui(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    default: f32,
    widget: impl FnOnce(&mut f32) -> egui::DragValue<'_>,
) {
    let mut on = *value > 0.0;
    if ui.checkbox(&mut on, label).changed() {
        *value = if on { default } else { 0.0 };
    }
    if on {
        ui.add(widget(value));
    } else {
        ui.weak(tr("no se vigila"));
    }
    ui.end_row();
}

fn settings_ui(ui: &mut egui::Ui, settings: &mut DashboardSettings, restart_needed: bool) {
    let defaults = Thresholds::default();
    let mut reset = false;
    ui.horizontal_top(|ui| {
        egui::Grid::new("dashboard_settings")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label(tr("Actualizar cada"));
                ui.add(
                    egui::DragValue::new(&mut settings.interval)
                        .range(DashboardSettings::MIN_INTERVAL..=DashboardSettings::MAX_INTERVAL)
                        .suffix(" s"),
                );
                ui.end_row();
                ui.label(tr("Conexiones a la vez, como mucho"))
                    .on_hover_text(tr(
                        "Cada equipo mantiene su conexión abierta entre refrescos. Si la \
                         carpeta tiene más equipos que este tope, se van turnando: cada uno \
                         conecta, da sus datos y cede el sitio al siguiente",
                    ));
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut settings.max_connections)
                            .range(1..=DashboardSettings::MAX_CONNECTIONS),
                    );
                    if restart_needed {
                        ui.colored_label(warn_color(), tr("se aplica al recargar (🔄)"));
                    }
                });
                ui.end_row();
                ui.label(tr("Minigráfica"));
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("dashboard_trend")
                        .selected_text(tr(settings.trend.label()))
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            for metric in TrendMetric::ALL {
                                ui.selectable_value(
                                    &mut settings.trend,
                                    metric,
                                    tr(metric.label()),
                                );
                            }
                        });
                    ui.label(tr("de los últimos"));
                    ui.add(
                        egui::DragValue::new(&mut settings.trend_minutes)
                            .range(1..=DashboardSettings::MAX_TREND_MINUTES)
                            .suffix(" min"),
                    );
                });
                ui.end_row();
                ui.label(tr("Avisos"));
                ui.checkbox(
                    &mut settings.notify,
                    tr("Avisar en la aplicación al saltar o resolverse una alerta"),
                );
                ui.end_row();
            });
        ui.add_space(24.0);
        let limits = &mut settings.thresholds;
        egui::Grid::new("dashboard_thresholds")
            .num_columns(2)
            .show(ui, |ui| {
                threshold_ui(
                    ui,
                    tr("CPU por encima del"),
                    &mut limits.cpu,
                    defaults.cpu,
                    |v| {
                        egui::DragValue::new(v)
                            .range(1.0..=100.0)
                            .max_decimals(0)
                            .suffix(tr(" %"))
                    },
                );
                threshold_ui(
                    ui,
                    tr("Carga por núcleo por encima de"),
                    &mut limits.load,
                    defaults.load,
                    |v| {
                        // Con la coma (o el punto) decimal del idioma en uso.
                        egui::DragValue::new(v)
                            .range(0.1..=100.0)
                            .speed(0.1)
                            .custom_formatter(|n, _| decimal(n as f32, 1))
                            .custom_parser(|text| text.trim().replace(',', ".").parse().ok())
                    },
                );
                ui.label(tr("     …sostenidas durante")).on_hover_text(tr(
                    "La CPU y la carga tienen que seguir altas todas estas muestras \
                         seguidas para que salte la alerta, y otras tantas bajas para darla \
                         por resuelta",
                ));
                ui.add(
                    egui::DragValue::new(&mut limits.sustained)
                        .range(1..=20)
                        .suffix(tr(" muestras seguidas")),
                );
                ui.end_row();
                threshold_ui(
                    ui,
                    tr("Memoria por encima del"),
                    &mut limits.memory,
                    defaults.memory,
                    |v| {
                        egui::DragValue::new(v)
                            .range(1.0..=100.0)
                            .max_decimals(0)
                            .suffix(tr(" %"))
                    },
                );
                threshold_ui(
                    ui,
                    tr("Disco más lleno por encima del"),
                    &mut limits.disk,
                    defaults.disk,
                    |v| {
                        egui::DragValue::new(v)
                            .range(1.0..=100.0)
                            .max_decimals(0)
                            .suffix(tr(" %"))
                    },
                );
                ui.checkbox(
                    &mut limits.unreachable,
                    tr("Equipo inalcanzable o sin acceso"),
                )
                .on_hover_text(tr(
                    "Cuando falla el intento de conectar o de volver a conectar, y cuando \
                         se llega al equipo pero no se puede entrar (falta una credencial, el \
                         servidor la rechaza o ha cambiado su clave)",
                ));
                reset = ui.button(tr("Restablecer")).clicked();
                ui.end_row();
            });
    });
    if reset {
        *settings = DashboardSettings::default();
    }
}

/// Columnas de la tabla.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cell {
    Name,
    State,
    Latency,
    Cpu,
    Load,
    Memory,
    Disk,
    Uptime,
    Trend,
    Updated,
}

impl Cell {
    /// Columnas, con su título (sin traducir: también las identifica) y su
    /// ancho (0 = lo que sobre).
    const ALL: [(Cell, &'static str, f32); 10] = [
        (Cell::Name, mark!("Equipo"), 0.0),
        (Cell::State, mark!("Estado"), 124.0),
        (Cell::Latency, mark!("Latencia"), 66.0),
        (Cell::Cpu, mark!("CPU"), 82.0),
        (Cell::Load, mark!("Carga"), 54.0),
        (Cell::Memory, mark!("Memoria"), 82.0),
        (Cell::Disk, mark!("Disco más lleno"), 136.0),
        (Cell::Uptime, mark!("Encendido"), 78.0),
        (Cell::Trend, mark!("Tendencia"), 116.0),
        (Cell::Updated, mark!("Actualizado"), 86.0),
    ];

    /// Por qué se ordena al pulsar su título.
    fn sorts_by(self) -> Option<Column> {
        Some(match self {
            Cell::Name => Column::Name,
            Cell::State => Column::State,
            Cell::Latency => Column::Latency,
            Cell::Cpu => Column::Cpu,
            Cell::Load => Column::Load,
            Cell::Memory => Column::Memory,
            Cell::Disk => Column::Disk,
            Cell::Uptime => Column::Uptime,
            Cell::Updated => Column::Updated,
            Cell::Trend => return None,
        })
    }
}

/// Reparte el ancho entre las columnas: la del nombre se queda lo que sobre.
fn layout(left: f32, width: f32) -> Vec<(Cell, &'static str, f32, f32)> {
    let fixed: f32 = Cell::ALL.iter().map(|(_, _, w)| w).sum();
    let name = (width - fixed).max(130.0);
    let mut x = left;
    Cell::ALL
        .iter()
        .map(|(cell, title, w)| {
            let w = if *w == 0.0 { name } else { *w };
            let range = (*cell, *title, x, x + w);
            x += w;
            range
        })
        .collect()
}

/// Con la coma (o el punto) decimal del idioma en uso.
fn decimal(value: f32, places: usize) -> String {
    crate::i18n::decimal(f64::from(value), places)
}

fn percent_text(value: Option<f32>) -> String {
    value.map(percent).unwrap_or_default()
}

fn latency_text(latency: Option<Duration>) -> String {
    match latency {
        None => String::new(),
        Some(l) if l < Duration::from_millis(1) => "<1 ms".to_string(),
        Some(l) if l < Duration::from_secs(1) => format!("{} ms", l.as_millis()),
        Some(l) => format!("{} s", decimal(l.as_secs_f32(), 1)),
    }
}

/// Texto de una celda. Vacío si el equipo no da ese dato.
fn cell_text(host: &HostRow, cell: Cell) -> String {
    let stats = host.stats.as_ref();
    match cell {
        Cell::Name => host.name.clone(),
        Cell::State => tr(host.state_label()).to_string(),
        Cell::Latency => latency_text(host.latency),
        Cell::Cpu => percent_text(stats.and_then(|s| s.cpu)),
        Cell::Load => stats
            .and_then(|s| s.load)
            .map(|l| decimal(l[0], 2))
            .unwrap_or_default(),
        Cell::Memory => percent_text(stats.and_then(memory_percent)),
        Cell::Disk => stats
            .and_then(fullest_disk)
            .map(|d| format!("{} {}", percent(d.used_percent()), brief(&d.mount, 60)))
            .unwrap_or_default(),
        Cell::Uptime => stats
            .and_then(|s| s.uptime_secs)
            .map(format_uptime)
            .unwrap_or_default(),
        Cell::Updated => host
            .updated
            .map(|at| at.format("%H:%M:%S").to_string())
            .unwrap_or_default(),
        Cell::Trend => String::new(),
    }
}

/// Color del punto y del texto de estado.
fn state_color(host: &HostRow, weak: Color32) -> Color32 {
    match &host.link {
        Link::Online if host.probe_error.is_some() => warn_color(),
        Link::Online => ok_color(),
        Link::Connecting(_) => warn_color(),
        Link::Waiting => weak,
        Link::Down(_) => error_color(),
        Link::NeedsCredential | Link::Rejected(_) => warn_color(),
    }
}

/// Todo lo que se sabe del equipo, para cuando se deja el ratón encima.
fn details(host: &HostRow) -> String {
    let mut lines = vec![host.target.clone()];
    match &host.link {
        Link::Waiting => lines.push(tr("A la espera de turno para conectar").into()),
        Link::Connecting(step) => lines.push(step.clone()),
        Link::Online => {}
        Link::Down(error) => {
            lines.push(trf!("Sin conexión: {error}", error));
            match (&host.step, host.retry_at) {
                (Some(step), _) => lines.push(trf!("Reintentando: {step}", step)),
                (None, Some(at)) => lines.push(trf!(
                    "Intentos fallidos: {n} · próximo intento a las {hora}",
                    n = host.failures,
                    hora = at.format("%H:%M:%S")
                )),
                (None, None) => {}
            }
        }
        Link::NeedsCredential => lines.push(
            tr(
                "Falta una contraseña o passphrase que no está en el baúl. Botón derecho → \
                 Reintentar para escribirla",
            )
            .into(),
        ),
        Link::Rejected(error) => lines.push(trf!(
            "No se reintenta sola: {error}. Botón derecho → Reintentar",
            error
        )),
    }
    if let Some(error) = &host.probe_error {
        lines.push(trf!("Conectado, pero sin datos: {error}", error));
    }
    if let Some(stats) = &host.stats {
        if host.link != Link::Online {
            lines.push(tr("Últimos datos conocidos:").into());
        }
        if let Some(load) = stats.load {
            let [a, b, c] = load.map(|value| decimal(value, 2));
            lines.push(match stats.cores {
                Some(cores) => trf!("Carga: {a} {b} {c} · {cores} núcleo(s)", a, b, c, cores),
                None => trf!("Carga: {a} {b} {c}", a, b, c),
            });
        }
        if let Some((used, total)) = stats.memory {
            lines.push(trf!(
                "Memoria: {usada} de {total}",
                usada = format_kb(used),
                total = format_kb(total)
            ));
        }
        for disk in stats.disks.iter().filter(|d| is_real_disk(d)).take(12) {
            lines.push(trf!(
                "Disco {disco}: {uso} · quedan {libre}",
                disco = brief(&disk.mount, 60),
                uso = percent(disk.used_percent()),
                libre = format_kb(disk.available)
            ));
        }
    }
    let alerts: Vec<&str> = host.alerts.active().map(|kind| tr(kind.label())).collect();
    if !alerts.is_empty() {
        lines.push(trf!("Alertas: {lista}", lista = alerts.join(", ")));
    }
    lines.push(tr("Doble clic: abrir su terminal").into());
    lines.join("\n")
}

/// Tramos de la minigráfica: cada uno es una línea seguida, y entre uno y
/// otro hay un hueco sin datos (el equipo estuvo sin conexión). `top` es el
/// valor que queda arriba del todo.
fn trend_lines<'a>(
    points: impl Iterator<Item = &'a TrendPoint>,
    metric: TrendMetric,
    rect: Rect,
    now: Instant,
    window: Duration,
    max_gap: Duration,
    top: f32,
) -> Vec<Vec<Pos2>> {
    let mut lines: Vec<Vec<Pos2>> = vec![];
    let mut last: Option<Instant> = None;
    for point in points {
        let value = match metric {
            TrendMetric::Cpu => point.cpu,
            TrendMetric::Load => point.load,
        };
        let age = now.saturating_duration_since(point.at);
        let Some(value) = value.filter(|_| age <= window) else {
            continue;
        };
        let x = rect.right() - rect.width() * age.as_secs_f32() / window.as_secs_f32().max(1.0);
        let y = rect.bottom() - rect.height() * (value / top.max(0.001)).clamp(0.0, 1.0);
        let pos = Pos2::new(x, y);
        match (lines.last_mut(), last) {
            (Some(line), Some(before)) if point.at.saturating_duration_since(before) <= max_gap => {
                line.push(pos)
            }
            _ => lines.push(vec![pos]),
        }
        last = Some(point.at);
    }
    lines
}

/// Dibuja la minigráfica de un equipo.
fn sparkline(
    painter: &egui::Painter,
    rect: Rect,
    host: &HostRow,
    settings: &DashboardSettings,
    faint: Color32,
) {
    let limits = &settings.thresholds;
    let cores = host
        .stats
        .as_ref()
        .and_then(|s| s.cores)
        .unwrap_or(1)
        .max(1) as f32;
    let (top, limit, kind) = match settings.trend {
        TrendMetric::Cpu => (100.0, limits.cpu, AlertKind::Cpu),
        TrendMetric::Load => {
            // Arriba, lo mayor de: los núcleos, el umbral y lo que se haya visto.
            let seen = host
                .trend
                .points()
                .filter_map(|p| p.load)
                .fold(0.0, f32::max);
            let limit = limits.load * cores;
            (cores.max(limit).max(seen) * 1.1, limit, AlertKind::Load)
        }
    };
    // Entre dos puntos seguidos no hay más que un intervalo (o un tramo, si
    // las muestras se funden): más que eso es un hueco.
    let window = settings.trend_window();
    let step = window / crate::dashboard::TREND_POINTS;
    let max_gap = Duration::from_secs(u64::from(settings.interval) * 3 + 5).max(step * 3);
    let inner = rect.shrink2(egui::vec2(1.0, 2.0));
    let lines = trend_lines(
        host.trend.points(),
        settings.trend,
        inner,
        Instant::now(),
        window,
        max_gap,
        top,
    );
    // Un equipo que no da ese dato no tiene gráfica: la celda queda vacía.
    if lines.is_empty() {
        return;
    }
    painter.rect_filled(rect, 2.0, faint);
    if limit > 0.0 && limit < top {
        let y = rect.bottom() - rect.height() * limit / top;
        painter.hline(
            rect.x_range(),
            y,
            Stroke::new(1.0, warn_color().gamma_multiply(0.45)),
        );
    }
    let color = if host.alerts.is_active(kind) {
        error_color()
    } else {
        ok_color()
    };
    for line in lines {
        match line[..] {
            [only] => {
                painter.circle_filled(only, 1.5, color);
            }
            _ => {
                painter.line(line, Stroke::new(1.3, color));
            }
        }
    }
}

/// Barrita de uso junto a un porcentaje.
fn gauge(painter: &egui::Painter, rect: Rect, percent: f32, color: Color32, back: Color32) {
    painter.rect_filled(rect, 2.0, back);
    let mut fill = rect;
    fill.set_width(rect.width() * percent.clamp(0.0, 100.0) / 100.0);
    painter.rect_filled(fill, 2.0, color);
}

/// La tabla de equipos. Devuelve lo que se pida desde ella.
fn table(
    ui: &mut egui::Ui,
    state: &State,
    view: &mut View,
    settings: &DashboardSettings,
) -> Option<DashboardRequest> {
    let mut request = None;
    let font = egui::TextStyle::Body.resolve(ui.style());
    let small = FontId::proportional(font.size - 1.0);
    let row_height = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
    let table_id = ui.scope_id().with("dashboard_table");

    // Cabecera: pulsar un título ordena por esa columna; otra vez, al revés.
    let width = ui.available_width();
    let (header, _) = ui.allocate_exact_size(egui::vec2(width, row_height), Sense::hover());
    let columns = layout(header.left(), width - 14.0);
    for (cell, title, x0, x1) in &columns {
        let rect = Rect::from_min_max(
            Pos2::new(*x0, header.top()),
            Pos2::new(*x1, header.bottom()),
        );
        let sorts_by = cell.sorts_by();
        let sorted = sorts_by == Some(view.sort);
        let mut text = tr(title).to_string();
        if sorted {
            text.push_str(if view.descending { " ⬇" } else { " ⬆" });
        }
        let mut color = ui.visuals().strong_text_color();
        if let Some(column) = sorts_by {
            let response = ui
                .interact(rect, table_id.with(("title", *title)), Sense::click())
                .on_hover_text(tr("Ordenar por esta columna"));
            if response.hovered() {
                ui.painter()
                    .rect_filled(rect, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
            }
            if response.clicked() {
                if sorted {
                    view.descending = !view.descending;
                } else {
                    view.sort = column;
                    // Los números, de mayor a menor: lo que más preocupa, arriba.
                    view.descending = !matches!(column, Column::Name | Column::State);
                }
            }
        } else {
            color = ui.visuals().text_color();
            ui.interact(rect, table_id.with(("title", *title)), Sense::hover())
                .on_hover_text(trf!(
                    "{dato} en los últimos {minutos} min. La raya marca el umbral de la alerta",
                    dato = tr(settings.trend.label()),
                    minutos = settings.trend_minutes
                ));
        }
        ui.painter().with_clip_rect(rect).text(
            Pos2::new(rect.left() + 6.0, rect.center().y),
            Align2::LEFT_CENTER,
            text,
            font.clone(),
            color,
        );
    }

    // El orden solo se recalcula cuando cambian los datos o lo elegido.
    let key: OrderKey = (
        state.revision,
        view.filter.clone(),
        view.only_problems,
        view.sort,
        view.descending,
    );
    if view.order.as_ref().is_none_or(|(k, _)| *k != key) {
        let rows = state.order(&view.filter, view.only_problems, view.sort, view.descending);
        view.order = Some((key, rows));
    }
    let rows: &[usize] = view.order.as_ref().map(|(_, r)| &r[..]).unwrap_or(&[]);
    if rows.is_empty() {
        ui.add_space(6.0);
        ui.weak(tr("Ningún equipo coincide con el filtro."));
        return request;
    }

    let now = Instant::now();
    let limits = &settings.thresholds;
    let selected = &mut view.selected;
    egui::ScrollArea::vertical()
        .id_salt("dashboard_rows")
        .auto_shrink([false, false])
        .show_rows(ui, row_height, rows.len(), |ui, range| {
            let weak = ui.visuals().weak_text_color();
            let normal = ui.visuals().text_color();
            let back = ui.visuals().widgets.inactive.bg_fill;
            let faint = ui.visuals().faint_bg_color;
            for index in &rows[range] {
                let host = &state.hosts[*index];
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(width, row_height), Sense::hover());
                let response = ui.interact(rect, table_id.with(host.id), Sense::click());
                // Fondo: la fila con alertas se ve de lejos.
                let alert = if host.alerts.is_active(AlertKind::Unreachable) {
                    Some(error_color())
                } else if host.alerts.any() {
                    Some(warn_color())
                } else {
                    None
                };
                let painter = ui.painter();
                match alert {
                    Some(color) => {
                        let strength = if response.hovered() { 0.26 } else { 0.16 };
                        painter.rect_filled(rect, 3.0, color.gamma_multiply(strength));
                    }
                    None if response.hovered() => {
                        painter.rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill);
                    }
                    None => {}
                }
                if *selected == Some(host.id) {
                    painter.rect_stroke(
                        rect.shrink(0.5),
                        3.0,
                        Stroke::new(1.0, ui.visuals().selection.bg_fill),
                        egui::StrokeKind::Inside,
                    );
                }
                // Los datos viejos (sin conexión, o sin muestras hace rato)
                // se ven apagados.
                let stale = host.is_stale(now, settings.interval);
                let data_color = if stale { weak } else { normal };
                let stats = host.stats.as_ref();
                for (cell, _, x0, x1) in &columns {
                    let cell_rect = Rect::from_min_max(
                        Pos2::new(*x0 + 6.0, rect.top()),
                        Pos2::new(*x1 - 4.0, rect.bottom()),
                    );
                    let clipped = painter.with_clip_rect(cell_rect);
                    let text_at = Pos2::new(cell_rect.left(), rect.center().y);
                    let text = cell_text(host, *cell);
                    let alerting = |kind| {
                        if host.alerts.is_active(kind) {
                            error_color()
                        } else {
                            data_color
                        }
                    };
                    match cell {
                        Cell::Name => {
                            let color = if host.has_problem() {
                                ui.visuals().strong_text_color()
                            } else {
                                normal
                            };
                            clipped.text(text_at, Align2::LEFT_CENTER, text, font.clone(), color);
                        }
                        Cell::State => {
                            let color = state_color(host, weak);
                            clipped.circle_filled(
                                Pos2::new(cell_rect.left() + 4.0, rect.center().y),
                                4.0,
                                color,
                            );
                            let color = if host.link == Link::Online && host.probe_error.is_none() {
                                normal
                            } else {
                                color
                            };
                            clipped.text(
                                text_at + egui::vec2(13.0, 0.0),
                                Align2::LEFT_CENTER,
                                text,
                                font.clone(),
                                color,
                            );
                        }
                        Cell::Cpu | Cell::Memory => {
                            let (percent, kind, limit) = if *cell == Cell::Cpu {
                                (stats.and_then(|s| s.cpu), AlertKind::Cpu, limits.cpu)
                            } else {
                                (
                                    stats.and_then(memory_percent),
                                    AlertKind::Memory,
                                    limits.memory,
                                )
                            };
                            if let Some(percent) = percent {
                                let color = alerting(kind);
                                clipped.text(
                                    Pos2::new(cell_rect.left() + 34.0, rect.center().y),
                                    Align2::RIGHT_CENTER,
                                    text,
                                    font.clone(),
                                    color,
                                );
                                let bar = Rect::from_min_size(
                                    Pos2::new(cell_rect.left() + 40.0, rect.center().y - 3.0),
                                    egui::vec2(30.0, 6.0),
                                );
                                let fill = if host.alerts.is_active(kind) {
                                    error_color()
                                } else if limit > 0.0 && percent >= limit {
                                    warn_color()
                                } else if stale {
                                    weak
                                } else {
                                    ok_color()
                                };
                                gauge(&clipped, bar, percent, fill, back);
                            }
                        }
                        Cell::Load => {
                            // Por encima del umbral, aunque aún no lleve las
                            // muestras seguidas que hacen saltar la alerta.
                            let high = limits.load > 0.0
                                && stats
                                    .and_then(load_per_core)
                                    .is_some_and(|l| l >= limits.load);
                            let color = if host.alerts.is_active(AlertKind::Load) {
                                error_color()
                            } else if high && !stale {
                                warn_color()
                            } else {
                                data_color
                            };
                            clipped.text(
                                Pos2::new(cell_rect.left() + 34.0, rect.center().y),
                                Align2::RIGHT_CENTER,
                                text,
                                font.clone(),
                                color,
                            );
                        }
                        Cell::Disk => {
                            clipped.text(
                                text_at,
                                Align2::LEFT_CENTER,
                                text,
                                font.clone(),
                                alerting(AlertKind::Disk),
                            );
                        }
                        Cell::Latency => {
                            clipped.text(
                                Pos2::new(cell_rect.right() - 6.0, rect.center().y),
                                Align2::RIGHT_CENTER,
                                text,
                                font.clone(),
                                normal,
                            );
                        }
                        Cell::Uptime => {
                            clipped.text(
                                text_at,
                                Align2::LEFT_CENTER,
                                text,
                                font.clone(),
                                data_color,
                            );
                        }
                        Cell::Updated => {
                            clipped.text(text_at, Align2::LEFT_CENTER, text, small.clone(), weak);
                        }
                        Cell::Trend => {
                            if !host.trend.is_empty() {
                                let area = Rect::from_min_max(
                                    Pos2::new(cell_rect.left(), rect.top() + 3.0),
                                    Pos2::new(cell_rect.right(), rect.bottom() - 3.0),
                                );
                                sparkline(&clipped, area, host, settings, faint);
                            }
                        }
                    }
                }
                if response.double_clicked() {
                    request = Some(DashboardRequest::Open(host.id));
                } else if response.clicked() || response.secondary_clicked() {
                    *selected = Some(host.id);
                }
                response.context_menu(|ui| {
                    if ui.button(tr("💻 Abrir su terminal")).clicked() {
                        request = Some(DashboardRequest::Open(host.id));
                        ui.close();
                    }
                    let retryable = matches!(host.link, Link::Down(_)) || host.link.needs_user();
                    if ui
                        .add_enabled(retryable, egui::Button::new(tr("🔁 Reintentar ahora")))
                        .on_hover_text(tr(
                            "Vuelve a conectar ya, preguntando la contraseña si hace falta",
                        ))
                        .clicked()
                    {
                        request = Some(DashboardRequest::Retry(host.id));
                        ui.close();
                    }
                    if ui.button(tr("Copiar lo que se sabe de él")).clicked() {
                        ui.ctx().copy_text(details(host));
                        ui.close();
                    }
                });
                response.on_hover_ui_at_pointer(|ui| {
                    ui.set_max_width(460.0);
                    ui.label(details(host));
                });
            }
        });
    request
}

/// Historial breve de alertas, de la más reciente a la más antigua.
/// Devuelve `true` si se pide vaciarlo.
fn history(ui: &mut egui::Ui, state: &State, shown: &mut bool) -> bool {
    let mut clear = false;
    ui.horizontal(|ui| {
        let active: usize = state.hosts.iter().map(|h| h.alerts.active().count()).sum();
        let title = match active {
            0 => tr("🔔 Alertas: ninguna activa").to_string(),
            1 => tr("🔔 Alertas: 1 activa").to_string(),
            n => trf!("🔔 Alertas: {n} activas", n),
        };
        ui.toggle_value(shown, title)
            .on_hover_text(tr("Mostrar u ocultar el historial de alertas"));
        if *shown {
            ui.weak(trf!("· historial ({n})", n = state.history.len()));
            if !state.history.is_empty() && ui.small_button(tr("Vaciar")).clicked() {
                clear = true;
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(4.0);
            summary(ui, state);
        });
    });
    if !*shown {
        return clear;
    }
    egui::ScrollArea::vertical()
        .id_salt("dashboard_history")
        .max_height(80.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if state.history.is_empty() {
                ui.weak(tr(
                    "Aquí se anota cada alerta que salta y cuándo se resuelve.",
                ));
            }
            for event in state.history.iter().rev() {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(
                        RichText::new(event.at.format("%H:%M:%S").to_string())
                            .weak()
                            .monospace(),
                    );
                    let (color, verb) = if event.raised {
                        (error_color(), tr("alerta"))
                    } else {
                        (ok_color(), tr("resuelto"))
                    };
                    super::status_dot(ui, color);
                    ui.label(RichText::new(&event.name).strong());
                    ui.colored_label(color, verb);
                    ui.add(egui::Label::new(&event.detail).truncate());
                });
            }
        });
    clear
}

impl super::App {
    /// Abre el panel de estado de una carpeta (`None`: todos los equipos).
    /// Si ya había uno abierto, lo sustituye y se cierran sus conexiones.
    pub(super) fn open_dashboard(&mut self, folder: Option<Uuid>) {
        self.dashboard_window = Some(DashboardWindow::new(folder));
    }

    /// Reúne los equipos de la carpeta y arranca el panel.
    fn start_dashboard(&mut self, window: &mut DashboardWindow) {
        let mut jobs = vec![];
        let mut problem = None;
        for id in window.hosts(&self.tree) {
            let Some(session) = self.tree.resolved_session(id) else {
                continue;
            };
            match self.endpoint_for(&session) {
                Ok(endpoint) => jobs.push(HostJob {
                    id,
                    name: session.display_name(),
                    target: session.target(),
                    endpoint,
                }),
                Err(e) => problem = Some(format!("{}: {e}", session.display_name())),
            }
        }
        if let Some(problem) = problem {
            self.notify(problem, true);
        }
        let options = Options {
            retryable: super::worth_retrying,
            ..Default::default()
        };
        window.view.order = None;
        window.view.selected = None;
        window.news.clear();
        window.news_since = None;
        window.started_connections = self.settings.dashboard.max_connections;
        // (El anterior, si lo había, se suelta aquí: cierra sus conexiones.)
        window.run = Some(Dashboard::start(
            self.rt.handle(),
            jobs,
            self.prompter.clone(),
            &self.settings.dashboard,
            options,
        ));
    }

    pub(super) fn dashboard_ui(&mut self, ctx: &egui::Context) {
        let Some(mut window) = self.dashboard_window.take() else {
            return;
        };
        if let Some(run) = &window.run {
            // Lo ocurrido en segundo plano desde el fotograma anterior.
            let mut secrets = run.take_secrets();
            secrets.retain(|(key, _)| {
                key.split('/')
                    .next()
                    .and_then(|id| Uuid::parse_str(id).ok())
                    .is_some_and(|id| self.tree.find(id).is_some())
            });
            self.store_secrets(secrets);
            let news = run.take_news();
            if !news.is_empty() {
                window.news.extend(news);
                window.news_since.get_or_insert_with(Instant::now);
            }
            // Con el baúl bloqueado, el panel tampoco se queda sus secretos…
            if window.vault_open && self.vault.is_none() {
                run.forget_credentials();
            }
            // …y al abrirlo, los equipos que esperaban una credencial vuelven
            // a intentarlo con lo que haya en él, sin preguntar nada.
            if !window.vault_open && self.vault.is_some() {
                let waiting: Vec<Uuid> = run
                    .state()
                    .hosts
                    .iter()
                    .filter(|h| h.link == Link::NeedsCredential)
                    .map(|h| h.id)
                    .collect();
                for id in waiting {
                    let endpoint = self
                        .tree
                        .resolved_session(id)
                        .and_then(|session| self.endpoint_for(&session).ok());
                    run.retry(id, endpoint, false);
                }
            }
        }
        window.vault_open = self.vault.is_some();
        if let Some(since) = window.news_since {
            let waited = since.elapsed();
            if waited >= NEWS_DELAY {
                window.news_since = None;
                if let Some((text, bad)) = summarize(&std::mem::take(&mut window.news)) {
                    self.notify(text, bad);
                }
            } else {
                // Un único repintado, para cuando toque avisar.
                ctx.request_repaint_after(NEWS_DELAY - waited);
            }
        }
        let (open, changed, request) = window.show(ctx, &self.tree, &mut self.settings.dashboard);
        if changed {
            self.settings.dashboard.sanitize();
            if let Some(run) = &window.run {
                run.configure(&self.settings.dashboard);
            }
            if let Err(e) = self.settings.save(&crate::paths::settings_file()) {
                self.notify(
                    trf!(
                        "No se pudieron guardar las preferencias: {error}",
                        error = format!("{e:#}")
                    ),
                    true,
                );
            }
        }
        match request {
            DashboardRequest::None => {}
            DashboardRequest::Start => self.start_dashboard(&mut window),
            DashboardRequest::Open(id) => {
                if let Some(session) = self.tree.resolved_session(id) {
                    self.selected = Some(id);
                    self.connect(session, None);
                }
            }
            DashboardRequest::Retry(id) => {
                // Con lo que el baúl tenga ahora: quizá ya está lo que faltaba.
                let endpoint = self
                    .tree
                    .resolved_session(id)
                    .and_then(|session| self.endpoint_for(&session).ok());
                if let Some(run) = &window.run {
                    run.retry(id, endpoint, true);
                }
            }
        }
        if open {
            self.dashboard_window = Some(window);
        } else {
            self.focus_active_terminal();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_share_the_width() {
        let columns = layout(10.0, 1000.0);
        assert_eq!(columns.len(), Cell::ALL.len());
        assert_eq!(columns[0].2, 10.0);
        // Sin huecos ni solapes, y hasta el borde.
        for pair in columns.windows(2) {
            assert_eq!(pair[0].3, pair[1].2);
        }
        assert_eq!(columns.last().unwrap().3, 1010.0);
        // La del nombre no desaparece en una ventana estrecha.
        let narrow = layout(0.0, 300.0);
        assert_eq!(narrow[0].3 - narrow[0].2, 130.0);
        // Todas se pueden ordenar menos la gráfica.
        let unsortable: Vec<Cell> = Cell::ALL
            .iter()
            .map(|(cell, ..)| *cell)
            .filter(|cell| cell.sorts_by().is_none())
            .collect();
        assert_eq!(unsortable, [Cell::Trend]);
    }

    #[test]
    fn a_host_without_some_data_leaves_those_cells_empty() {
        use crate::monitor::{Disk, Stats};
        let mut host = HostRow::new(Uuid::new_v4(), "router".into(), "root@10.0.0.1:22".into());
        let texts = |host: &HostRow| -> Vec<String> {
            Cell::ALL
                .iter()
                .map(|(cell, ..)| cell_text(host, *cell))
                .collect()
        };
        // Aún sin conectar: solo el nombre y el estado.
        assert_eq!(
            texts(&host),
            ["router", "en cola", "", "", "", "", "", "", "", ""]
        );
        // Un BusyBox recortado u otro Unix: lo que da se muestra y lo demás
        // queda vacío.
        host.link = Link::Online;
        host.latency = Some(Duration::from_millis(7));
        host.stats = Some(Stats {
            disks: vec![Disk {
                mount: "/".into(),
                filesystem: "/dev/root".into(),
                total: 1000,
                used: 350,
                available: 650,
                kind: String::new(),
                read_only: false,
            }],
            ..Default::default()
        });
        assert_eq!(
            texts(&host),
            [
                "router",
                "en línea",
                "7 ms",
                "",
                "",
                "",
                "35 % /",
                "",
                "",
                ""
            ]
        );
        assert!(details(&host).contains("Disco /: 35 %"));
        // Con todo: cada dato en su celda.
        host.stats = Some(Stats {
            cpu: Some(37.4),
            cores: Some(4),
            load: Some([1.5, 1.0, 0.5]),
            memory: Some((250, 1000)),
            uptime_secs: Some(90_000),
            ..host.stats.clone().unwrap()
        });
        assert_eq!(
            texts(&host)[2..8],
            ["7 ms", "37 %", "1,50", "25 %", "35 % /", "1 d 1 h"]
        );
        // Sin datos y con el motivo, se dice.
        host.stats = None;
        host.probe_error = Some("hace falta una shell POSIX".into());
        assert_eq!(texts(&host)[1], "sin datos");
        assert!(details(&host).contains("sin datos: hace falta una shell POSIX"));
    }

    #[test]
    fn readable_latency() {
        assert_eq!(latency_text(None), "");
        assert_eq!(latency_text(Some(Duration::from_micros(300))), "<1 ms");
        assert_eq!(latency_text(Some(Duration::from_millis(42))), "42 ms");
        assert_eq!(latency_text(Some(Duration::from_millis(1250))), "1,2 s");
    }

    #[test]
    fn trend_lines_break_where_there_was_no_data() {
        let now = Instant::now();
        let ago = |secs: u64, cpu: Option<f32>, load: Option<f32>| TrendPoint {
            at: now - Duration::from_secs(secs),
            cpu,
            load,
        };
        let points = [
            // Fuera de la ventana: no se dibuja.
            ago(700, Some(10.0), Some(1.0)),
            ago(100, Some(0.0), Some(1.0)),
            ago(90, Some(50.0), None),
            ago(80, Some(100.0), Some(4.0)),
            // Medio minuto sin datos: otro tramo.
            ago(40, Some(250.0), Some(2.0)),
            ago(30, None, Some(2.0)),
            ago(20, Some(25.0), Some(2.0)),
        ];
        let rect = Rect::from_min_max(Pos2::new(100.0, 10.0), Pos2::new(200.0, 30.0));
        let window = Duration::from_secs(600);
        let gap = Duration::from_secs(25);
        let lines = trend_lines(
            points.iter(),
            TrendMetric::Cpu,
            rect,
            now,
            window,
            gap,
            100.0,
        );
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0].len(), 3);
        // De abajo (0 %) a arriba (100 %), y avanzando hacia la derecha.
        assert_eq!(lines[0][0].y, 30.0);
        assert_eq!(lines[0][1].y, 20.0);
        assert_eq!(lines[0][2].y, 10.0);
        assert!(lines[0][0].x < lines[0][1].x && lines[0][1].x < lines[0][2].x);
        // Un valor imposible no se sale del recuadro; el punto sin dato se
        // salta sin partir la línea.
        assert_eq!(lines[1].len(), 2);
        assert_eq!(lines[1][0].y, 10.0);
        assert_eq!(lines[1][1].y, 25.0);
        for pos in lines.iter().flatten() {
            assert!(rect.contains(*pos), "{pos:?}");
        }
        // La carga tiene sus propios huecos.
        let load = trend_lines(
            points.iter(),
            TrendMetric::Load,
            rect,
            now,
            window,
            gap,
            4.0,
        );
        assert_eq!(
            load.iter().map(Vec::len).collect::<Vec<_>>(),
            [2, 3],
            "{load:?}"
        );
        let nothing = std::iter::empty::<&TrendPoint>();
        assert!(trend_lines(nothing, TrendMetric::Cpu, rect, now, window, gap, 100.0).is_empty());
    }
}
