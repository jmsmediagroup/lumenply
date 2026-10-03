//! Session persistence: the autosave backup, crash recovery and the
//! recent-files list. Everything lives in `~/.lumenply` (migrated
//! automatically from the pre-naming `~/.nge`).

use std::path::Path;

use super::*;

const RECENT_MAX: usize = 10;

/// A rebindable key chord. The key is stored by its egui name so it
/// serialises readably ("Z", "F5", "ArrowLeft").
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Chord {
    pub cmd: bool,
    pub shift: bool,
    pub key: String,
}

/// The rebindable actions: (id, label, default cmd, default shift, key).
/// Order matters for dispatch: chords that contain another chord (redo
/// holds undo's) must be consumed first.
pub(crate) const SHORTCUTS: &[(&str, &str, bool, bool, &str)] = &[
    ("redo", "Redo", true, true, "Z"),
    ("liquify", "Liquify", true, true, "X"),
    ("undo", "Undo", true, false, "Z"),
    ("invert-sel", "Invert selection", true, true, "I"),
    // Before "select-all": Shift+Cmd+A contains Cmd+A.
    ("camera-raw-filter", "Camera Raw Filter", true, true, "A"),
    ("select-all", "Select all", true, false, "A"),
    // Before "deselect": Shift+Cmd+D contains Cmd+D.
    ("reselect", "Reselect", true, true, "D"),
    ("deselect", "Deselect", true, false, "D"),
    // Before "save": Shift+Cmd+S contains Cmd+S.
    ("saveas", "Save as", true, true, "S"),
    ("save", "Save", true, false, "S"),
    ("open", "Open", true, false, "O"),
    // Before "new": Shift+Cmd+N contains Cmd+N.
    ("new-layer", "New layer", true, true, "N"),
    ("new", "New document", true, false, "N"),
    ("close", "Close document", true, false, "W"),
    ("xform", "Free transform", true, false, "T"),
    ("group", "Group layers", true, false, "G"),
    // Before "layer-via-copy": Shift+Cmd+J contains Cmd+J.
    ("layer-via-cut", "Layer via cut", true, true, "J"),
    ("layer-via-copy", "Layer via copy", true, false, "J"),
    ("merge-visible", "Merge visible", true, true, "E"),
    ("merge-down", "Merge down", true, false, "E"),
    ("rulers", "Rulers", true, false, "R"),
    // Before "guides": Shift+Cmd+; contains Cmd+;.
    ("snap", "Snap", true, true, "Semicolon"),
    ("guides", "Show guides", true, false, "Semicolon"),
    ("grid", "Show grid", true, false, "Quote"),
    ("adj-desaturate", "Desaturate", true, true, "U"),
    ("auto-tone", "Auto tone", true, true, "L"),
    // After their Shift chords above, which contain these.
    ("adjd-levels", "Levels", true, false, "L"),
    ("adjd-curves", "Curves", true, false, "M"),
    ("adjd-hue-saturation", "Hue/Saturation", true, false, "U"),
    ("adjd-invert", "Invert", true, false, "I"),
    // Before "proof-colors": Shift+Cmd+Y contains Cmd+Y.
    ("gamut-warning", "Gamut warning", true, true, "Y"),
    ("proof-colors", "Proof colors", true, false, "Y"),
];

/// The effective chord for an action: the user's binding when it parses,
/// else the built-in default.
pub(crate) fn resolve_chord(prefs: &Prefs, id: &str) -> Option<(egui::Modifiers, egui::Key)> {
    let (_, _, dc, ds, dk) = SHORTCUTS.iter().find(|(i, ..)| *i == id)?;
    let (cmd, shift, key) = match prefs.shortcuts.get(id) {
        Some(c) => (c.cmd, c.shift, egui::Key::from_name(&c.key)),
        None => (*dc, *ds, egui::Key::from_name(dk)),
    };
    let key = key.or_else(|| egui::Key::from_name(dk))?;
    let m = egui::Modifiers {
        command: cmd,
        shift,
        ..egui::Modifiers::NONE
    };
    Some((m, key))
}

