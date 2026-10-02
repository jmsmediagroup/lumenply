//! Text layout shared by the rasteriser and on-canvas editing.
//!
//! [`layout`] places every character of a text layer: explicit line
//! breaks, word wrap inside a paragraph box, alignment (left, centre,
//! right, justify), tracking, leading, baseline shift and all caps. The
//! same layout answers the editing questions — where the caret for a
//! character position sits ([`TextLayout::caret`]), which position a click
//! lands on ([`TextLayout::hit`]) and which rectangles a selection covers
//! ([`TextLayout::selection_rects`]) — so the caret always matches the
//! rendered glyphs. Positions are byte offsets into the layer's text, at
//! character boundaries.

use std::sync::Arc;

use fontdue::Font;
use lumenply_doc::{TextAlign, TextLayer};

use crate::text::{resolve, EMBOLDEN};

/// One laid-out glyph.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedGlyph {
    /// Byte offset in the layer's text of the character this glyph shows
    /// (all caps can show one character as several glyphs).
    pub byte: usize,
    /// Glyph index in the resolved face.
    pub glyph: u16,
    /// Pen position: the left edge of the glyph's advance, canvas px.
    pub x: f32,
    /// Advance including tracking, synthetic bold and justification.
    pub advance: f32,
    /// Index into [`TextLayout::lines`].
    pub line: usize,
}

/// One visual line.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutLine {
    /// Byte range of the text on this line. A newline itself belongs to no
    /// line; the trailing spaces of a wrapped line belong to it.
    pub start: usize,
    pub end: usize,
    /// Baseline the glyphs sit on (baseline shift applied), canvas px.
    pub baseline: f32,
    /// Caret stops left to right: (byte offset, x) for every character
    /// boundary on the line; the first is `start`, the last `end`.
    pub stops: Vec<(usize, f32)>,
    /// The line ends at a word wrap rather than a newline or the end.
    pub wrapped: bool,
    /// Box text: the line falls below the box and is not drawn.
    pub hidden: bool,
}

/// Where the caret for a position sits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Caret {
    pub x: f32,
    /// Top and bottom of the caret bar (ascent above, descent below the
    /// baseline), canvas px.
    pub top: f32,
    pub bottom: f32,
    pub line: usize,
}

/// A laid-out text layer. See the module docs.
#[derive(Clone)]
pub struct TextLayout {
    pub glyphs: Vec<PlacedGlyph>,
    /// Never empty: empty text still has one line holding the caret.
    pub lines: Vec<LayoutLine>,
    /// Font ascent and descent at the layer's size, both positive.
    pub ascent: f32,
    pub descent: f32,
    /// Baseline-to-baseline distance (the leading).
    pub line_step: f32,
    pub(crate) size: f32,
    pub(crate) font: Arc<Font>,
    pub(crate) bold_px: f32,
    pub(crate) oblique: bool,
}

/// A displayed character before line breaking.
struct Item {
    byte: usize,
    glyph: u16,
    advance: f32,
    /// Right edge of the glyph's ink relative to its pen position.
    ink_right: f32,
    ink: bool,
    space: bool,
    /// First displayed glyph of its source character (a caret stop).
    first: bool,
}

