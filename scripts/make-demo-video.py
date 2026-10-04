#!/usr/bin/env python3
"""Turn the `showcase` user session into the README's demo GIF and MP4.

Records nothing itself: run the scenario first (a uitest build), then this.

    cargo build --release -p lumenply-app --features uitest
    LUMENPLY_UITEST_PHOTOS=<dir with woman.jpg> target/release/lumenply-app \\
        --uitest showcase --out OUT --size 1280x800 --ppp 2 --video-scale 0.75 \\
        --models-from <dir with mobile-sam/ and birefnet-lite/>
    scripts/make-demo-video.py OUT/showcase docs/media

Steps whose description starts with "✦ " become the captions; everything
before the first of them (launching, opening the photo) is cut, and the
harness's own caption bar under the window is cropped away. Needs ffmpeg
and Pillow.
"""
import json, os, subprocess, sys, tempfile

from PIL import Image, ImageDraw, ImageFont

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FONT = os.path.join(ROOT, "crates", "render", "fonts", "IBMPlexSans-SemiBold.ttf")
MARK = "✦ "
LEAD_IN = 0.4  # seconds kept before the first caption
HOLD_END = 2.5  # seconds the last frame stays
GIF_WIDTH, GIF_FPS = 800, 8
# The GIF stops where this caption would start and closes on the final
# caption over the frozen frame: shorter, so the README loads fast.
GIF_CUT = "Cmd+K"
POSTER_AT = "Nothing is destroyed"  # the poster is the clean frame just before this caption


def captions(session):
    """[(start, end, text)] in seconds of the session video."""
    steps = [s for s in session["steps"] if s["description"].startswith(MARK)]
    end = float(session["video_seconds"])
    out = []
    for i, s in enumerate(steps):
        a = float(s["video_from_s"])
        b = float(steps[i + 1]["video_from_s"]) if i + 1 < len(steps) else end + HOLD_END
        out.append((a, b, s["description"][len(MARK):]))
    return out


def caption_png(text, width, path):
    """A centred pill with the caption, on a transparent strip `width` wide."""
    size = round(width / 44)
    font = ImageFont.truetype(FONT, size)
    pad_x, pad_y = size, round(size * 0.55)
    probe = ImageDraw.Draw(Image.new("RGBA", (1, 1)))
    l, t, r, b = probe.textbbox((0, 0), text, font=font)
    w, h = r - l + 2 * pad_x, b - t + 2 * pad_y
    img = Image.new("RGBA", (width, h + 4), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    x0 = (width - w) // 2
    d.rounded_rectangle((x0, 2, x0 + w, 2 + h), radius=h // 2, fill=(17, 19, 24, 225),
                        outline=(245, 158, 11, 255), width=max(2, size // 14))
    d.text((x0 + pad_x - l, 2 + pad_y - t), text, font=font, fill=(255, 255, 255, 255))
    img.save(path)
    return img.size


def build(video, vw, crop_h, caps, start, end, tmp, out):
    """Crop, trim to [start, end], hold the last frame, overlay the captions."""
    inputs = ["-ss", f"{start:.3f}", "-t", f"{end - start:.3f}", "-i", video]
    chain = [f"[0:v]crop={vw}:{crop_h}:0:0,tpad=stop_mode=clone:stop_duration={HOLD_END}[v0]"]
    for i, (a, b, text) in enumerate(caps):
        png = os.path.join(tmp, f"{os.path.basename(out)}-cap{i}.png")
        _, ch = caption_png(text, vw, png)
        inputs += ["-i", png]
        y = crop_h - ch - round(crop_h * 0.035)
        chain.append(
            f"[v{i}][{i + 1}:v]overlay=0:{y}:enable='between(t,{a - start:.3f},{b - start:.3f})'[v{i + 1}]")
    subprocess.run(["ffmpeg", "-loglevel", "error", "-y", *inputs, "-filter_complex", ";".join(chain),
                    "-map", f"[v{len(caps)}]", "-c:v", "libx264", "-crf", "16", "-preset", "slow",
                    "-pix_fmt", "yuv420p", out], check=True)


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    sess_dir, out_dir = sys.argv[1], sys.argv[2]
    session = json.load(open(os.path.join(sess_dir, "session.json")))
    video = os.path.join(sess_dir, "session.mp4")
    probe = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries",
         "stream=width,height", "-of", "csv=p=0", video],
        capture_output=True, text=True, check=True).stdout.strip()
    vw, vh = map(int, probe.split(","))
    win_w, win_h = session["window"]
    # The window fills the width; the harness's caption bar is below it.
    crop_h = round(vw * win_h / win_w) // 2 * 2
    caps = captions(session)
    start = max(0.0, caps[0][0] - LEAD_IN)
    end = float(session["video_seconds"])
    os.makedirs(out_dir, exist_ok=True)

    # The GIF: up to the cut, then the final caption over the held frame.
    cut = next((i for i, c in enumerate(caps) if c[2].startswith(GIF_CUT)), len(caps))
    cut_t = caps[cut][0] if cut < len(caps) else end
    short = [(a, min(b, cut_t), t) for a, b, t in caps[:cut]]
    if cut < len(caps):
        short.append((cut_t, cut_t + HOLD_END + 1, caps[-1][2]))

    with tempfile.TemporaryDirectory() as tmp:
        full = os.path.join(tmp, "full.mp4")
        build(video, vw, crop_h, caps, start, end, tmp, full)
        mp4 = os.path.join(out_dir, "lumenply-demo.mp4")
        subprocess.run(["ffmpeg", "-loglevel", "error", "-y", "-i", full, "-vf", "scale=1440:-2:flags=lanczos",
                        "-c:v", "libx264", "-crf", "24", "-preset", "slow", "-pix_fmt", "yuv420p",
                        "-movflags", "+faststart", mp4], check=True)

        brief = os.path.join(tmp, "brief.mp4")
        build(video, vw, crop_h, short, start, cut_t, tmp, brief)
        gif = os.path.join(out_dir, "lumenply-demo.gif")
        scale = f"fps={GIF_FPS},scale={GIF_WIDTH}:-1:flags=lanczos"
        subprocess.run(["ffmpeg", "-loglevel", "error", "-y", "-i", brief, "-filter_complex",
                        f"[0:v]{scale},split[a][b];[a]palettegen=max_colors=256:stats_mode=diff[p];"
                        f"[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle", gif], check=True)

        poster = os.path.join(out_dir, "lumenply-demo-poster.png")
        at = next((c[0] for c in caps if c[2].startswith(POSTER_AT)), end) - 0.1
        subprocess.run(["ffmpeg", "-loglevel", "error", "-y", "-ss", f"{at:.2f}", "-i", video, "-frames:v", "1",
                        "-vf", f"crop={vw}:{crop_h}:0:0,scale=1440:-2:flags=lanczos", poster], check=True)
    for f in (gif, mp4, poster):
        print(f"{f}: {os.path.getsize(f) / 1e6:.1f} MB")


if __name__ == "__main__":
    main()
