//! Colour lookup table files for the Color Lookup adjustment.
//!
//! - **`.cube`** (Adobe's Cube LUT Specification 1.0, plus Resolve's
//!   variant): `TITLE`, `LUT_1D_SIZE` / `LUT_3D_SIZE`, `DOMAIN_MIN` /
//!   `DOMAIN_MAX`, Resolve's `LUT_1D_INPUT_RANGE` / `LUT_3D_INPUT_RANGE`
//!   (and a 1D shaper followed by a 3D table in one file), `#` comments.
//!   Data lines are three floats, red varying fastest.
//! - **`.3dl`** (Autodesk Lustre / Flame): an optional `Mesh <in> <out>`
//!   header, a line of input mesh positions whose count is the cube size,
//!   then `N³` integer triples with **blue** varying fastest. The output
//!   bit depth comes from the header, else from the largest value
//!   (rounded up to 2ⁿ − 1, as OpenColorIO does).
//!
//! Parsers never panic on bad input: every failure is a [`LutError`] that
//! names the line. [`write_cube`] writes the exact values back (shortest
//! round-trip decimals), which is how projects and PSD files carry tables.

use std::path::Path;

use lumenply_doc::lut::{Lut1D, Lut3D, LUT3D_SIZES, MAX_LUT1D};

/// Largest LUT file we read (a 65³ `.cube` is about 8 MB of text).
const MAX_FILE: u64 = 64 << 20;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum LutError {
    #[error("line {line}: {msg}")]
    Line { line: usize, msg: String },
    #[error("{0}")]
    Format(String),
    #[error("could not read the file: {0}")]
    Io(String),
}

fn at(line: usize, msg: impl Into<String>) -> LutError {
    LutError::Line {
        line,
        msg: msg.into(),
    }
}

/// The formats [`parse_lut`] understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LutFormat {
    Cube,
    ThreeDl,
}

impl LutFormat {
    /// The format a file name's extension implies.
    pub fn from_name(name: &str) -> Option<LutFormat> {
        let ext = name.rsplit('.').next()?.to_ascii_lowercase();
        match ext.as_str() {
            "cube" => Some(LutFormat::Cube),
            "3dl" => Some(LutFormat::ThreeDl),
            _ => None,
        }
    }
}

/// Load a `.cube` or `.3dl` file; the table's title falls back to the
/// file name when the file has none.
pub fn load_lut(path: impl AsRef<Path>) -> Result<Lut3D, LutError> {
    use std::io::Read;
    let path = path.as_ref();
    let f = std::fs::File::open(path).map_err(|e| LutError::Io(e.to_string()))?;
    let mut bytes = Vec::new();
    f.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| LutError::Io(e.to_string()))?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(LutError::Format("file is too large for a LUT".into()));
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut lut = parse_lut(&name, &bytes)?;
    if lut.title.is_empty() {
        lut.title = path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
    }
    Ok(lut)
}

/// Parse LUT file bytes; `name` (a file name) picks the format by its
/// extension, otherwise the content decides.
pub fn parse_lut(name: &str, bytes: &[u8]) -> Result<Lut3D, LutError> {
    let text = String::from_utf8_lossy(bytes);
    let format = LutFormat::from_name(name).unwrap_or_else(|| {
        if text.contains("LUT_3D_SIZE") || text.contains("LUT_1D_SIZE") {
            LutFormat::Cube
        } else {
            LutFormat::ThreeDl
        }
    });
    match format {
        LutFormat::Cube => parse_cube(&text),
        LutFormat::ThreeDl => parse_3dl(&text),
    }
}

fn floats<const N: usize>(words: &[&str], line: usize) -> Result<[f32; N], LutError> {
    if words.len() != N {
        return Err(at(line, format!("expected {N} numbers, found {}", words.len())));
    }
    let mut out = [0f32; N];
    for (o, w) in out.iter_mut().zip(words) {
        *o = w
            .parse::<f32>()
            .ok()
            .filter(|v| v.is_finite())
            .ok_or_else(|| at(line, format!("'{w}' is not a number")))?;
    }
    Ok(out)
}

fn size_of(words: &[&str], line: usize, range: std::ops::RangeInclusive<usize>) -> Result<usize, LutError> {
    let n = match words {
        [w] => w.parse::<usize>().ok(),
        _ => None,
    }
    .ok_or_else(|| at(line, "expected one whole number"))?;
    if !range.contains(&n) {
        return Err(at(
            line,
            format!("size {n} is outside {}..={}", range.start(), range.end()),
        ));
    }
    Ok(n)
}

