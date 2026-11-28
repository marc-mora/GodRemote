//! Traducciones al inglés: pares (texto en español, texto en inglés),
//! repartidos por zonas del programa para que cada fichero sea manejable.
//! La prueba `every_text_has_its_translation` vigila que no falte ni sobre
//! ninguno y que los huecos `{nombre}` coincidan.

mod backend;
mod base;
mod ui_main;
mod ui_views;

/// Una tabla de traducciones.
pub(super) type Table = &'static [(&'static str, &'static str)];

pub(super) fn tables() -> &'static [Table] {
    &[base::T, backend::T, ui_main::T, ui_views::T]
}
