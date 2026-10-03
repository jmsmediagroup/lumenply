//! Object Selection, Select Subject, Remove Background, the first-use
//! download and the job plumbing, end to end through the app with
//! [`FakeAi`] (exact colour rules, so every expected value is exact).

use super::*;
use crate::a11y_tests::{ctx as a11y_ctx, launch, nameless};
use crate::quick_select_tool::WandMode;
use lumenply_tiles::Rgba;
use std::sync::atomic::{AtomicBool, Ordering};

const W: u32 = 300;
const H: u32 = 200;
/// The red block: 80 × 60 at (40, 30).
const RED: (u32, u32, u32, u32) = (40, 30, 80, 60);
/// The blue block: 80 × 60 at (180, 100).
const BLUE: (u32, u32, u32, u32) = (180, 100, 80, 60);
const BLOCK: f32 = 80.0 * 60.0;
const WAIT: Duration = Duration::from_secs(20);

/// White 300 × 200 (wider than a tile) with a red and a blue block.
fn card() -> Raster {
    let mut r = Raster::filled(W, H, Rgba::WHITE);
    for (rect, c) in [
        (RED, Rgba::new(0.8, 0.05, 0.05, 1.0)),
        (BLUE, Rgba::new(0.05, 0.1, 0.8, 1.0)),
    ] {
        for y in rect.1..rect.1 + rect.3 {
            for x in rect.0..rect.0 + rect.2 {
                r.set(x, y, c);
            }
        }
    }
    r
}

/// The app on the card document (no history yet), the Wand in Object
/// Selection mode, and the fake engine.
fn card_app(fake: FakeAi) -> (App, Arc<FakeAi>) {
    let mut doc = Document::new(W, H);
    let id = doc.add_pixel_layer("Card");
    *doc.layer_mut(id).unwrap().pixels_mut().unwrap() = lumenply_tiles::TileStore::from_raster(&card(), 0, 0);
    let mut app = launch(&[]);
    app.open_in_new_tab(Editor::new(doc), None);
    app.set_active(Some(id));
    app.tool = Tool::Wand;
    app.set_wand_mode(WandMode::Object);
    let fake = Arc::new(fake);
    app.ai_use_service(fake.clone());
    (app, fake)
}

fn installed(models: &[ModelKey]) -> FakeAi {
    FakeAi::new(models)
}

/// Selected coverage summed over the canvas (pixels when values are 0/1).
fn selected(app: &App) -> f32 {
    selected_in(app.editor.doc())
}

fn selected_in(doc: &Document) -> f32 {
    let Some(s) = doc.selection.as_ref() else {
        return 0.0;
    };
    let mut sum = 0.0;
    for y in 0..doc.height as i32 {
        for x in 0..doc.width as i32 {
            sum += s.value(x, y);
        }
    }
    sum
}

fn sel_at(app: &App, x: i32, y: i32) -> f32 {
    app.editor.doc().selection.as_ref().map_or(0.0, |s| s.value(x, y))
}

fn steps(app: &App, label: &str) -> usize {
    app.editor.history().iter().filter(|l| **l == label).count()
}

/// Pointer input on a bare canvas widget whose screen points are document
/// pixels: `path[0]` is pressed, the rest are moved through, the last is
/// released.
fn gesture(app: &mut App, path: &[(f32, f32)], mods: egui::Modifiers) {
    let ctx = egui::Context::default();
    let frame = |app: &mut App, events: Vec<egui::Event>| {
        let raw = egui::RawInput {
            events,
            modifiers: mods,
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(400.0, 300.0))),
            ..Default::default()
        };
        let _ = ctx.run(raw, |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::none())
                .show(ctx, |ui| {
                    let resp = ui.allocate_rect(ui.max_rect(), Sense::click_and_drag());
                    app.object_select_input(ctx, &resp, |p: Pos2| (p.x, p.y));
                });
        });
    };
    let at = |(x, y): (f32, f32)| egui::pos2(x, y);
    let button = |p: Pos2, pressed: bool| egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: mods,
    };
    let first = at(path[0]);
    frame(app, vec![egui::Event::PointerMoved(first)]);
    frame(app, vec![button(first, true)]);
    for p in &path[1..] {
        frame(app, vec![egui::Event::PointerMoved(at(*p))]);
    }
    let last = at(*path.last().unwrap());
    frame(app, vec![button(last, false)]);
    frame(app, vec![]);
}

