//! Image ▸ Image Size and File ▸ New with print sizes, as in Photoshop:
//! width and height in pixels, percent, inches, centimetres or
//! millimetres; resolution in pixels per inch or per centimetre; and the
//! Resample switch. With Resample off the pixels stay and print size and
//! resolution trade off against each other; with it on, pixels are added or
//! removed. The maths lives in [`ImageSizeState`] (no egui), so it is
//! tested without a window.

use super::*;
use crate::dialogs::note;
use lumenply_doc::RESOLUTION_RANGE;

/// The largest canvas side the size dialogs accept.
pub(crate) const MAX_SIDE: u32 = 30_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SizeUnit {
    Pixels,
    Percent,
    Inches,
    Cm,
    Mm,
}

impl SizeUnit {
    pub(crate) const ALL: [SizeUnit; 5] = [
        SizeUnit::Pixels,
        SizeUnit::Percent,
        SizeUnit::Inches,
        SizeUnit::Cm,
        SizeUnit::Mm,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            SizeUnit::Pixels => "Pixels",
            SizeUnit::Percent => "Percent",
            SizeUnit::Inches => "Inches",
            SizeUnit::Cm => "Centimetres",
            SizeUnit::Mm => "Millimetres",
        }
    }

    /// Units of this kind in one inch; `None` for pixels and percent.
    fn per_inch(self) -> Option<f64> {
        match self {
            SizeUnit::Inches => Some(1.0),
            SizeUnit::Cm => Some(2.54),
            SizeUnit::Mm => Some(25.4),
            SizeUnit::Pixels | SizeUnit::Percent => None,
        }
    }

    /// Whether a value in this unit names pixels (so only resampling can
    /// change it).
    fn is_pixel_based(self) -> bool {
        self.per_inch().is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResUnit {
    PerInch,
    PerCm,
}

impl ResUnit {
    pub(crate) fn label(self) -> &'static str {
        match self {
            ResUnit::PerInch => "Pixels/Inch",
            ResUnit::PerCm => "Pixels/Centimetre",
        }
    }
}

/// The Image Size dialog's state. Pixel sizes are whole numbers (rounded
/// as the user edits print sizes); the resolution keeps full precision so
/// a typed print size reads back exactly.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ImageSizeState {
    /// The document as the dialog opened on it.
    pub(crate) orig: (u32, u32, f32),
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Pixels per inch.
    pub(crate) ppi: f64,
    pub(crate) unit: SizeUnit,
    pub(crate) res_unit: ResUnit,
    pub(crate) resample: bool,
    /// Width and height keep the original aspect ratio (always, with
    /// Resample off).
    pub(crate) constrain: bool,
}

fn clamp_side(v: f64) -> u32 {
    v.round().clamp(1.0, MAX_SIDE as f64) as u32
}

fn clamp_ppi(v: f64) -> f64 {
    v.clamp(*RESOLUTION_RANGE.start() as f64, *RESOLUTION_RANGE.end() as f64)
}

impl ImageSizeState {
    pub(crate) fn new(width: u32, height: u32, ppi: f32) -> Self {
        ImageSizeState {
            orig: (width, height, ppi),
            width,
            height,
            ppi: ppi as f64,
            unit: SizeUnit::Pixels,
            res_unit: ResUnit::PerInch,
            resample: true,
            constrain: true,
        }
    }

    pub(crate) fn for_doc(doc: &Document) -> Self {
        Self::new(doc.width, doc.height, doc.resolution)
    }

    fn show(&self, px: u32, orig: u32, unit: SizeUnit) -> f64 {
        match unit.per_inch() {
            Some(k) => px as f64 / self.ppi * k,
            None if unit == SizeUnit::Percent => px as f64 / orig.max(1) as f64 * 100.0,
            None => px as f64,
        }
    }

    /// The width as shown in `unit`.
    pub(crate) fn width_in(&self, unit: SizeUnit) -> f64 {
        self.show(self.width, self.orig.0, unit)
    }