/// Parse a `.cube` file.
pub fn parse_cube(text: &str) -> Result<Lut3D, LutError> {
    let mut title = String::new();
    let mut size1 = None;
    let mut size3 = None;
    let mut dmin = None;
    let mut dmax = None;
    let mut range1 = None;
    let mut range3 = None;
    let mut rows: Vec<[f32; 3]> = Vec::new();
    let mut cap = 0usize;
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let l = raw.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let words: Vec<&str> = l.split_whitespace().collect();
        let key = words[0];
        let rest = &words[1..];
        if key.starts_with(|c: char| c.is_ascii_alphabetic()) {
            if !rows.is_empty() {
                return Err(at(line, format!("keyword {key} after the table data")));
            }
            match key {
                "TITLE" => {
                    let t = l["TITLE".len()..].trim();
                    title = t.trim_matches('"').to_string();
                }
                "LUT_1D_SIZE" => size1 = Some(size_of(rest, line, 2..=MAX_LUT1D)?),
                "LUT_3D_SIZE" => size3 = Some(size_of(rest, line, LUT3D_SIZES)?),
                "DOMAIN_MIN" => dmin = Some(floats::<3>(rest, line)?),
                "DOMAIN_MAX" => dmax = Some(floats::<3>(rest, line)?),
                "LUT_1D_INPUT_RANGE" => range1 = Some(floats::<2>(rest, line)?),
                "LUT_3D_INPUT_RANGE" => range3 = Some(floats::<2>(rest, line)?),
                // Video-range flags and vendor keywords change nothing here.
                _ => {}
            }
            continue;
        }
        if size1.is_none() && size3.is_none() {
            return Err(at(line, "table data before LUT_1D_SIZE or LUT_3D_SIZE"));
        }
        if cap == 0 {
            cap = size1.unwrap_or(0) + size3.map_or(0, |n| n * n * n);
            rows.reserve_exact(cap);
        }
        if rows.len() >= cap {
            return Err(at(
                line,
                format!("more than the {cap} table rows the sizes declare"),
            ));
        }
        rows.push(floats::<3>(&words, line)?);
    }
    if size1.is_none() && size3.is_none() {
        return Err(LutError::Format(
            "no LUT_1D_SIZE or LUT_3D_SIZE: not a .cube file".into(),
        ));
    }
    let want = size1.unwrap_or(0) + size3.map_or(0, |n| n * n * n);
    if rows.len() != want {
        return Err(LutError::Format(format!(
            "the sizes declare {want} table rows but the file has {}",
            rows.len()
        )));
    }
    let both = size1.is_some() && size3.is_some();
    let range = |r: Option<[f32; 2]>| r.map(|[lo, hi]| ([lo; 3], [hi; 3]));
    // DOMAIN_* belongs to the single table (Adobe); a combined Resolve
    // file uses the per-table input ranges.
    let domain = (dmin.unwrap_or([0.0; 3]), dmax.unwrap_or([1.0; 3]));
    let shaper = size1.map(|n| {
        let (lo, hi) = if both {
            range(range1).unwrap_or(([0.0; 3], [1.0; 3]))
        } else {
            range(range1).unwrap_or(domain)
        };
        Lut1D {
            domain_min: lo,
            domain_max: hi,
            data: rows[..n].to_vec(),
        }
    });
    let lut = match size3 {
        Some(n) => {
            let (lo, hi) = range(range3).unwrap_or(domain);
            Lut3D {
                title,
                size: n,
                domain_min: lo,
                domain_max: hi,
                data: rows[size1.unwrap_or(0)..].to_vec(),
                shaper,
            }
        }
        None => Lut3D::from_1d(shaper.expect("a 1D size was declared")).with_title(&title),
    };
    lut.validate().map_err(LutError::Format)?;
    Ok(lut)
}

