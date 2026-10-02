# ADR 0014: Editable Photoshop text (TySh) both ways

Date: 2026-10-03. Status: accepted.

## Context

Text layers left Lumenply as pixels and Photoshop's type layers came in as
pixels, so a PSD round trip lost every edit-ability. Photoshop keeps type
in a `TySh` block: a 2×3 transform, a `TxLr` descriptor whose raw
`EngineData` (a PostScript-like dictionary) holds the characters, style
runs, paragraph runs and the font list (PostScript names), a warp
descriptor, and bounds. Newer files also keep a document-wide `Txt2` block
(text frames, paths) that `TySh` does not repeat.

## Decision

**Our own EngineData codec** (`io::psd::engine_data`): dictionaries,
arrays, integers and decimals kept distinct, booleans, `/Name` values and
UTF-16BE strings with byte-wise backslash escapes. The writer reproduces
Photoshop CS6's layout (tabs, `.5` decimals, no exponents) because both
Photoshop and psd-tools tokenise it by whitespace and regular expressions.

**Import** (`io::psd::text`) maps onto `TextLayer`: text (\r and U+0003 →
newline, trailing \r dropped), the style covering most characters becomes
the layer's (font, size × the transform's scale, colour from 0–1 sRGB
`[a r g b]`, tracking, leading — auto uses the paragraph's AutoLeading,
explicit becomes Leading / FontSize — baseline shift, caps, metrics
kerning, underline, strikethrough, faux bold/italic), the other runs
become `TextRun`s (colour, size, bold, italic, underline, strikethrough;
UTF-16 run lengths mapped to byte offsets), the first paragraph's
justification sets alignment (0 left, 1 right, 2 centre, 3–6 justify).
Point text anchors at the transform origin; box text uses `BoxBounds`.
Anything the model cannot represent is listed in one "simplified on
import" warning (several families, paragraphs aligned differently,
per-character tracking/leading/shift, small caps, character scaling,
last-line justification).

**Pixels are kept instead** when the text would land somewhere else:
rotated, skewed, flipped or stretched transforms, warps, vertical text,
and — as a safety net for layouts kept outside `TySh` such as type on a
path — whenever our re-rendered ink overlaps the pixels Photoshop stored
by less than 20% (intersection over union; real files with substituted
fonts measure 0.6–1.0, type on a path 0.0).

**Fonts** go through `render::font_names`: a PostScript name found in
fontdb becomes its family plus bold/italic when the family query leads
back to that face, otherwise the PostScript name itself, which the
rasteriser now resolves to that exact face (e.g. HelveticaNeue-Light).
Unknown names are kept verbatim (bold/italic guessed from the suffix), so
the app lists them as missing and they export unchanged. Export asks
fontdb for the face's PostScript name and marks bold/italic the face lacks
as FauxBold/FauxItalic; the bundled face is `DejaVuSans`/`DejaVuSans-Bold`.

**First baseline in a box.** Photoshop puts it a "d" height below the box
top; our layout (ADR 0013) uses the font's full ascent. Rather than change
the layout, import raises the box by the difference (in 1/32 px, so the
reverse on export is exact) and keeps its bottom edge; export lowers it
back. If the layout ever adopts Photoshop's rule, drop
`box_baseline_gap`.

**Export** writes `TySh` with Photoshop's full structure (EngineDict,
ResourceDict and DocumentResources with kinsoku/mojikumi sets, normal
style and paragraph sheets, FontSet starting with AdobeInvisFont), one
paragraph run per paragraph and one style run per run edge, beside the
rendered pixels other readers show. Layers with smart filters still bake.

## Consequences

- PSD text round-trips as text both ways; psd-tools reads our type layers
  (kind, text, fonts, sizes, colours, justification, runs, box bounds).
- Not carried: rotation/warp (no model for it), text on paths, per-run
  families, paragraph spacing and indents, Txt2-only data, stroke colour.
- Rendering differs from Photoshop wherever the font is substituted; the
  text stays editable and the layer re-renders in the real font once it
  is installed.
