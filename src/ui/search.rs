//! Search (the Search tab of the middle column), album / artist cards, the "Add to playlist" menu
//! and playing a song from search by opening its album.

use std::sync::Arc;


use super::{MiddleTab, CodaApp};
use crate::model::{Card, Command, SearchResults, Source, TrackInfo};

pub(super) const SEARCH_BOX_ID: &str = "spotify_search";
/// What was searched for and what came back.
#[derive(Default)]
pub(super) struct SearchState {
    /// The text in the search box.
    pub input: String,
    /// The query the results (or the pending request) belong to.
    pub sent: String,
    pub results: Option<Arc<SearchResults>>,
    /// Songs of every page loaded so far.
    pub tracks: Vec<TrackInfo>,
    pub loading: bool,
    pub error: Option<String>,
    /// The chip chosen (0 All, 1 Songs, 2 Albums, 3 Artists, 4 Playlists, 5 Users).
    pub chip: u8,
    /// People found for the Users chip.
    pub users: Users,
    /// When the box last changed: the search goes out once typing pauses.
    pub typed_at: Option<std::time::Instant>,
    /// Enter was pressed before the results came: play the top one when they do.
    pub play_top: bool,
    /// The page the search started from (Esc in an empty box goes back).
    pub back: Option<MiddleTab>,
}

impl SearchState {
    /// Takes in a page of results for the current query.
    pub fn receive(&mut self, results: Arc<SearchResults>) {
        if results.query != self.sent {
            return;
        }
        if results.offset == 0 {
            self.tracks = results.tracks.clone();
            self.results = Some(results);
        } else {
            self.tracks.extend(results.tracks.iter().cloned());
            if let Some(first) = self.results.as_mut() {
                Arc::make_mut(first).more = results.more;
            }
        }
        self.loading = false;
        self.error = None;
    }

    /// Takes in a page of people for the current query: each person once.
    pub fn receive_users(&mut self, results: &SearchResults) {
        if results.query != self.sent || results.query != self.users.query {
            return;
        }
        if results.offset == 0 {
            self.users.list.clear();
        }
        for u in &results.users {
            if !self.users.list.iter().any(|k| k.uri == u.uri) {
                self.users.list.push(u.clone());
            }
        }
        self.users.next = results.users_next;
        self.users.loading = false;
        self.users.error = None;
    }
}

/// People found (Spotify users), asked for once the Users chip shows.
#[derive(Default)]
pub(super) struct Users {
    /// The query they (or the page on its way) are for; empty: not asked.
    pub query: String,
    /// Every page loaded so far.
    pub list: Vec<Card>,
    /// Where the next page starts, when there is one.
    pub next: Option<u32>,
    pub loading: bool,
    pub error: Option<String>,
}

impl CodaApp {

    pub(super) fn start_search(&mut self) {
        let query = self.search.input.trim().to_owned();
        if query.is_empty() {
            return;
        }
        self.middle_tab = MiddleTab::Search;
        if query == self.search.sent && self.search.results.is_some() {
            return;
        }
        let keep = SearchState { chip: self.search.chip, play_top: self.search.play_top, back: self.search.back, ..Default::default() };
        self.search = SearchState { input: self.search.input.clone(), sent: query.clone(), loading: true, ..keep };
        self.send(Command::Search { query, offset: 0 });
    }

    /// Plays a song that isn't in an open list by opening its album and starting there.
    pub(super) fn play_from_album(&mut self, t: &TrackInfo) {
        if !t.playable {
            self.toast("That song can't be played here.", true);
            return;
        }
        let source = Source::Album(t.album_uri.clone());
        if !t.album_uri.starts_with("spotify:album:") {
            self.toast("Couldn't find the album of that song.", true);
            return;
        }
        self.middle_tab = MiddleTab::Songs;
        self.pending_play = Some((source.clone(), t.uri.clone()));
        self.pending_scroll_uri = Some(t.uri.clone());
        let already = self.view.as_ref().is_some_and(|v| v.source == source && v.tracks.is_some());
        self.open_source(source, false);
        if already {
            self.start_pending_play();
        }
    }

    /// Opens what a pasted Spotify link or uri points at: a playlist's, album's or artist's page,
    /// a person's profile, or a song's album with the song found in it. False if `text` isn't one.
    pub(super) fn open_pasted_link(&mut self, text: &str) -> bool {
        let Some(uri) = pasted_link(text) else { return false };
        if let Some(user) = uri.strip_prefix("spotify:user:") {
            self.show_user(user);
        } else if uri.starts_with("spotify:track:") {
            self.send(Command::FindTrack(uri));
        } else {
            self.open_source(source_of(&uri), false);
        }
        true
    }

    /// A song from a pasted link, once its details came: its album, scrolled to it.
    pub(super) fn show_found_track(&mut self, t: &TrackInfo) {
        if t.album_uri.starts_with("spotify:album:") {
            self.show_track_in(Source::Album(t.album_uri.clone()), t.uri.clone());
        } else {
            self.toast("Couldn't find the album of that song.", true);
        }
    }

    /// Starts the song waiting in `pending_play` once its list is on screen.
    pub(super) fn start_pending_play(&mut self) {
        let Some((source, uri)) = self.pending_play.clone() else { return };
        let Some(view) = self.view.as_ref().filter(|v| v.source == source) else { return };
        let Some(index) = view.tracks.as_ref().and_then(|t| t.iter().position(|t| t.uri == uri)) else { return };
        self.pending_play = None;
        let shuffle = self.current_shuffle();
        self.send(Command::PlaySource { source, start_index: Some(index), shuffle });
    }
}

