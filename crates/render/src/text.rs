//! Text rasterisation for text layers: the bundled DejaVu Sans faces, any
//! installed system family (resolved through `fontdb`), or a font file
//! named by the layer. Italic uses a real italic face when the family has
//! one and a synthetic oblique shear when it does not; bold likewise uses
//! a real bold face or a synthetic horizontal emboldening. Where each
//! glyph goes (lines, wrapping, alignment) comes from [`crate::text_layout`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use fontdue::Font;
use lumenply_doc::{Layer, TextLayer};
use lumenply_tiles::{Rgba, TileStore};

static REGULAR: &[u8] = include_bytes!("../fonts/DejaVuSans.ttf");
static BOLD: &[u8] = include_bytes!("../fonts/DejaVuSans-Bold.ttf");

/// The family every text layer falls back to; always available.
pub const BUNDLED_FAMILY: &str = "DejaVu Sans";

/// Shear factor for synthetic oblique (≈ 12°).
pub(crate) const OBLIQUE: f32 = 0.21;

/// Synthetic bold widens every stem by this fraction of the font size
/// (FreeType's emboldening strength), never by less than one pixel.
pub(crate) const EMBOLDEN: f32 = 1.0 / 24.0;

/// Faces at or above this weight count as bold; lighter faces asked for
/// bold are emboldened synthetically.
const BOLD_WEIGHT: u16 = 600;

pub(crate) struct Resolved {
    pub(crate) font: Arc<Font>,
    /// The face itself is not italic, so the blit shears it.
    pub(crate) synthetic_oblique: bool,
    /// The face itself is not bold, so the blit thickens it.
    pub(crate) synthetic_bold: bool,
}

fn fonts() -> &'static Mutex<HashMap<String, Arc<Resolved>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Resolved>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn font_db() -> &'static fontdb::Database {
    static DB: OnceLock<fontdb::Database> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        db
    })
}

/// Every family name fontdb knows, hidden system families included.
fn all_families() -> &'static [String] {
    static LIST: OnceLock<Vec<String>> = OnceLock::new();
    LIST.get_or_init(|| {
        let mut names: Vec<String> = font_db()
            .faces()
            .flat_map(|f| f.families.iter().map(|(n, _)| n.clone()))
            .collect();
        names.sort();
        names.dedup();
        names
    })
}

/// Installed font families for pickers: sorted, deduplicated, and without
/// the dot-prefixed families macOS keeps for its own UI.
pub fn system_font_families() -> &'static [String] {
    static LIST: OnceLock<Vec<String>> = OnceLock::new();
    LIST.get_or_init(|| {
        all_families()
            .iter()
            .filter(|n| !n.starts_with('.'))
            .cloned()
            .collect()
    })
}

/// True when a layer's `font` names a font file rather than a family.
pub fn is_font_path(font: &str) -> bool {
    let lower = font.to_ascii_lowercase();
    font.contains('/')
        || font.contains('\\')
        || [".ttf", ".otf", ".ttc", ".otc"]
            .iter()
            .any(|e| lower.ends_with(e))
}

/// Whether a layer's `font` renders as itself on this machine: the empty
/// default and DejaVu Sans are bundled, a file must exist, and a family
/// must be installed (family names match exactly, as fontdb does).
pub fn font_is_available(font: &str) -> bool {
    if font.is_empty() || font == BUNDLED_FAMILY {
        return true;
    }
    if is_font_path(font) {
        return std::path::Path::new(font).is_file();
    }
    all_families().iter().any(|n| n == font)
}

