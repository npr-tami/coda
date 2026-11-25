//! The design's frame ("sign", "body", "pb" and "st"): the chrome
//! title bar, the sidebar (300 by default, 240-420 by dragging its edge, or the 56 strip), the side
//! panel (340 by default, 260-480 by dragging its edge), the player bar, and the window's own edges
//! (Coda draws its own title bar).
//!
//! No width folds the layout on its own above 760: each side keeps the width the user dragged it to,
//! and when the window gets too small for it and a 480 page, the side panel narrows first, then the
//! sidebar, then the side panel steps aside, and last the sidebar becomes the strip (see [`layout`]).
//! Shrinking the window never changes the widths kept in the settings, so they come back with the room.
//! Under 760 the sidebar opens over the page and the panel stays closed.

use egui::{
    Color32, CornerRadius, CursorIcon, Id, Order, Rect, Sense, Stroke, TextureHandle, Ui,
    ViewportCommand, pos2, vec2,
};

use super::fin::{self, Look};
use super::motion;
use super::svgicon;
use super::{MiddleTab, CodaApp};
use crate::model::Source;

/// From this width up the player bar and title bar take their roomier sizes (looks only: what the
/// sidebar and panel get comes from [`layout`]).
pub const WIDE: f32 = 1200.0;
/// Under this width: no sidebar or side panel beside the page.
pub const NARROW: f32 = 760.0;
pub const SIGN_H: f32 = 52.0;
pub const STRIP_W: f32 = 56.0;
/// The gap around and between the cards (the sidebar, the page, the side panel), and their corners.
pub const CARD_GAP: f32 = 8.0;
pub const CARD_R: u8 = 12;
/// How much of a whole-window wallpaper shows inside the cards: half the strength set in Settings,
/// so words stay easy to read over it.
pub const CARD_WALL: f32 = 0.5;
/// Islands: the base (the gaps between the cards, the title bar, the player bar) is plain. True puts
/// a whole-window wallpaper there too, at full strength (the "Cards + wallpaper" look).
pub const WALL_IN_GAPS: bool = false;
/// How far down the page card's lift fades out (see `Palette::page_lift`).
const PAGE_LIFT_H: f32 = 260.0;
/// The sidebar's width: by default, and the narrowest and widest it can be dragged to.
pub const SIDE_W: f32 = 300.0;
pub const SIDE_MIN: f32 = 240.0;
pub const SIDE_MAX: f32 = 420.0;
/// The side panel's width: by default, and the narrowest and widest it can be dragged to.
pub const PANEL_W: f32 = 340.0;
pub const PANEL_MIN: f32 = 260.0;
pub const PANEL_MAX: f32 = 480.0;
/// The narrowest page the sidebar and side panel leave beside them before giving way.
pub const PAGE_MIN: f32 = 480.0;
/// Dragged this far under its narrowest, the sidebar folds to the strip and the side panel closes;
/// the strip dragged out past this opens the sidebar again.
pub const SIDE_SNAP: f32 = 180.0;
pub const PANEL_SNAP: f32 = 180.0;
pub const STRIP_SNAP: f32 = 112.0;
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Wide,
    Medium,
    Narrow,
}

pub fn mode_of(w: f32) -> Mode {
    if w >= WIDE {
        Mode::Wide
    } else if w >= NARROW {
        Mode::Medium
    } else {
        Mode::Narrow
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Open,
    Strip,
    None,
}

/// Where everything goes at one window width.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub mode: Mode,
    pub w: f32,
    pub side: Side,
    pub side_w: f32,
    pub panel_w: f32,
    /// The search box's width in the title bar.
    pub search_w: f32,
    /// The sidebar is the strip only because the window has no room for it (Ctrl+B then opens it
    /// over the page).
    pub side_squeezed: bool,
    /// The side panel is open in the settings but the window has no room for it.
    pub panel_squeezed: bool,
}

/// What the user asked for: the sidebar's and side panel's widths (as dragged, or the defaults),
/// whether the sidebar is folded to the strip, whether the side panel is open, and whether, short of
/// room, the panel should stay and the sidebar fold (Ctrl+P on a squeezed-out panel).
#[derive(Clone, Copy, Debug)]
pub struct Want {
    pub side: f32,
    pub panel: f32,
    pub folded: bool,
    pub panel_open: bool,
    pub keep_panel: bool,
}

impl Default for Want {
    fn default() -> Self {
        Self { side: SIDE_W, panel: PANEL_W, folded: false, panel_open: true, keep_panel: false }
    }
}

/// A dragged or saved width kept within `min`..=`max` (nonsense gives `default`).
pub fn clamp_width(w: Option<f32>, default: f32, min: f32, max: f32) -> f32 {
    w.filter(|w| w.is_finite()).unwrap_or(default).clamp(min, max).round()
}

/// The sidebar and side panel in a window `w` wide. Each keeps the width wanted while the page keeps
/// [`PAGE_MIN`]; short of room the panel narrows to [`PANEL_MIN`], then the sidebar to [`SIDE_MIN`],
/// then the panel steps aside (the sidebar back to its width), and last the sidebar becomes the strip.
/// With `keep_panel` the sidebar becomes the strip before the panel steps aside.
pub fn layout(w: f32, want: Want) -> Layout {
    let mode = mode_of(w);
    let search_w = match mode {
        Mode::Wide => (w - 560.0).clamp(200.0, 420.0),
        _ if w >= 900.0 => 280.0,
        _ if w >= 820.0 => 240.0,
        _ => 200.0,
    };
    let base = Layout { mode, w, side: Side::None, side_w: 0.0, panel_w: 0.0, search_w, side_squeezed: false, panel_squeezed: false };
    if mode == Mode::Narrow {
        return base;
    }
    let side_want = clamp_width(Some(want.side), SIDE_W, SIDE_MIN, SIDE_MAX);
    let panel_want = clamp_width(Some(want.panel), PANEL_W, PANEL_MIN, PANEL_MAX);
    let room = w - PAGE_MIN;
    // The widths with the sidebar open (or the strip) and the panel open (or not), each narrowed as far
    // as it may go: the panel first.
    let fit = |side_open: bool, panel_open: bool| -> (f32, f32, bool) {
        let mut side = if side_open { side_want } else { STRIP_W };
        let mut panel = if panel_open { panel_want } else { 0.0 };
        if side + panel > room && panel_open {
            panel = (room - side).clamp(PANEL_MIN, panel);
        }
        if side + panel > room && side_open {
            side = (room - panel).clamp(SIDE_MIN, side);
        }
        (side, panel, side + panel <= room + 0.01)
    };
    let side_open = !want.folded;
    let mut choices = vec![(side_open, want.panel_open)];
    if want.keep_panel {
        choices.extend([(false, want.panel_open), (side_open, false)]);
    } else {
        choices.extend([(side_open, false), (false, want.panel_open)]);
    }
    choices.push((false, false));
    let (open, panel_on, side_w, panel_w) = choices
        .iter()
        .find_map(|&(s, p)| {
            let (sw, pw, fits) = fit(s, p);
            fits.then_some((s, p, sw, pw))
        })
        // Not even the strip and a page fit: the page gets what is left.
        .unwrap_or_else(|| (false, false, STRIP_W, 0.0));
    Layout {
        side: if open { Side::Open } else { Side::Strip },
        side_w: side_w.round(),
        panel_w: panel_w.round(),
        side_squeezed: side_open && !open,
        panel_squeezed: want.panel_open && !panel_on,
        ..base
    }
}

