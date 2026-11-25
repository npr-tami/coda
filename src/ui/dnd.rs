//! Drag-and-drop payloads shared by the sidebar and the song lists (egui's
//! `dnd_set_drag_payload` / `dnd_release_payload`): every song row anywhere (lists, search, Up
//! next, the player bar) drags [`SongsDrag`], and every playlist of the user's own anywhere (the
//! sidebar's sleeves and rows, its folded strip, Home's tiles, the profile's cards, and the drop
//! tray shown while no open sidebar is there) takes it. The chip by the pointer
//! is painted once for the whole window ([`paint_chip`]).

use egui::{CornerRadius, Id, Rect, Stroke, pos2, vec2};

use super::theme::{Palette, bold};
use crate::model::TrackInfo;

/// Songs being dragged, e.g. from the song list onto a sleeve to add them to that playlist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SongsDrag {
    pub uris: Vec<String>,
    /// What the chip by the pointer says: the song's name, or how many songs.
    pub label: String,
}

impl SongsDrag {
    /// One song.
    pub fn one(t: &TrackInfo) -> Self {
        Self { uris: vec![t.uri.clone()], label: t.name.clone() }
    }

    /// Songs by uri ("3 songs"; one by its `name` when given).
    pub fn many(uris: Vec<String>, name: Option<&str>) -> Self {
        let label = match (uris.len(), name) {
            (1, Some(name)) => name.to_owned(),
            (n, _) => super::util::fmt_song_count(n),
        };
        Self { uris, label }
    }
}

/// While songs are dragged: what they are, by the pointer (over everything).
pub fn paint_chip(ctx: &egui::Context, p: &Palette) {
    let Some(drag) = egui::DragAndDrop::payload::<SongsDrag>(ctx) else { return };
    let Some(pointer) = ctx.pointer_latest_pos() else { return };
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::new("songs_drag")));
    let g = painter.layout_no_wrap(drag.label.clone(), bold(12.0), p.text_strong);
    let w = g.size().x.min(260.0);
    let rect = Rect::from_min_size(pointer + vec2(14.0, 10.0), vec2(w + 38.0, 28.0));
    let radius = CornerRadius::same(super::kit::RADIUS);
    painter.add(ctx.global_style().visuals.popup_shadow.as_shape(rect, radius));
    painter.rect_filled(rect, radius, p.raised_bg);
    painter.rect_stroke(rect, radius, Stroke::new(1.0, p.accent.gamma_multiply(0.6)), egui::StrokeKind::Inside);
    super::svgicon::paint(&painter, Rect::from_center_size(pos2(rect.left() + 15.0, rect.center().y), vec2(13.0, 13.0)), "note", p.accent_ink, super::svgicon::STROKE);
    let clip = Rect::from_min_max(pos2(rect.left() + 28.0, rect.top()), pos2(rect.right() - 8.0, rect.bottom()));
    painter.with_clip_rect(clip).galley(pos2(clip.left(), rect.center().y - g.size().y / 2.0), g, p.text_strong);
}

/// Whether songs are being dragged now.
pub fn dragging_songs(ctx: &egui::Context) -> bool {
    egui::DragAndDrop::has_payload_of_type::<SongsDrag>(ctx)
}

/// A playlist at `resp` takes dragged songs: outlined while they are over it, and the songs let go
/// on it (`Some`).
pub fn drop_on(ui: &egui::Ui, resp: &egui::Response, outline: Rect, p: &Palette) -> Option<SongsDrag> {
    if resp.dnd_hover_payload::<SongsDrag>().is_some() {
        ui.painter().rect_stroke(outline, CornerRadius::same(6), Stroke::new(2.0, p.accent_ink), egui::StrokeKind::Inside);
    }
    resp.dnd_release_payload::<SongsDrag>().map(|s| (*s).clone())
}

/// How fast a list scrolls while songs are dragged `d` points from its edge (inside the last
/// `EDGE` points): faster nearer the edge, nothing further in.
const EDGE: f32 = 36.0;

fn edge_speed(top: f32, bottom: f32, y: f32) -> f32 {
    if y < top + EDGE {
        (top + EDGE - y).min(EDGE) * 0.4
    } else if y > bottom - EDGE {
        -(y - bottom + EDGE).min(EDGE) * 0.4
    } else {
        0.0
    }
}