fn click(app: &mut App, x: f32, y: f32, mods: egui::Modifiers) {
    gesture(app, &[(x, y)], mods);
}

const PLAIN: egui::Modifiers = egui::Modifiers::NONE;
const SHIFT: egui::Modifiers = egui::Modifiers::SHIFT;
const ALT: egui::Modifiers = egui::Modifiers::ALT;

#[test]
fn a_click_selects_exactly_what_the_model_returns_and_modifiers_combine() {
    let (mut app, fake) = card_app(installed(&[ModelKey::MobileSam]));
    assert_eq!(app.editor.history().len(), 0);
    click(&mut app, 60.0, 50.0, PLAIN);
    app.ai_wait(WAIT);
    // Pixel for pixel what the model returned for that click on the layer.
    let point = AiPrompt::Point {
        x: 60.0,
        y: 50.0,
        positive: true,
    };
    let want = fake.select(&card(), &[point], &|| ()).unwrap();
    let doc = app.editor.doc();
    let sel = doc.selection.as_ref().expect("a selection");
    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) as usize;
            assert_eq!(sel.value(x as i32, y as i32), want[i], "at {x},{y}");
        }
    }
    assert_eq!(selected(&app), BLOCK, "the red block, 80 × 60");
    assert_eq!(app.editor.history(), vec!["Object selection"], "one undo step");

    click(&mut app, 200.0, 120.0, SHIFT);
    app.ai_wait(WAIT);
    assert_eq!(selected(&app), 2.0 * BLOCK, "Shift adds the blue block");
    assert_eq!(
        (sel_at(&app, 50, 40), sel_at(&app, 200, 120), sel_at(&app, 10, 10)),
        (1.0, 1.0, 0.0)
    );

    click(&mut app, 60.0, 50.0, ALT);
    app.ai_wait(WAIT);
    assert_eq!(selected(&app), BLOCK, "Alt takes the red block out again");
    assert_eq!((sel_at(&app, 50, 40), sel_at(&app, 200, 120)), (0.0, 1.0));
    assert_eq!(steps(&app, "Object selection"), 3, "one undo step per click");

    app.undo();
    assert_eq!(
        selected(&app),
        2.0 * BLOCK,
        "undo restores the previous selection"
    );
    app.undo();
    assert_eq!(selected(&app), BLOCK);
    app.undo();
    assert!(app.editor.doc().selection.is_none(), "back to no selection");
}

#[test]
fn a_plain_click_replaces_and_a_box_selects_the_object_inside_it() {
    let (mut app, _) = card_app(installed(&[ModelKey::MobileSam]));
    app.run(&SetSelection {
        selection: Some(Selection::rect(Rect::new(0, 0, 10, 10))),
    });
    // A drag around the blue block (screen points are document pixels).
    gesture(
        &mut app,
        &[(170.0, 90.0), (200.0, 120.0), (240.0, 150.0), (270.0, 170.0)],
        PLAIN,
    );
    app.ai_wait(WAIT);
    assert_eq!(
        selected(&app),
        BLOCK,
        "exactly the blue block; the old selection is replaced"
    );
    assert_eq!(
        (sel_at(&app, 5, 5), sel_at(&app, 180, 100), sel_at(&app, 259, 159)),
        (0.0, 1.0, 1.0)
    );
    assert_eq!(app.editor.history().last().copied(), Some("Object selection"));
    // A click on the white surround floods everything but the blocks.
    click(&mut app, 10.0, 190.0, PLAIN);
    app.ai_wait(WAIT);
    assert_eq!(selected(&app), (W * H) as f32 - 2.0 * BLOCK);
}

#[test]
fn the_first_click_reports_encoding_and_later_ones_reuse_it() {
    let fake = FakeAi {
        encode_delay: Duration::from_millis(300),
        ..installed(&[ModelKey::MobileSam])
    };
    let (mut app, _) = card_app(fake);
    click(&mut app, 60.0, 50.0, PLAIN);
    let t = Instant::now();
    while app
        .ai
        .running
        .as_ref()
        .is_none_or(|r| r.job.progress.phase() != PHASE_ENCODING)
    {
        assert!(t.elapsed() < WAIT, "never reported encoding");
        std::thread::yield_now();
    }
    let doing = app
        .ai
        .running
        .as_ref()
        .map(|r| r.task.doing(r.job.progress.phase()));
    assert_eq!(doing, Some("Analysing the image (first click only)…"));
    app.ai_wait(WAIT);
    click(&mut app, 200.0, 120.0, SHIFT);
    let phase = app.ai.running.as_ref().map(|r| r.job.progress.phase());
    app.ai_wait(WAIT);
    assert_ne!(
        phase,
        Some(PHASE_ENCODING),
        "the second click doesn't encode again"
    );
    assert_eq!(selected(&app), 2.0 * BLOCK);
}

