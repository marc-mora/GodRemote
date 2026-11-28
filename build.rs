//! Anota en el programa qué versión de la librería SSH (russh) lleva esta
//! compilación, leyéndola del `Cargo.lock` con el que se compila. La ventana
//! de ajustes la muestra, porque depende de la edición (véase `reforzado/`).

use std::path::Path;

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let lock = Path::new(&dir).join("Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock.display());
    let text = std::fs::read_to_string(&lock).unwrap_or_default();
    // [[package]] / name = "russh" / version = "0.65.1"
    let version = text
        .split("[[package]]")
        .find(|block| block.lines().any(|l| l.trim() == "name = \"russh\""))
        .and_then(|block| {
            block
                .lines()
                .find_map(|l| l.trim().strip_prefix("version = \""))
                .map(|v| v.trim_end_matches('"').to_string())
        })
        .unwrap_or_else(|| "desconocida".to_string());
    println!("cargo:rustc-env=GODREMOTE_RUSSH_VERSION={version}");

    windows_resources(Path::new(&dir));
}

/// En Windows, el icono y los datos del ejecutable (lo que enseña el
/// Explorador). Si faltan las herramientas para compilar recursos, se avisa
/// y el ejecutable sale sin ellos.
fn windows_resources(manifest_dir: &Path) {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // El manifiesto de la edición reforzada vive en una subcarpeta.
    let Some(icon) = [manifest_dir.join("assets"), manifest_dir.join("../assets")]
        .into_iter()
        .map(|assets| assets.join("icono.ico"))
        .find(|icon| icon.exists())
    else {
        println!("cargo:warning=no se encuentra assets/icono.ico");
        return;
    };
    println!("cargo:rerun-if-changed={}", icon.display());
    let mut resources = winresource::WindowsResource::new();
    resources
        .set_icon(&icon.to_string_lossy())
        .set("ProductName", "GodRemote")
        .set("FileDescription", "GodRemote")
        .set("LegalCopyright", "Licencia MIT");
    if let Err(error) = resources.compile() {
        println!("cargo:warning=el ejecutable sale sin icono: {error}");
    }
}