/// The fonts named by text layers (searched through groups) that are not
/// available, sorted and deduplicated. Those layers render in DejaVu Sans.
pub fn missing_fonts(layers: &[Layer]) -> Vec<String> {
    fn walk(layers: &[Layer], out: &mut Vec<String>) {
        for l in layers {
            if let Some(children) = l.children() {
                walk(children, out);
            } else if let Some(t) = l.text_layer() {
                if !font_is_available(&t.font) {
                    out.push(t.font.clone());
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(layers, &mut out);
    out.sort();
    out.dedup();
    out
}

/// A human name for a layer's `font`: the bundled family for the empty
/// default, the file name for a font file, else the family itself.
pub fn font_display_name(font: &str) -> String {
    if font.is_empty() {
        BUNDLED_FAMILY.to_string()
    } else if is_font_path(font) {
        std::path::Path::new(font)
            .file_name()
            .map_or_else(|| font.to_string(), |n| n.to_string_lossy().into_owned())
    } else {
        font.to_string()
    }
}

/// Family plus style, e.g. "Helvetica Bold Italic", for messages.
pub fn font_label(t: &TextLayer) -> String {
    let style = match (t.bold, t.italic) {
        (true, true) => " Bold Italic",
        (true, false) => " Bold",
        (false, true) => " Italic",
        (false, false) => "",
    };
    format!("{}{style}", font_display_name(&t.font))
}

/// Load (and cache) the face for a text layer. Unknown names fall back to
/// the bundled default so a document always renders.
pub(crate) fn resolve(t: &TextLayer) -> Arc<Resolved> {
    let key = format!("{}|{}|{}", t.font, t.bold, t.italic);
    if let Some(f) = fonts().lock().expect("font cache").get(&key) {
        return f.clone();
    }
    let bundled = |bold: bool| if bold { BOLD.to_vec() } else { REGULAR.to_vec() };
    // (bytes, face index, the face is italic, the face is bold)
    let (bytes, face_index, real_italic, real_bold): (Vec<u8>, u32, bool, bool) =
        if !t.font.is_empty() && std::path::Path::new(&t.font).is_file() {
            match std::fs::read(&t.font) {
                Ok(data) => {
                    // A lone file is one face: read its own style so a bold
                    // or italic file is not thickened or sheared again.
                    let mut db = fontdb::Database::new();
                    db.load_font_data(data.clone());
                    let (italic, bold) = db.faces().next().map_or((false, false), |f| {
                        (f.style != fontdb::Style::Normal, f.weight.0 >= BOLD_WEIGHT)
                    });
                    (data, 0, italic, bold)
                }
                Err(_) => (bundled(t.bold), 0, false, t.bold),
            }
        } else if !t.font.is_empty() {
            // A family name: ask fontdb for the closest face.
            let query = fontdb::Query {
                families: &[fontdb::Family::Name(&t.font)],
                weight: if t.bold {
                    fontdb::Weight::BOLD
                } else {
                    fontdb::Weight::NORMAL
                },
                stretch: fontdb::Stretch::Normal,
                style: if t.italic {
                    fontdb::Style::Italic
                } else {
                    fontdb::Style::Normal
                },
            };
            match font_db().query(&query) {
                Some(id) => {
                    let (italic, bold) = font_db().face(id).map_or((false, false), |f| {
                        (f.style != fontdb::Style::Normal, f.weight.0 >= BOLD_WEIGHT)
                    });
                    // macOS families usually live in .ttc collections, so the
                    // face index matters as much as the bytes.
                    match font_db().with_face_data(id, |data, index| (data.to_vec(), index)) {
                        Some((data, index)) => (data, index, italic, bold),
                        None => (bundled(t.bold), 0, false, t.bold),
                    }
                }
                None => (bundled(t.bold), 0, false, t.bold),
            }
        } else {
            (bundled(t.bold), 0, false, t.bold)
        };
    let settings = fontdue::FontSettings {
        collection_index: face_index,
        ..fontdue::FontSettings::default()
    };
    let font = Font::from_bytes(bytes, settings).unwrap_or_else(|_| {
        Font::from_bytes(REGULAR, fontdue::FontSettings::default()).expect("bundled font")
    });
    let resolved = Arc::new(Resolved {
        font: Arc::new(font),
        synthetic_oblique: t.italic && !real_italic,
        synthetic_bold: t.bold && !real_bold,
    });
    fonts().lock().expect("font cache").insert(key, resolved.clone());
    resolved
}

/// Public for callers that only need the face (previews, metrics).
pub fn font_for(t: &TextLayer) -> Arc<Font> {
    resolve(t).font.clone()
}

/// Rasterise a text layer into a sparse tile store at its canvas position,
/// glyph by glyph from its [`layout`](crate::text_layout::layout).
pub fn rasterize(t: &TextLayer) -> TileStore {
    let mut store = TileStore::new();
    if t.text.trim().is_empty() || t.size <= 0.5 || !t.size.is_finite() {
        return store;
    }
    let lay = crate::text_layout::layout(t);
    let font = lay.font.as_ref();
    let [cr, cg, cb, ca] = t.color;
    for g in &lay.glyphs {
        let line = &lay.lines[g.line];
        if line.hidden {
            continue;
        }
        let (metrics, bitmap) = font.rasterize_indexed(g.glyph, lay.size);
        if metrics.width == 0 || metrics.height == 0 {
            continue;
        }
        let (width, bitmap) = if lay.bold_px > 0.0 {
            embolden(&bitmap, metrics.width, metrics.height, lay.bold_px)
        } else {
            (metrics.width, bitmap)
        };
        let baseline_y = line.baseline;
        let gx = g.x.round() as i32 + metrics.xmin;
        let gy = baseline_y.round() as i32 - metrics.ymin - metrics.height as i32;
        for row in 0..metrics.height {
            let py = gy + row as i32;
            // Synthetic oblique: shear rows around the baseline.
            let shear = if lay.oblique {
                ((baseline_y - py as f32) * OBLIQUE).round() as i32
            } else {
                0
            };
            for col in 0..width {
                let cov = bitmap[row * width + col] as f32 / 255.0;
                if cov <= 0.0 {
                    continue;
                }
                let px = gx + col as i32 + shear;
                let dst = store.get_pixel(px, py);
                store.set_pixel(px, py, Rgba::from_straight(cr, cg, cb, ca * cov).over(dst));
            }
        }
    }
    store.prune_blank();
    store
}

/// Thicken a coverage bitmap horizontally by `strength` pixels: each output
/// pixel takes the strongest coverage within `strength` pixels to its left
/// (the last, fractional pixel weighted by its fraction). Returns the new
/// row width and bitmap.
fn embolden(bitmap: &[u8], w: usize, h: usize, strength: f32) -> (usize, Vec<u8>) {
    let whole = strength.floor() as usize;
    let frac = strength - whole as f32;
    let reach = whole + usize::from(frac > 0.0);
    let out_w = w + reach;
    let mut out = vec![0u8; out_w * h];
    for row in 0..h {
        let src = &bitmap[row * w..(row + 1) * w];
        for x in 0..out_w {
            let at = |d: usize| x.checked_sub(d).and_then(|i| src.get(i)).copied().unwrap_or(0);
            let mut v = (0..=whole).map(at).max().unwrap_or(0) as f32;
            if frac > 0.0 {
                v = v.max(at(whole + 1) as f32 * frac);
            }
            out[row * out_w + x] = v.round() as u8;
        }
    }
    (out_w, out)
}

/// Rasterise into the layer's own cache.
pub fn refresh_cache(t: &mut TextLayer) {
    t.cache = Some(rasterize(t));
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumenply_doc::TextAlign;

    #[test]
    fn text_renders_glyphs_at_the_baseline_origin() {
        let t = TextLayer::new("Hello", 100.0, 200.0, 40.0, [1.0, 0.0, 0.0, 1.0]);
        let store = rasterize(&t);
        let b = store.content_bounds().expect("some glyphs");
        assert!(b.x >= 98 && b.x <= 110, "starts near x=100: {b:?}");
        assert!(
            b.bottom() <= 203 && b.bottom() >= 195,
            "sits on the baseline y=200: {b:?}"
        );
        assert!(b.y < 180, "capitals rise above the baseline: {b:?}");
        assert!(b.w > 80 && b.w < 160, "five glyphs at 40px: {b:?}");
        let mut saw_edge = false;
        for y in b.y..b.bottom() {
            for x in b.x..b.right() {
                let p = store.get_pixel(x, y);
                if p.a > 0.0 {
                    let s = p.to_straight();
                    assert!((s[0] - 1.0).abs() < 1e-3 && s[1] < 1e-3);
                    if p.a < 0.9 {
                        saw_edge = true;
                    }
                }
            }
        }
        assert!(saw_edge, "antialiased glyph edges");
        let t2 = TextLayer::new("A\nA", 0.0, 50.0, 30.0, [0.0; 4]);
        let two = rasterize(&TextLayer {
            color: [0.0, 0.0, 0.0, 1.0],
            ..t2
        });
        let one = rasterize(&TextLayer::new("A", 0.0, 50.0, 30.0, [0.0, 0.0, 0.0, 1.0]));
        assert!(two.content_bounds().unwrap().h > one.content_bounds().unwrap().h * 3 / 2);
        assert!(rasterize(&TextLayer::new("   ", 0.0, 0.0, 30.0, [0.0, 0.0, 0.0, 1.0])).is_empty());
    }

    #[test]
    fn alignment_places_lines_against_the_anchor() {
        let base = TextLayer::new("Wide line\nX", 300.0, 100.0, 30.0, [0.0, 0.0, 0.0, 1.0]);
        let left = rasterize(&base);
        let lb = left.content_bounds().unwrap();
        assert!(lb.x >= 298, "left-aligned starts at the anchor: {lb:?}");

        let right = rasterize(&TextLayer {
            align: TextAlign::Right,
            ..base.clone()
        });
        let rb = right.content_bounds().unwrap();
        assert!(
            (rb.right() - 300).abs() <= 3,
            "right-aligned ends at the anchor: {rb:?}"
        );

        let centre = rasterize(&TextLayer {
            align: TextAlign::Center,
            ..base.clone()
        });
        let cb = centre.content_bounds().unwrap();
        let mid = cb.x + cb.w as i32 / 2;
        assert!((mid - 300).abs() <= 4, "centred on the anchor: {cb:?}");
        // The short second line moves with the alignment too: under Right,
        // the "X" must end near 300 as well, so the bounds stay anchored.
        assert!(rb.w >= lb.w - 2 && rb.w <= lb.w + 2, "same overall width");
    }

    #[test]
    fn tracking_widens_and_tightens_the_line() {
        let base = TextLayer::new("lllll", 0.0, 60.0, 40.0, [0.0, 0.0, 0.0, 1.0]);
        let w = |tracking: f32| {
            rasterize(&TextLayer {
                tracking,
                ..base.clone()
            })
            .content_bounds()
            .unwrap()
            .w
        };
        let (normal, wide, tight) = (w(0.0), w(200.0), w(-50.0));
        // 4 gaps × 200/1000 em × 40 px = 32 px wider.
        assert!(
            (wide as i32 - normal as i32 - 32).abs() <= 2,
            "tracking 200 adds 4 × 8 px: {normal} → {wide}"
        );
        assert!(tight < normal, "negative tracking tightens: {normal} → {tight}");
    }

    #[test]
    fn synthetic_oblique_shears_and_real_families_resolve() {
        // The bundled face has no italic variant, so italic is synthetic:
        // the top of a tall glyph leans right of its base.
        let upright = rasterize(&TextLayer::new("ll", 50.0, 80.0, 48.0, [0.0, 0.0, 0.0, 1.0]));
        let italic = rasterize(&TextLayer {
            italic: true,
            ..TextLayer::new("ll", 50.0, 80.0, 48.0, [0.0, 0.0, 0.0, 1.0])
        });
        let (ub, ib) = (
            upright.content_bounds().unwrap(),
            italic.content_bounds().unwrap(),
        );
        assert!(
            ib.w > ub.w + 3,
            "sheared glyphs cover a wider box: {ub:?} vs {ib:?}"
        );

        // A real installed family resolves through fontdb (skip quietly on
        // systems without one of the common names).
        let fam = ["Helvetica", "Arial", "DejaVu Sans", "Liberation Sans"]
            .iter()
            .find(|f| system_font_families().iter().any(|n| n == *f));
        let Some(fam) = fam else {
            eprintln!("no common system family found; skipping fontdb check");
            return;
        };
        let sys = rasterize(&TextLayer {
            font: (*fam).to_string(),
            ..TextLayer::new("Hi", 0.0, 40.0, 32.0, [0.0, 0.0, 0.0, 1.0])
        });
        assert!(sys.content_bounds().is_some(), "system family renders");
    }

    const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
    const PLEX_MONO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fonts/IBMPlexMono-Regular.ttf");

    fn styled(text: &str, font: &str, bold: bool, italic: bool) -> TextLayer {
        TextLayer {
            font: font.to_string(),
            bold,
            italic,
            ..TextLayer::new(text, 40.0, 100.0, 48.0, BLACK)
        }
    }

    /// Alpha-weighted mean x of the ink in rows `y0..y1`.
    fn ink_centre_x(s: &TileStore, y0: i32, y1: i32) -> f32 {
        let b = s.content_bounds().unwrap();
        let (mut sum, mut wsum) = (0.0f32, 0.0f32);
        for y in y0..y1 {
            for x in b.x..b.right() {
                let a = s.get_pixel(x, y).a;
                sum += a * x as f32;
                wsum += a;
            }
        }
        sum / wsum
    }

    fn ink(s: &TileStore) -> f32 {
        let b = s.content_bounds().unwrap();
        let mut total = 0.0;
        for y in b.y..b.bottom() {
            for x in b.x..b.right() {
                total += s.get_pixel(x, y).a;
            }
        }
        total
    }

    #[test]
    fn embolden_smears_coverage_rightwards() {
        // One lit pixel, two pixels of strength: it covers three columns.
        let (w, out) = embolden(&[0, 255, 0], 3, 1, 2.0);
        assert_eq!(w, 5);
        assert_eq!(out, vec![0, 255, 255, 255, 0]);
        // A fractional strength adds a partial last column.
        let (w, out) = embolden(&[0, 255, 0], 3, 1, 1.5);
        assert_eq!(w, 5);
        assert_eq!(out, vec![0, 255, 255, 128, 0]);
        // Rows stay independent.
        let (w, out) = embolden(&[200, 0, 0, 100], 2, 2, 1.0);
        assert_eq!(w, 3);
        assert_eq!(out, vec![200, 200, 0, 0, 100, 100]);
    }

    #[test]
    fn bold_italic_on_the_bundled_family_is_bold_and_sheared() {
        // DejaVu Sans ships a bold face but no italic: bold is real, italic
        // is a synthetic oblique of the bold face.
        let regular = rasterize(&styled("l", "", false, false));
        let bold = rasterize(&styled("l", "", true, false));
        let bold_italic = rasterize(&styled("l", "", true, true));
        assert!(!resolve(&styled("l", "", true, true)).synthetic_bold);
        assert!(resolve(&styled("l", "", true, true)).synthetic_oblique);

        // The bold stem is heavier: DejaVu's "l" stem is ~6 px at 48 px,
        // the bold one ~9 px.
        let (rb, bb) = (regular.content_bounds().unwrap(), bold.content_bounds().unwrap());
        assert!(bb.w >= rb.w + 2, "bold stem is wider: {rb:?} vs {bb:?}");
        assert!(ink(&bold) > ink(&regular) * 1.3, "bold carries more ink");

        // Upright bold: top and bottom of the stem line up.
        let upright_lean =
            ink_centre_x(&bold, bb.y, bb.y + 8) - ink_centre_x(&bold, bb.bottom() - 8, bb.bottom());
        assert!(
            upright_lean.abs() < 1.0,
            "bold alone does not lean: {upright_lean}"
        );

        // Bold italic: the top of the stem leans right of its foot by the
        // oblique factor times the distance between the two bands.
        let bib = bold_italic.content_bounds().unwrap();
        let top = ink_centre_x(&bold_italic, bib.y, bib.y + 8);
        let foot = ink_centre_x(&bold_italic, bib.bottom() - 8, bib.bottom());
        let expected = OBLIQUE * (bib.h as f32 - 8.0);
        assert!(
            (top - foot - expected).abs() < 1.5,
            "lean {} px, expected {expected} px",
            top - foot
        );
        assert!(
            (ink(&bold_italic) - ink(&bold)).abs() < ink(&bold) * 0.05,
            "shear keeps the ink"
        );
    }

    #[test]
    fn synthetic_bold_thickens_a_face_without_a_bold_weight() {
        // IBM Plex Mono Regular is a single regular face, named by path.
        let font = PLEX_MONO;
        assert!(resolve(&styled("lll", font, true, false)).synthetic_bold);
        assert!(!resolve(&styled("lll", font, false, false)).synthetic_bold);
        let regular = rasterize(&styled("lll", font, false, false));
        let bold = rasterize(&styled("lll", font, true, false));
        let (rb, bb) = (regular.content_bounds().unwrap(), bold.content_bounds().unwrap());
        // 48 px / 24 = 2 px per glyph: two widened advances plus the last
        // glyph's own smear = 6 px wider overall.
        assert!(
            (bb.w as i32 - rb.w as i32 - 6).abs() <= 1,
            "bold widens by 3 x 2 px: {rb:?} -> {bb:?}"
        );
        assert_eq!(bb.x, rb.x, "emboldening grows rightwards from the same start");
        assert!(ink(&bold) > ink(&regular) * 1.3, "bold carries more ink");

        // Italic on the same face is synthetic too, and combines with bold:
        // Plex's "l" has a flag and a foot, so compare against the upright
        // bold glyph's own lean rather than against zero.
        assert!(resolve(&styled("l", font, true, true)).synthetic_oblique);
        let lean = |s: &TileStore| {
            let b = s.content_bounds().unwrap();
            ink_centre_x(s, b.y, b.y + 8) - ink_centre_x(s, b.bottom() - 8, b.bottom())
        };
        let upright = rasterize(&styled("l", font, true, false));
        let sheared = rasterize(&styled("l", font, true, true));
        let h = sheared.content_bounds().unwrap().h as f32;
        let extra = lean(&sheared) - lean(&upright);
        let expected = OBLIQUE * (h - 8.0);
        assert!(
            (extra - expected).abs() < 1.5,
            "bold italic leans {extra} px more than bold, expected {expected}"
        );
    }

    #[test]
    fn system_families_use_real_faces_or_synthesise_italic() {
        // A family with a real italic face renders it unsheared; a family
        // with only upright faces gets the synthetic oblique. Skip quietly
        // when this machine has neither kind.
        let db = font_db();
        let families = system_font_families();
        let has_italic = |fam: &str| {
            db.faces()
                .any(|f| f.families.iter().any(|(n, _)| n == fam) && f.style != fontdb::Style::Normal)
        };
        if let Some(fam) = families.iter().find(|f| has_italic(f)) {
            assert!(
                !resolve(&styled("l", fam, false, true)).synthetic_oblique,
                "{fam}"
            );
        }
        if let Some(fam) = families.iter().find(|f| !has_italic(f)) {
            assert!(resolve(&styled("l", fam, false, true)).synthetic_oblique, "{fam}");
            assert!(rasterize(&styled("l", fam, false, true))
                .content_bounds()
                .is_some());
        }
    }

    #[test]
    fn font_availability_and_missing_fonts() {
        const MISSING: &str = "Lumenply Missing Font 9f3a";
        assert!(font_is_available(""), "empty means the bundled face");
        assert!(font_is_available(BUNDLED_FAMILY));
        assert!(!font_is_available(MISSING));
        assert!(font_is_available(PLEX_MONO), "an existing font file");
        assert!(!font_is_available("/nonexistent/dir/Nope.ttf"));
        if let Some(fam) = system_font_families().first() {
            assert!(font_is_available(fam), "an installed family: {fam}");
        }
        assert!(
            system_font_families().iter().all(|n| !n.starts_with('.')),
            "hidden system families stay out of pickers"
        );

        // A missing family renders exactly like the bundled default.
        let fallback = rasterize(&styled("Hg", MISSING, false, false));
        let default = rasterize(&styled("Hg", "", false, false));
        let (fb, db) = (
            fallback.content_bounds().unwrap(),
            default.content_bounds().unwrap(),
        );
        assert_eq!(fb, db);
        for y in db.y..db.bottom() {
            for x in db.x..db.right() {
                assert_eq!(fallback.get_pixel(x, y), default.get_pixel(x, y), "({x}, {y})");
            }
        }

        // The document query walks groups and reports each name once.
        let text = |id: lumenply_doc::LayerId, font: &str| Layer::text(id, styled("A", font, false, false));
        let mut group = Layer::group(10, "Group");
        group.children_mut().unwrap().push(text(11, MISSING));
        group
            .children_mut()
            .unwrap()
            .push(text(12, "/nonexistent/dir/Nope.ttf"));
        let layers = vec![text(1, ""), text(2, MISSING), group, text(3, BUNDLED_FAMILY)];
        assert_eq!(
            missing_fonts(&layers),
            vec!["/nonexistent/dir/Nope.ttf".to_string(), MISSING.to_string()]
        );
        assert!(missing_fonts(&layers[..1]).is_empty());
    }

    #[test]
    fn font_names_for_messages() {
        assert_eq!(font_display_name(""), "DejaVu Sans");
        assert_eq!(font_display_name("Helvetica"), "Helvetica");
        assert_eq!(font_display_name("/Library/Fonts/My Face.otf"), "My Face.otf");
        assert!(is_font_path("C:\\Fonts\\a.ttf") && is_font_path("face.TTC"));
        assert!(!is_font_path("Times New Roman"));
        assert_eq!(
            font_label(&styled("A", "", true, true)),
            "DejaVu Sans Bold Italic"
        );
        assert_eq!(font_label(&styled("A", "Georgia", false, true)), "Georgia Italic");
        assert_eq!(font_label(&styled("A", "Georgia", true, false)), "Georgia Bold");
        assert_eq!(font_label(&styled("A", "Georgia", false, false)), "Georgia");
    }
}
