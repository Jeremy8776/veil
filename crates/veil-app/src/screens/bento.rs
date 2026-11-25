//! One responsive, identity-keyed call canvas for desktop and Android.
use super::{
    call::{CallView, View},
    share,
    share_sound::{self, Owner},
};
use crate::{
    app::VeilApp,
    engine::screen_key,
    layout,
    model::Participant,
    theme,
    widgets::{self, Tile},
};
use eframe::egui::{self, Rect, TextureId, UiBuilder};

const WAITING: u64 = u64::MAX;
enum Content {
    Person(Participant),
    Screen(String, (TextureId, [usize; 2]), Owner),
    Waiting,
}
struct Cell {
    key: u64,
    aspect: f32,
    content: Content,
}

pub(crate) fn video(app: &VeilApp, person: &Participant) -> Option<(TextureId, [usize; 2])> {
    if !person.camera {
        return None;
    }
    if person.is_you {
        app.preview_video()
    } else {
        app.remote_video(person.id)
    }
}
fn ratio(video: Option<(TextureId, [usize; 2])>) -> f32 {
    video.map_or(layout::ASPECT, |(_, size)| {
        size[0] as f32 / size[1].max(1) as f32
    })
}

/// Returns the explanatory/invitation cell, if requested. No floating tile obscures a face.
pub(crate) fn show(
    view: &mut CallView,
    app: &VeilApp,
    ui: &mut egui::Ui,
    area: Rect,
    waiting: bool,
) -> Option<Rect> {
    let mut cells = Vec::new();
    if waiting {
        cells.push(Cell {
            key: WAITING,
            aspect: 1.25,
            content: Content::Waiting,
        });
    }
    for person in &view.call.participants {
        let screen = if person.is_you {
            app.share_video()
        } else if view.sharers.contains(&person.id) {
            app.remote_video(screen_key(person.id))
        } else {
            None
        };
        if let Some(screen) = screen {
            cells.push(Cell {
                key: (1u64 << 32) | u64::from(person.id),
                aspect: ratio(Some(screen)),
                content: Content::Screen(
                    if person.is_you {
                        "You are sharing".into()
                    } else {
                        format!("{} is sharing", person.name)
                    },
                    screen,
                    if person.is_you {
                        Owner::You
                    } else {
                        Owner::Them {
                            id: person.id,
                            sounding: view.sounding.contains(&person.id),
                        }
                    },
                ),
            });
        }
    }
    let shares = cells.len() > usize::from(waiting);
    view.update_focus();
    let focus = view.view == View::Focus;
    let featured = if waiting || shares {
        0
    } else {
        let focused = view.focus.filter(|_| focus).map(|f| f.id);
        view.call
            .participants
            .iter()
            .position(|p| focused.map_or(!p.is_you, |id| p.id == id))
            .unwrap_or(0)
    };
    for person in &view.call.participants {
        cells.push(Cell {
            key: u64::from(person.id),
            aspect: ratio(video(app, person)),
            content: Content::Person(person.clone()),
        });
    }
    let aspects: Vec<_> = cells.iter().map(|cell| cell.aspect).collect();
    // Group: justified rows, so each card fills a cell of its own shape and no space is
    // wasted. Focus: the speaker on stage over a strip. Screen shares keep the mosaic.
    let rects = if waiting || shares {
        layout::bento(&aspects, featured, area, 12.0)
    } else if focus {
        layout::focus(&aspects, featured, area, 12.0)
    } else {
        layout::justified(&aspects, area, 12.0)
            .unwrap_or_else(|| layout::bento(&aspects, featured, area, 12.0))
    };
    let targets: Vec<_> = cells
        .iter()
        .zip(rects)
        .map(|(cell, rect)| (cell.key, rect))
        .collect();
    // Request final widths once, rather than changing subscriptions on every animation frame.
    let scale = ui.ctx().pixels_per_point();
    view.watch_tiles(
        cells
            .iter()
            .zip(&targets)
            .filter_map(|(cell, (_, rect))| match &cell.content {
                Content::Person(person) if !person.is_you => {
                    Some((person.id, rect.width() * scale))
                }
                _ => None,
            })
            .collect(),
    );
    let duration = if ui.style().animation_time <= 0.0 {
        0.0
    } else {
        0.24
    };
    let frame = view
        .tile_motion
        .frame(&targets, ui.input(|input| input.time), duration);
    if frame.moving {
        ui.ctx().request_repaint();
    }
    let mut waiting_area = None;
    for placement in frame.cells {
        let Some(cell) = cells.iter().find(|cell| cell.key == placement.key) else {
            continue;
        };
        let mut child = ui.new_child(
            UiBuilder::new()
                .id_salt(("bento-cell", placement.key))
                .max_rect(placement.rect),
        );
        child.set_clip_rect(area.intersect(ui.clip_rect()));
        child.set_opacity(placement.opacity);
        match &cell.content {
            Content::Waiting => {
                child
                    .painter()
                    .rect_filled(placement.rect, theme::RADIUS, theme::TILE);
                waiting_area = Some(placement.rect);
            }
            Content::Screen(label, source, owner) => {
                share::stage(&child, placement.rect, *source, label);
                share_sound::overlay(&mut child, placement.rect, app, *owner);
            }
            Content::Person(person) => widgets::tile(
                &child,
                placement.rect,
                &Tile {
                    name: &person.name,
                    video: video(app, person),
                    muted: person.muted,
                    speaking: person.speaking,
                    hand_raised: person.hand_raised,
                    is_you: person.is_you,
                },
            ),
        }
    }
    waiting_area
}
