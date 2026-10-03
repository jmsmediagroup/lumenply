# D. Painting and retouching: user-journey results

Tested on 2026-10-03 with the `uitest` harness, real mouse and keyboard
input only (tools from the rail and their keys, options-bar controls,
menus and dialogs by their visible names, strokes with `canvas_drag` /
`canvas_stroke` in document pixels), at 1440×900 and at 900×600.
Scenarios: `crates/app/src/uitest/scenarios/d_painting.rs` (14 journeys).
Each journey starts from File ▸ New or from a small PNG drawn by the
scenario and opened with File ▸ Open, so every pixel is known; checks are
composite or layer pixels at document points with explicit values
(colour, alpha, what was left alone) and undo.

Recordings (each folder has `session.mp4`, `session.json`, `keyframes/`):

- before: `<recordings>/d_painting/before/{1440x900,900x600}/<scenario>/`
- after: `<recordings>/d_painting/after/{1440x900,900x600}/<scenario>/`

Behaviour that is wrong but left open as a proposal is logged in the
session as an `OPEN Pn` note (not a failed check), so the rest of the
journey still runs; `session.json` lists them under the step notes.

## Journeys

| Journey | Result | After video (1440×900; same path under `900x600/`) |
| --- | --- | --- |
| `paint-brush-basics`: D, B, Size 20 and Hardness 100 typed in the bar, a stroke (black on the line, 8 px off still black, 12 px off white); a single 50% dab (half black over white, 188 in linear light); a 50% stroke that crosses itself; `]` 20→25 px, `[` back, Shift+`[` / Shift+`]` hardness 75/100%; click then Shift-click draws a straight line; X then Alt-click picks the dab's grey with no history step; four undos | pass, P1 open | `after/1440x900/paint-brush-basics/session.mp4` |
| `paint-brush-tips`: Scatter 200% throws dabs outside the 20 px band (and the Brush settings button shows its "dynamics on" dot); Save preset, change size, apply the preset from the Presets menu; Brush settings ▸ Chalk tip, Hardness greyed out and (900×600) its tooltip says why; a chalk stroke; Edit ▸ Define brush tip from a marquee; File ▸ Import brushes… an `.abr` (1 tip, 1 preset), apply its Round 30 preset (30 px, 70%) | fixed (13, 14) | `after/1440x900/paint-brush-tips/session.mp4` |
| `paint-eraser`: E on a red/blue PNG, stroke is alpha 0 on both halves, below untouched, undo; Shift+E Background Eraser erases red under and at the edge of a 40 px brush, keeps the blue inside the brush; Shift+E Magic Eraser click clears all connected red, blue stays; undo | pass, P1 open | `after/1440x900/paint-eraser/session.mp4` |
| `paint-bucket`: foreground #FF0000 typed in the picker, G, click inside a black frame: inside red, frame black, outside white; undo; tolerance 0 keeps a 240-grey patch out; 50% opacity fill is exactly half red over white | pass, P2 open | `after/1440x900/paint-bucket/session.mp4` |
| `paint-gradient`: Shift+G, drag across: 0 → 255, rising steadily, vertical bands; undo; popover ▸ Black, White preset, click the ramp to add a stop, make it red in its picker, Esc: name becomes "(edited)" and the middle paints red; Radial style from the bar; Fill layer mode adds a gradient fill layer in one undo step | fixed (7) | `after/1440x900/paint-gradient/session.mp4` |
| `paint-colours`: hex in the foreground picker, Blue field in the background picker, Swap colours button, X, Default colours button, Eyedropper (I and from the rail) picks #00A050 and #FF0000 with no history step, the picker's own eyedropper samples into the background | pass | `after/1440x900/paint-colours/session.mp4` |
| `paint-clone`: S, no source yet, Alt-click the red square (no history step), paint 140 px away: an exact copy with the square's edge, source untouched; a second stroke; two undos | pass, P6 open | `after/1440x900/paint-clone/session.mp4` |
| `paint-heal`: J Spot click heals a black dot to the grey (±6), undo; Shift+J Healing with an Alt-clicked source; Shift+J Patch (at 1440 the bar shows its how-to) lasso and drag; Shift+J Content-Aware Move moves a blue square 60 px, its hole fills with grey; Shift+J Red Eye click turns the red pupil dark and neutral, grey around untouched | fixed (12) | `after/1440x900/paint-heal/session.mp4` |
| `paint-blur-sharpen-history`: Brush mode ▸ Blur softens a hard edge only along the stroke; Sharpen darkens the dark side again; History mode paints the opened state back exactly (0 and 255 either side); undo | fixed (8) | `after/1440x900/paint-blur-sharpen-history/session.mp4` |
| `paint-toning`: Dodge lightens grey and keeps it neutral, Burn darkens, Desaturate halves the chroma, Saturate raises it, Smudge drags red into grey; one history step each | pass, P1/P3 open | `after/1440x900/paint-toning/session.mp4` |
| `paint-find-tools`: Cmd+K "dodge", "burn", "sponge", "smudge", "blur tool", "history brush" each pick the Brush in that mode; O, Shift+O, Shift+O step Dodge → Burn → Sponge; Y picks History | fixed (9, 10) | `after/1440x900/paint-find-tools/session.mp4` |
| `paint-fill-stroke`: with the brush at 50% Opacity, Alt+Backspace fills a marquee black at full strength, Cmd+Backspace white, Edit ▸ Fill… fills with the foreground colour; Edit ▸ Stroke… 4 px Inside: exactly 4 px, nothing outside or in the middle; undo keeps the fills | fixed (5, 6) | `after/1440x900/paint-fill-stroke/session.mp4` |
| `paint-caf-define`: marquee round a black square, Edit ▸ Content-Aware Fill… fills it with the surrounding grey (±8), outside unchanged, undo; Edit ▸ Define Pattern from a 20×20 corner; Layer ▸ New fill layer ▸ Pattern… picks it and the layer repeats it | pass | `after/1440x900/paint-caf-define/session.mp4` |
| `paint-latency`: an 80 px brush on a 4000×3000 document, strokes of 20, 80 and 240 pointer moves, timed | pass (P4 open, numbers below) | `after/1440x900/paint-latency/session.mp4` |

