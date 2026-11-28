#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::process::ExitCode;

use godremote::render::{self, Backend};

/// Errores de los motores de dibujo que ya se han probado (entre procesos).
const ERRORS_ENV: &str = "GODREMOTE_RENDER_ERRORES";

fn main() -> ExitCode {
    // El idioma, antes que nada: lo que se enseñe antes del primer fotograma
    // (también el aviso de que no se puede abrir la ventana) ya sale en él.
    let settings = godremote::theme::Settings::load(&godremote::paths::settings_file());
    godremote::i18n::set(godremote::i18n::resolve(settings.language));

    let plan = render::plan(std::env::var(render::ENV).ok().as_deref());
    let backend = plan[0];
    let error = match run(backend) {
        Ok(()) => return ExitCode::SUCCESS,
        Err(error) => format!("{}: {error}", backend.label()),
    };
    eprintln!("No se pudo abrir la ventana con {error}");
    let mut errors = std::env::var(ERRORS_ENV).unwrap_or_default();
    if !errors.is_empty() {
        errors.push('\n');
    }
    errors.push_str(&error);

    // La ventana solo se puede intentar crear una vez por proceso: el
    // siguiente motor se prueba en un proceso nuevo, y este espera por él.
    let rest = &plan[1..];
    if !rest.is_empty()
        && let Ok(program) = std::env::current_exe()
    {
        let status = std::process::Command::new(program)
            .args(std::env::args_os().skip(1))
            .env(render::ENV, render::remaining(rest))
            .env(ERRORS_ENV, &errors)
            .status();
        if let Ok(status) = status {
            return ExitCode::from(status.code().unwrap_or(1).clamp(0, 255) as u8);
        }
    }

    // Nada ha funcionado: que al menos se vea por qué (el ejecutable de
    // Windows no tiene consola).
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("GodRemote")
        .set_description(godremote::trf!(
            "GodRemote no ha podido abrir su ventana: este equipo no ofrece \
             ningún modo de dibujo que sirva.\n\n{errors}",
            errors
        ))
        .show();
    ExitCode::FAILURE
}

fn run(backend: Backend) -> eframe::Result {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .thread_name("godremote-net")
        .build()
        .expect("no se pudo crear el runtime de red");

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("GodRemote")
        .with_app_id("godremote")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([760.0, 480.0]);
    // Icono de la ventana (en macOS manda el del paquete de la aplicación).
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icono-256.png")) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: match backend {
            #[cfg(windows)]
            Backend::DirectX => eframe::Renderer::Wgpu,
            _ => eframe::Renderer::Glow,
        },
        ..Default::default()
    };
    eframe::run_native(
        "GodRemote",
        options,
        Box::new(move |cc| Ok(Box::new(godremote::ui::App::new(cc, runtime)))),
    )
}
