//! Nombres de color de X (los de `rgb.txt`) y especificaciones numéricas
//! (`#rgb`, `rgb:r/g/b`, `rgbi:r/g/b`).

/// Colores con nombre: `(nombre en minúsculas y sin espacios, r, g, b)`.
const NAMED: &[(&str, u8, u8, u8)] = &[
    ("snow", 255, 250, 250),
    ("ghostwhite", 248, 248, 255),
    ("whitesmoke", 245, 245, 245),
    ("gainsboro", 220, 220, 220),
    ("floralwhite", 255, 250, 240),
    ("oldlace", 253, 245, 230),
    ("linen", 250, 240, 230),
    ("antiquewhite", 250, 235, 215),
    ("papayawhip", 255, 239, 213),
    ("blanchedalmond", 255, 235, 205),
    ("bisque", 255, 228, 196),
    ("peachpuff", 255, 218, 185),
    ("navajowhite", 255, 222, 173),
    ("moccasin", 255, 228, 181),
    ("cornsilk", 255, 248, 220),
    ("ivory", 255, 255, 240),
    ("lemonchiffon", 255, 250, 205),
    ("seashell", 255, 245, 238),
    ("honeydew", 240, 255, 240),
    ("mintcream", 245, 255, 250),
    ("azure", 240, 255, 255),
    ("aliceblue", 240, 248, 255),
    ("lavender", 230, 230, 250),
    ("lavenderblush", 255, 240, 245),
    ("mistyrose", 255, 228, 225),
    ("white", 255, 255, 255),
    ("black", 0, 0, 0),
    ("darkslategray", 47, 79, 79),
    ("dimgray", 105, 105, 105),
    ("slategray", 112, 128, 144),
    ("lightslategray", 119, 136, 153),
    ("gray", 190, 190, 190),
    ("x11gray", 190, 190, 190),
    ("webgray", 128, 128, 128),
    ("lightgray", 211, 211, 211),
    ("midnightblue", 25, 25, 112),
    ("navy", 0, 0, 128),
    ("navyblue", 0, 0, 128),
    ("cornflowerblue", 100, 149, 237),
    ("darkslateblue", 72, 61, 139),
    ("slateblue", 106, 90, 205),
    ("mediumslateblue", 123, 104, 238),
    ("lightslateblue", 132, 112, 255),
    ("mediumblue", 0, 0, 205),
    ("royalblue", 65, 105, 225),
    ("blue", 0, 0, 255),
    ("dodgerblue", 30, 144, 255),
    ("deepskyblue", 0, 191, 255),
    ("skyblue", 135, 206, 235),
    ("lightskyblue", 135, 206, 250),
    ("steelblue", 70, 130, 180),
    ("lightsteelblue", 176, 196, 222),
    ("lightblue", 173, 216, 230),
    ("powderblue", 176, 224, 230),
    ("paleturquoise", 175, 238, 238),
    ("darkturquoise", 0, 206, 209),
    ("mediumturquoise", 72, 209, 204),
    ("turquoise", 64, 224, 208),
    ("cyan", 0, 255, 255),
    ("aqua", 0, 255, 255),
    ("lightcyan", 224, 255, 255),
    ("cadetblue", 95, 158, 160),
    ("mediumaquamarine", 102, 205, 170),
    ("aquamarine", 127, 255, 212),
    ("darkgreen", 0, 100, 0),
    ("darkolivegreen", 85, 107, 47),
    ("darkseagreen", 143, 188, 143),
    ("seagreen", 46, 139, 87),
    ("mediumseagreen", 60, 179, 113),
    ("lightseagreen", 32, 178, 170),
    ("palegreen", 152, 251, 152),
    ("springgreen", 0, 255, 127),
    ("lawngreen", 124, 252, 0),
    ("green", 0, 255, 0),
    ("lime", 0, 255, 0),
    ("x11green", 0, 255, 0),
    ("webgreen", 0, 128, 0),
    ("chartreuse", 127, 255, 0),
    ("mediumspringgreen", 0, 250, 154),
    ("greenyellow", 173, 255, 47),
    ("limegreen", 50, 205, 50),
    ("yellowgreen", 154, 205, 50),
    ("forestgreen", 34, 139, 34),
    ("olivedrab", 107, 142, 35),
    ("darkkhaki", 189, 183, 107),
    ("khaki", 240, 230, 140),
    ("palegoldenrod", 238, 232, 170),
    ("lightgoldenrodyellow", 250, 250, 210),
    ("lightyellow", 255, 255, 224),
    ("yellow", 255, 255, 0),
    ("gold", 255, 215, 0),
    ("lightgoldenrod", 238, 221, 130),
    ("goldenrod", 218, 165, 32),
    ("darkgoldenrod", 184, 134, 11),
    ("rosybrown", 188, 143, 143),
    ("indianred", 205, 92, 92),
    ("saddlebrown", 139, 69, 19),
    ("sienna", 160, 82, 45),
    ("peru", 205, 133, 63),
    ("burlywood", 222, 184, 135),
    ("beige", 245, 245, 220),
    ("wheat", 245, 222, 179),
    ("sandybrown", 244, 164, 96),
    ("tan", 210, 180, 140),
    ("chocolate", 210, 105, 30),
    ("firebrick", 178, 34, 34),
    ("brown", 165, 42, 42),
    ("darksalmon", 233, 150, 122),
    ("salmon", 250, 128, 114),
    ("lightsalmon", 255, 160, 122),
    ("orange", 255, 165, 0),
    ("darkorange", 255, 140, 0),
    ("coral", 255, 127, 80),
    ("lightcoral", 240, 128, 128),
    ("tomato", 255, 99, 71),
    ("orangered", 255, 69, 0),
    ("red", 255, 0, 0),
    ("hotpink", 255, 105, 180),
    ("deeppink", 255, 20, 147),
    ("pink", 255, 192, 203),
    ("lightpink", 255, 182, 193),
    ("palevioletred", 219, 112, 147),
    ("maroon", 176, 48, 96),
    ("x11maroon", 176, 48, 96),
    ("webmaroon", 128, 0, 0),
    ("mediumvioletred", 199, 21, 133),
    ("violetred", 208, 32, 144),
    ("magenta", 255, 0, 255),
    ("fuchsia", 255, 0, 255),
    ("violet", 238, 130, 238),
    ("plum", 221, 160, 221),
    ("orchid", 218, 112, 214),
    ("mediumorchid", 186, 85, 211),
    ("darkorchid", 153, 50, 204),
    ("darkviolet", 148, 0, 211),
    ("blueviolet", 138, 43, 226),
    ("purple", 160, 32, 240),
    ("x11purple", 160, 32, 240),
    ("webpurple", 128, 0, 128),
    ("mediumpurple", 147, 112, 219),
    ("thistle", 216, 191, 216),
    ("darkgray", 169, 169, 169),
    ("darkblue", 0, 0, 139),
    ("darkcyan", 0, 139, 139),
    ("darkmagenta", 139, 0, 139),
    ("darkred", 139, 0, 0),
    ("lightgreen", 144, 238, 144),
    ("crimson", 220, 20, 60),
    ("indigo", 75, 0, 130),
    ("olive", 128, 128, 0),
    ("rebeccapurple", 102, 51, 153),
    ("silver", 192, 192, 192),
    ("teal", 0, 128, 128),
];