/// Lay out a text layer. Cheap enough to call on every keystroke.
pub fn layout(t: &TextLayer) -> TextLayout {
    let resolved = resolve(t);
    let font = resolved.font.clone();
    let size = if t.size.is_finite() { t.size.max(1.0) } else { 12.0 };
    let (ascent, descent) = font
        .horizontal_line_metrics(size)
        .map_or((size * 0.8, size * 0.2), |m| (m.ascent, -m.descent));
    let line_step = t.line_height.max(0.5) * size;
    let track = if t.tracking.is_finite() {
        t.tracking / 1000.0 * size
    } else {
        0.0
    };
    // Synthetic bold smears each glyph rightwards by `bold_px` and widens
    // every advance by the same amount so neighbours do not collide.
    let bold_px = if resolved.synthetic_bold {
        (size * EMBOLDEN).max(1.0)
    } else {
        0.0
    };
    let shift = if t.baseline_shift.is_finite() {
        t.baseline_shift
    } else {
        0.0
    };
    let wrap = t.box_size.map(|[w, _]| w.max(1.0));
    let space_advance = font.metrics(' ', size).advance_width.ceil();

    let mut out = TextLayout {
        glyphs: Vec::new(),
        lines: Vec::new(),
        ascent,
        descent,
        line_step,
        size,
        font: font.clone(),
        bold_px,
        oblique: resolved.synthetic_oblique,
    };

    let mut para_start = 0usize;
    for para in t.text.split('\n') {
        let para_end = para_start + para.len();
        // Shape: one item per displayed character.
        let mut items: Vec<Item> = Vec::with_capacity(para.len());
        for (b, c) in para.char_indices() {
            let mut first = true;
            let mut push = |d: char, first: bool| {
                let (glyph, advance, ink_right, ink) = if d == '\t' {
                    (font.lookup_glyph_index(' '), space_advance * 4.0, 0.0, false)
                } else if d.is_control() {
                    (0, 0.0, 0.0, false)
                } else {
                    let g = font.lookup_glyph_index(d);
                    let m = font.metrics_indexed(g, size);
                    let ink = m.width > 0 && m.height > 0;
                    (
                        g,
                        m.advance_width.ceil(),
                        m.xmin as f32 + m.width as f32 + bold_px,
                        ink,
                    )
                };
                items.push(Item {
                    byte: para_start + b,
                    glyph,
                    advance: advance + track + bold_px,
                    ink_right,
                    ink,
                    space: d.is_whitespace(),
                    first,
                });
            };
            if t.all_caps {
                for d in c.to_uppercase() {
                    push(d, first);
                    first = false;
                }
            } else {
                push(c, true);
            }
        }

        // Break into lines: item ranges [a, b) plus "ends at a wrap".
        let mut ranges: Vec<(usize, usize, bool)> = Vec::new();
        if let Some(w) = wrap {
            // Never split one source character's glyphs across lines.
            let char_start = |mut i: usize, floor: usize| {
                while i > floor && !items[i].first {
                    i -= 1;
                }
                i
            };
            let (mut start, mut pen, mut brk) = (0usize, 0.0f32, None::<usize>);
            for i in 0..items.len() {
                if items[i].space {
                    pen += items[i].advance;
                    brk = Some(i + 1);
                    continue;
                }
                let fits = |pen: f32| pen + items[i].advance - track <= w + 0.01;
                if !fits(pen) && i > start {
                    let at = match brk {
                        Some(b) if b > start && b <= i => b,
                        _ => char_start(i, start),
                    };
                    if at > start {
                        ranges.push((start, at, true));
                        start = at;
                        pen = items[at..i].iter().map(|it| it.advance).sum();
                        brk = None;
                        // The carried-over word may still be too wide.
                        let at = char_start(i, start);
                        if !fits(pen) && at > start {
                            ranges.push((start, at, true));
                            start = at;
                            pen = items[at..i].iter().map(|it| it.advance).sum();
                        }
                    }
                }
                pen += items[i].advance;
            }
            ranges.push((start, items.len(), false));
        } else {
            ranges.push((0, items.len(), false));
        }

        for (a, b, wrapped) in ranges {
            let k = out.lines.len();
            let line_items = &items[a..b];
            // Ink width from the pen start: what alignment lines up.
            let mut pen = 0.0f32;
            let mut natural = 0.0f32;
            let mut last_ink = None;
            for (j, it) in line_items.iter().enumerate() {
                if it.ink {
                    natural = natural.max(pen + it.ink_right);
                    last_ink = Some(j);
                }
                pen += it.advance;
            }
            let justify = matches!(t.align, TextAlign::Justify) && wrapped && wrap.is_some();
            let mut extra = 0.0;
            if let (true, Some(w), Some(last)) = (justify, wrap, last_ink) {
                let gaps = line_items[..last].iter().filter(|it| it.space).count();
                if gaps > 0 && w > natural {
                    extra = (w - natural) / gaps as f32;
                }
            }
            let x0 = match (wrap, t.align) {
                (None, TextAlign::Left | TextAlign::Justify) => t.x,
                (None, TextAlign::Center) => t.x - natural / 2.0,
                (None, TextAlign::Right) => t.x - natural,
                (Some(_), TextAlign::Left | TextAlign::Justify) => t.x,
                (Some(w), TextAlign::Center) => t.x + (w - natural) / 2.0,
                (Some(w), TextAlign::Right) => t.x + w - natural,
            };
            let nominal = match wrap {
                None => t.y + k as f32 * line_step,
                Some(_) => t.y + ascent + k as f32 * line_step,
            };
            let hidden = match t.box_size {
                Some([_, h]) => nominal + descent > t.y + h.max(0.0) + 0.5,
                None => false,
            };
            let start_byte = line_items.first().map_or(para_start, |it| it.byte);
            let end_byte = items.get(b).map_or(para_end, |it| it.byte);
            let mut stops = Vec::with_capacity(line_items.len() + 1);
            let mut x = x0;
            for (j, it) in line_items.iter().enumerate() {
                if it.first {
                    stops.push((it.byte, x));
                }
                let widen = if it.space && last_ink.is_some_and(|l| j < l) {
                    extra
                } else {
                    0.0
                };
                out.glyphs.push(PlacedGlyph {
                    byte: it.byte,
                    glyph: it.glyph,
                    x,
                    advance: it.advance + widen,
                    line: k,
                });
                x += it.advance + widen;
            }
            stops.push((end_byte, x));
            out.lines.push(LayoutLine {
                start: start_byte,
                end: end_byte,
                baseline: nominal - shift,
                stops,
                wrapped,
                hidden,
            });
        }
        para_start = para_end + 1;
    }
    out
}

