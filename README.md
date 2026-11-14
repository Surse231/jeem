# Jeem

A free music player for Windows. Search and play music from YouTube Music, YouTube and SoundCloud in one app, with no ads and no subscriptions.

## Features

- One search across YouTube Music, YouTube and SoundCloud
- My Vibe: an endless stream tuned to your taste, mood and activity
- Synced lyrics (LRCLIB, NetEase) with a plain-text fallback from Genius
- Podcasts with offline downloads and resume
- Offline playback of liked tracks
- 10-band equalizer with presets
- Themes, accent colours and custom CSS
- Optional Google Drive sync and Last.fm scrobbling
- Automatic updates

## Download

Get the latest installer from the [releases page](https://github.com/Surse231/jeem/releases/latest).

## Build from source

Requirements:

- Windows 10 or 11 with [WebView2](https://developer.microsoft.com/microsoft-edge/webview2/)
- [Node.js](https://nodejs.org/) 18+
- [Rust](https://www.rust-lang.org/tools/install) (stable, MSVC toolchain)
- [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/)

```bash
npm install
npm run dev
```

`npm run build` creates the installer in `src-tauri/target/release/bundle/nsis`.

On first start Jeem downloads `yt-dlp` and `ffmpeg` into its data folder.

Google sign-in needs your own OAuth client: set the `JEEM_GOOGLE_CLIENT_ID` and `JEEM_GOOGLE_CLIENT_SECRET` environment variables before building. Without them the app works, only Google sync is turned off.

For development, `JEEM_DATA_DIR` points the app to a separate data folder, so a dev build does not touch the data of an installed Jeem.

## Project structure

- `src-tauri/` – Rust backend: local HTTP server, streams, search, lyrics, recommendations, sync
- `web/` – the interface (HTML, CSS, JavaScript)
- `scripts/release.ps1` – builds a signed release and publishes it

## License

[MIT](LICENSE). Third-party notices are in [THIRD-PARTY-NOTICES.txt](THIRD-PARTY-NOTICES.txt).