/// The frame's state for this run.
#[derive(Default)]
pub struct FrameState {
    /// Playlists that take dragged songs were drawn this pass (the sidebar, its strip or its
    /// overlay): when not, a drag shows the drop tray instead.
    pub song_targets: bool,
    /// The 300 sidebar is open over the page (medium and narrow widths).
    pub side_over: bool,
    /// The side panel's tab.
    pub panel_tab: super::panel::Tab,
    /// Ctrl+P brought back a side panel the window had no room for: the sidebar is the strip for it
    /// (for this run only).
    pub keep_panel: bool,
    /// The sidebar's or side panel's edge being dragged.
    pub sash: Option<Sash>,
    wordmark: Option<TextureHandle>,
    /// Passes drawn, and whether a self-shot has been asked for (see [`FrameState::self_shot`]).
    passes: u64,
    shot_asked: bool,
    /// Passes drawn at the size a self-shot asked for, since when.
    settled: u32,
    settled_at: Option<std::time::Instant>,
    /// Words after the size in a self-shot request ("panel": the side panel open for the picture).
    shot_flags: Vec<String>,
    /// The window's size and maximised state before a self-shot changed them: put back after it.
    before_shot: Option<(egui::Vec2, bool)>,
    /// The working set when memory was last reported in the log (see `memory_probe`).
    mem_peak: usize,
    /// The page and windows before self-shots changed them, and when the last picture was taken:
    /// put back two seconds after the last one (a run of pictures goes on in between).
    shot_restore: Option<ShotRestore>,
    shot_ended_at: Option<std::time::Instant>,
    /// The page last drawn, and when it was switched to (for the page animation).
    page_seen: Option<super::nav::Place>,
    /// Middle-click scrolling, as in a browser: where it started; and the page's area.
    autoscroll: Option<AutoScroll>,
    /// The middle button, which Coda takes on the page before egui sees it (held and moved,
    /// egui would take it for a drag of what is under the pointer, and no list would scroll):
    /// down now, and pressed (where) or let go this pass.
    middle_down: bool,
    middle_pressed: Option<egui::Pos2>,
    page_rect: Option<Rect>,
    page_since: Option<std::time::Instant>,
}

/// The edge between the page and the sidebar, or the page and the side panel.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Edge {
    Side,
    Panel,
}

/// An edge being dragged: the width its side had and where the mouse was when the drag started, and
/// the width under the mouse now (kept within what the side may be).
#[derive(Clone, Copy, Debug)]
pub struct Sash {
    pub edge: Edge,
    start_w: f32,
    start_x: f32,
    pub live: f32,
    /// Dragged past the snap: the sidebar folds to the strip, or the panel closes, on letting go.
    pub snapped: bool,
}

/// Middle-click scrolling: where it started, when, and whether the button was held and moved
/// (then letting go ends it).
struct AutoScroll {
    at: egui::Pos2,
    since: std::time::Instant,
    held: bool,
}

/// What self-shots may change in the window, as it was.
struct ShotRestore {
    tab: MiddleTab,
    source: Option<Source>,
    help: bool,
    settings: bool,
    stage: bool,
    lyrics_off: bool,
}

/// A test aid: while a file named `coda-shot.request` sits beside coda.exe, Coda saves what it
/// draws as `coda-shot.png` there (once per request) and logs its state. A request may name a size:
/// the window takes it for the picture and goes back to its own size after (it would otherwise stay
/// resized on the user's screen).
fn shot_paths() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    Some((dir.join("coda-shot.request"), dir.join("coda-shot.png")))
}

impl FrameState {
    pub fn self_shot(&mut self, ctx: &egui::Context, hidden: bool, switched: bool, mini: bool) {
        self.passes += 1;
        let Some((request, out)) = shot_paths() else { return };
        // The picture asked for comes back as an event a pass or two later.
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = shot {
            let [w, h] = image.size;
            let bytes: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Some(img) = image::RgbaImage::from_raw(w as u32, h as u32, bytes) {
                match img.save(&out) {
                    Ok(()) => log::info!("self-shot saved: {w}x{h}"),
                    Err(e) => log::warn!("self-shot not saved: {e}"),
                }
            }
            let _ = std::fs::remove_file(&request);
            self.shot_asked = false;
            self.shot_flags.clear();
            self.shot_ended_at = Some(std::time::Instant::now());
            if let Some((size, maximized)) = self.before_shot.take() {
                if maximized {
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(true));
                } else {
                    ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
                }
            }
            return;
        }
        if self.shot_asked || !request.exists() {
            if request.exists() {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            return;
        }
        // The request may name a window size ("1440 900"): the window takes it first, and the picture
        // is taken a few passes later, once it has been drawn at that size.
        let text = std::fs::read_to_string(&request).unwrap_or_default();
        // Put back afterwards only what the picture changes ("0 0" changes nothing).
        let sized = text.trim() == "max" || !text.split_whitespace().take(2).all(|v| v.parse::<f32>().is_ok_and(|n| n <= 0.0));
        if self.before_shot.is_none() && sized {
            let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            self.before_shot = Some((ctx.content_rect().size(), maximized));
        }
        if text.trim() == "max" && !ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
            if self.passes.is_multiple_of(10) {
                ctx.send_viewport_cmd(ViewportCommand::Maximized(true));
            }
            self.settled = 0;
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return;
        }
        self.shot_flags = text.split_whitespace().skip(2).map(str::to_owned).collect();
        let want = Some(text).and_then(|t| {
            let mut it = t.split_whitespace().filter_map(|v| v.parse::<f32>().ok());
            Some(egui::vec2(it.next()?, it.next()?))
        });
        // "0 0": the window keeps the size it has.
        let want = want.filter(|w| w.x > 0.0 && w.y > 0.0);
        let now = ctx.content_rect().size();
        if let Some(want) = want
            && (want - now).length() > 1.0
        {
            if self.passes.is_multiple_of(10) {
                ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
                ctx.send_viewport_cmd(ViewportCommand::InnerSize(want));
            }
            self.settled = 0;
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return;
        }
        // Eight passes and 0.7 s at the size asked for, so fades (a theme asked for, hovers) end.
        if self.settled == 0 {
            self.settled_at = Some(std::time::Instant::now());
        }
        self.settled += 1;
        if self.settled < 8 || self.settled_at.is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(700)) {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return;
        }
        self.shot_asked = true;
        log::info!("self-shot asked: pass {}, hidden {hidden}, switched {switched}, mini {mini}, size {:?}", self.passes, ctx.content_rect());
        ctx.send_viewport_cmd(ViewportCommand::Screenshot(egui::UserData::default()));
        ctx.request_repaint();
    }
}

/// The title bar's wordmark: coda in Unbounded 800, 19 px, letter-spaced, liquid chrome
/// (`.sign .wm`), drawn at 4x (assets/wordmark_bar.png from assets/wordmark_bar.html, trimmed)
/// and shown at its CSS size.
static WORDMARK_BAR: &[u8] = include_bytes!("../../assets/wordmark_bar.png");
const WORDMARK_SIZE: egui::Vec2 = egui::vec2(70.0, 16.25);


/// A title bar's empty part drags the window (double-click maximises it), and its right end has the
/// window buttons: 46 wide (40 when `narrow`), the bar's full height, coloured for a `light` bar or a
/// dark one. Returns where they start.
pub(super) fn window_controls(ui: &Ui, r: Rect, narrow: bool, light: bool) -> f32 {
    // The bar follows the theme (`light`).
    let p = crate::ui::theme::palette(ui.ctx());
    let p = if light { p } else { p.on_dark() };
    // Made first, so everything on the bar gets its own clicks.
    let drag = ui.interact(r, Id::new("sign-drag"), Sense::click_and_drag());
    let maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
    if drag.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    }
    let wc_w = if narrow { 40.0 } else { 46.0 };
    let wc_left = r.right() - wc_w * 3.0;
    for (i, (icon, tip)) in [("min", "Minimise"), ("max", if maximized { "Restore" } else { "Maximise" }), ("x", "Close")].into_iter().enumerate() {
        let rect = Rect::from_min_size(pos2(wc_left + wc_w * i as f32, r.top()), vec2(wc_w, r.height()));
        let resp = ui.interact(rect, Id::new(("wc", icon)), Sense::click()).on_hover_text(tip);
        let close = icon == "x";
        if resp.hovered() {
            let bg = if close {
                Color32::from_rgb(196, 43, 28)
            } else if light {
                Color32::from_black_alpha(15)
            } else {
                Color32::from_white_alpha(15)
            };
            ui.painter().rect_filled(rect, CornerRadius::ZERO, bg);
        }
        let fg = match (resp.hovered(), close) {
            (true, true) => Color32::WHITE,
            (true, false) => p.text,
            (false, _) => p.text_weak,
        };
        svgicon::paint(ui.painter(), Rect::from_center_size(rect.center(), vec2(13.0, 13.0)), icon, fg, 1.2);
        if resp.clicked() {
            match icon {
                "min" => ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true)),
                "max" => ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(!maximized)),
                _ => ui.ctx().send_viewport_cmd(ViewportCommand::Close),
            }
        }
    }
    wc_left
}

