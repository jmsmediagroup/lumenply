use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Tool {
    Heal,
    Pen,
    Move,
    RectSelect,
    EllipseSelect,
    Lasso,
    PolyLasso,
    Wand,
    Brush,
    Eraser,
    Clone,
    Bucket,
    Gradient,
    Text,
    Eyedropper,
    Hand,
}

impl Tool {
    pub(crate) const ALL: [Tool; 16] = [
        Tool::Move,
        Tool::RectSelect,
        Tool::EllipseSelect,
        Tool::Lasso,
        Tool::PolyLasso,
        Tool::Wand,
        Tool::Brush,
        Tool::Eraser,
        Tool::Clone,
        Tool::Heal,
        Tool::Bucket,
        Tool::Gradient,
        Tool::Pen,
        Tool::Text,
        Tool::Eyedropper,
        Tool::Hand,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Tool::Move => "Move",
            Tool::Brush => "Brush",
            Tool::Eraser => "Eraser",
            Tool::Clone => "Clone Stamp",
            Tool::Heal => "Healing Brush",
            Tool::Pen => "Pen",
            Tool::Bucket => "Paint Bucket",
            Tool::Gradient => "Gradient",
            Tool::Text => "Text",
            Tool::Eyedropper => "Eyedropper",
            Tool::RectSelect => "Rectangular Marquee",
            Tool::EllipseSelect => "Elliptical Marquee",
            Tool::Lasso => "Lasso",
            Tool::PolyLasso => "Polygonal Lasso",
            Tool::Wand => "Magic Wand",
            Tool::Hand => "Hand",
        }
    }

    /// The shortcut letter shown on the tool rail.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Tool::Move => "V",
            Tool::Brush => "B",
            Tool::Eraser => "E",
            Tool::Clone => "S",
            Tool::Heal => "J",
            Tool::Pen => "P",
            Tool::Bucket | Tool::Gradient => "G",
            Tool::Text => "T",
            Tool::Eyedropper => "I",
            Tool::RectSelect | Tool::EllipseSelect => "M",
            Tool::Lasso | Tool::PolyLasso => "L",
            Tool::Wand => "W",
            Tool::Hand => "H",
        }
    }

    pub(crate) fn tip(self) -> &'static str {
        match self {
            Tool::Move => "Move (V)",
            Tool::Brush => "Brush (B)",
            Tool::Eraser => "Eraser (E)",
            Tool::Clone => "Clone Stamp (S) — Alt+click or 'Pick source' to set the source",
            Tool::Heal => {
                "Healing Brush (J) — paints surroundings over blemishes; Alt+click sets a texture source"
            }
            Tool::Pen => {
                "Pen (P) — click for corners, drag for curves; click the first point to close; Enter finishes"
            }
            Tool::Bucket => "Paint Bucket (G)",
            Tool::Gradient => "Gradient (Shift+G)",
            Tool::Text => "Text (T)",
            Tool::Eyedropper => "Eyedropper (I)",
            Tool::RectSelect => "Rectangular Marquee (M)",
            Tool::EllipseSelect => "Elliptical Marquee (Shift+M)",
            Tool::Lasso => "Lasso (L)",
            Tool::PolyLasso => "Polygonal Lasso (Shift+L)",
            Tool::Wand => "Magic Wand (W)",
            Tool::Hand => "Hand (H)",
        }
    }
}

/// Rail button size, the gap between buttons, and the rail's padding.
const BTN: f32 = 34.0;
const GAP: f32 = 3.0;
const PAD: f32 = 9.0;
/// Height of a family separator.
const SEP: f32 = 7.0;
/// Height kept free at the foot of the rail for the colour well.
const WELL_H: f32 = 56.0;

/// Tool families, separated by a hairline on the rail: move / select /
/// paint / type & sample / navigate.
fn starts_family(tool: Tool) -> bool {
    matches!(tool, Tool::RectSelect | Tool::Brush | Tool::Text | Tool::Hand)
}

