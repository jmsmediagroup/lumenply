//! Selection tools as Photoshop drives them: the marquee's modifier keys
//! (read at the press to choose how the new shape combines; pressed during
//! the drag, Shift squares the shape and Alt draws it from the centre), the
//! options bar's Feather applied to every new marquee and lasso selection,
//! and the Select ▸ Modify amounts remembered between openings.

use super::*;
use std::cell::RefCell;

/// The modifier keys of a marquee drag, as they stood at its press.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct MarqueeKeys {
    shift_at_press: bool,
    alt_at_press: bool,
    /// Something was selected at the press: a held key then says how the
    /// new shape combines with it, rather than shaping the marquee.
    had_selection: bool,
    /// Whether Shift / Alt shape the marquee now: free at the press, or
    /// let go and pressed again during the drag (Photoshop's rule).
    shift_shapes: bool,
    alt_shapes: bool,
}

impl MarqueeKeys {
    pub(crate) fn press(m: egui::Modifiers, had_selection: bool) -> MarqueeKeys {
        MarqueeKeys {
            shift_at_press: m.shift,
            alt_at_press: m.alt,
            had_selection,
            // With nothing selected there is nothing to add to or take
            // from, so the keys shape the marquee from the start.
            shift_shapes: !m.shift || !had_selection,
            alt_shapes: !m.alt || !had_selection,
        }
    }

    /// How the new shape combines with the selection: Shift adds, Alt
    /// subtracts, both intersect, else the options bar's mode.
    pub(crate) fn op(&self, default: CombineOp) -> CombineOp {
        if !self.had_selection {
            // Subtracting from (or intersecting with) nothing would leave
            // nothing; a first shape is always a new selection.
            return match default {
                CombineOp::Subtract | CombineOp::Intersect => CombineOp::Replace,
                op => op,
            };
        }
        match (self.shift_at_press, self.alt_at_press) {
            (true, true) => CombineOp::Intersect,
            (true, false) => CombineOp::Union,
            (false, true) => CombineOp::Subtract,
            (false, false) => default,
        }
    }

    /// Follow the keys during the drag: (square, from the centre).
    pub(crate) fn track(&mut self, m: egui::Modifiers) -> (bool, bool) {
        if !m.shift {
            self.shift_shapes = true;
        }
        if !m.alt {
            self.alt_shapes = true;
        }
        (m.shift && self.shift_shapes, m.alt && self.alt_shapes)
    }
}

/// The marquee's corners for a drag from `a` to `b` (document pixels): a
/// square of the longer side when `square`, centred on `a` when `centre`.
pub(crate) fn marquee_corners(
    a: (f32, f32),
    b: (f32, f32),
    square: bool,
    centre: bool,
) -> ((f32, f32), (f32, f32)) {
    let (mut dx, mut dy) = (b.0 - a.0, b.1 - a.1);
    if square {
        let side = dx.abs().max(dy.abs());
        dx = side.copysign(dx);
        dy = side.copysign(dy);
    }
    if centre {
        ((a.0 - dx, a.1 - dy), (a.0 + dx, a.1 + dy))
    } else {
        (a, (a.0 + dx, a.1 + dy))
    }
}

fn keys_id() -> egui::Id {
    egui::Id::new("marquee-keys")
}