/// Runs `add` in its own area at `rect`: filled with `fill`, and cut off at its edges.
pub(super) fn region<R>(ui: &mut Ui, rect: Rect, id: &str, fill: Option<Color32>, add: impl FnOnce(&mut Ui) -> R) -> R {
    let mut child = ui.new_child(egui::UiBuilder::new().id_salt(id).max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    if let Some(fill) = fill {
        child.painter().rect_filled(rect, CornerRadius::ZERO, fill);
    }
    add(&mut child)
}

impl CodaApp {
    /// Memory checks: a "memory" self-shot request gets a report in coda-shot.txt (and the log), and
    /// the working set growing 256 MB past the last report gets one in the log, to catch what grows.
    pub(super) fn memory_probe(&mut self, ctx: &egui::Context) {
        if let Some((request, _)) = shot_paths()
            && std::fs::read_to_string(&request).is_ok_and(|t| t.trim() == "memory")
        {
            let report = self.memory_report(ctx);
            log::info!("{report}");
            let _ = std::fs::write(request.with_file_name("coda-shot.txt"), &report);
            let _ = std::fs::remove_file(&request);
        }
        if self.frame.passes.is_multiple_of(120)
            && let Some((_, ws, _)) = super::window::process_memory()
            && ws > self.frame.mem_peak + 256 * 1024 * 1024
        {
            self.frame.mem_peak = ws;
            log::info!("{}", self.memory_report(ctx));
        }
    }

    /// What takes memory, in one line: the process's numbers, egui's textures (and the five biggest),
    /// the font atlas and galley cache, the cover cache, lyrics, and the big lists.
    fn memory_report(&self, ctx: &egui::Context) -> String {
        let mb = |b: usize| b / (1024 * 1024);
        let (private, ws, peak) = super::window::process_memory().unwrap_or((0, 0, 0));
        let (textures, texture_bytes, biggest) = {
            let manager = ctx.tex_manager();
            let manager = manager.read();
            let mut list: Vec<(String, usize)> = manager.allocated().map(|(_, m)| (m.name.clone(), m.size[0] * m.size[1] * m.bytes_per_pixel)).collect();
            let total: usize = list.iter().map(|(_, b)| *b).sum();
            list.sort_by_key(|(_, b)| std::cmp::Reverse(*b));
            let biggest: Vec<String> = list.iter().take(5).map(|(n, b)| format!("{} {} KB", n.chars().take(40).collect::<String>(), b / 1024)).collect();
            (manager.num_allocated(), total, biggest.join(", "))
        };
        let (galleys, atlas) = ctx.fonts(|f| (f.num_galleys_in_cache(), f.font_image_size()));
        let (covers, covers_ready) = self.images.stats();
        let list = self.view.as_ref().and_then(|v| v.tracks.as_ref().map(|t| t.len())).unwrap_or(0);
        let queue = self.deck.as_ref().map_or(0, |d| d.order.len());
        let (albums, artists) = self.collection.as_ref().map_or((0, 0), |c| (c.albums.len(), c.artists.len()));
        let canvas = self.canvas_texture_size().map_or("none".to_owned(), |s| format!("{}x{}", s[0], s[1]));
        format!(
            "memory: private {} MB, working set {} MB (peak {} MB); textures {} ({} MB; biggest: {}); font atlas {}x{}, {} galleys cached; covers cached {} ({} ready); lyrics cached {}; playlists {}, liked {}, list {} songs, queue {}, collection {} albums {} artists; canvas texture {}; view open {}",
            mb(private), mb(ws), mb(peak), textures, mb(texture_bytes), biggest, atlas[0], atlas[1], galleys, covers, covers_ready, self.lyrics.len(), self.playlists.len(), self.liked.len(), list, queue, albums, artists, canvas, self.stage.open
        )
    }

    pub(super) fn frame_layout(&self, w: f32) -> Layout {
        let shot_panel = self.frame.shot_flags.iter().any(|f| f == "panel");
        // While an edge is dragged, its side takes the width under the mouse.
        let dragged = |edge: Edge| self.frame.sash.filter(|s| s.edge == edge).map(|s| s.live);
        let want = Want {
            side: dragged(Edge::Side).unwrap_or_else(|| clamp_width(self.prefs.library_width, SIDE_W, SIDE_MIN, SIDE_MAX)),
            panel: dragged(Edge::Panel).unwrap_or_else(|| clamp_width(self.prefs.deck_width, PANEL_W, PANEL_MIN, PANEL_MAX)),
            folded: self.prefs.sidebar_folded,
            panel_open: self.prefs.show_rules_panel || shot_panel,
            keep_panel: self.frame.keep_panel,
        };
        layout(w, want)
    }

    /// Ctrl+B and the sidebar button: the sidebar folds to the strip and back. When it is the strip only
    /// for want of room (or the window is narrow), the sidebar opens over the page and closes again; when
    /// that was for the side panel (Ctrl+P), the sidebar comes back and the panel steps aside.
    pub(super) fn cycle_sidebar(&mut self, w: f32) {
        let l = self.frame_layout(w);
        if l.mode != Mode::Narrow && l.side_squeezed && self.frame.keep_panel {
            self.frame.keep_panel = false;
            self.frame.side_over = false;
        } else if l.mode == Mode::Narrow || l.side_squeezed {
            self.frame.side_over = !self.frame.side_over;
        } else {
            self.prefs.sidebar_folded = !self.prefs.sidebar_folded;
            self.frame.side_over = false;
            self.prefs_changed();
        }
    }

    /// The sidebar is open over the page in a window `w` wide (never while it is open beside it).
    pub(super) fn sidebar_over_open(&self, w: f32) -> bool {
        self.frame.side_over && self.frame_layout(w).side != Side::Open
    }

    /// Ctrl+P and the panel button: the side panel opens or closes. Open but stepped aside for want of
    /// room, it comes back and the sidebar folds to the strip for it instead.
    pub(super) fn toggle_side_panel(&mut self, w: f32) {
        let l = self.frame_layout(w);
        if l.mode == Mode::Narrow {
            return;
        }
        if l.panel_squeezed {
            self.frame.keep_panel = true;
        } else {
            self.prefs.show_rules_panel = !self.prefs.show_rules_panel;
            self.frame.keep_panel = false;
            self.prefs_changed();
        }
    }

    /// The side panel opens (Friends, Lyrics from a song's menu): stepped aside for want of room, it
    /// comes back with the sidebar as the strip.
    pub(super) fn open_side_panel(&mut self, w: f32) {
        if !self.prefs.show_rules_panel {
            self.prefs.show_rules_panel = true;
            self.prefs_changed();
        }
        if self.frame_layout(w).panel_squeezed {
            self.frame.keep_panel = true;
        }
    }

    pub(super) fn main_ui(&mut self, ui: &mut Ui) {
        self.typing_at_start = ui.ctx().text_edit_focused();
        self.frame.song_targets = false;
        self.track_place();
        let p = self.palette;
        let ctx = ui.ctx().clone();
        self.autoscroll(&ctx);
        self.shot_restore(&ctx);
        let w = ui.available_width();
        let l = self.frame_layout(w);
        if self.frame.shot_flags.iter().any(|f| f == "home") && !self.home_shown() {
            self.show_home();
        }
        // A picture of Settings' window: drawn here at the requested size, as it draws in its own.
        if self.frame.shot_flags.iter().any(|f| f == "settings") {
            self.settings_query.clear();
            for f in &self.frame.shot_flags {
                if let Some(page) = super::settings::Page::from_word(f) {
                    self.settings_page = page;
                } else if let Some(q) = f.strip_prefix("q=") {
                    self.settings_query = q.replace('_', " ");
                }
            }
            ui.painter().rect_filled(ui.max_rect(), CornerRadius::ZERO, p.settings_bg());
            self.settings_contents(ui);
            return;
        }
        // The playing song's album or first artist (their pages, for a picture).
        let flags = self.frame.shot_flags.clone();
        let flag = |f: &str| flags.iter().any(|x| x == f);
        let (albums, artists) = (flag("albums"), flag("artists"));
        let want = match self.playback.track.as_ref() {
            Some(t) if flag("curalbum") => Some(Source::Album(t.album_uri.clone())),
            Some(t) if flag("curartist") => t.artists.first().map(|a| Source::Artist(a.uri.clone())),
            Some(_) if flag("curlist") => self.playback.source.clone(),
            _ => None,
        };
        let off_list = !matches!(self.middle_tab, MiddleTab::Songs | MiddleTab::Blocked);
        if let Some(want) = want.filter(|w| self.view.as_ref().is_none_or(|v| v.source != *w) || off_list) {
            // A picture leaves Recently played as it was.
            let recent = self.recent_sources.clone();
            self.open_source(want, false);
            self.recent_sources = recent.clone();
            self.prefs.recent_lists = recent;
            self.prefs_changed();
        }
        if flag("help") {
            self.help_open = true;
        }
        if flag("note") && self.toasts.is_empty() {
            self.note("Shuffle rules changed", Some(super::Undo::Rules(self.rules.clone())));
        }
        if flag("opensettings") && !self.settings_open {
            self.open_settings();
        }
        if flag("menu") {
            self.shot_song_menu(pos2(12.0, 478.0));
        }
        if flag("library") && self.middle_tab != MiddleTab::Library {
            self.middle_tab = MiddleTab::Library;
        }
        if flag("playing") && self.middle_tab != MiddleTab::Playing {
            self.middle_tab = MiddleTab::Playing;
        }
        if flag("profile") && self.middle_tab != MiddleTab::Profile {
            self.show_profile();
        }
        if flag("search") && !flag("settings") {
            let q = flags.iter().find_map(|f| f.strip_prefix("q=")).map(|q| q.replace('_', " ")).unwrap_or_default();
            self.middle_tab = MiddleTab::Search;
            if let Some(k) = flags.iter().find_map(|f| f.strip_prefix("chip=")).and_then(|k| k.parse().ok()) {
                self.search.chip = k;
            }
            if self.search.input != q {
                self.search.input = q;
                self.search.typed_at = Some(std::time::Instant::now());
            }
        }
        if albums && self.middle_tab != MiddleTab::Albums {
            self.show_collection(MiddleTab::Albums);
        } else if artists && self.middle_tab != MiddleTab::Artists {
            self.show_collection(MiddleTab::Artists);
        }
        for (flag, tab) in [("lyrics", super::panel::Tab::Lyrics), ("queue", super::panel::Tab::UpNext), ("rules", super::panel::Tab::Rules)] {
            if self.frame.shot_flags.iter().any(|f| f == flag) {
                self.frame.panel_tab = tab;
            }
        }
        // The Now playing view, over everything (for pictures: `stage`, with `stage-off` for its lyrics
        // turned off).
        if flag("stage") && !self.stage.open {
            self.show_stage(&ctx);
        }
        if flag("stage-off") {
            self.stage.lyrics_off = true;
        }
        if self.stage.open {
            let full = ui.max_rect();
            self.stage_ui(ui, full);
            let lift = if self.listening() { 38.0 } else { 0.0 };
            self.listen_pill(&ctx, full.center().x, full.bottom() - 16.0);
            self.note_popup(&ctx, full.center().x, full.bottom() - 20.0 - lift);
            self.song_menu_ui(&ctx);
            self.help_ui(&ctx);
            self.window_edges(&ctx);
            return;
        }

        // Every area has a fixed place, cut to its own box, so nothing can spill over its neighbour.
        let full = ui.max_rect();
        let sign_h = if l.mode == Mode::Narrow { 48.0 } else { SIGN_H };
        let sign = Rect::from_min_max(full.min, pos2(full.right(), full.top() + sign_h));
        if l.mode == Mode::Narrow {
            // Narrow: the title bar, then the page, the mini player and the bottom tabs.
            region(ui, sign, "sign", None, |ui| self.sign_bar(ui, &l));
            let body = Rect::from_min_max(pos2(full.left(), sign.bottom()), full.max);
            self.narrow_body(ui, body, &l);
            // Above the mini player and the tabs.
            // Listening along: the pill sits above the mini player, notes above it.
            let lift = if self.listening() { 38.0 } else { 0.0 };
            self.listen_pill(&ctx, full.center().x, full.bottom() - 58.0 - 56.0 - 8.0);
            self.note_popup(&ctx, full.center().x, full.bottom() - 58.0 - 56.0 - 10.0 - lift);
            self.song_menu_ui(&ctx);
            self.help_ui(&ctx);
            self.window_edges(&ctx);
            return;
        }
        // The player bar at the foot (no status line: notes pop up above the bar, the shortcuts are
        // behind Ctrl+H).
        let bar = Rect::from_min_max(pos2(full.left(), full.bottom() - super::player::BAR_HEIGHT), full.max);
        let body = Rect::from_min_max(pos2(full.left(), sign.bottom()), pos2(full.right(), bar.top()));

        // The window's base under the cards and the player bar (the title bar paints its own).
        let base = p.base_bg();
        region(ui, Rect::from_min_max(body.min, full.max), "base", Some(base), |ui| {
            if WALL_IN_GAPS {
                self.wallpaper_behind_panel(ui);
            }
        });
        region(ui, sign, "sign", None, |ui| self.sign_bar(ui, &l));
        region(ui, bar, "player-bar", None, |ui| self.player_bar(ui, &l));

        // The sidebar, the page and the side panel are cards on the base, CARD_GAP apart and from the
        // window's sides; their edges are dragged in the gaps between them.
        let cards = Rect::from_min_max(pos2(body.left() + CARD_GAP, body.top()), pos2(body.right() - CARD_GAP, body.bottom()));
        let side = Rect::from_min_max(cards.min, pos2(cards.left() + l.side_w, cards.bottom()));
        let panel = Rect::from_min_max(pos2(cards.right() - l.panel_w, cards.top()), cards.max);
        let left = if l.side != Side::None { side.right() + CARD_GAP } else { cards.left() };
        let right = if l.panel_w > 0.0 { panel.left() - CARD_GAP } else { cards.right() };
        let main = Rect::from_min_max(pos2(left, cards.top()), pos2(right.max(left), cards.bottom()));
        if l.side != Side::None {
            self.frame.song_targets = l.side == Side::Open;
            region(ui, side, "sidebar", None, |ui| {
                ui.painter().rect_filled(side, CornerRadius::same(CARD_R), p.sidebar_bg);
                self.wallpaper_in_card(ui, false, CARD_WALL);
                match l.side {
                    Side::Open => self.sidebar_full(ui),
                    _ => self.sidebar_strip(ui),
                }
            });
            self.round_card_corners(ui.painter(), side, f32::from(CARD_R), base);
        }
        if l.panel_w > 0.0 {
            region(ui, panel, "side-panel", None, |ui| {
                ui.painter().rect_filled(panel, CornerRadius::same(CARD_R), p.sidebar_bg);
                self.wallpaper_in_card(ui, false, CARD_WALL);
                self.side_panel(ui, &l);
            });
            self.round_card_corners(ui.painter(), panel, f32::from(CARD_R), base);
        }
        self.frame.page_rect = Some(main);
        region(ui, main, "page", None, |ui| {
            ui.painter().rect_filled(main, CornerRadius::same(CARD_R), p.main_bg);
            let lift = Rect::from_min_size(main.min, vec2(main.width(), PAGE_LIFT_H.min(main.height())));
            fin::vgradient(ui.painter(), lift, p.page_lift(), p.main_bg);
            self.wallpaper_in_card(ui, true, CARD_WALL);
            self.page(ui, p.main_bg, |app, ui| app.center_view(ui));
        });
        self.round_card_corners(ui.painter(), main, f32::from(CARD_R), base);
        // The edges that drag the sidebar's and the side panel's widths (over the regions, so they win
        // over what is beside them).
        if l.side == Side::Open || (l.side == Side::Strip && !l.side_squeezed) {
            self.sash(ui, Edge::Side, side.right() + CARD_GAP / 2.0, body, &l);
        }
        if l.panel_w > 0.0 {
            self.sash(ui, Edge::Panel, panel.left() - CARD_GAP / 2.0, body, &l);
        }
        // A drag that ended where no edge saw it let go (its side gone from under it) ends here.
        if self.frame.sash.is_some() && !ui.input(|i| i.pointer.any_down()) {
            self.frame.sash = None;
        }
        if l.side != Side::Open && self.frame.side_over {
            self.sidebar_over(&ctx, Rect::from_min_max(pos2(body.left() + l.side_w, body.top()), body.max), l.side_w);
        } else {
            self.frame.side_over = false;
        }
        // Listening along: the pill sits above the controls, notes above it.
        let lift = if self.listening() { 38.0 } else { 0.0 };
        self.listen_pill(&ctx, super::player::controls_center(bar, &l), bar.top() - 8.0);
        self.note_popup(&ctx, super::player::controls_center(bar, &l), bar.top() - 12.0 - lift);
        self.song_menu_ui(&ctx);
        self.help_ui(&ctx);
        self.window_edges(&ctx);
    }

    /// The edge at `x` between the page and the sidebar (`Edge::Side`) or the side panel: dragged, its
    /// side follows the mouse within what it may be and the page's room, and keeps that width after;
    /// dragged far enough in, the sidebar folds to the strip or the panel closes (and the strip
    /// dragged out opens the sidebar again). A double click puts the default width back. A line in
    /// the accent shows while it is hovered or dragged.
    fn sash(&mut self, ui: &mut Ui, edge: Edge, x: f32, body: Rect, l: &Layout) {
        let p = self.palette;
        // The gap between the cards, and a little over each (away from their scrollbars).
        let rect = Rect::from_min_max(pos2(x - CARD_GAP / 2.0 - 1.0, body.top()), pos2(x + CARD_GAP / 2.0 + 1.0, body.bottom()));
        let id = Id::new(("sash", edge));
        let resp = ui.interact(rect, id, Sense::click_and_drag());
        // Something else being dragged (a song, a pinned sleeve) passes over it untouched.
        if ui.ctx().dragged_id().is_some_and(|d| d != id) {
            return;
        }
        let strip = edge == Edge::Side && l.side == Side::Strip;
        let (drawn, other, min, max, snap) = match edge {
            Edge::Side => (l.side_w, l.panel_w, SIDE_MIN, SIDE_MAX, SIDE_SNAP),
            Edge::Panel => (l.panel_w, l.side_w, PANEL_MIN, PANEL_MAX, PANEL_SNAP),
        };
        let sign = if edge == Edge::Side { 1.0 } else { -1.0 };
        if resp.drag_started()
            && let Some(at) = resp.interact_pointer_pos()
        {
            self.frame.sash = Some(Sash { edge, start_w: drawn, start_x: at.x, live: drawn, snapped: false });
        }
        let pointer = ui.input(|i| i.pointer.latest_pos());
        if let Some(s) = self.frame.sash.as_mut().filter(|s| s.edge == edge)
            && let Some(at) = pointer
        {
            let wanted = s.start_w + sign * (at.x - s.start_x);
            if strip {
                // The strip opens the sidebar once dragged out far enough.
                s.snapped = wanted > STRIP_SNAP;
                s.live = clamp_width(self.prefs.library_width, SIDE_W, SIDE_MIN, SIDE_MAX);
            } else {
                // As wide as it may be, while the other side keeps its width and the page its room.
                let most = (l.w - PAGE_MIN - other).min(max).max(min);
                s.snapped = wanted < snap;
                s.live = wanted.clamp(min, most).round();
            }
        }
        if resp.drag_stopped()
            && let Some(s) = self.frame.sash.take()
        {
            match (edge, strip, s.snapped) {
                (Edge::Side, true, true) => self.prefs.sidebar_folded = false,
                (Edge::Side, true, false) => {}
                (Edge::Side, false, true) => self.prefs.sidebar_folded = true,
                (Edge::Panel, _, true) => {
                    self.prefs.show_rules_panel = false;
                    self.frame.keep_panel = false;
                }
                (Edge::Side, false, false) => self.prefs.library_width = Some(s.live),
                (Edge::Panel, _, false) => self.prefs.deck_width = Some(s.live),
            }
            self.prefs_changed();
        }
        if resp.double_clicked() {
            match edge {
                Edge::Side => {
                    self.prefs.library_width = None;
                    self.prefs.sidebar_folded = false;
                }
                Edge::Panel => self.prefs.deck_width = None,
            }
            self.prefs_changed();
        }
        let dragging = self.frame.sash.is_some_and(|s| s.edge == edge);
        if resp.hovered() || dragging {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeColumn);
            // Past the snap, the line dims: letting go folds or closes.
            let snapped = self.frame.sash.is_some_and(|s| s.edge == edge && s.snapped && !strip);
            let color = if snapped { p.text_weak } else { p.accent_ink };
            ui.painter().with_clip_rect(body).vline(x, body.y_range(), Stroke::new(2.0, color));
        }
        if dragging {
            ui.ctx().request_repaint();
        }
    }

    /// The 56 strip (`.strip`): Home, Albums, Artists, a line, then every playlist as a small sleeve.
    fn sidebar_strip(&mut self, ui: &mut Ui) {
        let p = self.palette;
        let r = ui.max_rect();
        let mut y = r.top() + 10.0;
        let home = self.home_shown();
        let (albums_on, artists_on) = (self.middle_tab == MiddleTab::Albums, self.middle_tab == MiddleTab::Artists);
        for (icon, tip, on) in [("home", "Home", home), ("album", "Albums", albums_on), ("artist", "Artists", artists_on)] {
            let rect = Rect::from_min_size(pos2(r.center().x - 18.0, y), vec2(36.0, 34.0));
            let look = if on { Look::OnBox } else { Look::Plain };
            if fin::ib(ui, &p, rect, Id::new(("strip-nav", icon)), icon, look, tip).clicked() {
                match icon {
                    "home" => self.show_home(),
                    "album" => self.show_collection(MiddleTab::Albums),
                    _ => self.show_collection(MiddleTab::Artists),
                }
            }
            y += 34.0 + 6.0;
        }
        ui.painter().hline((r.center().x - 15.0)..=(r.center().x + 15.0), y + 3.0, Stroke::new(1.0, p.border));
        y += 7.0 + 6.0;
        // As in the sidebar: Liked Songs and the pinned playlists first, then the rest.
        let mut lists: Vec<(Source, Option<String>, String)> = Vec::new();
        // A line after the pinned ones (`.strip .sep`), when there are others after them.
        if self.prefs.liked_pinned {
            lists.push((Source::LikedSongs, None, "Liked Songs".to_owned()));
        }
        for uri in &self.prefs.pinned_playlists {
            if let Some(pl) = self.playlist(uri) {
                lists.push((Source::Playlist(pl.uri.clone()), pl.image_url.clone(), pl.name.clone()));
            }
        }
        let pinned = lists.len();
        for pl in self.playlists.iter().filter(|pl| !self.prefs.pinned_playlists.contains(&pl.uri)) {
            lists.push((Source::Playlist(pl.uri.clone()), pl.image_url.clone(), pl.name.clone()));
        }
        let open = self.view.as_ref().map(|v| v.source.clone()).filter(|_| !home);
        let playing = self.playback.source.clone().filter(|_| self.playback.track.is_some());
        let total = lists.len();
        for (k, (source, image, name)) in lists.into_iter().enumerate() {
            if y > r.bottom() {
                break;
            }
            if k == pinned && k > 0 && k < total {
                ui.painter().hline((r.center().x - 15.0)..=(r.center().x + 15.0), y + 3.0, Stroke::new(1.0, p.border));
                y += 7.0 + 6.0;
            }
            let cell = Rect::from_min_size(pos2(r.center().x - 23.0, y), vec2(46.0, 42.0));
            let resp = ui.interact(cell, Id::new(("strip-pl", source.cache_key())), Sense::click()).on_hover_cursor(CursorIcon::PointingHand).on_hover_text(&name);
            if open.as_ref() == Some(&source) {
                fin::paint_chosen(ui.painter(), &p, cell);
            } else if resp.hovered() {
                ui.painter().rect_filled(cell, CornerRadius::same(6), p.hover_bg);
            }
            // Every list as a small sleeve with its record peeking out; the playing one further out.
            let cover = Rect::from_min_size(pos2(cell.left() + 3.0, cell.center().y - 17.0), vec2(34.0, 34.0));
            let sleeve = super::sidebar::Sleeve { source: source.clone(), name: name.clone(), image: image.clone(), count: None, uri: None, editable: false };
            self.paint_sleeve(ui, cover, &sleeve, playing.as_ref() == Some(&source), resp.hovered(), 0.2);
            // As in the open sidebar: songs dropped on one of the user's playlists go into it.
            if let Source::Playlist(uri) = &source
                && self.playlist(uri).is_some_and(|pl| pl.editable)
                && let Some(songs) = super::dnd::drop_on(ui, &resp, cell, &p)
            {
                self.add_dropped_songs(uri.clone(), songs.uris);
            }
            if resp.clicked() {
                self.open_source(source, false);
            }
            y += 42.0 + 6.0;
        }
    }

    /// The 300 sidebar over the page, with a shade over the rest (a click on it, or Esc, closes it).
    fn sidebar_over(&mut self, ctx: &egui::Context, body: Rect, left: f32) {
        let area = Rect::from_min_max(pos2(body.left() - left, body.top()), body.max);
        let shade = egui::Area::new(Id::new("side-shade")).order(Order::Middle).fixed_pos(area.min).show(ctx, |ui| {
            let (rect, resp) = ui.allocate_exact_size(area.size(), Sense::click());
            ui.painter().rect_filled(rect, CornerRadius::ZERO, Color32::from_black_alpha(140));
            resp
        });
        if shade.inner.clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.frame.side_over = false;
        }
        let p = self.palette;
        egui::Area::new(Id::new("side-over")).order(Order::Foreground).fixed_pos(area.min).show(ctx, |ui| {
            let w = clamp_width(self.prefs.library_width, SIDE_W, SIDE_MIN, SIDE_MAX);
            let rect = Rect::from_min_size(area.min + vec2(CARD_GAP, 0.0), vec2(w, area.height()));
            let r = CornerRadius::same(CARD_R);
            ui.painter().add(egui::Shadow { offset: [18, 0], blur: 40, spread: 0, color: Color32::from_black_alpha(150) }.as_shape(rect, r));
            ui.painter().rect_filled(rect, r, p.sidebar_bg);
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
            child.set_clip_rect(rect);
            self.sidebar_full(&mut child);
        });
    }

    /// The title bar (`.sign`): the chrome logo and wordmark, the sidebar button, back and forward, the
    /// search box in the middle, the avatar and the window buttons. Dragging its empty part moves the
    /// window; a double click maximises it.
    fn sign_bar(&mut self, ui: &mut Ui, l: &Layout) {
        // The window's base (Islands) in both themes, as the player bar under the cards.
        let p = self.palette;
        let r = ui.max_rect();
        let narrow = l.mode == Mode::Narrow;
        ui.painter().rect_filled(r, CornerRadius::ZERO, p.base_bg());

        let wc_left = window_controls(ui, r, narrow, !p.dark);
        let cy = r.center().y;

        // Avatar: 28, 10 before the window buttons.
        let ava = Rect::from_center_size(pos2(wc_left - 10.0 - 14.0, cy), vec2(28.0, 28.0));
        let ava_resp = ui.interact(ava, Id::new("sign-avatar"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        let name = self.profile.as_ref().map(|u| u.display_name.clone()).unwrap_or_default();
        let url = self.profile.as_ref().and_then(|u| u.image_url.clone());
        if !self.images.paint(ui, url.as_deref(), ava, 14, Color32::from_rgb(232, 80, 106)) {
            svgicon::paint(ui.painter(), ava.shrink(5.0), "artist", Color32::from_white_alpha(200), svgicon::STROKE);
        }
        ui.painter().circle_stroke(ava.center(), 14.5, Stroke::new(1.0, p.border));
        let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&ava_resp));
        if open {
            ui.painter().circle_stroke(ava.center(), 15.5, Stroke::new(1.5, p.accent_ink));
        }
        let ava_resp = if open { ava_resp } else { ava_resp.on_hover_text(if name.is_empty() { "Account".to_owned() } else { name.clone() }) };
        self.account_menu(ui, &ava_resp, &name);

        // Logo and wordmark: 14 in from the left.
        let logo_d = if narrow { 26.0 } else { 30.0 };
        let logo = Rect::from_min_size(pos2(r.left() + 14.0, cy - logo_d / 2.0), vec2(logo_d, logo_d));
        svgicon::paint(ui.painter(), logo, "logo", Color32::WHITE, svgicon::STROKE);
        let wm_scale = if narrow { 15.0 / 19.0 } else { 1.0 };
        let wm_size = WORDMARK_SIZE * wm_scale;
        let wm = Rect::from_min_size(pos2(logo.right() + 10.0, cy - wm_size.y / 2.0 + 1.0), wm_size);
        let texture = self.frame.wordmark.get_or_insert_with(|| super::login::png_texture(ui.ctx(), "wordmark-bar", WORDMARK_BAR)).clone();
        // Light chrome on the dark base; on the light one the same chrome darkened (its white
        // parts would vanish there).
        let tint = if p.dark { Color32::WHITE } else { Color32::from_rgb(70, 74, 88) };
        ui.painter().image(texture.id(), wm, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), tint);
        let brand = ui.interact(logo.union(wm), Id::new("sign-brand"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Home");
        if brand.clicked() {
            self.show_home();
        }
        if narrow {
            return;
        }

        // The sidebar button: at the open sidebar's right end (8 in), 6 after the wordmark otherwise.
        let side_x = if l.side == Side::Open { (r.left() + l.side_w - 38.0).max(wm.right() + 6.0 + 4.0) } else { wm.right() + 6.0 + 4.0 };
        let side = Rect::from_min_size(pos2(side_x, cy - 15.0), vec2(30.0, 30.0));
        let look = if l.side != Side::Open && self.frame.side_over { Look::OnBox } else { Look::Plain };
        if fin::ib(ui, &p, side, Id::new("sign-side"), "side", look, "Sidebar (Ctrl+B)").clicked() {
            self.cycle_sidebar(l.w);
        }

        // Back and forward, 4 after the zone.
        let back = Rect::from_min_size(pos2(side.right() + 4.0, cy - 15.0), vec2(30.0, 30.0));
        let fwd = back.translate(vec2(30.0, 0.0));
        let look = |on: bool| if on { Look::Plain } else { Look::Dim };
        if fin::ib(ui, &p, back, Id::new("sign-back"), "back", look(self.can_go_back()), "Back (Alt+Left)").clicked() {
            self.go_back();
        }
        if fin::ib(ui, &p, fwd, Id::new("sign-fwd"), "fwd", look(self.can_go_forward()), "Forward (Alt+Right)").clicked() {
            self.go_forward();
        }

        // The search box, centred in the space between forward and the avatar.
        let from = fwd.right() + 4.0;
        // The "Update ready" pill (after Later), between the search box and the avatar.
        let edge = ava.left() - 8.0;
        let pill = self.update_pill(ui, edge, cy);
        let to = if pill < edge { pill - 8.0 } else { edge };
        let sw = l.search_w.min((to - from - 8.0).max(120.0));
        let left = from + ((to - from) - sw) / 2.0;
        let rect = Rect::from_min_size(pos2(left, cy - 15.0), vec2(sw, 30.0));
        let hint = if sw >= 360.0 {
            "Search songs, albums, artists, lyrics"
        } else if sw >= 260.0 {
            "Search songs, albums, artists"
        } else {
            "Search"
        };
        let id = Id::new(super::search::SEARCH_BOX_ID);
        let mut text = std::mem::take(&mut self.search.input);
        let sb = fin::search_box(ui, &p, rect, id, &mut text, hint, "Ctrl+K");
        self.search.input = text;
        let left_with = |key| sb.resp.lost_focus() && ui.input(|i| i.key_pressed(key));
        let (enter, esc) = (left_with(egui::Key::Enter), left_with(egui::Key::Escape));
        self.search_box_events(sb.resp.changed(), sb.resp.gained_focus(), enter, esc);
        self.search_logic(&ui.ctx().clone());
    }

    /// Takes the middle button's presses and releases on the page out of egui's input (see
    /// [`FrameState::middle_down`]): called before each pass with the raw input.
    pub(super) fn take_middle_button(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.frame.middle_pressed = None;
        let on_page = |pos: egui::Pos2| self.frame.page_rect.is_some_and(|r| r.contains(pos)) && !egui::Popup::is_any_open(ctx);
        raw.events.retain(|e| match e {
            egui::Event::PointerButton { button: egui::PointerButton::Middle, pressed: true, pos, .. } if on_page(*pos) => {
                self.frame.middle_down = true;
                self.frame.middle_pressed = Some(*pos);
                false
            }
            egui::Event::PointerButton { button: egui::PointerButton::Middle, pressed: false, .. } if self.frame.middle_down => {
                self.frame.middle_down = false;
                false
            }
            _ => true,
        });
    }

    /// Middle-click scrolling, as in a browser: a middle click on the page sets a mark; the page
    /// then scrolls toward the pointer, faster the further it is, whether the button is held or
    /// was let go. Any click or Esc ends it (and letting go of a middle button that was held and
    /// moved).
    fn autoscroll(&mut self, ctx: &egui::Context) {
        let (pos, pressed, esc, dt) = ctx.input(|i| (i.pointer.hover_pos(), i.pointer.any_pressed(), i.key_pressed(egui::Key::Escape), i.stable_dt.min(0.05)));
        let (middle_down, middle_pressed) = (self.frame.middle_down, self.frame.middle_pressed);
        if let Some(a) = &mut self.frame.autoscroll {
            let moved = pos.is_some_and(|p| (p - a.at).length() > 12.0);
            if middle_down && moved && a.since.elapsed().as_millis() > 200 {
                a.held = true;
            }
            let let_go = a.held && !middle_down;
            let again = middle_pressed.is_some() && a.since.elapsed().as_millis() > 50;
            if (pressed && a.since.elapsed().as_millis() > 50) || again || esc || let_go || pos.is_none() {
                self.frame.autoscroll = None;
                return;
            }
        } else if let Some(p) = middle_pressed {
            self.frame.autoscroll = Some(AutoScroll { at: p, since: std::time::Instant::now(), held: false });
        }
        let Some(a) = &self.frame.autoscroll else { return };
        let Some(p) = pos else { return };
        let dy = p.y - a.at.y;
        let dead = 10.0;
        let speed = (dy.abs() - dead).max(0.0) * 9.0;
        if speed > 0.0 {
            let step = speed * dt * dy.signum();
            ctx.input_mut(|i| i.smooth_scroll_delta.y -= step);
        }
        ctx.set_cursor_icon(if dy < -dead { CursorIcon::ResizeNorth } else if dy > dead { CursorIcon::ResizeSouth } else { CursorIcon::AllScroll });
        // The mark: a small round button with an arrow up and down.
        let painter = ctx.layer_painter(egui::LayerId::new(Order::Tooltip, Id::new("autoscroll")));
        painter.circle_filled(a.at, 14.0, Color32::from_black_alpha(190));
        painter.circle_stroke(a.at, 14.0, Stroke::new(1.0, Color32::from_white_alpha(120)));
        let up = [pos2(a.at.x, a.at.y - 9.0), pos2(a.at.x - 4.0, a.at.y - 4.0), pos2(a.at.x + 4.0, a.at.y - 4.0)];
        let down = [pos2(a.at.x, a.at.y + 9.0), pos2(a.at.x + 4.0, a.at.y + 4.0), pos2(a.at.x - 4.0, a.at.y + 4.0)];
        for tri in [up, down] {
            painter.add(egui::Shape::convex_polygon(tri.to_vec(), Color32::WHITE, Stroke::NONE));
        }
        painter.circle_filled(a.at, 1.6, Color32::WHITE);
        ctx.request_repaint();
    }

    /// The page (`draw`), with the page animation when another page was just opened (not when the
    /// same one reloads): it fades in as it lifts into place (Rise), or shows from the top down (Top
    /// first). With Windows' animations off both only fade.
    pub(super) fn page(&mut self, ui: &mut Ui, bg: Color32, draw: impl FnOnce(&mut Self, &mut Ui)) {
        use crate::config::PageAnimation;
        let now = self.place();
        let switched = self.frame.page_seen.as_ref().is_some_and(|seen| *seen != now);
        if self.frame.page_seen.is_none() || switched {
            self.frame.page_since = switched.then(std::time::Instant::now);
            self.frame.page_seen = Some(now);
        }
        let anim = self.prefs.page_animation;
        let t = self.frame.page_since.map(|since| since.elapsed().as_secs_f32() / motion::secs(motion::PAGE).max(0.001));
        let Some(t) = t.filter(|&t| t < 1.0 && anim != PageAnimation::None) else {
            self.frame.page_since = None;
            draw(self, ui);
            return;
        };
        ui.ctx().request_repaint();
        let t = motion::ease_out(t);
        if anim == PageAnimation::TopFirst && motion::moves() {
            draw(self, ui);
            let rect = ui.clip_rect().intersect(ui.max_rect());
            ui.painter().add(top_first_cover(rect, bg, t));
            return;
        }
        let was = ui.opacity();
        ui.multiply_opacity(t);
        let by = egui::vec2(0.0, motion::offset(motion::PAGE_RISE, t, motion::moves()));
        motion::moved_within(ui, by, |ui| draw(self, ui));
        ui.set_opacity(was);
    }

    /// The menu under the avatar (like Spotify's): the name, then Profile, Settings, Shortcuts and
    /// Log out.
    fn account_menu(&mut self, ui: &mut Ui, resp: &egui::Response, name: &str) {
        enum Pick {
            Profile,
            Settings,
            Shortcuts,
            LogOut,
        }
        let mut pick = None;
        egui::Popup::menu(resp)
            .align(egui::RectAlign::BOTTOM_END)
            .align_alternatives(&[egui::RectAlign::BOTTOM_START])
            .gap(6.0)
            .show(|ui| {
                ui.set_width(200.0);
                if !name.is_empty() {
                    fin::menu_label(ui, name);
                }
                let items = [
                    ("person", "Profile", None, Pick::Profile),
                    ("gear", "Settings", Some("Ctrl+,"), Pick::Settings),
                    ("keys", "Shortcuts", Some("Ctrl+H"), Pick::Shortcuts),
                ];
                for (icon, label, key, what) in items {
                    if fin::menu_item(ui, Some(icon), label, key, false, true).clicked() {
                        pick = Some(what);
                        ui.close();
                    }
                }
                fin::menu_sep(ui);
                if fin::menu_item(ui, Some("back"), "Log out", None, false, self.profile.is_some()).clicked() {
                    pick = Some(Pick::LogOut);
                    ui.close();
                }
            });
        match pick {
            Some(Pick::Profile) => self.show_profile(),
            Some(Pick::Settings) => {
                if !self.settings_open {
                    self.open_settings();
                }
            }
            Some(Pick::Shortcuts) => {
                if !self.help_open {
                    let ctx = ui.ctx().clone();
                    self.toggle_help(&ctx);
                }
            }
            Some(Pick::LogOut) => {
                if self.settings_open {
                    self.close_settings();
                }
                self.send(crate::model::Command::Logout);
            }
            None => {}
        }
    }

    /// Self-shots leave the window as they found it: before the first, the page and windows are
    /// noted; two seconds after the last, they are put back (the Shortcuts window, Now playing and a
    /// song menu opened for a picture close again).
    fn shot_restore(&mut self, ctx: &egui::Context) {
        if !self.frame.shot_flags.is_empty() {
            if self.frame.shot_restore.is_none() {
                self.frame.shot_restore = Some(ShotRestore {
                    tab: self.middle_tab,
                    source: self.view.as_ref().map(|v| v.source.clone()),
                    help: self.help_open,
                    settings: self.settings_open,
                    stage: self.stage.open,
                    lyrics_off: self.stage.lyrics_off,
                });
            }
            self.frame.shot_ended_at = None;
            return;
        }
        let Some(at) = self.frame.shot_ended_at else { return };
        let wait = std::time::Duration::from_secs(2);
        if at.elapsed() < wait {
            ctx.request_repaint_after(wait - at.elapsed());
            return;
        }
        self.frame.shot_ended_at = None;
        let Some(r) = self.frame.shot_restore.take() else { return };
        self.help_open = r.help;
        self.song_menu = None;
        if self.settings_open && !r.settings {
            self.close_settings();
        }
        // Now playing, opened for a picture, closes again.
        if self.stage.open && !r.stage {
            self.close_stage(ctx);
        }
        self.stage.lyrics_off = r.lyrics_off;
        if let Some(source) = r.source.filter(|s| self.view.as_ref().is_none_or(|v| v.source != *s)) {
            let recent = self.recent_sources.clone();
            self.open_source(source, false);
            self.recent_sources = recent.clone();
            self.prefs.recent_lists = recent;
            self.prefs_changed();
        }
        self.middle_tab = r.tab;
    }

    /// Whether the self-shot being taken asked for `flag`.
    pub(super) fn shot_flag(&self, flag: &str) -> bool {
        self.frame.shot_flags.iter().any(|f| f == flag)
    }

    /// The window's edges resize it (it has no Windows frame): 5 px in from each side, 10 at the corners.
    pub(super) fn window_edges(&self, ctx: &egui::Context) {
        if !self.shell.mini {
            // The Now playing view keeps its shape: only its corners size it.
            super::settings::resize_edges(ctx, "main", self.stage.open);
        }
    }

    /// Ctrl+Z: undoes what the newest note offers to undo.
    pub(super) fn undo_newest(&mut self) {
        if let Some(i) = self.toasts.iter().rposition(|t| t.undo.is_some()) {
            let t = self.toasts.remove(i);
            if let Some(u) = t.undo {
                self.apply_undo(u);
            }
        }
    }
}

