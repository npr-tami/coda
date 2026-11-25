//! Changing a playlist's cover: after a picture is picked, a crop step shows it in a square. Drag
//! to move it, scroll or use the slider to zoom, double-click to start over. Only the chosen square
//! is uploaded.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use egui::{Color32, CornerRadius, CursorIcon, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};

use super::CodaApp;
use super::fin;
use super::theme::regular;
use crate::model::Command;
use crate::webapi::{CoverCrop, cover_preview, crop_cover, load_cover_picture};

/// The square the picture is shown in (points).
const VIEW: f32 = 320.0;
/// The preview's longer side (pixels): sharp enough at the closest zoom.
const PREVIEW_SIDE: u32 = 1280;
/// The smallest square of the real picture zooming can come down to (pixels).
const MIN_CROP_PX: f32 = 128.0;
const MAX_ZOOM: f32 = 8.0;

/// What the picker and crop threads send back to the window.
pub(super) enum CoverPick {
    /// The file picker was closed without a picture.
    Cancelled,
    /// A picture was picked and opened: show the crop step.
    Opened { uri: String, path: PathBuf, preview: image::RgbImage, full: (u32, u32) },
    /// The chosen square was saved to `path`: upload it.
    Cropped { uri: String, path: PathBuf },
    Failed(String),
}

/// The open crop step.
pub(super) struct CoverCropper {
    uri: String,
    path: PathBuf,
    tex: egui::TextureHandle,
    /// The preview's size (pixels).
    size: Vec2,
    max_zoom: f32,
    zoom: f32,
    /// The middle of the chosen square, in preview pixels.
    centre: Vec2,
}

impl CoverCropper {
    fn new(ctx: &egui::Context, uri: String, path: PathBuf, preview: image::RgbImage, full: (u32, u32)) -> Self {
        let (w, h) = preview.dimensions();
        let image = egui::ColorImage::from_rgb([w as usize, h as usize], preview.as_raw());
        let tex = ctx.load_texture("cover-crop", image, egui::TextureOptions::LINEAR);
        let size = vec2(w as f32, h as f32);
        Self { uri, path, tex, size, max_zoom: max_zoom(full), zoom: 1.0, centre: size / 2.0 }
    }

    /// The chosen square's side, in preview pixels.
    fn side(&self) -> f32 {
        self.size.min_elem() / self.zoom
    }

    /// Points on screen per preview pixel.
    fn scale(&self) -> f32 {
        VIEW / self.side().max(1.0)
    }

    fn reset(&mut self) {
        self.zoom = 1.0;
        self.centre = self.size / 2.0;
    }

    /// Zoom to `zoom`, keeping the picture's point at `anchor` (an offset from the view's middle,
    /// in points) where it is on screen.
    fn zoom_to(&mut self, zoom: f32, anchor: Vec2) {
        let before = self.centre + anchor / self.scale();
        self.zoom = zoom.clamp(1.0, self.max_zoom);
        self.centre = before - anchor / self.scale();
        self.clamp();
    }

    fn clamp(&mut self) {
        self.centre = clamp_centre(self.centre, self.size, self.side());
    }

    fn crop(&self) -> CoverCrop {
        crop_of(self.centre, self.size, self.zoom)
    }
}

/// How far in the picture can be zoomed: never to a square under [`MIN_CROP_PX`] of the real picture.
fn max_zoom(full: (u32, u32)) -> f32 {
    (full.0.min(full.1) as f32 / MIN_CROP_PX).clamp(1.0, MAX_ZOOM)
}

/// The square's middle, moved so the square of `side` stays inside a picture of `size`.
fn clamp_centre(centre: Vec2, size: Vec2, side: f32) -> Vec2 {
    let half = side / 2.0;
    let fit = |c: f32, len: f32| if len <= side { len / 2.0 } else { c.clamp(half, len - half) };
    vec2(fit(centre.x, size.x), fit(centre.y, size.y))
}

/// The square around `centre` at `zoom`, as fractions of the picture.
fn crop_of(centre: Vec2, size: Vec2, zoom: f32) -> CoverCrop {
    let side = size.min_elem() / zoom;
    CoverCrop {
        left: ((centre.x - side / 2.0) / size.x).max(0.0),
        top: ((centre.y - side / 2.0) / size.y).max(0.0),
        size: 1.0 / zoom,
    }
}

