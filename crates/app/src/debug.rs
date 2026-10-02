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

    /// Text tool and fonts (`text:...`).
    fn debug_text(&mut self, _ctx: &egui::Context, _tok: &str) -> bool {
        false
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
