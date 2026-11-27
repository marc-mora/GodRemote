//! Temas: todos los colores, tamaños de letra y formas de la aplicación.
//!
//! Un tema es un fichero JSON legible. Se puede exportar el tema base,
//! modificarlo con cualquier editor y volver a importarlo, o editarlo desde
//! la propia aplicación.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use anyhow::{Context, Result, anyhow, bail};
use egui::Color32;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Color RGB que se guarda como `"#rrggbb"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub [u8; 3]);

impl Rgb {
    pub const fn hex(value: u32) -> Self {
        Rgb([(value >> 16) as u8, (value >> 8) as u8, value as u8])
    }

    pub fn color32(self) -> Color32 {
        Color32::from_rgb(self.0[0], self.0[1], self.0[2])
    }

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0[0], self.0[1], self.0[2])
    }

    /// Acepta `#rrggbb`, `rrggbb` y la forma corta `#rgb`.
    pub fn parse(text: &str) -> Option<Self> {
        let t = text.trim().trim_start_matches('#');
        if !t.is_ascii() {
            return None;
        }
        match t.len() {
            6 => {
                let v = u32::from_str_radix(t, 16).ok()?;
                Some(Rgb::hex(v))
            }
            3 => {
                let mut out = [0u8; 3];
                for (i, c) in t.chars().enumerate() {
                    let d = c.to_digit(16)? as u8;
                    out[i] = d * 17;
                }
                Some(Rgb(out))
            }
            _ => None,
        }
    }
}

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Rgb::parse(&text).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "color no válido «{text}» (se espera #rrggbb, por ejemplo #1e90ff)"
            ))
        })
    }
}

/// Colores de la interfaz.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiColors {
    /// Fondo de los paneles.
    #[serde(rename = "fondo")]
    pub background: Rgb,
    /// Fondo de ventanas, menús y diálogos.
    #[serde(rename = "fondo_ventana")]
    pub window: Rgb,
    /// Fondo de los campos de texto.
    #[serde(rename = "fondo_campo")]
    pub field: Rgb,
    /// Fondo alterno (filas resaltadas, zonas secundarias).
    #[serde(rename = "fondo_alterno")]
    pub alternate: Rgb,
    #[serde(rename = "texto")]
    pub text: Rgb,
    /// Texto secundario (pistas, detalles).
    #[serde(rename = "texto_tenue")]
    pub text_weak: Rgb,
    /// Texto de títulos y elementos activos.
    #[serde(rename = "texto_destacado")]
    pub text_strong: Rgb,
    /// Color de selección y de la pestaña activa.
    #[serde(rename = "acento")]
    pub accent: Rgb,
    #[serde(rename = "texto_sobre_acento")]
    pub on_accent: Rgb,
    #[serde(rename = "boton")]
    pub button: Rgb,
    #[serde(rename = "boton_encima")]
    pub button_hover: Rgb,
    #[serde(rename = "boton_pulsado")]
    pub button_active: Rgb,
    #[serde(rename = "borde")]
    pub border: Rgb,
    #[serde(rename = "enlace")]
    pub link: Rgb,
    /// Estado correcto (conectado, completado).
    #[serde(rename = "correcto")]
    pub ok: Rgb,
    #[serde(rename = "aviso")]
    pub warn: Rgb,
    #[serde(rename = "error")]
    pub error: Rgb,
}

