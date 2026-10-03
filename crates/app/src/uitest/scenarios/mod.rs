//! The scenarios, one file per feature area, each a list of functions
//! that drive a [`Session`](super::Session) the way a person would.
//!
//! To add an area: create `<area>.rs` (copy the shape of basics.rs: a
//! `scenario_list!` and the functions), then add a `mod` line and its
//! `SCENARIOS` to [`AREAS`] below. Scenarios within an area only touch
//! that area's file.

use super::Scenario;

mod adjust;
mod b_layers;
mod basics;
mod c_selections;
mod d_painting;
mod e_transform;
mod f_adjust_filters;
mod g_text_shapes;
mod h_workspace;
mod i_ai_actions;
mod selection;

/// Every area's scenarios, in the order `--uitest` runs them.
pub(crate) const AREAS: &[&[Scenario]] = &[
    basics::SCENARIOS,
    adjust::SCENARIOS,
    selection::SCENARIOS,
    c_selections::SCENARIOS,
    b_layers::SCENARIOS,
    d_painting::SCENARIOS,
    e_transform::SCENARIOS,
    f_adjust_filters::SCENARIOS,
    h_workspace::SCENARIOS,
    i_ai_actions::SCENARIOS,
    g_text_shapes::SCENARIOS,
];
