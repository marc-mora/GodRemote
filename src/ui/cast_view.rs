//! Reproductor de grabaciones de terminal (`.cast`): una ventana con el
//! terminal grabado, reproducir y pausar, velocidad, salto de silencios y
//! una barra de posición para ir a cualquier punto.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::cast::{Cast, Player, SPEEDS, Step, clock_text};
use crate::i18n::{Lang, tr};
use crate::terminal::TerminalView;
use crate::theme::warn_color;
use crate::trf;

/// Salida que como mucho se aplica al terminal en un fotograma: un salto
/// largo se reparte entre varios para no dejar la interfaz parada.
const FRAME_BUDGET: usize = 256 * 1024;
/// Duración máxima de un silencio cuando se saltan los largos, en segundos.
const IDLE_LIMIT: f64 = 2.0;
/// Segundos que se salta con las flechas.
const JUMP: f64 = 5.0;

pub struct PlayerWindow {
    id: u64,
    path: PathBuf,
    name: String,
    player: Player,
    view: TerminalView,
    /// Cuándo se miró el reloj por última vez.
    last_tick: Option<Instant>,
    /// Silencio máximo al saltar los largos (el que recomiende la grabación).
    idle_limit: f64,
    /// Hay que vaciar el terminal antes de seguir (se ha vuelto atrás).
    reset: bool,
    /// Quedan eventos atrasados por aplicar.
    catching_up: bool,
}

/// Un número con los decimales justos y la coma (o el punto) del idioma en
/// uso.
fn number(value: impl std::fmt::Display) -> String {
    let text = value.to_string();
    match crate::i18n::current() {
        Lang::Es => text.replace('.', ","),
        Lang::En => text,
    }
}

fn speed_label(speed: f32) -> String {
    if speed.fract() == 0.0 {
        format!("{speed:.0}×")
    } else {
        format!("{}×", number(speed))
    }
}

