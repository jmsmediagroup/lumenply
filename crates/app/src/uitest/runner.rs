//! Running scenarios: `lumenply-app --uitest ...` from a `uitest` build,
//! and the glue that gives each scenario its own `cargo test`.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use eframe::egui;

use super::{scenarios, OnFail, Options, Scenario, Session, UiResult};

const USAGE: &str = "\
lumenply-app --uitest [SCENARIO...] [OPTIONS]

Runs user-session scenarios (all of them when none is named) on the real
UI, headlessly, and records each one: DIR/<scenario>/session.mp4,
session.json, keyframes/*.png (failures/*.png when a step fails).

  --out DIR          where sessions go (default: $TMPDIR/lumenply-uitest)
  --list             list the scenarios and exit
  --size WxH         window size in points (default 1440x900)
  --ppp N            pixels per point (default 1; 2 for a Retina look)
  --fps N            frames per second of virtual time and video (30)
  --every N          record every Nth frame (1)
  --hold SECONDS     how long each step's last frame stays on (0.6)
  --video-scale X    scale the video (1.0)
  --no-video         run without rendering or recording
  --stop-on-fail     end a scenario at its first failed check
  --tree [ARG...]    launch with ARGs (e.g. --demo), print every named
                     control a scenario can reach, and exit
  --still PNG [ARG...]  launch with ARGs, wait until idle, save the
                     rendered window (no pointer or caption) and exit;
                     compare with `lumenply-app ARG... --screenshot PNG`
";

fn parse_size(s: &str) -> Option<egui::Vec2> {
    let (w, h) = s.split_once('x')?;
    Some(egui::vec2(w.parse().ok()?, h.parse().ok()?))
}

/// The scenario registry, flattened.
pub(crate) fn all() -> Vec<Scenario> {
    scenarios::AREAS.iter().flat_map(|a| a.iter().copied()).collect()
}

/// `lumenply-app --uitest ...`; returns the process exit code.
pub(crate) fn main(args: &[String]) -> i32 {
    let mut names: Vec<String> = Vec::new();
    let mut opts = Options::new(std::env::temp_dir().join("lumenply-uitest"));
    let mut out = opts.out.clone();
    let mut i = 0;
    let next = |i: &mut usize| -> Option<String> {
        *i += 1;
        args.get(*i).cloned()
    };
    while i < args.len() {
        let a = args[i].as_str();
        let ok = match a {
            "--help" | "-h" => {
                print!("{USAGE}");
                return 0;
            }
            "--list" => {
                for s in all() {
                    println!("{:<24} {}", s.name, s.about);
                }
                return 0;
            }
            "--tree" => return tree(&args[i + 1..], opts),
            "--still" => match args.get(i + 1) {
                Some(png) => return still(PathBuf::from(png), &args[i + 2..], opts),
                None => false,
            },
            "--out" => next(&mut i).map(|v| out = PathBuf::from(v)).is_some(),
            "--size" => next(&mut i)
                .and_then(|v| parse_size(&v))
                .map(|v| opts.size = v)
                .is_some(),
            "--ppp" => next(&mut i)
                .and_then(|v| v.parse().ok())
                .map(|v| opts.pixels_per_point = v)
                .is_some(),
            "--fps" => next(&mut i)
                .and_then(|v| v.parse().ok())
                .map(|v: u32| opts.fps = v.max(1))
                .is_some(),
            "--every" => next(&mut i)
                .and_then(|v| v.parse().ok())
                .map(|v| opts.record_every = v)
                .is_some(),
            "--hold" => next(&mut i)
                .and_then(|v| v.parse().ok())
                .map(|v| opts.hold_secs = v)
                .is_some(),
            "--video-scale" => next(&mut i)
                .and_then(|v| v.parse().ok())
                .map(|v| opts.video_scale = v)
                .is_some(),
            "--no-video" => {
                opts.record = false;
                true
            }
            "--stop-on-fail" => {
                opts.on_fail = OnFail::Stop;
                true
            }
            name if !name.starts_with('-') => {
                names.push(name.to_string());
                true
            }
            _ => false,
        };
        if !ok {
            eprintln!("lumenply --uitest: bad argument {a:?}\n\n{USAGE}");
            return 2;
        }
        i += 1;
    }
    let registry = all();
    let chosen: Vec<Scenario> = if names.is_empty() {
        registry.clone()
    } else {
        let mut v = Vec::new();
        for n in &names {
            match registry.iter().find(|s| s.name == n) {
                Some(s) => v.push(*s),
                None => {
                    eprintln!("no scenario {n:?}; --list shows them");
                    return 2;
                }
            }
        }
        v
    };
    let mut failed = 0;
    for sc in chosen {
        let opts = Options {
            out: out.join(sc.name),
            ..opts.clone()
        };
        eprintln!("▶ {} — {}", sc.name, sc.about);
        let summary = match Session::start(sc.name, opts) {
            Ok(mut s) => {
                let r = (sc.run)(&mut s);
                s.finish(r)
            }
            Err(e) => {
                eprintln!("  could not start: {e}");
                failed += 1;
                continue;
            }
        };
        if summary.passed {
            eprintln!("  ✓ {} steps", summary.steps);
        } else {
            failed += 1;
            for f in &summary.failed {
                eprintln!("  ✗ {f}");
            }
        }
        if let Some(v) = &summary.video {
            eprintln!("  video: {}", v.display());
        }
        eprintln!("  log:   {}", summary.log.display());
    }
    i32::from(failed > 0)
}

/// `--tree [ARG...]`: what a scenario can click, after launching with ARGs.
fn tree(app_args: &[String], mut opts: Options) -> i32 {
    opts.args = app_args.to_vec();
    opts.record = false;
    opts.out = std::env::temp_dir().join("lumenply-uitest-tree");
    match Session::start("tree", opts) {
        Ok(s) => {
            print!("{}", s.tree().dump());
            s.finish(Ok(()));
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// `--still PNG [ARG...]`: the rendered window after launching with ARGs.
fn still(png: PathBuf, app_args: &[String], mut opts: Options) -> i32 {
    opts.args = app_args.to_vec();
    opts.out = std::env::temp_dir().join("lumenply-uitest-still");
    match Session::start("still", opts) {
        Ok(s) => {
            let saved = s
                .ui_image()
                .map(|img| super::record::save_png(img, &png))
                .unwrap_or_else(|| Err("nothing was rendered (no GPU?)".into()));
            s.finish(Ok(()));
            match saved {
                Ok(()) => {
                    eprintln!("saved {}", png.display());
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// One scenario as a `cargo test`: recorded under
/// `$LUMENPLY_UITEST_OUT/<name>` (default `$TMPDIR/lumenply-uitest`);
/// `LUMENPLY_UITEST_RECORD=0` skips rendering and video.
pub(crate) fn run_test(name: &str, f: fn(&mut Session) -> UiResult) {
    // Sessions are heavy (a GPU device, an encoder) and share the macOS
    // paste-key flag: one at a time.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let out = std::env::var_os("LUMENPLY_UITEST_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("lumenply-uitest"));
    let mut opts = Options::new(out.join(name));
    opts.record = std::env::var("LUMENPLY_UITEST_RECORD").map_or(true, |v| v != "0");
    opts.frame_timeout = Duration::from_secs(300);
    let mut s = Session::start(name, opts).unwrap_or_else(|e| panic!("{name}: could not start: {e}"));
    let r = f(&mut s);
    let summary = s.finish(r);
    assert!(
        summary.passed,
        "{name} failed:\n{}\nlog: {}",
        summary.failed.join("\n"),
        summary.log.display()
    );
}
