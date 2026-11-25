# Coda

Coda is a lightweight Windows music player for your Spotify library. It is one native window,
written in Rust, with no embedded browser.

[Download coda.exe](https://github.com/npr-tami/coda/releases/latest) · [What's new](CHANGELOG.md)

0.1.5 is the last public release for now, and this repository is archived. Coda still checks for updates, so a later release would reach you.

## Features

- Your playlists, Liked Songs, albums, artists and search, with likes and fast filtering.
- Shuffle where every order is equally likely, with optional rules: play every song before
  repeating, avoid the same artist twice in a row, favour songs you rarely play, skip songs
  you blocked.
- Sorting, drag and drop for Up next and your own playlists, and finding duplicates.
- Media keys, the Windows media overlay, a tray icon, a mini player and start with Windows.
- Equalizer, crossfade and a sleep timer.
- Listen along with friends through a link.
- Optional Discord status and Last.fm scrobbling, each with your own free API account.
- Dark and light themes, accent colours and custom colours.

## Install

1. Download `coda.exe` from [Releases](https://github.com/npr-tami/coda/releases/latest).
2. Put it in any folder and run it. There is no installer.
3. Click **Log in with Spotify** and log in on Spotify's own page in your browser.

Needs Windows 10 or 11 (64-bit). With Spotify Premium, songs play from Spotify. With a Free
account they play from YouTube, which needs [yt-dlp](https://github.com/yt-dlp/yt-dlp/releases)
next to `coda.exe` and [Node.js](https://nodejs.org) or [Deno](https://deno.com) installed.

Renaming playlists, the Discord status and Last.fm each need a short setup. Coda shows the
steps in its Settings.

## Your data

Settings and logins are in `%APPDATA%\coda`, caches in `%LOCALAPPDATA%\coda`. Nothing is stored
next to the exe. To remove Coda, turn off **Start with Windows** in Settings, then delete
`coda.exe` and both folders.

## Build

Needs Rust (stable, MSVC) and Visual Studio Build Tools with "Desktop development with C++".

```powershell
cargo build --release
.\target\release\coda.exe
```

Run the tests with `cargo test`. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for how the
code is laid out.

## About librespot

Coda plays music through [librespot](https://github.com/librespot-org/librespot), an
unofficial, open-source Spotify client library. Spotify does not make, support or endorse it,
or Coda. Spotify can change things at any time and break playback, and using unofficial clients
may go against Spotify's Terms of Use. Coda does not download music for offline use.

## License

MIT. See [LICENSE](LICENSE).