impl TextLayout {
    /// The line that shows the caret at `index`. At a word wrap the caret
    /// belongs to the start of the next line.
    pub fn line_of(&self, index: usize) -> usize {
        let mut found = None;
        for (k, l) in self.lines.iter().enumerate() {
            if l.start <= index && index <= l.end {
                found = Some(k);
                if !(index == l.end && l.wrapped) {
                    break;
                }
            } else if found.is_some() {
                break;
            }
        }
        found.unwrap_or_else(|| if index == 0 { 0 } else { self.lines.len() - 1 })
    }

    /// Where the caret for byte position `index` is drawn.
    pub fn caret(&self, index: usize) -> Caret {
        let k = self.line_of(index);
        let l = &self.lines[k];
        let x = l
            .stops
            .iter()
            .rev()
            .find(|(b, _)| *b <= index)
            .or(l.stops.first())
            .map_or(0.0, |s| s.1);
        Caret {
            x,
            top: l.baseline - self.ascent,
            bottom: l.baseline + self.descent,
            line: k,
        }
    }

    /// The position on line `k` nearest to x. A wrapped line's last stop
    /// is the next line's start, so it is left out here.
    pub fn hit_in_line(&self, k: usize, x: f32) -> usize {
        let l = &self.lines[k.min(self.lines.len() - 1)];
        let stops = if l.wrapped && l.stops.len() > 1 {
            &l.stops[..l.stops.len() - 1]
        } else {
            &l.stops[..]
        };
        stops
            .iter()
            .min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs()))
            .map_or(l.start, |s| s.0)
    }

    /// The visible line whose band (one leading tall, centred on its
    /// glyphs) holds canvas y; clamped to the first and last lines.
    pub fn line_at_y(&self, y: f32) -> usize {
        let visible = self.lines.iter().filter(|l| !l.hidden).count().max(1);
        let mid = |l: &LayoutLine| l.baseline - (self.ascent - self.descent) / 2.0;
        let first = mid(&self.lines[0]);
        let k = ((y - first) / self.line_step.max(1e-3)).round();
        (k.max(0.0) as usize).min(visible - 1)
    }

    /// The character position a click at canvas (x, y) lands on.
    pub fn hit(&self, x: f32, y: f32) -> usize {
        self.hit_in_line(self.line_at_y(y), x)
    }

    /// The first and last caret positions of line `k` (the end of a
    /// wrapped line stops before its last trailing character).
    pub fn line_bounds(&self, k: usize) -> (usize, usize) {
        let l = &self.lines[k.min(self.lines.len() - 1)];
        let end = if l.wrapped && l.stops.len() > 1 {
            l.stops[l.stops.len() - 2].0
        } else {
            l.end
        };
        (l.start, end)
    }

    /// Highlight rectangles `[x0, y0, x1, y1]` for the selection between
    /// two positions (in either order), one per line it touches. A
    /// selected newline shows as a short stub past the line's end.
    pub fn selection_rects(&self, a: usize, b: usize) -> Vec<[f32; 4]> {
        let (lo, hi) = (a.min(b), a.max(b));
        let mut rects = Vec::new();
        if lo == hi {
            return rects;
        }
        let stub = self.size * 0.3;
        for l in &self.lines {
            if l.hidden || hi < l.start || lo > l.end || (hi == l.start && l.start != l.end) {
                continue;
            }
            let x_at = |i: usize| {
                l.stops
                    .iter()
                    .rev()
                    .find(|(b, _)| *b <= i)
                    .or(l.stops.first())
                    .map_or(0.0, |s| s.1)
            };
            let x0 = x_at(lo.max(l.start));
            let mut x1 = x_at(hi.min(l.end));
            // The selection runs on past this line's newline.
            if hi > l.end && !l.wrapped {
                x1 += stub;
            }
            if x1 > x0 {
                rects.push([x0, l.baseline - self.ascent, x1, l.baseline + self.descent]);
            }
        }
        rects
    }

    /// The area the text occupies `[x0, y0, x1, y1]`: every visible line
    /// from its first to its last caret stop, ascent to descent.
    pub fn bounds(&self) -> [f32; 4] {
        let mut r = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for l in self.lines.iter().filter(|l| !l.hidden) {
            let (x0, x1) = (l.stops[0].1, l.stops[l.stops.len() - 1].1);
            r[0] = r[0].min(x0);
            r[1] = r[1].min(l.baseline - self.ascent);
            r[2] = r[2].max(x1);
            r[3] = r[3].max(l.baseline + self.descent);
        }
        if r[0] > r[2] {
            let l = &self.lines[0];
            let x = l.stops[0].1;
            return [x, l.baseline - self.ascent, x, l.baseline + self.descent];
        }
        r
    }

    /// Some box text lines did not fit and are hidden.
    pub fn overflows(&self) -> bool {
        self.lines.iter().any(|l| l.hidden)
    }
}