/// Top first: a cover over the page whose soft top edge moves down as `t` goes 0 to 1, so the
/// header shows first and the rows after it, one by one.
fn top_first_cover(rect: Rect, bg: Color32, t: f32) -> egui::Shape {
    const SOFT: f32 = 140.0;
    let edge = rect.top() - SOFT + t * (rect.height() + SOFT);
    let soft_end = (edge + SOFT).min(rect.bottom());
    let mut mesh = egui::Mesh::default();
    let mut band = |top: f32, bottom: f32, a: Color32, b: Color32| {
        if bottom <= top {
            return;
        }
        let i = mesh.vertices.len() as u32;
        mesh.colored_vertex(pos2(rect.left(), top), a);
        mesh.colored_vertex(pos2(rect.right(), top), a);
        mesh.colored_vertex(pos2(rect.left(), bottom), b);
        mesh.colored_vertex(pos2(rect.right(), bottom), b);
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i + 1, i + 3, i + 2);
    };
    let top = edge.max(rect.top());
    let at = |y: f32| bg.gamma_multiply(((y - edge) / SOFT).clamp(0.0, 1.0));
    band(top, soft_end, at(top), at(soft_end));
    band(soft_end, rect.bottom(), bg, bg);
    egui::Shape::mesh(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn want(side: f32, panel: f32) -> Want {
        Want { side, panel, ..Want::default() }
    }

    fn widths(l: Layout) -> (Side, f32, f32) {
        (l.side, l.side_w, l.panel_w)
    }

    #[test]
    fn each_side_keeps_its_width_while_there_is_room() {
        assert_eq!(widths(layout(1440.0, Want::default())), (Side::Open, 300.0, 340.0));
        // The widths in prip's settings, at their window (1080 px at 85 %).
        assert_eq!(widths(layout(1270.0, want(299.0, 333.0))), (Side::Open, 299.0, 333.0));
        assert_eq!(widths(layout(2400.0, want(420.0, 480.0))), (Side::Open, 420.0, 480.0));
    }

    #[test]
    fn the_panel_gives_way_first_then_the_sidebar() {
        // The panel narrows first...
        assert_eq!(widths(layout(1080.0, want(299.0, 333.0))), (Side::Open, 299.0, 301.0));
        // ...then the sidebar...
        assert_eq!(widths(layout(1000.0, want(299.0, 333.0))), (Side::Open, 260.0, 260.0));
        // ...then the panel steps aside and the sidebar has its width back.
        let l = layout(960.0, want(299.0, 333.0));
        assert_eq!(widths(l), (Side::Open, 299.0, 0.0));
        assert!(l.panel_squeezed && !l.side_squeezed);
    }

    #[test]
    fn a_kept_panel_folds_the_sidebar_instead() {
        let l = layout(960.0, Want { keep_panel: true, ..want(299.0, 333.0) });
        assert_eq!(widths(l), (Side::Strip, 56.0, 333.0));
        assert!(l.side_squeezed && !l.panel_squeezed);
    }

    #[test]
    fn the_widths_come_back_with_the_room() {
        let w = want(299.0, 333.0);
        let before = widths(layout(1270.0, w));
        assert_ne!(widths(layout(900.0, w)), before);
        assert_eq!(widths(layout(1270.0, w)), before);
    }

    #[test]
    fn the_sidebar_never_folds_on_its_own_and_the_page_keeps_its_room() {
        for want in [Want::default(), want(420.0, 480.0), want(240.0, 260.0), Want { panel_open: false, ..Want::default() }] {
            for k in 0..=1640 {
                let w = 760.0 + k as f32;
                let l = layout(w, want);
                assert_eq!(l.side, Side::Open, "at {w}");
                assert!(l.side_w >= SIDE_MIN && l.side_w <= SIDE_MAX, "at {w}");
                assert!(l.panel_w == 0.0 || (PANEL_MIN..=PANEL_MAX).contains(&l.panel_w), "at {w}");
                assert!(w - l.side_w - l.panel_w >= PAGE_MIN - 1.0, "at {w}: {l:?}");
            }
        }
    }

    #[test]
    fn widths_out_of_range_are_kept_in_it() {
        assert_eq!(widths(layout(2400.0, want(10.0, 9999.0))), (Side::Open, 240.0, 480.0));
        assert_eq!(widths(layout(2400.0, want(f32::NAN, f32::INFINITY))), (Side::Open, 300.0, 340.0));
        assert_eq!(clamp_width(None, SIDE_W, SIDE_MIN, SIDE_MAX), 300.0);
        assert_eq!(clamp_width(Some(333.4), PANEL_W, PANEL_MIN, PANEL_MAX), 333.0);
    }

    #[test]
    fn folded_is_the_strip_and_narrow_has_neither() {
        assert_eq!(widths(layout(1440.0, Want { folded: true, ..Want::default() })), (Side::Strip, 56.0, 340.0));
        assert_eq!(widths(layout(900.0, Want { folded: true, ..Want::default() })), (Side::Strip, 56.0, 340.0));
        assert_eq!(widths(layout(759.0, Want::default())), (Side::None, 0.0, 0.0));
        assert_eq!(layout(759.0, Want::default()).mode, Mode::Narrow);
        // Closed, the panel takes nothing and isn't squeezed.
        let l = layout(1000.0, Want { panel_open: false, ..Want::default() });
        assert_eq!(widths(l), (Side::Open, 300.0, 0.0));
        assert!(!l.panel_squeezed);
    }

    #[test]
    fn the_search_box_shrinks_with_the_window() {
        assert_eq!(layout(1440.0, Want::default()).search_w, 420.0);
        assert_eq!(layout(1300.0, Want::default()).search_w, 420.0);
        assert_eq!(layout(1200.0, Want::default()).search_w, 420.0);
        assert_eq!(layout(960.0, Want::default()).search_w, 280.0);
        assert_eq!(layout(830.0, Want::default()).search_w, 240.0);
        assert_eq!(layout(780.0, Want::default()).search_w, 200.0);
    }
}
