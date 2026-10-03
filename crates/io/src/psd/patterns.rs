//! Photoshop patterns (ADR 0016): the global `Patt` / `Pat2` / `Pat3`
//! blocks that hold a document's patterns, `PtFl` pattern fill layers and
//! the `patternFill` (Pattern Overlay) effect descriptor.
//!
//! A pattern record is: version 1, image mode, height and width (i16),
//! a Unicode name, a Pascal-string id (a UUID), a 256-entry palette for
//! indexed patterns, then a "virtual memory array list": version 3, a
//! length, the rectangle, the colour-slot count and one array per slot
//! plus two (user mask, then transparency). Each written array carries
//! its depth, rectangle, pixel depth and compression (0 raw, 1 RLE with
//! 16-bit row counts) before the data. Photoshop writes 24 colour slots
//! whatever the mode.

use lumenply_doc::pattern::{PatternOverlayFx, PatternRef};
use lumenply_doc::{BlendMode, Document, Fill, Pattern};
use lumenply_tiles::{Raster, Rgba};

use super::extra::{descriptor_block, parse_descriptor, Desc, Val};

fn be32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 4)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at + 2)?;
    Some(u16::from_be_bytes([s[0], s[1]]))
}

/// One channel slot of a pattern, decoded to 0..1 values.
fn read_array(b: &[u8], at: &mut usize, w: usize, h: usize) -> Option<Option<Vec<f32>>> {
    let written = be32(b, *at)?;
    *at += 4;
    if written == 0 {
        return Some(None);
    }
    let len = be32(b, *at)? as usize;
    *at += 4;
    if len == 0 {
        return Some(None);
    }
    let body = b.get(*at..at.checked_add(len)?)?;
    *at += len;
    if len < 23 {
        return Some(None);
    }
    let depth = be32(body, 0)? as usize;
    let (top, left, bottom, right) = (be32(body, 4)?, be32(body, 8)?, be32(body, 12)?, be32(body, 16)?);
    let (cw, ch) = (
        right.saturating_sub(left) as usize,
        bottom.saturating_sub(top) as usize,
    );
    if cw != w || ch != h {
        return Some(None);
    }
    let compression = body[22];
    let data = &body[23..];
    let bytes_per = match depth {
        8 => 1,
        16 => 2,
        32 => 4,
        _ => return Some(None),
    };
    let raw = match compression {
        0 => data.get(..w * h * bytes_per)?.to_vec(),
        1 => {
            // Row byte counts (16-bit), then PackBits rows.
            let counts = h * 2;
            let mut out = Vec::with_capacity(w * h * bytes_per);
            let mut p = counts;
            for row in 0..h {
                let n = be16(data, row * 2)? as usize;
                let src = data.get(p..p + n)?;
                p += n;
                out.extend(super::unpackbits(src, w * bytes_per).ok()?);
            }
            out
        }
        _ => return Some(None),
    };
    let vals = match bytes_per {
        1 => raw.iter().map(|&v| v as f32 / 255.0).collect(),
        2 => raw
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]) as f32 / 65535.0)
            .collect(),
        _ => raw
            .chunks_exact(4)
            .map(|c| f32::from_be_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    };
    Some(Some(vals))
}

