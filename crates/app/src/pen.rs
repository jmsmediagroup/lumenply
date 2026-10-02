//! Pen pressure. egui does not report it, so it is read per platform:
//! a Windows pen arrives as touches whose `force` is the pressure; on macOS
//! an AppKit local event monitor reads `pressure` from tablet mouse events.
//! A mouse reports nothing, so its strokes stay at full size and opacity.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

static PRESSURE: AtomicU32 = AtomicU32::new(0x3F80_0000); // 1.0
static FROM_PEN: AtomicBool = AtomicBool::new(false);

fn store(p: f32) {
    PRESSURE.store(p.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    FROM_PEN.store(true, Ordering::Relaxed);
}

/// The pen's current pressure (0..=1), or `None` when the pointer is a
/// mouse or trackpad.
pub(crate) fn pressure(ctx: &egui::Context) -> Option<f32> {
    ctx.input(|i| {
        for e in &i.events {
            if let egui::Event::Touch { force: Some(f), .. } = e {
                store(*f);
            }
        }
    });
    FROM_PEN
        .load(Ordering::Relaxed)
        .then(|| f32::from_bits(PRESSURE.load(Ordering::Relaxed)))
}

/// A stroke point's (size, opacity) factors for `pressure` under the
/// Brush bar's two toggles.
pub(crate) fn factors(pressure: Option<f32>, to_size: bool, to_opacity: bool) -> (f32, f32) {
    let p = pressure.unwrap_or(1.0);
    (if to_size { p } else { 1.0 }, if to_opacity { p } else { 1.0 })
}

impl App {
    /// A stroke point at document `(x, y)` carrying the pen's pressure as
    /// the Brush bar's toggles route it.
    pub(crate) fn pen_point(&self, ctx: &egui::Context, x: f32, y: f32) -> StrokePoint {
        let (size, opacity) = factors(pressure(ctx), self.prefs.pen_size, self.prefs.pen_opacity);
        StrokePoint::new(x, y, size).with_opacity(opacity)
    }
}

/// Photoshop's pressure button beside a brush slider: a pen nib that wears
/// the accent while pressure drives the setting. Returns true on toggle.
pub(crate) fn toggle(ui: &mut egui::Ui, on: &mut bool, name: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), Sense::click());
    let fill = if *on {
        ACCENT
    } else if resp.hovered() {
        HOVER
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, RADIUS, fill);
    let ink = if *on { ACCENT_INK } else { MUTED };
    let s = Stroke::new(1.4, ink);
    let c = rect.center();
    // A nib pointing down-left, with a pressure line under its tip.
    let tip = c + egui::vec2(-5.0, 5.0);
    let back = c + egui::vec2(4.0, -4.0);
    let half = egui::vec2(1.8, 1.8);
    let p = ui.painter();
    p.line_segment([tip, back + half], s);
    p.line_segment([tip, back - half], s);
    p.line_segment([back + half, back - half], s);
    p.line_segment([tip + egui::vec2(3.0, 3.5), tip + egui::vec2(8.0, 3.5)], s);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, *on, name));
    let clicked = resp.on_hover_text(name).clicked();
    if clicked {
        *on = !*on;
    }
    clicked
}

/// Starts listening for tablet events (macOS). Call once on the main
/// thread after the application exists.
#[cfg(target_os = "macos")]
pub(crate) fn install() {
    use objc2_app_kit::{NSEvent, NSEventMask, NSEventSubtype, NSEventType};
    use std::ptr::NonNull;
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let mask = NSEventMask::LeftMouseDown
            | NSEventMask::LeftMouseDragged
            | NSEventMask::TabletPoint
            | NSEventMask::TabletProximity;
        let block = block2::RcBlock::new(|ev: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: AppKit hands the monitor a live event.
            let e = unsafe { ev.as_ref() };
            let ty = e.r#type();
            if ty == NSEventType::TabletProximity {
                FROM_PEN.store(e.isEnteringProximity(), Ordering::Relaxed);
            } else if ty == NSEventType::TabletPoint {
                store(e.pressure());
            } else if ty == NSEventType::LeftMouseDown || ty == NSEventType::LeftMouseDragged {
                // `subtype` is valid for mouse events: a tablet's carry
                // its pressure, a mouse's mean the pen is not in use.
                if e.subtype() == NSEventSubtype::TabletPoint {
                    store(e.pressure());
                } else {
                    FROM_PEN.store(false, Ordering::Relaxed);
                }
            }
            ev.as_ptr()
        });
        // SAFETY: the block returns the event it was given, never null.
        let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block) };
        // The monitor lives as long as the app.
        std::mem::forget(monitor);
    });
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn install() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggles_route_pressure_to_size_and_opacity() {
        assert_eq!(factors(Some(0.25), true, false), (0.25, 1.0));
        assert_eq!(factors(Some(0.25), false, true), (1.0, 0.25));
        assert_eq!(factors(Some(0.25), true, true), (0.25, 0.25));
        // A mouse paints at full size and opacity whatever the toggles say.
        assert_eq!(factors(None, true, true), (1.0, 1.0));
    }

    #[test]
    fn a_pen_touch_reports_its_force() {
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Touch {
            device_id: egui::TouchDeviceId(1),
            id: egui::TouchId(1),
            phase: egui::TouchPhase::Move,
            pos: egui::pos2(10.0, 10.0),
            force: Some(0.4),
        });
        let mut got = None;
        let _ = ctx.run(raw, |ctx| got = pressure(ctx));
        assert_eq!(got, Some(0.4));
        FROM_PEN.store(false, Ordering::Relaxed);
    }
}
