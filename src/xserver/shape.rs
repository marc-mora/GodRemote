//! Extensión SHAPE (versión 1.1): ventanas que no son rectangulares.
//!
//! La forma de una ventana es una lista de rectángulos disjuntos, en
//! coordenadas de su interior. La de contorno («bounding») recorta lo que
//! se ve de la ventana y de sus hijas y dónde recibe el ratón; la de
//! entrada («input») solo esto último. La de recorte («clip») se acepta y
//! no se aplica: aquí el borde de una ventana con forma no se pinta.

use std::sync::Arc;

use super::draw::Rect;
use super::proto::*;
use super::server::{ClientId, ROOT, Server};

/// Código de operación de SHAPE (el mismo que usa X.Org).
pub const SHAPE_OPCODE: u8 = 129;
pub const SHAPE_FIRST_EVENT: u8 = SHAPE_NOTIFY;

/// Rectángulos de una forma. Una más complicada no se aplica (la ventana
/// se queda rectangular), que es mejor que cerrar la aplicación.
pub const MAX_SHAPE_RECTS: usize = 4096;
/// Clientes apuntados a los avisos de cambio de forma de una ventana.
const MAX_LISTENERS: usize = 64;

const KIND_BOUNDING: u8 = 0;
const KIND_CLIP: u8 = 1;
const KIND_INPUT: u8 = 2;

pub type Region = Arc<Vec<Rect>>;

/// Tope de trabajo al combinar regiones (rectángulos activos sumados banda
/// a banda).
const MAX_REGION_WORK: usize = 2_000_000;

/// Combina dos regiones (listas de rectángulos, que pueden solaparse) y
/// devuelve el resultado en bandas de rectángulos disjuntos. `keep` decide,
/// para cada trozo, si queda dentro según esté en `a` y en `b`. `None` si el
/// resultado pasa de [`MAX_SHAPE_RECTS`] o el cálculo se desmanda.
fn combine(a: &[Rect], b: &[Rect], keep: impl Fn(bool, bool) -> bool) -> Option<Vec<Rect>> {
    // Cada rectángulo con la región a la que pertenece, por orden de entrada
    // en el barrido de arriba abajo.
    let mut items: Vec<(Rect, bool)> = (a.iter().map(|r| (*r, false)))
        .chain(b.iter().map(|r| (*r, true)))
        .filter(|(r, _)| !r.is_empty())
        .collect();
    items.sort_unstable_by_key(|(r, _)| r.y0);
    let mut ys: Vec<i32> = items.iter().flat_map(|(r, _)| [r.y0, r.y1]).collect();
    ys.sort_unstable();
    ys.dedup();
    let mut out: Vec<Rect> = vec![];
    // Índice del primer rectángulo de la banda anterior, para fundir las
    // bandas iguales que se tocan.
    let mut previous = 0;
    let mut active: Vec<(Rect, bool)> = vec![];
    let mut next = 0;
    let mut work = 0;
    // Bordes verticales de la banda: (x, región, +1 al entrar o −1 al salir).
    let mut edges: Vec<(i32, bool, i32)> = vec![];
    for band in ys.windows(2) {
        let (y0, y1) = (band[0], band[1]);
        active.retain(|(r, _)| r.y1 > y0);
        while next < items.len() && items[next].0.y0 <= y0 {
            active.push(items[next]);
            next += 1;
        }
        work += active.len() + 1;
        if work > MAX_REGION_WORK {
            return None;
        }
        edges.clear();
        for (r, which) in &active {
            edges.push((r.x0, *which, 1));
            edges.push((r.x1, *which, -1));
        }
        edges.sort_unstable_by_key(|e| e.0);
        let start = out.len();
        let (mut in_a, mut in_b) = (0, 0);
        let mut i = 0;
        while i < edges.len() {
            let x0 = edges[i].0;
            while i < edges.len() && edges[i].0 == x0 {
                if edges[i].1 {
                    in_b += edges[i].2;
                } else {
                    in_a += edges[i].2;
                }
                i += 1;
            }
            let Some(x1) = edges.get(i).map(|e| e.0) else {
                break;
            };
            if !keep(in_a > 0, in_b > 0) {
                continue;
            }
            match out[start..].last_mut() {
                Some(last) if last.x1 == x0 => last.x1 = x1,
                _ => out.push(Rect { x0, y0, x1, y1 }),
            }
        }
        // ¿Misma banda que la de encima, y pegada a ella? Se alarga aquella.
        let same = out.len() - start == start - previous
            && start > previous
            && out[previous..start]
                .iter()
                .zip(&out[start..])
                .all(|(p, n)| p.y1 == n.y0 && p.x0 == n.x0 && p.x1 == n.x1);
        if same {
            out.truncate(start);
            for r in &mut out[previous..] {
                r.y1 = y1;
            }
        } else {
            previous = start;
        }
        if out.len() > MAX_SHAPE_RECTS {
            return None;
        }
    }
    Some(out)
}

