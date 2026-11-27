//! Utilidades de presentación.

use crate::model::{AuthMethod, Protocol, Session};

/// Colores listos para marcar pestañas (el primero, para producción).
pub const TAB_COLORS: [(&str, u32); 6] = [
    ("Rojo", 0xd9453c),
    ("Naranja", 0xe08a2b),
    ("Amarillo", 0xd9b93c),
    ("Verde", 0x4fae5b),
    ("Azul", 0x3f8fd9),
    ("Morado", 0x9a63d6),
];

/// Elección del color con el que se marcan las pestañas: ninguno propio
/// (casilla sin marcar) o uno de los preparados o cualquier otro. Devuelve
/// `true` si ha cambiado.
pub fn tab_color_ui(ui: &mut egui::Ui, label: &str, color: &mut Option<crate::theme::Rgb>) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let mut own = color.is_some();
        if ui.checkbox(&mut own, label).changed() {
            *color = own.then_some(crate::theme::Rgb::hex(TAB_COLORS[0].1));
            changed = true;
        }
        if let Some(current) = color {
            for (name, value) in TAB_COLORS {
                let preset = crate::theme::Rgb::hex(value);
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
                ui.painter()
                    .rect_filled(rect.shrink(2.0), 3.0, preset.color32());
                if *current == preset {
                    ui.painter().rect_stroke(
                        rect.shrink(1.0),
                        3.0,
                        egui::Stroke::new(1.5, ui.visuals().strong_text_color()),
                        egui::StrokeKind::Inside,
                    );
                }
                if response.on_hover_text(name).clicked() {
                    *current = preset;
                    changed = true;
                }
            }
            changed |= ui
                .color_edit_button_srgb(&mut current.0)
                .on_hover_text("Otro color")
                .changed();
        }
    });
    changed
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Formatea segundos Unix como `AAAA-MM-DD HH:MM` en la hora local.
pub fn format_time(secs: i64) -> String {
    format_time_with_offset(secs, local_offset(secs))
}

/// Desfase de la zona horaria local (segundos al este de UTC) en ese
/// instante; tiene en cuenta el horario de verano de esa fecha.
fn local_offset(secs: i64) -> i64 {
    use chrono::{Offset, TimeZone};
    chrono::Local
        .timestamp_opt(secs, 0)
        .single()
        .map(|dt| dt.offset().fix().local_minus_utc() as i64)
        .unwrap_or(0)
}

/// Formatea segundos Unix aplicando un desfase fijo respecto a UTC.
pub fn format_time_with_offset(secs: i64, offset_secs: i64) -> String {
    let secs = secs + offset_secs;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Algoritmo de fecha civil (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60
    )
}

/// Interpreta `usuario@host[:puerto]` para la conexión rápida.
pub fn parse_quick_connect(input: &str, protocol: Protocol) -> Result<Session, String> {
    let input = input.trim();
    let (user, rest) = match input.rsplit_once('@') {
        Some((u, r)) => (u.trim(), r.trim()),
        None => ("", input),
    };
    if rest.is_empty() {
        return Err("Escribe usuario@host[:puerto]".into());
    }
    let (host, port) = if let Some(stripped) = rest.strip_prefix('[') {
        // [IPv6]:puerto
        let (h, tail) = stripped
            .split_once(']')
            .ok_or("Dirección IPv6 mal formada")?;
        let port = match tail.strip_prefix(':') {
            Some(p) => p.parse::<u16>().map_err(|_| "Puerto no válido")?,
            None => protocol.default_port(),
        };
        (h.to_string(), port)
    } else {
        match rest.rsplit_once(':') {
            // Un solo ':' => host:puerto; varios => IPv6 sin corchetes.
            Some((h, p)) if !h.contains(':') => (
                h.to_string(),
                p.parse::<u16>().map_err(|_| "Puerto no válido")?,
            ),
            _ => (rest.to_string(), protocol.default_port()),
        }
    };
    if host.is_empty() || port == 0 {
        return Err("Host o puerto no válidos".into());
    }
    if protocol == Protocol::Serial {
        return Err("La conexión rápida no admite el puerto serie".into());
    }
    if user.is_empty() && protocol.is_ssh() {
        return Err("Falta el usuario: usuario@host".into());
    }
    Ok(Session {
        name: String::new(),
        protocol,
        host,
        port,
        username: user.to_string(),
        auth: AuthMethod::Password,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(3 * 1024 * 1024), "3.0 MB");
    }

    #[test]
    fn times() {
        assert_eq!(format_time_with_offset(0, 0), "1970-01-01 00:00");
        assert_eq!(format_time_with_offset(951_782_400, 0), "2000-02-29 00:00");
        assert_eq!(
            format_time_with_offset(1_700_000_000, 0),
            "2023-11-14 22:13"
        );
        // Madrid en invierno (UTC+1) y en verano (UTC+2); cambio de día.
        assert_eq!(
            format_time_with_offset(1_700_000_000, 3600),
            "2023-11-14 23:13"
        );
        assert_eq!(
            format_time_with_offset(1_690_929_000, 7200),
            "2023-08-02 00:30"
        );
        assert_eq!(format_time_with_offset(0, -5 * 3600), "1969-12-31 19:00");
        // La hora local coincide con aplicar el desfase que da el sistema.
        let now = 1_700_000_000;
        assert_eq!(
            format_time(now),
            format_time_with_offset(now, local_offset(now))
        );
    }

    #[test]
    fn quick_connect() {
        let s = parse_quick_connect("root@srv.example.org", Protocol::Ssh).unwrap();
        assert_eq!(
            (s.username.as_str(), s.host.as_str(), s.port),
            ("root", "srv.example.org", 22)
        );
        let s = parse_quick_connect(" ana@10.0.0.1:2222 ", Protocol::Sftp).unwrap();
        assert_eq!((s.host.as_str(), s.port), ("10.0.0.1", 2222));
        let s = parse_quick_connect("u@[fe80::1]:22022", Protocol::Ssh).unwrap();
        assert_eq!((s.host.as_str(), s.port), ("fe80::1", 22022));
        let s = parse_quick_connect("u@fe80::1", Protocol::Ssh).unwrap();
        assert_eq!((s.host.as_str(), s.port), ("fe80::1", 22));
        let s = parse_quick_connect("winsrv", Protocol::Rdp).unwrap();
        assert_eq!((s.host.as_str(), s.port), ("winsrv", 3389));
        let s = parse_quick_connect("pc-oficina:5901", Protocol::Vnc).unwrap();
        assert_eq!(
            (s.host.as_str(), s.port, s.protocol),
            ("pc-oficina", 5901, Protocol::Vnc)
        );
        let s = parse_quick_connect("switch", Protocol::Telnet).unwrap();
        assert_eq!((s.host.as_str(), s.port), ("switch", 23));
        assert!(parse_quick_connect("COM3", Protocol::Serial).is_err());
        assert!(parse_quick_connect("host", Protocol::Ssh).is_err());
        assert!(parse_quick_connect("u@host:abc", Protocol::Ssh).is_err());
        assert!(parse_quick_connect("", Protocol::Ssh).is_err());
    }
}
