//! Text rasterisation for text layers, using the bundled DejaVu Sans fonts
//! (Bitstream Vera licence) or a font file named by the layer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use fontdue::layout::{CoordinateSystem, Layout, TextStyle};
use fontdue::Font;
use nge_doc::TextLayer;
use nge_tiles::{Rgba, TileStore};

static REGULAR: &[u8] = include_bytes!("../fonts/DejaVuSans.ttf");
static BOLD: &[u8] = include_bytes!("../fonts/DejaVuSans-Bold.ttf");

fn fonts() -> &'static Mutex<HashMap<String, Arc<Font>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Font>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Load (and cache) the font for a text layer. Unknown names fall back to
/// the bundled default so a document always renders.
pub fn font_for(t: &TextLayer) -> Arc<Font> {
    let key = format!("{}|{}", t.font, t.bold);
    if let Some(f) = fonts().lock().expect("font cache").get(&key) {
        return f.clone();
    }
    let bytes: Vec<u8> = if !t.font.is_empty() && std::path::Path::new(&t.font).exists() {
        std::fs::read(&t.font).unwrap_or_else(|_| if t.bold { BOLD.to_vec() } else { REGULAR.to_vec() })
    } else if t.bold {
        BOLD.to_vec()
    } else {
        REGULAR.to_vec()
    };
    let font = Font::from_bytes(bytes, fontdue::FontSettings::default()).unwrap_or_else(|_| {
        Font::from_bytes(REGULAR, fontdue::FontSettings::default()).expect("bundled font")
    });
    let font = Arc::new(font);
    fonts().lock().expect("font cache").insert(key, font.clone());
    font
}

/// Rasterise a text layer into a sparse tile store at its canvas position.
pub fn rasterize(t: &TextLayer) -> TileStore {
    let mut store = TileStore::new();
    if t.text.trim().is_empty() || t.size <= 0.5 {
        return store;
    }
    let font = font_for(t);
    let fonts = [font.as_ref()];
    let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
    layout.reset(&fontdue::layout::LayoutSettings {
        x: 0.0,
        y: 0.0,
        line_height: t.line_height.max(0.5),
        ..Default::default()
    });
    layout.append(&fonts, &TextStyle::new(&t.text, t.size, 0));
    let [cr, cg, cb, ca] = t.color;
    // The layout's y origin is the top of the first line; shift so (x, y)
    // is the first baseline instead.
    let ascent = font
        .horizontal_line_metrics(t.size)
        .map_or(t.size * 0.8, |m| m.ascent);
    let ox = t.x.round() as i32;
    let oy = (t.y - ascent).round() as i32;
    for g in layout.glyphs() {
        if g.width == 0 || g.height == 0 {
            continue;
        }
        let (metrics, bitmap) = font.rasterize_config(g.key);
        let gx = ox + g.x.round() as i32;
        let gy = oy + g.y.round() as i32;
        for row in 0..metrics.height {
            for col in 0..metrics.width {
                let cov = bitmap[row * metrics.width + col] as f32 / 255.0;
                if cov <= 0.0 {
                    continue;
                }
                let (px, py) = (gx + col as i32, gy + row as i32);
                let dst = store.get_pixel(px, py);
                store.set_pixel(px, py, Rgba::from_straight(cr, cg, cb, ca * cov).over(dst));
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
        // Colour is the requested red, with partial alpha on edges.
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
        // Two lines stack downward.
        let t2 = TextLayer::new("A\nA", 0.0, 50.0, 30.0, [0.0; 4]);
        let two = rasterize(&TextLayer {
            color: [0.0, 0.0, 0.0, 1.0],
            ..t2
        });
        let one = rasterize(&TextLayer::new("A", 0.0, 50.0, 30.0, [0.0, 0.0, 0.0, 1.0]));
        assert!(two.content_bounds().unwrap().h > one.content_bounds().unwrap().h * 3 / 2);
        assert!(rasterize(&TextLayer::new("   ", 0.0, 0.0, 30.0, [0.0, 0.0, 0.0, 1.0])).is_empty());
    }
}