impl UiColors {
    /// Referencias mutables a todos los colores con su nombre visible.
    pub fn fields_mut(&mut self) -> [(&'static str, &mut Rgb); 17] {
        [
            ("Fondo", &mut self.background),
            ("Fondo de ventanas y menús", &mut self.window),
            ("Fondo de campos de texto", &mut self.field),
            ("Fondo alterno", &mut self.alternate),
            ("Texto", &mut self.text),
            ("Texto tenue", &mut self.text_weak),
            ("Texto destacado", &mut self.text_strong),
            ("Acento (selección)", &mut self.accent),
            ("Texto sobre el acento", &mut self.on_accent),
            ("Botón", &mut self.button),
            ("Botón (ratón encima)", &mut self.button_hover),
            ("Botón (pulsado)", &mut self.button_active),
            ("Bordes", &mut self.border),
            ("Enlaces", &mut self.link),
            ("Correcto", &mut self.ok),
            ("Aviso", &mut self.warn),
            ("Error", &mut self.error),
        ]
    }
}

impl TerminalColors {
    /// Colores básicos del terminal (sin la paleta ANSI).
    pub fn fields_mut(&mut self) -> [(&'static str, &mut Rgb); 5] {
        [
            ("Texto", &mut self.foreground),
            ("Fondo", &mut self.background),
            ("Cursor", &mut self.cursor),
            ("Fondo de la selección", &mut self.selection_bg),
            ("Texto de la selección", &mut self.selection_fg),
        ]
    }
}

/// Los 16 colores ANSI del terminal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AnsiColors {
    #[serde(rename = "negro")]
    pub black: Rgb,
    #[serde(rename = "rojo")]
    pub red: Rgb,
    #[serde(rename = "verde")]
    pub green: Rgb,
    #[serde(rename = "amarillo")]
    pub yellow: Rgb,
    #[serde(rename = "azul")]
    pub blue: Rgb,
    #[serde(rename = "magenta")]
    pub magenta: Rgb,
    #[serde(rename = "cian")]
    pub cyan: Rgb,
    #[serde(rename = "blanco")]
    pub white: Rgb,
    #[serde(rename = "negro_brillante")]
    pub bright_black: Rgb,
    #[serde(rename = "rojo_brillante")]
    pub bright_red: Rgb,
    #[serde(rename = "verde_brillante")]
    pub bright_green: Rgb,
    #[serde(rename = "amarillo_brillante")]
    pub bright_yellow: Rgb,
    #[serde(rename = "azul_brillante")]
    pub bright_blue: Rgb,
    #[serde(rename = "magenta_brillante")]
    pub bright_magenta: Rgb,
    #[serde(rename = "cian_brillante")]
    pub bright_cyan: Rgb,
    #[serde(rename = "blanco_brillante")]
    pub bright_white: Rgb,
}

impl AnsiColors {
    pub fn to_array(&self) -> [Rgb; 16] {
        [
            self.black,
            self.red,
            self.green,
            self.yellow,
            self.blue,
            self.magenta,
            self.cyan,
            self.white,
            self.bright_black,
            self.bright_red,
            self.bright_green,
            self.bright_yellow,
            self.bright_blue,
            self.bright_magenta,
            self.bright_cyan,
            self.bright_white,
        ]
    }

    /// Referencias mutables a los 16 colores con su nombre visible.
    pub fn fields_mut(&mut self) -> [(&'static str, &mut Rgb); 16] {
        [
            ("Negro", &mut self.black),
            ("Rojo", &mut self.red),
            ("Verde", &mut self.green),
            ("Amarillo", &mut self.yellow),
            ("Azul", &mut self.blue),
            ("Magenta", &mut self.magenta),
            ("Cian", &mut self.cyan),
            ("Blanco", &mut self.white),
            ("Negro brillante", &mut self.bright_black),
            ("Rojo brillante", &mut self.bright_red),
            ("Verde brillante", &mut self.bright_green),
            ("Amarillo brillante", &mut self.bright_yellow),
            ("Azul brillante", &mut self.bright_blue),
            ("Magenta brillante", &mut self.bright_magenta),
            ("Cian brillante", &mut self.bright_cyan),
            ("Blanco brillante", &mut self.bright_white),
        ]
    }
}

/// Colores del terminal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalColors {
    #[serde(rename = "texto")]
    pub foreground: Rgb,
    #[serde(rename = "fondo")]
    pub background: Rgb,
    pub cursor: Rgb,
    #[serde(rename = "seleccion_fondo")]
    pub selection_bg: Rgb,
    #[serde(rename = "seleccion_texto")]
    pub selection_fg: Rgb,
    pub ansi: AnsiColors,
}

/// Tamaños de letra, en puntos.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FontSizes {
    #[serde(rename = "interfaz")]
    pub ui: f32,
    pub terminal: f32,
}

/// Forma de los elementos.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Shape {
    /// Radio de las esquinas de botones, campos y ventanas.
    #[serde(rename = "redondeo")]
    pub rounding: f32,
    /// Separación entre elementos.
    #[serde(rename = "espaciado")]
    pub spacing: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    #[serde(rename = "nombre")]
    pub name: String,
    /// `true` para temas de fondo oscuro (afecta a sombras y contrastes).
    #[serde(rename = "oscuro")]
    pub dark: bool,
    #[serde(rename = "interfaz")]
    pub ui: UiColors,
    pub terminal: TerminalColors,
    #[serde(rename = "letra")]
    pub fonts: FontSizes,
    #[serde(rename = "forma")]
    pub shape: Shape,
}

// Los valores que falten en un fichero se toman del tema base.
impl Default for Theme {
    fn default() -> Self {
        Theme::base()
    }
}

impl Default for UiColors {
    fn default() -> Self {
        Theme::base().ui
    }
}

impl Default for TerminalColors {
    fn default() -> Self {
        Theme::base().terminal
    }
}

impl Default for AnsiColors {
    fn default() -> Self {
        Theme::base().terminal.ansi
    }
}

