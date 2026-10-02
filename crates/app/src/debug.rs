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
    /// (any egui key name), `popups:type=text`, `popups:sleep=ms` (real
    /// time, for tooltip delays). A target `T` is
    /// `X:Y` in points or a name recorded by [`theme::note_target`]: a
    /// menu title ("File", "Export"), a menu item label ("Undo"), or an
    /// icon menu id ("add-adj", "layer-more", "export-button").
    fn debug_popups(&mut self, ctx: &egui::Context, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("popups:") else {
            return false;
        };
        let (verb, target) = rest.split_once('=').unwrap_or((rest, ""));
        let t = target.to_string();
        // The pointer glides the last few points over three frames, as a
        // real mouse does: egui only counts it as having moved (which
        // tooltips wait for after a click) once it has a velocity.
        let glide = |t: &String| vec![("move+8", t.clone()), ("move+4", t.clone()), ("move", t.clone())];
        let steps: Vec<(&str, String)> = match verb {
            "click" => [glide(&t), vec![("press-l", t.clone()), ("release-l", t)]].concat(),
            "rclick" => [glide(&t), vec![("press-r", t.clone()), ("release-r", t)]].concat(),
            "hover" => [glide(&t), vec![("wait", String::new())]].concat(),
            "wait" => vec![("wait", String::new())],
            "esc" => vec![("key", "Escape".into())],
            "key" => vec![("key", t)],
            "type" => vec![("type", t)],
            "sleep" => vec![("sleep", t)],
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
    /// Does nothing unless `--screenshot-do` queued steps. Once a replay
    /// has started, the real pointer is ignored (the OS cursor entering or
    /// leaving the window would otherwise clear hovers mid-capture) and the
    /// replayed pointer stays where it was last put.
    pub(crate) fn debug_popups_input(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        let id = egui::Id::new("popups:steps");
        let last_id = egui::Id::new("popups:pointer");
        let (step, replaying) = ctx.data_mut(|d| {
            let q = d.get_temp_mut_or_default::<Vec<(String, String)>>(id);
            let step = (!q.is_empty()).then(|| q.remove(0));
            let replaying = step.is_some() || d.get_temp::<bool>(id.with("started")).unwrap_or(false);
            (step, replaying)
        });
        if !replaying {
            return;
        }
        ctx.data_mut(|d| d.insert_temp(id.with("started"), true));
        raw.events.retain(|e| {
            !matches!(
                e,
                egui::Event::PointerMoved(_)
                    | egui::Event::PointerButton { .. }
                    | egui::Event::PointerGone
                    | egui::Event::MouseMoved(_)
            )
        });
        if let Some(p) = ctx.data(|d| d.get_temp::<Pos2>(last_id)) {
            raw.events.push(egui::Event::PointerMoved(p));
        }
        let Some((verb, target)) = step else {
            return;
        };
        match verb.as_str() {
            // Real time passing, for tooltip delays.
            "sleep" => {
                let ms = target.parse().unwrap_or(600);
                std::thread::sleep(std::time::Duration::from_millis(ms));
                return;
            }
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
        let pos = match verb.strip_prefix("move+").and_then(|d| d.parse::<f32>().ok()) {
            Some(dx) => pos.map(|p| p + egui::vec2(dx, 0.0)),
            None => pos,
        };
        let event = match (verb.as_str(), pos) {
            (v, Some(p)) if v.starts_with("move") => egui::Event::PointerMoved(p),
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
        if let Some(p) = pos {
            ctx.data_mut(|d| d.insert_temp(last_id, p));
        }
        raw.events.push(event);
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
    fn debug_color(&mut self, ctx: &egui::Context, tok: &str) -> bool {
        self.debug_color_token(ctx, tok)
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
