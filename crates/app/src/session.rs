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
    ("undo", "Undo", true, false, "Z"),
    ("invert-sel", "Invert selection", true, true, "I"),
    ("select-all", "Select all", true, false, "A"),
    ("deselect", "Deselect", true, false, "D"),
    ("save", "Save", true, false, "S"),
    ("open", "Open", true, false, "O"),
    ("xform", "Free transform", true, false, "T"),
    ("group", "Group layers", true, false, "G"),
    ("layer-via-copy", "Layer via copy", true, false, "J"),
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

/// Human-readable form of an action's effective chord.
pub(crate) fn chord_label(prefs: &Prefs, id: &str) -> String {
    let Some((m, k)) = resolve_chord(prefs, id) else {
        return "—".into();
    };
    let mut out = String::new();
    if m.contains(egui::Modifiers::COMMAND) {
        out.push_str("Ctrl+");
    }
    if m.contains(egui::Modifiers::SHIFT) {
        out.push_str("Shift+");
    }
    out.push_str(k.name());
    out
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
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            canvas_bg: [0x14, 0x16, 0x19], // theme::GROUND
            undo_steps: 100,
            undo_memory_mb: 1024,
            autosave_secs: 120,
            shortcuts: std::collections::BTreeMap::new(),
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
    }
}

pub(crate) fn data_dir() -> Option<PathBuf> {
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
}

/// Write the document (and where it came from) as the autosave backup, off
/// the UI thread. The project save is atomic, so a crash mid-write never
/// leaves a corrupt backup.
pub(crate) fn autosave(doc: Document, source: Option<PathBuf>) {
    let (Some(dir), Some(file), Some(src)) = (data_dir(), autosave_file(), autosave_source_file()) else {
        return;
    };
    std::thread::spawn(move || {
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        if project::save(&file, &doc).is_ok() {
            let text = source
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let _ = std::fs::write(src, text);
        }
    });
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
}