/// Parse an Autodesk `.3dl` file.
pub fn parse_3dl(text: &str) -> Result<Lut3D, LutError> {
    let mut out_bits: Option<u32> = None;
    let mut mesh: Option<usize> = None;
    let mut rows: Vec<[f32; 3]> = Vec::new();
    let mut cap = 0usize;
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let l = raw.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let words: Vec<&str> = l.split_whitespace().collect();
        let first = words[0];
        if first.eq_ignore_ascii_case("3DMESH")
            || first.eq_ignore_ascii_case("LUT8")
            || first.eq_ignore_ascii_case("gamma")
        {
            continue;
        }
        if first.eq_ignore_ascii_case("Mesh") {
            // `Mesh <input bits> <output bits>`.
            out_bits = words
                .get(2)
                .and_then(|w| w.parse::<u32>().ok())
                .filter(|b| (1..=24).contains(b));
            if out_bits.is_none() {
                return Err(at(line, "Mesh needs input and output bit depths"));
            }
            continue;
        }
        let ints = words
            .iter()
            .map(|w| w.parse::<i64>().ok().filter(|v| (0..=1 << 24).contains(v)))
            .collect::<Option<Vec<i64>>>()
            .ok_or_else(|| at(line, "expected whole numbers"))?;
        match mesh {
            None => {
                // The input mesh line: its count is the lattice size.
                if !LUT3D_SIZES.contains(&ints.len()) {
                    return Err(at(
                        line,
                        format!("a mesh of {} points is not a 3D LUT", ints.len()),
                    ));
                }
                mesh = Some(ints.len());
                cap = ints.len().pow(3);
                rows.reserve_exact(cap);
            }
            Some(_) => {
                if ints.len() != 3 {
                    return Err(at(line, format!("expected 3 numbers, found {}", ints.len())));
                }
                if rows.len() >= cap {
                    return Err(at(line, format!("more than the {cap} rows the mesh implies")));
                }
                rows.push([ints[0] as f32, ints[1] as f32, ints[2] as f32]);
            }
        }
    }
    let n = mesh.ok_or_else(|| LutError::Format("no input mesh line: not a .3dl file".into()))?;
    if rows.len() != cap {
        return Err(LutError::Format(format!(
            "a {n}-point mesh needs {cap} rows, the file has {}",
            rows.len()
        )));
    }
    let max = rows.iter().flatten().fold(0f32, |m, v| m.max(*v));
    let scale = match out_bits {
        Some(b) => ((1u64 << b) - 1) as f32,
        None => {
            let mut b = 8;
            while b < 24 && max > ((1u64 << b) - 1) as f32 {
                b += 2;
            }
            ((1u64 << b) - 1) as f32
        }
    };
    // Blue varies fastest in the file; red fastest in ours.
    let mut data = vec![[0f32; 3]; cap];
    for (k, row) in rows.iter().enumerate() {
        let (r, g, b) = (k / (n * n), (k / n) % n, k % n);
        data[r + n * (g + n * b)] = row.map(|v| v / scale);
    }
    let lut = Lut3D {
        title: String::new(),
        size: n,
        domain_min: [0.0; 3],
        domain_max: [1.0; 3],
        data,
        shaper: None,
    };
    lut.validate().map_err(LutError::Format)?;
    Ok(lut)
}

/// A float written so that parsing it gives the same `f32` back, always
/// with a decimal point.
fn num(v: f32) -> String {
    let mut s = format!("{v}");
    if !s.contains('.') {
        s.push_str(".0");
    }
    s
}

fn triple(out: &mut String, c: [f32; 3]) {
    out.push_str(&num(c[0]));
    out.push(' ');
    out.push_str(&num(c[1]));
    out.push(' ');
    out.push_str(&num(c[2]));
    out.push('\n');
}

fn uniform(min: [f32; 3], max: [f32; 3]) -> bool {
    min.iter().all(|v| *v == min[0]) && max.iter().all(|v| *v == max[0])
}

/// Write a table as `.cube` text. A pure 3D or pure 1D table writes the
/// Adobe form; a shaper plus a cube writes Resolve's combined form (its
/// input ranges are scalars, so per-channel domains are baked into a 65³
/// cube instead).
pub fn write_cube(lut: &Lut3D) -> String {
    let mut s = String::new();
    s.push_str("# Created by Lumenply\n");
    if !lut.title.is_empty() {
        let t: String = lut
            .title
            .chars()
            .filter(|c| *c != '"' && *c != '\n' && *c != '\r')
            .collect();
        s.push_str(&format!("TITLE \"{t}\"\n"));
    }
    let only_1d = lut.shaper.is_some()
        && lut.size == 2
        && Lut3D::identity(2).data == lut.data
        && lut.domain_min == [0.0; 3]
        && lut.domain_max == [1.0; 3];
    match &lut.shaper {
        Some(sh) if only_1d => {
            s.push_str(&format!("LUT_1D_SIZE {}\n", sh.data.len()));
            domain_lines(&mut s, sh.domain_min, sh.domain_max);
            for c in &sh.data {
                triple(&mut s, *c);
            }
        }
        Some(sh) if uniform(sh.domain_min, sh.domain_max) && uniform(lut.domain_min, lut.domain_max) => {
            s.push_str(&format!("LUT_1D_SIZE {}\n", sh.data.len()));
            s.push_str(&format!("LUT_3D_SIZE {}\n", lut.size));
            s.push_str(&format!(
                "LUT_1D_INPUT_RANGE {} {}\n",
                num(sh.domain_min[0]),
                num(sh.domain_max[0])
            ));
            s.push_str(&format!(
                "LUT_3D_INPUT_RANGE {} {}\n",
                num(lut.domain_min[0]),
                num(lut.domain_max[0])
            ));
            for c in sh.data.iter().chain(&lut.data) {
                triple(&mut s, *c);
            }
        }
        Some(_) => return write_cube(&lut.baked(*LUT3D_SIZES.end())),
        None => {
            s.push_str(&format!("LUT_3D_SIZE {}\n", lut.size));
            domain_lines(&mut s, lut.domain_min, lut.domain_max);
            for c in &lut.data {
                triple(&mut s, *c);
            }
        }
    }
    s
}

