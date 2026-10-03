//! Regression tests for the painting and retouching fixes from the D-area
//! user sessions (docs/testing/results/d_painting.md): Fill strength, the
//! Fill dialog's contents, the brush-mode keys and palette entries, the
//! options bar's per-mode layout and the gradient fill scale.

use super::*;
use lumenply_doc::{Fill, GradientStyle};

/// The app on a `w` × `h` white document, no autosave while frames run.
fn app(w: u32, h: u32) -> App {
    let mut app = App::launch(&[]);
    app.dialog = None;
    app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
    app.open_in_new_tab(blank(w, h), None);
    let bg = app.editor.doc().layers()[0].id;
    app.set_active(Some(bg));
    app
}

fn ctx() -> egui::Context {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    ctx
}

/// One frame at `w` × `h` points; returns every piece of text it painted.
fn frame(app: &mut App, ctx: &egui::Context, size: (f32, f32), events: Vec<egui::Event>) -> Vec<String> {
    // Held modifiers live on the input as well as on each key event.
    let modifiers = events
        .iter()
        .find_map(|e| match e {
            egui::Event::Key { modifiers, .. } => Some(*modifiers),
            _ => None,
        })
        .unwrap_or_default();
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(size.0, size.1))),
        modifiers,
        events,
        ..Default::default()
    };
    let out = ctx.run(raw, |ctx| app.frame(ctx));
    fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut texts = Vec::new();
    for s in &out.shapes {
        walk(&s.shape, &mut texts);
    }
    texts
}

fn key(key: Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
    [true, false]
        .map(|pressed| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        })
        .to_vec()
}

#[test]
fn alt_backspace_fills_at_full_strength_whatever_the_brush_opacity() {
    let mut app = app(32, 32);
    let layer = app.active.unwrap();
    app.brush_rgb = [1.0, 0.0, 0.0];
    // The Brush bar's Opacity at 50%.
    app.brush.color[3] = 0.5;
    app.run(&SetSelection {
        selection: Some(Selection::rect(Rect::new(4, 4, 8, 8))),
    });
    app.run_menu_action("fill");
    let px = app
        .editor
        .doc()
        .layer(layer)
        .unwrap()
        .pixels()
        .unwrap()
        .get_pixel(8, 8);
    assert_eq!((px.r, px.g, px.b, px.a), (1.0, 0.0, 0.0, 1.0));
    // Outside the selection the white stays.
    let out = app
        .editor
        .doc()
        .layer(layer)
        .unwrap()
        .pixels()
        .unwrap()
        .get_pixel(20, 20);
    assert_eq!((out.r, out.g, out.b, out.a), (1.0, 1.0, 1.0, 1.0));
    // The brush keeps its own opacity.
    assert_eq!(app.brush.color[3], 0.5);
}

#[test]
fn fill_opens_on_the_foreground_colour_and_content_aware_fill_on_content_aware() {
    let mut app = app(32, 32);
    app.run(&SetSelection {
        selection: Some(Selection::rect(Rect::new(4, 4, 8, 8))),
    });
    app.run_menu_action("fill-dialog");
    assert!(matches!(app.dialog, Some(Dialog::Fill(false, _, 0))));
    app.dialog = None;
    app.run_menu_action("content-aware");
    assert!(matches!(app.dialog, Some(Dialog::Fill(true, _, 0))));
    app.dialog = None;
    // Without a selection there is nothing for content-aware to fill.
    app.run(&SetSelection { selection: None });
    app.run_menu_action("fill-dialog");
    assert!(matches!(app.dialog, Some(Dialog::Fill(false, _, 0))));
}

#[test]
fn o_and_y_pick_the_toning_brushes_and_the_history_brush() {
    let mut app = app(32, 32);
    let ctx = ctx();
    let size = (1280.0, 800.0);
    app.tool = Tool::Move;
    frame(&mut app, &ctx, size, key(Key::O, egui::Modifiers::NONE));
    assert_eq!((app.tool, app.brush.mode), (Tool::Brush, BrushMode::Dodge));
    // Shift+O steps Dodge → Burn → Sponge (desaturate) → Dodge.
    for want in [BrushMode::Burn, BrushMode::Desaturate, BrushMode::Dodge] {
        frame(&mut app, &ctx, size, key(Key::O, egui::Modifiers::SHIFT));
        assert_eq!(app.brush.mode, want);
    }
    // O again keeps the toning mode in use.
    frame(&mut app, &ctx, size, key(Key::O, egui::Modifiers::SHIFT));
    frame(&mut app, &ctx, size, key(Key::O, egui::Modifiers::NONE));
    assert_eq!(app.brush.mode, BrushMode::Burn);
    frame(&mut app, &ctx, size, key(Key::Y, egui::Modifiers::NONE));
    assert_eq!((app.tool, app.brush.mode), (Tool::Brush, BrushMode::History));
    // From painting, O starts on Dodge.
    app.brush.mode = BrushMode::Paint;
    frame(&mut app, &ctx, size, key(Key::O, egui::Modifiers::SHIFT));
    assert_eq!(app.brush.mode, BrushMode::Dodge);
}

