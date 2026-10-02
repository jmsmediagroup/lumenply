//! Character styling inside a text layer: runs that override the layer's
//! colour, size, bold, italic, underline or strikethrough over a byte
//! range of its text (Photoshop's
//! per-character formatting). Runs never overlap, stay sorted, never
//! repeat the layer's own value for a field, and follow every text edit
//! made through [`TextLayer::replace_text`] or
//! [`TextLayer::set_text_keep_runs`].

use serde::{Deserialize, Serialize};

use crate::TextLayer;

/// Style overrides; `None` keeps the layer's own value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CharStyle {
    /// Straight linear RGBA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[f32; 4]>,
    /// Font size in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underline: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strikethrough: Option<bool>,
}

impl CharStyle {
    pub fn is_empty(&self) -> bool {
        *self == CharStyle::default()
    }

    /// `self` with every field `over` sets replaced.
    pub fn merged(self, over: CharStyle) -> CharStyle {
        CharStyle {
            color: over.color.or(self.color),
            size: over.size.or(self.size),
            bold: over.bold.or(self.bold),
            italic: over.italic.or(self.italic),
            underline: over.underline.or(self.underline),
            strikethrough: over.strikethrough.or(self.strikethrough),
        }
    }
}

/// A styled byte range `start..end` of the layer's text.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextRun {
    pub start: usize,
    pub end: usize,
    #[serde(flatten)]
    pub style: CharStyle,
}

/// The style a character is drawn with: the layer's, with its run's
/// overrides applied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedStyle {
    pub color: [f32; 4],
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

impl TextLayer {
    /// The overrides in force at byte `i` (none outside every run).
    pub fn overrides_at(&self, i: usize) -> CharStyle {
        self.runs
            .iter()
            .find(|r| r.start <= i && i < r.end)
            .map_or_else(CharStyle::default, |r| r.style)
    }

    /// The style the character at byte `i` is drawn with.
    pub fn style_at(&self, i: usize) -> ResolvedStyle {
        let o = self.overrides_at(i);
        ResolvedStyle {
            color: o.color.unwrap_or(self.color),
            size: o.size.unwrap_or(self.size),
            bold: o.bold.unwrap_or(self.bold),
            italic: o.italic.unwrap_or(self.italic),
            underline: o.underline.unwrap_or(self.underline),
            strikethrough: o.strikethrough.unwrap_or(self.strikethrough),
        }
    }

    /// The largest size anywhere in the text (the layer's own when no run
    /// sets one).
    pub fn max_size(&self) -> f32 {
        self.runs
            .iter()
            .filter_map(|r| r.style.size)
            .fold(self.size, f32::max)
    }

    /// Style the byte range `a..b` (either order) with `patch`.
    pub fn apply_style(&mut self, a: usize, b: usize, patch: CharStyle) {
        let (a, b) = (a.min(b), a.max(b).min(self.text.len()));
        if a >= b || patch.is_empty() {
            return;
        }
        let mut cuts: Vec<usize> = vec![a, b];
        for r in &self.runs {
            cuts.extend([r.start, r.end]);
        }
        cuts.sort_unstable();
        cuts.dedup();
        let mut runs = Vec::new();
        for w in cuts.windows(2) {
            let (x, y) = (w[0], w[1]);
            let mut style = self.overrides_at(x);
            if x >= a && y <= b {
                style = style.merged(patch);
            }
            runs.push(TextRun {
                start: x,
                end: y,
                style,
            });
        }
        self.runs = runs;
        self.normalize_runs();
    }

    /// Set the layer's own colour, size, bold or italic from `patch` and
    /// drop those fields from every run, so the change reaches all text.
    pub fn set_base_style(&mut self, patch: CharStyle) {
        if let Some(c) = patch.color {
            self.color = c;
        }
        if let Some(s) = patch.size {
            self.size = s;
        }
        if let Some(v) = patch.bold {
            self.bold = v;
        }
        if let Some(v) = patch.italic {
            self.italic = v;
        }
        if let Some(v) = patch.underline {
            self.underline = v;
        }
        if let Some(v) = patch.strikethrough {
            self.strikethrough = v;
        }
        for r in &mut self.runs {
            if patch.color.is_some() {
                r.style.color = None;
            }
            if patch.size.is_some() {
                r.style.size = None;
            }
            if patch.bold.is_some() {
                r.style.bold = None;
            }
            if patch.italic.is_some() {
                r.style.italic = None;
            }
            if patch.underline.is_some() {
                r.style.underline = None;
            }
            if patch.strikethrough.is_some() {
                r.style.strikethrough = None;
            }
        }
        self.normalize_runs();
    }