impl Default for FontSizes {
    fn default() -> Self {
        Self {
            ui: 14.0,
            terminal: 14.0,
        }
    }
}

impl Default for Shape {
    fn default() -> Self {
        Self {
            rounding: 4.0,
            spacing: 8.0,
        }
    }
}

pub const BASE_NAME: &str = "GodRemote Oscuro";

impl Theme {
    /// Tema base (oscuro). Es el que se exporta como punto de partida.
    pub fn base() -> Self {
        let h = Rgb::hex;
        Theme {
            name: BASE_NAME.into(),
            dark: true,
            ui: UiColors {
                background: h(0x1b1b1b),
                window: h(0x222222),
                field: h(0x0f0f0f),
                alternate: h(0x262626),
                text: h(0xc4c4c4),
                text_weak: h(0x8a8a8a),
                text_strong: h(0xffffff),
                accent: h(0x005c80),
                on_accent: h(0xffffff),
                button: h(0x3c3c3c),
                button_hover: h(0x4a4a4a),
                button_active: h(0x575757),
                border: h(0x3c3c3c),
                link: h(0x5aaaff),
                ok: h(0x8cc06b),
                warn: h(0xe6c547),
                error: h(0xff5c5c),
            },
            terminal: TerminalColors {
                foreground: h(0xd4d7dd),
                background: h(0x121418),
                cursor: h(0xdcdcdc),
                selection_bg: h(0x2f5d9e),
                selection_fg: h(0xffffff),
                ansi: AnsiColors {
                    black: h(0x1d1f21),
                    red: h(0xcc6666),
                    green: h(0x8cc06b),
                    yellow: h(0xe6c547),
                    blue: h(0x6c9fe0),
                    magenta: h(0xb294bb),
                    cyan: h(0x5fc0c0),
                    white: h(0xc5c8c6),
                    bright_black: h(0x66696e),
                    bright_red: h(0xff7b7b),
                    bright_green: h(0xa8e07a),
                    bright_yellow: h(0xffdd66),
                    bright_blue: h(0x8ab8ff),
                    bright_magenta: h(0xd6a8e6),
                    bright_cyan: h(0x7de0e0),
                    bright_white: h(0xffffff),
                },
            },
            fonts: FontSizes::default(),
            shape: Shape::default(),
        }
    }

    fn light() -> Self {
        let h = Rgb::hex;
        Theme {
            name: "GodRemote Claro".into(),
            dark: false,
            ui: UiColors {
                background: h(0xf3f3f3),
                window: h(0xffffff),
                field: h(0xffffff),
                alternate: h(0xe9e9e9),
                text: h(0x333333),
                text_weak: h(0x777777),
                text_strong: h(0x000000),
                accent: h(0x1976d2),
                on_accent: h(0xffffff),
                button: h(0xdedede),
                button_hover: h(0xcfcfcf),
                button_active: h(0xbdbdbd),
                border: h(0xc4c4c4),
                link: h(0x0b62c4),
                ok: h(0x2e7d32),
                warn: h(0xa86400),
                error: h(0xc62828),
            },
            terminal: TerminalColors {
                foreground: h(0x2e3436),
                background: h(0xffffff),
                cursor: h(0x2e3436),
                selection_bg: h(0xb5d5ff),
                selection_fg: h(0x000000),
                ansi: AnsiColors {
                    black: h(0x2e3436),
                    red: h(0xcc0000),
                    green: h(0x3d8b00),
                    yellow: h(0xa67c00),
                    blue: h(0x3465a4),
                    magenta: h(0x75507b),
                    cyan: h(0x06989a),
                    white: h(0xbabdb6),
                    bright_black: h(0x555753),
                    bright_red: h(0xef2929),
                    bright_green: h(0x4e9a06),
                    bright_yellow: h(0xc4a000),
                    bright_blue: h(0x204a87),
                    bright_magenta: h(0xad7fa8),
                    bright_cyan: h(0x0a7f80),
                    bright_white: h(0x6e706b),
                },
            },
            fonts: FontSizes::default(),
            shape: Shape::default(),
        }
    }

