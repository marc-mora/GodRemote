//! Búsqueda de la paleta de órdenes: encontrar un equipo, una pestaña, un
//! snippet o una acción escribiendo parte de su nombre.

/// Texto en minúsculas y sin acentos, para comparar sin fijarse en ellos.
pub fn normalize(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' | 'ã' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'õ' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            other => other,
        })
        .collect()
}

/// Los caracteres de `needle` aparecen en `haystack` en ese orden.
fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut chars = haystack.chars();
    needle.chars().all(|n| chars.any(|h| h == n))
}

/// `needle` abrevia alguna palabra de `text`: empieza como ella y sus
/// letras aparecen en orden («prd» abrevia «producción»).
fn is_abbreviation(needle: &str, text: &str) -> bool {
    let first = needle.chars().next();
    text.split(|c: char| !c.is_alphanumeric())
        .any(|word| word.chars().next() == first && is_subsequence(needle, word))
}

/// Puntuación de `text` para lo que se ha escrito, o `None` si no encaja.
/// Cada palabra de la consulta debe aparecer: mejor al principio del texto o
/// de una de sus palabras, después en cualquier sitio y, por último, con
/// sus letras salteadas («prd» encuentra «producción»).
pub fn score(query: &str, text: &str) -> Option<i32> {
    let text = normalize(text);
    let mut total = 0;
    for word in normalize(query).split_whitespace() {
        total += if let Some(position) = text.find(word) {
            let at_word_start = position == 0
                || text[..position]
                    .chars()
                    .next_back()
                    .is_some_and(|c| !c.is_alphanumeric());
            let bonus = if at_word_start { 50 } else { 0 };
            // Cuanto antes aparece, mejor.
            100 + bonus - (position.min(40) as i32)
        } else if word.chars().count() >= 2 && is_abbreviation(word, &text) {
            10
        } else {
            return None;
        };
    }
    // A igualdad de aciertos, primero lo más corto (más específico).
    Some(total * 4 - (text.chars().count().min(200) as i32) / 8)
}

/// Índices de los textos que encajan con la consulta, del mejor al peor.
/// Sin consulta se devuelven todos en su orden.
pub fn rank<'a>(query: &str, texts: impl Iterator<Item = &'a str>) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..texts.count()).collect();
    }
    let mut scored: Vec<(usize, i32)> = texts
        .enumerate()
        .filter_map(|(index, text)| score(query, text).map(|s| (index, s)))
        .collect();
    // Orden estable: a igual puntuación se respeta el orden de entrada.
    scored.sort_by_key(|(_, s)| std::cmp::Reverse(*s));
    scored.into_iter().map(|(index, _)| index).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranked(query: &str, texts: &[&str]) -> Vec<String> {
        rank(query, texts.iter().copied())
            .into_iter()
            .map(|i| texts[i].to_string())
            .collect()
    }

    #[test]
    fn accents_and_case_do_not_matter() {
        assert_eq!(normalize("Producción ÑU"), "produccion nu");
        assert!(score("produccion", "Carpeta PRODUCCIÓN").is_some());
        assert!(score("túnel", "Tunel web").is_some());
    }

    #[test]
    fn every_word_must_match() {
        let texts = [
            "Producción / web-01 (deploy@10.0.3.7)",
            "Producción / web-02 (deploy@10.0.3.8)",
            "Preproducción / bd-01 (postgres@10.1.0.4)",
            "Abrir un terminal local",
        ];
        assert_eq!(ranked("web 02", &texts), [texts[1]]);
        assert_eq!(ranked("10.1", &texts), [texts[2]]);
        assert!(ranked("web oracle", &texts).is_empty());
        // Sin consulta, todo y en su orden.
        assert_eq!(ranked("  ", &texts), texts);
    }

    #[test]
    fn better_matches_come_first() {
        let texts = ["mi-web-antigua", "web-01", "sitio (web)", "wxexb"];
        // Al principio del texto, luego al principio de palabra más pronto,
        // y las letras salteadas al final.
        assert_eq!(
            ranked("web", &texts),
            ["web-01", "mi-web-antigua", "sitio (web)", "wxexb"]
        );
        // Letras salteadas: «prd» encuentra «producción».
        assert_eq!(ranked("prd", &["producción", "desarrollo"]), ["producción"]);
        // Una sola letra no se busca salteada: sería casi todo.
        assert!(score("z", "producción").is_none());
        // Las letras salteadas deben abreviar UNA palabra, desde su inicial.
        assert!(score("dir", "Producción / heredero").is_none());
        assert!(score("rdc", "producción").is_none());
        assert!(score("srvd", "Producción / servidor-directo").is_some());
    }
}
