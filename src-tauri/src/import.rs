// Playlist imports: YouTube / SoundCloud (yt-dlp), Deezer, Spotify (public embed page), Apple Music, Yandex Music.
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::{Ctx, tools};

type Reply = (u16, Value);

fn s(v: Option<&Value>) -> &str {
    v.and_then(Value::as_str).unwrap_or("")
}

fn err(status: u16, code: &str, msg: &str) -> Reply {
    (status, json!({ "success": false, "code": code, "error": msg, "status": status }))
}

async fn get_json(ctx: &Ctx, url: &str) -> Option<Value> {
    let r = ctx.client.get(url).timeout(Duration::from_secs(12)).send().await.ok()?;
    r.json::<Value>().await.ok()
}

fn between<'a>(body: &'a str, start_marker: &str, end_marker: &str) -> Option<&'a str> {
    let i = body.find(start_marker)? + start_marker.len();
    let rest = &body[i..];
    let j = rest.find(end_marker)?;
    Some(&rest[..j])
}

// <script ... id="ID" ...>JSON</script>
fn script_json(body: &str, id: &str) -> Option<Value> {
    let pos = body.find(&format!("id=\"{id}\""))?;
    let after = &body[pos..];
    let start = after.find('>')? + 1;
    let end = after[start..].find("</script>")?;
    serde_json::from_str(&after[start..start + end]).ok()
}

pub async fn playlist_import(ctx: &Arc<Ctx>, input: &str) -> Reply {
    let mut input = input.trim().to_string();
    if ["on.soundcloud.com", "soundcloud.app.goo.gl", "link.deezer.com"].iter().any(|h| input.contains(h)) {
        if let Ok(r) = ctx.client.get(&input).send().await {
            input = r.url().to_string();
        }
    }
    if input.starts_with("spotify:") {
        return spotify_import(ctx, &input).await;
    }
    let Ok(parsed) = reqwest::Url::parse(&input) else { return err(400, "unsupported", "Unsupported link") };
    let host = parsed.host_str().unwrap_or("").to_lowercase();
    let is = |d: &str| host == d || host.ends_with(&format!(".{d}"));

    if is("spotify.com") {
        return spotify_import(ctx, &input).await;
    }
    if is("deezer.com") {
        return deezer_import(ctx, parsed.path()).await;
    }
    let soundcloud = is("soundcloud.com");
    let youtube = is("youtube.com") || host == "youtu.be";
    if !soundcloud && !youtube {
        return err(400, "unsupported", "Unsupported link");
    }
    let platform = if soundcloud { "soundcloud" } else if host == "music.youtube.com" { "ytmusic" } else { "youtube" };

    let ctx2 = ctx.clone();
    let target = input.clone();
    let data = tauri::async_runtime::spawn_blocking(move || -> Result<Value, String> {
        let mut cmd = tools::command(tools::tool_path(&ctx2, "yt-dlp"));
        cmd.args(["--flat-playlist", "-J", "--no-warnings"]).arg(&target);
        let out = tools::run_cmd(cmd)?;
        serde_json::from_str::<Value>(&out).map_err(|e| e.to_string())
    })
    .await;
    let data = match data {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => return err(502, "import_failed", &e),
        Err(e) => return err(500, "import_failed", &e.to_string()),
    };
    let entries: Vec<Value> = match data.get("entries").and_then(Value::as_array) {
        Some(a) => a.clone(),
        None => vec![data.clone()],
    };
    let mut tracks = Vec::new();
    for item in &entries {
        let id = s(item.get("id"));
        let title = s(item.get("title"));
        if id.is_empty() || title == "[Private video]" || title == "[Deleted video]" {
            continue;
        }
        let url = if youtube {
            format!("https://www.youtube.com/watch?v={id}")
        } else {
            let u = s(item.get("webpage_url"));
            if u.is_empty() { s(item.get("url")).to_string() } else { u.to_string() }
        };
        if url.is_empty() {
            continue;
        }
        let artist = ["artist", "creator", "channel", "uploader"]
            .iter()
            .map(|k| s(item.get(*k)))
            .find(|v| !v.is_empty())
            .unwrap_or("")
            .trim_end_matches(" - Topic")
            .to_string();
        let thumb = if youtube {
            format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")
        } else {
            item.get("thumbnails")
                .and_then(Value::as_array)
                .and_then(|a| a.last())
                .map(|t| s(t.get("url")).to_string())
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| s(item.get("thumbnail")).to_string())
        };
        tracks.push(json!({
            "id": id, "title": title, "artist": artist,
            "duration": item.get("duration").and_then(Value::as_f64).unwrap_or(0.0).round(),
            "thumbnail": thumb, "url": url, "platform": platform, "isCached": false
        }));
    }
    if tracks.is_empty() {
        return (200, json!({ "success": false, "error": "No tracks found" }));
    }
    let mut cover = s(data.get("thumbnails").and_then(|t| t.as_array()).and_then(|a| a.last()).and_then(|t| t.get("url"))).to_string();
    if cover.is_empty() {
        cover = s(data.get("thumbnail")).to_string();
    }
    if cover.is_empty() && tracks.len() == 1 {
        cover = s(tracks[0].get("thumbnail")).to_string();
    }
    let name = if s(data.get("title")).is_empty() { "Imported playlist" } else { s(data.get("title")) };
    (200, json!({ "success": true, "name": name, "cover": cover, "tracks": tracks }))
}