    fn dracula() -> Self {
        let h = Rgb::hex;
        Theme {
            name: "Dracula".into(),
            dark: true,
            ui: UiColors {
                background: h(0x282a36),
                window: h(0x21222c),
                field: h(0x191a21),
                alternate: h(0x343746),
                text: h(0xf8f8f2),
                text_weak: h(0x8f9bc4),
                text_strong: h(0xffffff),
                accent: h(0x6d4fb5),
                on_accent: h(0xffffff),
                button: h(0x44475a),
                button_hover: h(0x565a73),
                button_active: h(0x6272a4),
                border: h(0x44475a),
                link: h(0x8be9fd),
                ok: h(0x50fa7b),
                warn: h(0xf1fa8c),
                error: h(0xff5555),
            },
            terminal: TerminalColors {
                foreground: h(0xf8f8f2),
                background: h(0x282a36),
                cursor: h(0xf8f8f2),
                selection_bg: h(0x44475a),
                selection_fg: h(0xffffff),
                ansi: AnsiColors {
                    black: h(0x21222c),
                    red: h(0xff5555),
                    green: h(0x50fa7b),
                    yellow: h(0xf1fa8c),
                    blue: h(0xbd93f9),
                    magenta: h(0xff79c6),
                    cyan: h(0x8be9fd),
                    white: h(0xf8f8f2),
                    bright_black: h(0x6272a4),
                    bright_red: h(0xff6e6e),
                    bright_green: h(0x69ff94),
                    bright_yellow: h(0xffffa5),
                    bright_blue: h(0xd6acff),
                    bright_magenta: h(0xff92df),
                    bright_cyan: h(0xa4ffff),
                    bright_white: h(0xffffff),
                },
            },
            fonts: FontSizes::default(),
            shape: Shape::default(),
        }
    }

    fn nord() -> Self {
        let h = Rgb::hex;
        Theme {
            name: "Nord".into(),
            dark: true,
            ui: UiColors {
                background: h(0x2e3440),
                window: h(0x3b4252),
                field: h(0x272c36),
                alternate: h(0x353c4a),
                text: h(0xd8dee9),
                text_weak: h(0x8f9bb3),
                text_strong: h(0xeceff4),
                accent: h(0x5e81ac),
                on_accent: h(0xeceff4),
                button: h(0x434c5e),
                button_hover: h(0x4c566a),
                button_active: h(0x5e6b85),
                border: h(0x4c566a),
                link: h(0x88c0d0),
                ok: h(0xa3be8c),
                warn: h(0xebcb8b),
                error: h(0xbf616a),
            },
            terminal: TerminalColors {
                foreground: h(0xd8dee9),
                background: h(0x2e3440),
                cursor: h(0xd8dee9),
                selection_bg: h(0x4c566a),
                selection_fg: h(0xeceff4),
                ansi: AnsiColors {
                    black: h(0x3b4252),
                    red: h(0xbf616a),
                    green: h(0xa3be8c),
                    yellow: h(0xebcb8b),
                    blue: h(0x81a1c1),
                    magenta: h(0xb48ead),
                    cyan: h(0x88c0d0),
                    white: h(0xe5e9f0),
                    bright_black: h(0x4c566a),
                    bright_red: h(0xbf616a),
                    bright_green: h(0xa3be8c),
                    bright_yellow: h(0xebcb8b),
                    bright_blue: h(0x81a1c1),
                    bright_magenta: h(0xb48ead),
                    bright_cyan: h(0x8fbcbb),
                    bright_white: h(0xeceff4),
                },
            },
            fonts: FontSizes::default(),
            shape: Shape::default(),
        }
    }

    fn solarized_dark() -> Self {
        let h = Rgb::hex;
        Theme {
            name: "Solarized Oscuro".into(),
            dark: true,
            ui: UiColors {
                background: h(0x002b36),
                window: h(0x073642),
                field: h(0x00222b),
                alternate: h(0x0a3a47),
                text: h(0x93a1a1),
                text_weak: h(0x657b83),
                text_strong: h(0xfdf6e3),
                accent: h(0x268bd2),
                on_accent: h(0xfdf6e3),
                button: h(0x0d4250),
                button_hover: h(0x155566),
                button_active: h(0x1d6a7e),
                border: h(0x174956),
                link: h(0x2aa198),
                ok: h(0x859900),
                warn: h(0xb58900),
                error: h(0xdc322f),
            },
            terminal: TerminalColors {
                foreground: h(0x839496),
                background: h(0x002b36),
                cursor: h(0x93a1a1),
                selection_bg: h(0x174956),
                selection_fg: h(0xfdf6e3),
                ansi: AnsiColors {
                    black: h(0x073642),
                    red: h(0xdc322f),
                    green: h(0x859900),
                    yellow: h(0xb58900),
                    blue: h(0x268bd2),
                    magenta: h(0xd33682),
                    cyan: h(0x2aa198),
                    white: h(0xeee8d5),
                    bright_black: h(0x586e75),
                    bright_red: h(0xcb4b16),
                    bright_green: h(0x93a1a1),
                    bright_yellow: h(0x657b83),
                    bright_blue: h(0x839496),
                    bright_magenta: h(0x6c71c4),
                    bright_cyan: h(0x93a1a1),
                    bright_white: h(0xfdf6e3),
                },
            },
            fonts: FontSizes::default(),
            shape: Shape::default(),
        }
    }

