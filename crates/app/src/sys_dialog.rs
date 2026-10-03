//! The system's open and save panels, behind one seam.
//!
//! Every file the user picks goes through [`FileDialog`], a thin stand-in
//! for `rfd::FileDialog` with the same builder calls. In a normal build it
//! is rfd and nothing else. With the `uitest` feature, the user-session
//! harness (uitest/) answers the panel in place of a person, because a
//! native panel can't open headlessly; without a harness on the calling
//! thread it is still rfd.

use std::path::{Path, PathBuf};

/// What the app asked the system panel for, as a person would see it.
#[derive(Clone, Debug, Default, serde::Serialize)]
#[cfg_attr(not(feature = "uitest"), allow(dead_code))]
pub(crate) struct DialogAbout {
    /// "open" or "save".
    pub kind: &'static str,
    pub title: String,
    /// (description, extensions) in the order the panel lists them.
    pub filters: Vec<(String, Vec<String>)>,
    /// The name a save panel suggests.
    pub file_name: Option<String>,
    pub directory: Option<PathBuf>,
}

pub(crate) struct FileDialog {
    rfd: rfd::FileDialog,
    about: DialogAbout,
}

impl FileDialog {
    pub(crate) fn new() -> FileDialog {
        FileDialog {
            rfd: rfd::FileDialog::new(),
            about: DialogAbout::default(),
        }
    }

    pub(crate) fn set_directory<P: AsRef<Path>>(mut self, dir: P) -> FileDialog {
        self.about.directory = Some(dir.as_ref().to_path_buf());
        self.rfd = self.rfd.set_directory(dir);
        self
    }

    pub(crate) fn set_title(mut self, title: impl Into<String>) -> FileDialog {
        let title = title.into();
        self.about.title = title.clone();
        self.rfd = self.rfd.set_title(title);
        self
    }

    pub(crate) fn add_filter(mut self, name: impl Into<String>, extensions: &[impl ToString]) -> FileDialog {
        let name = name.into();
        let exts: Vec<String> = extensions.iter().map(|e| e.to_string()).collect();
        self.rfd = self.rfd.add_filter(name.clone(), &exts);
        self.about.filters.push((name, exts));
        self
    }

    pub(crate) fn set_file_name(mut self, name: impl Into<String>) -> FileDialog {
        let name = name.into();
        self.about.file_name = Some(name.clone());
        self.rfd = self.rfd.set_file_name(name);
        self
    }

    pub(crate) fn pick_file(mut self) -> Option<PathBuf> {
        self.about.kind = "open";
        #[cfg(feature = "uitest")]
        if let Some(answer) = crate::uitest::seam::answer_dialog(&self.about) {
            return answer;
        }
        self.rfd.pick_file()
    }

    pub(crate) fn save_file(mut self) -> Option<PathBuf> {
        self.about.kind = "save";
        #[cfg(feature = "uitest")]
        if let Some(answer) = crate::uitest::seam::answer_dialog(&self.about) {
            return answer;
        }
        self.rfd.save_file()
    }
}