/// The first click after launch waits for the model to load (21 s on a
/// busy machine the first time, CoreML compiling): the card says so
/// instead of "Selecting the object…", then moves on to analysing.
#[test]
fn the_first_click_says_the_model_is_loading() {
    let fake = FakeAi {
        cold: AtomicBool::new(true),
        load_delay: Duration::from_millis(300),
        encode_delay: Duration::from_millis(300),
        ..installed(&[ModelKey::MobileSam])
    };
    let (mut app, fake) = card_app(fake);
    assert!(!fake.select_ready());
    click(&mut app, 60.0, 50.0, PLAIN);
    let doing = |app: &App| {
        app.ai.running.as_ref().map(|r| {
            (
                r.task.doing(r.job.progress.phase()),
                r.task.doing_short(r.job.progress.phase()),
            )
        })
    };
    let t = Instant::now();
    while app
        .ai
        .running
        .as_ref()
        .is_none_or(|r| r.job.progress.phase() != PHASE_LOADING)
    {
        assert!(t.elapsed() < WAIT, "never reported loading");
        std::thread::yield_now();
    }
    assert_eq!(
        doing(&app),
        Some((
            "Loading the Object Selection model (first click only)…",
            "Loading the model…"
        ))
    );
    while app
        .ai
        .running
        .as_ref()
        .is_some_and(|r| r.job.progress.phase() == PHASE_LOADING)
    {
        assert!(t.elapsed() < WAIT, "stuck loading");
        std::thread::yield_now();
    }
    assert_eq!(
        doing(&app).map(|d| d.0),
        Some("Analysing the image (first click only)…"),
        "then the image is analysed"
    );
    app.ai_wait(WAIT);
    assert!(fake.select_ready(), "loaded once");
    assert_eq!(selected(&app), BLOCK);
    // The next click neither loads nor encodes.
    click(&mut app, 200.0, 120.0, SHIFT);
    let phase = app.ai.running.as_ref().map(|r| r.job.progress.phase());
    app.ai_wait(WAIT);
    assert!(phase.is_none() || phase == Some(PHASE_RUN), "{phase:?}");
}

#[test]
fn remove_background_adds_a_mask_equal_to_the_matte_and_keeps_the_pixels() {
    let (mut app, fake) = card_app(installed(&[ModelKey::BiRefNetLite]));
    let layer = app.active.unwrap();
    let before = app
        .active_layer()
        .unwrap()
        .pixels()
        .unwrap()
        .to_raster(Rect::new(0, 0, W, H));
    assert_eq!(app.action_block(REMOVE_BG), None);
    app.run_menu_action(REMOVE_BG);
    app.ai_wait(WAIT);
    let want = fake.matte(&card(), false).unwrap();
    let l = app.editor.doc().layer(layer).unwrap();
    let mask = l.mask.as_ref().expect("Remove Background adds a mask");
    assert!(mask.enabled);
    let mut sum = 0.0;
    for y in 0..H {
        for x in 0..W {
            let v = mask.value(x as i32, y as i32);
            // Masks rest at 16 bits.
            assert!(
                (v - want[(y * W + x) as usize]).abs() <= 0.5 / 65535.0,
                "at {x},{y}"
            );
            sum += v;
        }
    }
    assert_eq!(sum, 2.0 * BLOCK, "both blocks shown, the white surround hidden");
    assert_eq!(
        (mask.value(50, 40), mask.value(200, 120), mask.value(10, 10)),
        (1.0, 1.0, 0.0)
    );
    let after = l.pixels().unwrap().to_raster(Rect::new(0, 0, W, H));
    assert!(after == before, "non-destructive: the pixels are untouched");
    assert_eq!(app.editor.history(), vec!["Remove background"]);
    app.undo();
    assert!(app.active_layer().unwrap().mask.is_none());
}

