//! Discord "Listening to" status over Discord's local IPC pipe, on its own thread.
//!
//! Only a plain activity is ever sent: two lines about the song, a picture, a progress bar and,
//! if the user asks for it, an "Open on Spotify" link button. No party,
//! join/spectate secrets or instance, so nobody can join or "listen along".

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use discord_rich_presence::activity::{Activity, ActivityType, Assets, Button, StatusDisplayType, Timestamps};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient};
use serde_json::{Value, json};

use super::state::{truncate_utf16, utf16_len};
use crate::config::{DiscordLook, DiscordPaused, DiscordPicture, DiscordShows};

/// Retry interval while Discord isn't running.
const RETRY_INTERVAL: Duration = Duration::from_secs(15);
/// Retry interval after Discord refused the application id (it won't change by itself).
const REJECTED_RETRY_INTERVAL: Duration = Duration::from_secs(120);
/// Wait this long after a change before sending, so quick skips collapse into one update.
const DEBOUNCE: Duration = Duration::from_millis(700);
/// While nothing changes, the status is sent again this often. Discord forgets it when it quits
/// or restarts, and an idle pipe only shows it has closed when something is written to it.
const KEEPALIVE: Duration = Duration::from_secs(30);
/// Discord allows about 5 activity updates per 20 seconds.
const RATE_MAX: usize = 5;
const RATE_WINDOW: Duration = Duration::from_secs(20);
/// Discord text fields must be 2..=128 characters.
const TEXT_MIN: usize = 2;
const TEXT_MAX: usize = 128;
/// Longest image URL Discord accepts.
const IMAGE_URL_MAX: usize = 256;
/// Frames to read while waiting for the reply to one command (skips unrelated events).
const MAX_FRAMES_PER_REPLY: usize = 16;

/// What to show; `None` clears the status. Built by [`presence_for`] with the settings applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Presence {
    /// The two lines, already fitted to Discord's limits.
    pub top: String,
    pub bottom: Option<String>,
    pub shows: DiscordShows,
    /// The album cover (`None`: the application's own picture) and its hover text.
    pub cover: Option<String>,
    pub cover_text: Option<String>,
    /// The small play/pause sign on the cover: `Some(true)` playing, `Some(false)` paused.
    pub badge: Option<bool>,
    /// Unix ms when the song started (adjusted for seeks); `None` shows no time at all.
    pub start_ms: Option<i64>,
    /// Unix ms when it ends; `None` hides the progress bar.
    pub end_ms: Option<i64>,
    /// "Open on Spotify": the song's web page.
    pub link: Option<String>,
}

/// The song the status is about.
pub struct Song<'a> {
    pub uri: &'a str,
    pub title: &'a str,
    pub artist: &'a str,
    pub album: &'a str,
    /// The list it plays from ("" when unknown).
    pub playlist: &'a str,
    pub cover: Option<&'a str>,
}

/// The art asset keys of the small sign. They must be uploaded under these names in the Discord
/// application's Rich Presence → Art Assets, or Discord shows no sign.
pub const BADGE_PLAYING: &str = "playing";
pub const BADGE_PAUSED: &str = "paused";
/// The link button's words (Discord allows 32 characters).
pub const BUTTON_LABEL: &str = "Open on Spotify";

/// What Discord should show for `song`, with the user's `look`. `timing` is (start, end) in Unix
/// ms; `playing` false means paused. `None`: show nothing (paused, with "clear when paused").
pub fn presence_for(song: &Song, playing: bool, timing: (i64, Option<i64>), look: &DiscordLook) -> Option<Presence> {
    if !playing && look.paused == DiscordPaused::Clear {
        return None;
    }
    let top = discord_text(&fill_line(&look.top, song))
        .or_else(|| discord_text(song.title))
        .unwrap_or_else(|| "Unknown song".to_owned());
    let mut bottom = discord_text(&fill_line(&look.bottom, song));
    if !playing {
        bottom = discord_text(&bottom.map_or_else(|| "Paused".to_owned(), |b| format!("Paused · {b}")));
    }
    let cover = match look.picture {
        DiscordPicture::Cover => song.cover.filter(|url| url.starts_with("https://") && url.len() <= IMAGE_URL_MAX),
        DiscordPicture::AppIcon => None,
    };
    let (start_ms, end_ms) = match (playing, look.progress) {
        (true, true) => (Some(timing.0), timing.1.filter(|end| *end > timing.0)),
        // Paused, or no progress bar: no time at all (a start alone shows a running clock).
        _ => (None, None),
    };
    Some(Presence {
        top,
        bottom,
        shows: look.shows,
        cover: cover.map(str::to_owned),
        cover_text: cover.and_then(|_| discord_text(song.album)),
        badge: (look.badge && cover.is_some()).then_some(playing),
        start_ms,
        end_ms,
        link: look.button.then(|| song.uri.strip_prefix("spotify:track:")).flatten().filter(|id| !id.is_empty())
            .map(|id| format!("https://open.spotify.com/track/{id}")),
    })
}

