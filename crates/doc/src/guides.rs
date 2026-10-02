//! Ruler guides: straight lines across the whole canvas at a fixed
//! position, used for alignment and snapping. They belong to the document
//! (saved in `.lumen`, round-tripped through PSD) and are covered by undo;
//! they never render into the image.

use serde::{Deserialize, Serialize};

/// Which way a guide runs. A vertical guide sits at an x position, a
/// horizontal one at a y position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Orientation {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Guide {
    pub orientation: Orientation,
    /// Canvas pixels from the left edge (vertical) or top edge
    /// (horizontal). Fractions are allowed: PSD stores 1/32 px.
    pub pos: f32,
}

impl Guide {
    pub fn vertical(x: f32) -> Self {
        Guide {
            orientation: Orientation::Vertical,
            pos: x,
        }
    }

    pub fn horizontal(y: f32) -> Self {
        Guide {
            orientation: Orientation::Horizontal,
            pos: y,
        }
    }

    pub fn is_vertical(&self) -> bool {
        self.orientation == Orientation::Vertical
    }

    /// The same guide after the canvas origin moved by (dx, dy) pixels.
    pub fn shifted(self, dx: f32, dy: f32) -> Self {
        let d = if self.is_vertical() { dx } else { dy };
        Guide {
            pos: self.pos + d,
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guides_shift_along_their_own_axis_only() {
        assert_eq!(Guide::vertical(10.0).shifted(-4.0, 7.0), Guide::vertical(6.0));
        assert_eq!(
            Guide::horizontal(10.0).shifted(-4.0, 7.0),
            Guide::horizontal(17.0)
        );
        assert!(Guide::vertical(0.0).is_vertical());
        assert!(!Guide::horizontal(0.0).is_vertical());
    }
}
