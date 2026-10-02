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
    ///
    /// Pointer tokens replay real input (see [`App::debug_popups_input`]),
    /// so a menu opens, and an item fires, through exactly the code a
    /// user's click runs: `popups:click=T`, `popups:rclick=T`,
    /// `popups:hover=T`, `popups:wait`, `popups:esc`, `popups:key=Enter`
    /// (any egui key name), `popups:type=text`. A target `T` is
    /// `X:Y` in points or a name recorded by [`theme::note_target`]: a
    /// menu title ("File", "Export"), a menu item label ("Undo"), or an
    /// icon menu id ("add-adj", "layer-more", "export-button").
    fn debug_popups(&mut self, ctx: &egui::Context, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("popups:") else {
            return false;
        };
        let (verb, target) = rest.split_once('=').unwrap_or((rest, ""));
        let t = target.to_string();
        let steps: Vec<(&str, String)> = match verb {
            "click" => vec![("move", t.clone()), ("press-l", t.clone()), ("release-l", t)],
            "rclick" => vec![("move", t.clone()), ("press-r", t.clone()), ("release-r", t)],
            "hover" => vec![("move", t), ("wait", String::new())],
            "wait" => vec![("wait", String::new())],
            "esc" => vec![("key", "Escape".into())],
            "key" => vec![("key", t)],
            "type" => vec![("type", t)],
            _ => return false,
        };
        let id = egui::Id::new("popups:steps");
        let queued = ctx.data_mut(|d| {
            let q = d.get_temp_mut_or_default::<Vec<(String, String)>>(id);
            q.extend(steps.into_iter().map(|(v, t)| (v.to_string(), t)));
            q.len() as u32
        });
        // Leave room for every step, the menu's sizing pass and its fade-in.
        if let Some((_, frames)) = &mut self.shot {
            *frames = (*frames).max(queued + 12);
        }
        true
    }

    /// Feed one queued `popups:` pointer step into the frame's raw input.
    /// Does nothing unless `--screenshot-do` queued steps.
    pub(crate) fn debug_popups_input(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        let id = egui::Id::new("popups:steps");
        let step = ctx.data_mut(|d| {
            let q = d.get_temp_mut_or_default::<Vec<(String, String)>>(id);
            (!q.is_empty()).then(|| q.remove(0))
        });
        let Some((verb, target)) = step else {
            return;
        };
        match verb.as_str() {
            "type" => {
                raw.events.push(egui::Event::Text(target));
                return;
            }
            "key" => {
                if let Some(key) = Key::from_name(&target) {
                    for pressed in [true, false] {
                        raw.events.push(egui::Event::Key {
                            key,
                            physical_key: None,
                            pressed,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        });
                    }
                }
                return;
            }
            _ => {}
        }
        let pos = match target.split_once(':') {
            Some((x, y)) => x.parse().ok().zip(y.parse().ok()).map(|(x, y)| egui::pos2(x, y)),
            None => theme::target_rect(ctx, &target).map(|r| r.center()),
        };
        let button = |b: egui::PointerButton, pressed: bool, pos: Pos2| egui::Event::PointerButton {
            pos,
            button: b,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let event = match (verb.as_str(), pos) {
            ("move", Some(p)) => egui::Event::PointerMoved(p),
            ("press-l", Some(p)) => button(egui::PointerButton::Primary, true, p),
            ("release-l", Some(p)) => button(egui::PointerButton::Primary, false, p),
            ("press-r", Some(p)) => button(egui::PointerButton::Secondary, true, p),
            ("release-r", Some(p)) => button(egui::PointerButton::Secondary, false, p),
            ("wait", _) => return,
            (_, None) => {
                eprintln!("popups: no target named {target:?}");
                return;
            }
            _ => return,
        };
        raw.events.push(event);
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
