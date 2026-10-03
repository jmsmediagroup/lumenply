//! What a screen reader would see of a frame: the accesskit tree flattened
//! into nodes with names, roles, states and screen rectangles. Scenarios
//! find every control through it, by the name a user reads.

use eframe::egui;
use egui::accesskit::{self, Role};

#[derive(Clone, Debug)]
pub(crate) struct Node {
    /// The accesskit id, which is the egui widget id's value.
    pub id: u64,
    pub role: Role,
    pub name: String,
    /// A text field's text or a control's value text.
    pub value: Option<String>,
    /// A slider's or spin button's number, with its range when known.
    pub numeric: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// In points, as the pointer sees it.
    pub rect: egui::Rect,
    pub disabled: bool,
    pub toggled: Option<bool>,
    pub focused: bool,
}

impl Node {
    pub(crate) fn describe(&self) -> String {
        let mut s = format!(
            "{:?} “{}” at ({:.0}, {:.0}) {:.0}×{:.0}",
            self.role,
            self.name,
            self.rect.min.x,
            self.rect.min.y,
            self.rect.width(),
            self.rect.height()
        );
        if self.disabled {
            s.push_str(" [disabled]");
        }
        if let Some(v) = &self.value {
            s.push_str(&format!(" value={v:?}"));
        }
        if let Some(v) = self.numeric {
            s.push_str(&format!(" = {v}"));
        }
        if let Some(t) = self.toggled {
            s.push_str(if t { " [on]" } else { " [off]" });
        }
        s
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Tree {
    /// Depth-first from the root, so later nodes were drawn later
    /// (popups and dialogs come after the panels beneath them).
    pub nodes: Vec<Node>,
}

/// Names compare the way a person reads them: "…" and "..." alike,
/// surrounding space ignored.
pub(crate) fn normalize(s: &str) -> String {
    s.replace('…', "...").replace('\u{a0}', " ").trim().to_string()
}

impl Tree {
    pub(crate) fn from_update(update: &accesskit::TreeUpdate) -> Tree {
        let map: std::collections::HashMap<accesskit::NodeId, &accesskit::Node> =
            update.nodes.iter().map(|(id, n)| (*id, n)).collect();
        let Some(root) = update.tree.as_ref().map(|t| t.root) else {
            return Tree::default();
        };
        let mut nodes = Vec::with_capacity(map.len());
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            let Some(n) = map.get(&id) else { continue };
            if id != root {
                let b = n.bounds().unwrap_or_default();
                nodes.push(Node {
                    id: id.0,
                    role: n.role(),
                    name: n.name().unwrap_or_default().to_string(),
                    value: n.value().map(str::to_string),
                    numeric: n.numeric_value(),
                    min: n.min_numeric_value(),
                    max: n.max_numeric_value(),
                    rect: egui::Rect::from_min_max(
                        egui::pos2(b.x0 as f32, b.y0 as f32),
                        egui::pos2(b.x1 as f32, b.y1 as f32),
                    ),
                    disabled: n.is_disabled(),
                    toggled: n.toggled().map(|t| t == accesskit::Toggled::True),
                    focused: id == update.focus,
                });
            }
            stack.extend(n.children().iter().rev());
        }
        Tree { nodes }
    }

    /// Every node named `name` (exact match first, then ignoring case),
    /// optionally of one role. A trailing `*` matches any ending, for
    /// names that carry a count or value ("Curve*" for "Curve, 3 points").
    pub(crate) fn matches(&self, name: &str, role: Option<Role>) -> Vec<&Node> {
        let of_role = |n: &&Node| role.is_none_or(|r| n.role == r);
        if let Some(prefix) = name.strip_suffix('*') {
            let want = normalize(prefix);
            return self
                .nodes
                .iter()
                .filter(of_role)
                .filter(|n| normalize(&n.name).starts_with(&want) && !n.name.is_empty())
                .collect();
        }
        let want = normalize(name);
        let exact: Vec<&Node> = self
            .nodes
            .iter()
            .filter(of_role)
            .filter(|n| normalize(&n.name) == want)
            .collect();
        if !exact.is_empty() {
            return exact;
        }
        let lower = want.to_lowercase();
        self.nodes
            .iter()
            .filter(of_role)
            .filter(|n| normalize(&n.name).to_lowercase() == lower)
            .collect()
    }

    /// Names that look like `name`, for an error message.
    pub(crate) fn close_matches(&self, name: &str, role: Option<Role>) -> Vec<String> {
        let want = normalize(name).to_lowercase();
        let mut scored: Vec<(usize, String)> = self
            .nodes
            .iter()
            .filter(|n| !n.name.trim().is_empty())
            .filter(|n| role.is_none_or(|r| n.role == r))
            .map(|n| {
                let have = normalize(&n.name).to_lowercase();
                let d = if have.contains(&want) || want.contains(&have) {
                    0
                } else {
                    edit_distance(&have, &want)
                };
                (d, format!("{:?} “{}”", n.role, n.name))
            })
            .filter(|(d, _)| *d <= (want.chars().count() / 3).max(2))
            .collect();
        scored.sort();
        scored.dedup_by(|a, b| a.1 == b.1);
        scored.into_iter().take(8).map(|(_, s)| s).collect()
    }

    pub(crate) fn by_id(&self, id: u64) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Every visible piece of text a node carries (names and values).
    pub(crate) fn texts(&self) -> impl Iterator<Item = &str> {
        self.nodes
            .iter()
            .flat_map(|n| std::iter::once(n.name.as_str()).chain(n.value.as_deref()))
    }

    pub(crate) fn labels(&self) -> Vec<String> {
        self.nodes
            .iter()
            .filter(|n| n.role == Role::Label && !n.name.trim().is_empty())
            .map(|n| n.name.clone())
            .collect()
    }

    /// A readable listing (`--dump-tree`), one node per line.
    pub(crate) fn dump(&self) -> String {
        let mut out = String::new();
        for n in &self.nodes {
            // Text runs repeat their label's text.
            if (n.name.trim().is_empty() && n.value.is_none()) || n.role == Role::InlineTextBox {
                continue;
            }
            out.push_str(&n.describe());
            out.push('\n');
        }
        out
    }
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_compare_as_people_read_them() {
        assert_eq!(normalize(" Save as… "), "Save as...");
        assert_eq!(edit_distance("layer", "layers"), 1);
        assert_eq!(edit_distance("", "abc"), 3);
        let node = |name: &str| Node {
            id: 1,
            role: Role::Button,
            name: name.into(),
            value: None,
            numeric: None,
            min: None,
            max: None,
            rect: egui::Rect::ZERO,
            disabled: false,
            toggled: None,
            focused: false,
        };
        let tree = Tree {
            nodes: vec![node("Save as..."), node("Curve, 3 points"), node("save")],
        };
        assert_eq!(tree.matches("Save as…", None).len(), 1);
        assert_eq!(tree.matches("Curve*", None)[0].name, "Curve, 3 points");
        assert_eq!(
            tree.matches("SAVE", None)[0].name,
            "save",
            "case only when nothing is exact"
        );
        assert_eq!(tree.matches("Save", None)[0].name, "save");
    }
}