/// Columns the rail needs to show every tool and the colour well in a
/// column `height` tall: one when it fits, else two.
fn rail_columns(height: f32) -> usize {
    let n = Tool::ALL.len() as f32;
    let seps = Tool::ALL.into_iter().filter(|t| starts_family(*t)).count() as f32;
    let one_col = n * BTN + seps * SEP + (n + seps - 1.0) * GAP + WELL_H + 2.0 * PAD;
    if height >= one_col {
        1
    } else {
        2
    }
}

/// The rail's panel width for `cols` columns of buttons.
fn rail_width(cols: usize) -> f32 {
    2.0 * PAD + cols as f32 * BTN + (cols as f32 - 1.0) * GAP
}

/// A hairline between tool families.
fn rail_separator(ui: &mut egui::Ui) {
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(egui::vec2(w, SEP), Sense::hover());
    ui.painter().hline(
        r.min.x + 6.0..=r.max.x - 6.0,
        r.center().y,
        Stroke::new(1.0, LINE),
    );
}

impl App {
    /// The tool rail. One column when the window is tall enough; two
    /// columns (packed in rail order) when it isn't; and a scroll area as
    /// the last resort, so every tool stays reachable at any height.
    pub(crate) fn tool_palette(&mut self, ctx: &egui::Context) {
        let cols = rail_columns(ctx.available_rect().height());
        let width = rail_width(cols);
        egui::SidePanel::left("tools")
            .exact_width(width)
            .resizable(false)
            .frame(egui::Frame::none().fill(PANEL).inner_margin(PAD))
            .show(ctx, |ui| {
                let tools_h = (ui.available_height() - WELL_H).max(BTN);
                egui::ScrollArea::vertical()
                    .id_salt("tool-rail")
                    .max_height(tools_h)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(GAP, GAP);
                        if cols == 1 {
                            for tool in Tool::ALL {
                                if starts_family(tool) {
                                    rail_separator(ui);
                                }
                                self.rail_button(ui, tool);
                            }
                        } else {
                            // Two columns, packed; the select family ends a
                            // row exactly, so it keeps its separator.
                            for (row, pair) in Tool::ALL.chunks(2).enumerate() {
                                if row == 3 {
                                    rail_separator(ui);
                                }
                                ui.horizontal(|ui| {
                                    for tool in pair {
                                        self.rail_button(ui, *tool);
                                    }
                                });
                            }
                        }
                    });
                ui.vertical_centered(|ui| self.color_well(ui));
            });
    }

    /// One tool button: icon, shortcut letter in the corner, accent fill
    /// when active, hover fill, keyboard focus ring.
    fn rail_button(&mut self, ui: &mut egui::Ui, tool: Tool) {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(BTN), Sense::hover());
        let resp = ui.interact(rect, rail_id(tool), Sense::click());
        let active = self.tool == tool;
        let bg = if active {
            ACCENT
        } else if resp.hovered() || resp.has_focus() {
            HOVER
        } else {
            PANEL
        };
        ui.painter().rect_filled(rect, RADIUS, bg);
        focus_ring(ui, &resp, rect, RADIUS);
        let ink = if active { ACCENT_INK } else { TEXT };
        // The icon sits a touch up-left so the letter owns the corner.
        let icon = egui::Rect::from_center_size(rect.center() - egui::vec2(1.5, 1.5), Vec2::splat(16.0));
        draw_icon(ui.painter(), icon, tool, ink);
        ui.painter().text(
            rect.right_bottom() + egui::vec2(-3.5, -1.5),
            Align2::RIGHT_BOTTOM,
            tool.key(),
            FontId::monospace(9.0),
            if active { ACCENT_INK } else { MUTED },
        );
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, active, tool.name())
        });
        if resp.on_hover_text(tool.tip()).clicked() {
            self.tool = tool;
            self.lasso.clear();
            self.pen_open = false;
            self.editor.end_coalescing();
            if tool != Tool::Move {
                self.cancel_free_transform();
            }
        }
    }

    /// The foreground/background colour well at the foot of the rail:
    /// two overlapping swatches (click to edit), a swap arrow in the free
    /// top-right corner (X) and a mini reset in the bottom-left (D) —
    /// the classic layout in one compact block.
    fn color_well(&mut self, ui: &mut egui::Ui) {
        ui.add_space((ui.available_height() - 48.0).max(4.0));
        let (rect, _) = ui.allocate_exact_size(egui::vec2(40.0, 44.0), Sense::hover());
        // Swap, in the free top-right corner.
        let swap_rect = egui::Rect::from_min_size(rect.min + egui::vec2(28.0, 0.0), Vec2::splat(12.0));
        let swap = ui.interact(swap_rect, ui.id().with("swap-colors"), Sense::click());
        {
            let p = ui.painter();
            let st = Stroke::new(1.3, MUTED);
            let c = swap_rect.center();
            p.line_segment([c + egui::vec2(-5.0, 2.0), c + egui::vec2(5.0, 2.0)], st);
            p.line_segment([c + egui::vec2(-5.0, -2.0), c + egui::vec2(5.0, -2.0)], st);
            p.line_segment([c + egui::vec2(5.0, 2.0), c + egui::vec2(2.0, 5.0)], st);
            p.line_segment([c + egui::vec2(-5.0, -2.0), c + egui::vec2(-2.0, -5.0)], st);
        }
        if swap.on_hover_text("Swap colours (X)").clicked() {
            std::mem::swap(&mut self.brush_rgb, &mut self.bg_rgb);
        }
        // Reset to black over white, in the free bottom-left corner.
        let reset_rect = egui::Rect::from_min_size(rect.min + egui::vec2(0.0, 32.0), Vec2::splat(12.0));
        let reset = ui.interact(reset_rect, ui.id().with("reset-colors"), Sense::click());
        {
            let p = ui.painter();
            let a = egui::Rect::from_min_size(reset_rect.min, Vec2::splat(7.0));
            let b = egui::Rect::from_min_size(reset_rect.min + egui::vec2(4.0, 4.0), Vec2::splat(7.0));
            p.rect_filled(b, 1.5, Color32::WHITE);
            p.rect_stroke(b, 1.5, Stroke::new(1.0, MUTED));
            p.rect_filled(a, 1.5, Color32::BLACK);
            p.rect_stroke(a, 1.5, Stroke::new(1.0, MUTED));
        }
        if reset.on_hover_text("Default colours (D)").clicked() {
            self.brush_rgb = [0.0; 3];
            self.bg_rgb = [1.0; 3];
        }
        let to32 = |c: [f32; 3]| {
            Color32::from_rgb(
                (c[0] * 255.0 + 0.5) as u8,
                (c[1] * 255.0 + 0.5) as u8,
                (c[2] * 255.0 + 0.5) as u8,
            )
        };
        let bg_rect = egui::Rect::from_min_size(rect.min + egui::vec2(14.0, 18.0), Vec2::splat(26.0));
        let fg_rect = egui::Rect::from_min_size(rect.min + egui::vec2(0.0, 2.0), Vec2::splat(26.0));
        let bg_resp = ui.interact(bg_rect, ui.id().with("bg-well"), Sense::click());
        let p = ui.painter();
        p.rect_filled(bg_rect, 4.0, to32(self.bg_rgb));
        p.rect_stroke(bg_rect, 4.0, Stroke::new(1.0, LINE));
        let fg_resp = ui.interact(fg_rect, ui.id().with("fg-well"), Sense::click());
        p.rect_filled(fg_rect, 4.0, to32(self.brush_rgb));
        p.rect_stroke(fg_rect.expand(1.0), 5.0, Stroke::new(2.0, PANEL));
        p.rect_stroke(fg_rect, 4.0, Stroke::new(1.0, LINE));
        color_popup(
            ui,
            &fg_resp.on_hover_text("Brush colour"),
            "fg-pick",
            &mut self.brush_rgb,
        );
        color_popup(
            ui,
            &bg_resp.on_hover_text("Background colour"),
            "bg-pick",
            &mut self.bg_rgb,
        );
    }
}

