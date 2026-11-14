// Album and playlist search tabs (iTunes Search API and Deezer, both public).
use std::collections::HashSet;
use std::time::Duration;

use serde_json::{Value, json};

use crate::{Ctx, ytm::urlencode};

type Reply = (u16, Value);

fn s(v: Option<&Value>) -> &str {
    v.and_then(Value::as_str).unwrap_or("")
}

async fn get_json(ctx: &Ctx, url: &str) -> Option<Value> {
    ctx.client.get(url).timeout(Duration::from_secs(8)).send().await.ok()?.json::<Value>().await.ok()
}

fn art(item: &Value) -> String {
    s(item.get("artworkUrl100")).replace("100x100", "600x600")
}

fn year(item: &Value) -> String {
    s(item.get("releaseDate")).chars().take(4).collect()
}

fn norm(t: &str) -> String {
    t.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

fn itunes_album(item: &Value) -> Value {
    json!({
        "id": item.get("collectionId").map(|v| v.to_string()).unwrap_or_default(),
        "title": s(item.get("collectionName")), "artist": s(item.get("artistName")),
        "year": year(item), "trackCount": item.get("trackCount").and_then(Value::as_u64).unwrap_or(0),
        "cover": art(item)
    })
}

pub async fn album_search(ctx: &Ctx, q: &str) -> Reply {
    let q = q.trim();
    if q.is_empty() {
        return (200, json!({ "success": true, "albums": [] }));
    }
    // A pasted Apple Music album link resolves to exactly that album.
    if let Some(id) = q
        .split("/album/")
        .nth(1)
        .and_then(|r| r.split('/').find(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())))
    {
        if let Some(d) = get_json(ctx, &format!("https://itunes.apple.com/lookup?id={id}&entity=album")).await {
            if let Some(item) = d.get("results").and_then(Value::as_array).and_then(|r| r.iter().find(|x| s(x.get("wrapperType")) == "collection")) {
                return (200, json!({ "success": true, "albums": [itunes_album(item)] }));
            }
        }
    }
    let clean: String = q.replace(" - ", " ").split_whitespace().filter(|w| !w.eq_ignore_ascii_case("by")).collect::<Vec<_>>().join(" ");
    let mut urls = vec![
        format!("https://itunes.apple.com/search?term={}&entity=album&media=music&limit=25", urlencode(q)),
        format!("https://itunes.apple.com/search?term={}&media=music&limit=25", urlencode(q)),
    ];
    if !clean.is_empty() && !clean.eq_ignore_ascii_case(q) {
        urls.push(format!("https://itunes.apple.com/search?term={}&entity=album&media=music&limit=25", urlencode(&clean)));
        urls.push(format!("https://itunes.apple.com/search?term={}&media=music&limit=25", urlencode(&clean)));
    }
    let responses = futures_join(ctx, &urls).await;
    let mut seen = HashSet::new();
    let mut albums: Vec<Value> = Vec::new();
    for data in responses.into_iter().flatten() {
        for item in data.get("results").and_then(Value::as_array).cloned().unwrap_or_default() {
            if item.get("collectionId").is_none() {
                continue;
            }
            let name = s(item.get("collectionName")).to_string();
            let lower = name.to_lowercase();
            if lower.ends_with("- single") {
                continue;
            }
            let count = item.get("trackCount").and_then(Value::as_u64).unwrap_or(0);
            if count < 4 && !lower.ends_with("- ep") {
                continue;
            }
            if seen.insert(format!("{}:{}", norm(&name), norm(s(item.get("artistName"))))) {
                albums.push(itunes_album(&item));
            }
        }
    }
    let ql = clean.to_lowercase();
    let words: Vec<String> = ql.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() > 1).map(String::from).collect();
    let score = |a: &Value| -> i32 {
        let title = s(a.get("title")).to_lowercase();
        let artist = s(a.get("artist")).to_lowercase();
        let full = format!("{title} {artist}");
        let mut sc = 0;
        for w in &words {
            if title.contains(w.as_str()) { sc += 15 } else if artist.contains(w.as_str()) { sc += 10 }
        }
        if title == ql || full == ql { sc += 50 } else if full.contains(&ql) || ql.contains(&title) { sc += 25 }
        sc
    };
    albums.sort_by_key(|a| std::cmp::Reverse(score(a)));
    albums.truncate(12);
    (200, json!({ "success": true, "albums": albums }))
}

async fn futures_join(ctx: &Ctx, urls: &[String]) -> Vec<Option<Value>> {
    let mut out = Vec::new();
    for u in urls {
        out.push(get_json(ctx, u).await);
    }
    out
}

pub async fn playlist_search(ctx: &Ctx, q: &str) -> Reply {
    let q = q.trim();
    if q.is_empty() {
        return (200, json!({ "success": true, "playlists": [] }));
    }
    let mut playlists = Vec::new();
    if let Some(d) = get_json(ctx, &format!("https://api.deezer.com/search/playlist?q={}&limit=12", urlencode(q))).await {
        for item in d.get("data").and_then(Value::as_array).cloned().unwrap_or_default() {
            let id = item.get("id").map(|v| v.to_string()).unwrap_or_default();
            let cover = ["picture_xl", "picture_big", "picture_medium"].iter().map(|k| s(item.get(*k))).find(|v| !v.is_empty()).unwrap_or("");
            playlists.push(json!({
                "id": format!("dzpl_{id}"),
                "title": s(item.get("title")),
                "author": if s(item.pointer("/user/name")).is_empty() { "Playlist" } else { s(item.pointer("/user/name")) },
                "trackCount": item.get("nb_tracks").and_then(Value::as_u64).unwrap_or(0),
                "cover": cover,
                "url": if s(item.get("link")).is_empty() { format!("https://www.deezer.com/playlist/{id}") } else { s(item.get("link")).to_string() },
                "source": "deezer"
            }));
        }
    }
    (200, json!({ "success": true, "playlists": playlists }))
}