/// Rectángulos de los píxeles a 1 de un mapa de bits.
fn mask_rects(data: &[u32], w: i32, h: i32) -> Option<Vec<Rect>> {
    let mut rows: Vec<Rect> = vec![];
    for y in 0..h {
        let row = &data[(y * w) as usize..((y + 1) * w) as usize];
        let mut x = 0;
        while x < w {
            if row[x as usize] & 1 == 0 {
                x += 1;
                continue;
            }
            let x0 = x;
            while x < w && row[x as usize] & 1 != 0 {
                x += 1;
            }
            rows.push(Rect {
                x0,
                y0: y,
                x1: x,
                y1: y + 1,
            });
            // (Antes de fundir filas iguales puede haber muchos más.)
            if rows.len() > 64 * MAX_SHAPE_RECTS {
                return None;
            }
        }
    }
    combine(&rows, &[], |a, _| a)
}

/// `true` si el punto cae dentro de la región.
pub fn region_contains(region: &[Rect], x: i32, y: i32) -> bool {
    region.iter().any(|r| r.contains(x, y))
}

impl Server {
    /// Región de una ventana (la que tenga, o su rectángulo por omisión).
    fn shape_or_default(&self, id: Xid, kind: u8) -> XResult<Vec<Rect>> {
        let w = self.win(id)?;
        let shape = match kind {
            KIND_BOUNDING => &w.shape,
            KIND_INPUT => &w.input_shape,
            _ => &None,
        };
        Ok(match shape {
            Some(region) => region.to_vec(),
            None => vec![Rect::new(0, 0, w.w as i32, w.h as i32)],
        })
    }

    /// Aplica una operación de SHAPE: combina la región `source` (ya
    /// desplazada) con la que tenía la ventana.
    fn shape_apply(&mut self, id: Xid, kind: u8, op: u8, source: Option<Vec<Rect>>) -> XResult {
        if kind > KIND_INPUT {
            return Err(XError::new(BAD_VALUE, kind as u32));
        }
        if id == ROOT {
            return Err(XError::new(BAD_MATCH, id));
        }
        let current = self.shape_or_default(id, kind)?;
        // Lo que quede a un millón de píxeles de la ventana da igual dónde
        // esté: se acota, y así desplazar una forma una y otra vez no puede
        // desbordar las coordenadas.
        const FAR: i32 = 1 << 20;
        let source = source.map(|rects| {
            rects
                .into_iter()
                .map(|r| Rect {
                    x0: r.x0.clamp(-FAR, FAR),
                    y0: r.y0.clamp(-FAR, FAR),
                    x1: r.x1.clamp(-FAR, FAR),
                    y1: r.y1.clamp(-FAR, FAR),
                })
                .collect::<Vec<_>>()
        });
        let result: Option<Vec<Rect>> = match (op, source) {
            // «Set» sin región: la ventana vuelve a ser rectangular.
            (0, None) => None,
            (0, Some(src)) => combine(&src, &[], |a, _| a),
            (1, Some(src)) => combine(&current, &src, |a, b| a || b),
            (2, Some(src)) => combine(&current, &src, |a, b| a && b),
            (3, Some(src)) => combine(&current, &src, |a, b| a && !b),
            (4, Some(src)) => combine(&current, &src, |a, b| !a && b),
            (1..=4, None) => return Ok(()),
            _ => return Err(XError::new(BAD_VALUE, op as u32)),
        };
        let region = result.map(Arc::new);
        self.damage_frame(id);
        let w = self.win_mut(id)?;
        let shaped = region.is_some();
        match kind {
            KIND_BOUNDING => w.shape = region,
            KIND_INPUT => w.input_shape = region,
            _ => {}
        }
        let listeners = w.shape_listeners.clone();
        let extents = self.shape_extents(id, kind);
        self.damage_frame(id);
        self.refresh_pointer_window();
        let time = self.now();
        for client in listeners {
            let ev = Ev::new(SHAPE_NOTIFY, kind)
                .u32(id)
                .i16(extents.x0 as i16)
                .i16(extents.y0 as i16)
                .u16(extents.width() as u16)
                .u16(extents.height() as u16)
                .u32(time)
                .u8(shaped as u8)
                .done();
            self.send(client, ev);
        }
        Ok(())
    }

