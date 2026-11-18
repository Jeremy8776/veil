//! The call stats overlay: a small table over the top left of the stage, for debugging a call.
//!
//! Purpose: per person, how their link is doing (path, round trip, loss, bit rate down and
//! up), what their video arrives as (size and frame rate) and how loud their sound arrives;
//! for you, your microphone level after its volume. A forwarder link carries everyone, so it
//! gets its own row. The engine measures once a second (`engine/stats.rs`).
//!
//! Invariants:
//! - Off by default; the Stats button in the top bar or Ctrl+Shift+D turns it on and off.
//! - It only reads. Nothing shown here is sent anywhere.
//! - A number not measured yet shows as "-", never as zero.

use std::collections::HashMap;
use std::time::Duration;

use eframe::egui::{self, Color32, CornerRadius, Rect, RichText, vec2};

use crate::engine::stats::{LinkRow, Stats, VideoRow};
use crate::model::Call;
use crate::theme::MUTED;

/// What the overlay shows, gathered by the call screen.
pub struct Shown<'a> {
    pub call: &'a Call,
    pub stats: &'a Stats,
    /// Each person's received sound level, 0 to 1.
    pub levels: &'a HashMap<u32, f32>,
    /// Your microphone level, 0 to 1, or `None` while muted.
    pub mic: Option<f32>,
}

pub fn overlay(ui: &egui::Ui, area: Rect, shown: &Shown<'_>) {
    egui::Area::new(ui.scope_id().with("call_stats"))
        .order(egui::Order::Foreground)
        .fixed_pos(area.min + vec2(8.0, 8.0))
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(Color32::from_black_alpha(210))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| table(ui, shown));
        });
}

fn table(ui: &mut egui::Ui, shown: &Shown<'_>) {
    let text = |s: String| {
        RichText::new(s)
            .monospace()
            .size(11.5)
            .color(Color32::WHITE)
    };
    let head = |s: &str| RichText::new(s).monospace().size(11.5).color(MUTED);
    if shown.call.demo {
        ui.label(head("Stats need a real call; this one is simulated."));
        return;
    }
    egui::Grid::new("call_stats_grid")
        .spacing(vec2(14.0, 3.0))
        .show(ui, |ui| {
            for h in ["", "Path", "RTT", "Loss", "Down", "Up", "Video", "Sound"] {
                ui.label(head(h));
            }
            ui.end_row();
            let up: Option<u32> = shown
                .stats
                .links
                .iter()
                .filter_map(|l| l.up_kbps)
                .reduce(|a, b| a.saturating_add(b));
            let you = [
                String::from("You"),
                dash(),
                dash(),
                dash(),
                dash(),
                rate(up),
                dash(),
                shown.mic.map_or_else(|| String::from("muted"), level),
            ];
            for cell in you {
                ui.label(text(cell));
            }
            ui.end_row();
            for person in shown.call.participants.iter().filter(|p| !p.is_you) {
                let link = shown.stats.links.iter().find(|l| l.id == Some(person.id));
                let mut cells = link_cells(&person.name, person.path.label(), link);
                cells.push(video(shown.stats.video.get(&person.id)));
                cells.push(
                    shown
                        .levels
                        .get(&person.id)
                        .copied()
                        .map_or_else(dash, level),
                );
                for cell in cells {
                    ui.label(text(cell));
                }
                ui.end_row();
            }
            for link in shown.stats.links.iter().filter(|l| l.id.is_none()) {
                let mut cells = link_cells("Forwarder", link.path.label(), Some(link));
                cells.extend([dash(), dash()]);
                for cell in cells {
                    ui.label(text(cell));
                }
                ui.end_row();
            }
        });
}

/// Name, path (with ", IPv6" for a link over IPv6), RTT, loss, down and up for one row.
fn link_cells(name: &str, path: &str, link: Option<&LinkRow>) -> Vec<String> {
    let path = if link.is_some_and(|l| l.ipv6) {
        format!("{path}, IPv6")
    } else {
        path.to_string()
    };
    let mut cells = vec![short_name(name), path];
    match link {
        Some(l) => cells.extend([ms(l.rtt), loss(l.loss), rate(l.down_kbps), rate(l.up_kbps)]),
        // Through a forwarder the numbers are on its row.
        None => cells.extend([dash(), dash(), dash(), dash()]),
    }
    cells
}

fn dash() -> String {
    String::from("-")
}

/// A name cut to 16 characters, so a long one does not stretch the table.
pub fn short_name(name: &str) -> String {
    if name.chars().count() <= 16 {
        return name.to_string();
    }
    let mut cut: String = name.chars().take(15).collect();
    cut.push('…');
    cut
}

pub fn ms(rtt: Duration) -> String {
    format!("{} ms", rtt.as_millis())
}

pub fn loss(loss: Option<f32>) -> String {
    loss.map_or_else(dash, |l| format!("{l:.1}%"))
}

pub fn rate(kbps: Option<u32>) -> String {
    match kbps {
        None => dash(),
        Some(k) if k >= 1_000 => format!("{:.1} Mbit/s", f64::from(k) / 1_000.0),
        Some(k) => format!("{k} kbit/s"),
    }
}

pub fn video(row: Option<&VideoRow>) -> String {
    match row {
        None => dash(),
        Some(v) => match v.fps {
            Some(fps) => format!("{}x{} {fps:.0} fps", v.width, v.height),
            None => format!("{}x{}", v.width, v.height),
        },
    }
}

/// A meter level (0 to 1 on a 60 dB scale, `media::meter`) as dB below full scale.
pub fn level(level: f32) -> String {
    if level <= 0.0 {
        return String::from("silent");
    }
    format!("{:.0} dB", level.min(1.0) * 60.0 - 60.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_plainly() {
        assert_eq!(ms(Duration::from_micros(23_400)), "23 ms");
        assert_eq!(loss(None), "-");
        assert_eq!(loss(Some(0.54)), "0.5%");
        assert_eq!(rate(None), "-");
        assert_eq!(rate(Some(820)), "820 kbit/s");
        assert_eq!(rate(Some(1_260)), "1.3 Mbit/s");
        assert_eq!(level(0.0), "silent");
        assert_eq!(level(1.0), "0 dB");
        assert_eq!(level(0.5), "-30 dB");
    }

    #[test]
    fn video_shows_size_then_rate() {
        assert_eq!(video(None), "-");
        let mut row = VideoRow {
            width: 1280,
            height: 720,
            fps: None,
        };
        assert_eq!(video(Some(&row)), "1280x720");
        row.fps = Some(29.7);
        assert_eq!(video(Some(&row)), "1280x720 30 fps");
    }

    #[test]
    fn long_names_are_cut() {
        assert_eq!(short_name("Jo"), "Jo");
        assert_eq!(short_name("Bartholomew Smithson"), "Bartholomew Smi…");
        assert_eq!(short_name("Bartholomew Smithson").chars().count(), 16);
    }
}
