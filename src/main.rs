#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> eframe::Result {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .thread_name("godremote-net")
        .build()
        .expect("no se pudo crear el runtime de red");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("GodRemote")
            .with_app_id("godremote")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([760.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "GodRemote",
        options,
        Box::new(move |cc| Ok(Box::new(godremote::ui::App::new(cc, runtime)))),
    )
}