/// A line template with {title} {artist} {album} {playlist} filled in. When a part is empty, the
/// separators left dangling at either end ("Song · ") go too.
pub fn fill_line(template: &str, song: &Song) -> String {
    let parts = [("{title}", song.title), ("{artist}", song.artist), ("{album}", song.album), ("{playlist}", song.playlist)];
    let mut out = template.to_owned();
    let mut gap = false;
    for (key, value) in parts {
        if out.contains(key) {
            gap |= value.trim().is_empty();
            out = out.replace(key, value.trim());
        }
    }
    let out = out.trim();
    if gap { out.trim_matches(|c: char| c.is_whitespace() || "·•-–—|,:/".contains(c)).to_owned() } else { out.to_owned() }
}


/// Connection state shown in Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscordState {
    Off,
    MissingAppId,
    InvalidAppId,
    Connecting,
    NotRunning,
    Connected,
    Rejected,
    /// Connected, but Discord answered the last status update with an error.
    UpdateRejected,
    /// The background connection couldn't be started.
    Failed,
}

impl DiscordState {
    pub fn describe(&self) -> &'static str {
        match self {
            DiscordState::Off => "Off",
            DiscordState::MissingAppId => "Add your Discord application ID to turn this on",
            DiscordState::InvalidAppId => {
                "That application ID doesn't look right. It's the long number shown as \"Application ID\" in the Discord Developer Portal."
            }
            DiscordState::Connecting => "Connecting to Discord…",
            DiscordState::NotRunning => "Discord isn't running. coda will connect when it starts.",
            DiscordState::Connected => "Connected",
            DiscordState::Rejected => "Discord didn't accept this application ID. Check it in the Discord Developer Portal.",
            DiscordState::UpdateRejected => {
                "Connected, but Discord didn't accept the status. Check the application ID in the Discord Developer Portal."
            }
            DiscordState::Failed => "Couldn't start the Discord status. Try turning it off and on again.",
        }
    }
}

/// Check a Discord application id: digits only, the length of a Discord id. Returns it trimmed.
pub fn validate_app_id(raw: &str) -> Result<String, DiscordState> {
    let id = raw.trim();
    if id.is_empty() {
        return Err(DiscordState::MissingAppId);
    }
    if !(16..=21).contains(&id.len()) || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(DiscordState::InvalidAppId);
    }
    Ok(id.to_owned())
}

/// Make text acceptable to Discord: trimmed, 2..=128 characters. `None` when there's no text.
pub fn discord_text(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut out = truncate_utf16(text, TEXT_MAX);
    while utf16_len(&out) < TEXT_MIN {
        // A blank (non-whitespace) character, so one-letter titles still pass validation.
        out.push('\u{2800}');
    }
    Some(out)
}

/// The activity payload. Deliberately has no party, secrets or join/spectate: at most one plain
/// link button, and only when the user turned it on.
pub fn activity_json(presence: &Presence) -> Value {
    let shows = match presence.shows {
        DiscordShows::App => StatusDisplayType::Name,
        DiscordShows::Song => StatusDisplayType::Details,
        DiscordShows::Artist => StatusDisplayType::State,
    };
    let mut activity = Activity::new()
        .activity_type(ActivityType::Listening)
        .status_display_type(shows)
        .details(presence.top.as_str());
    if let Some(bottom) = &presence.bottom {
        activity = activity.state(bottom.as_str());
    }
    if let Some(cover) = &presence.cover {
        let mut assets = Assets::new().large_image(cover.as_str());
        if let Some(text) = &presence.cover_text {
            assets = assets.large_text(text.as_str());
        }
        if let Some(playing) = presence.badge {
            let (key, text) = if playing { (BADGE_PLAYING, "Playing") } else { (BADGE_PAUSED, "Paused") };
            assets = assets.small_image(key).small_text(text);
        }
        activity = activity.assets(assets);
    }
    if let Some(start) = presence.start_ms {
        let mut timestamps = Timestamps::new().start(start);
        if let Some(end) = presence.end_ms {
            timestamps = timestamps.end(end);
        }
        activity = activity.timestamps(timestamps);
    }
    if let Some(link) = &presence.link {
        activity = activity.buttons(vec![Button::new(BUTTON_LABEL, link.as_str())]);
    }
    serde_json::to_value(activity).unwrap_or(Value::Null)
}