fn domain_lines(s: &mut String, min: [f32; 3], max: [f32; 3]) {
    if min != [0.0; 3] || max != [1.0; 3] {
        s.push_str(&format!(
            "DOMAIN_MIN {} {} {}\n",
            num(min[0]),
            num(min[1]),
            num(min[2])
        ));
        s.push_str(&format!(
            "DOMAIN_MAX {} {} {}\n",
            num(max[0]),
            num(max[1]),
            num(max[2])
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|c| (a[c] - b[c]).abs() < 1e-6)
    }

    /// A 2³ cube that swaps red and blue, in Adobe's layout.
    const SWAP: &str =
        "# comment\r\nTITLE \"Swap R/B\"\r\n\r\nLUT_3D_SIZE 2\r\nDOMAIN_MIN 0 0 0\r\nDOMAIN_MAX 1 1 1\r\n\
        0 0 0\n0 0 1\n0 1 0\n0 1 1\n1 0 0\n1 0 1\n1 1 0\n1 1 1\n";

    #[test]
    fn a_cube_file_parses_with_title_domain_and_order() {
        let lut = parse_cube(SWAP).unwrap();
        assert_eq!(lut.title, "Swap R/B");
        assert_eq!(lut.size, 2);
        // Red varies fastest: row 2 is (r=1, g=0, b=0) → (0, 0, 1).
        assert_eq!(lut.data[1], [0.0, 0.0, 1.0]);
        assert!(near(lut.apply([0.9, 0.5, 0.1]), [0.1, 0.5, 0.9]));
        // A domain of 0..2 halves the inputs.
        let wide = SWAP.replace("DOMAIN_MAX 1 1 1", "DOMAIN_MAX 2 2 2");
        let lut = parse_cube(&wide).unwrap();
        assert!(near(lut.apply([1.0, 0.0, 0.0]), [0.0, 0.0, 0.5]));
    }

    #[test]
    fn a_1d_cube_becomes_a_shaper() {
        // Invert each channel: two entries, 1 → 0.
        let text = "TITLE \"Invert\"\nLUT_1D_SIZE 2\n1 1 1\n0 0 0\n";
        let lut = parse_cube(text).unwrap();
        assert!(lut.shaper.is_some() && lut.size == 2);
        assert!(near(lut.apply([0.25, 0.5, 1.0]), [0.75, 0.5, 0.0]));
        // It writes back as a 1D file, value for value.
        let back = parse_cube(&write_cube(&lut)).unwrap();
        assert!(write_cube(&lut).contains("LUT_1D_SIZE 2"));
        assert_eq!(back, lut);
    }

    #[test]
    fn a_resolve_shaper_plus_cube_parses_and_round_trips() {
        // 1D: square-ish (0, 0.25, 1) over input 0..1; 3D: identity 2³
        // over 0..1. 0.5 → 0.25 through the shaper, then the cube.
        let mut text =
            String::from("LUT_1D_SIZE 3\nLUT_3D_SIZE 2\nLUT_1D_INPUT_RANGE 0 1\nLUT_3D_INPUT_RANGE 0 1\n");
        text.push_str("0 0 0\n0.25 0.25 0.25\n1 1 1\n");
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    text.push_str(&format!("{r} {g} {b}\n"));
                }
            }
        }
        let lut = parse_cube(&text).unwrap();
        assert!(near(lut.apply([0.5, 0.75, 1.0]), [0.25, 0.625, 1.0]));
        let back = parse_cube(&write_cube(&lut)).unwrap();
        assert_eq!(back, lut);
    }

    #[test]
    fn written_cubes_round_trip_exactly() {
        let lut = Lut3D::from_fn(5, |[r, g, b]| [g * 0.3 + 0.1, b, r * r]).with_title("Odd \"quotes\"");
        let text = write_cube(&lut);
        let back = parse_cube(&text).unwrap();
        assert_eq!(back.data, lut.data, "shortest round-trip decimals are exact");
        assert_eq!(back.title, "Odd quotes");
        let mut d = lut.clone();
        d.domain_min = [-0.5, 0.0, 0.0];
        d.domain_max = [1.5, 1.0, 2.0];
        assert_eq!(parse_cube(&write_cube(&d)).unwrap().domain_max, [1.5, 1.0, 2.0]);
    }

    #[test]
    fn a_3dl_file_parses_blue_fastest_and_scales_by_bit_depth() {
        // A 2-point mesh (0, 1023) and a 12-bit output swapping R and B:
        // rows run b fastest, so row k is (r=k/4, g=k/2%2, b=k%2).
        let mut text = String::from("# Lustre\n0 1023\n");
        for k in 0..8 {
            let (r, g, b) = (k / 4, (k / 2) % 2, k % 2);
            text.push_str(&format!("{} {} {}\n", b * 4095, g * 4095, r * 4095));
        }
        let lut = parse_3dl(&text).unwrap();
        assert_eq!(lut.size, 2);
        assert!(near(lut.apply([0.9, 0.5, 0.1]), [0.1, 0.5, 0.9]));
        // With a Mesh header saying 10-bit output, 1023 is white.
        let text = text.replace("4095", "1023").replace("# Lustre", "Mesh 1 10");
        let lut = parse_3dl(&text).unwrap();
        assert!(near(lut.apply([1.0, 0.0, 0.0]), [0.0, 0.0, 1.0]));
        // Without the header, a table topping out at 1023 reads as 10-bit.
        let lut = parse_3dl(&text.replace("Mesh 1 10", "")).unwrap();
        assert!(near(lut.apply([0.0, 0.0, 1.0]), [1.0, 0.0, 0.0]));
    }

    #[test]
    fn malformed_files_fail_cleanly() {
        let bad = [
            "",
            "TITLE \"x\"\n",
            "LUT_3D_SIZE 1\n0 0 0\n",
            "LUT_3D_SIZE 99\n",
            "LUT_3D_SIZE two\n",
            "0 0 0\nLUT_3D_SIZE 2\n",
            "LUT_3D_SIZE 2\n0 0 0\n",
            "LUT_3D_SIZE 2\n0 0 0\n0 0\n",
            "LUT_3D_SIZE 2\n0 0 nan\n",
            "LUT_3D_SIZE 2\nDOMAIN_MIN 1 1 1\nDOMAIN_MAX 0 0 0\n0 0 0\n0 0 1\n0 1 0\n0 1 1\n1 0 0\n1 0 1\n1 1 0\n1 1 1\n",
        ];
        for b in bad {
            assert!(parse_cube(b).is_err(), "{b:?} should fail");
        }
        let e = parse_cube("LUT_3D_SIZE 2\n0 0 x\n").unwrap_err();
        assert_eq!(e, at(2, "'x' is not a number"));
        assert!(parse_3dl("0 1023\n0 0 0\n").is_err());
        assert!(parse_3dl("Mesh 4\n").is_err());
        assert!(parse_3dl("0 a\n").is_err());
        // Every truncation of good files errors or parses, never panics.
        let mut d3 = String::from("0 1023\n");
        for k in 0..8 {
            d3.push_str(&format!("{} {} {}\n", k * 100, k * 50, 4095 - k * 300));
        }
        for text in [SWAP.to_string(), d3] {
            for cut in 0..text.len() {
                if text.is_char_boundary(cut) {
                    let _ = parse_cube(&text[..cut]);
                    let _ = parse_3dl(&text[..cut]);
                    let _ = parse_lut("x", &text.as_bytes()[..cut]);
                }
            }
            // ... and so does a byte-flip at every position.
            let bytes = text.as_bytes();
            for i in 0..bytes.len() {
                let mut b = bytes.to_vec();
                b[i] ^= 0x15;
                let _ = parse_lut("x.cube", &b);
                let _ = parse_lut("x.3dl", &b);
            }
        }
    }

    #[test]
    fn the_format_follows_the_name_then_the_content() {
        assert_eq!(LutFormat::from_name("Look.CUBE"), Some(LutFormat::Cube));
        assert_eq!(LutFormat::from_name("film.3dl"), Some(LutFormat::ThreeDl));
        assert_eq!(LutFormat::from_name("x.look"), None);
        assert!(parse_lut("", SWAP.as_bytes()).is_ok());
    }
}
