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
}