impl PlayerWindow {
    /// Abre una grabación y empieza a reproducirla.
    pub fn open(path: &Path, id: u64) -> anyhow::Result<Self> {
        let cast = Cast::load(path)?;
        let view = TerminalView::viewer(cast.cols, cast.rows);
        // Si la grabación recomienda recortar los silencios, se le hace caso.
        let recommended = cast.idle_limit;
        let idle_limit = recommended.unwrap_or(IDLE_LIMIT);
        let mut player = Player::new(cast);
        player.set_idle_limit(recommended);
        player.playing = true;
        Ok(Self {
            id,
            path: path.to_path_buf(),
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string()),
            player,
            view,
            last_tick: None,
            idle_limit,
            reset: false,
            catching_up: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn seek(&mut self, position: f64) {
        self.reset |= self.player.seek(position);
    }

    fn toggle(&mut self) {
        if !self.player.playing && self.player.position() >= self.player.duration() {
            // Terminó: reproducir es empezar otra vez.
            self.seek(0.0);
        }
        self.player.playing = !self.player.playing;
    }

    /// Pone en el terminal lo que toca hasta la posición actual.
    fn apply(&mut self) {
        if std::mem::take(&mut self.reset) {
            let (cols, rows) = (self.player.cast().cols, self.player.cast().rows);
            self.view.viewer_reset(cols, rows);
        }
        let view = &mut self.view;
        self.catching_up = self.player.drain(FRAME_BUDGET, |step| match step {
            Step::Output(bytes) => view.viewer_feed(bytes),
            Step::Resize(cols, rows) => view.viewer_resize(cols, rows),
        });
    }

    /// Dibuja la ventana. Devuelve `false` cuando se cierra.
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        // El reloj corre aunque la ventana esté plegada; un parón de la
        // aplicación no se convierte en un salto.
        let now = Instant::now();
        if let Some(last) = self.last_tick.replace(now) {
            self.player
                .advance(now.duration_since(last).as_secs_f64().min(1.0));
        }
        let mut open = true;
        egui::Window::new(format!("▶ {}", self.name))
            .id(egui::Id::unique(("cast_player", self.id)))
            .open(&mut open)
            .collapsible(true)
            .resizable(true)
            .default_pos([
                270.0 + 24.0 * (self.id % 8) as f32,
                70.0 + 24.0 * (self.id % 8) as f32,
            ])
            .default_size([900.0, 580.0])
            .min_size([440.0, 220.0])
            .show(ctx, |ui| {
                self.controls(ui);
                self.info(ui);
                ui.separator();
                self.apply();
                let area = ui.available_rect_before_wrap();
                ui.painter()
                    .rect_filled(area, 0.0, crate::theme::terminal_palette().background);
                self.view.ui(ui);
                // El resto del hueco es de la ventana: así conserva el
                // tamaño que le dé el usuario. Un clic en él también deja
                // el teclado en el reproductor.
                let rest = ui.allocate_response(ui.available_size(), egui::Sense::click());
                if rest.clicked() {
                    self.view.request_focus();
                }
                if self.view.has_focus {
                    self.keys(ui.ctx());
                }
            });
        if self.catching_up {
            ctx.request_repaint();
        } else if self.player.playing {
            // Hasta el próximo evento, pero sin dejar parados el reloj y la
            // barra de posición.
            let wait = self.player.next_event_in().unwrap_or(0.1).clamp(0.016, 0.1);
            ctx.request_repaint_after(Duration::from_secs_f64(wait));
        }
        open
    }

    /// Teclas del reproductor, con el foco en su terminal.
    fn keys(&mut self, ctx: &egui::Context) {
        let (space, left, right, home) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::Home),
            )
        });
        if space {
            self.toggle();
        }
        if left {
            self.seek(self.player.position() - JUMP);
        }
        if right {
            self.seek(self.player.position() + JUMP);
        }
        if home {
            self.seek(0.0);
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (label, hover) = if self.player.playing {
                ("⏸", tr("Pausa (Espacio)"))
            } else {
                ("▶", tr("Reproducir (Espacio)"))
            };
            if ui
                .add_sized([30.0, 20.0], egui::Button::new(label))
                .on_hover_text(hover)
                .clicked()
            {
                self.toggle();
            }
            if ui
                .button("⏮")
                .on_hover_text(tr("Volver al principio (Inicio)"))
                .clicked()
            {
                self.seek(0.0);
            }
            let duration = self.player.duration();
            let mut position = self.player.position();
            ui.label(
                egui::RichText::new(format!(
                    "{} / {}",
                    clock_text(position),
                    clock_text(duration)
                ))
                .monospace(),
            );
            // La barra de posición ocupa lo que dejen los demás controles.
            ui.spacing_mut().slider_width = (ui.available_width() - 250.0).max(80.0);
            let slider = ui
                .add_enabled(
                    duration > 0.0,
                    egui::Slider::new(&mut position, 0.0..=duration.max(0.001)).show_value(false),
                )
                .on_hover_text(tr(
                    "Posición: arrastra para ir a cualquier punto (flechas: ±5 s)",
                ));
            if slider.changed() {
                self.seek(position);
            }
            egui::ComboBox::from_id_salt(("cast_speed", self.id))
                .selected_text(speed_label(self.player.speed))
                .width(54.0)
                .show_ui(ui, |ui| {
                    for speed in SPEEDS {
                        ui.selectable_value(&mut self.player.speed, speed, speed_label(speed));
                    }
                })
                .response
                .on_hover_text(tr("Velocidad"));
            let mut skip = self.player.idle_limit().is_some();
            if ui
                .checkbox(&mut skip, tr("Saltar silencios"))
                .on_hover_text(trf!(
                    "Ningún silencio dura más de {segundos} s",
                    segundos = number(self.idle_limit)
                ))
                .changed()
            {
                self.player.set_idle_limit(skip.then_some(self.idle_limit));
            }
        });
    }

    /// Datos de la grabación y avisos sobre ella.
    fn info(&self, ui: &mut egui::Ui) {
        let cast = self.player.cast();
        ui.horizontal(|ui| {
            let mut parts = vec![];
            if !cast.title.is_empty() {
                parts.push(cast.title.clone());
            }
            if let Some(when) = cast
                .timestamp
                .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
            {
                parts.push(
                    when.with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M")
                        .to_string(),
                );
            }
            let (cols, rows) = self.view.size();
            parts.push(format!("{cols}×{rows}"));
            parts.push(format!("asciicast v{}", cast.version));
            ui.weak(parts.join(" · "));
            if cast.event_count() == 0 {
                ui.colored_label(warn_color(), tr("La grabación está vacía"));
            }
            if cast.skipped > 0 {
                ui.colored_label(
                    warn_color(),
                    match cast.skipped {
                        1 => tr("⚠ 1 línea ilegible se ha saltado").to_string(),
                        n => trf!("⚠ {n} líneas ilegibles se han saltado", n),
                    },
                );
            }
            if self.catching_up {
                ui.weak(tr("⌛ avanzando…"));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speeds_are_labelled_the_spanish_way() {
        let labels: Vec<String> = SPEEDS.iter().map(|s| speed_label(*s)).collect();
        assert_eq!(labels, ["0,5×", "1×", "2×", "4×", "8×"]);
    }

    #[test]
    fn a_recording_plays_into_its_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.cast");
        std::fs::write(
            &path,
            concat!(
                "{\"version\":2,\"width\":40,\"height\":10,\"title\":\"Demo\"}\n",
                "[0.5, \"o\", \"$ echo hola\\r\\n\"]\n",
                "[1.0, \"o\", \"hola\\r\\n$ \"]\n",
                "[2.0, \"r\", \"60x12\"]\n",
                "[30.0, \"o\", \"\\u001b[6nfin\"]\n",
            ),
        )
        .unwrap();
        let mut window = PlayerWindow::open(&path, 1).unwrap();
        assert_eq!(window.name, "demo.cast");
        assert_eq!(window.view.size(), (40, 10));
        assert!(window.player.playing);
        // Un segundo: lo grabado hasta ahí.
        window.player.advance(1.0);
        window.apply();
        assert_eq!(window.view.contents().trim_end(), "$ echo hola\nhola\n$");
        // Al final: el cambio de tamaño y el resto.
        window.seek(30.0);
        window.apply();
        assert_eq!(window.view.size(), (60, 12));
        assert!(window.view.contents().contains("$ fin"));
        // Atrás: se vacía el terminal, vuelve a su tamaño y se rehace.
        window.seek(0.7);
        assert!(window.reset);
        window.apply();
        assert_eq!(window.view.size(), (40, 10));
        assert_eq!(window.view.contents().trim_end(), "$ echo hola");
        // Al terminar se para, y reproducir otra vez empieza de cero.
        window.seek(1e9);
        window.apply();
        assert!(!window.player.playing);
        window.toggle();
        assert!(window.player.playing && window.player.position() == 0.0);
        window.apply();
        assert_eq!(window.view.contents().trim(), "");
        // Un fichero que no es una grabación no abre ventana.
        std::fs::write(&path, "no es una grabación").unwrap();
        assert!(PlayerWindow::open(&path, 2).is_err());
    }
}