/// Parse one pattern record starting at `b[0]`; returns it and the bytes
/// it used. `None` when the record is unreadable.
pub(crate) fn read_pattern_record(b: &[u8]) -> Option<(Pattern, usize)> {
    if be32(b, 0)? != 1 {
        return None;
    }
    let mode = be32(b, 4)?;
    let h = be16(b, 8)? as i16;
    let w = be16(b, 10)? as i16;
    let mut at = 12;
    let n = (be32(b, at)? as usize).min(4096);
    at += 4;
    let mut units = Vec::with_capacity(n);
    for i in 0..n {
        units.push(be16(b, at + i * 2)?);
    }
    at += n * 2;
    while units.last() == Some(&0) {
        units.pop();
    }
    let name = String::from_utf16_lossy(&units);
    let idn = *b.get(at)? as usize;
    let id = String::from_utf8_lossy(b.get(at + 1..at + 1 + idn)?)
        .trim_end_matches('\0')
        .to_string();
    at += 1 + idn;
    let mut palette = None;
    if mode == 2 {
        palette = Some(b.get(at..at + 768)?.to_vec());
        at += 768 + 4;
    }
    // Virtual memory array list.
    if be32(b, at)? != 3 {
        return None;
    }
    let len = be32(b, at + 4)? as usize;
    let list_start = at + 8;
    let end = list_start.checked_add(len)?;
    let list = b.get(..end)?;
    let (top, left, bottom, right) = (
        be32(list, list_start)?,
        be32(list, list_start + 4)?,
        be32(list, list_start + 8)?,
        be32(list, list_start + 12)?,
    );
    let (pw, ph) = (
        right.saturating_sub(left) as usize,
        bottom.saturating_sub(top) as usize,
    );
    if pw == 0
        || ph == 0
        || pw > 8192
        || ph > 8192
        || (w > 0 && h > 0 && (w as usize != pw || h as usize != ph))
    {
        return None;
    }
    let slots = (be32(list, list_start + 16)? as usize).min(64) + 2;
    let mut p = list_start + 20;
    let mut planes: Vec<Option<Vec<f32>>> = Vec::with_capacity(slots);
    for _ in 0..slots {
        planes.push(read_array(list, &mut p, pw, ph)?);
    }
    let color: Vec<&Vec<f32>> = planes[..slots - 2].iter().flatten().collect();
    let alpha = planes[slots - 1].as_ref().or(planes[slots - 2].as_ref());
    let mut img = Raster::new(pw as u32, ph as u32);
    let lin8 = |v: f32| crate::srgb_to_linear_f(v.clamp(0.0, 1.0));
    for i in 0..pw * ph {
        let get = |c: usize| color.get(c).map_or(0.0, |pl| pl[i]);
        let rgb = match (mode, color.len()) {
            (_, 0) => [0.0; 3],
            // Indexed: the index picks a palette entry.
            (2, _) => {
                let k = (get(0) * 255.0).round() as usize;
                let pal = palette.as_deref().unwrap_or(&[]);
                let c = |j: usize| pal.get(j * 256 + k).copied().unwrap_or(0) as f32 / 255.0;
                [lin8(c(0)), lin8(c(1)), lin8(c(2))]
            }
            // CMYK stores inverted ink (1 = no ink).
            (4, n) if n >= 4 => super::color_modes::cmyk_percent_to_linear([
                (1.0 - get(0) as f64) * 100.0,
                (1.0 - get(1) as f64) * 100.0,
                (1.0 - get(2) as f64) * 100.0,
                (1.0 - get(3) as f64) * 100.0,
            ]),
            (9, n) if n >= 3 => super::color_modes::lab_to_linear_srgb(
                get(0) * 100.0,
                get(1) * 255.0 - 128.0,
                get(2) * 255.0 - 128.0,
            )
            .map(|v| v.clamp(0.0, 1.0)),
            // RGB (and multichannel with three or more planes).
            (_, n) if n >= 3 => [lin8(get(0)), lin8(get(1)), lin8(get(2))],
            // Greyscale, duotone, single-plane multichannel.
            _ => [lin8(get(0)); 3],
        };
        let a = alpha.map_or(1.0, |pl| pl[i].clamp(0.0, 1.0));
        img.pixels[i] = Rgba::from_straight(rgb[0], rgb[1], rgb[2], a);
    }
    let name = if name.is_empty() { id.clone() } else { name };
    Some((Pattern::new(id, name, img), end))
}