#[test]
fn the_palette_finds_dodge_burn_sponge_and_smudge() {
    let mut app = app(32, 32);
    for (id, mode) in [
        ("tool-dodge", BrushMode::Dodge),
        ("tool-burn", BrushMode::Burn),
        ("tool-sponge", BrushMode::Desaturate),
        ("tool-smudge", BrushMode::Smudge),
    ] {
        app.tool = Tool::Move;
        app.run_menu_action(id);
        assert_eq!((app.tool, app.brush.mode), (Tool::Brush, mode), "{id}");
    }
    let ctx = ctx();
    // Shortcut labels need the fonts of a first frame.
    frame(&mut app, &ctx, (1280.0, 800.0), vec![]);
    assert_eq!(app.action_keys(&ctx, "tool-dodge"), "O");
    assert_eq!(app.action_keys(&ctx, "tool-history-brush"), "Y");
    // The Sponge's two modes are named as Photoshop names them.
    let labels: Vec<&str> = crate::options_bar::BRUSH_MODES.iter().map(|(_, l)| *l).collect();
    assert_eq!(
        labels,
        [
            "Paint",
            "Dodge",
            "Burn",
            "Smudge",
            "Saturate",
            "Desaturate",
            "Blur",
            "Sharpen",
            "History"
        ]
    );
}

#[test]
fn a_crowded_heal_mode_does_not_keep_the_others_on_the_tight_bar() {
    let mut app = app(64, 64);
    let ctx = ctx();
    let size = (1440.0, 900.0);
    let hint = "Draw around the flaw, then drag it onto clean texture";
    // Spot healing has the fullest bar of the Heal modes: it folds to
    // the tight bar even at 1440 points.
    app.tool = Tool::Heal;
    app.retouch.heal_mode = crate::retouch_ui::HealMode::Spot;
    let mut texts = vec![];
    for _ in 0..4 {
        texts = frame(&mut app, &ctx, size, vec![]);
    }
    assert!(
        texts.iter().any(|t| t == "Options"),
        "Spot folds its options: {texts:?}"
    );
    // Patch has three controls: its own measure gives it the wide bar,
    // with the Source / Destination switch and the how-to hint.
    app.retouch.heal_mode = crate::retouch_ui::HealMode::Patch;
    for _ in 0..4 {
        texts = frame(&mut app, &ctx, size, vec![]);
    }
    assert!(texts.iter().any(|t| t == hint), "Patch shows its hint: {texts:?}");
    assert!(texts.iter().any(|t| t == "Destination") && texts.iter().any(|t| t == "Source"));
    // And back: Spot is tight again, Patch wide again.
    app.retouch.heal_mode = crate::retouch_ui::HealMode::Spot;
    for _ in 0..2 {
        texts = frame(&mut app, &ctx, size, vec![]);
    }
    assert!(!texts.iter().any(|t| t == hint));
    app.retouch.heal_mode = crate::retouch_ui::HealMode::Patch;
    texts = frame(&mut app, &ctx, size, vec![]);
    assert!(texts.iter().any(|t| t == hint));
}

#[test]
fn showing_a_wide_gradient_fill_leaves_it_and_the_history_alone() {
    let mut app = app(400, 100);
    let ctx = ctx();
    // A long radial drag in fill-layer mode: 200% of the canvas span.
    let fill = Fill::Gradient {
        gradient: lumenply_doc::Gradient::two([0.0; 3], [1.0; 3]),
        style: GradientStyle::Radial,
        angle: 0.0,
        scale: 2.0,
        reverse: false,
        offset: [0.0, 0.0],
    };
    let id = app.editor.doc().next_id();
    app.run(&AddFillLayer::new(fill.clone()));
    app.set_active(Some(id));
    let steps = app.editor.history().len();
    for _ in 0..4 {
        frame(&mut app, &ctx, (1440.0, 900.0), vec![]);
    }
    assert_eq!(app.editor.history().len(), steps, "{:?}", app.editor.history());
    let now = app
        .editor
        .doc()
        .layer(id)
        .and_then(|l| l.fill_layer())
        .map(|f| f.fill.clone());
    assert_eq!(now, Some(fill));
}

