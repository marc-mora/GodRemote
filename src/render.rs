//! Con qué se pinta la ventana. En Windows, con DirectX (wgpu): siempre hay
//! al menos su modo por software, mientras que OpenGL falta en máquinas
//! virtuales, servidores sin tarjeta gráfica y algunas sesiones de
//! escritorio remoto. En el resto de sistemas, con OpenGL (glow).
//!
//! Si el primero no arranca se prueba el siguiente, y la variable de entorno
//! `GODREMOTE_RENDER` (`directx` u `opengl`) permite elegir a mano.

/// Variable de entorno para elegir el motor de dibujo a mano.
pub const ENV: &str = "GODREMOTE_RENDER";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// OpenGL 2.0 o posterior.
    OpenGl,
    /// DirectX 12 a través de wgpu. Solo existe en Windows.
    DirectX,
}

impl Backend {
    /// ¿Se ha compilado este motor en este sistema?
    pub fn available(self) -> bool {
        match self {
            Backend::OpenGl => true,
            Backend::DirectX => cfg!(windows),
        }
    }

    /// Nombre con el que se pide en [`ENV`].
    pub fn key(self) -> &'static str {
        match self {
            Backend::OpenGl => "opengl",
            Backend::DirectX => "directx",
        }
    }

    /// Nombre para enseñar al usuario.
    pub fn label(self) -> &'static str {
        match self {
            Backend::OpenGl => "OpenGL",
            Backend::DirectX => "DirectX 12",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "opengl" | "gl" | "glow" => Some(Backend::OpenGl),
            "directx" | "dx12" | "d3d12" | "wgpu" => Some(Backend::DirectX),
            _ => None,
        }
    }
}

/// Motores que se probarán, por orden. `requested` es el valor de [`ENV`]:
/// uno o varios nombres separados por comas; lo que no se entienda o no
/// exista en este sistema se ignora, y si no queda nada vale el orden de
/// fábrica.
pub fn plan(requested: Option<&str>) -> Vec<Backend> {
    let mut chosen: Vec<Backend> = Vec::new();
    for backend in requested
        .unwrap_or_default()
        .split(',')
        .filter_map(Backend::parse)
        .filter(|b| b.available())
    {
        if !chosen.contains(&backend) {
            chosen.push(backend);
        }
    }
    if chosen.is_empty() {
        chosen = [Backend::DirectX, Backend::OpenGl]
            .into_iter()
            .filter(|b| b.available())
            .collect();
    }
    chosen
}

/// Valor de [`ENV`] para un nuevo intento con los motores que quedan.
pub fn remaining(plan: &[Backend]) -> String {
    plan.iter().map(|b| b.key()).collect::<Vec<_>>().join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_follows_the_request_and_the_platform() {
        // De fábrica: DirectX y luego OpenGL en Windows; OpenGL en el resto.
        let default = plan(None);
        if cfg!(windows) {
            assert_eq!(default, [Backend::DirectX, Backend::OpenGl]);
        } else {
            assert_eq!(default, [Backend::OpenGl]);
        }
        assert_eq!(plan(Some("")), default);
        assert_eq!(plan(Some("no-existe")), default);
        // Lo que se pide a mano, en su orden y sin repetir.
        assert_eq!(plan(Some("opengl")), [Backend::OpenGl]);
        assert_eq!(plan(Some(" GL , opengl ")), [Backend::OpenGl]);
        if cfg!(windows) {
            assert_eq!(
                plan(Some("opengl,directx")),
                [Backend::OpenGl, Backend::DirectX]
            );
            assert_eq!(plan(Some("wgpu")), [Backend::DirectX]);
        } else {
            // Lo que no existe en este sistema se ignora.
            assert_eq!(plan(Some("directx")), default);
            assert_eq!(plan(Some("directx,opengl")), [Backend::OpenGl]);
        }
        // Y lo que queda por probar se vuelve a leer igual.
        for chain in [vec![Backend::OpenGl], default] {
            assert_eq!(plan(Some(&remaining(&chain))), chain);
        }
    }
}
