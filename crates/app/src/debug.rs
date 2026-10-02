//! Debug-only hooks for `--screenshot-do`: tokens that put the UI into a
//! state behind a click (an open popup, the colour picker, the start
//! screen) so it can be captured headlessly. Each UI area owns one
//! function; tokens are namespaced by area (`popups:...`, `layout:...`,
//! `text:...`, `color:...`, `start:...`). Return true when handled.

use super::*;

impl App {
    pub(crate) fn debug_token(&mut self, ctx: &egui::Context, tok: &str) -> bool {
        self.debug_popups(ctx, tok)
            || self.debug_layout(ctx, tok)
            || self.debug_text(ctx, tok)
            || self.debug_color(ctx, tok)
            || self.debug_start(ctx, tok)
    }

    /// Menus, popups, context menus and combo boxes (`popups:...`).
    fn debug_popups(&mut self, _ctx: &egui::Context, _tok: &str) -> bool {
        false
    }

    /// Panels, rails, bars and dialogs layout (`layout:...`).
    fn debug_layout(&mut self, _ctx: &egui::Context, _tok: &str) -> bool {
        false
    }

    /// Text tool and fonts (`text:...`):
    /// `text:tool` selects the Text tool; `text:click:X:Y` and
    /// `text:shift-click:X:Y` run the tool's click at a document position;
    /// `text:new` presses "New text"; `text:font-open[:QUERY]` opens the
    /// options-bar font picker (typed search optional) and
    /// `text:panel-font-open[:QUERY]` the Properties one; `text:font:NAME`
    /// sets the active text layer's font; `text:bold`, `text:italic` toggle
    /// its style; `text:save:PATH` saves the document there.
    fn debug_text(&mut self, ctx: &egui::Context, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("text:") else {
            return false;
        };
        let xy = |s: &str| -> Option<(f32, f32)> {
            let (x, y) = s.split_once(':')?;
            Some((x.parse().ok()?, y.parse().ok()?))
        };
        let restyle = |app: &mut Self, f: &dyn Fn(&mut TextLayer)| {
            if let (Some(id), Some(mut t)) = (app.active, app.active_text()) {
                f(&mut t);
                app.run(&SetText { layer: id, text: t });
            }
        };
        if rest == "tool" {
            self.tool = Tool::Text;
        } else if rest == "new" {
            self.text_new_armed = true;
        } else if rest == "bold" {
            restyle(self, &|t| t.bold = !t.bold);
        } else if rest == "italic" {
            restyle(self, &|t| t.italic = !t.italic);
        } else if let Some(name) = rest.strip_prefix("font:") {
            restyle(self, &|t| t.font = name.to_string());
        } else if let Some(path) = rest.strip_prefix("save:") {
            // Straight to disk: no recent-files entry, no autosave cleanup.
            if let Err(e) = project::save(path, self.editor.doc()) {
                self.status = format!("Could not save: {e}");
            }
        } else if let Some(q) = rest.strip_prefix("font-open") {
            crate::text_ui::open_font_picker(ctx, "bar", q.trim_start_matches(':'));
        } else if let Some(q) = rest.strip_prefix("panel-font-open") {
            crate::text_ui::open_font_picker(ctx, "panel", q.trim_start_matches(':'));
        } else if let Some((x, y)) = rest.strip_prefix("click:").and_then(xy) {
            self.text_click(ctx, x, y, false);
        } else if let Some((x, y)) = rest.strip_prefix("shift-click:").and_then(xy) {
            self.text_click(ctx, x, y, true);
        } else {
            return false;
        }
        true
    }

    /// Colour picker and eyedropper (`color:...`).
    fn debug_color(&mut self, _ctx: &egui::Context, _tok: &str) -> bool {
        false
    }

    /// Start screen, empty states and file dialogs (`start:...`).
    fn debug_start(&mut self, _ctx: &egui::Context, _tok: &str) -> bool {
        false
    }
}