/// Parse the body of a `Patt` / `Pat2` / `Pat3` block.
pub(super) fn parse_patterns_block(data: &[u8]) -> Vec<Pattern> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(len) = be32(data, at) {
        let len = len as usize;
        let Some(rec) = data.get(at + 4..(at + 4).saturating_add(len).min(data.len())) else {
            break;
        };
        if let Some((p, _)) = read_pattern_record(rec) {
            if !out.iter().any(|q: &Pattern| q.id == p.id) {
                out.push(p);
            }
        }
        // Records are padded to four bytes.
        at += 4 + len.div_ceil(4) * 4;
        if len == 0 {
            break;
        }
    }
    out
}

/// The patterns in the global tagged blocks that follow the layer info
/// (`lm_start` is the layer-and-mask section's body).
pub(super) fn global_patterns(buf: &[u8], lm_start: usize, lm_len: usize, psb: bool) -> Vec<Pattern> {
    let end = lm_start.saturating_add(lm_len).min(buf.len());
    let len_at = |p: usize, wide: bool| -> Option<usize> {
        if wide {
            let s = buf.get(p..p + 8)?;
            usize::try_from(u64::from_be_bytes(s.try_into().ok()?)).ok()
        } else {
            be32(buf, p).map(|v| v as usize)
        }
    };
    let Some(li_len) = len_at(lm_start, psb) else {
        return Vec::new();
    };
    let li_end = lm_start + if psb { 8 } else { 4 } + li_len;
    // Global layer mask info.
    let Some(gm) = be32(buf, li_end) else {
        return Vec::new();
    };
    let mut at = li_end + 4 + gm as usize;
    let mut out: Vec<Pattern> = Vec::new();
    while at + 12 <= end {
        let sig = &buf[at..at + 4];
        if sig != b"8BIM" && sig != b"8B64" {
            // Tolerate writers that pad blocks to 4 bytes or don't pad.
            at += 1;
            continue;
        }
        let key = &buf[at + 4..at + 8];
        let wide = psb
            && matches!(
                key,
                b"LMsk"
                    | b"Lr16"
                    | b"Lr32"
                    | b"Layr"
                    | b"Mt16"
                    | b"Mt32"
                    | b"Mtrn"
                    | b"Alph"
                    | b"FMsk"
                    | b"lnk2"
                    | b"FEid"
                    | b"FXid"
                    | b"PxSD"
            );
        let Some(len) = len_at(at + 8, wide) else {
            break;
        };
        let data_at = at + if wide { 16 } else { 12 };
        let Some(data_end) = data_at.checked_add(len).filter(|e| *e <= end) else {
            break;
        };
        if matches!(key, b"Patt" | b"Pat2" | b"Pat3") {
            for p in parse_patterns_block(&buf[data_at..data_end]) {
                if !out.iter().any(|q| q.id == p.id) {
                    out.push(p);
                }
            }
        }
        at = data_end;
    }
    out
}

fn put32(d: &mut Vec<u8>, v: u32) {
    d.extend_from_slice(&v.to_be_bytes());
}