/// Sliding-window limiter: at most `max` sends per `window`.
#[derive(Debug)]
pub struct RateLimiter {
    sent: VecDeque<Instant>,
    max: usize,
    window: Duration,
}

impl RateLimiter {
    pub fn new(max: usize, window: Duration) -> Self {
        Self { sent: VecDeque::with_capacity(max), max: max.max(1), window }
    }

    /// Earliest instant (not before `now`) at which another send is allowed.
    pub fn next_allowed(&mut self, now: Instant) -> Instant {
        while self.sent.front().is_some_and(|t| now.saturating_duration_since(*t) >= self.window) {
            self.sent.pop_front();
        }
        if self.sent.len() < self.max { now } else { self.sent.front().map_or(now, |t| *t + self.window).max(now) }
    }

    pub fn record(&mut self, at: Instant) {
        if self.sent.len() >= self.max {
            self.sent.pop_front();
        }
        self.sent.push_back(at);
    }
}

enum Msg {
    Show(Option<Presence>),
}

/// Timings of the worker loop (shortened in tests).
#[derive(Debug, Clone, Copy)]
struct Timing {
    retry: Duration,
    rejected_retry: Duration,
    /// After a lost connection Discord may be restarting: try again this soon.
    after_loss: Duration,
    debounce: Duration,
    keepalive: Duration,
    rate_max: usize,
    rate_window: Duration,
}

const TIMING: Timing = Timing {
    retry: RETRY_INTERVAL,
    rejected_retry: REJECTED_RETRY_INTERVAL,
    after_loss: Duration::from_secs(2),
    debounce: DEBOUNCE,
    keepalive: KEEPALIVE,
    rate_max: RATE_MAX,
    rate_window: RATE_WINDOW,
};

/// A running Discord thread for one application id. Dropping it stops the thread (without
/// waiting: a stuck pipe read must never block the UI) and clears the status.
pub struct DiscordHandle {
    app_id: String,
    tx: Sender<Msg>,
    state: Arc<Mutex<DiscordState>>,
}

impl DiscordHandle {
    pub fn start(app_id: String, ctx: egui::Context, initial: Option<Presence>) -> Result<Self, String> {
        let link = IpcLink { app_id: app_id.clone(), client: None, nonce: 0 };
        Self::start_with(app_id, link, TIMING, ctx, initial)
    }

    fn start_with(
        app_id: String,
        link: impl Link + Send + 'static,
        timing: Timing,
        ctx: egui::Context,
        initial: Option<Presence>,
    ) -> Result<Self, String> {
        let (tx, rx) = channel();
        let state = Arc::new(Mutex::new(DiscordState::Connecting));
        let worker = Worker { state: state.clone(), ctx, timing };
        std::thread::Builder::new()
            .name("coda-discord".to_owned())
            .stack_size(512 * 1024)
            .spawn(move || worker.run(link, rx, initial))
            .map_err(|e| format!("could not start the Discord thread: {e}"))?;
        Ok(Self { app_id, tx, state })
    }

    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    pub fn show(&self, presence: Option<Presence>) {
        let _ = self.tx.send(Msg::Show(presence));
    }

