//! Files opened from Finder (double-click, Open With, a drop on the Dock
//! icon): macOS sends them to the application delegate, which eframe 0.29
//! does not forward. `install` (before the event loop starts) gives every
//! object an `application:openURLs:` method, so winit's delegate — an
//! NSObject — answers it even for the file that launches the app; the
//! paths queue up for the app to open on its next frame.

use std::sync::Mutex;

static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());
static WAKE: std::sync::OnceLock<eframe::egui::Context> = std::sync::OnceLock::new();

/// Paths macOS asked us to open since the last call.
pub(crate) fn take_pending() -> Vec<String> {
    PENDING
        .lock()
        .map(|mut p| std::mem::take(&mut *p))
        .unwrap_or_default()
}

/// Lets a file arriving while the app idles trigger a frame.
pub(crate) fn set_waker(ctx: &eframe::egui::Context) {
    let _ = WAKE.set(ctx.clone());
}

#[cfg(target_os = "macos")]
pub(crate) fn install() {
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2_foundation::{NSArray, NSURL};

    unsafe extern "C-unwind" fn open_urls(
        _this: *mut AnyObject,
        _cmd: Sel,
        _app: *mut AnyObject,
        urls: *mut NSArray<NSURL>,
    ) {
        // SAFETY: AppKit passes a live array of file URLs.
        let Some(urls) = (unsafe { urls.as_ref() }) else {
            return;
        };
        let paths: Vec<String> = urls
            .iter()
            .filter_map(|u| u.path())
            .map(|p| p.to_string())
            .collect();
        if let Ok(mut q) = PENDING.lock() {
            q.extend(paths);
        }
        if let Some(ctx) = WAKE.get() {
            ctx.request_repaint();
        }
    }

    let Some(cls) = AnyClass::get(c"NSObject") else {
        return;
    };
    // SAFETY: the implementation matches the selector's type encoding
    // (void, self, _cmd, NSApplication*, NSArray*); adding a method that
    // already exists is a harmless no-op.
    unsafe {
        let imp: Imp = std::mem::transmute::<
            unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut NSArray<NSURL>),
            Imp,
        >(open_urls);
        objc2::ffi::class_addMethod(
            cls as *const AnyClass as *mut AnyClass,
            objc2::sel!(application:openURLs:),
            imp,
            c"v@:@@".as_ptr(),
        );
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn install() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_paths_are_handed_over_once() {
        PENDING
            .lock()
            .unwrap()
            .extend(["/a.lumen".to_string(), "/b.psd".to_string()]);
        assert_eq!(take_pending(), vec!["/a.lumen", "/b.psd"]);
        assert!(take_pending().is_empty());
    }
}