    /// Replace the bytes `a..b` of the text with `s`, keeping the runs on
    /// their characters. The new text takes the style of the first
    /// replaced character, or else of the character before it (typing
    /// continues in the style at the caret).
    pub fn replace_text(&mut self, a: usize, b: usize, s: &str) {
        let (a, b) = (a.min(b), a.max(b).min(self.text.len()));
        let a = a.min(b);
        let inherit = if b > a {
            self.overrides_at(a)
        } else if a > 0 {
            self.overrides_at(a - 1)
        } else {
            self.overrides_at(0)
        };
        self.text.replace_range(a..b, s);
        let (removed, n) = (b - a, s.len());
        let mut runs = Vec::with_capacity(self.runs.len() + 2);
        for r in &self.runs {
            // Cut out the replaced bytes...
            let cut = |p: usize| {
                if p <= a {
                    p
                } else if p >= b {
                    p - removed
                } else {
                    a
                }
            };
            let (start, end) = (cut(r.start), cut(r.end));
            // ...then open a gap for the new ones.
            if end <= a {
                runs.push(TextRun { start, end, ..*r });
            } else if start >= a {
                runs.push(TextRun {
                    start: start + n,
                    end: end + n,
                    ..*r
                });
            } else {
                runs.push(TextRun { start, end: a, ..*r });
                runs.push(TextRun {
                    start: a + n,
                    end: end + n,
                    ..*r
                });
            }
        }
        if n > 0 && !inherit.is_empty() {
            runs.push(TextRun {
                start: a,
                end: a + n,
                style: inherit,
            });
        }
        self.runs = runs;
        self.normalize_runs();
    }

    /// Replace the whole text (from a plain text field, say), keeping the
    /// runs on the characters that did not change: the edit is taken to
    /// be the one span between the common prefix and suffix.
    pub fn set_text_keep_runs(&mut self, new: &str) {
        if self.text == new {
            return;
        }
        if self.runs.is_empty() {
            self.text = new.to_string();
            return;
        }
        let old = self.text.clone();
        let prefix: usize = old
            .chars()
            .zip(new.chars())
            .take_while(|(x, y)| x == y)
            .map(|(c, _)| c.len_utf8())
            .sum();
        let room = old.len().min(new.len()) - prefix;
        let suffix: usize = old[prefix..]
            .chars()
            .rev()
            .zip(new[prefix..].chars().rev())
            .take_while(|(x, y)| x == y)
            .map(|(c, _)| c.len_utf8())
            .scan(0usize, |acc, l| {
                *acc += l;
                Some(*acc)
            })
            .take_while(|&total| total <= room)
            .last()
            .unwrap_or(0);
        self.replace_text(prefix, old.len() - suffix, &new[prefix..new.len() - suffix]);
    }

