//! Check the painted text geometry, including egui's alignment around the galley origin.

use super::*;

#[test]
#[allow(clippy::expect_used)]
fn switch_titles_and_details_are_painted_on_the_row_centre_line() {
    let ctx = egui::Context::default();
    theme::load_fonts(&ctx).expect("Windows UI fonts");
    theme::apply(&ctx);
    for width in [220.0, 247.0, 320.0, 518.0] {
        for label in [
            "Local only",
            "Discoverable nearby",
            "Open a port on my router",
            "Hide my address",
        ] {
            for detail in [
                "",
                "Temporarily open the ports guests need while you host a call.",
            ] {
                let mut row = Rect::NOTHING;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            egui::Pos2::ZERO,
                            vec2(1000.0, 1000.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        let mut child = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(Rect::from_min_size(pos2(24.0, 24.0), vec2(width, 900.0)))
                                .layout(egui::Layout::top_down(egui::Align::Center)),
                        );
                        row = toggle_row(&mut child, &mut true, label, detail).rect;
                    },
                );
                output.textures_delta.clear();
                let mut painted = 0;
                for shape in &output.shapes {
                    if let egui::Shape::Text(text) = &shape.shape
                        && (text.galley.job.text == label
                            || (!detail.is_empty() && text.galley.job.text == detail))
                    {
                        painted += 1;
                        let centre = text.pos.x + text.galley.rect.center().x;
                        assert!(
                            (centre - row.center().x).abs() < 1.0,
                            "painted text is off centre: label={label:?}, detail={detail:?}, width={width}, text={centre}, row={}",
                            row.center().x
                        );
                        for line in &text.galley.rows {
                            let mut glyphs = line
                                .glyphs
                                .iter()
                                .filter(|glyph| !glyph.chr.is_whitespace());
                            if let Some(first) = glyphs.next() {
                                let last = glyphs.next_back().unwrap_or(first);
                                let centre = text.pos.x
                                    + line.pos.x
                                    + (first.logical_rect().left() + last.logical_rect().right())
                                        / 2.0;
                                assert!(
                                    (centre - row.center().x).abs() < 1.0,
                                    "painted glyph row is off centre: {label:?}, width={width}, text={centre}, row={}",
                                    row.center().x
                                );
                            }
                        }
                    }
                }
                assert_eq!(painted, if detail.is_empty() { 1 } else { 2 });
            }
        }
    }
}