/// Human-readable form of an action's effective chord, written the way
/// this platform writes shortcuts (as the menus do).
pub(crate) fn chord_label(ctx: &egui::Context, prefs: &Prefs, id: &str) -> String {
    match resolve_chord(prefs, id) {
        Some((m, k)) => crate::theme::shortcut_text(ctx, m, k),
        None => "—".into(),
    }
}

/// User preferences, persisted as JSON in the data dir.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct Prefs {
    /// Canvas surround colour (sRGB bytes).
    pub canvas_bg: [u8; 3],
    /// Undo history cap in steps.
    pub undo_steps: usize,
    /// Undo history cap in megabytes.
    pub undo_memory_mb: usize,
    /// Seconds of unsaved changes between autosave backups.
    pub autosave_secs: u64,
    /// User key bindings by action id; missing ids use the defaults.
    pub shortcuts: std::collections::BTreeMap<String, Chord>,
    /// Saved brush configurations, selectable from the brush options bar.
    pub brush_presets: Vec<BrushPreset>,
    /// The history strip is folded down to its one-line header.
    pub history_collapsed: bool,
    /// Pen pressure scales the brush size (Photoshop's default).
    pub pen_size: bool,
    /// Pen pressure scales each dab's opacity.
    pub pen_opacity: bool,
    /// View ▸ Rulers along the canvas.
    pub show_rulers: bool,
    /// View ▸ Show guides.
    pub show_guides: bool,
    /// View ▸ Lock guides: the Move tool leaves them alone.
    pub lock_guides: bool,
    /// View ▸ Show grid.
    pub show_grid: bool,
    /// View ▸ Snap: drags snap to guides, grid, canvas and layer edges.
    pub snap: bool,
    /// View ▸ Show smart guides: Move drags show and snap to alignments.
    pub smart_guides: bool,
    /// Grid line every this many document pixels...
    pub grid_spacing: f32,
    /// ...with this many subdivisions per cell.
    pub grid_subdivisions: u32,
    /// Gradients saved from the Gradient tool's popover.
    #[serde(default)]
    pub gradient_presets: Vec<crate::gradient_ui::GradientPreset>,
    /// The brush as it was at the last exit (tip, shape, dynamics),
    /// restored at launch.
    pub current_brush: Option<Box<BrushPreset>>,
    /// The dock's tab and the floating panels (panels.rs).
    pub panels: crate::panels::PanelPrefs,
    /// Memory for rendered tiles the canvas can reuse (the edit graph's
    /// render cache, ADR 0025), in megabytes; see [`DEFAULT_RENDER_CACHE_MB`].
    pub render_cache_mb: usize,
    /// Show the render cache's memory use in the status bar.
    pub show_render_cache: bool,
}

/// The render cache's default budget. Rendering keeps the image, layers'
/// pixels and the backdrop under the layer being edited (transient
/// composites, see `lumenply_graph::Renderer::set_transient_composites`):
/// on a 4000 × 3000 document each is 192 MB in 32-bit float, so 1.5 GB
/// holds the image with an undo step or two of it and a backdrop besides,
/// and keeps the previous versions of everything a session of local edits
/// touches, while staying under a tenth of a 16 GB machine.
pub(crate) const DEFAULT_RENDER_CACHE_MB: usize = 1536;

/// Lowest and highest render cache budgets the Preferences offer (MB).
pub(crate) const RENDER_CACHE_MB: std::ops::RangeInclusive<usize> = 256..=16384;