// ---- point and paragraph conversion --------------------------------------------------------

/// Convert point text to paragraph text that looks the same: a box just
/// around its lines, wide enough that nothing re-wraps. Paragraph text is
/// returned unchanged.
pub fn to_paragraph(t: &TextLayer) -> TextLayer {
    if t.box_size.is_some() {
        return t.clone();
    }
    let lay = layout(t);
    let [x0, _, x1, _] = lay.bounds();
    let w = ((x1 - x0).ceil() + 1.0).max(8.0);
    let x = match t.align {
        TextAlign::Left | TextAlign::Justify => t.x,
        TextAlign::Center => t.x - w / 2.0,
        TextAlign::Right => t.x - w,
    };
    let n = lay.lines.len() as f32;
    let h = ((n - 1.0) * lay.line_step + lay.ascent + lay.descent).ceil() + 1.0;
    TextLayer {
        x,
        y: t.y - lay.ascent,
        box_size: Some([w, h.max(8.0)]),
        align: if t.align == TextAlign::Justify {
            TextAlign::Left
        } else {
            t.align
        },
        ..t.clone()
    }
}

/// Convert paragraph text to point text that looks the same: every word
/// wrap becomes a return and the anchor moves to the first baseline.
/// Point text is returned unchanged.
pub fn to_point(t: &TextLayer) -> TextLayer {
    let Some([w, _]) = t.box_size else {
        return t.clone();
    };
    let lay = layout(t);
    let mut text = String::with_capacity(t.text.len() + lay.lines.len());
    for (k, l) in lay.lines.iter().enumerate() {
        let slice = &t.text[l.start..l.end];
        text.push_str(if l.wrapped { slice.trim_end() } else { slice });
        if k + 1 < lay.lines.len() {
            text.push('\n');
        }
    }
    let align = if t.align == TextAlign::Justify {
        TextAlign::Left
    } else {
        t.align
    };
    let x = match align {
        TextAlign::Center => t.x + w / 2.0,
        TextAlign::Right => t.x + w,
        _ => t.x,
    };
    TextLayer {
        text,
        x,
        y: t.y + lay.ascent,
        align,
        box_size: None,
        ..t.clone()
    }
}

// ---- character and word boundaries ---------------------------------------------------------

/// The character boundary before `i` (0 stays 0).
pub fn prev_char(text: &str, i: usize) -> usize {
    text[..i.min(text.len())]
        .char_indices()
        .next_back()
        .map_or(0, |(b, _)| b)
}

/// The character boundary after `i` (the end stays the end).
pub fn next_char(text: &str, i: usize) -> usize {
    let i = i.min(text.len());
    text[i..].chars().next().map_or(i, |c| i + c.len_utf8())
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '\''
}