    /// The height as shown in `unit`.
    pub(crate) fn height_in(&self, unit: SizeUnit) -> f64 {
        self.show(self.height, self.orig.1, unit)
    }

    /// Whether the width/height fields can be edited in `unit` (pixel
    /// counts are fixed while Resample is off).
    pub(crate) fn editable(&self, unit: SizeUnit) -> bool {
        self.resample || !unit.is_pixel_based()
    }

    /// Set one side from a value in `unit`. `horizontal` picks width.
    fn set_side(&mut self, horizontal: bool, unit: SizeUnit, v: f64) {
        if !v.is_finite() || v <= 0.0 || !self.editable(unit) {
            return;
        }
        let orig = if horizontal { self.orig.0 } else { self.orig.1 };
        match unit.per_inch() {
            // A print size with Resample off: the pixels stay, so the
            // resolution is what changes (both sides follow).
            Some(k) if !self.resample => {
                let px = if horizontal { self.width } else { self.height };
                self.ppi = clamp_ppi(px as f64 / (v / k));
            }
            Some(k) => {
                let px = clamp_side(v / k * self.ppi);
                self.set_px(horizontal, px);
            }
            None => {
                let px = if unit == SizeUnit::Percent {
                    clamp_side(v / 100.0 * orig as f64)
                } else {
                    clamp_side(v)
                };
                self.set_px(horizontal, px);
            }
        }
    }

    fn set_px(&mut self, horizontal: bool, px: u32) {
        let (ow, oh) = (self.orig.0.max(1) as f64, self.orig.1.max(1) as f64);
        if horizontal {
            self.width = px;
            if self.constrain {
                self.height = clamp_side(px as f64 * oh / ow);
            }
        } else {
            self.height = px;
            if self.constrain {
                self.width = clamp_side(px as f64 * ow / oh);
            }
        }
    }

    pub(crate) fn set_width(&mut self, unit: SizeUnit, v: f64) {
        self.set_side(true, unit, v);
    }

    pub(crate) fn set_height(&mut self, unit: SizeUnit, v: f64) {
        self.set_side(false, unit, v);
    }

    /// The resolution as shown in `unit`.
    pub(crate) fn resolution_in(&self, unit: ResUnit) -> f64 {
        match unit {
            ResUnit::PerInch => self.ppi,
            ResUnit::PerCm => self.ppi / 2.54,
        }
    }

    /// Set the resolution from a value in `unit`. With Resample on the
    /// print size stays and the pixel count follows.
    pub(crate) fn set_resolution(&mut self, unit: ResUnit, v: f64) {
        if !v.is_finite() || v <= 0.0 {
            return;
        }
        let ppi = clamp_ppi(match unit {
            ResUnit::PerInch => v,
            ResUnit::PerCm => v * 2.54,
        });
        if self.resample {
            let (w_in, h_in) = (self.width as f64 / self.ppi, self.height as f64 / self.ppi);
            self.width = clamp_side(w_in * ppi);
            self.height = clamp_side(h_in * ppi);
        }
        self.ppi = ppi;
    }

    /// Turning Resample off restores the original pixel size (pixels can
    /// no longer change) and locks the aspect ratio.
    pub(crate) fn set_resample(&mut self, on: bool) {
        self.resample = on;
        if !on {
            (self.width, self.height) = (self.orig.0, self.orig.1);
            self.constrain = true;
        }
    }

    /// The resolution the command will store (f32, rounded to 1/1000 ppi
    /// so typed values don't pick up float noise).
    fn ppi_f32(&self) -> f32 {
        ((self.ppi * 1000.0).round() / 1000.0) as f32
    }