thread_local! {
    /// The last amount confirmed in each Select ▸ Modify dialog.
    static MODIFY_AMOUNTS: RefCell<Vec<(&'static str, f32)>> = const { RefCell::new(Vec::new()) };
}

/// A Select ▸ Modify reshaping with the amount last confirmed for it (the
/// defaults the first time), as Photoshop reopens these dialogs.
pub(crate) fn remembered(op: EdgeOp) -> EdgeOp {
    let saved = MODIFY_AMOUNTS.with(|m| m.borrow().iter().find(|(n, _)| *n == op.name()).map(|e| e.1));
    match (op, saved) {
        (_, None) => op,
        (EdgeOp::Expand(_), Some(v)) => EdgeOp::Expand(v),
        (EdgeOp::Contract(_), Some(v)) => EdgeOp::Contract(v),
        (EdgeOp::Border(_), Some(v)) => EdgeOp::Border(v),
        (EdgeOp::Smooth(_), Some(v)) => EdgeOp::Smooth(v),
        (EdgeOp::Feather(_), Some(v)) => EdgeOp::Feather(v),
    }
}

/// Remember a confirmed Select ▸ Modify amount for the next opening.
pub(crate) fn remember(op: EdgeOp) {
    MODIFY_AMOUNTS.with(|m| {
        let mut m = m.borrow_mut();
        m.retain(|(n, _)| *n != op.name());
        m.push((op.name(), op.amount()));
    });
}

impl App {
    /// A marquee drag began: keep its keys for the drag and the release.
    pub(crate) fn marquee_press(&self, ctx: &egui::Context) {
        let keys = MarqueeKeys::press(ctx.input(|i| i.modifiers), self.editor.doc().selection.is_some());
        ctx.data_mut(|d| d.insert_temp(keys_id(), keys));
    }

    /// The marquee follows the pointer: shaped by the keys held now, then
    /// snapped (document pixels).
    pub(crate) fn marquee_drag(&mut self, ctx: &egui::Context, a: (f32, f32), b: (f32, f32)) {
        let mut keys: MarqueeKeys = ctx.data(|d| d.get_temp(keys_id())).unwrap_or_default();
        let (square, centre) = keys.track(ctx.input(|i| i.modifiers));
        ctx.data_mut(|d| d.insert_temp(keys_id(), keys));
        let (p, q) = marquee_corners(a, b, square, centre);
        self.track_marquee(p, q);
    }

    /// How the released marquee combines with the selection.
    pub(crate) fn marquee_op(&self, ctx: &egui::Context) -> CombineOp {
        match ctx.data(|d| d.get_temp::<MarqueeKeys>(keys_id())) {
            Some(keys) => keys.op(self.select_op),
            None => self.selection_op(ctx),
        }
    }

    /// A new marquee or lasso shape, softened by the options bar's Feather.
    pub(crate) fn feathered(&self, mut shape: Selection) -> Selection {
        if self.feather > 0.0 {
            shape.feather(self.feather);
        }
        shape
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Modifiers as M;

    #[test]
    fn keys_at_the_press_pick_how_the_shape_combines() {
        let press = |m, had| MarqueeKeys::press(m, had).op(CombineOp::Replace);
        assert_eq!(press(M::NONE, true), CombineOp::Replace);
        assert_eq!(press(M::SHIFT, true), CombineOp::Union);
        assert_eq!(press(M::ALT, true), CombineOp::Subtract);
        assert_eq!(press(M::SHIFT | M::ALT, true), CombineOp::Intersect);
        // Nothing selected: the first shape is new, whatever is held.
        assert_eq!(press(M::ALT, false), CombineOp::Replace);
        assert_eq!(
            MarqueeKeys::press(M::NONE, false).op(CombineOp::Subtract),
            CombineOp::Replace
        );
        assert_eq!(
            MarqueeKeys::press(M::NONE, true).op(CombineOp::Subtract),
            CombineOp::Subtract
        );
    }

    #[test]
    fn keys_pressed_during_the_drag_square_and_centre() {
        // Pressed after the start: they shape the marquee.
        let mut k = MarqueeKeys::press(M::NONE, true);
        assert_eq!(k.track(M::NONE), (false, false));
        assert_eq!(k.track(M::SHIFT), (true, false));
        assert_eq!(k.track(M::SHIFT | M::ALT), (true, true));
        // Shift held from the press over a selection adds and does not
        // square, until it is let go and pressed again.
        let mut k = MarqueeKeys::press(M::SHIFT, true);
        assert_eq!(k.track(M::SHIFT), (false, false));
        assert_eq!(k.track(M::NONE), (false, false));
        assert_eq!(k.track(M::SHIFT), (true, false));
        assert_eq!(k.op(CombineOp::Replace), CombineOp::Union, "still adds");
        // With nothing selected, Shift and Alt at the press shape at once.
        let mut k = MarqueeKeys::press(M::SHIFT | M::ALT, false);
        assert_eq!(k.track(M::SHIFT | M::ALT), (true, true));
    }

    #[test]
    fn corners_square_on_the_longer_side_and_centre_on_the_press() {
        let a = (200.0, 150.0);
        assert_eq!(
            marquee_corners(a, (300.0, 190.0), false, false),
            (a, (300.0, 190.0))
        );
        assert_eq!(
            marquee_corners(a, (300.0, 190.0), true, false),
            (a, (300.0, 250.0))
        );
        // Up and to the left keeps its direction.
        assert_eq!(
            marquee_corners(a, (170.0, 100.0), true, false),
            (a, (150.0, 100.0))
        );
        assert_eq!(
            marquee_corners((100.0, 100.0), (150.0, 140.0), false, true),
            ((50.0, 60.0), (150.0, 140.0))
        );
        assert_eq!(
            marquee_corners((100.0, 100.0), (150.0, 140.0), true, true),
            ((50.0, 50.0), (150.0, 150.0))
        );
    }

    /// The app on a 64×64 white document, no autosave while frames run.
    fn small() -> App {
        let mut app = App::launch(&[]);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app.open_in_new_tab(crate::blank(64, 64), None);
        let bg = app.editor.doc().layers()[0].id;
        app.set_active(Some(bg));
        app
    }

    fn ctx() -> egui::Context {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        ctx
    }

    const SCREEN: egui::Vec2 = egui::vec2(1280.0, 800.0);

    /// One frame at `time` seconds with these key presses.
    fn frame_at(app: &mut App, ctx: &egui::Context, time: f64, keys: &[(M, Key)]) {
        let events = keys
            .iter()
            .flat_map(|&(modifiers, key)| {
                [true, false].map(|pressed| egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers,
                })
            })
            .collect();
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, SCREEN)),
            time: Some(time),
            modifiers: keys.first().map_or(M::NONE, |k| k.0),
            events,
            ..Default::default()
        };
        let _ = ctx.run(raw, |ctx| app.frame(ctx));
    }

    fn frame(app: &mut App, ctx: &egui::Context, keys: &[(M, Key)]) {
        frame_at(app, ctx, 0.0, keys);
    }

    fn select_rect(app: &mut App, r: Rect) {
        app.run(&SetSelection {
            selection: Some(Selection::rect(r)),
        });
    }

    fn value(app: &App, x: i32, y: i32) -> f32 {
        app.editor.doc().selection.as_ref().map_or(0.0, |s| s.value(x, y))
    }

    #[test]
    fn a_dialog_reopened_after_another_is_above_its_backdrop() {
        let mut app = small();
        let ctx = ctx();
        select_rect(&mut app, Rect::new(20, 20, 20, 20));
        let centre = (SCREEN / 2.0).to_pos2();
        // Expand, then Contract, then Expand again: the third used to sit
        // below the backdrop, out of the mouse's reach.
        for id in ["sel-expand", "sel-contract", "sel-expand"] {
            app.run_menu_action(id);
            for _ in 0..3 {
                frame(&mut app, &ctx, &[]);
            }
            let top = ctx.memory(|m| m.layer_id_at(centre));
            assert!(
                top.is_some_and(|l| l != dialogs::backdrop_layer()),
                "{id}: {top:?}"
            );
            frame(&mut app, &ctx, &[(M::NONE, Key::Escape)]);
            assert!(app.dialog.is_none());
        }
    }

    #[test]
    fn select_modify_feather_opens_a_dialog_with_shift_f6() {
        let mut app = small();
        let ctx = ctx();
        assert_eq!(app.action_block("feather"), Some("Make a selection first"));
        select_rect(&mut app, Rect::new(16, 16, 32, 32));
        let steps = app.editor.history().len();
        frame(&mut app, &ctx, &[(M::SHIFT, Key::F6)]);
        assert!(matches!(
            app.dialog,
            Some(Dialog::SelectEdge(EdgeOp::Feather(_), _))
        ));
        frame(&mut app, &ctx, &[]);
        // Previewed at 2 px as one step; Esc takes it back.
        assert_eq!(
            app.editor.history().last().copied(),
            Some("Feather selection 2 px")
        );
        assert!(value(&app, 16, 30) < 1.0 && value(&app, 15, 30) > 0.0);
        frame(&mut app, &ctx, &[(M::NONE, Key::Escape)]);
        assert!(app.dialog.is_none());
        assert_eq!(app.editor.history().len(), steps);
        assert_eq!((value(&app, 15, 30), value(&app, 16, 30)), (0.0, 1.0));
        // Enter keeps it, and the radius comes back next time.
        app.run_menu_action("feather");
        frame(&mut app, &ctx, &[]);
        frame(&mut app, &ctx, &[(M::NONE, Key::Enter)]);
        assert_eq!(app.editor.history().len(), steps + 1);
        assert_eq!(remembered(EdgeOp::Feather(9.0)), EdgeOp::Feather(2.0));
    }

    #[test]
    fn the_options_bar_feather_softens_new_shapes_only() {
        let mut app = small();
        assert_eq!(app.feather, 0.0, "Photoshop's default: hard edges");
        let r = Rect::new(16, 16, 32, 32);
        let hard = app.feathered(Selection::rect(r));
        assert_eq!((hard.value(15, 30), hard.value(16, 30)), (0.0, 1.0));
        app.feather = 4.0;
        let soft = app.feathered(Selection::rect(r));
        let edge = (soft.value(15, 30) + soft.value(16, 30)) / 2.0;
        assert!((edge - 0.5).abs() < 0.05, "{edge}");
        assert_eq!(soft.value(32, 32), 1.0);
        assert_eq!(soft.value(2, 30), 0.0);
    }

    #[test]
    fn esc_drops_an_unfinished_polygon_and_keeps_the_selection() {
        let mut app = small();
        let ctx = ctx();
        select_rect(&mut app, Rect::new(4, 4, 10, 10));
        app.tool = Tool::PolyLasso;
        app.lasso = vec![(20.0, 20.0), (40.0, 20.0)];
        frame(&mut app, &ctx, &[(M::NONE, Key::Escape)]);
        assert!(app.lasso.is_empty());
        assert_eq!(value(&app, 8, 8), 1.0, "the selection stays");
        assert_eq!(app.status, "Polygon cancelled");
        // With no polygon open, Esc still drops the selection.
        frame(&mut app, &ctx, &[(M::NONE, Key::Escape)]);
        assert!(app.editor.doc().selection.is_none());
    }

    #[test]
    fn layer_via_copy_makes_the_copy_active() {
        let mut app = small();
        select_rect(&mut app, Rect::new(8, 8, 16, 16));
        app.run_menu_action("layer-via-copy");
        let active = app.active_layer().map(|l| l.name.clone());
        assert_eq!(active.as_deref(), Some("Background copy"));
        assert_eq!(app.editor.doc().layers().len(), 2);
    }

    #[test]
    fn a_status_message_lasts_until_the_next_edit_or_ten_seconds() {
        let mut app = small();
        let ctx = ctx();
        let shown = |app: &App, ctx: &egui::Context, t: f64| {
            let mut out = String::new();
            let raw = egui::RawInput {
                time: Some(t),
                ..Default::default()
            };
            let _ = ctx.run(raw, |ctx| out = app.status_message(ctx));
            out
        };
        app.status = "Picked #FF0000".into();
        assert_eq!(shown(&app, &ctx, 1.0), "Picked #FF0000");
        assert_eq!(shown(&app, &ctx, 10.5), "Picked #FF0000");
        assert_eq!(shown(&app, &ctx, 11.5), "", "ten seconds later");
        app.status = "Opened shapes.png".into();
        assert_eq!(shown(&app, &ctx, 12.0), "Opened shapes.png");
        select_rect(&mut app, Rect::new(0, 0, 8, 8));
        assert_eq!(shown(&app, &ctx, 12.1), "", "an edit moves on from it");
        app.undo();
        assert!(shown(&app, &ctx, 12.2).starts_with("Undid"), "undo says so");
    }

    #[test]
    fn modify_dialogs_reopen_with_the_last_amount() {
        assert_eq!(
            remembered(EdgeOp::Expand(4.0)),
            EdgeOp::Expand(4.0),
            "the default first"
        );
        remember(EdgeOp::Expand(10.0));
        remember(EdgeOp::Feather(24.0));
        assert_eq!(remembered(EdgeOp::Expand(4.0)), EdgeOp::Expand(10.0));
        assert_eq!(remembered(EdgeOp::Feather(2.0)), EdgeOp::Feather(24.0));
        assert_eq!(
            remembered(EdgeOp::Contract(4.0)),
            EdgeOp::Contract(4.0),
            "each its own"
        );
        remember(EdgeOp::Expand(7.0));
        assert_eq!(remembered(EdgeOp::Expand(4.0)), EdgeOp::Expand(7.0));
    }
}