    /// Keep runs tidy: inside the text, without fields that repeat the
    /// layer's own values, without empty runs, adjacent equal runs merged.
    pub fn normalize_runs(&mut self) {
        let len = self.text.len();
        let base = (self.color, self.size, self.bold, self.italic);
        let mut runs: Vec<TextRun> = Vec::with_capacity(self.runs.len());
        let mut sorted = std::mem::take(&mut self.runs);
        sorted.sort_by_key(|r| r.start);
        for mut r in sorted {
            r.end = r.end.min(len);
            if r.style.color == Some(base.0) {
                r.style.color = None;
            }
            if r.style.size == Some(base.1) {
                r.style.size = None;
            }
            if r.style.bold == Some(base.2) {
                r.style.bold = None;
            }
            if r.style.italic == Some(base.3) {
                r.style.italic = None;
            }
            if r.style.underline == Some(self.underline) {
                r.style.underline = None;
            }
            if r.style.strikethrough == Some(self.strikethrough) {
                r.style.strikethrough = None;
            }
            if r.start >= r.end || r.style.is_empty() {
                continue;
            }
            match runs.last_mut() {
                Some(last) if last.end == r.start && last.style == r.style => last.end = r.end,
                _ => runs.push(r),
            }
        }
        self.runs = runs;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

    fn red() -> CharStyle {
        CharStyle {
            color: Some(RED),
            ..CharStyle::default()
        }
    }

    fn big() -> CharStyle {
        CharStyle {
            size: Some(40.0),
            ..CharStyle::default()
        }
    }

    fn spans(t: &TextLayer) -> Vec<(usize, usize)> {
        t.runs.iter().map(|r| (r.start, r.end)).collect()
    }

    #[test]
    fn styling_a_range_splits_and_merges_runs() {
        let mut t = TextLayer::new("Hello world", 0.0, 0.0, 20.0, BLACK);
        t.apply_style(0, 5, red());
        assert_eq!(spans(&t), vec![(0, 5)]);
        assert_eq!(t.style_at(4).color, RED);
        assert_eq!(t.style_at(5).color, BLACK);
        // Overlapping a second style splits into three pieces.
        t.apply_style(3, 8, big());
        assert_eq!(spans(&t), vec![(0, 3), (3, 5), (5, 8)]);
        assert_eq!(
            t.style_at(4),
            ResolvedStyle {
                color: RED,
                size: 40.0,
                bold: false,
                italic: false,
                underline: false,
                strikethrough: false
            }
        );
        assert_eq!((t.style_at(6).color, t.style_at(6).size), (BLACK, 40.0));
        assert_eq!(t.max_size(), 40.0);
        // Styling back to the layer's own values leaves no run behind.
        t.apply_style(
            0,
            11,
            CharStyle {
                color: Some(BLACK),
                size: Some(20.0),
                ..CharStyle::default()
            },
        );
        assert!(t.runs.is_empty());
        // Neighbouring runs with the same style merge.
        t.apply_style(0, 2, red());
        t.apply_style(2, 4, red());
        assert_eq!(spans(&t), vec![(0, 4)]);
        // The layer-wide change wins over the runs.
        t.set_base_style(CharStyle {
            color: Some([0.0, 0.0, 1.0, 1.0]),
            ..CharStyle::default()
        });
        assert!(t.runs.is_empty());
        assert_eq!(t.style_at(1).color, [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn edits_keep_runs_on_their_characters() {
        let mut t = TextLayer::new("Hello world", 0.0, 0.0, 20.0, BLACK);
        t.apply_style(6, 11, red()); // "world"
                                     // Typing before the run shifts it.
        t.replace_text(0, 0, ">> ");
        assert_eq!(t.text, ">> Hello world");
        assert_eq!(spans(&t), vec![(9, 14)]);
        // Typing at the run's end continues its style.
        t.replace_text(14, 14, "!");
        assert_eq!(spans(&t), vec![(9, 15)]);
        // Typing inside it stays inside it.
        t.replace_text(11, 11, "--");
        assert_eq!(t.text, ">> Hello wo--rld!");
        assert_eq!(spans(&t), vec![(9, 17)]);
        // Deleting across its start trims it.
        t.replace_text(7, 11, "");
        assert_eq!(t.text, ">> Hell--rld!");
        assert_eq!(spans(&t), vec![(7, 13)]);
        // Replacing a selection that starts in the run takes its style.
        t.replace_text(8, 12, "XY");
        assert_eq!(t.text, ">> Hell-XY!");
        assert_eq!(spans(&t), vec![(7, 11)]);
        // Typing right after unstyled text stays unstyled.
        t.replace_text(3, 3, "a");
        assert_eq!(spans(&t), vec![(8, 12)]);
        // Deleting all the styled text drops the run.
        t.replace_text(8, 12, "");
        assert!(t.runs.is_empty());
        assert_eq!(t.text, ">> aHell");
    }

    #[test]
    fn replacing_the_whole_text_diffs_one_span() {
        let mut t = TextLayer::new("one two three", 0.0, 0.0, 20.0, BLACK);
        t.apply_style(8, 13, red()); // "three"
        t.set_text_keep_runs("one 2 three");
        assert_eq!(spans(&t), vec![(6, 11)]);
        // Text added right after the run continues its style, as typing does.
        t.set_text_keep_runs("one 2 three four");
        assert_eq!(spans(&t), vec![(6, 16)]);
        // Multi-byte characters never split.
        t.set_text_keep_runs("one 2 thrée four");
        assert_eq!(t.text, "one 2 thrée four");
        assert_eq!(spans(&t), vec![(6, 17)]);
        // Repeated characters at the seam: "aa" → "aaa" grows by one.
        let mut t = TextLayer::new("aa", 0.0, 0.0, 20.0, BLACK);
        t.apply_style(0, 2, red());
        t.set_text_keep_runs("aaa");
        assert_eq!(spans(&t), vec![(0, 3)]);
    }

    #[test]
    fn runs_round_trip_through_json_without_empty_fields() {
        let mut t = TextLayer::new("ab", 0.0, 0.0, 20.0, BLACK);
        t.apply_style(1, 2, big());
        let json = serde_json::to_string(&t.runs).unwrap();
        assert_eq!(json, r#"[{"start":1,"end":2,"size":40.0}]"#);
        let back: Vec<TextRun> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t.runs);
    }
}