async fn deezer_import(ctx: &Arc<Ctx>, path: &str) -> Reply {
    let id: String = path
        .split("/playlist/")
        .nth(1)
        .map(|r| r.chars().take_while(|c| c.is_ascii_digit()).collect())
        .unwrap_or_default();
    if id.is_empty() {
        return err(400, "unsupported", "Unsupported Deezer link");
    }
    let Some(pl) = get_json(ctx, &format!("https://api.deezer.com/playlist/{id}")).await else {
        return err(502, "import_failed", "Deezer playlist was not found");
    };
    if pl.get("error").is_some() || pl.get("id").is_none() {
        return err(502, "import_failed", "Deezer playlist was not found");
    }
    let mut items: Vec<Value> = pl.pointer("/tracks/data").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut next = s(pl.pointer("/tracks/next")).to_string();
    let mut pages = 0;
    while !next.is_empty() && pages < 100 {
        let Some(page) = get_json(ctx, &next).await else { break };
        if let Some(a) = page.get("data").and_then(Value::as_array) {
            items.extend(a.iter().cloned());
        }
        next = s(page.get("next")).to_string();
        pages += 1;
    }
    let tracks: Vec<Value> = items
        .iter()
        .filter(|t| t.get("id").is_some() && !s(t.get("title")).is_empty() && !s(t.pointer("/artist/name")).is_empty())
        .map(|t| {
            let thumb = ["cover_xl", "cover_big", "cover"]
                .iter()
                .map(|k| s(t.pointer(&format!("/album/{k}"))))
                .find(|v| !v.is_empty())
                .unwrap_or("");
            json!({
                "id": t.get("id").map(|v| v.to_string().trim_matches('"').to_string()).unwrap_or_default(),
                "title": s(t.get("title")), "artist": s(t.pointer("/artist/name")),
                "duration": t.get("duration").and_then(Value::as_u64).unwrap_or(0),
                "thumbnail": thumb, "url": s(t.get("link")), "platform": "deezer",
                "isExplicit": t.get("explicit_lyrics").and_then(Value::as_bool).unwrap_or(false)
            })
        })
        .collect();
    if tracks.is_empty() {
        return err(502, "import_failed", "No tracks found in this Deezer playlist");
    }
    let cover = ["picture_xl", "picture_big", "picture"].iter().map(|k| s(pl.get(*k))).find(|v| !v.is_empty()).unwrap_or("");
    (200, json!({ "success": true, "name": s(pl.get("title")), "cover": cover, "tracks": tracks }))
}

/* ---------- Spotify: the public embed page only (up to 100 tracks) ---------- */