    /// The edit that OK applies; `None` when nothing changes.
    pub(crate) fn command(&self) -> Option<ResizeImage> {
        let ppi = self.ppi_f32();
        let same_px = (self.width, self.height) == (self.orig.0, self.orig.1);
        if same_px && ppi == self.orig.2 {
            return None;
        }
        Some(ResizeImage {
            width: self.width,
            height: self.height,
            resolution: Some(ppi),
        })
    }

    /// "2400 × 1600 px (was 800 × 600)".
    pub(crate) fn pixels_summary(&self) -> String {
        let (ow, oh, _) = self.orig;
        if (self.width, self.height) == (ow, oh) {
            format!("{} × {} px", self.width, self.height)
        } else {
            format!("{} × {} px (was {ow} × {oh})", self.width, self.height)
        }
    }
}

/// "300" or "72.5": a resolution without trailing zeros.
pub(crate) fn fmt_ppi(ppi: f32) -> String {
    let s = format!("{ppi:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// "6.00 × 4.00 in at 300 ppi".
pub(crate) fn print_size_text(width: u32, height: u32, ppi: f32) -> String {
    let ppi_d = ppi.max(f32::MIN_POSITIVE);
    format!(
        "{:.2} × {:.2} in at {} ppi",
        width as f32 / ppi_d,
        height as f32 / ppi_d,
        fmt_ppi(ppi)
    )
}

/// "15.24 × 10.16 cm".
pub(crate) fn print_size_cm(width: u32, height: u32, ppi: f32) -> String {
    let k = 2.54 / ppi.max(f32::MIN_POSITIVE);
    format!("{:.2} × {:.2} cm", width as f32 * k, height as f32 * k)
}

/// File ▸ New presets: name, pixels, resolution.
pub(crate) const NEW_PRESETS: &[(&str, u32, u32, f32)] = &[
    ("HD 1920 × 1080 (72 ppi)", 1920, 1080, 72.0),
    ("4K UHD 3840 × 2160 (72 ppi)", 3840, 2160, 72.0),
    ("Instagram portrait 1080 × 1350", 1080, 1350, 72.0),
    ("Instagram square 1080 × 1080", 1080, 1080, 72.0),
    // 210 × 297 mm and 8.5 × 11 in at 300 ppi.
    ("A4 (300 ppi)", 2480, 3508, 300.0),
    ("US Letter (300 ppi)", 2550, 3300, 300.0),
    ("4 × 6 in photo (300 ppi)", 1200, 1800, 300.0),
    ("5 × 7 in photo (300 ppi)", 1500, 2100, 300.0),
];

/// The preset a New dialog's values match, if any.
pub(crate) fn matching_preset(w: u32, h: u32, ppi: f32) -> Option<usize> {
    NEW_PRESETS
        .iter()
        .position(|&(_, pw, ph, pp)| (pw, ph) == (w, h) && pp == ppi)
}

/// Logical screen points per inch, estimated: no portable API reports a
/// display's physical size. Retina Macs at default scaling show about
/// 110–127 points per inch; Windows and Linux at 100 % scaling are
/// nominally 96.
pub(crate) fn screen_points_per_inch() -> f32 {
    if cfg!(target_os = "macos") {
        110.0
    } else {
        96.0
    }
}

/// The zoom (document pixels per screen point) at which the document shows
/// at about its printed size.
pub(crate) fn print_size_zoom(ppi: f32) -> f32 {
    (screen_points_per_inch() / ppi.max(1.0)).clamp(0.05, 32.0)
}

impl App {
    /// View ▸ Print size.
    pub(crate) fn run_resolution_action(&mut self, id: &str) -> bool {
        match id {
            "print-size" => {
                self.view_cmd = Some(ViewCmd::PrintSize);
                let doc = self.editor.doc();
                self.status = format!(
                    "Print size (approximate): {}, taking the screen as {} points per inch",
                    print_size_text(doc.width, doc.height, doc.resolution),
                    screen_points_per_inch()
                );
                true
            }
            _ => false,
        }
    }
}

/// A white one-layer document at `ppi`, as File ▸ New makes it.
pub(crate) fn blank_at(w: u32, h: u32, ppi: f32) -> Editor {
    let mut doc = crate::blank(w, h).doc().clone();
    doc.resolution = ppi;
    Editor::new(doc)
}

/// A unit dropdown: its id salt, the value, and the (value, label) choices.
type UnitPick<'a, U> = (&'a str, &'a mut U, &'a [(U, &'a str)]);

/// A labelled number field with a unit dropdown after it.
fn unit_row<U: Copy + PartialEq>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f64,
    decimals: (usize, usize),
    speed: f64,
    enabled: bool,
    unit: Option<UnitPick<'_, U>>,
) -> bool {
    ui.horizontal(|ui| {
        row_label(ui, label, LABEL_W);
        let before = *value;
        let r = ui
            .add_enabled_ui(enabled, |ui| {
                num_field(
                    ui,
                    egui::DragValue::new(value)
                        .speed(speed)
                        .min_decimals(decimals.0)
                        .max_decimals(decimals.1)
                        .range(0.001..=1.0e6),
                    96.0,
                )
            })
            .inner;
        a11y_name(&r, label);
        if let Some((salt, u, options)) = unit {
            let current = options.iter().find(|(v, _)| v == u).map_or("", |(_, l)| *l);
            let c = egui::ComboBox::from_id_salt(salt)
                .selected_text(current)
                .width(132.0)
                .show_ui(ui, |ui| {
                    popup_style(ui);
                    for (v, l) in options {
                        ui.selectable_value(u, *v, *l);
                    }
                });
            a11y_name(&c.response, &format!("{label} unit"));
        }
        *value != before
    })
    .inner
}

/// The Image Size dialog's body.
pub(crate) fn image_size_ui(ui: &mut egui::Ui, st: &mut ImageSizeState) {
    ui.horizontal(|ui| {
        row_label(ui, "Dimensions", LABEL_W);
        ui.label(RichText::new(st.pixels_summary()).monospace().color(TEXT));
    });
    ui.add_space(4.0);
    let units: Vec<(SizeUnit, &str)> = SizeUnit::ALL.iter().map(|u| (*u, u.label())).collect();
    let unit = st.unit;
    let editable = st.editable(unit);
    let decimals = if unit == SizeUnit::Pixels { (0, 0) } else { (2, 2) };
    let speed = match unit {
        SizeUnit::Pixels => 1.0,
        SizeUnit::Percent => 0.5,
        SizeUnit::Inches => 0.01,
        SizeUnit::Cm => 0.02,
        SizeUnit::Mm => 0.2,
    };
    let mut w = st.width_in(unit);
    let mut u = st.unit;
    if unit_row(
        ui,
        "Width",
        &mut w,
        decimals,
        speed,
        editable,
        Some(("image-size-unit", &mut u, units.as_slice())),
    ) {
        st.set_width(unit, w);
    }
    st.unit = u;
    let mut h = st.height_in(unit);
    if unit_row::<SizeUnit>(ui, "Height", &mut h, decimals, speed, editable, None) {
        st.set_height(unit, h);
    }
    ui.horizontal(|ui| {
        ui.add_space(LABEL_W + ui.spacing().item_spacing.x);
        ui.add_enabled_ui(st.resample, |ui| {
            check(ui, &mut st.constrain, "Keep aspect ratio")
        });
    });
    let res_units = [
        (ResUnit::PerInch, ResUnit::PerInch.label()),
        (ResUnit::PerCm, ResUnit::PerCm.label()),
    ];
    let mut r = st.resolution_in(st.res_unit);
    let mut ru = st.res_unit;
    if unit_row(
        ui,
        "Resolution",
        &mut r,
        (0, 2),
        1.0,
        true,
        Some(("image-size-res-unit", &mut ru, &res_units[..])),
    ) {
        st.set_resolution(st.res_unit, r);
    }
    st.res_unit = ru;
    ui.horizontal(|ui| {
        ui.add_space(LABEL_W + ui.spacing().item_spacing.x);
        let mut on = st.resample;
        if check(ui, &mut on, "Resample").changed() {
            st.set_resample(on);
        }
    });
    let (w, h) = (st.width, st.height);
    let ppi = st.ppi_f32();
    note(
        ui,
        &format!(
            "Prints at {} ({}).",
            print_size_text(w, h, ppi),
            print_size_cm(w, h, ppi)
        ),
    );
    note(
        ui,
        if st.resample {
            "Resample on: pixels are added or removed (bilinear when enlarging, averaged when shrinking)."
        } else {
            "Resample off: the pixels stay; print size and resolution change together."
        },
    );
}

/// A pixel count shown in `unit` (pixels, or a print size at `ppi`).
fn px_in(px: u32, unit: SizeUnit, ppi: f32) -> f64 {
    match unit.per_inch() {
        Some(k) => px as f64 / ppi.max(f32::MIN_POSITIVE) as f64 * k,
        None => px as f64,
    }
}

/// The pixel count for `v` in `unit` at `ppi`.
fn px_from(v: f64, unit: SizeUnit, ppi: f32) -> u32 {
    if !v.is_finite() || v <= 0.0 {
        return 1;
    }
    match unit.per_inch() {
        Some(k) => clamp_side(v / k * ppi as f64),
        None => clamp_side(v),
    }
}

/// File ▸ New's body: preset, size in pixels or print units, resolution,
/// and the print size.
pub(crate) fn new_doc_ui(ui: &mut egui::Ui, w: &mut u32, h: &mut u32, ppi: &mut f32) {
    ui.horizontal(|ui| {
        row_label(ui, "Preset", LABEL_W);
        let current = matching_preset(*w, *h, *ppi).map_or("Custom", |i| NEW_PRESETS[i].0);
        let mut pick = None;
        let r = egui::ComboBox::from_id_salt("new-doc-preset")
            .selected_text(current)
            .width(232.0)
            .show_ui(ui, |ui| {
                popup_style(ui);
                for (i, (name, ..)) in NEW_PRESETS.iter().enumerate() {
                    if ui.selectable_label(current == *name, *name).clicked() {
                        pick = Some(i);
                    }
                }
            });
        a11y_name(&r.response, "Document preset");
        if let Some(i) = pick {
            let (_, pw, ph, pp) = NEW_PRESETS[i];
            (*w, *h, *ppi) = (pw, ph, pp);
        }
    });
    // The size unit is a view preference kept for the session.
    let unit_id = egui::Id::new("new-doc-size-unit");
    let unit = ui
        .data(|d| d.get_temp::<SizeUnit>(unit_id))
        .unwrap_or(SizeUnit::Pixels);
    let units: Vec<(SizeUnit, &str)> = SizeUnit::ALL
        .iter()
        .filter(|u| **u != SizeUnit::Percent)
        .map(|u| (*u, u.label()))
        .collect();
    let (decimals, speed) = match unit.per_inch() {
        None => ((0, 0), 1.0),
        Some(k) => ((2, 2), 0.01 * k),
    };
    let mut wv = px_in(*w, unit, *ppi);
    let mut u = unit;
    if unit_row(
        ui,
        "Width",
        &mut wv,
        decimals,
        speed,
        true,
        Some(("new-doc-unit", &mut u, units.as_slice())),
    ) {
        *w = px_from(wv, unit, *ppi);
    }
    let mut hv = px_in(*h, unit, *ppi);
    if unit_row::<SizeUnit>(ui, "Height", &mut hv, decimals, speed, true, None) {
        *h = px_from(hv, unit, *ppi);
    }
    ui.data_mut(|d| d.insert_temp(unit_id, u));
    let before = *ppi;
    field_row(
        ui,
        "Resolution",
        egui::DragValue::new(ppi)
            .range(RESOLUTION_RANGE)
            .max_decimals(2)
            .suffix(" ppi"),
    );
    if *ppi != before && unit.per_inch().is_some() {
        // Sized in print units: the print size stays, pixels follow.
        *w = px_from(px_in(*w, unit, before), unit, *ppi);
        *h = px_from(px_in(*h, unit, before), unit, *ppi);
    }
    note(
        ui,
        &format!(
            "{} × {} pixels.\nPrints at {} ({}).",
            *w,
            *h,
            print_size_text(*w, *h, *ppi),
            print_size_cm(*w, *h, *ppi)
        ),
    );
}

impl App {
    /// Screenshot hooks (`size:...`): `size:unit=in|cm|mm|px|pct`,
    /// `size:resample=on|off`, `size:width=V` (in the shown unit) and
    /// `size:res=V` act on an open Image Size dialog; `size:new=N` picks
    /// New's preset N and `size:new-unit=in|cm|mm|px` its size unit;
    /// `size:doc-ppi=V` sets the document's resolution.
    pub(crate) fn debug_image_size(&mut self, ctx: &egui::Context, tok: &str) -> bool {
        let Some((key, val)) = tok.strip_prefix("size:").and_then(|r| r.split_once('=')) else {
            return false;
        };
        let num = val.parse::<f64>().ok();
        let unit = match val {
            "in" => SizeUnit::Inches,
            "cm" => SizeUnit::Cm,
            "mm" => SizeUnit::Mm,
            "pct" => SizeUnit::Percent,
            _ => SizeUnit::Pixels,
        };
        match (key, &mut self.dialog) {
            ("unit", Some(Dialog::ImageSize(st))) => st.unit = unit,
            ("new-unit", _) => ctx.data_mut(|d| d.insert_temp(egui::Id::new("new-doc-size-unit"), unit)),
            ("resample", Some(Dialog::ImageSize(st))) => st.set_resample(val != "off"),
            ("width", Some(Dialog::ImageSize(st))) => st.set_width(st.unit, num.unwrap_or(0.0)),
            ("res", Some(Dialog::ImageSize(st))) => st.set_resolution(st.res_unit, num.unwrap_or(0.0)),
            ("new", Some(Dialog::New(w, h, ppi))) => {
                if let Some(&(_, pw, ph, pp)) = val.parse::<usize>().ok().and_then(|i| NEW_PRESETS.get(i)) {
                    (*w, *h, *ppi) = (pw, ph, pp);
                }
            }
            ("doc-ppi", _) => {
                self.run(&lumenply_core::resolution::SetResolution {
                    ppi: num.unwrap_or(72.0) as f32,
                });
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_show_the_same_size() {
        let st = ImageSizeState::new(2400, 1600, 300.0);
        assert_eq!(st.width_in(SizeUnit::Pixels), 2400.0);
        assert_eq!(st.width_in(SizeUnit::Percent), 100.0);
        assert_eq!(st.width_in(SizeUnit::Inches), 8.0);
        assert!((st.width_in(SizeUnit::Cm) - 20.32).abs() < 1e-9);
        assert!((st.height_in(SizeUnit::Mm) - 135.4666).abs() < 1e-3);
        assert!((st.resolution_in(ResUnit::PerCm) - 118.110).abs() < 1e-3);
        assert_eq!(print_size_text(1800, 1200, 300.0), "6.00 × 4.00 in at 300 ppi");
        assert_eq!(print_size_cm(1800, 1200, 300.0), "15.24 × 10.16 cm");
        assert_eq!(fmt_ppi(72.5), "72.5");
    }

    #[test]
    fn resample_off_trades_print_size_for_resolution() {
        let mut st = ImageSizeState::new(2400, 1600, 72.0);
        st.set_resample(false);
        // 8 in wide from 2400 px: 300 ppi, height follows to 5.33 in.
        st.set_width(SizeUnit::Inches, 8.0);
        assert_eq!((st.width, st.height), (2400, 1600));
        assert_eq!(st.ppi, 300.0);
        assert!((st.height_in(SizeUnit::Inches) - 5.3333).abs() < 1e-3);
        // A resolution typed in px/cm: 118.11 px/cm ≈ 300 ppi.
        st.set_resolution(ResUnit::PerCm, 100.0);
        assert!((st.ppi - 254.0).abs() < 1e-9);
        assert_eq!((st.width, st.height), (2400, 1600), "no resampling");
        // Pixel units can't change the pixel count.
        assert!(!st.editable(SizeUnit::Pixels) && !st.editable(SizeUnit::Percent));
        st.set_width(SizeUnit::Pixels, 1000.0);
        assert_eq!(st.width, 2400);
        let cmd = st.command().unwrap();
        assert_eq!((cmd.width, cmd.height, cmd.resolution), (2400, 1600, Some(254.0)));
    }

    #[test]
    fn resample_on_changes_pixels() {
        let mut st = ImageSizeState::new(800, 600, 72.0);
        // Resolution up with the print size kept: 800 px at 72 → 300 ppi.
        st.set_resolution(ResUnit::PerInch, 300.0);
        assert_eq!((st.width, st.height), (3333, 2500));
        // 50 % of the original, aspect kept.
        st.set_width(SizeUnit::Percent, 50.0);
        assert_eq!((st.width, st.height), (400, 300));
        // 4 in at 300 ppi = 1200 px wide, 900 tall.
        st.set_width(SizeUnit::Inches, 4.0);
        assert_eq!((st.width, st.height), (1200, 900));
        // Unconstrained height edit leaves the width.
        st.constrain = false;
        st.set_height(SizeUnit::Cm, 2.54);
        assert_eq!((st.width, st.height), (1200, 300));
        let cmd = st.command().unwrap();
        assert_eq!((cmd.width, cmd.height, cmd.resolution), (1200, 300, Some(300.0)));
        // Turning Resample off restores the pixels and locks the aspect.
        st.set_resample(false);
        assert_eq!((st.width, st.height, st.constrain), (800, 600, true));
        // Garbage input is ignored; clamps hold.
        st.set_resample(true);
        st.set_width(SizeUnit::Pixels, f64::NAN);
        st.set_width(SizeUnit::Pixels, -3.0);
        assert_eq!(st.width, 800);
        st.set_width(SizeUnit::Pixels, 1e9);
        assert_eq!(st.width, MAX_SIDE);
    }

    #[test]
    fn unchanged_dialog_does_nothing() {
        let st = ImageSizeState::new(640, 480, 72.0);
        assert!(st.command().is_none());
        let mut st = ImageSizeState::new(640, 480, 72.0);
        st.set_resample(false);
        st.set_width(SizeUnit::Inches, 640.0 / 72.0);
        assert!(st.command().is_none(), "same values typed back");
    }

    /// The app on a 64×64 white document, no autosave while frames run.
    fn small() -> App {
        let mut app = App::launch(&[]);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app.open_in_new_tab(crate::blank(64, 64), None);
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
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(900.0, 600.0))),
            events,
            ..Default::default()
        };
        let _ = ctx.run(raw, |ctx| app.frame(ctx));
    }

    #[test]
    fn image_size_dialog_applies_one_step() {
        let mut app = small();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let steps = app.editor.history().len();
        // Resample off, 2 in wide: 64 px at 32 ppi, pixels untouched.
        app.run_menu_action("image-size");
        frame(&mut app, &ctx, &[]);
        if let Some(Dialog::ImageSize(st)) = &mut app.dialog {
            assert_eq!(st.orig, (64, 64, 72.0));
            st.set_resample(false);
            st.set_width(SizeUnit::Inches, 2.0);
        } else {
            panic!("Image Size did not open");
        }
        frame(&mut app, &ctx, &[Key::Enter]);
        assert!(app.dialog.is_none());
        let d = app.editor.doc();
        assert_eq!((d.width, d.height, d.resolution), (64, 64, 32.0));
        assert_eq!(app.editor.history().len(), steps + 1);
        assert_eq!(app.editor.history().last().copied(), Some("Image size"));
        // Resample on, resolution doubled with the 2 in print size kept.
        app.run_menu_action("image-size");
        frame(&mut app, &ctx, &[]);
        if let Some(Dialog::ImageSize(st)) = &mut app.dialog {
            st.set_resolution(ResUnit::PerInch, 64.0);
        }
        frame(&mut app, &ctx, &[Key::Enter]);
        let d = app.editor.doc();
        assert_eq!((d.width, d.height, d.resolution), (128, 128, 64.0));
        assert_eq!(d.print_size_inches(), (2.0, 2.0));
        // OK with nothing changed adds no step.
        let steps = app.editor.history().len();
        app.run_menu_action("image-size");
        frame(&mut app, &ctx, &[]);
        frame(&mut app, &ctx, &[Key::Enter]);
        assert!(app.dialog.is_none());
        assert_eq!(app.editor.history().len(), steps);
        // View ▸ Print size: an inch of document per screen inch.
        app.run_menu_action("print-size");
        frame(&mut app, &ctx, &[]);
        let want = screen_points_per_inch() / 64.0;
        assert!((app.zoom - want).abs() < 1e-6, "{} vs {want}", app.zoom);
        // File ▸ New at 300 ppi opens a 300 ppi document.
        app.dialog = Some(Dialog::New(1200, 1800, 300.0));
        frame(&mut app, &ctx, &[]);
        frame(&mut app, &ctx, &[Key::Enter]);
        let d = app.editor.doc();
        assert_eq!((d.width, d.height, d.resolution), (1200, 1800, 300.0));
    }

    #[test]
    fn print_size_zoom_maps_an_inch_to_an_inch() {
        let sppi = screen_points_per_inch();
        assert_eq!(print_size_zoom(sppi), 1.0);
        assert_eq!(print_size_zoom(sppi * 2.0), 0.5);
        assert_eq!(print_size_zoom(30_000.0), 0.05, "clamped like other zooms");
    }

    #[test]
    fn new_document_print_units() {
        assert_eq!(px_in(2550, SizeUnit::Inches, 300.0), 8.5);
        assert_eq!(px_from(8.5, SizeUnit::Inches, 300.0), 2550);
        // 210 × 297 mm at 300 ppi: A4's 2480 × 3508.
        assert_eq!(px_from(210.0, SizeUnit::Mm, 300.0), 2480);
        assert_eq!(px_from(29.7, SizeUnit::Cm, 300.0), 3508);
        assert_eq!(px_from(1920.0, SizeUnit::Pixels, 72.0), 1920);
        assert_eq!(px_from(-1.0, SizeUnit::Inches, 300.0), 1);
        // A print-sized document keeps its inches when the ppi changes.
        assert_eq!(
            px_from(px_in(2550, SizeUnit::Inches, 300.0), SizeUnit::Inches, 150.0),
            1275
        );
    }

    #[test]
    fn new_presets() {
        let a4 = NEW_PRESETS.iter().find(|p| p.0.starts_with("A4")).unwrap();
        // 210 mm at 300 ppi = 2480.3 px.
        assert_eq!((a4.1, a4.2), (2480, 3508));
        assert_eq!(matching_preset(2550, 3300, 300.0), Some(5));
        assert_eq!(matching_preset(2550, 3300, 72.0), None);
        let ed = blank_at(20, 10, 300.0);
        assert_eq!((ed.doc().width, ed.doc().resolution), (20, 300.0));
        assert_eq!(ed.doc().layer_count(), 1);
        assert!(ed.history().is_empty());
    }
}