/// One saved brush setup (the shape parameters; colour stays with the
/// colour well, mode with the options bar).
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct BrushPreset {
    pub name: String,
    pub radius: f32,
    pub hardness: f32,
    pub spacing: f32,
    pub jitter: f32,
    pub opacity: f32,
    /// Tip id: "" is the round tip, `builtin:<name>` a generated one,
    /// `abr-<hash>` an imported one (brush_panel.rs).
    pub tip: String,
    /// Tip angle in degrees, counter-clockwise.
    pub angle: f32,
    /// Tip roundness, 0.01–1.
    pub roundness: f32,
    pub dynamics: crate::brush_panel::PresetDynamics,
}

impl Default for BrushPreset {
    fn default() -> Self {
        BrushPreset {
            name: "Preset".into(),
            radius: 8.0,
            hardness: 0.8,
            spacing: 0.2,
            jitter: 0.0,
            opacity: 1.0,
            tip: String::new(),
            angle: 0.0,
            roundness: 1.0,
            dynamics: Default::default(),
        }
    }
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            canvas_bg: [0x14, 0x16, 0x19], // theme::GROUND
            undo_steps: 100,
            undo_memory_mb: 1024,
            autosave_secs: 120,
            shortcuts: std::collections::BTreeMap::new(),
            brush_presets: Vec::new(),
            history_collapsed: false,
            pen_size: true,
            pen_opacity: false,
            show_rulers: false,
            show_guides: true,
            lock_guides: false,
            show_grid: false,
            snap: true,
            smart_guides: true,
            grid_spacing: 100.0,
            grid_subdivisions: 4,
            gradient_presets: Vec::new(),
            current_brush: None,
            panels: Default::default(),
            render_cache_mb: DEFAULT_RENDER_CACHE_MB,
            show_render_cache: false,
        }
    }
}

impl Prefs {
    pub(crate) fn load() -> Self {
        data_dir()
            .and_then(|d| std::fs::read_to_string(d.join("prefs.json")).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub(crate) fn save(&self) {
        if let Some(d) = data_dir() {
            let _ = std::fs::create_dir_all(&d);
            if let Ok(json) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(d.join("prefs.json"), json);
            }
        }
    }

    pub(crate) fn canvas_color(&self) -> Color32 {
        Color32::from_rgb(self.canvas_bg[0], self.canvas_bg[1], self.canvas_bg[2])
    }

    pub(crate) fn autosave_every(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.autosave_secs.clamp(15, 3600))
    }

    /// Push the limits into an editor.
    pub(crate) fn apply(&self, editor: &mut Editor) {
        editor.history_limit = self.undo_steps.clamp(1, 10_000);
        editor.history_memory_limit = self.undo_memory_mb.clamp(16, 1 << 20) << 20;
        // A quarter for whole-layer results (text, fills, smart filters),
        // the rest for tiles.
        let budget = self
            .render_cache_mb
            .clamp(*RENDER_CACHE_MB.start(), *RENDER_CACHE_MB.end())
            << 20;
        let r = editor.renderer();
        r.cache.set_budget(budget / 4 * 3);
        r.wholes.set_budget(budget / 4);
        r.set_transient_composites(true);
    }
}

/// Bytes the editor's render caches hold (tiles and whole-layer results).
pub(crate) fn render_cache_bytes(editor: &Editor) -> usize {
    let r = editor.renderer();
    r.cache.stats().bytes + r.wholes.stats().1
}

pub(crate) fn data_dir() -> Option<PathBuf> {
    // Unit tests drive the real open/save paths; they must never read or
    // write the user's recent list, prefs or autosave backup.
    // Each test thread gets its own folder: tests run in parallel, and one
    // test's autosave backup would otherwise put a Recover prompt in front
    // of another test's keystrokes.
    if cfg!(test) {
        let base = std::env::temp_dir().join("lumenply-unit-test-data");
        return Some(match std::thread::current().name() {
            Some(test) => base.join(test.replace("::", "-")),
            None => base,
        });
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| {
            let home = PathBuf::from(h);
            let dir = home.join(".lumenply");
            // One-time migration from the working-title directory.
            let legacy = home.join(".nge");
            if !dir.exists() && legacy.exists() {
                let _ = std::fs::rename(&legacy, &dir);
            }
            dir
        })
}

