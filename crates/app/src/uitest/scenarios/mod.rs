//! The scenarios, one file per feature area, each a list of functions
//! that drive a [`Session`](super::Session) the way a person would.
//!
//! To add an area: create `<area>.rs` (copy the shape of basics.rs: a
//! `scenario_list!` and the functions), then add a `mod` line and its
//! `SCENARIOS` to [`AREAS`] below. Scenarios within an area only touch
//! that area's file.

use super::Scenario;

mod adjust;
mod basics;
mod d_painting;
mod selection;

/// Every area's scenarios, in the order `--uitest` runs them.
pub(crate) const AREAS: &[&[Scenario]] = &[
    basics::SCENARIOS,
    adjust::SCENARIOS,
    selection::SCENARIOS,
    d_painting::SCENARIOS,
];