#[test]
fn remove_background_needs_a_pixel_layer() {
    let (mut app, _) = card_app(installed(&ModelKey::ALL));
    app.set_active(None);
    assert_eq!(app.action_block(REMOVE_BG), Some("Select a pixel layer first"));
    app.add_adjustment(Adjustment::Invert);
    assert_eq!(app.action_block(REMOVE_BG), Some("Select a pixel layer first"));
    app.run_menu_action(REMOVE_BG);
    assert!(!app.ai.active(), "nothing started");
    assert_eq!(app.status, "Select a pixel layer first");
}

#[test]
fn select_subject_replaces_the_selection_with_the_matte() {
    let (mut app, _) = card_app(installed(&[ModelKey::BiRefNetLite]));
    app.run(&SetSelection {
        selection: Some(Selection::rect(Rect::new(0, 0, 10, 10))),
    });
    app.run_menu_action(SELECT_SUBJECT);
    app.ai_wait(WAIT);
    assert_eq!(selected(&app), 2.0 * BLOCK);
    assert_eq!(
        (sel_at(&app, 5, 5), sel_at(&app, 41, 31), sel_at(&app, 259, 159)),
        (0.0, 1.0, 1.0)
    );
    assert_eq!(app.editor.history(), vec!["Select", "Select subject"]);
    assert!(app.status.starts_with("Select subject ("), "{}", app.status);
}

#[test]
fn first_use_asks_then_downloads_then_runs_the_request() {
    let (mut app, fake) = card_app(installed(&[]));
    click(&mut app, 60.0, 50.0, PLAIN);
    let c = app.ai.consent.as_ref().expect("the first-use dialog");
    assert_eq!(
        (c.model, c.phase.clone()),
        (ModelKey::MobileSam, ConsentPhase::Ask)
    );
    assert!(c.pending.is_some(), "the click waits for the model");
    assert!(
        app.ai.running.is_none() && app.ai.downloads.is_empty(),
        "nothing runs before Download"
    );
    app.ai_wait(WAIT);
    assert!(app.editor.doc().selection.is_none() && app.editor.history().is_empty());

    app.ai_consent_download();
    assert_eq!(app.ai.consent.as_ref().unwrap().phase, ConsentPhase::Downloading);
    app.ai_wait(WAIT);
    assert!(fake.is_installed(ModelKey::MobileSam));
    assert!(app.ai.installed(ModelKey::MobileSam), "the list was reread");
    assert!(app.ai.consent.is_none(), "the dialog closed");
    assert_eq!(selected(&app), BLOCK, "the click ran once the model was in");
    assert_eq!(app.editor.history(), vec!["Object selection"]);
    // Installed now: the next click runs straight away.
    click(&mut app, 200.0, 120.0, SHIFT);
    assert!(app.ai.consent.is_none());
    app.ai_wait(WAIT);
    assert_eq!(selected(&app), 2.0 * BLOCK);
}

#[test]
fn cancelling_a_download_installs_nothing_and_drops_the_request() {
    let fake = FakeAi {
        steps: 2000,
        tick: Duration::from_millis(2),
        ..installed(&[])
    };
    let (mut app, fake) = card_app(fake);
    app.run_menu_action(SELECT_SUBJECT);
    assert_eq!(
        app.ai.consent.as_ref().map(|c| c.model),
        Some(ModelKey::BiRefNetLite)
    );
    app.ai_consent_download();
    let t = Instant::now();
    while app.ai.downloads[0].1.progress.get().0 == 0 {
        assert!(t.elapsed() < WAIT, "no progress");
        std::thread::yield_now();
    }
    app.ai_consent_cancel();
    assert!(app.ai.consent.is_none(), "Cancel closes the dialog at once");
    app.ai_wait(WAIT);
    assert!(!fake.is_installed(ModelKey::BiRefNetLite));
    assert!(!app.ai.installed(ModelKey::BiRefNetLite));
    assert!(app.editor.doc().selection.is_none() && app.editor.history().is_empty());
    assert_eq!(
        app.status,
        "Download of BiRefNet lite cancelled; nothing was installed"
    );
    assert!(!app.ai.active());
}

