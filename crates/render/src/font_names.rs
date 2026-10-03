//! PostScript font names, the way Photoshop's type layers name a face
//! ("ArialMT", "MyriadPro-Bold"), mapped to and from a text layer's font
//! fields: a family plus bold and italic, or — for a face the family's
//! regular/bold/italic slots cannot reach, such as "HelveticaNeue-Light" —
//! the PostScript name itself, which [`crate::text`] resolves to that exact
//! face. Names this machine does not have are kept as they are (the layer
//! renders in the bundled face and the app lists the font as missing).

use crate::text::{font_db, BOLD_WEIGHT, BUNDLED_FAMILY};

/// The bundled DejaVu faces' PostScript names.
const BUNDLED_REGULAR: &str = "DejaVuSans";
const BUNDLED_BOLD: &str = "DejaVuSans-Bold";

/// A text layer's font fields for one PostScript name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerFont {
    /// What goes into `TextLayer::font`: a family, a PostScript name, or
    /// empty for the bundled face.
    pub font: String,
    pub bold: bool,
    pub italic: bool,
}

/// A PostScript name's style, guessed from its suffix when the font is not
/// installed: "Arial-BoldItalicMT" is bold and italic.
pub fn style_from_name(name: &str) -> (bool, bool) {
    let style = name.rsplit_once('-').map_or("", |(_, s)| s).to_ascii_lowercase();
    let bold = ["bold", "black", "heavy", "semibold", "demi"]
        .iter()
        .any(|w| style.contains(w));
    let italic = style.contains("italic") || style.contains("oblique") || style.ends_with("it");
    (bold, italic)
}

/// The installed face with this PostScript name.
pub(crate) fn face_by_postscript(name: &str) -> Option<fontdb::ID> {
    if name.is_empty() {
        return None;
    }
    font_db()
        .faces()
        .find(|f| f.post_script_name == name)
        .map(|f| f.id)
}

fn face_style(id: fontdb::ID) -> (bool, bool) {
    font_db().face(id).map_or((false, false), |f| {
        (f.weight.0 >= BOLD_WEIGHT, f.style != fontdb::Style::Normal)
    })
}

fn family_query(family: &str, bold: bool, italic: bool) -> Option<fontdb::ID> {
    font_db().query(&fontdb::Query {
        families: &[fontdb::Family::Name(family)],
        weight: if bold {
            fontdb::Weight::BOLD
        } else {
            fontdb::Weight::NORMAL
        },
        stretch: fontdb::Stretch::Normal,
        style: if italic {
            fontdb::Style::Italic
        } else {
            fontdb::Style::Normal
        },
    })
}

/// The layer font for a PostScript name (Photoshop's `FontSet` entry).
pub fn from_postscript(name: &str) -> LayerFont {
    match name {
        "" | BUNDLED_REGULAR => {
            return LayerFont {
                font: String::new(),
                bold: false,
                italic: false,
            }
        }
        BUNDLED_BOLD => {
            return LayerFont {
                font: String::new(),
                bold: true,
                italic: false,
            }
        }
        _ => {}
    }
    if let Some(id) = face_by_postscript(name) {
        let (bold, italic) = face_style(id);
        let family = font_db()
            .face(id)
            .and_then(|f| f.families.first().map(|(n, _)| n.clone()))
            .unwrap_or_default();
        // The family name is enough when it leads back to this very face.
        if !family.is_empty() && family_query(&family, bold, italic) == Some(id) {
            return LayerFont {
                font: family,
                bold,
                italic,
            };
        }
        return LayerFont {
            font: name.to_string(),
            bold: false,
            italic: false,
        };
    }
    let (bold, italic) = style_from_name(name);
    LayerFont {
        font: name.to_string(),
        bold,
        italic,
    }
}