/// One pattern record, 8-bit RGB (with transparency in the last slot when
/// any pixel is not opaque), raw channels.
fn pattern_record(p: &Pattern) -> Vec<u8> {
    let img = &p.image;
    let (w, h) = (img.width.min(i16::MAX as u32), img.height.min(i16::MAX as u32));
    let mut d = Vec::new();
    put32(&mut d, 1);
    put32(&mut d, 3); // RGB
    d.extend_from_slice(&(h as i16).to_be_bytes());
    d.extend_from_slice(&(w as i16).to_be_bytes());
    let units: Vec<u16> = p.name.encode_utf16().chain(std::iter::once(0)).collect();
    put32(&mut d, units.len() as u32);
    for u in units {
        d.extend_from_slice(&u.to_be_bytes());
    }
    let id: Vec<u8> = p.id.bytes().filter(|b| b.is_ascii()).take(255).collect();
    d.push(id.len() as u8);
    d.extend_from_slice(&id);
    let mut planes: [Vec<u8>; 4] = Default::default();
    for y in 0..h {
        for x in 0..w {
            let [r, g, b, a] = img.get(x, y).to_straight();
            planes[0].push(crate::linear_to_srgb(r));
            planes[1].push(crate::linear_to_srgb(g));
            planes[2].push(crate::linear_to_srgb(b));
            planes[3].push((a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        }
    }
    let has_alpha = planes[3].iter().any(|&a| a < 255);
    let mut list = Vec::new();
    for v in [0, 0, h, w] {
        put32(&mut list, v);
    }
    const SLOTS: usize = 24;
    put32(&mut list, SLOTS as u32);
    let array = |list: &mut Vec<u8>, plane: &[u8]| {
        put32(list, 1);
        put32(list, (23 + plane.len()) as u32);
        put32(list, 8);
        for v in [0, 0, h, w] {
            put32(list, v);
        }
        list.extend_from_slice(&8u16.to_be_bytes());
        list.push(0); // raw
        list.extend_from_slice(plane);
    };
    for slot in 0..SLOTS + 2 {
        match slot {
            0..=2 => array(&mut list, &planes[slot]),
            s if s == SLOTS + 1 && has_alpha => array(&mut list, &planes[3]),
            _ => put32(&mut list, 0),
        }
    }
    put32(&mut d, 3);
    put32(&mut d, list.len() as u32);
    d.extend_from_slice(&list);
    d
}

/// The `Patt` block body for `patterns`.
pub(super) fn patterns_block(patterns: &[&Pattern]) -> Vec<u8> {
    let mut d = Vec::new();
    for p in patterns {
        let rec = pattern_record(p);
        put32(&mut d, rec.len() as u32);
        d.extend_from_slice(&rec);
        while d.len() % 4 != 0 {
            d.push(0);
        }
    }
    d
}

/// The global `Patt` tagged block for the patterns `doc` uses (empty when
/// it uses none).
pub(super) fn global_block(doc: &Document) -> Vec<u8> {
    let used = doc.used_pattern_ids();
    let pats: Vec<&Pattern> = used
        .iter()
        .filter_map(|id| doc.patterns.iter().find(|p| &p.id == id))
        .collect();
    if pats.is_empty() {
        return Vec::new();
    }
    let body = patterns_block(&pats);
    let mut out = Vec::new();
    out.extend_from_slice(b"8BIM");
    out.extend_from_slice(b"Patt");
    put32(&mut out, body.len() as u32);
    out.extend_from_slice(&body);
    out
}

fn text_of(d: &Desc, key: &[u8]) -> String {
    match d.get(key) {
        Some(Val::Text(s)) => s.trim_end_matches('\0').to_string(),
        _ => String::new(),
    }
}

fn reference_of(d: &Desc) -> Option<PatternRef> {
    let p = d.obj(b"Ptrn")?;
    let id = text_of(p, b"Idnt");
    let name = text_of(p, b"Nm  ");
    if id.is_empty() && name.is_empty() {
        return None;
    }
    Some(PatternRef {
        id,
        name,
        image: None,
    })
}

fn phase_of(d: &Desc) -> [f32; 2] {
    d.obj(b"phase").map_or([0.0, 0.0], |p| {
        [
            p.num(b"Hrzn").unwrap_or(0.0) as f32,
            p.num(b"Vrtc").unwrap_or(0.0) as f32,
        ]
    })
}

fn scale_of(d: &Desc) -> f32 {
    (d.num(b"Scl ").unwrap_or(100.0) as f32 / 100.0).clamp(0.01, 100.0)
}

/// A `PtFl` block as a pattern fill.
pub(super) fn parse_pattern_fill(data: &[u8]) -> Option<Fill> {
    let d = parse_descriptor(data)?;
    Some(Fill::Pattern {
        pattern: reference_of(&d)?,
        scale: scale_of(&d),
        offset: phase_of(&d),
        angle: d.num(b"Angl").unwrap_or(0.0) as f32,
    })
}

fn ptrn(r: &PatternRef) -> Val {
    Val::Obj(
        Desc::new(b"Ptrn")
            .with(b"Nm  ", Val::Text(r.name.clone()))
            .with(b"Idnt", Val::Text(r.id.clone())),
    )
}

fn phase(o: [f32; 2]) -> Val {
    Val::Obj(
        Desc::new(b"Pnt ")
            .with(b"Hrzn", Val::Doub(o[0] as f64))
            .with(b"Vrtc", Val::Doub(o[1] as f64)),
    )
}

/// The `PtFl` block body of a pattern fill.
pub(super) fn pattern_fill_block(pattern: &PatternRef, scale: f32, offset: [f32; 2], angle: f32) -> Vec<u8> {
    let mut d = Desc::new(b"null").with(b"Ptrn", ptrn(pattern));
    if angle != 0.0 {
        d = d.with(b"Angl", Val::Unit(*b"#Ang", angle as f64));
    }
    d = d
        .with(b"Scl ", Val::Unit(*b"#Prc", (scale * 100.0) as f64))
        .with(b"Algn", Val::Bool(true))
        .with(b"phase", phase(offset));
    descriptor_block(&d)
}

/// A `patternFill` effect object as a Pattern Overlay (`blend` already
/// read by the caller).
pub(super) fn overlay_of(d: &Desc, blend: BlendMode) -> Option<PatternOverlayFx> {
    Some(PatternOverlayFx {
        pattern: reference_of(d)?,
        scale: scale_of(d),
        opacity: (d.num(b"Opct").unwrap_or(100.0) as f32 / 100.0).clamp(0.0, 1.0),
        blend,
        offset: phase_of(d),
        angle: d.num(b"Angl").unwrap_or(0.0) as f32,
        link: !matches!(d.get(b"Algn"), Some(Val::Bool(false))),
    })
}

/// The `patternFill` effect object's settings after its head (enable flags
/// and blend mode, which the caller writes).
pub(super) fn overlay_desc(head: Desc, p: &PatternOverlayFx) -> Desc {
    head.with(
        b"Opct",
        Val::Unit(*b"#Prc", (p.opacity.clamp(0.0, 1.0) * 100.0) as f64),
    )
    .with(b"Ptrn", ptrn(&p.pattern))
    .with(b"Angl", Val::Unit(*b"#Ang", p.angle as f64))
    .with(b"Scl ", Val::Unit(*b"#Prc", (p.scale * 100.0) as f64))
    .with(b"Algn", Val::Bool(p.link))
    .with(b"phase", phase(p.offset))
}

/// Photoshop anchors a linked pattern where the layer was when the
/// pattern was applied and follows the layer as it moves, without always
/// writing that phase down. When the file stored the layer's rendered
/// pixels, find the phase that reproduces them: every offset within one
/// pattern period is tried against up to 256 opaque sample pixels (RGB
/// files, 100% unrotated patterns). The fill's offset is replaced only by
/// an exact (within 8-bit rounding) match.
pub(super) fn fit_phase(
    fill: &mut Fill,
    doc: &Document,
    channels: &[(i16, Vec<u16>)],
    bounds: lumenply_tiles::Rect,
) {
    let Fill::Pattern {
        pattern,
        scale,
        offset,
        angle,
    } = fill
    else {
        return;
    };
    if *scale != 1.0 || *angle != 0.0 || bounds.w == 0 || bounds.h == 0 {
        return;
    }
    let Some(p) = doc.find_pattern(pattern) else {
        return;
    };
    let plane = |id: i16| channels.iter().find(|(c, _)| *c == id).map(|(_, v)| v);
    let (Some(r), Some(g), Some(b)) = (plane(0), plane(1), plane(2)) else {
        return;
    };
    let alpha = plane(-1);
    let n = (bounds.w * bounds.h) as usize;
    if r.len() < n || g.len() < n || b.len() < n {
        return;
    }
    // Opaque samples on a grid across the layer.
    let mut samples: Vec<(i32, i32, [i32; 3])> = Vec::new();
    let step = ((n as f64 / 1024.0).sqrt().ceil() as u32).max(1);
    let mut y = step / 2;
    while y < bounds.h && samples.len() < 256 {
        let mut x = (y / step % 2) * step / 2;
        while x < bounds.w && samples.len() < 256 {
            let i = (y * bounds.w + x) as usize;
            if alpha.is_none_or(|a| a.get(i) == Some(&65535)) {
                let v = |pl: &Vec<u16>| (pl[i] / 257) as i32;
                samples.push((bounds.x + x as i32, bounds.y + y as i32, [v(r), v(g), v(b)]));
            }
            x += step;
        }
        y += step;
    }
    if samples.len() < 16 {
        return;
    }
    let img = &p.image;
    let (pw, ph) = (img.width as i32, img.height as i32);
    let srgb: Vec<[i32; 3]> = img
        .pixels
        .iter()
        .map(|q| {
            let [cr, cg, cb, _] = q.to_straight();
            [cr, cg, cb].map(|v| crate::linear_to_srgb(v) as i32)
        })
        .collect();
    let err_at = |dx: i32, dy: i32, limit: i64| -> i64 {
        let mut e = 0i64;
        for (x, y, c) in &samples {
            let q = srgb[((y - dy).rem_euclid(ph) * pw + (x - dx).rem_euclid(pw)) as usize];
            e += ((q[0] - c[0]).abs() + (q[1] - c[1]).abs() + (q[2] - c[2]).abs()) as i64;
            if e > limit {
                break;
            }
        }
        e
    };
    let tolerance = 3 * samples.len() as i64; // a step per channel on average
    let current = (offset[0].round() as i32, offset[1].round() as i32);
    if err_at(current.0, current.1, tolerance) <= tolerance {
        return;
    }
    let mut best = (i64::MAX, 0, 0);
    for dy in 0..ph {
        for dx in 0..pw {
            let e = err_at(dx, dy, best.0.min(tolerance));
            if e < best.0 {
                best = (e, dx, dy);
            }
        }
    }
    if best.0 <= tolerance {
        *offset = [best.1 as f32, best.2 as f32];
    }
}

/// Report references whose pattern the file did not carry.
pub(super) fn warn_missing(doc: &Document, warnings: &mut Vec<String>) {
    doc.for_each_layer(|l| {
        let mut check = |r: &PatternRef, what: &str| {
            if doc.find_pattern(r).is_none() {
                warnings.push(format!(
                    "layer '{}': {what} pattern '{}' is not in the file",
                    l.name,
                    r.display_name()
                ));
            }
        };
        if let Some(po) = &l.effects.pattern_overlay {
            check(&po.pattern, "pattern overlay");
        }
        if let lumenply_doc::LayerContent::Shape(s) = &l.content {
            if let Some(Fill::Pattern { pattern, .. }) = &s.fill {
                check(pattern, "shape");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker() -> Pattern {
        let mut r = Raster::new(3, 2);
        for y in 0..2 {
            for x in 0..3 {
                let v = if (x + y) % 2 == 0 { 1.0 } else { 0.0 };
                r.set(x, y, Rgba::new(v, 0.5, 0.0, 1.0));
            }
        }
        r.set(2, 1, Rgba::from_straight(0.0, 0.0, 1.0, 0.5));
        Pattern::new("5e1713ab-e968-4c4c-8855-c8fa2cde8610", "Checks", r)
    }

    #[test]
    fn pattern_records_round_trip_with_transparency() {
        let p = checker();
        let block = patterns_block(&[&p]);
        assert_eq!(block.len() % 4, 0);
        let back = parse_patterns_block(&block);
        assert_eq!(back.len(), 1);
        let q = &back[0];
        assert_eq!((q.id.as_str(), q.name.as_str()), (p.id.as_str(), "Checks"));
        assert_eq!((q.width(), q.height()), (3, 2));
        // 8-bit sRGB storage: linear 0.5 survives within a step.
        let a = q.image.get(0, 0);
        assert!(
            (a.r - 1.0).abs() < 1e-3 && (a.g - 0.5).abs() < 4e-3 && a.a == 1.0,
            "{a:?}"
        );
        assert_eq!(q.image.get(1, 0).r, 0.0);
        let t = q.image.get(2, 1);
        assert!((t.a - 0.502).abs() < 3e-3 && (t.b - t.a).abs() < 3e-3, "{t:?}");
    }

    #[test]
    fn rle_channels_decode() {
        // Hand-built RGB 2×1 record with RLE planes: red 255,0 green 7,7 blue 0,0.
        let mut d = Vec::new();
        put32(&mut d, 1);
        put32(&mut d, 3);
        d.extend_from_slice(&1i16.to_be_bytes());
        d.extend_from_slice(&2i16.to_be_bytes());
        put32(&mut d, 2);
        d.extend_from_slice(&[0, b'P', 0, 0]);
        d.push(2);
        d.extend_from_slice(b"id");
        let mut list = Vec::new();
        for v in [0, 0, 1, 2] {
            put32(&mut list, v);
        }
        put32(&mut list, 3);
        let rle = |list: &mut Vec<u8>, packed: &[u8]| {
            put32(list, 1);
            put32(list, (23 + 2 + packed.len()) as u32);
            put32(list, 8);
            for v in [0, 0, 1, 2] {
                put32(list, v);
            }
            list.extend_from_slice(&8u16.to_be_bytes());
            list.push(1);
            list.extend_from_slice(&(packed.len() as u16).to_be_bytes());
            list.extend_from_slice(packed);
        };
        rle(&mut list, &[1, 255, 0]); // literal run of two
        rle(&mut list, &[0xff, 7]); // repeat 7 twice
        rle(&mut list, &[0xff, 0]);
        put32(&mut list, 0);
        put32(&mut list, 0);
        put32(&mut d, 3);
        put32(&mut d, list.len() as u32);
        d.extend_from_slice(&list);
        let (p, used) = read_pattern_record(&d).unwrap();
        assert_eq!(used, d.len());
        assert_eq!((p.name.as_str(), p.id.as_str()), ("P", "id"));
        assert_eq!(p.image.get(0, 0).r, 1.0);
        assert_eq!(p.image.get(1, 0).r, 0.0);
        assert!((p.image.get(1, 0).g - crate::srgb_to_linear_f(7.0 / 255.0)).abs() < 1e-6);
    }

    #[test]
    fn pattern_fill_and_overlay_descriptors_round_trip() {
        let r = checker().reference();
        let data = pattern_fill_block(&r, 0.5, [3.0, -2.0], 0.0);
        let Some(Fill::Pattern {
            pattern,
            scale,
            offset,
            angle,
        }) = parse_pattern_fill(&data)
        else {
            panic!("not a pattern fill");
        };
        assert_eq!(pattern, r);
        assert_eq!((scale, offset, angle), (0.5, [3.0, -2.0], 0.0));
        let mut fx = PatternOverlayFx::new(r.clone());
        fx.scale = 2.0;
        fx.opacity = 0.25;
        fx.link = false;
        let back = overlay_of(&overlay_desc(Desc::new(b"patternFill"), &fx), BlendMode::Multiply).unwrap();
        assert_eq!(back.pattern, r);
        assert_eq!((back.scale, back.opacity, back.link), (2.0, 0.25, false));
        assert_eq!(back.blend, BlendMode::Multiply);
    }
}