/// Familias con variantes numeradas `nombre1`…`nombre4`: el valor es el de la
/// variante 1; las demás son ese color atenuado (93 %, 80 % y 55 %).
const NUMBERED: &[(&str, u8, u8, u8)] = &[
    ("snow", 255, 250, 250),
    ("seashell", 255, 245, 238),
    ("antiquewhite", 255, 239, 219),
    ("bisque", 255, 228, 196),
    ("peachpuff", 255, 218, 185),
    ("navajowhite", 255, 222, 173),
    ("lemonchiffon", 255, 250, 205),
    ("cornsilk", 255, 248, 220),
    ("ivory", 255, 255, 240),
    ("honeydew", 240, 255, 240),
    ("lavenderblush", 255, 240, 245),
    ("mistyrose", 255, 228, 225),
    ("azure", 240, 255, 255),
    ("slateblue", 131, 111, 255),
    ("royalblue", 72, 118, 255),
    ("blue", 0, 0, 255),
    ("dodgerblue", 30, 144, 255),
    ("steelblue", 99, 184, 255),
    ("deepskyblue", 0, 191, 255),
    ("skyblue", 135, 206, 255),
    ("lightskyblue", 176, 226, 255),
    ("slategray", 198, 226, 255),
    ("lightsteelblue", 202, 225, 255),
    ("lightblue", 191, 239, 255),
    ("lightcyan", 224, 255, 255),
    ("paleturquoise", 187, 255, 255),
    ("cadetblue", 152, 245, 255),
    ("turquoise", 0, 245, 255),
    ("cyan", 0, 255, 255),
    ("darkslategray", 151, 255, 255),
    ("aquamarine", 127, 255, 212),
    ("darkseagreen", 193, 255, 193),
    ("seagreen", 84, 255, 159),
    ("palegreen", 154, 255, 154),
    ("springgreen", 0, 255, 127),
    ("green", 0, 255, 0),
    ("chartreuse", 127, 255, 0),
    ("olivedrab", 192, 255, 62),
    ("darkolivegreen", 202, 255, 112),
    ("khaki", 255, 246, 143),
    ("lightgoldenrod", 255, 236, 139),
    ("lightyellow", 255, 255, 224),
    ("yellow", 255, 255, 0),
    ("gold", 255, 215, 0),
    ("goldenrod", 255, 193, 37),
    ("darkgoldenrod", 255, 185, 15),
    ("rosybrown", 255, 193, 193),
    ("indianred", 255, 106, 106),
    ("sienna", 255, 130, 71),
    ("burlywood", 255, 211, 155),
    ("wheat", 255, 231, 186),
    ("tan", 255, 165, 79),
    ("chocolate", 255, 127, 36),
    ("firebrick", 255, 48, 48),
    ("brown", 255, 64, 64),
    ("salmon", 255, 140, 105),
    ("lightsalmon", 255, 160, 122),
    ("orange", 255, 165, 0),
    ("darkorange", 255, 127, 0),
    ("coral", 255, 114, 86),
    ("tomato", 255, 99, 71),
    ("orangered", 255, 69, 0),
    ("red", 255, 0, 0),
    ("deeppink", 255, 20, 147),
    ("hotpink", 255, 110, 180),
    ("pink", 255, 181, 197),
    ("lightpink", 255, 174, 185),
    ("palevioletred", 255, 130, 171),
    ("maroon", 255, 52, 179),
    ("violetred", 255, 62, 150),
    ("magenta", 255, 0, 255),
    ("orchid", 255, 131, 250),
    ("plum", 255, 187, 255),
    ("mediumorchid", 224, 102, 255),
    ("darkorchid", 191, 62, 255),
    ("purple", 155, 48, 255),
    ("mediumpurple", 171, 130, 255),
    ("thistle", 255, 225, 255),
];