/// How long one frame of a Brush stroke takes as the stroke grows, on a
/// 4000 × 3000 document, through the canvas's own path (clone the
/// document, apply the stroke so far, composite the newest part). Not a
/// check: run with
/// `cargo test --release -p lumenply-app stroke_latency -- --ignored --nocapture`.
#[test]
#[ignore = "timings, not a check"]
fn stroke_latency_bench() {
    let mut app = app(4000, 3000);
    let ctx = ctx();
    frame(&mut app, &ctx, (1440.0, 900.0), vec![]);
    let layer = app.active.unwrap();
    app.tool = Tool::Brush;
    app.brush.radius = 40.0;
    app.brush.hardness = 0.7;
    // A zigzag across the canvas, a point every 12 px (a quick hand at
    // 100% zoom and 60 fps).
    let pts: Vec<StrokePoint> = (0..640)
        .map(|i| {
            let t = i as f32 * 12.0;
            let x = 200.0 + t % 3600.0;
            let y = 300.0 + (t / 3600.0).floor() * 600.0 + 150.0 * (t / 90.0).sin();
            StrokePoint::new(x, y, 1.0)
        })
        .collect();
    let mut times = Vec::new();
    for i in 1..=pts.len() {
        let t = std::time::Instant::now();
        let cmd = app.stroke_command(layer, pts[..i].to_vec());
        let mut preview = app.editor.doc().clone();
        cmd.apply(&mut preview).unwrap();
        let from = i.saturating_sub(3);
        let area = stroke_bounds(&app.make_brush(), &pts[from..i], preview.canvas());
        app.preview(&ctx, &preview, Some(area));
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    println!("\nBrush stroke preview, 80 px brush, 4000 × 3000, one frame per new point:");
    for n in [10usize, 50, 100, 200, 400, 640] {
        let w = &times[n.saturating_sub(5)..n];
        println!(
            "  point {n:>3}: {:.1} ms a frame (mean of the 5 frames up to it)",
            w.iter().sum::<f64>() / w.len() as f64
        );
    }
    let t = std::time::Instant::now();
    let cmd = app.stroke_command(layer, pts.clone());
    app.run(cmd.as_ref());
    println!(
        "  commit of the 640-point stroke: {:.0} ms",
        t.elapsed().as_secs_f64() * 1000.0
    );
}

/// One frame of real pointer input at `t` seconds; returns where the
/// control named `name` is, if the frame drew it.
fn pointer_frame(
    app: &mut App,
    ctx: &egui::Context,
    t: f64,
    events: Vec<egui::Event>,
    name: &str,
) -> Option<egui::Rect> {
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        time: Some(t),
        events,
        ..Default::default()
    };
    let out = ctx.run(raw, |ctx| app.frame(ctx));
    let update = out.platform_output.accesskit_update?;
    update.nodes.iter().find_map(|(_, n)| {
        (n.name() == Some(name)).then(|| {
            let b = n.bounds().unwrap_or_default();
            egui::Rect::from_min_max(
                egui::pos2(b.x0 as f32, b.y0 as f32),
                egui::pos2(b.x1 as f32, b.y1 as f32),
            )
        })
    })
}

fn press(at: Pos2, pressed: bool) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(at),
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}

#[test]
fn a_click_that_closes_the_brush_mode_menu_paints_nothing() {
    let mut app = app(200, 200);
    let ctx = ctx();
    ctx.enable_accesskit();
    app.tool = Tool::Brush;
    let mut t = 0.0;
    let mut tick = |app: &mut App, events: Vec<egui::Event>| {
        t += 1.0 / 30.0;
        pointer_frame(app, &ctx, t, events, "Brush mode")
    };
    let mut combo = None;
    for _ in 0..4 {
        combo = tick(&mut app, vec![]).or(combo);
    }
    let combo = combo.expect("a 1280-point bar folds the modes into the Brush mode menu");
    // Open the menu.
    tick(&mut app, press(combo.center(), true));
    tick(&mut app, press(combo.center(), false));
    tick(&mut app, vec![]);
    assert!(ctx.memory(|m| m.any_popup_open()), "the menu is open");
    let steps = app.editor.history().len();
    // Click the canvas: the menu closes, nothing is painted.
    let canvas = egui::pos2(600.0, 450.0);
    tick(&mut app, press(canvas, true));
    tick(&mut app, press(canvas, false));
    tick(&mut app, vec![]);
    assert!(!ctx.memory(|m| m.any_popup_open()), "the click closed the menu");
    assert_eq!(app.editor.history().len(), steps, "{:?}", app.editor.history());
    // The next click paints a dab.
    tick(&mut app, press(canvas, true));
    tick(&mut app, press(canvas, false));
    tick(&mut app, vec![]);
    assert_eq!(app.editor.history().len(), steps + 1);
    assert_eq!(app.editor.history().last().copied(), Some("Paint stroke"));
}

#[test]
fn the_strength_control_has_photoshops_name_for_each_brush_mode() {
    use crate::options_bar::strength_label;
    assert_eq!(strength_label(BrushMode::Paint), "Opacity");
    assert_eq!(strength_label(BrushMode::History), "Opacity");
    assert_eq!(strength_label(BrushMode::Dodge), "Exposure");
    assert_eq!(strength_label(BrushMode::Burn), "Exposure");
    assert_eq!(strength_label(BrushMode::Saturate), "Flow");
    assert_eq!(strength_label(BrushMode::Desaturate), "Flow");
    assert_eq!(strength_label(BrushMode::Smudge), "Strength");
    assert_eq!(strength_label(BrushMode::Blur), "Strength");
    assert_eq!(strength_label(BrushMode::Sharpen), "Strength");
    // In the bar: Dodge mode names it Exposure.
    let mut app = app(64, 64);
    let ctx = ctx();
    app.tool = Tool::Brush;
    app.brush.mode = BrushMode::Dodge;
    let mut texts = vec![];
    for _ in 0..3 {
        texts = frame(&mut app, &ctx, (1440.0, 900.0), vec![]);
    }
    assert!(texts.iter().any(|t| t == "Exposure"), "{texts:?}");
}