pub async fn spotify_import(ctx: &Arc<Ctx>, input: &str) -> Reply {
    let id: String = input
        .split("playlist/")
        .nth(1)
        .or_else(|| input.split("spotify:playlist:").nth(1))
        .map(|r| r.chars().take_while(|c| c.is_ascii_alphanumeric()).collect())
        .unwrap_or_default();
    if id.len() != 22 {
        return err(400, "spotify_error", "Invalid Spotify playlist URL");
    }
    if let Some(full) = crate::spotify::playlist(ctx, &id).await {
        return (200, full);
    }
    let resp = ctx
        .client
        .get(format!("https://open.spotify.com/embed/playlist/{id}"))
        .header("Accept-Language", "en-US,en;q=0.9")
        .timeout(Duration::from_secs(15))
        .send()
        .await;
    let Ok(resp) = resp else { return err(500, "spotify_error", "Network error connecting to Spotify") };
    let status = resp.status().as_u16();
    match status {
        404 => return err(404, "spotify_not_found", "Playlist not found"),
        403 => return err(403, "spotify_forbidden", "Spotify refused the request (403)"),
        429 => return err(429, "spotify_rate_limited", "Spotify rate limited (429)"),
        200 => {}
        _ => return err(status, "spotify_error", &format!("Spotify returned status {status}")),
    }
    let body = resp.text().await.unwrap_or_default();
    let Some(data) = script_json(&body, "__NEXT_DATA__") else {
        let private = body.to_lowercase().contains("private");
        return err(403, if private { "spotify_forbidden" } else { "spotify_error" }, if private { "Private playlist" } else { "Could not parse playlist" });
    };
    let entity = data.pointer("/props/pageProps/state/data/entity").cloned().unwrap_or(Value::Null);
    let name = {
        let n = s(entity.get("name"));
        if n.is_empty() { "Spotify Playlist" } else { n }.to_string()
    };
    let cover = entity
        .pointer("/coverArt/sources/0/url")
        .or_else(|| entity.pointer("/images/0/url"))
        .and_then(Value::as_str)
        .map(String::from)
        .or_else(|| between(&body, "property=\"og:image\" content=\"", "\"").map(String::from))
        .unwrap_or_default();
    let tracks: Vec<Value> = entity
        .get("trackList")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .take(100)
                .filter_map(|t| {
                    let title = s(t.get("title")).trim().to_string();
                    if title.is_empty() {
                        return None;
                    }
                    let ms = t.get("duration").and_then(Value::as_f64).unwrap_or(0.0);
                    let art = s(t.get("image"));
                    Some(json!({
                        "title": title, "artist": s(t.get("subtitle")).trim(),
                        "duration": (ms / 1000.0).round(), "thumbnail": art, "cover": art,
                        "isUnavailable": t.pointer("/isPlayable").and_then(Value::as_bool) == Some(false)
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    if tracks.is_empty() {
        let private = body.to_lowercase().contains("private");
        return err(403, if private { "spotify_forbidden" } else { "spotify_error" }, if private { "Private playlist" } else { "Could not read tracks" });
    }
    let total = entity.get("totalCount").and_then(Value::as_u64).unwrap_or(tracks.len() as u64);
    let limited = total as usize > tracks.len();
    (200, json!({ "success": true, "name": name, "cover": cover, "tracks": tracks, "hasMore": false, "total": total, "limited": limited }))
}

/* ---------- Apple Music ---------- */

fn apple_art(node: &Value) -> String {
    let u = s(node.pointer("/artwork/dictionary/url").or_else(|| node.pointer("/artwork/url")));
    u.replace("{w}x{h}bb.{f}", "600x600bb.jpg").replace("{w}x{h}", "600x600")
}

struct AppleWalk {
    tracks: Vec<Value>,
    seen: std::collections::HashSet<String>,
    title: String,
    cover: String,
    total: u64,
}

fn apple_walk(node: &Value, w: &mut AppleWalk) {
    match node {
        Value::Array(a) => a.iter().for_each(|n| apple_walk(n, w)),
        Value::Object(o) => {
            if w.title.is_empty() {
                let kind = s(node.pointer("/contentDescriptor/kind"));
                let ty = s(o.get("type"));
                if kind == "playlist" || ty == "playlists" || ty == "playlist" {
                    let t = s(o.get("title")).to_string();
                    w.title = if t.is_empty() { s(node.pointer("/attributes/name")).to_string() } else { t };
                    w.cover = apple_art(node);
                } else if s(o.get("@type")) == "MusicPlaylist" {
                    w.title = s(o.get("name")).to_string();
                }
            }
            if w.total == 0 {
                w.total = o.get("trackCount").and_then(Value::as_u64).unwrap_or(0);
            }
            let kind = s(node.pointer("/contentDescriptor/kind"));
            let ty = s(o.get("type"));
            let ld = s(o.get("@type")) == "MusicRecording";
            let is_song = kind == "song" || kind == "music-video" || ty == "songs" || ty == "song" || ld;
            let title = if !s(o.get("title")).is_empty() {
                s(o.get("title"))
            } else if ld || ty == "songs" {
                s(o.get("name"))
            } else {
                ""
            };
            let artist = [s(o.get("artistName")), s(o.get("artist")), s(node.pointer("/byArtist/name"))]
                .into_iter()
                .find(|v| !v.is_empty())
                .unwrap_or("");
            if is_song && !title.is_empty() && !artist.is_empty() {
                let key = format!("{}___{}", title.to_lowercase(), artist.to_lowercase());
                if w.seen.insert(key) {
                    let dur = match o.get("durationInMillis").or_else(|| o.get("duration")) {
                        Some(Value::Number(n)) => {
                            let v = n.as_f64().unwrap_or(0.0);
                            if v > 1000.0 { (v / 1000.0).round() } else { v.round() }
                        }
                        Some(Value::String(t)) if t.starts_with("PT") => {
                            let m = between(t, "PT", "M").and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
                            let sec = t.rsplit('M').next().and_then(|x| x.trim_end_matches('S').parse::<f64>().ok()).unwrap_or(0.0);
                            m * 60.0 + sec
                        }
                        _ => 0.0,
                    };
                    let art = apple_art(node);
                    w.tracks.push(json!({ "title": title, "artist": artist, "duration": dur, "thumbnail": art, "cover": art }));
                }
                return;
            }
            o.values().for_each(|n| apple_walk(n, w));
        }
        _ => {}
    }
}

pub async fn apple_import(ctx: &Arc<Ctx>, input: &str) -> Reply {
    if let Some(album_id) = input
        .split("/album/")
        .nth(1)
        .and_then(|r| r.split('/').find(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())))
        .map(String::from)
    {
        if let Some(data) = get_json(ctx, &format!("https://itunes.apple.com/lookup?id={album_id}&entity=song")).await {
            let results = data.get("results").and_then(Value::as_array).cloned().unwrap_or_default();
            if let Some(album) = results.iter().find(|r| s(r.get("wrapperType")) == "collection") {
                let tracks: Vec<Value> = results
                    .iter()
                    .filter(|r| s(r.get("wrapperType")) == "track")
                    .map(|t| {
                        let art = {
                            let a = s(t.get("artworkUrl100"));
                            let a = if a.is_empty() { s(album.get("artworkUrl100")) } else { a };
                            a.replace("100x100", "600x600")
                        };
                        json!({
                            "title": s(t.get("trackName")),
                            "artist": if s(t.get("artistName")).is_empty() { s(album.get("artistName")) } else { s(t.get("artistName")) },
                            "duration": (t.get("trackTimeMillis").and_then(Value::as_f64).unwrap_or(0.0) / 1000.0).round(),
                            "thumbnail": art, "cover": art
                        })
                    })
                    .collect();
                if !tracks.is_empty() {
                    let cover = s(album.get("artworkUrl100")).replace("100x100", "600x600");
                    let name = if s(album.get("collectionName")).is_empty() { "Album" } else { s(album.get("collectionName")) };
                    return (200, json!({ "success": true, "name": name, "cover": cover, "tracks": tracks }));
                }
            }
        }
    }
    if !input.contains("music.apple.com") || !input.contains("pl.") {
        return err(400, "invalid", "Invalid Apple Music playlist URL");
    }
    let resp = ctx.client.get(input).header("Accept-Language", "en-US,en;q=0.9").timeout(Duration::from_secs(15)).send().await;
    let Ok(resp) = resp else { return err(504, "network", "network") };
    match resp.status().as_u16() {
        404 => return err(404, "not_found", "not_found"),
        429 => return err(429, "rate_limit", "rate_limit"),
        401 | 403 => return err(403, "private", "private"),
        _ => {}
    }
    let body = resp.text().await.unwrap_or_default();
    let parsed = script_json(&body, "serialized-server-data").or_else(|| script_json(&body, "schema:music-playlist"));
    let Some(parsed) = parsed else { return err(403, "private", "private") };
    let mut w = AppleWalk { tracks: Vec::new(), seen: Default::default(), title: String::new(), cover: String::new(), total: 0 };
    apple_walk(&parsed, &mut w);
    if w.title.is_empty() {
        w.title = between(&body, "property=\"og:title\" content=\"", "\"")
            .map(|t| t.trim_end_matches(" on Apple Music").trim().to_string())
            .unwrap_or_default();
    }
    if w.tracks.is_empty() {
        return err(403, "private", "private");
    }
    let total = if w.total == 0 { w.tracks.len() as u64 } else { w.total };
    let name = if w.title.is_empty() { "Apple Music Playlist".to_string() } else { w.title };
    (200, json!({ "success": true, "name": name, "cover": w.cover, "hasMore": total as usize > w.tracks.len(), "total": total, "tracks": w.tracks }))
}

/* ---------- Yandex Music ---------- */

const YANDEX_API: &str = "https://api.music.yandex.net";

async fn yandex_get(ctx: &Ctx, path: &str) -> Result<Value, u16> {
    let r = ctx
        .client
        .get(format!("{YANDEX_API}{path}"))
        .header("User-Agent", "Yandex-Music-API")
        .header("X-Yandex-Music-Client", "YandexMusicAndroid/24023231")
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|_| 504u16)?;
    if !r.status().is_success() {
        return Err(r.status().as_u16());
    }
    r.json::<Value>().await.map_err(|_| 502u16)
}

async fn yandex_tracks_by_ids(ctx: &Ctx, ids: &[String]) -> Vec<Value> {
    let mut out = Vec::new();
    for chunk in ids.chunks(100) {
        let r = ctx
            .client
            .post(format!("{YANDEX_API}/tracks"))
            .header("User-Agent", "Yandex-Music-API")
            .header("X-Yandex-Music-Client", "YandexMusicAndroid/24023231")
            .form(&[("track-ids", chunk.join(","))])
            .timeout(Duration::from_secs(15))
            .send()
            .await;
        if let Ok(r) = r {
            if let Ok(v) = r.json::<Value>().await {
                if let Some(a) = v.get("result").and_then(Value::as_array) {
                    out.extend(a.iter().cloned());
                }
            }
        }
    }
    out
}

fn yandex_cover(uri: &str) -> String {
    if uri.is_empty() {
        return String::new();
    }
    let v = uri.replace("%%", "400x400");
    if v.starts_with("http") { v } else { format!("https://{v}") }
}

fn yandex_track(t: &Value, fallback_album: &str) -> Option<Value> {
    let mut title = s(t.get("title")).trim().to_string();
    if title.is_empty() {
        return None;
    }
    let version = s(t.get("version")).trim().to_string();
    if !version.is_empty() && !title.to_lowercase().contains(&version.to_lowercase()) {
        title.push_str(&format!(" ({version})"));
    }
    let artist = t
        .get("artists")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|x| s(x.get("name"))).filter(|n| !n.is_empty()).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();
    let album = t.pointer("/albums/0");
    let cover = yandex_cover(
        [s(t.get("ogImage")), s(t.get("coverUri")), s(album.and_then(|a| a.get("coverUri")))]
            .into_iter()
            .find(|v| !v.is_empty())
            .unwrap_or(""),
    );
    let duration = (t.get("durationMs").and_then(Value::as_f64).unwrap_or(0.0) / 1000.0).round();
    let album_title = album.map(|a| s(a.get("title"))).filter(|v| !v.is_empty()).unwrap_or(fallback_album);
    Some(json!({ "title": title, "artist": artist, "album": album_title, "duration": duration, "cover": cover, "thumbnail": cover, "image": cover }))
}

pub async fn yandex_import(ctx: &Arc<Ctx>, input: &str) -> Reply {
    let fail = |status: u16| match status {
        401 | 403 => err(403, "private", "private"),
        404 => err(404, "not found", "not found"),
        429 => err(429, "too many requests", "too many requests"),
        other => err(if other == 0 { 502 } else { other }, "failed", "Failed to fetch from Yandex Music"),
    };
    let text = input.trim();
    let seg = |after: &str| -> Option<String> {
        text.split(after).nth(1).map(|r| r.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '.').collect())
    };
    let digits = |after: &str| -> Option<String> {
        text.split(after).nth(1).map(|r| r.chars().take_while(char::is_ascii_digit).collect::<String>()).filter(|d| !d.is_empty())
    };

    let mut name = "Yandex Music Playlist".to_string();
    let mut cover = String::new();
    let raw: Vec<Value>;

    if let (Some(user), Some(kind)) = (text.split("users/").nth(1).map(|r| r.split('/').next().unwrap_or("").to_string()), digits("/playlists/")) {
        let path = format!("/users/{user}/playlists/{kind}");
        let data = match yandex_get(ctx, &path).await { Ok(d) => d, Err(c) => return fail(c) };
        let r = data.get("result").cloned().unwrap_or(Value::Null);
        name = if s(r.get("title")).is_empty() { name } else { s(r.get("title")).to_string() };
        cover = yandex_cover(s(r.get("ogImage")));
        let items = r.get("tracks").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut resolved: Vec<Value> = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        for it in &items {
            if let Some(t) = it.get("track").filter(|t| t.get("title").is_some()) {
                resolved.push(t.clone());
            } else if let Some(id) = it.get("id") {
                let id = id.to_string().trim_matches('"').to_string();
                missing.push(match it.get("albumId") { Some(a) => format!("{id}:{}", a.to_string().trim_matches('"')), None => id });
            }
        }
        if !missing.is_empty() {
            resolved.extend(yandex_tracks_by_ids(ctx, &missing).await);
        }
        raw = resolved;
    } else if let Some(id) = seg("playlists/").filter(|p| p.len() >= 8) {
        let data = match yandex_get(ctx, &format!("/playlist/{id}")).await { Ok(d) => d, Err(c) => return fail(c) };
        let r = data.get("result").cloned().unwrap_or(Value::Null);
        name = if s(r.get("title")).is_empty() { name } else { s(r.get("title")).to_string() };
        cover = yandex_cover(s(r.get("ogImage")));
        let items = r.get("tracks").and_then(Value::as_array).cloned().unwrap_or_default();
        raw = items.iter().filter_map(|i| i.get("track").cloned()).collect();
    } else if let (Some(album), Some(track)) = (digits("album/"), digits("/track/")) {
        raw = yandex_tracks_by_ids(ctx, &[format!("{track}:{album}")]).await;
    } else if let Some(track) = digits("track/") {
        raw = yandex_tracks_by_ids(ctx, &[track]).await;
    } else if let Some(album) = digits("album/") {
        let data = match yandex_get(ctx, &format!("/albums/{album}/with-tracks")).await { Ok(d) => d, Err(c) => return fail(c) };
        let r = data.get("result").cloned().unwrap_or(Value::Null);
        name = if s(r.get("title")).is_empty() { "Yandex Music Album".into() } else { s(r.get("title")).to_string() };
        cover = yandex_cover(s(r.get("ogImage")));
        raw = r.get("volumes").and_then(Value::as_array).map(|v| v.iter().filter_map(Value::as_array).flatten().cloned().collect()).unwrap_or_default();
    } else {
        return err(400, "unsupported link", "unsupported link");
    }

    let tracks: Vec<Value> = raw.iter().filter_map(|t| yandex_track(t, &name)).collect();
    if tracks.is_empty() {
        return fail(404);
    }
    if cover.is_empty() {
        cover = s(tracks[0].get("cover")).to_string();
    }
    (200, json!({ "success": true, "name": name, "cover": cover, "total": raw.len(), "skipped": raw.len() - tracks.len(), "tracks": tracks }))
}