All 14 pass at both sizes after the fixes, with four OPEN notes (P1 in
`paint-brush-basics` and `paint-toning`, P2 in `paint-bucket`, P6 in
`paint-clone`). Before the fixes, 4 of 14 failed at both sizes
(`paint-gradient`, `paint-find-tools`, `paint-fill-stroke`, and
`paint-heal` at 1440×900), and the Blur/Sharpen/History part of
`paint-blur-sharpen-history` only passed by scrolling the mode list.

## Findings

Severity as in user-journeys.md. "Fix" is the commit on branch
`worktree-agent-a895359cb01e7f39b` (`1eada06`, "Painting fixes from the
D user sessions"); regression tests are in `crates/app/src/paint_ux_tests.rs`
unless noted.

| # | Severity | Finding (step) | Status |
| --- | --- | --- | --- |
| 1 | major | **Opacity is really Flow.** Overlapping dabs inside one stroke compound: a 50% brush stroke paints full black along its length (only a single click gives 50%), with a soft build-up at the start. The same in the Eraser (50% erases fully), Clone Stamp and History Brush. Photoshop's Opacity caps the whole stroke. (`paint-brush-basics`, "Paint a 50% stroke that crosses itself": got [0, 0, 0], expected [188, 188, 188]) | open, proposal 1 |
| 2 | major | **Dodge, Burn and the Sponge go all the way in one stroke.** At the shared 100% strength one Dodge stroke turns a mid grey white, Burn black, Desaturate fully grey, Saturate fully saturated, Smudge drags the colour 70 px at full strength. Same compounding as 1, and the strength is the Brush's Opacity (100%) instead of Photoshop's 50% defaults. (`paint-toning`) | open, proposals 1 and 3; the control's name fixed (11) |
| 3 | major | **Tolerance is measured in linear light** (Paint Bucket, Magic Eraser; Magic Wand in area C). 12% leaves out a near-white 15 levels off white but takes in everything from black to sRGB 97 in the shadows. (`paint-bucket`, "the pale patch…": stayed [240, 240, 240]) | open, proposal 2 (= C's proposal 1) |
| 4 | major | **Long strokes slow to a crawl.** Each frame re-applies the whole stroke to a fresh copy of the document: 3 ms a frame at point 10, 30 ms at 100, 216 ms at 200, about 650 ms from 400 points on (80 px brush, 4000×3000). See "Stroke latency". | open, proposal 4 |
| 5 | major | Alt+Backspace (and Edit ▸ Fill… with the brush colour) filled at the Brush bar's Opacity: with the brush at 50%, a marquee filled half black. Photoshop fills at full strength; Cmd+Backspace already did. (`paint-fill-stroke`) | fixed; `alt_backspace_fills_at_full_strength_whatever_the_brush_opacity` |
| 6 | major | Edit ▸ Fill… with a selection opened on Content-Aware, the same as Edit ▸ Content-Aware Fill…; clicking Fill ran content-aware fill, which on a new empty layer silently did nothing ("took 0.0 s"). Fill… now opens on the foreground colour. (`paint-fill-stroke`) | fixed; `fill_opens_on_the_foreground_colour_and_content_aware_fill_on_content_aware` |
| 7 | major | Gradient tool, Fill layer mode: a long radial drag makes a fill of more than 150% scale; the moment the new layer showed in Properties its Scale slider clamped it to 150%, changing the gradient and adding a phantom "Edit Gradient Fill" step, so one drag took two undos. (`paint-gradient`, "undo removes it": 2 layers left) | fixed; `showing_a_wide_gradient_fill_leaves_it_and_the_history_alone` |
| 8 | minor | The Brush mode menu (900–1400 points wide) was a 200 px scrolling list: Blur, Sharpen and History sat below the fold, and a click where they seemed to be landed on the canvas and painted a dab. It now shows every mode. (`paint-blur-sharpen-history`, before video) | fixed (scenario) |
| 9 | minor | Dodge, Burn, Sponge and Smudge could not be found with Cmd+K (Blur, Sharpen, History could); Photoshop's O (Dodge/Burn/Sponge, Shift+O cycles) and Y (History Brush) did nothing. The Brush's tooltip now names its retouching modes too. (`paint-find-tools`) | fixed; `the_palette_finds_dodge_burn_sponge_and_smudge`, `o_and_y_pick_the_toning_brushes_and_the_history_brush` |
| 10 | minor | "Sat+" and "Sat−" are cryptic; they are Photoshop's Sponge modes Saturate and Desaturate. | fixed; same palette test |
| 11 | minor | The strength slider was called Opacity in every Brush mode. Photoshop: Exposure (Dodge, Burn), Flow (Sponge), Strength (Smudge, Blur, Sharpen). | fixed; `the_strength_control_has_photoshops_name_for_each_brush_mode` |
| 12 | minor | The options bar remembered its fitted width per tool, not per mode: once Spot healing's crowded bar fell to the tight layout, Patch, Move and Red Eye stayed tight at 1440 (no Source/Destination switch, no how-to hint). | fixed; `a_crowded_heal_mode_does_not_keep_the_others_on_the_tight_bar` |
| 13 | minor | Hardness is greyed out for a sampled tip without saying why. Its slider and field now say "A sampled tip has its own edge: pick the Round tip…". | fixed (scenario, 900×600 hover) |
| 14 | minor | On a tight bar the preset button reads "Save"; screen readers heard only "Save". Named "Save brush preset". | fixed (scenario) |
| 15 | minor | "Brush colour" (Edit menu, Fill dialog, Eyedropper hint, the selection bar's Fill) next to "Foreground colour" (the colour well, Stroke). Now "foreground colour" in this area. Left for their owners: Pen / Paths "Fill path with the brush colour" (G), Colour Range's note (C). | fixed (wording) |
| 16 | minor | A click on the canvas that closes a menu (Brush mode, presets, Sample) also painted a dab / filled. It now only closes the menu, as in Photoshop. A drag still paints (egui closes combo menus on clicks only). | fixed; `a_click_that_closes_the_brush_mode_menu_paints_nothing` |
| 17 | minor | Clone Stamp is not Aligned: every stroke restarts at the source point (a second stroke 60 px lower copies the red square again). Photoshop's default is Aligned. (`paint-clone`, OPEN P6 note) | open, proposal 6 |
| 18 | minor | Healing mode's bar is tight even at 1440 (Spot-sized brush row plus Pick source): "No source yet" is only in Pick source's tooltip; the highlighted Pick source button and the status bar carry it. | open (polish of 12) |
| 19 | minor | Edit ▸ Fill… offers only the foreground colour and Content-Aware: no background colour, pattern, black, white or 50% grey, and no mode or opacity; a defined pattern can only be used through a pattern fill layer. | open, proposal 5 |
| 20 | minor | B after O or Y keeps the Brush in Dodge or History mode; in Photoshop B is always the painting brush. | open, proposal 3 |
| 21 | minor | Save preset names the preset itself ("Hard 20"); Photoshop asks for a name. | open, proposal 7 |
| 22 | polish | The status bar keeps "Clone source set; now paint where the copy should go" after switching to Patch, Move and Red Eye; Red Eye and Patch say nothing on success. Content-Aware Fill on an empty layer reports "took 0.0 s" and changes nothing. | open |
| 23 | polish | "Stop color" in the gradient stop editor beside "Stop colour" everywhere else. | fixed |
| 24 | note | A 50% black dab over white is 188, not Photoshop's 128: Lumenply blends in linear light (ADR 0005). Expected values in the scenarios follow the app's model. | by design |

## Proposals (not done here)

1. **Stroke-level opacity, with a separate Flow** (findings 1, 2). Paint
   each stroke's coverage into a stroke mask (each dab raises it toward
   its own coverage × flow, never past it) and composite the colour once
   at Opacity × mask, as Photoshop does. Opacity then caps the stroke and
   Flow builds up; Dodge, Burn and the Sponge use the mask as their
   per-pixel amount, so one pass at 50% exposure lifts a grey part way.
   This changes how `PaintStroke` and the retouching strokes compute
   pixels, so it needs an ADR and the PSD/corpus checks; the Brush
   settings' existing "Flow jitter" then has a Flow to jitter.
2. **Tolerance on gamma-encoded levels** (finding 3). Compare sRGB-encoded
   values (0–255, Photoshop's default 32) in `flood_region`, as
   adjustments already do (ADR 0005). Shared with area C's proposal 1
   (Magic Wand, Grow, Similar).
3. **Per-mode strength with Photoshop's defaults** (findings 2, 20).
   Remember Exposure / Flow / Strength per Brush mode (Dodge and Burn
   50%, Sponge 50%, Smudge, Blur, Sharpen 50%) instead of sharing the
   painting Opacity, and let B return the Brush to Paint.
4. **Incremental stroke preview** (finding 4). Keep the previewed
   document between frames and apply only the dabs whose positions can
   no longer change (Catmull-Rom smoothing moves at most the last two
   segments), re-applying from a checkpoint; or paint the stroke into its
   own layer-sized buffer and composite it over the cached layer. Frame
   time then stays near the 3 ms of a short stroke. The committed pixels
   don't change, but the preview path does: worth an ADR and a test that
   the preview equals the commit.
5. **A full Fill dialog** (finding 19): Contents (foreground, background,
   colour…, content-aware, pattern with the pattern picker, history,
   black, 50% grey, white), Mode and Opacity, and Preserve transparency.
6. **Aligned for the Clone Stamp and Healing Brush** (finding 17): an
   Aligned checkbox, on by default, keeping the first stroke's offset
   until the next Alt-click.
7. **Name a preset when saving it** (finding 21): a small name field with
   the generated name preselected.

Suggested ROADMAP lines:

- [ ] Brush: stroke-level Opacity plus Flow; Dodge/Burn/Sponge amounts from the stroke mask (ADR)
- [ ] Paint Bucket / Magic Eraser / Magic Wand tolerance on sRGB levels, 0–255 (ADR; with area C)
- [ ] Incremental brush preview: frame time independent of stroke length
- [ ] Per-mode Exposure/Flow/Strength with Photoshop defaults; B returns to Paint
- [ ] Fill dialog: background, pattern, black/white/50% grey, mode, opacity
- [ ] Clone Stamp / Healing Brush: Aligned option

## Stroke latency

**In-app, per frame** (`stroke_latency_bench`, ignored test in
`paint_ux_tests.rs`: the canvas's own preview path — clone the document,
apply the stroke so far, composite the newest part — an 80 px brush on a
blank 4000×3000 document, a point every 12 px; mean of five frames; this
machine was shared with nine other test runs):

| Stroke so far | Preview frame |
| --- | --- |
| 10 points | 3.0 ms |
| 50 points | 16.4 ms |
| 100 points | 30.2 ms |
| 200 points | 216 ms |
| 400 points | 674 ms |
| 640 points | 631 ms |
| commit of the 640-point stroke | 107 ms |

The cost grows with the stroke because every frame repaints every dab of
it: a short stroke feels instant, a two-second scribble on a large image
drops to one or two frames a second while the pointer is still moving.
The commit itself is quick.

**As a user session** (`paint-latency`; wall time per frame including the
harness, the offscreen render and, when recording, video encoding;
strokes of 20, 80 and 240 pointer moves, about 14 px apart):

| Run | 20 moves | 80 moves | 240 moves |
| --- | --- | --- | --- |
| before, 1440×900, recorded | 78 ms | 96 ms | 95 ms |
| before, 900×600, recorded | 91 ms | 131 ms | 125 ms |
| without recording, 1440×900 | 49 ms | 73 ms | 52 ms |
| after, 1440×900, recorded | 81 ms | 92 ms | 144 ms |
| after, 900×600, recorded | 64 ms | 74 ms | 48 ms |

(The fixes don't touch the stroke path; the spread between runs is the
shared machine.)

The harness's per-frame numbers include settle frames after the release
(cheap) and the machine's load, so the bench above is the one to compare
against; both agree that painting on a 4000×3000 image is far from the
16 ms a frame a brush needs.

How painting feels otherwise: the brush cursor is a circle at the brush's
size (an outline of the tip for sampled and elliptical tips), the stroke
previews live under the pointer, and each stroke, fill or retouch is one
history step named after the tool. Shift-click lines, bracket keys and
Alt-click picking behave as in Photoshop.

## Not tested, and why

- **Pen pressure**: needs a tablet; the harness has no pressure source
  (`pen.rs` reads `NSEvent.pressure` / touch force). The two pressure
  toggles were only seen, not exercised.
- **Real camera photos for healing**: the healing journeys use flat
  synthetic flaws with exact expected values; how PatchMatch copes with
  real texture was judged only on the demo photo by eye, not checked.
- **Brush tips at large sizes and `.abr` v6+ files**: the imported set is
  a minimal version-1 file (one computed, one 3×2 sampled brush), as the
  unit tests use.
- **Smudge, Blur and Sharpen strength curves**: checked for direction
  (more or less contrast, colour dragged), not against Photoshop's
  amounts.
