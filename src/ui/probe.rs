//! Prueba de arranque: con la variable de entorno `GODREMOTE_CAPTURA=<fichero.png>`
//! la aplicación pinta unos fotogramas, guarda una captura de su ventana en
//! ese fichero y se cierra sola. La usan las pruebas automáticas para
//! comprobar que el ejecutable arranca y dibuja en cada sistema; el código de
//! salida dice cómo ha ido (0 bien; 2 no se pudo guardar; 3 la captura no
//! llegó; 4 la aplicación no se cerró; 5 la ventana salió vacía).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Variable de entorno que activa la prueba.
pub const ENV: &str = "GODREMOTE_CAPTURA";

/// Fotogramas que se dejan pasar antes de capturar (fuentes, tema, ventana).
const SETTLE_FRAMES: u32 = 8;
/// Lo que se espera como mucho a que llegue la captura.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(30);
/// Lo que se espera como mucho a que la aplicación se cierre después.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(15);

type Outcome = Arc<Mutex<Option<Result<String, (i32, String)>>>>;

pub struct StartupProbe {
    path: PathBuf,
    frames: u32,
    started: Instant,
    requested: bool,
    outcome: Outcome,
    closing: Option<Instant>,
}

impl StartupProbe {
    /// La prueba, si se ha pedido con la variable de entorno.
    pub fn from_env() -> Option<Self> {
        let path = std::env::var_os(ENV).filter(|p| !p.is_empty())?;
        Some(Self {
            path: PathBuf::from(path),
            frames: 0,
            started: Instant::now(),
            requested: false,
            outcome: Arc::default(),
            closing: None,
        })
    }

    /// Se llama en cada fotograma.
    pub fn tick(&mut self, ctx: &egui::Context) {
        // Sin esperar a que pase nada: la prueba va sola.
        ctx.request_repaint();
        self.frames += 1;
        if let Some(since) = self.closing {
            if since.elapsed() > CLOSE_TIMEOUT {
                fail(4, "la aplicación no se cerró al pedírselo");
            }
            return;
        }
        if self.frames >= SETTLE_FRAMES && !self.requested {
            self.requested = true;
            let (path, outcome, wake) = (self.path.clone(), self.outcome.clone(), ctx.clone());
            ctx.request_screenshot(move |image| {
                *outcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(save(&path, &image));
                wake.request_repaint();
            });
        }
        let outcome = self
            .outcome
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        match outcome {
            Some(Ok(summary)) => {
                println!("{summary}");
                // Un cierre ordenado, como el del usuario: también se prueba.
                self.closing = Some(Instant::now());
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Some(Err((code, message))) => fail(code, &message),
            None if self.started.elapsed() > CAPTURE_TIMEOUT => {
                fail(3, "la captura de la ventana no llegó a tiempo");
            }
            None => {}
        }
    }
}

fn fail(code: i32, message: &str) -> ! {
    eprintln!("Prueba de arranque fallida: {message}");
    std::process::exit(code)
}

/// Guarda la captura y comprueba que no es una ventana en blanco.
fn save(path: &Path, image: &egui::ColorImage) -> Result<String, (i32, String)> {
    let [width, height] = image.size;
    let colors = distinct_colors(image.as_raw());
    if width == 0 || height == 0 || colors < 8 {
        return Err((
            5,
            format!("la ventana se pintó vacía ({width}×{height}, {colors} colores)"),
        ));
    }
    image::save_buffer(
        path,
        image.as_raw(),
        width as u32,
        height as u32,
        image::ColorType::Rgba8,
    )
    .map_err(|e| (2, format!("no se pudo guardar {}: {e}", path.display())))?;
    Ok(format!(
        "Captura de arranque guardada en {} ({width}×{height}, {colors} colores)",
        path.display()
    ))
}

/// Cuántos colores distintos hay (como mucho se cuentan 64): una interfaz
/// pintada tiene muchos; una ventana vacía, uno.
fn distinct_colors(rgba: &[u8]) -> usize {
    let mut seen: Vec<[u8; 4]> = Vec::new();
    // Una muestra basta, y evita recorrer millones de píxeles.
    for pixel in rgba.chunks_exact(4).step_by(7) {
        let pixel = [pixel[0], pixel[1], pixel[2], pixel[3]];
        if !seen.contains(&pixel) {
            seen.push(pixel);
            if seen.len() >= 64 {
                break;
            }
        }
    }
    seen.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_windows_are_told_apart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("captura.png");
        // Un solo color: no se ha pintado nada.
        let blank = egui::ColorImage::filled([64, 48], egui::Color32::from_gray(27));
        let (code, _) = save(&path, &blank).unwrap_err();
        assert_eq!(code, 5);
        assert!(!path.exists());
        // Con contenido, se guarda un PNG del mismo tamaño.
        let mut painted = blank.clone();
        for (i, pixel) in painted.pixels.iter_mut().enumerate() {
            *pixel = egui::Color32::from_rgb((i % 251) as u8, (i % 13) as u8, 40);
        }
        let summary = save(&path, &painted).unwrap();
        assert!(summary.contains("64×48"), "{summary}");
        let saved = image::open(&path).unwrap();
        assert_eq!((saved.width(), saved.height()), (64, 48));
        // Y si no se puede escribir, se dice (sin confundirlo con lo anterior).
        let (code, _) = save(&dir.path().join("no/existe/captura.png"), &painted).unwrap_err();
        assert_eq!(code, 2);
    }
}
