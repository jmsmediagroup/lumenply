//! Photoshop's layer locks.
//!
//! A lock never changes how a layer looks; it limits what edits may do to
//! it. `lumenply-core`'s editor enforces them on every command, so tools,
//! scripts and the CLI all obey the same rules:
//!
//! * **transparency** — painting, fills and filters keep the layer's alpha
//!   exactly as it was (colour lands only where pixels already are);
//! * **pixels** — the layer's pixels (or its text, or a smart object's
//!   content) can't be edited at all;
//! * **position** — the layer can't be moved, transformed or warped;
//! * **all** — all three, and the layer's opacity, blend mode, effects,
//!   mask and clipping can't change either. It can still be shown, hidden,
//!   renamed, reordered and deleted, as in Photoshop.
//!
//! A group's locks apply to everything inside it.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct LayerLocks {
    pub transparency: bool,
    pub pixels: bool,
    pub position: bool,
    pub all: bool,
}

impl LayerLocks {
    pub const NONE: LayerLocks = LayerLocks {
        transparency: false,
        pixels: false,
        position: false,
        all: false,
    };

    pub fn is_empty(&self) -> bool {
        *self == LayerLocks::NONE
    }

    /// The locks that are in force: `all` implies the other three.
    pub fn effective(&self) -> LayerLocks {
        if self.all {
            LayerLocks {
                transparency: true,
                pixels: true,
                position: true,
                all: true,
            }
        } else {
            *self
        }
    }

    /// Both sets of locks at once (a group's locks over a child's own).
    pub fn union(&self, other: &LayerLocks) -> LayerLocks {
        LayerLocks {
            transparency: self.transparency || other.transparency,
            pixels: self.pixels || other.pixels,
            position: self.position || other.position,
            all: self.all || other.all,
        }
    }

    /// Short description of what is locked, for tooltips and errors:
    /// "all", "pixels", "transparency and position"...
    pub fn describe(&self) -> String {
        if self.all {
            return "all".into();
        }
        let parts: Vec<&str> = [
            (self.transparency, "transparency"),
            (self.pixels, "pixels"),
            (self.position, "position"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect();
        match parts.len() {
            0 => "nothing".into(),
            1 => parts[0].into(),
            _ => format!(
                "{} and {}",
                parts[..parts.len() - 1].join(", "),
                parts[parts.len() - 1]
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_implies_every_lock_and_descriptions_read_well() {
        let all = LayerLocks {
            all: true,
            ..LayerLocks::NONE
        };
        let e = all.effective();
        assert!(e.transparency && e.pixels && e.position);
        assert_eq!(all.describe(), "all");
        let two = LayerLocks {
            transparency: true,
            position: true,
            ..LayerLocks::NONE
        };
        assert_eq!(two.describe(), "transparency and position");
        assert_eq!(two.effective(), two);
        assert!(LayerLocks::default().is_empty());
        let three = two.union(&LayerLocks {
            pixels: true,
            ..LayerLocks::NONE
        });
        assert_eq!(three.describe(), "transparency, pixels and position");
    }
}