/// The Spotify uri in a pasted link (`https://open.spotify.com/intl-de/album/<id>?si=…`, the old
/// `…/user/<name>/playlist/<id>`) or uri (`spotify:track:<id>`, `spotify:user:<name>:playlist:<id>`):
/// a playlist, album, artist, song or person. `None` for anything else, which is searched for.
pub(super) fn pasted_link(text: &str) -> Option<String> {
    let text = text.trim();
    if text.contains(char::is_whitespace) {
        return None;
    }
    let parts: Vec<&str> = if let Some(rest) = text.strip_prefix("spotify:") {
        rest.split(':').collect()
    } else {
        let rest = text.strip_prefix("https://").or_else(|| text.strip_prefix("http://")).unwrap_or(text);
        let rest = rest.strip_prefix("open.spotify.com/")?;
        let path = rest.split(['?', '#']).next().unwrap_or_default();
        path.split('/').filter(|p| !p.is_empty() && !p.starts_with("intl-")).collect()
    };
    let id_ok = |id: &str| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric());
    match parts.as_slice() {
        ["user", _, "playlist", id] if id_ok(id) => Some(format!("spotify:playlist:{id}")),
        [kind @ ("playlist" | "album" | "artist" | "track"), id] if id_ok(id) => Some(format!("spotify:{kind}:{id}")),
        ["user", name] if !name.is_empty() => Some(format!("spotify:user:{name}")),
        _ => None,
    }
}

/// The page a Spotify URI opens.
pub(super) fn source_of(uri: &str) -> Source {
    if uri.starts_with("spotify:artist:") {
        Source::Artist(uri.to_owned())
    } else if uri.starts_with("spotify:album:") {
        Source::Album(uri.to_owned())
    } else {
        Source::Playlist(uri.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page of people for `query` from `offset`.
    fn page(query: &str, offset: u32, ids: &[&str], next: Option<u32>) -> SearchResults {
        let users = ids.iter().map(|&id| Card { uri: format!("spotify:user:{id}"), name: id.into(), subtitle: id.into(), image: None }).collect();
        SearchResults { query: query.into(), offset, users, users_next: next, ..Default::default() }
    }

    #[test]
    fn pasted_links_and_uris_open_their_page_and_anything_else_is_searched() {
        let id = "37i9dQZF1DXcBWIGoYBM5M";
        let cases = [
            (format!("https://open.spotify.com/playlist/{id}?si=abc123"), Some(format!("spotify:playlist:{id}"))),
            (format!("  https://open.spotify.com/intl-de/album/{id}?si=x  "), Some(format!("spotify:album:{id}"))),
            (format!("open.spotify.com/artist/{id}"), Some(format!("spotify:artist:{id}"))),
            (format!("https://open.spotify.com/track/{id}#x"), Some(format!("spotify:track:{id}"))),
            (format!("https://open.spotify.com/user/bob/playlist/{id}"), Some(format!("spotify:playlist:{id}"))),
            ("https://open.spotify.com/user/mira.l?si=1".to_owned(), Some("spotify:user:mira.l".to_owned())),
            (format!("spotify:track:{id}"), Some(format!("spotify:track:{id}"))),
            (format!("spotify:user:bob:playlist:{id}"), Some(format!("spotify:playlist:{id}"))),
            ("spotify".to_owned(), None),
            ("open spotify playlist".to_owned(), None),
            ("https://example.com/album/abc".to_owned(), None),
            ("https://open.spotify.com/episode/abc".to_owned(), None),
            ("https://open.spotify.com/album/".to_owned(), None),
            ("spotify:album:ab-c".to_owned(), None),
        ];
        for (text, want) in cases {
            assert_eq!(pasted_link(&text), want, "{text}");
        }
    }

    fn ids(s: &SearchState) -> Vec<&str> {
        s.users.list.iter().map(|c| c.subtitle.as_str()).collect()
    }

    #[test]
    fn pages_of_people_add_up_each_one_once() {
        let users = Users { query: "emma".into(), loading: true, error: Some("Couldn't search Spotify.".into()), ..Default::default() };
        let mut s = SearchState { sent: "emma".into(), users, ..Default::default() };
        // A page for what was typed before: left out.
        s.receive_users(&page("emm", 0, &["x"], None));
        assert!(s.users.list.is_empty() && s.users.loading);
        // Nobody found.
        s.receive_users(&page("emma", 0, &[], None));
        assert!(s.users.list.is_empty() && !s.users.loading && s.users.next.is_none() && s.users.error.is_none());
        // One, then a page that repeats them.
        s.receive_users(&page("emma", 0, &["a"], Some(10)));
        s.receive_users(&page("emma", 10, &["a", "b", "b", "c"], Some(20)));
        assert_eq!((ids(&s), s.users.next), (vec!["a", "b", "c"], Some(20)));
        s.receive_users(&page("emma", 20, &[], None));
        assert_eq!((ids(&s), s.users.next), (vec!["a", "b", "c"], None));
        // The first page again starts over; a search for something else is never taken in.
        s.receive_users(&page("emma", 0, &["z"], Some(10)));
        assert_eq!(ids(&s), ["z"]);
        s.sent = "bob".into();
        s.receive_users(&page("emma", 10, &["y"], None));
        assert_eq!(ids(&s), ["z"]);
    }
}