/// Option/Alt+Left: the start of the word before `i`, skipping spaces and
/// punctuation first.
pub fn prev_word(text: &str, i: usize) -> usize {
    let mut i = i.min(text.len());
    while i > 0 && !text[..i].chars().next_back().is_some_and(is_word) {
        i = prev_char(text, i);
    }
    while i > 0 && text[..i].chars().next_back().is_some_and(is_word) {
        i = prev_char(text, i);
    }
    i
}

/// Option/Alt+Right: the end of the word after `i`, skipping spaces and
/// punctuation first.
pub fn next_word(text: &str, i: usize) -> usize {
    let mut i = i.min(text.len());
    while i < text.len() && !text[i..].chars().next().is_some_and(is_word) {
        i = next_char(text, i);
    }
    while i < text.len() && text[i..].chars().next().is_some_and(is_word) {
        i = next_char(text, i);
    }
    i
}

/// Double-click: the word (or run of spaces, or single other character)
/// under position `i`, as a byte range.
pub fn word_at(text: &str, i: usize) -> (usize, usize) {
    let i = i.min(text.len());
    // At the end of a word, pick the word to the left.
    let probe = if i == text.len() || (i > 0 && !text[i..].starts_with(is_word)) {
        let p = prev_char(text, i);
        if text[p..].starts_with(is_word) || i == text.len() {
            p
        } else {
            i
        }
    } else {
        i
    };
    let Some(c) = text[probe..].chars().next() else {
        return (i, i);
    };
    let same = |d: char| {
        if is_word(c) {
            is_word(d)
        } else if c.is_whitespace() && c != '\n' {
            d.is_whitespace() && d != '\n'
        } else {
            false
        }
    };
    let mut a = probe;
    while a > 0 && text[..a].chars().next_back().is_some_and(same) {
        a = prev_char(text, a);
    }
    let mut b = next_char(text, probe);
    while b < text.len() && text[b..].chars().next().is_some_and(same) {
        b = next_char(text, b);
    }
    (a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

    /// DejaVu Sans at 20 px: every advance in whole pixels.
    fn adv(c: char, size: f32) -> f32 {
        let t = TextLayer::new("", 0.0, 0.0, size, BLACK);
        resolve(&t).font.metrics(c, size).advance_width.ceil()
    }

    #[test]
    fn point_text_places_carets_at_the_glyph_advances() {
        let t = TextLayer::new("Hi\nyo", 100.0, 50.0, 20.0, BLACK);
        let l = layout(&t);
        assert_eq!(l.lines.len(), 2);
        let (h, i) = (adv('H', 20.0), adv('i', 20.0));
        assert_eq!(
            l.lines[0].stops,
            vec![(0, 100.0), (1, 100.0 + h), (2, 100.0 + h + i)]
        );
        assert_eq!((l.lines[0].start, l.lines[0].end), (0, 2));
        assert_eq!((l.lines[1].start, l.lines[1].end), (3, 5));
        assert_eq!(l.lines[0].baseline, 50.0);
        assert_eq!(l.lines[1].baseline, 50.0 + 1.2 * 20.0);
        // The caret after "H" sits at its advance; on line 2 after "y".
        let c = l.caret(1);
        assert_eq!((c.x, c.line), (100.0 + h, 0));
        assert!((c.top - (50.0 - l.ascent)).abs() < 1e-4);
        assert!((c.bottom - (50.0 + l.descent)).abs() < 1e-4);
        assert_eq!(l.caret(4).x, 100.0 + adv('y', 20.0));
        assert_eq!(l.caret(4).line, 1);
        // The newline position (2) ends line 0; 3 starts line 1.
        assert_eq!(l.caret(2).line, 0);
        assert_eq!(
            l.caret(3),
            Caret {
                x: 100.0,
                line: 1,
                ..l.caret(3)
            }
        );
    }

    #[test]
    fn hits_snap_to_the_nearest_character_boundary() {
        let t = TextLayer::new("Hi\nyo", 100.0, 50.0, 20.0, BLACK);
        let l = layout(&t);
        let h = adv('H', 20.0);
        // Left of the first glyph's middle → 0, right of it → 1.
        assert_eq!(l.hit(100.0 + h * 0.4, 45.0), 0);
        assert_eq!(l.hit(100.0 + h * 0.6, 45.0), 1);
        // Far right of line 0 → its end; far left on line 1 → its start.
        assert_eq!(l.hit(500.0, 45.0), 2);
        assert_eq!(l.hit(0.0, 50.0 + 24.0), 3);
        // Above the first line or below the last clamp to them.
        assert_eq!(l.hit(0.0, -100.0), 0);
        assert_eq!(l.hit(500.0, 900.0), 5);
    }

    #[test]
    fn alignment_moves_the_line_origin() {
        let base = TextLayer::new("ab", 200.0, 50.0, 20.0, BLACK);
        let left = layout(&base);
        let right = layout(&TextLayer {
            align: TextAlign::Right,
            ..base.clone()
        });
        let centre = layout(&TextLayer {
            align: TextAlign::Center,
            ..base.clone()
        });
        let shift_r = 200.0 - right.lines[0].stops[0].1;
        let shift_c = 200.0 - centre.lines[0].stops[0].1;
        assert!(shift_r > 0.0 && (shift_c - shift_r / 2.0).abs() < 1e-4);
        // Right alignment ends the ink at the anchor: the line's ink width
        // is the shift, within a pixel of the advances.
        let w = adv('a', 20.0) + adv('b', 20.0);
        assert!((shift_r - w).abs() <= 2.0, "{shift_r} vs {w}");
        assert_eq!(left.lines[0].stops[0].1, 200.0);
    }

    #[test]
    fn box_text_wraps_at_spaces_and_breaks_long_words() {
        let size = 20.0;
        let (a, sp) = (adv('a', size), adv(' ', size));
        // "aaa aaa aaa" in a box 7.5 'a' wide: "aaa aaa " fits 6a+sp,
        // the third word wraps.
        let w = 6.0 * a + sp + 0.5 * a;
        let t = TextLayer {
            box_size: Some([w, 200.0]),
            ..TextLayer::new("aaa aaa aaa", 10.0, 20.0, size, BLACK)
        };
        let l = layout(&t);
        assert_eq!(l.lines.len(), 2);
        assert_eq!(
            (l.lines[0].start, l.lines[0].end, l.lines[0].wrapped),
            (0, 8, true)
        );
        assert_eq!(
            (l.lines[1].start, l.lines[1].end, l.lines[1].wrapped),
            (8, 11, false)
        );
        // Box text: the first baseline is one ascent below the top.
        assert!((l.lines[0].baseline - (20.0 + l.ascent)).abs() < 1e-4);
        assert!((l.lines[1].baseline - (20.0 + l.ascent + 24.0)).abs() < 1e-4);
        // The wrap position shows at the start of line 1.
        assert_eq!(l.caret(8).line, 1);
        assert_eq!(l.caret(8).x, 10.0);
        // Clicking past the end of line 0 stays before its trailing space.
        assert_eq!(l.hit(500.0, l.lines[0].baseline), 7);
        assert_eq!(l.line_bounds(0), (0, 7));

        // One word longer than the box breaks between characters.
        let t = TextLayer {
            box_size: Some([2.5 * a, 200.0]),
            ..TextLayer::new("aaaaa", 0.0, 0.0, size, BLACK)
        };
        let l = layout(&t);
        let ranges: Vec<_> = l.lines.iter().map(|l| (l.start, l.end)).collect();
        assert_eq!(ranges, vec![(0, 2), (2, 4), (4, 5)]);
    }

    #[test]
    fn justify_stretches_wrapped_lines_to_the_box() {
        let size = 20.0;
        let a = adv('a', size);
        let w = 9.0 * a;
        let t = TextLayer {
            box_size: Some([w, 200.0]),
            align: TextAlign::Justify,
            ..TextLayer::new("aa aa aa aaaa", 0.0, 0.0, size, BLACK)
        };
        let l = layout(&t);
        assert_eq!(l.lines.len(), 2, "{:?}", l.lines);
        // Line 0 "aa aa aa " is wrapped: its last ink ends at the box edge.
        let last_a = l
            .glyphs
            .iter()
            .filter(|g| g.line == 0 && g.byte == 7)
            .map(|g| g.x)
            .next()
            .unwrap();
        let ink_right = {
            let m = resolve(&t).font.metrics('a', size);
            m.xmin as f32 + m.width as f32
        };
        assert!(
            (last_a + ink_right - w).abs() < 1e-3,
            "justified ink ends at {} for a box {w} wide",
            last_a + ink_right
        );
        // The last line of the paragraph stays left-aligned.
        assert_eq!(l.lines[1].stops[0].1, 0.0);
        let plain = layout(&TextLayer {
            align: TextAlign::Left,
            ..t.clone()
        });
        assert_eq!(plain.lines[1].stops, l.lines[1].stops);
    }

    #[test]
    fn centre_and_right_align_inside_the_box() {
        let size = 20.0;
        let t = TextLayer {
            box_size: Some([300.0, 100.0]),
            align: TextAlign::Center,
            ..TextLayer::new("ab", 50.0, 10.0, size, BLACK)
        };
        let c = layout(&t);
        let r = layout(&TextLayer {
            align: TextAlign::Right,
            ..t.clone()
        });
        let x_c = c.lines[0].stops[0].1;
        let x_r = r.lines[0].stops[0].1;
        // Ink width n: centre starts at 50 + (300 − n) / 2, right at 350 − n.
        let n = 350.0 - x_r;
        assert!((x_c - (50.0 + (300.0 - n) / 2.0)).abs() < 1e-3);
        assert!(n > 0.0 && n < 40.0);
    }

    #[test]
    fn box_lines_below_the_bottom_are_hidden() {
        // 20 px text, leading 24: in a 50 px box line 0 (bottom ≈ ascent +
        // descent ≈ 23) and line 1 (≈ 47) fit, line 2 (≈ 71) does not.
        let t = TextLayer {
            box_size: Some([400.0, 50.0]),
            ..TextLayer::new("a\nb\nc", 0.0, 0.0, 20.0, BLACK)
        };
        let l = layout(&t);
        let hidden: Vec<bool> = l.lines.iter().map(|l| l.hidden).collect();
        assert_eq!(hidden, vec![false, false, true]);
        assert!(l.overflows());
        // Clicks below the box land on the last visible line.
        assert_eq!(l.line_at_y(500.0), 1);
    }

    #[test]
    fn leading_shift_and_caps_move_the_carets() {
        let t = TextLayer {
            line_height: 2.0,
            baseline_shift: 5.0,
            ..TextLayer::new("a\nb", 0.0, 100.0, 20.0, BLACK)
        };
        let l = layout(&t);
        assert_eq!(l.lines[0].baseline, 95.0);
        assert_eq!(l.lines[1].baseline, 135.0);
        assert_eq!(l.line_step, 40.0);
        // All caps: "ß" shows as "SS" — two glyphs, one caret step.
        let caps = layout(&TextLayer {
            all_caps: true,
            ..TextLayer::new("aß", 0.0, 50.0, 20.0, BLACK)
        });
        let s = adv('S', 20.0);
        assert_eq!(caps.glyphs.len(), 3);
        assert_eq!(
            caps.lines[0].stops,
            vec![(0, 0.0), (1, adv('A', 20.0)), (3, adv('A', 20.0) + 2.0 * s)]
        );
    }

    #[test]
    fn selection_rects_cover_each_line_touched() {
        let t = TextLayer::new("ab\ncd", 0.0, 50.0, 20.0, BLACK);
        let l = layout(&t);
        let (a, b) = (adv('a', 20.0), adv('b', 20.0));
        assert!(l.selection_rects(2, 2).is_empty());
        // "b" alone.
        let r = l.selection_rects(1, 2);
        assert_eq!(r.len(), 1);
        assert_eq!((r[0][0], r[0][2]), (a, a + b));
        // From "b" through "c": line 0 to its end plus the newline stub,
        // line 1 from its start to after "c".
        let r = l.selection_rects(4, 1);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0][0], r[0][2]), (a, a + b + 6.0));
        assert_eq!((r[1][0], r[1][2]), (0.0, adv('c', 20.0)));
        assert!((r[1][1] - (50.0 + 24.0 - l.ascent)).abs() < 1e-4);
    }

    #[test]
    fn empty_text_keeps_one_line_for_the_caret() {
        let l = layout(&TextLayer::new("", 30.0, 40.0, 20.0, BLACK));
        assert_eq!(l.lines.len(), 1);
        assert_eq!(l.lines[0].stops, vec![(0, 30.0)]);
        assert_eq!(l.caret(0).x, 30.0);
        assert_eq!(l.hit(99.0, 99.0), 0);
        let b = l.bounds();
        assert_eq!((b[0], b[2]), (30.0, 30.0));
        // A trailing newline adds an empty second line.
        let l = layout(&TextLayer::new("a\n", 0.0, 40.0, 20.0, BLACK));
        assert_eq!(l.lines.len(), 2);
        assert_eq!((l.lines[1].start, l.lines[1].end), (2, 2));
        assert_eq!(l.caret(2).line, 1);
    }

    /// Same ink, pixel for pixel.
    fn same_pixels(a: &TextLayer, b: &TextLayer) {
        let (sa, sb) = (crate::text::rasterize(a), crate::text::rasterize(b));
        let (ba, bb) = (sa.content_bounds().unwrap(), sb.content_bounds().unwrap());
        assert_eq!(ba, bb);
        for y in ba.y..ba.bottom() {
            for x in ba.x..ba.right() {
                assert_eq!(sa.get_pixel(x, y), sb.get_pixel(x, y), "({x}, {y})");
            }
        }
    }

    #[test]
    fn point_and_paragraph_convert_without_moving_glyphs() {
        let point = TextLayer::new("Two lines\nof text", 40.0, 100.0, 20.0, BLACK);
        let para = to_paragraph(&point);
        let lay = layout(&point);
        let [w, h] = para.box_size.unwrap();
        // The box hugs the lines: top one ascent above the first baseline,
        // as wide as the longest line plus a pixel of slack.
        assert_eq!((para.x, para.y), (40.0, 100.0 - lay.ascent));
        let widest = lay.bounds()[2] - lay.bounds()[0];
        assert_eq!(w, widest.ceil() + 1.0);
        assert_eq!(h, (24.0 + lay.ascent + lay.descent).ceil() + 1.0);
        assert_eq!(layout(&para).lines.len(), 2, "nothing re-wraps");
        same_pixels(&point, &para);
        assert_eq!(to_paragraph(&para), para, "already paragraph text");

        // Back to point text: soft wraps become returns.
        let boxed = TextLayer {
            box_size: Some([60.0, 200.0]),
            ..TextLayer::new("aaa bbb ccc", 10.0, 20.0, 20.0, BLACK)
        };
        assert!(layout(&boxed).lines.len() >= 2);
        let pt = to_point(&boxed);
        assert_eq!(pt.box_size, None);
        assert_eq!(pt.text.replace('\n', " "), "aaa bbb ccc");
        assert_eq!(pt.text.lines().count(), layout(&boxed).lines.len());
        assert_eq!((pt.x, pt.y), (10.0, 20.0 + layout(&boxed).ascent));
        same_pixels(&boxed, &pt);
        // Centred box text anchors on the box's centre line.
        let centred = TextLayer {
            align: TextAlign::Center,
            ..boxed.clone()
        };
        assert_eq!(to_point(&centred).x, 40.0);
        assert_eq!(
            to_point(&TextLayer {
                align: TextAlign::Justify,
                ..boxed
            })
            .align,
            TextAlign::Left
        );
    }

    #[test]
    fn word_and_character_boundaries() {
        let s = "Hello, wörld  foo_bar";
        assert_eq!(prev_char(s, 0), 0);
        assert_eq!(next_char(s, 8), 10, "ö is two bytes");
        assert_eq!(prev_char(s, 10), 8);
        assert_eq!(next_char(s, s.len()), s.len());
        // Alt+Right: to the end of the next word, skipping ", ".
        assert_eq!(next_word(s, 0), 5);
        assert_eq!(next_word(s, 5), 13);
        assert_eq!(next_word(s, 13), s.len(), "underscores join words");
        // Alt+Left: to the start of the previous word.
        assert_eq!(prev_word(s, s.len()), 15);
        assert_eq!(prev_word(s, 15), 7);
        assert_eq!(prev_word(s, 7), 0);
        // Double-click ranges: a word, a run of spaces, one punctuation.
        assert_eq!(word_at(s, 8), (7, 13));
        assert_eq!(word_at(s, 13), (7, 13), "the word ending at the click");
        assert_eq!(word_at(s, 14), (13, 15));
        assert_eq!(word_at(s, 5), (0, 5));
        assert_eq!(word_at(s, 6), (6, 7), "the space after the comma");
        assert_eq!(word_at(s, s.len()), (15, s.len()));
        assert_eq!(word_at("", 0), (0, 0));
        assert_eq!(word_at("a\nb", 1), (0, 1));
    }
}
