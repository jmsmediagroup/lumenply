//! The session's UI thread: it owns the real `App` and its egui context
//! and runs each frame exactly as eframe's event loop would.
//!
//! A thread of its own because a file panel is modal: the app calls it in
//! the middle of a frame and waits for the user's answer. Here the frame
//! stays parked inside that call (see seam.rs) while the scenario, on the
//! harness thread, looks at the panel's request and answers it.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use eframe::egui;

use crate::sys_dialog::DialogAbout;
use crate::App;

use super::seam::{self, SharedClip};

/// What lives on the UI thread.
pub(crate) struct UiState {
    pub app: App,
    pub ctx: egui::Context,
}

type Job = Box<dyn FnOnce(&mut UiState) -> Box<dyn Any + Send> + Send>;

pub(crate) enum Reply {
    /// A job finished (or panicked, with its message).
    Done(Result<Box<dyn Any + Send>, String>),
    /// The running frame opened a file panel and waits for an answer.
    Dialog(DialogAbout),
}

pub(crate) struct UiThread {
    jobs: Option<mpsc::Sender<Job>>,
    replies: mpsc::Receiver<Reply>,
    answers: Option<mpsc::Sender<Option<PathBuf>>>,
    handle: Option<std::thread::JoinHandle<()>>,
    /// The thread stopped answering (a hang): never join it.
    pub(crate) dead: bool,
}

fn panic_message(e: Box<dyn Any + Send>) -> String {
    if let Some(s) = e.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = e.downcast_ref::<String>() {
        s.clone()
    } else {
        "a panic without a message".into()
    }
}

impl UiThread {
    /// Start the thread and build the UI state on it with `make`.
    pub(crate) fn start(
        name: String,
        clipboard: SharedClip,
        make: impl FnOnce() -> UiState + Send + 'static,
    ) -> Result<UiThread, String> {
        let (jobs_tx, jobs_rx) = mpsc::channel::<Job>();
        let (reply_tx, reply_rx) = mpsc::channel::<Reply>();
        let (answer_tx, answer_rx) = mpsc::channel::<Option<PathBuf>>();
        let handle = std::thread::Builder::new()
            .name(name)
            // The real app runs on the 8 MB main thread; deep UI closures
            // must not overflow a 2 MB default worker stack.
            .stack_size(32 << 20)
            .spawn(move || {
                seam::install(reply_tx.clone(), answer_rx, clipboard);
                let mut state = match catch_unwind(AssertUnwindSafe(make)) {
                    Ok(s) => {
                        let _ = reply_tx.send(Reply::Done(Ok(Box::new(()))));
                        s
                    }
                    Err(e) => {
                        let _ = reply_tx.send(Reply::Done(Err(panic_message(e))));
                        seam::uninstall();
                        return;
                    }
                };
                while let Ok(job) = jobs_rx.recv() {
                    let r = catch_unwind(AssertUnwindSafe(|| job(&mut state))).map_err(panic_message);
                    if reply_tx.send(Reply::Done(r)).is_err() {
                        break;
                    }
                }
                // The app is dropped here, on its own thread.
                drop(state);
                seam::uninstall();
            })
            .map_err(|e| format!("could not start the UI thread: {e}"))?;
        let mut t = UiThread {
            jobs: Some(jobs_tx),
            replies: reply_rx,
            answers: Some(answer_tx),
            handle: Some(handle),
            dead: false,
        };
        match t.recv(Duration::from_secs(120))? {
            Reply::Done(Ok(_)) => Ok(t),
            Reply::Done(Err(e)) => {
                t.dead = true;
                Err(format!("Lumenply panicked while starting: {e}"))
            }
            Reply::Dialog(d) => Err(format!("a file panel opened during launch: {}", d.title)),
        }
    }

    pub(crate) fn send<R: Send + 'static>(&self, f: impl FnOnce(&mut UiState) -> R + Send + 'static) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(Box::new(move |st| Box::new(f(st)) as Box<dyn Any + Send>));
        }
    }

    pub(crate) fn recv(&mut self, timeout: Duration) -> Result<Reply, String> {
        if self.dead {
            return Err("the UI thread has stopped".into());
        }
        match self.replies.recv_timeout(timeout) {
            Ok(r) => Ok(r),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.dead = true;
                Err(format!(
                    "Lumenply did not finish a frame within {:.0} s (hung?)",
                    timeout.as_secs_f32()
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.dead = true;
                Err("the UI thread ended unexpectedly".into())
            }
        }
    }

    /// Answer the file panel the running frame waits on.
    pub(crate) fn answer(&self, path: Option<PathBuf>) {
        if let Some(a) = &self.answers {
            let _ = a.send(path);
        }
    }
}

impl Drop for UiThread {
    fn drop(&mut self) {
        // A parked file panel gets "cancelled", then the job loop ends.
        self.answers.take();
        self.jobs.take();
        if let Some(h) = self.handle.take() {
            if !self.dead {
                let _ = h.join();
            }
        }
    }
}
