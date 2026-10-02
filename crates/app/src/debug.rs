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
            || self.debug_liquify(ctx, tok)
            || self.debug_camera_raw(ctx, tok)
            || self.debug_select(tok)
    }

    /// Selections and what acts on them (`select:...`):
    /// `select:rect=X:Y:W:H` and `select:ellipse=X:Y:W:H` replace the
    /// selection (canvas pixels); `select:layer=Name` makes the layer of
    /// that name active; `select:caf` runs Content-Aware Fill on the active
    /// pixel layer straight away (no dialog), timing it in the status bar.
    fn debug_select(&mut self, tok: &str) -> bool {
        let Some(rest) = tok.strip_prefix("select:") else {
            return false;
        };
        let (verb, arg) = rest.split_once('=').unwrap_or((rest, ""));
        let nums: Vec<i32> = arg.split(':').filter_map(|s| s.trim().parse().ok()).collect();
        let rect = (nums.len() == 4)
            .then(|| Rect::new(nums[0], nums[1], nums[2].max(1) as u32, nums[3].max(1) as u32));
        match (verb, rect) {
            ("rect", Some(r)) => self.run(&SetSelection {
                selection: Some(Selection::rect(r)),
            }),
            ("ellipse", Some(r)) => self.run(&SetSelection {
                selection: Some(Selection::ellipse(r)),
            }),
            ("layer", _) => {
                let mut found = None;
                self.editor.doc().for_each_layer(|l| {
                    if l.name == arg {
                        found = Some(l.id);
                    }
                });
                self.set_active(found);
            }
            ("caf", _) => {
                if let Some(layer) = self.active {
                    let margin = self.content_aware_margin().round() as u32;
                    let t = std::time::Instant::now();
                    self.run(&ContentAwareFill { layer, margin });
                    self.status = format!("Content-Aware Fill took {:.2} s", t.elapsed().as_secs_f32());
                    eprintln!("{}", self.status);
                }
            }
            _ => return false,
        }
        true
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
                // `Enter`, or with modifiers: `Cmd+Equals`, `Shift+Cmd+Z`.
                let mut modifiers = egui::Modifiers::NONE;
                let mut name = target.as_str();
                while let Some((m, rest)) = name.split_once('+') {
                    match m {
                        "Cmd" => {
                            modifiers.command = true;
                            if cfg!(target_os = "macos") {
                                modifiers.mac_cmd = true;
                            } else {
                                modifiers.ctrl = true;
                            }
                        }
                        "Shift" => modifiers.shift = true,
                        "Alt" => modifiers.alt = true,
                        _ => break,
                    }
                    name = rest;
                }
                if let Some(key) = Key::from_name(name) {
                    raw.modifiers = modifiers;
                    for pressed in [true, false] {
                        raw.events.push(egui::Event::Key {
                            key,
                            physical_key: None,
                            pressed,
                            repeat: false,
                            modifiers,
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
    ///
    /// `layout:tool=<name>` picks a tool by name ("magic-wand", "brush"),
    /// `layout:layer=<name>` activates a layer, `layout:mask` targets its
    /// mask, `layout:rename` opens the rename field, `layout:name=<text>`
    /// renames the active layer, `layout:empty` deletes every layer,
    /// `layout:about` / `recover` / `confirm-close` / `confirm-tab` /
    /// `export-jpeg` open those dialogs, `layout:history` folds or opens the history strip,
    /// `layout:focus-tool=<name>` gives a rail button keyboard focus,
    /// `layout:live-blur` adds a live blur layer above the active one,
    /// `layout:fx` turns on a drop shadow and a stroke on the active layer.
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
            // The Properties/Layers divider, as if dragged to give Layers
            // this many points of height.
            "split" => {
                if let Ok(h) = arg.parse::<f32>() {
                    ctx.data_mut(|d| d.insert_persisted(egui::Id::new("dock-layers-h"), h));
                }
            }
            "about" => self.dialog = Some(Dialog::About),
            "recover" => self.dialog = Some(Dialog::Recover),
            "confirm-close" => self.dialog = Some(Dialog::ConfirmClose),
            "confirm-tab" => self.dialog = Some(Dialog::ConfirmCloseTab(0)),
            // The JPEG options normally open after the native save panel.
            "export-jpeg" => self.dialog = Some(Dialog::ExportJpeg("/tmp/untitled.jpg".into(), 90)),
            // Flip without saving: a screenshot run must not rewrite the
            // user's preferences.
            "history" => self.prefs.history_collapsed = !self.prefs.history_collapsed,
            "live-blur" => self.add_filter_layer(Filter::GaussianBlur { radius: 8.0 }),
            // Drop shadow and stroke on the active layer (effects section).
            "fx" => {
                if let Some(layer) = self.active {
                    let effects = lumenply_doc::LayerEffects {
                        drop_shadow: Some(lumenply_doc::ShadowFx::default()),
                        stroke: Some(lumenply_doc::StrokeFx::default()),
                        ..Default::default()
                    };
                    self.run(&SetLayerEffects { layer, effects });
                }
            }
            _ => return false,
        }
        true
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