/// The rail button's widget id, stable so focus can be requested on it.
pub(crate) fn rail_id(tool: Tool) -> egui::Id {
    egui::Id::new(("tool-rail", tool.name()))
}

/// A colour-picker popup anchored to a painted swatch.
fn color_popup(ui: &mut egui::Ui, resp: &egui::Response, id: &str, rgb: &mut [f32; 3]) {
    let popup = ui.make_persistent_id(id);
    if resp.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup));
    }
    egui::popup::popup_above_or_below_widget(
        ui,
        popup,
        resp,
        egui::AboveOrBelow::Above,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            ui.set_min_width(220.0);
            let mut c = Color32::from_rgb(
                (rgb[0] * 255.0 + 0.5) as u8,
                (rgb[1] * 255.0 + 0.5) as u8,
                (rgb[2] * 255.0 + 0.5) as u8,
            );
            if egui::color_picker::color_picker_color32(ui, &mut c, egui::color_picker::Alpha::Opaque) {
                *rgb = [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0];
            }
        },
    );
}

/// Draw a tool's icon into `r` (nominally 16 × 16) in ink `c`. Every
/// icon is an outline on a 16-unit grid with the same 1.5 px stroke, so
/// the rail reads as one family; only the gradient ramp is filled.
pub(crate) fn draw_icon(p: &egui::Painter, r: egui::Rect, tool: Tool, c: Color32) {
    let k = r.width() / 16.0;
    // Grid point (x, y) in 0..16 design units.
    let g = |x: f32, y: f32| r.min + egui::vec2(x * k, y * k);
    let s = Stroke::new(1.5, c);
    let line = |pts: &[(f32, f32)]| Shape::line(pts.iter().map(|&(x, y)| g(x, y)).collect(), s);
    let closed = |pts: &[(f32, f32)]| Shape::closed_line(pts.iter().map(|&(x, y)| g(x, y)).collect(), s);
    match tool {
        Tool::Move => {
            p.add(line(&[(8.0, 1.0), (8.0, 15.0)]));
            p.add(line(&[(1.0, 8.0), (15.0, 8.0)]));
            p.add(line(&[(5.8, 3.2), (8.0, 1.0), (10.2, 3.2)]));
            p.add(line(&[(5.8, 12.8), (8.0, 15.0), (10.2, 12.8)]));
            p.add(line(&[(3.2, 5.8), (1.0, 8.0), (3.2, 10.2)]));
            p.add(line(&[(12.8, 5.8), (15.0, 8.0), (12.8, 10.2)]));
        }
        Tool::RectSelect => {
            let pts = [
                g(1.5, 2.5),
                g(14.5, 2.5),
                g(14.5, 13.5),
                g(1.5, 13.5),
                g(1.5, 2.5),
            ];
            p.extend(Shape::dashed_line(&pts, s, 2.6 * k, 2.0 * k));
        }
        Tool::EllipseSelect => {
            let e = egui::Rect::from_min_max(g(1.0, 2.5), g(15.0, 13.5));
            p.extend(Shape::dashed_line(&ellipse_points(e, 40), s, 2.6 * k, 2.0 * k));
        }
        Tool::Lasso => {
            let pts: Vec<Pos2> = (0..=28)
                .map(|i| {
                    let t = i as f32 / 28.0 * std::f32::consts::TAU;
                    g(8.0 + t.cos() * 6.8, 6.5 + t.sin() * 4.6)
                })
                .collect();
            p.extend(Shape::dashed_line(&pts, s, 2.6 * k, 2.0 * k));
            // The rope's tail with its knot.
            p.add(line(&[(5.5, 10.8), (4.5, 13.0), (6.0, 15.0)]));
        }
        Tool::PolyLasso => {
            let pts = [
                g(1.5, 5.0),
                g(9.0, 1.5),
                g(14.5, 7.0),
                g(10.0, 14.5),
                g(3.5, 11.5),
                g(1.5, 5.0),
            ];
            p.extend(Shape::dashed_line(&pts, s, 2.6 * k, 2.0 * k));
        }
        Tool::Wand => {
            p.add(line(&[(1.5, 14.5), (9.5, 6.5)]));
            // Sparkles: one star and two glints.
            p.add(line(&[(12.0, 1.0), (12.0, 6.0)]));
            p.add(line(&[(9.5, 3.5), (14.5, 3.5)]));
            p.add(line(&[(15.0, 8.8), (15.0, 9.2)]));
            p.add(line(&[(6.8, 1.3), (6.8, 1.7)]));
        }
        Tool::Brush => {
            // Handle, ferrule, and a bristle tip sweeping to a point.
            p.add(line(&[(14.5, 1.5), (9.0, 7.0)]));
            p.add(closed(&[
                (7.6, 5.9),
                (10.1, 8.4),
                (8.9, 11.4),
                (5.8, 13.9),
                (1.5, 14.5),
                (2.3, 10.4),
                (4.9, 7.4),
            ]));
        }
        Tool::Eraser => {
            // A tilted block with its rubber tip split off, on a baseline.
            p.add(closed(&[(1.5, 10.0), (8.0, 3.5), (13.5, 9.0), (7.0, 15.0)]));
            p.add(line(&[(4.7, 6.8), (10.2, 12.2)]));
            p.add(line(&[(9.0, 15.0), (15.0, 15.0)]));
        }
        Tool::Clone => {
            // A rubber stamp: knob, neck, block, pad.
            p.circle_stroke(g(8.0, 3.5), 2.5 * k, s);
            p.add(line(&[(6.8, 6.0), (6.8, 9.0)]));
            p.add(line(&[(9.2, 6.0), (9.2, 9.0)]));
            p.add(closed(&[(2.0, 9.0), (14.0, 9.0), (14.0, 12.5), (2.0, 12.5)]));
            p.add(line(&[(3.0, 15.0), (13.0, 15.0)]));
        }
        Tool::Heal => {
            // A sticking plaster: a diagonal capsule with a pad of dots.
            let a = egui::vec2(3.5, 12.5);
            let b = egui::vec2(12.5, 3.5);
            let d = (b - a).normalized();
            let n = egui::vec2(-d.y, d.x) * 3.2;
            p.add(closed(&[
                ((a + n).x, (a + n).y),
                ((b + n).x, (b + n).y),
                ((b - n).x, (b - n).y),
                ((a - n).x, (a - n).y),
            ]));
            // The healing cross on the pad.
            p.add(line(&[(6.4, 8.0), (9.6, 8.0)]));
            p.add(line(&[(8.0, 6.4), (8.0, 9.6)]));
        }
        Tool::Bucket => {
            // A tipped bucket pouring a drop.
            p.add(closed(&[(1.5, 7.0), (7.0, 1.5), (12.5, 7.0), (7.0, 12.5)]));
            p.add(line(&[(4.0, 7.0), (12.5, 7.0)]));
            p.add(line(&[(12.5, 7.0), (14.3, 10.5)]));
            p.circle_filled(g(14.3, 12.6), 1.4 * k, c);
        }
        Tool::Gradient => {
            let well = egui::Rect::from_min_max(g(1.5, 3.5), g(14.5, 12.5));
            let steps = 6;
            for i in 0..steps {
                let t = i as f32 / (steps - 1) as f32;
                let x0 = well.min.x + well.width() * i as f32 / steps as f32;
                let x1 = well.min.x + well.width() * (i + 1) as f32 / steps as f32;
                let shade = (255.0 * (1.0 - t * 0.85)) as u8;
                p.rect_filled(
                    egui::Rect::from_min_max(egui::pos2(x0, well.min.y), egui::pos2(x1, well.max.y)),
                    0.0,
                    Color32::from_gray(shade),
                );
            }
            p.rect_stroke(well, 1.5, s);
        }
        Tool::Pen => {
            // A nib: shoulders, pointed tip, slit and breather hole.
            p.add(closed(&[
                (8.0, 15.0),
                (3.0, 8.0),
                (5.5, 3.5),
                (10.5, 3.5),
                (13.0, 8.0),
            ]));
            p.add(line(&[(8.0, 15.0), (8.0, 10.0)]));
            p.circle_stroke(g(8.0, 8.6), 1.3 * k, s);
            p.add(line(&[(5.5, 1.0), (10.5, 1.0)]));
        }
        Tool::Text => {
            p.add(line(&[(3.0, 4.5), (3.0, 2.0), (13.0, 2.0), (13.0, 4.5)]));
            p.add(line(&[(8.0, 2.0), (8.0, 14.5)]));
            p.add(line(&[(5.5, 14.5), (10.5, 14.5)]));
        }
        Tool::Eyedropper => {
            // A pipette: a slim glass tube, a collar, and a squeeze bulb
            // continuing the same diagonal, with the tip at bottom left.
            let quad = |a: egui::Vec2, b: egui::Vec2, half: f32, cap: f32| {
                let d = (b - a).normalized();
                let n = egui::vec2(-d.y, d.x) * half;
                let tip = b + d * cap;
                closed(&[
                    ((a + n).x, (a + n).y),
                    ((b + n).x, (b + n).y),
                    (tip.x, tip.y),
                    ((b - n).x, (b - n).y),
                    ((a - n).x, (a - n).y),
                ])
            };
            p.add(quad(egui::vec2(3.6, 12.4), egui::vec2(9.0, 7.0), 1.2, 0.0));
            p.add(line(&[(7.9, 4.9), (11.1, 8.1)]));
            p.add(quad(egui::vec2(10.6, 5.4), egui::vec2(13.0, 3.0), 2.0, 1.6));
            p.add(line(&[(3.6, 12.4), (1.3, 14.7)]));
        }
        Tool::Hand => {
            // An open hand: thumb, four fingers, palm.
            p.add(line(&[
                (5.5, 15.0),
                (2.8, 11.2),
                (1.4, 8.6),
                (2.0, 7.6),
                (3.2, 7.8),
                (5.0, 9.8),
                (5.0, 3.6),
                (6.0, 2.6),
                (7.0, 3.6),
                (7.0, 8.0),
                (7.0, 2.0),
                (8.0, 1.0),
                (9.0, 2.0),
                (9.0, 8.0),
                (9.0, 2.8),
                (10.0, 1.8),
                (11.0, 2.8),
                (11.0, 8.4),
                (11.0, 4.6),
                (12.0, 3.6),
                (13.0, 4.6),
                (13.0, 11.0),
                (11.8, 15.0),
                (5.5, 15.0),
            ]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rail_folds_to_two_columns_when_short() {
        // 16 buttons, 4 family separators, gaps, colour well and padding.
        let one_col = 16.0 * BTN + 4.0 * SEP + 19.0 * GAP + WELL_H + 2.0 * PAD;
        assert_eq!(one_col, 703.0);
        assert_eq!(rail_columns(703.0), 1);
        assert_eq!(rail_columns(702.0), 2);
        assert_eq!(rail_columns(430.0), 2);
        assert_eq!(rail_width(1), 52.0);
        assert_eq!(rail_width(2), 89.0);
    }

    #[test]
    fn every_tool_has_a_distinct_rail_id() {
        let ids: std::collections::HashSet<egui::Id> = Tool::ALL.into_iter().map(rail_id).collect();
        assert_eq!(ids.len(), 16);
    }
}
