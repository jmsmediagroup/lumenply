use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Tool {
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
    const ALL: [Tool; 14] = [
        Tool::Move,
        Tool::RectSelect,
        Tool::EllipseSelect,
        Tool::Lasso,
        Tool::PolyLasso,
        Tool::Wand,
        Tool::Brush,
        Tool::Eraser,
        Tool::Clone,
        Tool::Bucket,
        Tool::Gradient,
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

impl App {
    pub(crate) fn tool_palette(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("tools")
            .exact_width(58.0)
            .resizable(false)
            .frame(egui::Frame::none().fill(PANEL).inner_margin(9.0))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 5.0;
                for tool in Tool::ALL {
                    // A breath between tool families: move / select / paint /
                    // type & sample / navigate.
                    if matches!(tool, Tool::RectSelect | Tool::Brush | Tool::Text | Tool::Hand) {
                        ui.add_space(3.0);
                        let (r, _) = ui.allocate_exact_size(egui::vec2(40.0, 1.0), Sense::hover());
                        ui.painter().hline(
                            r.min.x + 6.0..=r.max.x - 6.0,
                            r.center().y,
                            Stroke::new(1.0, LINE),
                        );
                        ui.add_space(3.0);
                    }
                    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::click());
                    let active = self.tool == tool;
                    let bg = if active {
                        ACCENT
                    } else if resp.hovered() {
                        Color32::from_rgb(0x32, 0x38, 0x3F)
                    } else {
                        PANEL
                    };
                    ui.painter().rect_filled(rect, 6.0, bg);
                    let ink = if active { ACCENT_INK } else { TEXT };
                    draw_icon(ui.painter(), rect.shrink(11.0), tool, ink);
                    ui.painter().text(
                        rect.right_bottom() + egui::vec2(-4.0, -2.0),
                        Align2::RIGHT_BOTTOM,
                        tool.key(),
                        FontId::monospace(8.5),
                        if active { ACCENT_INK } else { MUTED },
                    );
                    if resp.on_hover_text(tool.tip()).clicked() {
                        self.tool = tool;
                        self.lasso.clear();
                        if tool != Tool::Move {
                            self.cancel_free_transform();
                        }
                    }
                }
                self.color_well(ui);
            });
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

pub(crate) fn draw_icon(p: &egui::Painter, r: egui::Rect, tool: Tool, c: Color32) {
    let s = Stroke::new(1.6, c);
    match tool {
        Tool::Brush => {
            p.line_segment(
                [
                    egui::pos2(r.min.x + 1.0, r.max.y - 1.0),
                    egui::pos2(r.center().x + 2.0, r.center().y - 2.0),
                ],
                Stroke::new(3.0, c),
            );
            p.circle_filled(egui::pos2(r.max.x - 4.0, r.min.y + 4.0), 4.5, c);
        }
        Tool::RectSelect => {
            let pts = [
                r.left_top(),
                r.right_top(),
                r.right_bottom(),
                r.left_bottom(),
                r.left_top(),
            ];
            p.extend(Shape::dashed_line(&pts, s, 3.0, 2.5));
        }
        Tool::EllipseSelect => {
            p.extend(Shape::dashed_line(&ellipse_points(r, 40), s, 3.0, 2.5));
        }
        Tool::Hand => {
            let ctr = r.center();
            let h = r.width() / 2.0;
            for v in [
                egui::vec2(0.0, -h),
                egui::vec2(0.0, h),
                egui::vec2(-h, 0.0),
                egui::vec2(h, 0.0),
            ] {
                p.arrow(ctr, v, s);
            }
        }
        Tool::Move => {
            let pts = vec![
                egui::pos2(r.min.x + 2.0, r.min.y),
                egui::pos2(r.min.x + 2.0, r.max.y - 3.0),
                egui::pos2(r.min.x + 7.0, r.max.y - 8.0),
                egui::pos2(r.min.x + 10.0, r.max.y),
                egui::pos2(r.min.x + 13.0, r.max.y - 2.0),
                egui::pos2(r.min.x + 10.0, r.max.y - 9.0),
                egui::pos2(r.max.x, r.max.y - 9.0),
            ];
            p.add(Shape::convex_polygon(pts, c, Stroke::NONE));
        }
        Tool::Eraser => {
            let ctr = r.center();
            let a = egui::pos2(ctr.x - 6.0, ctr.y + 6.0);
            let b = egui::pos2(ctr.x + 6.0, ctr.y - 6.0);
            p.line_segment([a, b], Stroke::new(7.0, c));
            p.line_segment(
                [
                    egui::pos2(ctr.x - 8.0, ctr.y + 10.0),
                    egui::pos2(ctr.x + 1.0, ctr.y + 10.0),
                ],
                s,
            );
        }
        Tool::Eyedropper => {
            let ctr = r.center();
            p.line_segment(
                [
                    egui::pos2(ctr.x - 7.0, ctr.y + 7.0),
                    egui::pos2(ctr.x + 3.0, ctr.y - 3.0),
                ],
                Stroke::new(2.5, c),
            );
            p.circle_filled(egui::pos2(ctr.x + 5.0, ctr.y - 5.0), 4.0, c);
        }
        Tool::Clone => {
            let ctr = r.center();
            p.rect_filled(
                egui::Rect::from_center_size(egui::pos2(ctr.x, ctr.y - 6.0), egui::vec2(6.0, 8.0)),
                2.0,
                c,
            );
            p.rect_filled(
                egui::Rect::from_center_size(egui::pos2(ctr.x, ctr.y + 1.0), egui::vec2(3.0, 6.0)),
                0.0,
                c,
            );
            p.rect_filled(
                egui::Rect::from_center_size(egui::pos2(ctr.x, ctr.y + 7.0), egui::vec2(18.0, 6.0)),
                2.0,
                c,
            );
        }
        Tool::Text => {
            p.text(
                r.center(),
                Align2::CENTER_CENTER,
                "T",
                FontId::proportional(20.0),
                c,
            );
        }
        Tool::Bucket => {
            let ctr = r.center();
            let pts = vec![
                egui::pos2(ctr.x - 8.0, ctr.y - 2.0),
                egui::pos2(ctr.x + 1.0, ctr.y - 9.0),
                egui::pos2(ctr.x + 8.0, ctr.y - 1.0),
                egui::pos2(ctr.x - 1.0, ctr.y + 7.0),
            ];
            p.add(Shape::convex_polygon(pts, c, Stroke::NONE));
            p.circle_filled(egui::pos2(ctr.x + 7.0, ctr.y + 7.0), 2.5, c);
        }
        Tool::Gradient => {
            let steps = 6;
            for i in 0..steps {
                let t = i as f32 / (steps - 1) as f32;
                let x0 = r.min.x + r.width() * i as f32 / steps as f32;
                let x1 = r.min.x + r.width() * (i + 1) as f32 / steps as f32;
                let g = (255.0 * (1.0 - t * 0.8)) as u8;
                p.rect_filled(
                    egui::Rect::from_min_max(egui::pos2(x0, r.min.y + 2.0), egui::pos2(x1, r.max.y - 2.0)),
                    0.0,
                    Color32::from_gray(g),
                );
            }
        }
        Tool::Lasso => {
            let ctr = r.center();
            let pts: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let t = i as f32 / 24.0 * std::f32::consts::TAU;
                    egui::pos2(
                        ctr.x + t.cos() * 9.0 + (t * 2.0).sin() * 2.0,
                        ctr.y - 2.0 + t.sin() * 6.0,
                    )
                })
                .collect();
            p.extend(Shape::dashed_line(&pts, s, 3.0, 2.0));
            p.line_segment(
                [
                    egui::pos2(ctr.x + 7.0, ctr.y + 3.0),
                    egui::pos2(ctr.x + 4.0, ctr.y + 10.0),
                ],
                s,
            );
        }
        Tool::PolyLasso => {
            let pts = [
                egui::pos2(r.min.x, r.min.y + 4.0),
                egui::pos2(r.center().x + 2.0, r.min.y),
                egui::pos2(r.max.x, r.center().y),
                egui::pos2(r.max.x - 6.0, r.max.y),
                egui::pos2(r.min.x + 3.0, r.max.y - 4.0),
                egui::pos2(r.min.x, r.min.y + 4.0),
            ];
            p.extend(Shape::dashed_line(&pts, s, 3.0, 2.0));
        }
        Tool::Wand => {
            let ctr = r.center();
            p.line_segment(
                [
                    egui::pos2(ctr.x - 7.0, ctr.y + 7.0),
                    egui::pos2(ctr.x + 2.0, ctr.y - 2.0),
                ],
                Stroke::new(2.5, c),
            );
            for (dx, dy) in [(6.0, -6.0), (9.0, -2.0), (3.0, -9.0)] {
                p.circle_filled(egui::pos2(ctr.x + dx, ctr.y + dy), 1.8, c);
            }
        }
    }
}