#[test]
fn an_offline_download_explains_itself_and_try_again_works() {
    let fake = FakeAi {
        offline: AtomicBool::new(true),
        ..installed(&[])
    };
    let (mut app, fake) = card_app(fake);
    click(&mut app, 60.0, 50.0, PLAIN);
    app.ai_consent_download();
    app.ai_wait(WAIT);
    match &app.ai.consent.as_ref().expect("still up").phase {
        ConsentPhase::Failed(why) => assert!(why.contains("offline"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(!fake.is_installed(ModelKey::MobileSam));
    assert!(
        app.status.starts_with("Couldn't download MobileSAM: "),
        "{}",
        app.status
    );
    assert!(app.editor.doc().selection.is_none());
    // Back online: Try again downloads and runs the click that waited.
    fake.offline.store(false, Ordering::Relaxed);
    app.ai_consent_download();
    app.ai_wait(WAIT);
    assert!(app.ai.consent.is_none());
    assert_eq!(selected(&app), BLOCK);
}

#[test]
fn a_result_lands_in_the_document_it_was_asked_for() {
    let fake = FakeAi {
        run_delay: Duration::from_millis(150),
        ..installed(&[ModelKey::BiRefNetLite])
    };
    let (mut app, _) = card_app(fake);
    let card_key = app.doc_key;
    app.open_in_new_tab(blank(64, 64), None);
    let other_steps = app.editor.history().len();
    app.switch_tab(0);
    assert_eq!(app.doc_key, card_key);
    app.run_menu_action(SELECT_SUBJECT);
    // The user moves on to the other tab before the model is done.
    app.switch_tab(1);
    app.ai_wait(WAIT);
    assert!(
        app.editor.doc().selection.is_none(),
        "the tab in front is untouched"
    );
    assert_eq!(app.editor.history().len(), other_steps);
    assert!(app.status.contains("another tab"), "{}", app.status);
    let parked = app.tabs.iter().find(|t| t.doc_key == card_key).unwrap();
    assert_eq!(selected_in(parked.editor.doc()), 2.0 * BLOCK);
    assert_eq!(parked.editor.history(), vec!["Select subject"]);
    app.switch_tab(0);
    assert_eq!(selected(&app), 2.0 * BLOCK, "there when the tab comes back");
    app.undo();
    assert!(app.editor.doc().selection.is_none(), "and undoable there");

    // A closed document's result is dropped, with a message.
    app.run_menu_action(SELECT_SUBJECT);
    app.force_close_tab(0);
    assert_ne!(app.doc_key, card_key);
    app.ai_wait(WAIT);
    assert!(app.editor.doc().selection.is_none());
    assert_eq!(
        app.status,
        "Select Subject finished after its document was closed; the result was dropped"
    );
}

#[test]
fn a_result_waits_while_a_dialog_is_open_and_cancel_drops_it() {
    let fake = FakeAi {
        run_delay: Duration::from_millis(100),
        ..installed(&ModelKey::ALL)
    };
    let (mut app, _) = card_app(fake);
    app.ai_request(AiTask::Subject);
    app.dialog = Some(Dialog::About);
    app.ai_wait(WAIT);
    assert!(app.editor.doc().selection.is_none(), "not under an open dialog");
    assert_eq!(app.ai.finished.len(), 1);
    app.dialog = None;
    app.ai_poll(true);
    assert!(
        app.editor.doc().selection.is_none(),
        "nor under a held mouse button"
    );
    app.ai_poll(false);
    assert_eq!(selected(&app), 2.0 * BLOCK);

    // Cancel: the running job's result and the queued click are dropped.
    let before = app.editor.history().len();
    click(&mut app, 60.0, 50.0, PLAIN);
    click(&mut app, 200.0, 120.0, SHIFT);
    assert!(
        app.ai.running.is_some() && app.ai.queue.len() == 1,
        "one runs, one waits"
    );
    app.ai_cancel_running();
    app.ai_wait(WAIT);
    assert_eq!(app.editor.history().len(), before, "nothing applied");
    assert_eq!(app.status, "Object Selection cancelled");
}

#[test]
fn shift_w_cycles_the_three_wand_modes() {
    let (mut app, _) = card_app(installed(&[]));
    app.set_wand_mode(WandMode::Wand);
    let ctx = egui::Context::default();
    let mut seen = Vec::new();
    for _ in 0..3 {
        let mut raw = egui::RawInput {
            modifiers: SHIFT,
            ..Default::default()
        };
        raw.events.push(egui::Event::Key {
            key: Key::W,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: SHIFT,
        });
        let _ = ctx.run(raw, |ctx| app.shortcuts(ctx));
        seen.push((app.wand_mode(), app.quick.on, app.quick.object));
    }
    assert_eq!(
        seen,
        vec![
            (WandMode::Quick, true, false),
            (WandMode::Object, false, true),
            (WandMode::Wand, false, false)
        ]
    );
    assert_eq!(app.tool, Tool::Wand);
    app.run_menu_action(OBJECT_TOOL);
    assert_eq!(app.wand_mode(), WandMode::Object);
}

#[test]
fn without_the_engine_the_features_say_why() {
    let mut app = launch(&[]);
    app.open_in_new_tab(blank(32, 32), None);
    let why = Some("AI features aren't available in this build");
    assert_eq!(app.action_block(SELECT_SUBJECT), why);
    assert_eq!(app.action_block(REMOVE_BG), why);
    assert_eq!(app.action_block(OBJECT_TOOL), why);
    assert_eq!(app.action_block(AI_MODELS), None, "the model list still opens");
    app.ai_request(AiTask::Subject);
    assert!(app.ai.consent.is_none() && !app.ai.active());
    assert_eq!(
        app.status,
        "Select Subject: This build of Lumenply doesn't include the AI engine"
    );
}

#[test]
fn every_ai_control_has_a_name() {
    let fake = FakeAi {
        steps: 1000,
        tick: Duration::from_millis(20),
        encode_delay: Duration::from_secs(30),
        ..installed(&[ModelKey::BiRefNetLite])
    };
    let (mut app, _) = card_app(fake);
    let ctx = a11y_ctx();
    let mut failures = Vec::new();
    let mut check = |app: &mut App, what: &str| {
        let missing = nameless(app, &ctx);
        if !missing.is_empty() {
            failures.push(format!("{what}: {missing:?}"));
        }
    };
    check(&mut app, "Object Selection bar, model missing");
    app.debug_ai("ai:consent=sam");
    check(&mut app, "first-use dialog");
    app.ai_consent_download();
    check(&mut app, "first-use dialog, downloading");
    app.ai_consent_cancel();
    app.ai_wait(WAIT);
    app.ai.consent = Some(Consent {
        phase: ConsentPhase::Failed("Couldn't reach huggingface.co".into()),
        detail: Some("download failed: io: Connection refused".into()),
        ..Consent::ask(ModelKey::MobileSam, None)
    });
    check(&mut app, "first-use dialog, failed");
    app.ai.consent = None;
    app.run_menu_action(AI_MODELS);
    app.ai_start_download(ModelKey::MobileSam);
    check(&mut app, "Preferences ▸ AI models");
    app.ai_cancel_download(ModelKey::MobileSam);
    app.ai_wait(WAIT);
    app.dialog = None;
    // Object Selection encoding: the bar's spinner and the progress card.
    app.ai_use_service(Arc::new(FakeAi {
        encode_delay: Duration::from_secs(30),
        ..installed(&ModelKey::ALL)
    }));
    app.ai_request(AiTask::Object {
        prompt: AiPrompt::Point {
            x: 60.0,
            y: 50.0,
            positive: true,
        },
        op: CombineOp::Replace,
    });
    let t = Instant::now();
    while app
        .ai
        .running
        .as_ref()
        .is_some_and(|r| r.job.progress.phase() != PHASE_ENCODING)
    {
        assert!(t.elapsed() < WAIT);
        std::thread::yield_now();
    }
    check(&mut app, "encoding: bar and progress card");
    let card = ctx.memory(|m| m.area_rect(egui::Id::new("ai-progress")));
    assert!(card.is_some(), "the progress card is up");
    app.ai_cancel_running();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The real engine's errors carry a URL and an OS error; the dialog says
/// what happened in words and keeps the detail for its tooltip.
#[test]
fn download_errors_read_as_what_to_do() {
    let offline = "download failed: https://huggingface.co/senty-au/BiRefNet_lite-ONNX-dynamic/resolve/\
                   173d635935b93839608b9b9039da8d1d212471e9/onnx/model.onnx: io: Connection refused (os error 61)";
    assert_eq!(
        download_problem(offline),
        "Couldn't reach huggingface.co: the computer seems to be offline, or a firewall or proxy \
         blocks the connection."
    );
    assert_eq!(
        download_problem("model.onnx does not match its pinned SHA-256 (got ab, expected cd)"),
        "The downloaded file was damaged (it doesn't match the published one), so it wasn't installed. \
         Try again."
    );
    assert_eq!(
        download_problem("download failed: model.onnx: got 1000 of 2000 bytes"),
        "The download was cut off before it finished."
    );
    assert_eq!(
        download_problem("No space left on device (os error 28)"),
        "There isn't enough free disk space for the model."
    );
    assert_eq!(download_problem("something else"), "something else");

    // Through the app: the dialog's message and the status bar.
    let fake = FakeAi {
        offline: AtomicBool::new(true),
        ..installed(&[])
    };
    let (mut app, _) = card_app(fake);
    app.run_menu_action(SELECT_SUBJECT);
    app.ai_consent_download();
    app.ai_wait(WAIT);
    let c = app.ai.consent.as_ref().expect("still up");
    assert_eq!(
        c.phase,
        ConsentPhase::Failed(
            "Couldn't reach huggingface.co: the computer seems to be offline, or a firewall or proxy \
             blocks the connection."
                .into()
        )
    );
    assert_eq!(
        c.detail.as_deref(),
        Some("Couldn't reach huggingface.co: the computer seems to be offline")
    );
    assert!(
        app.status
            .starts_with("Couldn't download BiRefNet lite: Couldn't reach huggingface.co: the computer"),
        "{}",
        app.status
    );
}

/// A long download needn't hold the user: "Download in background"
/// closes the dialog, the card shows progress, a later click replaces the
/// one waiting, and it runs once the model is in.
#[test]
fn a_download_can_continue_in_the_background() {
    let fake = FakeAi {
        steps: 200,
        tick: Duration::from_millis(5),
        ..installed(&[])
    };
    let (mut app, fake) = card_app(fake);
    click(&mut app, 60.0, 50.0, PLAIN);
    app.ai_consent_download();
    app.ai.consent.as_mut().unwrap().hidden = true;
    assert!(
        app.ai.downloading(ModelKey::MobileSam).is_some(),
        "still downloading"
    );
    // The user keeps working, and clicks the blue block meanwhile.
    app.run(&AddPixelLayer::new("Meanwhile"));
    click(&mut app, 200.0, 120.0, PLAIN);
    assert!(
        app.status
            .starts_with("Object Selection runs when MobileSAM has downloaded ("),
        "{}",
        app.status
    );
    app.ai_wait(WAIT);
    assert!(fake.is_installed(ModelKey::MobileSam));
    assert!(app.ai.consent.is_none());
    assert_eq!(selected(&app), BLOCK, "the latest click ran: the blue block");
    assert_eq!(sel_at(&app, 200, 120), 1.0);
    assert_eq!(sel_at(&app, 60, 50), 0.0);
    assert_eq!(
        app.editor.history(),
        vec!["Add layer 'Meanwhile'", "Object selection"]
    );

    // A failure in the background brings the dialog back.
    let fake = FakeAi {
        offline: AtomicBool::new(true),
        tick: Duration::from_millis(20),
        ..installed(&[])
    };
    let (mut app, _) = card_app(fake);
    app.run_menu_action(SELECT_SUBJECT);
    app.ai_consent_download();
    app.ai.consent.as_mut().unwrap().hidden = true;
    app.ai_wait(WAIT);
    let c = app.ai.consent.as_ref().expect("back up");
    assert!(!c.hidden && matches!(c.phase, ConsentPhase::Failed(_)));
}

/// Picking Object Selection loads its model ahead of the first click.
#[test]
fn picking_object_selection_loads_the_model_ahead() {
    let fake = FakeAi {
        cold: AtomicBool::new(true),
        load_delay: Duration::from_millis(50),
        ..installed(&[ModelKey::MobileSam])
    };
    let (mut app, fake) = card_app(fake);
    assert!(!fake.select_ready());
    // Drawing the frame draws the Object Selection bar, which asks once.
    let ctx = a11y_ctx();
    let _ = nameless(&mut app, &ctx);
    let t = Instant::now();
    while !fake.select_ready() {
        assert!(t.elapsed() < WAIT, "never loaded");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(app.ai.warm_requested);
    // Not installed: nothing to load.
    let cold = FakeAi {
        cold: AtomicBool::new(true),
        ..installed(&[])
    };
    let (mut app, cold) = card_app(cold);
    app.ai_warm_select();
    assert!(!app.ai.warm_requested && !cold.select_ready());
}

/// Preferences ▸ AI models ▸ High detail reaches the model, survives the
/// dialog's Save, and the status says which detail ran.
#[test]
fn high_detail_reaches_the_model_and_is_kept() {
    let (mut app, fake) = card_app(installed(&ModelKey::ALL));
    assert!(!session::Prefs::default().ai_high_detail, "standard by default");
    // (An earlier run of this test may have saved it in the test folder.)
    app.prefs.ai_high_detail = false;
    app.run_menu_action(SELECT_SUBJECT);
    app.ai_wait(WAIT);
    assert!(!fake.high_detail.load(Ordering::Relaxed));
    assert!(app.status.starts_with("Select subject (") && !app.status.contains("high"));
    app.run_menu_action(AI_MODELS);
    app.prefs.ai_high_detail = true; // what the checkbox sets
                                     // Save in Preferences writes its own copy of the prefs back.
    if let Some(Dialog::Preferences(p, _)) = app.dialog.as_ref() {
        assert!(!p.ai_high_detail, "the dialog's copy predates the change");
    }
    // Enter is the dialog's Save.
    let ctx = a11y_ctx();
    let enter = egui::Event::Key {
        key: Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    for events in [vec![], vec![enter]] {
        let raw = egui::RawInput {
            events,
            ..Default::default()
        };
        let _ = ctx.run(raw, |ctx| app.frame(ctx));
    }
    assert!(app.dialog.is_none(), "saved and closed");
    assert!(app.prefs.ai_high_detail, "Save keeps the AI page's setting");
    app.run_menu_action(SELECT_SUBJECT);
    app.ai_wait(WAIT);
    assert!(
        fake.high_detail.load(Ordering::Relaxed),
        "the model ran at high detail"
    );
    assert!(
        app.status.starts_with("Select subject (high detail, "),
        "{}",
        app.status
    );
}

/// Shown again after another modal dialog, the first-use dialog still
/// takes the mouse (it sits just above the backdrop, not under it).
#[test]
fn the_first_use_dialog_takes_clicks_after_another_dialog() {
    let (mut app, _) = card_app(FakeAi {
        steps: 1000,
        tick: Duration::from_millis(20),
        ..installed(&[])
    });
    let ctx = a11y_ctx();
    let screen = egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(1440.0, 900.0));
    let frame = |app: &mut App, events: Vec<egui::Event>| {
        let raw = egui::RawInput {
            screen_rect: Some(screen),
            events,
            ..Default::default()
        };
        ctx.run(raw, |ctx| app.frame(ctx))
            .platform_output
            .accesskit_update
            .expect("accesskit")
    };
    let rect_of = |update: &egui::accesskit::TreeUpdate, name: &str| {
        update.nodes.iter().find_map(|(_, n)| {
            (n.name() == Some(name)).then(|| n.bounds()).flatten().map(|b| {
                egui::Rect::from_min_max(
                    egui::pos2(b.x0 as f32, b.y0 as f32),
                    egui::pos2(b.x1 as f32, b.y1 as f32),
                )
            })
        })
    };
    app.debug_ai("ai:consent=sam");
    for _ in 0..3 {
        frame(&mut app, vec![]);
    }
    app.ai.consent = None;
    app.run_menu_action(AI_MODELS);
    for _ in 0..3 {
        frame(&mut app, vec![]);
    }
    app.dialog = None;
    frame(&mut app, vec![]);
    app.debug_ai("ai:consent=sam");
    let mut at = None;
    for _ in 0..3 {
        let u = frame(&mut app, vec![]);
        at = rect_of(&u, "Download").or(at);
    }
    let p = at.expect("the Download button is shown").center();
    let button = |pressed| egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    frame(&mut app, vec![egui::Event::PointerMoved(p)]);
    frame(&mut app, vec![button(true)]);
    frame(&mut app, vec![button(false)]);
    assert_eq!(
        app.ai.consent.as_ref().map(|c| c.phase.clone()),
        Some(ConsentPhase::Downloading),
        "the click reached Download"
    );
    app.ai_consent_cancel();
    app.ai_wait(WAIT);
}

#[test]
fn the_first_use_dialog_fits_a_900_by_600_window() {
    let (mut app, _) = card_app(installed(&[]));
    app.debug_ai("ai:consent=birefnet");
    let ctx = a11y_ctx();
    let screen = egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(900.0, 600.0));
    for _ in 0..3 {
        let raw = egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };
        let _ = ctx.run(raw, |ctx| app.frame(ctx));
    }
    let r = ctx
        .memory(|m| m.area_rect(egui::Id::new("ai-consent")))
        .expect("the dialog is up");
    assert!(screen.contains_rect(r), "{r:?}");
    assert!(r.width() <= 460.0, "{r:?}");
}
