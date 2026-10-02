//! App-level tests for Select > Modify, Grow/Similar and the Fill /
//! Content-Aware Fill dialog: what the menu actions and dialogs put on the
//! history, driven through real frames on a tiny document.

use super::*;

/// The app on a 64×64 white document with no autosave while frames run.
fn small() -> App {
    let mut app = App::launch(&[]);
    app.dialog = None;
    app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
    app.open_in_new_tab(blank(64, 64), None);
    let bg = app.editor.doc().layers()[0].id;
    app.set_active(Some(bg));
    app
}

fn frame(app: &mut App, ctx: &egui::Context, keys: &[Key]) {
    let events = keys
        .iter()
        .flat_map(|&key| {
            [true, false].map(|pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
        })
        .collect();
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 800.0))),
        events,
        ..Default::default()
    };
    let _ = ctx.run(raw, |ctx| app.frame(ctx));
}

fn ctx() -> egui::Context {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    ctx
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
fn modify_dialogs_preview_one_step_and_cancel_restores() {
    let mut app = small();
    let ctx = ctx();
    select_rect(&mut app, Rect::new(20, 20, 10, 10));
    let steps = app.editor.history().len();
    assert_eq!(app.action_block("sel-expand"), None);

    app.run_menu_action("sel-expand");
    frame(&mut app, &ctx, &[]);
    frame(&mut app, &ctx, &[]);
    // Previewed once (4 px by default), as one history step.
    assert_eq!(app.editor.history().len(), steps + 1);
    assert_eq!(
        app.editor.history().last().copied(),
        Some("Expand selection 4 px")
    );
    assert_eq!((value(&app, 16, 25), value(&app, 15, 25)), (1.0, 0.0));
    // Esc withdraws the preview.
    frame(&mut app, &ctx, &[Key::Escape]);
    assert!(app.dialog.is_none());
    assert_eq!(app.editor.history().len(), steps);
    assert_eq!((value(&app, 19, 25), value(&app, 20, 25)), (0.0, 1.0));

    // Enter keeps it: contract by 4 leaves x 24..26.
    app.run_menu_action("sel-contract");
    frame(&mut app, &ctx, &[]);
    frame(&mut app, &ctx, &[Key::Enter]);
    assert!(app.dialog.is_none());
    assert_eq!(app.editor.history().len(), steps + 1);
    assert_eq!((value(&app, 23, 25), value(&app, 24, 25)), (0.0, 1.0));
    assert_eq!((value(&app, 25, 25), value(&app, 26, 25)), (1.0, 0.0));
}

#[test]
fn grow_and_similar_need_a_selection_and_add_matching_pixels() {
    let mut app = small();
    assert_eq!(app.action_block("sel-grow"), Some("Make a selection first"));
    assert_eq!(
        app.action_block("content-aware"),
        Some("Select the area to fill first")
    );
    select_rect(&mut app, Rect::new(10, 10, 4, 4));
    // The whole page is one white: Grow takes all of it.
    app.run_menu_action("sel-grow");
    assert_eq!(app.editor.history().last().copied(), Some("Grow selection"));
    assert_eq!((value(&app, 0, 0), value(&app, 63, 63)), (1.0, 1.0));
    app.undo();
    app.run_menu_action("sel-similar");
    assert_eq!(app.editor.history().last().copied(), Some("Select similar"));
    assert_eq!(value(&app, 40, 50), 1.0);
}

#[test]
fn content_aware_fill_dialog_shows_busy_then_fills_in_one_step() {
    let mut app = small();
    let ctx = ctx();
    let bg = app.active.unwrap();
    // A dark square on the white page, selected with a 2 px margin.
    select_rect(&mut app, Rect::new(30, 30, 6, 6));
    app.run(&Fill {
        layer: bg,
        color: [0.1, 0.1, 0.1, 1.0],
    });
    select_rect(&mut app, Rect::new(28, 28, 10, 10));
    let steps = app.editor.history().len();
    app.run_menu_action("content-aware");
    assert!(matches!(app.dialog, Some(Dialog::Fill(true, m, 0)) if m == 64.0));
    frame(&mut app, &ctx, &[]);
    frame(&mut app, &ctx, &[Key::Enter]);
    // The busy frame: still open, nothing run yet.
    assert!(matches!(app.dialog, Some(Dialog::Fill(true, _, 1))));
    assert_eq!(app.editor.history().len(), steps);
    frame(&mut app, &ctx, &[]);
    assert!(matches!(app.dialog, Some(Dialog::Fill(true, _, 2))));
    frame(&mut app, &ctx, &[]);
    assert!(app.dialog.is_none());
    assert_eq!(app.editor.history().len(), steps + 1);
    assert_eq!(app.editor.history().last().copied(), Some("Content-Aware Fill"));
    let px = app
        .editor
        .doc()
        .layer(bg)
        .unwrap()
        .pixels()
        .unwrap()
        .get_pixel(33, 33);
    assert!(px.r > 0.99 && px.a > 0.99, "the square is gone: {px:?}");

    // Brush colour through the same dialog: one plain Fill step.
    app.run_menu_action("fill-dialog");
    if let Some(Dialog::Fill(aware, _, _)) = app.dialog.as_mut() {
        *aware = false;
    }
    frame(&mut app, &ctx, &[]);
    frame(&mut app, &ctx, &[Key::Enter]);
    assert!(app.dialog.is_none());
    assert_eq!(app.editor.history().last().copied(), Some("Fill"));
}

/// Writes before/after crops of Content-Aware Fill removing the lake from
/// the demo photo, for judging by eye:
/// `LUMENPLY_FILL_OUT=/some/dir cargo test --release -p lumenply-app demo_lake -- --ignored`
#[test]
#[ignore = "writes PNGs for visual review"]
fn demo_lake_fill_crops() {
    let Ok(dir) = std::env::var("LUMENPLY_FILL_OUT") else {
        return;
    };
    let mut app = App::launch(&["--demo".to_string()]);
    app.dialog = None;
    app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
    let mut bg = None;
    app.editor.doc().for_each_layer(|l| {
        if l.name == "Background" {
            bg = Some(l.id);
        }
    });
    let bg = bg.expect("demo has a Background");
    let save = |app: &App, name: &str, crop: Rect| {
        let r = app
            .editor
            .doc()
            .layer(bg)
            .unwrap()
            .pixels()
            .unwrap()
            .to_raster(crop);
        lumenply_io::save_png(format!("{dir}/{name}.png"), &r).unwrap();
    };
    // The lake (smooth slopes around it) and a patch of the snowy ridge
    // (busy texture).
    for (name, hole, crop) in [
        (
            "lake",
            Rect::new(455, 850, 290, 170),
            Rect::new(380, 760, 460, 340),
        ),
        (
            "ridge",
            Rect::new(760, 590, 180, 110),
            Rect::new(640, 500, 420, 300),
        ),
    ] {
        save(&app, &format!("{name}-before"), crop);
        app.run(&SetSelection {
            selection: Some(Selection::ellipse(hole)),
        });
        let t = std::time::Instant::now();
        app.run(&ContentAwareFill { layer: bg, margin: 0 });
        println!("demo {name} fill: {:?}", t.elapsed());
        save(&app, &format!("{name}-after"), crop);
    }
}