    /// Temas que vienen con la aplicación (no se pueden modificar).
    pub fn builtins() -> Vec<Theme> {
        vec![
            Theme::base(),
            Theme::light(),
            Theme::dracula(),
            Theme::nord(),
            Theme::solarized_dark(),
        ]
    }

    /// Ajusta los valores numéricos a rangos razonables.
    pub fn sanitize(&mut self) {
        let fix = |v: f32, min: f32, max: f32, default: f32| {
            if v.is_finite() {
                v.clamp(min, max)
            } else {
                default
            }
        };
        self.fonts.ui = fix(self.fonts.ui, 8.0, 32.0, 14.0);
        self.fonts.terminal = fix(self.fonts.terminal, 6.0, 48.0, 14.0);
        self.shape.rounding = fix(self.shape.rounding, 0.0, 16.0, 4.0);
        self.shape.spacing = fix(self.shape.spacing, 2.0, 20.0, 8.0);
        self.name = self.name.trim().chars().take(60).collect();
    }

    /// Lee un tema de un texto JSON, con errores comprensibles.
    pub fn from_json(text: &str) -> Result<Theme> {
        let mut theme: Theme = serde_json::from_str(text).map_err(|e| {
            anyhow!(
                "El tema no es válido (línea {}, columna {}): {e}",
                e.line(),
                e.column()
            )
        })?;
        theme.sanitize();
        if theme.name.is_empty() {
            bail!("El tema no tiene nombre (campo «nombre»)");
        }
        Ok(theme)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Aplica el tema a toda la interfaz y al terminal.
    pub fn apply(&self, ctx: &egui::Context) {
        use egui::{CornerRadius, FontFamily, FontId, Stroke, TextStyle};
        let c = &self.ui;
        ctx.set_theme(if self.dark {
            egui::ThemePreference::Dark
        } else {
            egui::ThemePreference::Light
        });
        let theme = self.clone();
        ctx.all_styles_mut(move |style| {
            let c = &theme.ui;
            let radius = CornerRadius::same(theme.shape.rounding.round() as u8);
            let mut v = if theme.dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            };
            v.dark_mode = theme.dark;
            v.panel_fill = c.background.color32();
            v.window_fill = c.window.color32();
            v.extreme_bg_color = c.field.color32();
            v.text_edit_bg_color = Some(c.field.color32());
            v.faint_bg_color = c.alternate.color32();
            v.code_bg_color = c.alternate.color32();
            v.weak_text_color = Some(c.text_weak.color32());
            v.hyperlink_color = c.link.color32();
            v.warn_fg_color = c.warn.color32();
            v.error_fg_color = c.error.color32();
            v.window_stroke = Stroke::new(1.0, c.border.color32());
            v.window_corner_radius = radius;
            v.menu_corner_radius = radius;
            v.selection.bg_fill = c.accent.color32();
            v.selection.stroke = Stroke::new(1.0, c.on_accent.color32());

            let w = &mut v.widgets;
            w.noninteractive.bg_fill = c.background.color32();
            w.noninteractive.weak_bg_fill = c.background.color32();
            w.noninteractive.bg_stroke = Stroke::new(1.0, c.border.color32());
            w.noninteractive.fg_stroke = Stroke::new(1.0, c.text.color32());
            w.inactive.bg_fill = c.button.color32();
            w.inactive.weak_bg_fill = c.button.color32();
            w.inactive.bg_stroke = Stroke::NONE;
            w.inactive.fg_stroke = Stroke::new(1.0, c.text.color32());
            w.hovered.bg_fill = c.button_hover.color32();
            w.hovered.weak_bg_fill = c.button_hover.color32();
            w.hovered.bg_stroke = Stroke::new(1.0, c.text_weak.color32());
            w.hovered.fg_stroke = Stroke::new(1.5, c.text_strong.color32());
            w.active.bg_fill = c.button_active.color32();
            w.active.weak_bg_fill = c.button_active.color32();
            w.active.bg_stroke = Stroke::new(1.0, c.text_strong.color32());
            w.active.fg_stroke = Stroke::new(2.0, c.text_strong.color32());
            w.open.bg_fill = c.button_active.color32();
            w.open.weak_bg_fill = c.button_active.color32();
            w.open.bg_stroke = Stroke::new(1.0, c.border.color32());
            w.open.fg_stroke = Stroke::new(1.0, c.text_strong.color32());
            for state in [
                &mut w.noninteractive,
                &mut w.inactive,
                &mut w.hovered,
                &mut w.active,
                &mut w.open,
            ] {
                state.corner_radius = radius;
            }
            style.visuals = v;

            let size = theme.fonts.ui;
            style.text_styles = [
                (
                    TextStyle::Small,
                    FontId::new(size * 0.75, FontFamily::Proportional),
                ),
                (TextStyle::Body, FontId::new(size, FontFamily::Proportional)),
                (
                    TextStyle::Button,
                    FontId::new(size, FontFamily::Proportional),
                ),
                (
                    TextStyle::Heading,
                    FontId::new(size * 1.45, FontFamily::Proportional),
                ),
                (
                    TextStyle::Monospace,
                    FontId::new(size, FontFamily::Monospace),
                ),
            ]
            .into();
            style.spacing.item_spacing = egui::vec2(theme.shape.spacing, theme.shape.spacing * 0.5);
            style.spacing.button_padding =
                egui::vec2(theme.shape.spacing, (theme.shape.spacing * 0.25).max(1.0));
        });

        let ansi = self.terminal.ansi.to_array().map(Rgb::color32);
        *CURRENT.write().unwrap() = Some(Applied {
            ok: c.ok.color32(),
            warn: c.warn.color32(),
            terminal: TermPalette {
                foreground: self.terminal.foreground.color32(),
                background: self.terminal.background.color32(),
                cursor: self.terminal.cursor.color32(),
                selection_bg: self.terminal.selection_bg.color32(),
                selection_fg: self.terminal.selection_fg.color32(),
                ansi,
                font_size: self.fonts.terminal,
            },
        });
        ctx.request_repaint();
    }
}

