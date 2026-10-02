//! Session persistence: the autosave backup, crash recovery and the
//! recent-files list. Everything lives in `~/.nge` (or `%USERPROFILE%\.nge`).

use std::path::Path;

use super::*;

pub(crate) const AUTOSAVE_EVERY: std::time::Duration = std::time::Duration::from_secs(120);
const RECENT_MAX: usize = 10;

pub(crate) fn data_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".nge"))
}

pub(crate) fn autosave_file() -> Option<PathBuf> {
    data_dir().map(|d| d.join("autosave.nge"))
}

/// Sidecar remembering which file the autosaved document came from.
pub(crate) fn autosave_source_file() -> Option<PathBuf> {
    data_dir().map(|d| d.join("autosave.src"))
}

pub(crate) fn remove_autosave() {
    for p in [autosave_file(), autosave_source_file()].into_iter().flatten() {
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