    pub fn state(&self) -> DiscordState {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

enum ConnectError {
    NotRunning,
    Rejected(String),
    Failed(String),
}

/// The connection to Discord, abstracted so the worker loop can be tested without Discord.
trait Link {
    fn connect(&mut self) -> Result<(), ConnectError>;
    /// Show (or clear) the status. `Ok(false)`: Discord answered with an error.
    /// `Err`: the connection is gone (the link is then disconnected).
    fn send(&mut self, presence: Option<&Presence>) -> Result<bool, String>;
    fn close(&mut self);
}

struct Worker {
    state: Arc<Mutex<DiscordState>>,
    ctx: egui::Context,
    timing: Timing,
}

impl Worker {
    fn set_state(&self, new: DiscordState) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if *state != new {
            log::info!("Discord status: {}", new.describe());
            *state = new;
            drop(state);
            self.ctx.request_repaint();
        }
    }

    fn run(self, mut link: impl Link, rx: Receiver<Msg>, initial: Option<Presence>) {
        let timing = self.timing;
        let mut connected = false;
        let mut desired = initial;
        // What Discord shows for the current connection (`None` = not connected).
        let mut shown: Option<Option<Presence>> = None;
        let mut pending_since: Option<Instant> = None;
        let mut next_connect = Instant::now();
        // The last time Discord answered (the connection was surely alive then).
        let mut last_exchange = Instant::now();
        let mut limiter = RateLimiter::new(timing.rate_max, timing.rate_window);

        loop {
            let now = Instant::now();
            if !connected && now >= next_connect {
                match link.connect() {
                    Ok(()) => {
                        connected = true;
                        shown = Some(None);
                        last_exchange = now;
                        self.set_state(DiscordState::Connected);
                    }
                    Err(ConnectError::NotRunning) => {
                        self.set_state(DiscordState::NotRunning);
                        next_connect = now + timing.retry;
                    }
                    Err(ConnectError::Rejected(message)) => {
                        log::warn!("Discord refused the application id: {message}");
                        self.set_state(DiscordState::Rejected);
                        next_connect = now + timing.rejected_retry;
                    }
                    Err(ConnectError::Failed(message)) => {
                        log::debug!("Discord connection failed: {message}");
                        self.set_state(DiscordState::NotRunning);
                        next_connect = now + timing.retry;
                    }
                }
            }

            let wait = if !connected {
                next_connect.saturating_duration_since(now)
            } else {
                let due = if shown.as_ref() == Some(&desired) {
                    // Back to what Discord already shows (e.g. a quick pause/resume): nothing to
                    // do but check now and then that Discord is still there (and still shows it).
                    pending_since = None;
                    last_exchange + timing.keepalive
                } else {
                    pending_since.map_or(now, |t| t + timing.debounce)
                };
                let earliest = due.max(limiter.next_allowed(now));
                if earliest <= now {
                    match link.send(desired.as_ref()) {
                        Ok(accepted) => {
                            shown = Some(desired.clone());
                            pending_since = None;
                            last_exchange = now;
                            limiter.record(now);
                            self.set_state(if accepted { DiscordState::Connected } else { DiscordState::UpdateRejected });
                        }
                        Err(e) => {
                            log::info!("lost the Discord connection: {e}");
                            connected = false;
                            shown = None;
                            self.set_state(DiscordState::NotRunning);
                            next_connect = now + timing.after_loss;
                        }
                    }
                    continue;
                }
                earliest - now
            };

            match rx.recv_timeout(wait) {
                Ok(Msg::Show(presence)) => {
                    desired = presence;
                    // Keep only the newest state if several queued up.
                    while let Ok(Msg::Show(newer)) = rx.try_recv() {
                        desired = newer;
                    }
                    pending_since.get_or_insert_with(Instant::now);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        // Turned off or app id switched: clear the status and close the pipe.
        if connected {
            if matches!(shown, Some(Some(_))) {
                let _ = link.send(None);
            }
            link.close();
        }
        log::debug!("Discord thread stopped");
    }
}

/// The real link: Discord's local IPC pipe.
struct IpcLink {
    app_id: String,
    client: Option<DiscordIpcClient>,
    nonce: u64,
}

impl Link for IpcLink {
    fn connect(&mut self) -> Result<(), ConnectError> {
        let mut client = DiscordIpcClient::new(&self.app_id);
        // Any failure to open the pipe means Discord isn't running (or not ready yet).
        client.connect_ipc().map_err(|_| ConnectError::NotRunning)?;
        client
            .send(json!({ "v": 1, "client_id": self.app_id }), 0)
            .map_err(|e| ConnectError::Failed(e.to_string()))?;
        let (opcode, reply) = client.recv().map_err(|e| ConnectError::Failed(e.to_string()))?;
        match opcode {
            1 if reply.get("evt").and_then(Value::as_str) == Some("READY") => {
                self.client = Some(client);
                Ok(())
            }
            2 => Err(ConnectError::Rejected(reply_message(&reply))),
            _ => Err(ConnectError::Failed(format!("unexpected handshake reply (op {opcode}): {}", reply_message(&reply)))),
        }
    }

    fn send(&mut self, presence: Option<&Presence>) -> Result<bool, String> {
        let Some(client) = self.client.as_mut() else { return Err("not connected".to_owned()) };
        self.nonce += 1;
        let result = send_presence(client, presence, self.nonce);
        if result.is_err() {
            self.client = None;
        }
        result
    }

    fn close(&mut self) {
        if let Some(mut client) = self.client.take() {
            let _ = client.close();
        }
    }
}

fn reply_message(reply: &Value) -> String {
    reply
        .get("message")
        .or_else(|| reply.pointer("/data/message"))
        .and_then(Value::as_str)
        .unwrap_or("no message")
        .to_owned()
}

/// Send SET_ACTIVITY (or clear it) and wait for Discord's reply, so replies never pile up in
/// the pipe. Returns whether Discord accepted it; an error reply (e.g. a field Discord didn't
/// like) is logged and not retried. `Err` means the connection is gone.
fn send_presence(client: &mut DiscordIpcClient, presence: Option<&Presence>, nonce: u64) -> Result<bool, String> {
    let nonce = format!("coda-{nonce}");
    let activity = presence.map_or(Value::Null, activity_json);
    let payload = json!({
        "cmd": "SET_ACTIVITY",
        "args": { "pid": std::process::id(), "activity": activity },
        "nonce": nonce,
    });
    client.send(payload, 1).map_err(|e| e.to_string())?;
    for _ in 0..MAX_FRAMES_PER_REPLY {
        let (opcode, reply) = client.recv().map_err(|e| e.to_string())?;
        match opcode {
            1 if reply.get("nonce").and_then(Value::as_str) == Some(nonce.as_str()) => {
                if reply.get("evt").and_then(Value::as_str) == Some("ERROR") {
                    log::warn!("Discord rejected the status update: {}", reply_message(&reply));
                    return Ok(false);
                }
                return Ok(true);
            }
            // Ping: answer with pong carrying the same data.
            3 => client.send(reply, 4).map_err(|e| e.to_string())?,
            2 => return Err(format!("Discord closed the connection: {}", reply_message(&reply))),
            _ => {}
        }
    }
    log::debug!("no reply from Discord to the status update");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_id_validation() {
        assert_eq!(validate_app_id(" 1234567890123456789 "), Ok("1234567890123456789".to_owned()));
        assert_eq!(validate_app_id(""), Err(DiscordState::MissingAppId));
        assert_eq!(validate_app_id("   "), Err(DiscordState::MissingAppId));
        assert_eq!(validate_app_id("12345"), Err(DiscordState::InvalidAppId));
        assert_eq!(validate_app_id("12345678901234567a9"), Err(DiscordState::InvalidAppId));
        assert_eq!(validate_app_id("1234567890-123456789"), Err(DiscordState::InvalidAppId));
        assert_eq!(validate_app_id("١٢٣٤٥٦٧٨٩٠١٢٣٤٥٦٧"), Err(DiscordState::InvalidAppId));
    }

    #[test]
    fn text_is_fitted_to_discord_limits() {
        assert_eq!(discord_text("  "), None);
        assert_eq!(utf16_len(&discord_text("X").unwrap()), 2);
        assert_eq!(discord_text(" Song "), Some("Song".to_owned()));
        assert_eq!(utf16_len(&discord_text(&"y".repeat(300)).unwrap()), TEXT_MAX);
    }

    fn rick<'a>() -> Song<'a> {
        Song {
            uri: "spotify:track:4cOdK2wGLETKBW3PvgPWqT",
            title: "Never Gonna Give You Up",
            artist: "Rick Astley",
            album: "Whenever You Need Somebody",
            playlist: "Road trip",
            cover: Some("https://i.scdn.co/image/abc"),
        }
    }

    #[test]
    fn activity_is_listening_with_progress_and_nothing_joinable() {
        // The default settings look exactly like the status always did.
        let presence = presence_for(&rick(), true, (1_000, Some(214_000)), &DiscordLook::default()).unwrap();
        let v = activity_json(&presence);
        assert_eq!(v["type"], 2);
        assert_eq!(v["status_display_type"], 2);
        assert_eq!(v["details"], "Never Gonna Give You Up");
        assert_eq!(v["state"], "Rick Astley");
        assert_eq!(v["assets"]["large_image"], "https://i.scdn.co/image/abc");
        assert_eq!(v["assets"]["large_text"], "Whenever You Need Somebody");
        assert!(v["assets"].get("small_image").is_none());
        assert_eq!(v["timestamps"]["start"], 1_000);
        assert_eq!(v["timestamps"]["end"], 214_000);
        for forbidden in ["party", "secrets", "buttons", "instance"] {
            assert!(v.get(forbidden).is_none(), "{forbidden} must never be sent");
        }
        // Paused: nothing, as before.
        assert_eq!(presence_for(&rick(), false, (1_000, Some(214_000)), &DiscordLook::default()), None);

        // No cover, no artist, unknown length.
        let bare = Song { artist: "", cover: None, ..rick() };
        let v = activity_json(&presence_for(&bare, true, (1_000, None), &DiscordLook::default()).unwrap());
        assert!(v.get("state").is_none() && v.get("assets").is_none());
        assert!(v["timestamps"].get("end").is_none());
    }

    #[test]
    fn the_link_button_is_the_only_button_and_only_when_turned_on() {
        let look = DiscordLook { button: true, ..DiscordLook::default() };
        let v = activity_json(&presence_for(&rick(), true, (1_000, Some(214_000)), &look).unwrap());
        let buttons = v["buttons"].as_array().unwrap();
        assert_eq!(buttons.len(), 1);
        assert_eq!(buttons[0]["label"], BUTTON_LABEL);
        assert_eq!(buttons[0]["url"], "https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT");
        assert!(utf16_len(BUTTON_LABEL) <= 32);
        for forbidden in ["party", "secrets", "instance"] {
            assert!(v.get(forbidden).is_none(), "{forbidden} must never be sent");
        }
        assert!(v.to_string().find("join").is_none() && v.to_string().find("spectate").is_none());
        // A local file has no Spotify page: no button.
        let local = Song { uri: "spotify:local:a:b:c:1", ..rick() };
        assert!(activity_json(&presence_for(&local, true, (1_000, None), &look).unwrap()).get("buttons").is_none());
    }

    #[test]
    fn the_status_follows_the_discord_settings() {
        let look = DiscordLook {
            top: "{title} · {album}".into(),
            bottom: "{artist} on {playlist}".into(),
            shows: DiscordShows::Artist,
            paused: DiscordPaused::Keep,
            progress: false,
            picture: DiscordPicture::Cover,
            badge: true,
            button: false,
        };
        let v = activity_json(&presence_for(&rick(), true, (1_000, Some(214_000)), &look).unwrap());
        assert_eq!(v["details"], "Never Gonna Give You Up · Whenever You Need Somebody");
        assert_eq!(v["state"], "Rick Astley on Road trip");
        assert_eq!(v["status_display_type"], 1);
        assert!(v.get("timestamps").is_none(), "no progress bar");
        assert_eq!(v["assets"]["small_image"], BADGE_PLAYING);

        // Kept while paused: marked "Paused", no time, the pause sign.
        let v = activity_json(&presence_for(&rick(), false, (1_000, Some(214_000)), &look).unwrap());
        assert_eq!(v["state"], "Paused · Rick Astley on Road trip");
        assert!(v.get("timestamps").is_none());
        assert_eq!(v["assets"]["small_image"], BADGE_PAUSED);
        let with_bar = DiscordLook { progress: true, ..look.clone() };
        assert!(activity_json(&presence_for(&rick(), false, (1_000, Some(9_000)), &with_bar).unwrap()).get("timestamps").is_none());

        // coda's picture: no cover, so no badge either; "Listening to coda" in the member list.
        let icon = DiscordLook { picture: DiscordPicture::AppIcon, shows: DiscordShows::App, ..look.clone() };
        let v = activity_json(&presence_for(&rick(), true, (1_000, None), &icon).unwrap());
        assert!(v.get("assets").is_none());
        assert_eq!(v["status_display_type"], 0);
        let song = DiscordLook { shows: DiscordShows::Song, ..look };
        assert_eq!(activity_json(&presence_for(&rick(), true, (1_000, None), &song).unwrap())["status_display_type"], 2);
    }

    #[test]
    fn line_templates_fill_in_and_stay_within_discord_limits() {
        let song = rick();
        assert_eq!(fill_line("{title}", &song), "Never Gonna Give You Up");
        assert_eq!(fill_line("{artist} – {album}", &song), "Rick Astley – Whenever You Need Somebody");
        assert_eq!(fill_line("Now: {title}!", &song), "Now: Never Gonna Give You Up!");
        // An empty part takes its dangling separator with it.
        let loose = Song { playlist: "", album: " ", ..rick() };
        assert_eq!(fill_line("{title} · {playlist}", &loose), "Never Gonna Give You Up");
        assert_eq!(fill_line("{album} - {artist}", &loose), "Rick Astley");
        // An empty top line falls back to the title; an empty bottom line is left out.
        let look = DiscordLook { top: "{playlist}".into(), bottom: "  ".into(), ..DiscordLook::default() };
        let p = presence_for(&loose, true, (0, None), &look).unwrap();
        assert_eq!((p.top.as_str(), p.bottom), ("Never Gonna Give You Up", None));
        // Too long is cut to 128; one letter is padded to 2.
        let long = "x".repeat(400);
        let look = DiscordLook { top: long.clone(), bottom: "y".into(), ..DiscordLook::default() };
        let p = presence_for(&song, true, (0, None), &look).unwrap();
        assert_eq!(utf16_len(&p.top), TEXT_MAX);
        assert_eq!(utf16_len(p.bottom.as_deref().unwrap()), TEXT_MIN);
        let paused = DiscordLook { top: long, bottom: "{album}".repeat(3), paused: DiscordPaused::Keep, ..DiscordLook::default() };
        let p = presence_for(&song, false, (0, None), &paused).unwrap();
        assert!(utf16_len(p.bottom.as_deref().unwrap()) <= TEXT_MAX && p.bottom.unwrap().starts_with("Paused · "));
    }

    #[test]
    fn rate_limiter_allows_five_per_window() {
        let mut limiter = RateLimiter::new(5, Duration::from_secs(20));
        let t0 = Instant::now();
        for i in 0..5 {
            let at = t0 + Duration::from_secs(i);
            assert_eq!(limiter.next_allowed(at), at);
            limiter.record(at);
        }
        let t5 = t0 + Duration::from_secs(5);
        assert_eq!(limiter.next_allowed(t5), t0 + Duration::from_secs(20));
        // Once the first send leaves the window, one more is allowed.
        let t20 = t0 + Duration::from_secs(20);
        assert_eq!(limiter.next_allowed(t20), t20);
        limiter.record(t20);
        assert_eq!(limiter.next_allowed(t20), t0 + Duration::from_secs(21));
    }

    #[derive(Debug, Clone, PartialEq)]
    enum Call {
        Connect,
        Send(Option<String>),
        Close,
    }

    /// Fake Discord: refuses the first `not_running` connects, records every call, and can be
    /// told to drop the connection on the next send.
    struct FakeLink {
        calls: Arc<Mutex<Vec<Call>>>,
        not_running: usize,
        fail_next_send: Arc<Mutex<bool>>,
    }

    impl Link for FakeLink {
        fn connect(&mut self) -> Result<(), ConnectError> {
            self.calls.lock().unwrap().push(Call::Connect);
            if self.not_running > 0 {
                self.not_running -= 1;
                return Err(ConnectError::NotRunning);
            }
            Ok(())
        }
        fn send(&mut self, presence: Option<&Presence>) -> Result<bool, String> {
            if std::mem::take(&mut *self.fail_next_send.lock().unwrap()) {
                return Err("pipe closed".to_owned());
            }
            self.calls.lock().unwrap().push(Call::Send(presence.map(|p| p.top.trim_end_matches('\u{2800}').to_owned())));
            Ok(true)
        }
        fn close(&mut self) {
            self.calls.lock().unwrap().push(Call::Close);
        }
    }

    fn song(title: &str) -> Presence {
        let song = Song { uri: "spotify:track:x", title, artist: "Artist", album: "Album", playlist: "", cover: None };
        presence_for(&song, true, (0, Some(1)), &DiscordLook::default()).unwrap()
    }

    fn wait_for(calls: &Arc<Mutex<Vec<Call>>>, what: impl Fn(&[Call]) -> bool) -> Vec<Call> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let now = calls.lock().unwrap().clone();
            if what(&now) || Instant::now() > deadline {
                return now;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    const FAST: Timing = Timing {
        retry: Duration::from_millis(60),
        rejected_retry: Duration::from_millis(60),
        after_loss: Duration::from_millis(10),
        debounce: Duration::from_millis(40),
        keepalive: Duration::from_secs(30),
        rate_max: 5,
        rate_window: Duration::from_secs(20),
    };

    #[test]
    fn a_closed_pipe_is_noticed_while_nothing_changes() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let fail_next_send = Arc::new(Mutex::new(false));
        let link = FakeLink { calls: calls.clone(), not_running: 0, fail_next_send: fail_next_send.clone() };
        let timing = Timing { keepalive: Duration::from_millis(150), ..FAST };
        let handle =
            DiscordHandle::start_with("1".into(), link, timing, egui::Context::default(), Some(song("long mix"))).unwrap();
        let shown = Call::Send(Some("long mix".into()));
        wait_for(&calls, |c| c.contains(&shown));
        // Discord quits and comes back mid-song: the next check finds the pipe closed, reconnects
        // and shows the song again, with no change from Coda's side.
        *fail_next_send.lock().unwrap() = true;
        let seen = wait_for(&calls, |c| c.iter().filter(|call| **call == Call::Connect).count() >= 2);
        assert_eq!(seen.iter().filter(|call| **call == Call::Connect).count(), 2, "{seen:?}");
        let seen = wait_for(&calls, |c| c.last() == Some(&shown) && c.len() > 3);
        assert_eq!(&seen[seen.len() - 2..], &[Call::Connect, shown.clone()]);
        assert_eq!(handle.state(), DiscordState::Connected);
        drop(handle);
    }

    #[test]
    fn worker_retries_coalesces_reconnects_and_clears_on_stop() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let fail_next_send = Arc::new(Mutex::new(false));
        let link = FakeLink { calls: calls.clone(), not_running: 1, fail_next_send: fail_next_send.clone() };
        let handle =
            DiscordHandle::start_with("1".into(), link, FAST, egui::Context::default(), Some(song("first"))).unwrap();

        // Discord not running at first: retried after the interval, then the song is shown.
        let seen = wait_for(&calls, |c| c.contains(&Call::Send(Some("first".into()))));
        assert_eq!(&seen[..3], &[Call::Connect, Call::Connect, Call::Send(Some("first".into()))]);
        assert_eq!(handle.state(), DiscordState::Connected);

        // Quick skips collapse into one update with the latest song.
        for title in ["a", "b", "c"] {
            handle.show(Some(song(title)));
        }
        let seen = wait_for(&calls, |c| c.contains(&Call::Send(Some("c".into()))));
        assert!(!seen.contains(&Call::Send(Some("a".into()))) && !seen.contains(&Call::Send(Some("b".into()))));

        // Pausing and resuming within the debounce sends nothing.
        let before = calls.lock().unwrap().len();
        handle.show(None);
        handle.show(Some(song("c")));
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(calls.lock().unwrap().len(), before);

        // Lost connection (Discord restarted): reconnects and shows the latest state again.
        *fail_next_send.lock().unwrap() = true;
        handle.show(Some(song("d")));
        let seen = wait_for(&calls, |c| c.contains(&Call::Send(Some("d".into()))));
        let tail = &seen[seen.len() - 2..];
        assert_eq!(tail, &[Call::Connect, Call::Send(Some("d".into()))]);

        // Turning it off clears the status and closes the pipe.
        drop(handle);
        let seen = wait_for(&calls, |c| c.last() == Some(&Call::Close));
        assert_eq!(&seen[seen.len() - 2..], &[Call::Send(None), Call::Close]);
    }

    #[test]
    fn worker_respects_the_rate_limit() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let link = FakeLink { calls: calls.clone(), not_running: 0, fail_next_send: Arc::new(Mutex::new(false)) };
        let timing = Timing { debounce: Duration::ZERO, rate_max: 2, rate_window: Duration::from_millis(400), ..FAST };
        let handle = DiscordHandle::start_with("1".into(), link, timing, egui::Context::default(), None).unwrap();
        let started = Instant::now();
        for title in ["1", "2", "3"] {
            handle.show(Some(song(title)));
            wait_for(&calls, |c| c.contains(&Call::Send(Some(title.into()))));
        }
        // The third update had to wait for the first to leave the 400 ms window.
        assert!(started.elapsed() >= Duration::from_millis(350), "{:?}", started.elapsed());
        drop(handle);
    }
}