/// Colores del terminal ya resueltos, listos para dibujar.
#[derive(Debug, Clone, PartialEq)]
pub struct TermPalette {
    pub foreground: Color32,
    pub background: Color32,
    pub cursor: Color32,
    pub selection_bg: Color32,
    pub selection_fg: Color32,
    pub ansi: [Color32; 16],
    pub font_size: f32,
}

struct Applied {
    ok: Color32,
    warn: Color32,
    terminal: TermPalette,
}

static CURRENT: RwLock<Option<Applied>> = RwLock::new(None);

fn with_applied<T>(read: impl FnOnce(&Applied) -> T) -> T {
    let guard = CURRENT.read().unwrap();
    match guard.as_ref() {
        Some(applied) => read(applied),
        None => {
            // Aún no se ha aplicado ningún tema (pruebas, arranque).
            let base = Theme::base();
            let ansi = base.terminal.ansi.to_array().map(Rgb::color32);
            read(&Applied {
                ok: base.ui.ok.color32(),
                warn: base.ui.warn.color32(),
                terminal: TermPalette {
                    foreground: base.terminal.foreground.color32(),
                    background: base.terminal.background.color32(),
                    cursor: base.terminal.cursor.color32(),
                    selection_bg: base.terminal.selection_bg.color32(),
                    selection_fg: base.terminal.selection_fg.color32(),
                    ansi,
                    font_size: base.fonts.terminal,
                },
            })
        }
    }
}

/// Color de «todo bien» del tema activo.
pub fn ok_color() -> Color32 {
    with_applied(|a| a.ok)
}

/// Color de aviso del tema activo.
pub fn warn_color() -> Color32 {
    with_applied(|a| a.warn)
}

/// Paleta del terminal del tema activo.
pub fn terminal_palette() -> TermPalette {
    with_applied(|a| a.terminal.clone())
}

/// Nombre de fichero seguro para un tema.
fn file_stem(name: &str) -> String {
    let stem: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if stem.is_empty() { "tema".into() } else { stem }
}

/// Temas disponibles: los integrados más los del usuario (en disco).
pub struct ThemeStore {
    dir: PathBuf,
    builtin: Vec<Theme>,
    user: Vec<Theme>,
    /// Ficheros de la carpeta de temas que no se pudieron leer.
    pub load_errors: Vec<String>,
}

/// Tamaño máximo de un fichero de tema.
const MAX_THEME_FILE: u64 = 256 * 1024;

fn read_theme_file(path: &Path) -> Result<Theme> {
    let meta =
        std::fs::metadata(path).with_context(|| format!("No se pudo leer {}", path.display()))?;
    if !meta.is_file() {
        bail!("{} no es un fichero", path.display());
    }
    if meta.len() > MAX_THEME_FILE {
        bail!("{} es demasiado grande para ser un tema", path.display());
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("No se pudo leer {}", path.display()))?;
    Theme::from_json(&text)
}

