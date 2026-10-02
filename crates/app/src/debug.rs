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
    fn debug_start(&mut self, _ctx: &egui::Context, tok: &str) -> bool {
        match tok {
            // Close every tab through the real close path (unsaved changes
            // are discarded) and land on the welcome screen.
            "start:close-all" => {
                while !self.no_doc {
                    self.force_close_tab(self.cur_tab);
                }
            }
            "start:demo" => self.open_demo(),
            // A realistic recent list, in memory only (never written to
            // recent.txt): real files with staggered ages plus one missing.
            "start:fake-recent" => {
                let dir = std::env::temp_dir().join("lumenply-fake-recent");
                let _ = std::fs::create_dir_all(&dir);
                let now = std::time::SystemTime::now();
                let hour = std::time::Duration::from_secs(3600);
                let mut list = Vec::new();
                for (name, hours) in [
                    ("summit-final.lumen", 0),
                    ("poster.psd", 3),
                    ("portrait-retouch.lumen", 26),
                    ("IMG_2041.jpg", 24 * 4),
                    ("storyboard.ora", 24 * 20),
                    ("scan-0007.tiff", 24 * 90),
                ] {
                    let p = dir.join(name);
                    if let Ok(f) = std::fs::File::create(&p) {
                        let _ = f.set_modified(now - hour * hours);
                    }
                    list.push(p.to_string_lossy().into_owned());
                }
                list.insert(4, dir.join("old-project.lumen").to_string_lossy().into_owned());
                self.recent = list;
            }
            "start:no-recent" => self.recent.clear(),
            "start:error" => {
                self.status =
                    "Could not open /Users/me/Desktop/broken.psd: unsupported colour mode (CMYK)".into();
            }
            "start:recover" => self.dialog = Some(Dialog::Recover),
            // Write the demo as a crash backup, then recover it, to check
            // the recovery path end to end.
            "start:write-autosave" => {
                if let (Ok(ed), Some(file)) = (demo::build(), session::autosave_file()) {
                    if let Some(dir) = file.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    let _ = project::save(&file, ed.doc());
                }
            }
            "start:recover-now" => self.recover_autosave(),
            "start:new" => self.dialog = Some(Dialog::New(1920, 1080)),
            _ => return false,
        }
        true
    }
}