/// While songs are dragged near the top or bottom of the scrolling list `ui` draws (call inside
/// its `ScrollArea`), the list scrolls, so playlists out of view can be reached.
pub fn edge_scroll(ui: &egui::Ui) {
    if !dragging_songs(ui.ctx()) {
        return;
    }
    let Some(at) = ui.ctx().pointer_latest_pos() else { return };
    let clip = ui.clip_rect();
    if !clip.contains(at) {
        return;
    }
    let dy = edge_speed(clip.top(), clip.bottom(), at.y);
    if dy != 0.0 {
        ui.scroll_with_delta(vec2(0.0, dy));
        ui.ctx().request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ArtistRef;
    use crate::ui::kit::headless::{move_to, pass, press, release};
    use egui::{Pos2, Sense};

    #[test]
    fn a_list_scrolls_only_near_its_edges_and_faster_nearer() {
        assert_eq!(edge_speed(0.0, 400.0, 200.0), 0.0);
        assert!(edge_speed(0.0, 400.0, 10.0) > edge_speed(0.0, 400.0, 30.0));
        assert!(edge_speed(0.0, 400.0, 30.0) > 0.0, "near the top: up (content moves down)");
        assert!(edge_speed(0.0, 400.0, 390.0) < 0.0, "near the bottom: down");
    }

    #[test]
    fn the_chip_says_the_song_or_how_many() {
        let t = TrackInfo {
            uri: "spotify:track:a".into(),
            name: "Blue".into(),
            artists: vec![ArtistRef { name: "X".into(), uri: String::new() }],
            album: String::new(),
            album_uri: String::new(),
            cover_small: None,
            cover_large: None,
            duration_ms: 1,
            playable: true,
            is_local: false,
            added_at: 0,
        };
        assert_eq!(SongsDrag::one(&t).label, "Blue");
        assert_eq!(SongsDrag::many(vec!["a".into()], Some("Red")).label, "Red");
        assert_eq!(SongsDrag::many(vec!["a".into(), "b".into(), "c".into()], None).label, "3 songs");
    }

    /// A song dragged from one widget and let go on another (a playlist) arrives there, also when
    /// the place it came from is no longer drawn (scrolled away, another page).
    #[test]
    fn songs_dropped_on_a_playlist_arrive_even_when_their_row_is_gone() {
        let ctx = egui::Context::default();
        let p = Palette::build(crate::config::ThemeMode::Dark, egui::Color32::RED, None);
        let row = Rect::from_min_size(Pos2::new(300.0, 100.0), vec2(200.0, 30.0));
        let target = Rect::from_min_size(Pos2::new(10.0, 100.0), vec2(60.0, 60.0));
        let mut got: Option<SongsDrag> = None;
        let frame = |events: Vec<egui::Event>, show_row: bool, got: &mut Option<SongsDrag>| {
            pass(&ctx, events, |ui| {
                let t = ui.interact(target, egui::Id::new("pl"), Sense::click());
                if let Some(s) = drop_on(ui, &t, target, &p) {
                    *got = Some(s);
                }
                if show_row {
                    let r = ui.interact(row, egui::Id::new("row"), Sense::click_and_drag());
                    if r.drag_started_by(egui::PointerButton::Primary) {
                        r.dnd_set_drag_payload(SongsDrag::many(vec!["spotify:track:a".into()], Some("A")));
                    }
                }
            });
        };
        frame(move_to(row.center()), true, &mut got);
        frame(press(row.center()), true, &mut got);
        frame(move_to(row.center() + vec2(-30.0, 0.0)), true, &mut got);
        frame(move_to(row.center() + vec2(-60.0, 0.0)), true, &mut got);
        assert!(dragging_songs(&ctx));
        // The row scrolls away; the pointer goes on to the playlist.
        frame(move_to(target.center()), false, &mut got);
        frame(move_to(target.center()), false, &mut got);
        frame(release(target.center()), false, &mut got);
        assert_eq!(got.clone().map(|s| s.uris), Some(vec!["spotify:track:a".to_owned()]));
        frame(Vec::new(), false, &mut got);
        assert!(!dragging_songs(&ctx), "the drag ends with the drop");
    }
}