pub(crate) fn autosave_file() -> Option<PathBuf> {
    data_dir().map(|d| d.join("autosave.lumen"))
}

/// Sidecar remembering which file the autosaved document came from.
pub(crate) fn autosave_source_file() -> Option<PathBuf> {
    data_dir().map(|d| d.join("autosave.src"))
}

/// Folder of per-document backups: `<n>.lumen` plus `<n>.src` (where the
/// document came from) for each unsaved open document.
pub(crate) fn autosave_dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("autosave"))
}

pub(crate) fn remove_autosave() {
    // "autosave.nge" is the backup's pre-rename name; a migrated directory
    // may still hold one, and nothing else ever cleans it up.
    let legacy = data_dir().map(|d| d.join("autosave.nge"));
    for p in [autosave_file(), autosave_source_file(), legacy]
        .into_iter()
        .flatten()
    {
        let _ = std::fs::remove_file(p);
    }
    if let Some(dir) = autosave_dir() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Back up every unsaved open document (and where each came from), off
/// the UI thread, replacing the previous set. Project saves are atomic, so
/// a crash mid-write never leaves a corrupt backup.
pub(crate) fn autosave_all(docs: Vec<(Document, Option<PathBuf>)>) {
    std::thread::spawn(move || write_backups(&docs));
}

/// [`autosave_all`]'s work, on the calling thread.
pub(crate) fn write_backups(docs: &[(Document, Option<PathBuf>)]) {
    let (Some(dir), Some(single), Some(single_src)) =
        (autosave_dir(), autosave_file(), autosave_source_file())
    else {
        return;
    };
    {
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        for (i, (doc, source)) in docs.iter().enumerate() {
            if project::save(dir.join(format!("{i}.lumen")), doc).is_ok() {
                let text = source
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let _ = std::fs::write(dir.join(format!("{i}.src")), text);
            }
        }
        // Slots past this set, and the single backup older versions wrote,
        // are stale now.
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let p = e.path();
                let slot = p
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.parse::<usize>().ok());
                if slot.is_some_and(|n| n >= docs.len()) {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
        let _ = std::fs::remove_file(single);
        let _ = std::fs::remove_file(single_src);
    }
}

/// Every backup a previous session left: the documents and where each
/// came from, in tab order.
pub(crate) fn autosave_backups() -> Vec<(PathBuf, Option<PathBuf>)> {
    let mut out = Vec::new();
    if let Some(f) = autosave_file().filter(|f| f.exists()) {
        out.push((f, autosave_source()));
    }
    if let Some(dir) = autosave_dir() {
        let mut slots: Vec<(usize, PathBuf)> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "lumen"))
            .filter_map(|p| Some((p.file_stem()?.to_str()?.parse().ok()?, p)))
            .collect();
        slots.sort();
        for (_, p) in slots {
            let src = std::fs::read_to_string(p.with_extension("src"))
                .ok()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .map(PathBuf::from);
            out.push((p, src));
        }
    }
    out
}

/// The original path of the autosaved document, if it had one.
pub(crate) fn autosave_source() -> Option<PathBuf> {
    let text = std::fs::read_to_string(autosave_source_file()?).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| PathBuf::from(text))
}

pub(crate) fn load_recent() -> Vec<String> {
    data_dir().map_or_else(Vec::new, |d| load_recent_in(&d))
}

pub(crate) fn push_recent(path: &str) -> Vec<String> {
    match data_dir() {
        Some(d) => push_recent_in(&d, path),
        None => Vec::new(),
    }
}

/// Drop one entry (a file that was moved, or one the user removed).
pub(crate) fn remove_recent(path: &str) -> Vec<String> {
    match data_dir() {
        Some(d) => remove_recent_in(&d, path),
        None => Vec::new(),
    }
}

