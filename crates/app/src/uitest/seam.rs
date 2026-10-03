//! The two places the app talks to the operating system that a headless
//! session must stand in for: the file panels (sys_dialog.rs) and the
//! image clipboard (clipboard.rs).
//!
//! Each seam is a thread-local installed on the session's UI thread only.
//! Every other thread (the real app's main thread above all) finds it
//! empty and uses the system, so a `uitest` build behaves exactly like a
//! normal one outside a session.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::sys_dialog::DialogAbout;

use super::ui_thread::Reply;

/// The session's in-memory image clipboard: (width, height, sRGB RGBA8).
pub(crate) type SharedClip = Arc<Mutex<Option<(usize, usize, Vec<u8>)>>>;

struct Seams {
    /// Asks the harness to answer a file panel...
    ask: mpsc::Sender<Reply>,
    /// ...and waits here for the path the "user" picked (None: cancelled).
    answer: mpsc::Receiver<Option<PathBuf>>,
    clipboard: SharedClip,
}

thread_local! {
    static SEAMS: RefCell<Option<Seams>> = const { RefCell::new(None) };
}

/// Make the calling thread a session's UI thread.
pub(crate) fn install(
    ask: mpsc::Sender<Reply>,
    answer: mpsc::Receiver<Option<PathBuf>>,
    clipboard: SharedClip,
) {
    SEAMS.with(|s| {
        *s.borrow_mut() = Some(Seams {
            ask,
            answer,
            clipboard,
        })
    });
}

pub(crate) fn uninstall() {
    SEAMS.with(|s| s.borrow_mut().take());
}

/// A file panel opened on a session's UI thread: block, like the modal
/// panel would, until the harness answers. `None` off a session thread.
pub(crate) fn answer_dialog(about: &DialogAbout) -> Option<Option<PathBuf>> {
    SEAMS.with(|s| {
        let s = s.borrow();
        let seams = s.as_ref()?;
        if seams.ask.send(Reply::Dialog(about.clone())).is_err() {
            return Some(None); // the session is gone: as if cancelled
        }
        Some(seams.answer.recv().unwrap_or(None))
    })
}

/// The session clipboard's image (`Some(None)`: it holds none), or `None`
/// off a session thread.
pub(crate) fn clipboard_image() -> Option<Option<(usize, usize, Vec<u8>)>> {
    SEAMS.with(|s| {
        let s = s.borrow();
        let seams = s.as_ref()?;
        Some(seams.clipboard.lock().ok().and_then(|c| c.clone()))
    })
}

/// Put an image on the session clipboard; false off a session thread.
pub(crate) fn set_clipboard_image(w: usize, h: usize, bytes: &[u8]) -> bool {
    SEAMS.with(|s| {
        let s = s.borrow();
        let Some(seams) = s.as_ref() else {
            return false;
        };
        if let Ok(mut c) = seams.clipboard.lock() {
            *c = Some((w, h, bytes.to_vec()));
        }
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seams_are_inert_off_a_session_thread() {
        // The real app's thread: the system answers, not the harness.
        assert!(answer_dialog(&DialogAbout::default()).is_none());
        assert!(clipboard_image().is_none());
        assert!(!set_clipboard_image(1, 1, &[0; 4]));
    }
}
