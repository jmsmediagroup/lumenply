//! Long-running work off the UI thread: a job runs a closure on its own
//! thread, reports progress and its phase through atomics, can be asked to
//! stop, and hands back one result the UI picks up with [`Job::poll`].
//! Every progress report and the finish wake the UI (a repaint), so the
//! window stays responsive and still shows each step.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

use eframe::egui;

/// What a job reports while it runs.
pub(crate) struct Progress {
    done: AtomicU64,
    total: AtomicU64,
    phase: AtomicU8,
    waker: Option<egui::Context>,
}

impl Progress {
    /// `done` of `total` units (bytes for a download).
    pub(crate) fn set(&self, done: u64, total: u64) {
        self.total.store(total, Ordering::Relaxed);
        self.done.store(done.min(total), Ordering::Relaxed);
        self.wake();
    }

    /// The job's current phase; what each value means is up to the job.
    pub(crate) fn set_phase(&self, phase: u8) {
        self.phase.store(phase, Ordering::Relaxed);
        self.wake();
    }

    pub(crate) fn get(&self) -> (u64, u64) {
        (
            self.done.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
        )
    }

    /// Done as a fraction of the total (0 while the total is unknown).
    pub(crate) fn fraction(&self) -> f32 {
        let (d, t) = self.get();
        if t == 0 {
            0.0
        } else {
            (d as f64 / t as f64) as f32
        }
    }

    pub(crate) fn phase(&self) -> u8 {
        self.phase.load(Ordering::Relaxed)
    }

    fn wake(&self) {
        if let Some(ctx) = &self.waker {
            ctx.request_repaint();
        }
    }
}

/// A unit of work running on its own thread.
pub(crate) struct Job<T> {
    pub(crate) progress: Arc<Progress>,
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<T>,
    pub(crate) started: Instant,
}

impl<T: Send + 'static> Job<T> {
    /// Run `work` on a new thread. It gets the progress to report into and
    /// the cancel flag to check; its return value is the job's result.
    /// `waker` (the UI's context) is asked to repaint on every report and
    /// when the work ends.
    pub(crate) fn spawn(
        waker: Option<egui::Context>,
        work: impl FnOnce(&Progress, &AtomicBool) -> T + Send + 'static,
    ) -> Job<T> {
        let progress = Arc::new(Progress {
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            phase: AtomicU8::new(0),
            waker,
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let (p, c) = (progress.clone(), cancel.clone());
        let spawned = std::thread::Builder::new().name("ai-job".into()).spawn(move || {
            let out = work(&p, &c);
            // The UI may have dropped the job (its document closed).
            let _ = tx.send(out);
            p.wake();
        });
        if let Err(e) = spawned {
            eprintln!("could not start a background job: {e}");
        }
        Job {
            progress,
            cancel,
            rx,
            started: Instant::now(),
        }
    }

    /// The result once the work has finished; `Err` when the thread died
    /// without one (a panic). `None` while it runs.
    pub(crate) fn poll(&self) -> Option<Result<T, String>> {
        match self.rx.try_recv() {
            Ok(v) => Some(Ok(v)),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("the background job stopped unexpectedly".into()))
            }
        }
    }

    /// Block until the work finishes or `timeout` passes.
    #[cfg(test)]
    pub(crate) fn wait(&self, timeout: Duration) -> Option<Result<T, String>> {
        match self.rx.recv_timeout(timeout) {
            Ok(v) => Some(Ok(v)),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Some(Err("the background job stopped unexpectedly".into()))
            }
        }
    }
}

impl<T> Job<T> {
    /// Ask the work to stop. Work that can't be interrupted runs to its end;
    /// its result is then dropped by whoever cancelled it.
    pub(crate) fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_reports_progress_and_hands_back_its_result() {
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let job = Job::spawn(None, move |p, _| {
            p.set_phase(2);
            p.set(30, 120);
            go_rx.recv().unwrap();
            7 * 6
        });
        // Wait for the first report without racing the worker.
        let t = Instant::now();
        while job.progress.get() != (30, 120) && t.elapsed() < Duration::from_secs(5) {
            std::thread::yield_now();
        }
        assert_eq!((job.progress.get(), job.progress.phase()), ((30, 120), 2));
        assert_eq!(job.progress.fraction(), 0.25);
        assert!(job.poll().is_none(), "still running");
        go_tx.send(()).unwrap();
        assert_eq!(job.wait(Duration::from_secs(5)), Some(Ok(42)));
    }

    #[test]
    fn cancelling_sets_the_flag_the_work_sees() {
        let job = Job::spawn(None, |_, cancel| {
            let t = Instant::now();
            while !cancel.load(Ordering::Relaxed) {
                if t.elapsed() > Duration::from_secs(5) {
                    return "timed out";
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            "stopped"
        });
        job.cancel();
        assert!(job.cancelled());
        assert_eq!(job.wait(Duration::from_secs(10)), Some(Ok("stopped")));
    }

    #[test]
    fn a_job_that_dies_reports_an_error() {
        let job: Job<u8> = Job::spawn(None, |_, _| panic!("boom (expected in this test)"));
        let got = job.wait(Duration::from_secs(5)).expect("finished");
        assert_eq!(got, Err("the background job stopped unexpectedly".to_string()));
    }
}
