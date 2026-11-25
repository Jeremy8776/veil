//! A local QR of the complete signed invitation. No shortening or internet fetch required.
//! The QR is cached only while its window is open, then discarded from egui's memory.

use eframe::egui::{self, Color32, RichText, pos2, vec2};
use qrcodegen::{QrCode, QrCodeEcc};

#[derive(Clone)]
struct Code {
    text: String,
    size: i32,
    modules: Vec<bool>,
}

impl Code {
    fn encode(text: &str) -> Result<Self, String> {
        if text.is_empty() {
            return Err("The full invitation is not ready yet.".into());
        }
        if text.len() > veil_security::invite::MAX_LINK_LEN {
            return Err(
                "This invitation is too long for a QR code. Copy the full link instead.".into(),
            );
        }
        let qr = QrCode::encode_text(text, QrCodeEcc::Low).map_err(
            |_| "This invitation is too long for a QR code. Copy the full link instead.",
        )?;
        let size = qr.size();
        let modules = (0..size)
            .flat_map(|y| (0..size).map(move |x| (x, y)))
            .map(|(x, y)| qr.get_module(x, y))
            .collect();
        Ok(Self {
            text: text.to_string(),
            size,
            modules,
        })
    }
}

fn open_id() -> egui::Id {
    egui::Id::unique("full-invite-qr-open")
}
fn cache_id() -> egui::Id {
    egui::Id::unique("full-invite-qr-cache")
}

pub fn clear(ctx: &egui::Context) {
    ctx.data_mut(|data| {
        data.remove::<Code>(cache_id());
        data.remove::<bool>(open_id());
    });
}

pub fn open(ctx: &egui::Context) {
    ctx.data_mut(|data| data.insert_temp(open_id(), true));
}

pub fn button(ui: &mut egui::Ui) {
    if ui.small_button("Show full invite QR").on_hover_text(
        "Contains the whole invitation. No server is needed to read it. Share only with people you want to invite.").clicked() {
        open(ui.ctx());
    }
}

pub fn show(ctx: &egui::Context, text: &str) {
    let mut open = ctx
        .data_mut(|data| data.get_temp::<bool>(open_id()))
        .unwrap_or(false);
    if !open {
        return;
    }
    let cached = ctx.data_mut(|data| data.get_temp::<Code>(cache_id()));
    let code = match cached {
        Some(code) if code.text == text => Ok(code),
        _ => Code::encode(text).inspect(|code| {
            ctx.data_mut(|data| data.insert_temp(cache_id(), code.clone()));
        }),
    };
    if cfg!(target_os = "android") {
        open = crate::mobile::sheet::show(ctx, "invite-qr", "Invite", |ui| {
            match &code {
                Ok(code) => {
                    let side = ui.available_width().min(280.0);
                    // A white rounded card, the code inset by its quiet zone.
                    let (card, _) = ui.allocate_exact_size(vec2(side, side), egui::Sense::hover());
                    ui.painter().rect_filled(card, 16.0, Color32::WHITE);
                    paint(ui, code, card.shrink(12.0), false);
                }
                Err(problem) => {
                    ui.label(RichText::new(problem.as_str()).color(crate::theme::MUTED));
                }
            }
            ui.add_space(24.0);
            let (slot, _) = ui.allocate_exact_size(vec2(88.0, 72.0), egui::Sense::hover());
            let mut slot = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(slot)
                    .layout(egui::Layout::top_down(egui::Align::Center)),
            );
            if crate::mobile::widgets::call_control(&mut slot, crate::icons::Icon::Copy, "Copy invite", false, false)
                .on_hover_text("Anyone who sees this code can ask to join. You still decide who gets in.")
                .clicked()
            {
                ctx.copy_text(text.to_string());
            }
        });
        ctx.data_mut(|data| {
            data.insert_temp(open_id(), open);
            if !open {
                data.remove::<Code>(cache_id());
            }
        });
        return;
    }
    egui::Window::new("Full invitation QR")
        .open(&mut open).collapsible(false).resizable(false)
        .default_width(420.0).show(ctx, |ui| {
            ui.label("Scan or share this full invitation with one guest.");
            ui.label(RichText::new("The host still decides who joins. Anyone who sees this code can request admission.")
                .small().color(crate::theme::MUTED));
            ui.add_space(8.0);
            match &code {
                Ok(code) => {
                    let side = ui.available_width().min(420.0);
                    let (rect, _) = ui.allocate_exact_size(vec2(side, side), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 0.0, Color32::WHITE);
                    paint(ui, code, rect, true);
                }
                Err(problem) => { ui.label(problem); }
            }
            if ui.button("Copy full invitation").clicked() { ctx.copy_text(text.to_string()); }
            ui.label(RichText::new("Veil currently accepts the decoded invitation as pasted text.").small().color(crate::theme::MUTED));
        });
    ctx.data_mut(|data| {
        data.insert_temp(open_id(), open);
        if !open {
            data.remove::<Code>(cache_id());
        }
    });
}

/// Paints `code` in black modules on whole device pixels, centred in `area`. With `quiet`,
/// a four-module quiet zone is kept inside `area`; without it, the caller's white margin is
/// the quiet zone.
fn paint(ui: &egui::Ui, code: &Code, area: egui::Rect, quiet: bool) {
    let scale = ui.ctx().pixels_per_point();
    let cells = code.size + if quiet { 8 } else { 0 };
    let unit = ((area.width().min(area.height()) * scale) / cells as f32).floor().max(1.0) / scale;
    let side = code.size as f32 * unit;
    let origin = area.center() - vec2(side, side) * 0.5;
    let origin = pos2((origin.x * scale).round() / scale, (origin.y * scale).round() / scale);
    let painter = ui.painter();
    for y in 0..code.size {
        for x in 0..code.size {
            if code.modules.get((y * code.size + x) as usize).copied().unwrap_or(false) {
                let at = origin + vec2(x as f32 * unit, y as f32 * unit);
                painter.rect_filled(egui::Rect::from_min_size(at, vec2(unit, unit)), 0.0, Color32::BLACK);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn full_invitation_limit_fits_and_has_finder_patterns() {
        let text = "v".repeat(veil_security::invite::MAX_LINK_LEN);
        let code = Code::encode(&text).unwrap();
        assert_eq!(code.modules.len(), (code.size * code.size) as usize);
        assert!(code.modules[0]);
        assert!(code.modules[6]);
        assert!(!code.modules[code.size as usize + 1]);
        assert!(Code::encode("").is_err());
        assert!(Code::encode(&(text + "v")).is_err());
    }
}