impl ThemeStore {
    pub fn load(dir: &Path) -> Self {
        let mut store = Self {
            dir: dir.to_path_buf(),
            builtin: Theme::builtins(),
            user: vec![],
            load_errors: vec![],
        };
        let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        paths.sort();
        for path in paths {
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            match read_theme_file(&path) {
                Ok(theme) if !store.exists(&theme.name) => store.user.push(theme),
                Ok(theme) => store.load_errors.push(format!(
                    "{}: nombre repetido «{}»",
                    path.display(),
                    theme.name
                )),
                Err(e) => store.load_errors.push(format!("{e:#}")),
            }
        }
        store
    }

    pub fn names(&self) -> Vec<(String, bool)> {
        self.builtin
            .iter()
            .map(|t| (t.name.clone(), true))
            .chain(self.user.iter().map(|t| (t.name.clone(), false)))
            .collect()
    }

    pub fn get(&self, name: &str) -> Option<&Theme> {
        self.builtin
            .iter()
            .chain(&self.user)
            .find(|t| t.name == name)
    }

    pub fn exists(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    pub fn is_builtin(&self, name: &str) -> bool {
        self.builtin.iter().any(|t| t.name == name)
    }

    /// Nombre libre a partir de uno deseado («X», «X 2», «X 3»…).
    pub fn free_name(&self, wanted: &str) -> String {
        let wanted = wanted.trim();
        if !self.exists(wanted) {
            return wanted.to_string();
        }
        (2..)
            .map(|n| format!("{wanted} {n}"))
            .find(|n| !self.exists(n))
            .unwrap_or_else(|| wanted.to_string())
    }

    fn path_for(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.json", file_stem(name)))
    }

    /// Guarda (o actualiza) un tema del usuario. `previous_name` permite
    /// renombrarlo.
    pub fn save_user(&mut self, mut theme: Theme, previous_name: Option<&str>) -> Result<()> {
        theme.sanitize();
        if theme.name.is_empty() {
            bail!("El tema necesita un nombre");
        }
        if self.is_builtin(&theme.name) {
            bail!("«{}» es un tema integrado; elige otro nombre", theme.name);
        }
        let renaming = previous_name.is_some_and(|p| p != theme.name);
        if (renaming || previous_name.is_none()) && self.exists(&theme.name) {
            bail!("Ya existe un tema llamado «{}»", theme.name);
        }
        std::fs::create_dir_all(&self.dir)?;
        std::fs::write(self.path_for(&theme.name), theme.to_json())
            .with_context(|| format!("No se pudo guardar el tema en {}", self.dir.display()))?;
        if let Some(previous) = previous_name {
            if renaming {
                let _ = std::fs::remove_file(self.path_for(previous));
            }
            self.user.retain(|t| t.name != previous);
        }
        self.user.retain(|t| t.name != theme.name);
        self.user.push(theme);
        self.user.sort_by_key(|t| t.name.to_lowercase());
        Ok(())
    }

    pub fn delete_user(&mut self, name: &str) -> Result<()> {
        if self.is_builtin(name) {
            bail!("Los temas integrados no se pueden eliminar");
        }
        let _ = std::fs::remove_file(self.path_for(name));
        self.user.retain(|t| t.name != name);
        Ok(())
    }

    /// Importa un tema desde un fichero. Si el nombre ya existe se le añade
    /// un número. Devuelve el nombre con el que queda guardado.
    pub fn import(&mut self, path: &Path) -> Result<String> {
        let mut theme = read_theme_file(path)?;
        theme.name = self.free_name(&theme.name);
        let name = theme.name.clone();
        self.save_user(theme, None)?;
        Ok(name)
    }
}

/// Escribe un tema en la ruta indicada (para compartirlo o editarlo fuera).
pub fn export(theme: &Theme, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, theme.to_json())
        .with_context(|| format!("No se pudo escribir {}", path.display()))
}

/// Carpeta sugerida para exportar: Descargas, o la carpeta personal.
pub fn default_export_dir() -> PathBuf {
    dirs::download_dir().unwrap_or_else(crate::paths::home_dir)
}

