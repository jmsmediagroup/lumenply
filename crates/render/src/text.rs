//! Text rasterisation for text layers: the bundled DejaVu Sans faces, any
//! installed system family (resolved through `fontdb`), or a font file
//! named by the layer. Italic uses a real italic face when the family has
//! one and a synthetic oblique shear when it does not; lines align left,
//! centre or right against the layer's anchor.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use fontdue::layout::{CoordinateSystem, Layout, TextStyle};
use fontdue::Font;
use lumenply_doc::{TextAlign, TextLayer};
use lumenply_tiles::{Rgba, TileStore};

static REGULAR: &[u8] = include_bytes!("../fonts/DejaVuSans.ttf");
static BOLD: &[u8] = include_bytes!("../fonts/DejaVuSans-Bold.ttf");

/// Shear factor for synthetic oblique (≈ 12°).
const OBLIQUE: f32 = 0.21;

struct Resolved {
    font: Arc<Font>,
    /// The face itself is not italic, so the blit shears it.
    synthetic_oblique: bool,
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

/// Installed font families, sorted and deduplicated, for pickers.
pub fn system_font_families() -> &'static [String] {
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

/// Load (and cache) the face for a text layer. Unknown names fall back to
/// the bundled default so a document always renders.
fn resolve(t: &TextLayer) -> Arc<Resolved> {
    let key = format!("{}|{}|{}", t.font, t.bold, t.italic);
    if let Some(f) = fonts().lock().expect("font cache").get(&key) {
        return f.clone();
    }
    let bundled = |bold: bool| if bold { BOLD.to_vec() } else { REGULAR.to_vec() };
    let (bytes, face_index, real_italic): (Vec<u8>, u32, bool) =
        if !t.font.is_empty() && std::path::Path::new(&t.font).exists() {
            (
                std::fs::read(&t.font).unwrap_or_else(|_| bundled(t.bold)),
                0,
                false,
            )
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
                    let italic = font_db()
                        .face(id)
                        .is_some_and(|f| f.style != fontdb::Style::Normal);
                    // macOS families usually live in .ttc collections, so the
                    // face index matters as much as the bytes.
                    match font_db().with_face_data(id, |data, index| (data.to_vec(), index)) {
                        Some((data, index)) => (data, index, italic),
                        None => (bundled(t.bold), 0, false),
                    }
                }
                None => (bundled(t.bold), 0, false),
            }
        } else {
            (bundled(t.bold), 0, false)
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
    });
    fonts().lock().expect("font cache").insert(key, resolved.clone());
    resolved
}

/// Public for callers that only need the face (previews, metrics).
pub fn font_for(t: &TextLayer) -> Arc<Font> {
    resolve(t).font.clone()
}

/// Rasterise a text layer into a sparse tile store at its canvas position.
pub fn rasterize(t: &TextLayer) -> TileStore {
    let mut store = TileStore::new();
    if t.text.trim().is_empty() || t.size <= 0.5 || !t.size.is_finite() {
        return store;
    }
    let resolved = resolve(t);
    let font = resolved.font.as_ref();
    let fonts_arr = [font];
    let [cr, cg, cb, ca] = t.color;
    let ascent = font
        .horizontal_line_metrics(t.size)
        .map_or(t.size * 0.8, |m| m.ascent);
    let line_step = t.line_height.max(0.5) * t.size;

    // Lay each line out on its own so alignment can place it.
    struct Line {
        glyphs: Vec<fontdue::layout::GlyphPosition>,
        width: f32,
    }
    let mut lines = Vec::new();
    for line in t.text.split('\n') {
        let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
        layout.reset(&fontdue::layout::LayoutSettings::default());
        layout.append(&fonts_arr, &TextStyle::new(line, t.size, 0));
        let glyphs: Vec<_> = layout.glyphs().clone();
        let width = glyphs.iter().map(|g| g.x + g.width as f32).fold(0.0f32, f32::max);
        lines.push(Line { glyphs, width });
    }

    for (i, line) in lines.iter().enumerate() {
        let align_dx = match t.align {
            TextAlign::Left => 0.0,
            TextAlign::Center => -line.width / 2.0,
            TextAlign::Right => -line.width,
        };
        let baseline_y = t.y + i as f32 * line_step;
        let oy = (baseline_y - ascent).round() as i32;
        for g in &line.glyphs {
            if g.width == 0 || g.height == 0 {
                continue;
            }
            let (metrics, bitmap) = font.rasterize_config(g.key);
            let gx = (t.x + align_dx + g.x).round() as i32;
            let gy = oy + g.y.round() as i32;
            for row in 0..metrics.height {
                let py = gy + row as i32;
                // Synthetic oblique: shear rows around the baseline.
                let shear = if resolved.synthetic_oblique {
                    ((baseline_y - py as f32) * OBLIQUE).round() as i32
                } else {
                    0
                };
                for col in 0..metrics.width {
                    let cov = bitmap[row * metrics.width + col] as f32 / 255.0;
                    if cov <= 0.0 {
                        continue;
                    }
                    let px = gx + col as i32 + shear;
                    let dst = store.get_pixel(px, py);
                    store.set_pixel(px, py, Rgba::from_straight(cr, cg, cb, ca * cov).over(dst));
                }
            }
        }
    }
    store.prune_blank();
    store
}

/// Rasterise into the layer's own cache.
pub fn refresh_cache(t: &mut TextLayer) {
    t.cache = Some(rasterize(t));
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