/// The PostScript name for a layer's font drawn bold and/or italic, and
/// whether that face is itself bold and italic (a writer marks whatever it
/// is not as Photoshop's faux bold / faux italic).
pub fn to_postscript(font: &str, bold: bool, italic: bool) -> (String, bool, bool) {
    if font.is_empty() || font == BUNDLED_FAMILY {
        return if bold {
            (BUNDLED_BOLD.into(), true, false)
        } else {
            (BUNDLED_REGULAR.into(), false, false)
        };
    }
    if crate::text::is_font_path(font) {
        if let Ok(data) = std::fs::read(font) {
            let mut db = fontdb::Database::new();
            db.load_font_data(data);
            let face = db.faces().next().map(|f| {
                (
                    f.post_script_name.clone(),
                    f.weight.0 >= BOLD_WEIGHT,
                    f.style != fontdb::Style::Normal,
                )
            });
            if let Some(face) = face {
                return face;
            }
        }
        let stem = std::path::Path::new(font)
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        let (b, i) = style_from_name(&stem);
        return (stem, b, i);
    }
    if let Some(id) = family_query(font, bold, italic) {
        if let Some(f) = font_db().face(id) {
            let (b, i) = face_style(id);
            return (f.post_script_name.clone(), b, i);
        }
    }
    if let Some(id) = face_by_postscript(font) {
        let (b, i) = face_style(id);
        return (font.to_string(), b, i);
    }
    // Not installed: a PostScript-looking name is kept as it is; a family
    // name becomes the usual "Family-Style" spelling.
    let name = if font.contains(' ') {
        let style = match (bold, italic) {
            (true, true) => "BoldItalic",
            (true, false) => "Bold",
            (false, true) => "Italic",
            (false, false) => "Regular",
        };
        format!("{}-{style}", font.replace(' ', ""))
    } else {
        font.to_string()
    };
    let (b, i) = style_from_name(&name);
    (name, b, i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_suffixes_read_as_bold_and_italic() {
        assert_eq!(style_from_name("Arial-BoldItalicMT"), (true, true));
        assert_eq!(style_from_name("MyriadPro-Bold"), (true, false));
        assert_eq!(style_from_name("MyriadPro-It"), (false, true));
        assert_eq!(style_from_name("Helvetica-Oblique"), (false, true));
        assert_eq!(style_from_name("ArialMT"), (false, false));
        assert_eq!(style_from_name("MyriadPro-Regular"), (false, false));
        // "Bold" in the family part is not a style.
        assert_eq!(style_from_name("BoldFont-Regular"), (false, false));
    }

    #[test]
    fn missing_fonts_keep_their_names_both_ways() {
        let f = from_postscript("NoSuchFontXYZ-BoldItalic");
        assert_eq!(
            f,
            LayerFont {
                font: "NoSuchFontXYZ-BoldItalic".into(),
                bold: true,
                italic: true
            }
        );
        assert_eq!(
            to_postscript("NoSuchFontXYZ-BoldItalic", true, true),
            ("NoSuchFontXYZ-BoldItalic".into(), true, true)
        );
        // A regular name drawn bold is a faux bold of that face.
        assert_eq!(
            to_postscript("NoSuchFontXYZ-Regular", true, false),
            ("NoSuchFontXYZ-Regular".into(), false, false)
        );
        // A missing family name becomes Family-Style.
        assert_eq!(
            to_postscript("No Such Font XYZ", false, true),
            ("NoSuchFontXYZ-Italic".into(), false, true)
        );
    }

    #[test]
    fn the_bundled_face_maps_to_dejavu_postscript_names() {
        assert_eq!(
            to_postscript("", false, false),
            ("DejaVuSans".into(), false, false)
        );
        assert_eq!(
            to_postscript("", true, true),
            ("DejaVuSans-Bold".into(), true, false)
        );
        assert_eq!(
            from_postscript("DejaVuSans-Bold"),
            LayerFont {
                font: String::new(),
                bold: true,
                italic: false
            }
        );
    }

    #[test]
    fn installed_faces_round_trip_through_their_postscript_names() {
        // Every installed face whose family reaches it maps back to the
        // same family and style (skips quietly on a machine without fonts).
        let mut checked = 0;
        for f in font_db().faces().take(400) {
            let lf = from_postscript(&f.post_script_name);
            if lf.font.is_empty() || lf.font == f.post_script_name {
                continue;
            }
            let (name, b, i) = to_postscript(&lf.font, lf.bold, lf.italic);
            assert_eq!(name, f.post_script_name, "{lf:?}");
            assert_eq!((b, i), (lf.bold, lf.italic), "{lf:?}");
            checked += 1;
        }
        eprintln!("{checked} installed faces round-tripped");
    }
}
