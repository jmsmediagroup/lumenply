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
    ///
    /// `layout:tool=<name>` picks a tool by name ("magic-wand", "brush"),
    /// `layout:layer=<name>` activates a layer, `layout:mask` targets its
    /// mask, `layout:rename` opens the rename field, `layout:name=<text>`
    /// renames the active layer, `layout:empty` deletes every layer,
    /// `layout:about` / `recover` / `confirm-close` / `confirm-tab` open
    /// those dialogs, `layout:history` folds or opens the history strip,
    /// `layout:focus-tool=<name>` gives a rail button keyboard focus,
    /// `layout:live-blur` adds a live blur layer above the active one.
    fn debug_layout(&mut self, ctx: &egui::Context, tok: &str) -> bool {
        let Some(tok) = tok.strip_prefix("layout:") else {
            return false;
        };
        let slug = |s: &str| s.to_ascii_lowercase().replace(' ', "-");
        let (key, arg) = tok.split_once('=').unwrap_or((tok, ""));
        match key {
            "tool" => {
                if let Some(t) = Tool::ALL.into_iter().find(|t| slug(t.name()) == arg) {
                    self.tool = t;
                }
            }
            "focus-tool" => {
                if let Some(t) = Tool::ALL.into_iter().find(|t| slug(t.name()) == arg) {
                    ctx.memory_mut(|m| m.request_focus(tools::rail_id(t)));
                }
            }
            "layer" => {
                let id = self
                    .layer_rows()
                    .into_iter()
                    .find(|r| slug(r.name()) == arg)
                    .map(|r| r.id());
                self.set_active(id);
            }
            "mask" => self.editing_mask = self.active_has_mask(),
            "rename" => {
                if let Some(l) = self.active_layer() {
                    self.renaming = Some((l.id, l.name.clone()));
                }
            }
            "name" => {
                if let Some(layer) = self.active {
                    self.run(&RenameLayer {
                        layer,
                        name: arg.replace('_', " "),
                    });
                }
            }
            "empty" => {
                while let Some(layer) = self.editor.doc().layers().last().map(|l| l.id) {
                    self.run(&RemoveLayer { layer });
                }
                self.fix_active();
            }
            "about" => self.dialog = Some(Dialog::About),
            "recover" => self.dialog = Some(Dialog::Recover),
            "confirm-close" => self.dialog = Some(Dialog::ConfirmClose),
            "confirm-tab" => self.dialog = Some(Dialog::ConfirmCloseTab(0)),
            // Flip without saving: a screenshot run must not rewrite the
            // user's preferences.
            "history" => self.prefs.history_collapsed = !self.prefs.history_collapsed,
            "live-blur" => self.add_filter_layer(Filter::GaussianBlur { radius: 8.0 }),
            _ => return false,
        }
        true
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
