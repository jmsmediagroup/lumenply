//! User-session tests: the real app, driven headlessly through the same
//! input a person gives it (pointer, keys, text), seen through its
//! accessibility tree and its rendered frames, and recorded as a video.
//!
//! Built only with the `uitest` feature. How to write and run scenarios:
//! docs/testing/harness.md.
//!
//! - `session.rs`: [`Session`], the API scenarios use.
//! - `ui_thread.rs`: the thread that owns the app and runs its frames.
//! - `gpu.rs`: offscreen egui-wgpu rendering of each frame.
//! - `record.rs`: pointer and caption overlay, video, stills.
//! - `a11y.rs`, `input.rs`: finding controls by name, parsing key chords.
//! - `seam.rs`: the stand-ins for system file panels and the clipboard.
//! - `scenarios/`: the scenarios, one file per feature area.
//! - `runner.rs`: `lumenply-app --uitest`, and the `cargo test` glue.

// The session API offers more than today's scenarios use.
#![allow(dead_code)]

/// Declare a scenario file's scenarios, each with a name and a line
/// about it, and give each its own `cargo test` (ignored by default):
///
/// ```ignore
/// scenario_list! {
///     "first-steps" => first_steps: "Welcome screen to a saved, reopened document",
/// }
/// ```
macro_rules! scenario_list {
    ($($name:literal => $f:ident : $about:literal),* $(,)?) => {
        pub(crate) const SCENARIOS: &[$crate::uitest::Scenario] = &[
            $($crate::uitest::Scenario { name: $name, about: $about, run: $f },)*
        ];

        /// `cargo test --release -p lumenply-app --features uitest uitest_ -- --ignored`
        #[cfg(test)]
        mod uitest_scenarios {
            $(
                #[test]
                #[ignore = "user session: cargo test --release -p lumenply-app --features uitest uitest_ -- --ignored"]
                fn $f() {
                    $crate::uitest::runner::run_test($name, super::$f);
                }
            )*
        }
    };
}
pub(crate) use scenario_list;

pub(crate) mod a11y;
pub(crate) mod gpu;
pub(crate) mod input;
pub(crate) mod record;
pub(crate) mod runner;
pub(crate) mod scenarios;
pub(crate) mod seam;
pub(crate) mod session;
pub(crate) mod ui_thread;

pub(crate) use session::{OnFail, Options, Session, UiError, UiResult};

/// A named scenario: a function that drives a [`Session`].
#[derive(Clone, Copy)]
pub(crate) struct Scenario {
    pub name: &'static str,
    pub about: &'static str,
    pub run: fn(&mut Session) -> UiResult,
}

/// What a scenario file needs: `use crate::uitest::prelude::*;`.
#[allow(unused_imports)] // offered to every scenario file, used by some
pub(crate) mod prelude {
    pub(crate) use super::scenario_list;
    pub(crate) use super::session::find_layer;
    pub(crate) use super::{OnFail, Scenario, Session, UiError, UiResult};
    pub(crate) use eframe::egui::accesskit::Role;
    pub(crate) use eframe::egui::{pos2, vec2, Pos2};
}