    /// Rectángulo que abarca una región de la ventana.
    fn shape_extents(&self, id: Xid, kind: u8) -> Rect {
        self.shape_or_default(id, kind)
            .unwrap_or_default()
            .iter()
            .fold(Rect::EMPTY, |acc, r| acc.union(r))
    }

    pub(super) fn dispatch_shape(&mut self, c: ClientId, minor: u8, rd: &mut Reader) -> XResult {
        match minor {
            // QueryVersion
            0 => {
                let mut w = self.reply(c, 0);
                w.u16(1).u16(1);
                self.send_reply(c, w);
                Ok(())
            }
            // Rectangles
            1 => {
                let (op, kind) = (rd.u8()?, rd.u8()?);
                rd.skip(2)?;
                let id = rd.u32()?;
                let (dx, dy) = (rd.i16()? as i32, rd.i16()? as i32);
                self.win(id)?;
                let mut rects = Vec::with_capacity(rd.remaining() / 8);
                while rd.remaining() >= 8 {
                    let (x, y) = (rd.i16()? as i32 + dx, rd.i16()? as i32 + dy);
                    rects.push(Rect::new(x, y, rd.u16()? as i32, rd.u16()? as i32));
                }
                self.shape_apply(id, kind, op, Some(rects))
            }
            // Mask
            2 => {
                let (op, kind) = (rd.u8()?, rd.u8()?);
                rd.skip(2)?;
                let id = rd.u32()?;
                let (dx, dy) = (rd.i16()? as i32, rd.i16()? as i32);
                let source = rd.u32()?;
                self.win(id)?;
                let region = if source == 0 {
                    None
                } else {
                    let pixmap = self
                        .pixmaps
                        .get(&source)
                        .ok_or(XError::new(BAD_PIXMAP, source))?;
                    let img = &pixmap.image;
                    if img.depth != 1 {
                        return Err(XError::new(BAD_MATCH, source));
                    }
                    // Una máscara demasiado enrevesada no se aplica.
                    let Some(rects) = mask_rects(&img.data, img.w, img.h) else {
                        return Ok(());
                    };
                    Some(rects.iter().map(|r| r.translate(dx, dy)).collect())
                };
                self.shape_apply(id, kind, op, region)
            }
            // Combine: con la forma de otra ventana.
            3 => {
                let (op, kind, source_kind) = (rd.u8()?, rd.u8()?, rd.u8()?);
                rd.skip(1)?;
                let id = rd.u32()?;
                let (dx, dy) = (rd.i16()? as i32, rd.i16()? as i32);
                let source = rd.u32()?;
                self.win(id)?;
                if source_kind > KIND_INPUT {
                    return Err(XError::new(BAD_VALUE, source_kind as u32));
                }
                let region = self
                    .shape_or_default(source, source_kind)?
                    .iter()
                    .map(|r| r.translate(dx, dy))
                    .collect();
                self.shape_apply(id, kind, op, Some(region))
            }
            // Offset
            4 => {
                let kind = rd.u8()?;
                rd.skip(3)?;
                let id = rd.u32()?;
                let (dx, dy) = (rd.i16()? as i32, rd.i16()? as i32);
                let w = self.win(id)?;
                let shaped = match kind {
                    KIND_BOUNDING => w.shape.is_some(),
                    KIND_INPUT => w.input_shape.is_some(),
                    KIND_CLIP => false,
                    _ => return Err(XError::new(BAD_VALUE, kind as u32)),
                };
                if !shaped {
                    return Ok(());
                }
                let moved = self
                    .shape_or_default(id, kind)?
                    .iter()
                    .map(|r| r.translate(dx, dy))
                    .collect();
                self.shape_apply(id, kind, 0, Some(moved))
            }
            // QueryExtents
            5 => {
                let id = rd.u32()?;
                let shaped = self.win(id)?.shape.is_some();
                let b = self.shape_extents(id, KIND_BOUNDING);
                let clip = self.shape_extents(id, KIND_CLIP);
                let mut w = self.reply(c, 0);
                w.u8(shaped as u8).u8(0).u16(0);
                for r in [b, clip] {
                    w.i16(r.x0 as i16).i16(r.y0 as i16);
                    w.u16(r.width() as u16).u16(r.height() as u16);
                }
                self.send_reply(c, w);
                Ok(())
            }
            // SelectInput
            6 => {
                let id = rd.u32()?;
                let enable = rd.u8()? != 0;
                let w = self.win_mut(id)?;
                w.shape_listeners.retain(|l| *l != c);
                if enable {
                    if w.shape_listeners.len() >= MAX_LISTENERS {
                        return Err(XError::new(BAD_ALLOC, id));
                    }
                    w.shape_listeners.push(c);
                }
                Ok(())
            }
            // InputSelected
            7 => {
                let id = rd.u32()?;
                let selected = self.win(id)?.shape_listeners.contains(&c);
                let w = self.reply(c, selected as u8);
                self.send_reply(c, w);
                Ok(())
            }
            // GetRectangles
            8 => {
                let id = rd.u32()?;
                let kind = rd.u8()?;
                if kind > KIND_INPUT {
                    return Err(XError::new(BAD_VALUE, kind as u32));
                }
                let rects = self.shape_or_default(id, kind)?;
                // Ordenados por bandas («YXBanded»).
                let mut w = self.reply(c, 3);
                w.u32(rects.len() as u32).zeros(20);
                for r in rects {
                    w.i16(r.x0 as i16).i16(r.y0 as i16);
                    w.u16(r.width() as u16).u16(r.height() as u16);
                }
                self.send_reply(c, w);
                Ok(())
            }
            _ => Err(XError::new(BAD_REQUEST, 0)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(region: &[Rect]) -> i32 {
        region.iter().map(|r| r.width() * r.height()).sum()
    }

    #[test]
    fn regions_combine_into_disjoint_bands() {
        let a = [Rect::new(0, 0, 10, 10)];
        let b = [Rect::new(5, 5, 10, 10)];
        let union = combine(&a, &b, |a, b| a || b).unwrap();
        assert_eq!(area(&union), 175);
        assert_eq!(
            union,
            vec![
                Rect::new(0, 0, 10, 5),
                Rect::new(0, 5, 15, 5),
                Rect::new(5, 10, 10, 5)
            ]
        );
        let both = combine(&a, &b, |a, b| a && b).unwrap();
        assert_eq!(both, vec![Rect::new(5, 5, 5, 5)]);
        let minus = combine(&a, &b, |a, b| a && !b).unwrap();
        assert_eq!(area(&minus), 75);
        assert!(region_contains(&minus, 2, 8) && !region_contains(&minus, 7, 7));
        // Rectángulos que se solapan o se tocan se funden.
        let merged = combine(
            &[
                Rect::new(0, 0, 4, 4),
                Rect::new(4, 0, 4, 4),
                Rect::new(0, 4, 8, 4),
                Rect::new(2, 2, 3, 3),
            ],
            &[],
            |a, _| a,
        )
        .unwrap();
        assert_eq!(merged, vec![Rect::new(0, 0, 8, 8)]);
        // Los vacíos no cuentan.
        assert_eq!(
            combine(&[Rect::new(3, 3, 0, 9)], &[], |a, _| a).unwrap(),
            vec![]
        );
    }

    #[test]
    fn masks_become_rectangles() {
        // Un rombo de 5×5 y un píxel suelto.
        #[rustfmt::skip]
        let bits = [
            0, 0, 1, 0, 0, 0, 0,
            0, 1, 1, 1, 0, 0, 0,
            0, 1, 1, 1, 0, 0, 1,
            0, 0, 1, 0, 0, 0, 0,
        ];
        let rects = mask_rects(&bits, 7, 4).unwrap();
        assert_eq!(area(&rects), 9);
        for (i, bit) in bits.iter().enumerate() {
            let (x, y) = (i as i32 % 7, i as i32 / 7);
            assert_eq!(region_contains(&rects, x, y), *bit == 1, "({x}, {y})");
        }
        // Un tablero de ajedrez grande es demasiado: no hay forma.
        let board: Vec<u32> = (0..200 * 200)
            .map(|i| ((i % 200 + i / 200) % 2) as u32)
            .collect();
        assert!(mask_rects(&board, 200, 200).is_none());
    }
}