/// Preferencias de la aplicación.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    #[serde(rename = "tema")]
    pub theme: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: BASE_NAME.into(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::paths::write_private(path, &serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_parse_and_print() {
        assert_eq!(Rgb::parse("#1e90ff"), Some(Rgb([0x1e, 0x90, 0xff])));
        assert_eq!(Rgb::parse("1E90FF"), Some(Rgb([0x1e, 0x90, 0xff])));
        assert_eq!(Rgb::parse(" #abc "), Some(Rgb([0xaa, 0xbb, 0xcc])));
        assert_eq!(Rgb::parse("#12345"), None);
        assert_eq!(Rgb::parse("rojo"), None);
        assert_eq!(Rgb::parse("#ññññññ"), None);
        assert_eq!(Rgb::hex(0x0a0b0c).to_hex(), "#0a0b0c");
    }

    #[test]
    fn base_theme_roundtrips_and_is_editable() {
        let base = Theme::base();
        let json = base.to_json();
        // El fichero base es legible: claves en español y colores #rrggbb.
        assert!(json.contains("\"nombre\": \"GodRemote Oscuro\""));
        assert!(json.contains("\"texto_sobre_acento\": \"#ffffff\""));
        assert!(json.contains("\"rojo_brillante\""));
        assert_eq!(Theme::from_json(&json).unwrap(), base);
        // Todos los integrados sobreviven a exportar e importar.
        for theme in Theme::builtins() {
            assert_eq!(Theme::from_json(&theme.to_json()).unwrap(), theme);
        }
    }

    #[test]
    fn partial_and_invalid_files() {
        // Un tema parcial hereda el resto del tema base.
        let t = Theme::from_json(
            r##"{"nombre":"Mío","interfaz":{"acento":"#ff0000"},"letra":{"terminal":999}}"##,
        )
        .unwrap();
        assert_eq!(t.ui.accent, Rgb([255, 0, 0]));
        assert_eq!(t.ui.background, Theme::base().ui.background);
        assert_eq!(t.terminal, Theme::base().terminal);
        assert_eq!(t.fonts.terminal, 48.0); // se limita a un máximo razonable
        // Errores comprensibles.
        let e = Theme::from_json(r##"{"nombre":"x","interfaz":{"acento":"azul"}}"##).unwrap_err();
        assert!(format!("{e}").contains("color no válido «azul»"), "{e}");
        let e =
            Theme::from_json(r##"{"nombre":"x","interfaz":{"acentoo":"#000000"}}"##).unwrap_err();
        assert!(format!("{e}").contains("acentoo"), "{e}");
        assert!(Theme::from_json("{").is_err());
        assert!(Theme::from_json(r#"{"nombre":"   "}"#).is_err());
    }

    #[test]
    fn store_import_export_edit_delete() {
        let dir = tempfile::tempdir().unwrap();
        let themes_dir = dir.path().join("themes");
        let mut store = ThemeStore::load(&themes_dir);
        assert_eq!(store.names().len(), Theme::builtins().len());

        // Descargar la base, modificarla fuera y subirla.
        let exported = dir.path().join("base.json");
        export(&Theme::base(), &exported).unwrap();
        let edited = std::fs::read_to_string(&exported)
            .unwrap()
            .replace("#005c80", "#aa00ff");
        std::fs::write(&exported, edited).unwrap();
        // Tiene el mismo nombre que un integrado: se importa con otro nombre.
        let name = store.import(&exported).unwrap();
        assert_eq!(name, "GodRemote Oscuro 2");
        assert_eq!(store.get(&name).unwrap().ui.accent, Rgb::hex(0xaa00ff));
        assert!(!store.is_builtin(&name));

        // Persiste en disco.
        let reloaded = ThemeStore::load(&themes_dir);
        assert!(reloaded.exists(&name) && reloaded.load_errors.is_empty());

        // Renombrar y editar.
        let mut theme = store.get(&name).unwrap().clone();
        theme.name = "Morado".into();
        store.save_user(theme, Some(&name)).unwrap();
        assert!(store.exists("Morado") && !store.exists(&name));
        assert_eq!(std::fs::read_dir(&themes_dir).unwrap().count(), 1);

        // No se puede pisar un integrado ni duplicar un nombre.
        let mut clash = Theme::base();
        assert!(store.save_user(clash.clone(), None).is_err());
        clash.name = "Morado".into();
        assert!(store.save_user(clash, None).is_err());
        assert!(store.delete_user(BASE_NAME).is_err());

        store.delete_user("Morado").unwrap();
        assert!(!store.exists("Morado"));
        assert_eq!(std::fs::read_dir(&themes_dir).unwrap().count(), 0);

        // Ficheros que no son temas se rechazan con un mensaje claro.
        let bad = dir.path().join("malo.json");
        std::fs::write(&bad, "esto no es JSON").unwrap();
        assert!(format!("{:#}", store.import(&bad).unwrap_err()).contains("no es válido"));
        assert!(store.import(&dir.path().join("no-existe.json")).is_err());

        // Un fichero roto en la carpeta no impide cargar los demás.
        std::fs::create_dir_all(&themes_dir).unwrap();
        std::fs::write(themes_dir.join("roto.json"), "{").unwrap();
        let reloaded = ThemeStore::load(&themes_dir);
        assert_eq!(reloaded.load_errors.len(), 1);
    }

    #[test]
    fn settings_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert_eq!(Settings::load(&path).theme, BASE_NAME);
        let s = Settings {
            theme: "Nord".into(),
        };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
    }
}
