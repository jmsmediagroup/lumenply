//! Saved selections (Photoshop's alpha channels): selections kept under a
//! name in the document, to load back later.

use crate::Mask;

/// A selection kept under a name.
#[derive(Clone, Debug)]
pub struct SavedSelection {
    pub name: String,
    pub mask: Mask,
}

/// `base`, or `base 2`, `base 3`... — the first not already in `taken`.
pub fn unique_name(base: &str, taken: &[SavedSelection]) -> String {
    let used = |n: &str| taken.iter().any(|s| s.name == n);
    if !used(base) {
        return base.to_string();
    }
    (2..)
        .map(|i| format!("{base} {i}"))
        .find(|n| !used(n))
        .expect("unbounded")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_stay_unique() {
        let s = |n: &str| SavedSelection {
            name: n.into(),
            mask: Mask::hide_all(),
        };
        assert_eq!(unique_name("Sky", &[]), "Sky");
        assert_eq!(unique_name("Sky", &[s("Sky")]), "Sky 2");
        assert_eq!(unique_name("Sky", &[s("Sky"), s("Sky 2")]), "Sky 3");
    }
}
