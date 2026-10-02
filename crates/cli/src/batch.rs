//! `lumenply batch`: Photoshop's Image Processor on the command line —
//! open many files (images, camera RAW, PSD, projects), optionally
//! develop RAW with Auto, resize, and write PNG, JPEG or WebP.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use lumenply_render::develop::{auto_develop, develop, Develop};
use lumenply_render::resample::resample;
use lumenply_tiles::{Raster, Rgba};

/// How to size each output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Resize {
    /// Scale by a fraction (0.5 for "50%").
    Percent(f32),
    /// Longest side at most this many pixels (never enlarges).
    LongEdge(u32),
    /// Fit inside a box (never enlarges).
    Fit(u32, u32),
}

impl Resize {
    /// "50%", "2048", or "1920x1080".
    pub fn parse(s: &str) -> Result<Resize> {
        let s = s.trim();
        if let Some(p) = s.strip_suffix('%') {
            let v: f32 = p.trim().parse().context("percentage")?;
            if !(v > 0.0 && v <= 1000.0) {
                bail!("percentage {v} is out of range");
            }
            return Ok(Resize::Percent(v / 100.0));
        }
        if let Some((w, h)) = s.split_once(['x', 'X']) {
            let (w, h): (u32, u32) = (
                w.trim().parse().context("width")?,
                h.trim().parse().context("height")?,
            );
            if w == 0 || h == 0 {
                bail!("a fit box needs two sizes above zero");
            }
            return Ok(Resize::Fit(w, h));
        }
        let n: u32 = s.parse().context("size: use 50%, 2048 or 1920x1080")?;
        if n == 0 {
            bail!("the long edge must be above zero");
        }
        Ok(Resize::LongEdge(n))
    }

    /// The output size for a `w × h` image.
    pub fn apply(self, w: u32, h: u32) -> (u32, u32) {
        let scale = match self {
            Resize::Percent(f) => f,
            Resize::LongEdge(n) => (n as f32 / w.max(h) as f32).min(1.0),
            Resize::Fit(bw, bh) => (bw as f32 / w as f32).min(bh as f32 / h as f32).min(1.0),
        };
        (
            ((w as f32 * scale).round() as u32).max(1),
            ((h as f32 * scale).round() as u32).max(1),
        )
    }
}

pub struct Options {
    pub out: PathBuf,
    pub format: String,
    pub quality: u8,
    pub resize: Option<Resize>,
    pub auto: bool,
    pub flatten: bool,
}

/// The flattened image of any file Lumenply opens.
fn open_flat(path: &Path, auto: bool) -> Result<Raster> {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with(".lumen")
        || lower.ends_with(".nge")
        || lower.ends_with(".psd")
        || lower.ends_with(".psb")
    {
        let doc = super::load_any(&path.to_path_buf())?;
        return Ok(lumenply_render::composite_raster(&doc));
    }
    let raster = lumenply_io::load(path)?;
    if lumenply_io::raw::is_raw(path) {
        // As the Camera Raw workspace opens it: the camera tone curve,
        // plus Auto when asked.
        let d = if auto {
            auto_develop(&raster)
        } else {
            Develop::default()
        };
        return Ok(develop(&raster, &d));
    }
    Ok(raster)
}

fn encode(r: &Raster, o: &Options) -> Result<(Vec<u8>, &'static str)> {
    Ok(match o.format.to_ascii_lowercase().as_str() {
        "png" => (lumenply_io::encode_png(r, !o.flatten)?, "png"),
        "jpg" | "jpeg" => (lumenply_io::encode_jpeg(r, o.quality)?, "jpg"),
        "webp" => (lumenply_io::encode_webp(r, !o.flatten)?, "webp"),
        other => bail!("unknown format '{other}' (png, jpeg or webp)"),
    })
}

/// Converts every input; returns how many failed.
pub fn run(inputs: &[PathBuf], o: &Options) -> Result<usize> {
    std::fs::create_dir_all(&o.out).with_context(|| format!("creating {}", o.out.display()))?;
    let started = Instant::now();
    let mut failed = 0;
    for input in inputs {
        let t = Instant::now();
        let result = (|| -> Result<PathBuf> {
            let mut img = open_flat(input, o.auto)?;
            if let Some(rs) = o.resize {
                let (w, h) = rs.apply(img.width, img.height);
                img = resample(&img, w, h);
            }
            if o.flatten {
                for p in &mut img.pixels {
                    *p = p.over(Rgba::WHITE);
                }
            }
            let (bytes, ext) = encode(&img, o)?;
            let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("image");
            let dest = o.out.join(format!("{stem}.{ext}"));
            if dest.canonicalize().ok() == input.canonicalize().ok() && dest.exists() {
                bail!("would overwrite the input; choose another --out folder");
            }
            std::fs::write(&dest, bytes)?;
            println!(
                "{} -> {} ({}×{}, {:.0} ms)",
                input.display(),
                dest.display(),
                img.width,
                img.height,
                t.elapsed().as_secs_f32() * 1000.0
            );
            Ok(dest)
        })();
        if let Err(e) = result {
            eprintln!("{}: {e:#}", input.display());
            failed += 1;
        }
    }
    eprintln!(
        "{} of {} converted in {:.1} s",
        inputs.len() - failed,
        inputs.len(),
        started.elapsed().as_secs_f32()
    );
    Ok(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_specs_parse_and_never_enlarge_unless_asked() {
        assert_eq!(Resize::parse("50%").unwrap(), Resize::Percent(0.5));
        assert_eq!(Resize::parse("2048").unwrap(), Resize::LongEdge(2048));
        assert_eq!(Resize::parse("1920x1080").unwrap(), Resize::Fit(1920, 1080));
        assert!(
            Resize::parse("0").is_err() && Resize::parse("abc").is_err() && Resize::parse("0x5").is_err()
        );
        assert_eq!(Resize::Percent(0.5).apply(4000, 3000), (2000, 1500));
        assert_eq!(Resize::Percent(2.0).apply(100, 50), (200, 100));
        assert_eq!(Resize::LongEdge(2048).apply(6000, 4000), (2048, 1365));
        assert_eq!(
            Resize::LongEdge(2048).apply(1000, 800),
            (1000, 800),
            "no enlarging"
        );
        assert_eq!(Resize::Fit(1920, 1080).apply(4000, 4000), (1080, 1080));
        assert_eq!(Resize::Fit(1920, 1080).apply(6000, 2000), (1920, 640));
    }

    #[test]
    fn a_batch_converts_and_reports_failures() {
        let dir = std::env::temp_dir().join(format!("lumenply-batch-{}", std::process::id()));
        let out = dir.join("out");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("a.png");
        let mut r = Raster::new(40, 20);
        for p in &mut r.pixels {
            *p = Rgba::new(0.2, 0.4, 0.6, 1.0);
        }
        lumenply_io::save_png(&src, &r).unwrap();
        let missing = dir.join("missing.png");
        let opts = Options {
            out: out.clone(),
            format: "jpeg".into(),
            quality: 80,
            resize: Some(Resize::Percent(0.5)),
            auto: false,
            flatten: true,
        };
        let failed = run(&[src, missing], &opts).unwrap();
        assert_eq!(failed, 1);
        let img = image::open(out.join("a.jpg")).unwrap();
        assert_eq!((img.width(), img.height()), (20, 10));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
