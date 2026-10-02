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
            .frame(egui::Frame::none().fill(Color32::from_gray(28)).inner_margin(9.0))
            .show(ctx, |ui| {
                for tool in Tool::ALL {
                    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::click());
                    let bg = if self.tool == tool {
                        ACCENT
                    } else if resp.hovered() {
                        Color32::from_gray(64)
                    } else {
                        Color32::from_gray(44)
                    };
                    ui.painter().rect_filled(rect, 6.0, bg);
                    draw_icon(ui.painter(), rect.shrink(10.0), tool);
                    if resp.on_hover_text(tool.tip()).clicked() {
                        self.tool = tool;
                        self.lasso.clear();
                        if tool != Tool::Move {
                            self.cancel_free_transform();
                        }
                    }
                }
            });
    }
}

pub(crate) fn draw_icon(p: &egui::Painter, r: egui::Rect, tool: Tool) {
    let c = Color32::WHITE;
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