/// Nombre normalizado: minúsculas, sin espacios y con «grey» como «gray».
fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect::<String>()
        .replace("grey", "gray")
}

fn hex_channel(digits: &str) -> Option<u16> {
    if digits.is_empty() || digits.len() > 4 {
        return None;
    }
    let v = u16::from_str_radix(digits, 16).ok()?;
    // `rgb:` escala el valor: «f» es todo, igual que «ffff».
    Some(match digits.len() {
        1 => v * 0x1111,
        2 => v * 0x0101,
        3 => (v << 4) | (v >> 8),
        _ => v,
    })
}

/// Busca un color por nombre o especificación. Devuelve los tres canales en
/// 16 bits, como los usa el protocolo.
pub fn lookup(name: &str) -> Option<(u16, u16, u16)> {
    let name = name.trim();
    if let Some(hex) = name.strip_prefix('#') {
        // Forma antigua: los dígitos son los bits altos (sin escalar).
        let n = hex.len() / 3;
        if hex.len() % 3 != 0 || !(1..=4).contains(&n) || !hex.is_ascii() {
            return None;
        }
        let ch = |i: usize| -> Option<u16> {
            let v = u16::from_str_radix(&hex[i * n..(i + 1) * n], 16).ok()?;
            Some(v << (16 - 4 * n))
        };
        return Some((ch(0)?, ch(1)?, ch(2)?));
    }
    let lower = name.to_ascii_lowercase();
    if let Some(spec) = lower.strip_prefix("rgb:") {
        let mut parts = spec.split('/');
        let (r, g, b) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() {
            return None;
        }
        return Some((hex_channel(r)?, hex_channel(g)?, hex_channel(b)?));
    }
    if let Some(spec) = lower.strip_prefix("rgbi:") {
        let mut parts = spec.split('/').map(|p| p.parse::<f32>().ok());
        let mut ch = || -> Option<u16> {
            let v = parts.next()??;
            (0.0..=1.0)
                .contains(&v)
                .then_some((v * 65535.0).round() as u16)
        };
        return Some((ch()?, ch()?, ch()?));
    }
    let key = normalize(name);
    let wide = |r: u8, g: u8, b: u8| (r as u16 * 257, g as u16 * 257, b as u16 * 257);
    if let Some((_, r, g, b)) = NAMED.iter().find(|(n, ..)| *n == key) {
        return Some(wide(*r, *g, *b));
    }
    // gray0 … gray100.
    if let Some(level) = key.strip_prefix("gray")
        && let Ok(level) = level.parse::<u32>()
        && level <= 100
    {
        // Como en rgb.txt, donde 50 y 90 quedaron redondeados hacia abajo.
        let mut v = ((level * 255 + 50) / 100) as u8;
        if level == 50 || level == 90 {
            v -= 1;
        }
        return Some(wide(v, v, v));
    }
    // Variantes numeradas: red1 … red4.
    let (base, digit) = key.split_at(key.len().saturating_sub(1));
    let factor = match digit {
        "1" => 1.0,
        "2" => 0.932,
        "3" => 0.804,
        "4" => 0.545,
        _ => return None,
    };
    let (_, r, g, b) = NUMBERED.iter().find(|(n, ..)| *n == base)?;
    let dim = |v: u8| (v as f32 * factor).round() as u8;
    Some(wide(dim(*r), dim(*g), dim(*b)))
}