/// Removes cropped covers an earlier crop step left behind (they are uploaded within seconds).
fn clear_old_crops(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let old = SystemTime::now() - Duration::from_secs(60 * 60);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("coda-cover-") && name.ends_with(".png") {
            let stale = entry.metadata().and_then(|m| m.modified()).map(|t| t < old).unwrap_or(false);
            if stale {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

impl CodaApp {
    /// Opens the native file picker on a background thread (it blocks until closed), then opens
    /// the picked picture there too, for the crop step.
    pub(super) fn pick_cover(&mut self, ctx: &egui::Context, uri: String) {
        if self.cover_picker_open || self.cover_crop.is_some() {
            return;
        }
        self.cover_picker_open = true;
        let tx = self.cover_tx.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("coda-file-dialog".into()).spawn(move || {
            let path = rfd::FileDialog::new()
                .set_title("Choose a cover image")
                .add_filter("Images", &["jpg", "jpeg", "png", "webp"])
                .pick_file();
            let pick = match path {
                None => CoverPick::Cancelled,
                Some(path) => match load_cover_picture(&path) {
                    Ok(image) => {
                        let full = (image.width(), image.height());
                        if full.0 == 0 || full.1 == 0 {
                            CoverPick::Failed("That picture is empty.".into())
                        } else {
                            let preview = cover_preview(&image, PREVIEW_SIDE);
                            CoverPick::Opened { uri, path, preview, full }
                        }
                    }
                    Err(e) => CoverPick::Failed(e.to_string()),
                },
            };
            let _ = tx.send(pick);
            ctx.request_repaint();
        });
        if let Err(e) = spawned {
            log::warn!("could not open the file picker: {e}");
            self.cover_picker_open = false;
            self.toast("Could not open the file picker.", true);
        }
    }

    pub(super) fn handle_cover_picks(&mut self, ctx: &egui::Context) {
        while let Ok(pick) = self.cover_rx.try_recv() {
            match pick {
                CoverPick::Cancelled => self.cover_picker_open = false,
                CoverPick::Opened { uri, path, preview, full } => {
                    self.cover_picker_open = false;
                    self.cover_crop = Some(CoverCropper::new(ctx, uri, path, preview, full));
                }
                CoverPick::Cropped { uri, path } => {
                    self.toast("Uploading the new cover…", false);
                    self.send(Command::SetPlaylistCover { uri, image_path: path });
                }
                CoverPick::Failed(e) => {
                    self.cover_picker_open = false;
                    self.toast(e, true);
                }
            }
        }
    }

    /// Cuts the chosen square out of the full picture on a background thread and saves it for
    /// the upload.
    fn use_crop(&mut self, ctx: &egui::Context, cropper: CoverCropper) {
        let crop = cropper.crop();
        let (uri, source) = (cropper.uri, cropper.path);
        let tx = self.cover_tx.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("coda-cover-crop".into()).spawn(move || {
            let dir = std::env::temp_dir();
            clear_old_crops(&dir);
            let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
            let out = dir.join(format!("coda-cover-{}-{stamp}.png", std::process::id()));
            let saved = load_cover_picture(&source).and_then(|image| crop_cover(&image, crop)).and_then(|square| {
                square.save_with_format(&out, image::ImageFormat::Png).map_err(|e| {
                    log::warn!("could not save the cropped cover {}: {e}", out.display());
                    anyhow::anyhow!("coda couldn't save the cropped picture.")
                })
            });
            let pick = match saved {
                Ok(()) => CoverPick::Cropped { uri, path: out },
                Err(e) => CoverPick::Failed(e.to_string()),
            };
            let _ = tx.send(pick);
            ctx.request_repaint();
        });
        if let Err(e) = spawned {
            log::warn!("could not start cropping the cover: {e}");
            self.toast("coda couldn't crop the picture.", true);
        }
    }

    pub(super) fn cover_crop_modal(&mut self, ctx: &egui::Context) {
        let p = self.palette;
        let Some(cropper) = self.cover_crop.as_mut() else { return };
        let mut done = None;
        let modal = fin::dialog(ctx, Id::new("cover_crop"), |ui| {
            ui.set_width(VIEW);
            fin::dialog_head(ui, "Crop the cover", Some("Drag to move the picture, scroll or use the slider to zoom. Double-click to start over."));
            ui.add_space(14.0);

            let (view, resp) = ui.allocate_exact_size(vec2(VIEW, VIEW), Sense::click_and_drag());
            let cursor = if resp.dragged() { CursorIcon::Grabbing } else { CursorIcon::Grab };
            let resp = resp.on_hover_cursor(cursor);
            if resp.dragged() {
                cropper.centre -= resp.drag_delta() / cropper.scale();
                cropper.clamp();
            }
            if resp.hovered() {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y + i.zoom_delta().ln() * 200.0);
                if scroll != 0.0 {
                    let anchor = resp.hover_pos().map(|at| at - view.center()).unwrap_or(Vec2::ZERO);
                    cropper.zoom_to(cropper.zoom * (scroll / 300.0).exp(), anchor);
                }
            }
            if resp.double_clicked() {
                cropper.reset();
            }
            let s = cropper.scale();
            let image = Rect::from_min_size(view.center() - cropper.centre * s, cropper.size * s);
            let painter = ui.painter().with_clip_rect(view);
            painter.rect_filled(view, CornerRadius::same(6), Color32::WHITE);
            painter.image(cropper.tex.id(), image, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
            if resp.dragged() {
                let line = Stroke::new(1.0, Color32::from_white_alpha(90));
                for k in [1.0 / 3.0, 2.0 / 3.0] {
                    painter.vline(view.left() + VIEW * k, view.y_range(), line);
                    painter.hline(view.x_range(), view.top() + VIEW * k, line);
                }
            }
            ui.painter().rect_stroke(view, CornerRadius::same(6), Stroke::new(1.0, p.border), StrokeKind::Inside);

            ui.add_space(14.0);
            let (row, _) = ui.allocate_exact_size(vec2(VIEW, 18.0), Sense::hover());
            ui.painter().text(pos2(row.left(), row.center().y), egui::Align2::LEFT_CENTER, "Zoom", regular(11.5), p.text_weak);
            let line = Rect::from_min_max(pos2(row.left() + 48.0, row.top()), row.right_bottom());
            let can_zoom = cropper.max_zoom > 1.0;
            let value = if can_zoom { (cropper.zoom - 1.0) / (cropper.max_zoom - 1.0) } else { 0.0 };
            let (_, held) = fin::line(ui, &p, line, Id::new("cover_crop_zoom"), value, can_zoom);
            if let Some(v) = held {
                cropper.zoom_to(1.0 + v * (cropper.max_zoom - 1.0), Vec2::ZERO);
            }

            fin::dialog_actions(ui, &p, Id::new("cover_crop_actions"), "Use picture", true)
        });
        let (ok, cancel) = modal.inner;
        if ok {
            done = self.cover_crop.take();
        } else if cancel || modal.should_close() {
            self.cover_crop = None;
        }
        if let Some(cropper) = done {
            self.use_crop(ctx, cropper);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_square_at_first() {
        // A landscape picture opens on its middle square, as the old upload cropped it.
        let size = vec2(300.0, 100.0);
        let crop = crop_of(size / 2.0, size, 1.0);
        assert_eq!(crop, CoverCrop { left: 1.0 / 3.0, top: 0.0, size: 1.0 });
        assert_eq!(crop.pixels(3000, 1000), (1000, 0, 1000));
    }

    #[test]
    fn square_never_leaves_the_picture() {
        let size = vec2(300.0, 100.0);
        assert_eq!(clamp_centre(vec2(-50.0, 500.0), size, 100.0), vec2(50.0, 50.0));
        assert_eq!(clamp_centre(vec2(999.0, 0.0), size, 50.0), vec2(275.0, 25.0));
        // Zoomed in, the square can sit anywhere inside.
        assert_eq!(clamp_centre(vec2(120.0, 60.0), size, 50.0), vec2(120.0, 60.0));
    }

    #[test]
    fn zoom_limits_follow_the_real_picture() {
        assert_eq!(max_zoom((100, 80)), 1.0);
        assert_eq!(max_zoom((1280, 512)), 4.0);
        assert_eq!(max_zoom((8000, 6000)), MAX_ZOOM);
    }

    #[test]
    fn zoomed_crop_in_a_corner() {
        let size = vec2(400.0, 200.0);
        // Zoom 4: a 50 px square at the bottom right.
        let centre = clamp_centre(vec2(1000.0, 1000.0), size, 50.0);
        let crop = crop_of(centre, size, 4.0);
        assert_eq!(crop.pixels(4000, 2000), (3500, 1500, 500));
    }
}