/// Forget every recent file.
pub(crate) fn clear_recent() -> Vec<String> {
    if let Some(d) = data_dir() {
        let _ = std::fs::remove_file(d.join("recent.txt"));
    }
    Vec::new()
}

fn remove_recent_in(dir: &Path, path: &str) -> Vec<String> {
    let mut list = load_recent_in(dir);
    let before = list.len();
    list.retain(|p| p != path);
    if list.len() != before {
        let _ = std::fs::write(dir.join("recent.txt"), list.join("\n"));
    }
    list
}

fn load_recent_in(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("recent.txt"))
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .take(RECENT_MAX)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Move (or insert) `path` to the front of the list and persist it.
fn push_recent_in(dir: &Path, path: &str) -> Vec<String> {
    let mut list = load_recent_in(dir);
    list.retain(|p| p != path);
    list.insert(0, path.to_string());
    list.truncate(RECENT_MAX);
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join("recent.txt"), list.join("\n"));
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join("nge-session-test").join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn recent_list_dedupes_promotes_and_truncates() {
        let dir = temp_dir("recent");
        assert!(load_recent_in(&dir).is_empty());
        for i in 0..12 {
            push_recent_in(&dir, &format!("/tmp/file{i}.nge"));
        }
        let list = load_recent_in(&dir);
        assert_eq!(list.len(), 10, "capped at ten");
        assert_eq!(list[0], "/tmp/file11.nge", "newest first");

        // Reopening an old entry promotes it without duplicating it.
        let list = push_recent_in(&dir, "/tmp/file5.nge");
        assert_eq!(list[0], "/tmp/file5.nge");
        assert_eq!(list.iter().filter(|p| *p == "/tmp/file5.nge").count(), 1);
        assert_eq!(list.len(), 10);

        // The list survives a reload.
        assert_eq!(load_recent_in(&dir), list);
    }

    #[test]
    fn recent_entries_can_be_removed() {
        let dir = temp_dir("recent-remove");
        for i in 0..4 {
            push_recent_in(&dir, &format!("/tmp/r{i}.lumen"));
        }
        let list = remove_recent_in(&dir, "/tmp/r2.lumen");
        assert_eq!(list, ["/tmp/r3.lumen", "/tmp/r1.lumen", "/tmp/r0.lumen"]);
        assert_eq!(load_recent_in(&dir), list, "the removal is persisted");
        // Removing something that is not listed changes nothing.
        assert_eq!(remove_recent_in(&dir, "/tmp/nope.lumen").len(), 3);
        // Removing the last entry leaves an empty (still loadable) list.
        for p in list {
            remove_recent_in(&dir, &p);
        }
        assert!(load_recent_in(&dir).is_empty());
    }

    #[test]
    fn prefs_round_trip_keeps_brush_presets() {
        let mut p = Prefs::default();
        p.brush_presets.push(BrushPreset {
            name: "Soft 60".into(),
            radius: 30.0,
            hardness: 0.2,
            spacing: 0.15,
            jitter: 0.4,
            opacity: 0.7,
            ..BrushPreset::default()
        });
        let json = serde_json::to_string(&p).unwrap();
        let back: Prefs = serde_json::from_str(&json).unwrap();
        assert!(back == p, "presets survive the JSON round trip");
        // Old prefs files without the field still load.
        let legacy: Prefs = serde_json::from_str("{\"undo_steps\": 42}").unwrap();
        assert_eq!(legacy.undo_steps, 42);
        assert!(legacy.brush_presets.is_empty());
        assert!(!legacy.history_collapsed, "the history strip starts open");
    }

    #[test]
    fn prefs_round_trip_keeps_the_history_fold() {
        let p = Prefs {
            history_collapsed: true,
            ..Prefs::default()
        };
        let back: Prefs = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert!(back.history_collapsed);
    }
}