/// Píxel TrueColor (0xRRGGBB) de un color de 16 bits por canal.
pub fn pixel(r: u16, g: u16, b: u16) -> u32 {
    ((r as u32 >> 8) << 16) | ((g as u32 >> 8) << 8) | (b as u32 >> 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_ignore_case_spaces_and_grey_spelling() {
        assert_eq!(lookup("red"), Some((0xffff, 0, 0)));
        assert_eq!(lookup("Navy Blue"), lookup("navyblue"));
        assert_eq!(lookup("LightSlateGrey"), lookup("light slate gray"));
        assert_eq!(lookup("DarkGrey"), Some((169 * 257, 169 * 257, 169 * 257)));
        assert_eq!(lookup("no-existe"), None);
        assert!(NAMED.len() >= 140);
    }

    #[test]
    fn gray_levels_and_numbered_variants() {
        assert_eq!(lookup("gray0"), Some((0, 0, 0)));
        assert_eq!(lookup("grey100"), Some((0xffff, 0xffff, 0xffff)));
        assert_eq!(lookup("gray50"), Some((127 * 257, 127 * 257, 127 * 257)));
        assert_eq!(lookup("gray90"), Some((229 * 257, 229 * 257, 229 * 257)));
        assert_eq!(lookup("gray101"), None);
        assert_eq!(lookup("red3"), Some((205 * 257, 0, 0)));
        assert_eq!(lookup("red4"), Some((139 * 257, 0, 0)));
        assert_eq!(lookup("snow2"), Some((238 * 257, 233 * 257, 233 * 257)));
        assert_eq!(lookup("red5"), None);
    }

    #[test]
    fn numeric_specifications() {
        assert_eq!(lookup("#204080"), Some((0x2000, 0x4000, 0x8000)));
        assert_eq!(lookup("#fff"), Some((0xf000, 0xf000, 0xf000)));
        assert_eq!(lookup("#123456789abc"), Some((0x1234, 0x5678, 0x9abc)));
        assert_eq!(lookup("#12345"), None);
        assert_eq!(lookup("rgb:ff/80/0"), Some((0xffff, 0x8080, 0)));
        assert_eq!(lookup("RGB:f/f/f"), Some((0xffff, 0xffff, 0xffff)));
        assert_eq!(lookup("rgb:1/2"), None);
        assert_eq!(lookup("rgbi:1.0/0.5/0"), Some((65535, 32768, 0)));
        assert_eq!(pixel(0x2000, 0x4000, 0x8000), 0x204080);
    }
}