fn song(item: &Value, fallback_artist: &str) -> Value {
    let ms = item.get("trackTimeMillis").and_then(Value::as_u64).unwrap_or(0);
    json!({
        "id": item.get("trackId").map(|v| v.to_string()).unwrap_or_default(),
        "no": item.get("trackNumber").and_then(Value::as_u64).unwrap_or(1),
        "disc": item.get("discNumber").and_then(Value::as_u64).unwrap_or(1),
        "title": s(item.get("trackName")),
        "artist": if s(item.get("artistName")).is_empty() { fallback_artist } else { s(item.get("artistName")) },
        "duration": (ms as f64 / 1000.0).round(), "durationMs": ms,
        "thumbnail": art(item),
        "isExplicit": s(item.get("trackExplicitness")) == "explicit"
    })
}

fn sort_tracks(t: &mut [Value]) {
    t.sort_by_key(|x| (x.get("disc").and_then(Value::as_u64).unwrap_or(1), x.get("no").and_then(Value::as_u64).unwrap_or(1)));
}

pub async fn album_tracks(ctx: &Ctx, id: &str, title: &str, artist: &str) -> Reply {
    let (id, title, artist) = (id.trim(), title.trim(), artist.trim());
    if id.is_empty() && title.is_empty() {
        return (400, json!({ "success": false, "error": "Invalid album id" }));
    }
    if !id.is_empty() {
        if let Some(d) = get_json(ctx, &format!("https://itunes.apple.com/lookup?id={}&entity=song", urlencode(id))).await {
            let results = d.get("results").and_then(Value::as_array).cloned().unwrap_or_default();
            let album = results.iter().find(|r| s(r.get("wrapperType")) == "collection" || s(r.get("collectionType")) == "Album");
            let mut tracks: Vec<Value> = results
                .iter()
                .filter(|r| s(r.get("wrapperType")) == "track" || s(r.get("kind")) == "song")
                .map(|r| song(r, artist))
                .collect();
            if !tracks.is_empty() {
                sort_tracks(&mut tracks);
                let pick = |k: &str, d: &str| album.map(|a| s(a.get(k)).to_string()).filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string());
                return (200, json!({
                    "success": true,
                    "album": {
                        "id": album.and_then(|a| a.get("collectionId")).map(|v| v.to_string()).unwrap_or_else(|| id.to_string()),
                        "title": pick("collectionName", title), "artist": pick("artistName", artist),
                        "year": album.map(year).unwrap_or_default(), "trackCount": tracks.len(),
                        "cover": album.map(art).unwrap_or_default()
                    },
                    "tracks": tracks
                }));
            }
        }
        if let Some(d) = get_json(ctx, &format!("https://api.deezer.com/album/{}/tracks?limit=50", urlencode(id))).await {
            let items = d.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
            let mut tracks: Vec<Value> = items
                .iter()
                .enumerate()
                .map(|(i, it)| {
                    let dur = it.get("duration").and_then(Value::as_u64).unwrap_or(0);
                    let thumb = ["cover_xl", "cover_big", "cover_medium"].iter().map(|k| s(it.pointer(&format!("/album/{k}")))).find(|v| !v.is_empty()).unwrap_or("");
                    json!({
                        "id": it.get("id").map(|v| v.to_string()).unwrap_or_default(),
                        "no": it.get("track_position").and_then(Value::as_u64).unwrap_or(i as u64 + 1),
                        "disc": it.get("disk_number").and_then(Value::as_u64).unwrap_or(1),
                        "title": s(it.get("title")),
                        "artist": if s(it.pointer("/artist/name")).is_empty() { artist } else { s(it.pointer("/artist/name")) },
                        "duration": dur, "durationMs": dur * 1000, "thumbnail": thumb,
                        "isExplicit": it.get("explicit_lyrics").and_then(Value::as_bool).unwrap_or(false)
                    })
                })
                .collect();
            if !tracks.is_empty() {
                sort_tracks(&mut tracks);
                return (200, json!({
                    "success": true,
                    "album": { "id": id, "title": title, "artist": artist, "year": "", "trackCount": tracks.len(), "cover": "" },
                    "tracks": tracks
                }));
            }
        }
    }
    let term = format!("{artist} {title}").trim().to_string();
    if let Some(d) = get_json(ctx, &format!("https://itunes.apple.com/search?term={}&entity=song&limit=25", urlencode(&term))).await {
        let results = d.get("results").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut tracks: Vec<Value> = results
            .iter()
            .filter(|r| s(r.get("wrapperType")) == "track" || s(r.get("kind")) == "song")
            .map(|r| song(r, artist))
            .collect();
        if !tracks.is_empty() {
            sort_tracks(&mut tracks);
            let first = &results[0];
            return (200, json!({
                "success": true,
                "album": {
                    "id": if id.is_empty() { first.get("collectionId").map(|v| v.to_string()).unwrap_or_default() } else { id.to_string() },
                    "title": if title.is_empty() { s(first.get("collectionName")) } else { title },
                    "artist": if artist.is_empty() { s(first.get("artistName")) } else { artist },
                    "year": year(first), "trackCount": tracks.len(), "cover": art(first)
                },
                "tracks": tracks
            }));
        }
    }
    (404, json!({ "success": false, "error": "Failed to load album" }))
}
