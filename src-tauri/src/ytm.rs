// Music metadata: YouTube Music search, yt-dlp search and stream resolving, Deezer charts, LRCLIB lyrics.
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::{Ctx, tools};

pub const UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const YTM_SONGS_PARAMS: &str = "EgWKAQIIAWoMEA4QChADEAQQCRAF";
const STREAM_TTL: Duration = Duration::from_secs(3 * 3600);
const SEARCH_TTL: Duration = Duration::from_secs(300);

fn text(v: Option<&Value>) -> &str {
    v.and_then(Value::as_str).unwrap_or("")
}

fn parse_duration(t: &str) -> u64 {
    let parts: Vec<&str> = t.split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return 0;
    }
    let mut total = 0u64;
    for p in parts {
        match p.trim().parse::<u64>() {
            Ok(n) => total = total * 60 + n,
            Err(_) => return 0,
        }
    }
    total
}

fn runs_text(runs: Option<&Value>) -> Vec<String> {
    runs.and_then(Value::as_array)
        .map(|a| a.iter().map(|r| text(r.get("text")).to_string()).collect())
        .unwrap_or_default()
}

fn valid_video_id(id: &str) -> bool {
    id.len() == 11 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn track(id: &str, title: &str, artist: &str, album: &str, duration: u64, thumbnail: &str, explicit: bool) -> Value {
    json!({
        "id": id, "title": title, "artist": artist, "album": album,
        "duration": if duration == 0 { 180 } else { duration },
        "thumbnail": thumbnail,
        "url": format!("https://www.youtube.com/watch?v={id}"),
        "platform": "youtube", "isExplicit": explicit
    })
}

pub fn parse_ytm_songs(root: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let sections = root
        .pointer("/contents/tabbedSearchResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for section in &sections {
        let mut items: Vec<&Value> = Vec::new();
        for key in ["musicShelfRenderer", "itemSectionRenderer", "musicCardShelfRenderer"] {
            if let Some(arr) = section.get(key).and_then(|s| s.get("contents")).and_then(Value::as_array) {
                items.extend(arr.iter());
            }
        }
        for wrap in items {
            let Some(r) = wrap.get("musicResponsiveListItemRenderer") else { continue };
            let cols = r.get("flexColumns").and_then(Value::as_array);
            let col_runs = |i: usize| {
                runs_text(cols.and_then(|c| c.get(i)).and_then(|c| {
                    c.pointer("/musicResponsiveListItemFlexColumnRenderer/text/runs")
                }))
            };
            let video_id = r
                .pointer("/playlistItemData/videoId")
                .or_else(|| {
                    r.pointer("/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/watchEndpoint/videoId")
                })
                .and_then(Value::as_str)
                .unwrap_or("");
            if !valid_video_id(video_id) {
                continue;
            }
            let title = col_runs(0).concat().trim().to_string();
            let mut segments: Vec<String> = Vec::new();
            let mut cur = String::new();
            for run in col_runs(1) {
                if run == " • " {
                    segments.push(cur.trim().to_string());
                    cur.clear();
                } else {
                    cur.push_str(&run);
                }
            }
            segments.push(cur.trim().to_string());
            let mut clean: Vec<String> = segments
                .into_iter()
                .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("song") && !s.eq_ignore_ascii_case("video"))
                .collect();
            let mut duration = 0;
            if let Some(last) = clean.last() {
                let d = parse_duration(last);
                if d > 0 {
                    duration = d;
                    clean.pop();
                }
            }
            let artist = clean.first().cloned().unwrap_or_default();
            let artist = artist.strip_suffix(" - Topic").unwrap_or(&artist).trim().to_string();
            let album = clean.get(1).cloned().unwrap_or_default();

            let thumbs = r
                .pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
                .and_then(Value::as_array);
            let mut thumb = thumbs
                .and_then(|t| t.last())
                .map(|t| text(t.get("url")).to_string())
                .unwrap_or_else(|| format!("https://i.ytimg.com/vi/{video_id}/hqdefault.jpg"));
            if let Some(i) = thumb.rfind("=w") {
                if thumb[i + 2..].starts_with(|c: char| c.is_ascii_digit()) {
                    thumb.truncate(i);
                    thumb.push_str("=w544-h544");
                }
            }
            let explicit = r
                .get("badges")
                .and_then(Value::as_array)
                .is_some_and(|b| {
                    b.iter().any(|x| {
                        x.pointer("/musicInlineBadgeRenderer/icon/iconType").and_then(Value::as_str)
                            == Some("MUSIC_EXPLICIT_BADGE")
                    })
                });
            if title.is_empty() || (duration > 0 && duration < 30) {
                continue;
            }
            let artist = if artist.is_empty() { "Unknown Artist".to_string() } else { artist };
            let mut item = track(video_id, &title, &artist, &album, duration, &thumb, explicit);
            let plays = col_runs(2).concat();
            if let Some(n) = plays.trim().strip_suffix(" plays").or_else(|| plays.trim().strip_suffix(" views")) {
                item["playsText"] = json!(n.trim());
            }
            out.push(item);
        }
    }
    out
}

pub async fn ytm_search(ctx: &Ctx, query: &str) -> Result<Vec<Value>, String> {
    let body = json!({
        "context": { "client": { "clientName": "WEB_REMIX", "clientVersion": "1.20250101.01.00", "hl": "en", "gl": "US" } },
        "query": query,
        "params": YTM_SONGS_PARAMS
    });
    let resp = ctx
        .client
        .post("https://music.youtube.com/youtubei/v1/search?prettyPrint=false")
        .header("Origin", "https://music.youtube.com")
        .header("Referer", "https://music.youtube.com/")
        .header("User-Agent", UA)
        .timeout(Duration::from_secs(10))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("status {}", resp.status()));
    }
    let root: Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(parse_ytm_songs(&root))
}

// yt-dlp based search for YouTube ("ytsearch") and SoundCloud ("scsearch").
pub async fn ytdlp_search(ctx: &Arc<Ctx>, prefix: &str, query: &str, n: usize, platform: &str) -> Vec<Value> {
    let ctx = ctx.clone();
    let target = format!("{prefix}{n}:{query}");
    let platform = platform.to_string();
    tauri::async_runtime::spawn_blocking(move || -> Vec<Value> {
        let mut cmd = tools::command(tools::tool_path(&ctx, "yt-dlp"));
        cmd.args(["--flat-playlist", "-J", "--no-warnings"]).arg(&target);
        let Ok(out) = tools::run_cmd(cmd) else { return Vec::new() };
        let Ok(json) = serde_json::from_str::<Value>(&out) else { return Vec::new() };
        let entries = json.get("entries").and_then(Value::as_array).cloned().unwrap_or_default();
        entries
            .iter()
            .filter_map(|e| {
                let id = text(e.get("id"));
                let mut url = text(e.get("webpage_url")).to_string();
                if url.is_empty() {
                    url = text(e.get("url")).to_string();
                }
                if id.is_empty() || !url.starts_with("http") {
                    return None;
                }
                let artist = ["artist", "uploader", "channel"]
                    .iter()
                    .map(|k| text(e.get(*k)))
                    .find(|s| !s.is_empty())
                    .unwrap_or("")
                    .trim_end_matches(" - Topic")
                    .to_string();
                let mut thumb = text(e.get("thumbnail")).to_string();
                if thumb.is_empty() {
                    thumb = e
                        .get("thumbnails")
                        .and_then(Value::as_array)
                        .and_then(|a| a.last())
                        .map(|t| text(t.get("url")).to_string())
                        .unwrap_or_default();
                }
                if thumb.is_empty() && platform == "youtube" {
                    thumb = format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg");
                }
                let views = e.get("view_count").and_then(Value::as_u64).unwrap_or(0);
                Some(json!({
                    "id": id, "title": text(e.get("title")), "artist": artist, "album": "",
                    "duration": e.get("duration").and_then(Value::as_f64).unwrap_or(0.0),
                    "thumbnail": thumb, "url": url, "platform": platform, "isExplicit": false,
                    "playsText": if views > 0 { format_listeners(views) } else { String::new() }
                }))
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

pub async fn search(ctx: &Arc<Ctx>, query: &str, platform: &str, limit: usize, offset: usize) -> Value {
    let q = query.trim();
    if q.is_empty() {
        return json!({ "success": true, "tracks": [], "hasMore": false });
    }
    let pool = if platform == "all" || platform == "all-noyt" {
        let with_youtube = platform == "all";
        let key = format!("{platform}:{}", q.to_lowercase());
        let cached = {
            let cache = ctx.search_cache.lock().unwrap();
            cache.get(&key).filter(|(t, _)| t.elapsed() < SEARCH_TTL).map(|(_, v)| v.clone())
        };
        match cached {
            Some(p) => p,
            None => {
                let (a, b, c) = tokio::join!(
                    search_pool(ctx, "ytmusic", q),
                    async { if with_youtube { search_pool(ctx, "youtube", q).await } else { Vec::new() } },
                    search_pool(ctx, "soundcloud", q)
                );
                let merged = merge_pools(&[a, b, c]);
                if !merged.is_empty() {
                    ctx.search_cache.lock().unwrap().insert(key, (Instant::now(), merged.clone()));
                }
                merged
            }
        }
    } else if platform == "youtube" {
        search_pool(ctx, "youtube-raw", q).await
    } else {
        search_pool(ctx, platform, q).await
    };
    let tracks: Vec<Value> = pool.iter().skip(offset).take(limit).cloned().collect();
    json!({ "success": true, "tracks": tracks, "hasMore": offset + limit < pool.len() })
}

/// YouTube search returns every kind of video. Podcasts, interviews, shows, streams and very long or very short videos are not music
/// (podcasts have their own section), so they are dropped from the music search.
fn is_not_music(t: &Value, max_secs: f64) -> bool {
    let dur = t.get("duration").and_then(Value::as_f64).unwrap_or(0.0);
    if dur > 0.0 && (dur > max_secs || dur < 30.0) {
        return true;
    }
    let words = |s: &str| -> String {
        let low = s.to_lowercase();
        format!(" {} ", low.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" "))
    };
    let title = words(text(t.get("title")));
    let who = words(text(t.get("artist")));
    const TITLE: [&str; 52] = [
        "podcast", "podcasts", "подкаст", "подкасты", "интервью", "interview", "interviews", "выпуск", "выпуска", "эпизод", "episode", "full episode", "full show",
        "reaction", "реакция", "обзор", "review", "влог", "vlog", "стрим", "livestream", "live stream", "news", "новости", "лекция", "lecture", "documentary",
        "документальный", "ток шоу", "talk show", "shorts", "tutorial", "урок", "разбор", "сериал", "серия", "behind the scenes", "making of", "за кадром",
        "trailer", "трейлер", "gameplay", "прохождение", "как стать", "топ 10", "top 10", "speech", "речь", "q a", "ask me anything", "stand up", "стендап",
    ];
    const CHANNEL: [&str; 8] = ["podcast", "подкаст", "news", "новости", "tv show", "talk show", "stand up", "стендап"];
    TITLE.iter().any(|w| title.contains(&format!(" {w} "))) || CHANNEL.iter().any(|w| who.contains(&format!(" {w} ")))
}

async fn search_pool(ctx: &Arc<Ctx>, platform: &str, q: &str) -> Vec<Value> {
    let key = format!("{platform}:{}", q.to_lowercase());
    let cached = {
        let cache = ctx.search_cache.lock().unwrap();
        cache.get(&key).filter(|(t, _)| t.elapsed() < SEARCH_TTL).map(|(_, v)| v.clone())
    };
    if let Some(p) = cached {
        return p;
    }
    let mut pool = match platform {
        "soundcloud" => ytdlp_search(ctx, "scsearch", q, 30, "soundcloud").await,
        // the free search of the whole of YouTube: nothing is filtered out
        "youtube-raw" => ytdlp_search(ctx, "ytsearch", q, 40, "youtube").await,
        "youtube" => ytdlp_search(ctx, "ytsearch", q, 40, "youtube").await,
        _ => {
            let songs = ytm_search(ctx, q).await.unwrap_or_default();
            if songs.is_empty() { ytdlp_search(ctx, "ytsearch", q, 40, "youtube").await } else { songs }
        }
    };
    // SoundCloud keeps DJ sets and long tracks up to 20 minutes; YouTube music videos rarely run longer than 11.
    let max_secs = if platform == "soundcloud" { 1200.0 } else { 660.0 };
    if platform != "youtube-raw" {
        pool.retain(|t| !is_not_music(t, max_secs));
    }
    if !pool.is_empty() {
        ctx.search_cache.lock().unwrap().insert(key, (Instant::now(), pool.clone()));
    }
    pool
}

// ---- Podcasts (YouTube playlists are shows, their videos are episodes) ----

fn best_thumb(e: &Value) -> String {
    let t = text(e.get("thumbnail"));
    if !t.is_empty() {
        return t.to_string();
    }
    e.get("thumbnails")
        .and_then(Value::as_array)
        .and_then(|a| a.last())
        .map(|t| text(t.get("url")).to_string())
        .unwrap_or_default()
}

fn author_of(e: &Value) -> String {
    ["uploader", "channel"].iter().map(|k| text(e.get(*k))).find(|s| !s.is_empty()).unwrap_or("").to_string()
}

async fn yt_flat_json(ctx: &Arc<Ctx>, args: Vec<String>, target: String) -> Option<Value> {
    let ctx = ctx.clone();
    tauri::async_runtime::spawn_blocking(move || -> Option<Value> {
        let mut cmd = tools::command(tools::tool_path(&ctx, "yt-dlp"));
        cmd.args(["--flat-playlist", "-J", "--no-warnings"]).args(args).arg(&target);
        let out = tools::run_cmd(cmd).ok()?;
        serde_json::from_str::<Value>(&out).ok()
    })
    .await
    .ok()
    .flatten()
}

fn episode(e: &Value, show_title: &str) -> Option<Value> {
    let id = text(e.get("id"));
    if !valid_video_id(id) {
        return None;
    }
    let author = ["uploader", "channel"].iter().map(|k| text(e.get(*k))).find(|s| !s.is_empty()).unwrap_or(show_title);
    let mut thumb = best_thumb(e);
    if thumb.is_empty() {
        thumb = format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg");
    }
    Some(json!({
        "id": id, "title": text(e.get("title")), "artist": author, "album": show_title,
        "duration": e.get("duration").and_then(Value::as_f64).unwrap_or(0.0),
        "thumbnail": thumb, "url": format!("https://www.youtube.com/watch?v={id}"),
        "platform": "youtube", "isExplicit": false, "isPodcast": true
    }))
}

pub async fn podcast_shows(ctx: &Arc<Ctx>, query: &str, n: usize) -> Vec<Value> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let key = format!("pod-shows:{n}:{}", q.to_lowercase());
    if let Some(v) = ctx.search_cache.lock().unwrap().get(&key).filter(|(t, _)| t.elapsed() < SEARCH_TTL).map(|(_, v)| v.clone()) {
        return v;
    }
    let url = format!("https://www.youtube.com/results?search_query={}&sp=EgIQAw%253D%253D", urlencode(&format!("{q} podcast")));
    let Some(json) = yt_flat_json(ctx, vec!["--playlist-end".into(), n.to_string()], url).await else { return Vec::new() };
    let shows: Vec<Value> = json
        .get("entries")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let id = text(e.get("id"));
                    if id.is_empty() {
                        return None;
                    }
                    Some(json!({
                        "id": id, "title": text(e.get("title")),
                        "author": author_of(e),
                        "thumbnail": best_thumb(e),
                        "url": format!("https://www.youtube.com/playlist?list={id}")
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    if !shows.is_empty() {
        ctx.search_cache.lock().unwrap().insert(key, (Instant::now(), shows.clone()));
    }
    shows
}

pub async fn podcast_show(ctx: &Arc<Ctx>, id: &str, n: usize) -> Value {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return json!({ "success": false, "error": "bad show id" });
    }
    let url = format!("https://www.youtube.com/playlist?list={id}");
    let Some(json) = yt_flat_json(ctx, vec!["--playlist-end".into(), n.to_string()], url).await else {
        return json!({ "success": false, "error": "could not load the show" });
    };
    let title = text(json.get("title")).to_string();
    let episodes: Vec<Value> = json
        .get("entries")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|e| episode(e, &title)).collect())
        .unwrap_or_default();
    let thumb = {
        let t = best_thumb(&json);
        if t.is_empty() { episodes.first().map(|e| text(e.get("thumbnail")).to_string()).unwrap_or_default() } else { t }
    };
    json!({
        "success": true,
        "show": {
            "id": id, "title": title,
            "author": author_of(&json),
            "thumbnail": thumb, "count": json.get("playlist_count").and_then(Value::as_u64).unwrap_or(episodes.len() as u64),
            "url": format!("https://www.youtube.com/playlist?list={id}")
        },
        "episodes": episodes
    })
}

pub async fn podcast_episode_search(ctx: &Arc<Ctx>, query: &str) -> Vec<Value> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let key = format!("pod-eps:{}", q.to_lowercase());
    if let Some(v) = ctx.search_cache.lock().unwrap().get(&key).filter(|(t, _)| t.elapsed() < SEARCH_TTL).map(|(_, v)| v.clone()) {
        return v;
    }
    let found = ytdlp_search(ctx, "ytsearch", &format!("{q} podcast"), 30, "youtube").await;
    let eps: Vec<Value> = found
        .into_iter()
        .filter(|t| t.get("duration").and_then(Value::as_f64).unwrap_or(0.0) >= 600.0)
        .map(|mut t| {
            t["isPodcast"] = json!(true);
            t
        })
        .collect();
    if !eps.is_empty() {
        ctx.search_cache.lock().unwrap().insert(key, (Instant::now(), eps.clone()));
    }
    eps
}

/// Round-robin merge of several result lists, dropping repeats of the same title + artist.
fn merge_pools(pools: &[Vec<Value>]) -> Vec<Value> {
    let norm = |s: &str| -> String { s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect() };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let longest = pools.iter().map(Vec::len).max().unwrap_or(0);
    for i in 0..longest {
        for pool in pools {
            let Some(t) = pool.get(i) else { continue };
            let key = format!("{}:{}", norm(text(t.get("title"))), norm(text(t.get("artist"))));
            if seen.insert(key) {
                out.push(t.clone());
            }
        }
    }
    out
}

fn format_listeners(n: u64) -> String {
    if n >= 1_000_000 { format!("{:.1}M", n as f64 / 1e6) } else if n >= 1000 { format!("{}K", n / 1000) } else { n.to_string() }
}

fn find_all<'a>(v: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if k == key {
                    out.push(x);
                } else {
                    find_all(x, key, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| find_all(x, key, out)),
        _ => {}
    }
}

/// "Song (Official Music Video)" -> "Song", and "Artist - Song" -> "Song" when the artist is already known.
fn clean_chart_title(title: &str, artist: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    let mut chunk = String::new();
    let junk = ["official", "video", "клип", "lyric", "audio", "mv", "m/v", "hd", "4k"];
    for c in title.chars() {
        match c {
            '(' | '[' => {
                if depth == 0 {
                    chunk.clear();
                }
                depth += 1;
                chunk.push(c);
            }
            ')' | ']' if depth > 0 => {
                chunk.push(c);
                depth -= 1;
                if depth == 0 {
                    let low = chunk.to_lowercase();
                    if !junk.iter().any(|j| low.contains(j)) {
                        out.push_str(&chunk);
                    }
                    chunk.clear();
                }
            }
            _ if depth > 0 => chunk.push(c),
            _ => out.push(c),
        }
    }
    out.push_str(&chunk);
    let mut out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let prefix = format!("{} - ", artist.to_lowercase());
    if out.to_lowercase().starts_with(&prefix) {
        out = out[prefix.len()..].trim().to_string();
    }
    if out.is_empty() { title.to_string() } else { out }
}

/// The video charts also hold TV shows, live sessions and clips from broadcaster channels; those are not tracks.
fn is_broadcast_upload(title: &str, artist: &str) -> bool {
    let a = artist.to_lowercase();
    let t = title.to_lowercase();
    const CHANNELS: &[&str] = &[
        "kbs", "sbs", "mbc", "tonight show", "triple j", "mosh pit", "entertainment", "news", "tv", "show", "official channel", "late night", "live session",
        "podcast", "episode", "trailer", "스브스", "예능", "mnet", "zoom", "all the k pop", "music bank", "inkigayo", "show champion", "fancam", "facecam",
    ];
    let title_junk = ["facecam", "fancam", "show champion", "inkigayo", "music bank", "full episode", "reaction"];
    if title_junk.iter().any(|j| t.contains(j)) {
        return true;
    }
    CHANNELS.iter().any(|c| {
        if c.len() <= 3 {
            a.split(|ch: char| !ch.is_alphanumeric()).any(|w| w == *c)
        } else {
            a.contains(c)
        }
    }) || title.contains('#') || t.contains(" | ") || t.contains("full episode") || t.contains("reaction")
}

/// The official YouTube Music chart of a country (ISO code, e.g. "RU"); unsupported countries get the global chart.
pub async fn yt_chart(ctx: &Ctx, country: &str) -> (String, Vec<Value>) {
    let code: String = country.trim().to_uppercase().chars().filter(|c| c.is_ascii_alphabetic()).take(2).collect();
    let code = if code.len() == 2 { code } else { "ZZ".to_string() };
    let key = format!("chart:{code}");
    if let Some((t, v)) = ctx.search_cache.lock().unwrap().get(&key) {
        if t.elapsed() < Duration::from_secs(6 * 3600) {
            let name = v.first().map(|f| text(f.get("chartName")).to_string()).unwrap_or_default();
            return (name, v.clone());
        }
    }
    let browse = |body: Value| async move {
        ctx.client
            .post("https://music.youtube.com/youtubei/v1/browse?prettyPrint=false")
            .header("Origin", "https://music.youtube.com")
            .header("Referer", "https://music.youtube.com/")
            .header("User-Agent", UA)
            .timeout(Duration::from_secs(12))
            .json(&body)
            .send()
            .await
            .ok()?
            .json::<Value>()
            .await
            .ok()
    };
    let ctx_json = json!({ "client": { "clientName": "WEB_REMIX", "clientVersion": "1.20250101.01.00", "hl": "en", "gl": "US" } });
    let Some(root) = browse(json!({ "context": ctx_json, "browseId": "FEmusic_charts", "formData": { "selectedValues": [code] } })).await else {
        return (String::new(), Vec::new());
    };
    let mut buttons = Vec::new();
    find_all(&root, "musicSortFilterButtonRenderer", &mut buttons);
    let chart_name = buttons.first().map(|b| text(b.pointer("/title/runs/0/text")).to_string()).unwrap_or_default();
    let mut cards = Vec::new();
    find_all(&root, "musicTwoRowItemRenderer", &mut cards);
    let mut best: Option<String> = None;
    for c in &cards {
        let title = text(c.pointer("/title/runs/0/text"));
        let id = text(c.pointer("/title/runs/0/navigationEndpoint/browseEndpoint/browseId"));
        if id.is_empty() {
            continue;
        }
        if title.contains("Top 100") {
            best = Some(id.to_string());
            break;
        }
        if best.is_none() && !title.contains("Trending") {
            best = Some(id.to_string());
        }
    }
    let Some(pl) = best else { return (chart_name, Vec::new()) };
    let Some(list) = browse(json!({ "context": ctx_json, "browseId": pl })).await else { return (chart_name, Vec::new()) };
    let mut rows = Vec::new();
    find_all(&list, "musicResponsiveListItemRenderer", &mut rows);
    let mut out: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for r in rows {
        let id = text(r.pointer("/playlistItemData/videoId"));
        if id.is_empty() || !seen.insert(id.to_string()) {
            continue;
        }
        let col = |i: usize| -> String {
            r.pointer(&format!("/flexColumns/{i}/musicResponsiveListItemFlexColumnRenderer/text/runs"))
                .and_then(Value::as_array)
                .map(|runs| runs.iter().map(|x| text(x.get("text"))).collect::<String>())
                .unwrap_or_default()
        };
        let raw_title = col(0);
        let artist = col(1);
        let artist = artist.strip_suffix(" - Topic").unwrap_or(&artist).trim().to_string();
        if raw_title.is_empty() || is_broadcast_upload(&raw_title, &artist) {
            continue;
        }
        let title = clean_chart_title(&raw_title, &artist);
        let dur = text(r.pointer("/fixedColumns/0/musicResponsiveListItemFixedColumnRenderer/text/runs/0/text"));
        let thumb = format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg");
        let mut item = track(id, &title, if artist.is_empty() { "Unknown Artist" } else { &artist }, "", parse_duration(dur), &thumb, false);
        item["platform"] = json!("ytmusic");
        item["chartPosition"] = json!(out.len() + 1);
        item["chartName"] = json!(chart_name);
        out.push(item);
        if out.len() >= 50 {
            break;
        }
    }
    if !out.is_empty() {
        ctx.search_cache.lock().unwrap().insert(key, (Instant::now(), out.clone()));
    }
    (chart_name, out)
}

/// The id of the YouTube video that plays "artist - title" (cached for a day).
pub async fn find_video_id(ctx: &Ctx, artist: &str, title: &str) -> Option<String> {
    let q = format!("{artist} {title}").trim().to_lowercase();
    if q.is_empty() {
        return None;
    }
    let key = format!("vid:{q}");
    if let Some((t, v)) = ctx.search_cache.lock().unwrap().get(&key) {
        if t.elapsed() < Duration::from_secs(24 * 3600) {
            return v.first().map(|x| text(x.get("id")).to_string()).filter(|x| !x.is_empty());
        }
    }
    let found = ytm_search(ctx, &q).await.unwrap_or_default();
    let id = found.first().map(|x| text(x.get("id")).to_string()).unwrap_or_default();
    ctx.search_cache.lock().unwrap().insert(key, (Instant::now(), vec![json!({ "id": id })]));
    if id.is_empty() { None } else { Some(id) }
}

/// "Radio" of a track on YouTube Music: about 50 tracks that people play after it. Works for music of any language.
pub async fn yt_radio(ctx: &Ctx, video_id: &str) -> Vec<Value> {
    if video_id.is_empty() {
        return Vec::new();
    }
    let key = format!("radio:{video_id}");
    if let Some((t, v)) = ctx.search_cache.lock().unwrap().get(&key) {
        if t.elapsed() < Duration::from_secs(30 * 60) {
            return v.clone();
        }
    }
    let body = json!({
        "context": { "client": { "clientName": "WEB_REMIX", "clientVersion": "1.20250101.01.00", "hl": "en", "gl": "US" } },
        "videoId": video_id, "playlistId": format!("RDAMVM{video_id}"), "isAudioOnly": true
    });
    let Ok(resp) = ctx
        .client
        .post("https://music.youtube.com/youtubei/v1/next?prettyPrint=false")
        .header("Origin", "https://music.youtube.com")
        .header("Referer", "https://music.youtube.com/")
        .header("User-Agent", UA)
        .timeout(Duration::from_secs(10))
        .json(&body)
        .send()
        .await
    else {
        return Vec::new();
    };
    let Ok(root) = resp.json::<Value>().await else { return Vec::new() };
    let mut rows = Vec::new();
    find_all(&root, "playlistPanelVideoRenderer", &mut rows);
    let mut out: Vec<Value> = Vec::new();
    for r in rows {
        let id = text(r.get("videoId"));
        let title = text(r.pointer("/title/runs/0/text"));
        if id.is_empty() || title.is_empty() || id == video_id {
            continue;
        }
        // "Artist & Artist • 14M views • 149K likes"
        let mut parts: Vec<String> = vec![String::new()];
        for run in r.pointer("/longBylineText/runs").and_then(Value::as_array).into_iter().flatten() {
            let t = text(run.get("text"));
            if t.trim() == "\u{2022}" {
                parts.push(String::new());
            } else {
                parts.last_mut().unwrap().push_str(t);
            }
        }
        let artist = parts[0].trim().to_string();
        if artist.is_empty() {
            continue;
        }
        let dur = parse_duration(text(r.pointer("/lengthText/runs/0/text")));
        if dur > 0 && (dur > 660 || dur < 40) {
            continue;
        }
        let thumb = format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg");
        let mut item = track(id, title, artist.strip_suffix(" - Topic").unwrap_or(&artist), "", dur, &thumb, false);
        item["platform"] = json!("ytmusic");
        if let Some(v) = parts.get(1).and_then(|p| p.trim().strip_suffix(" views")) {
            item["playsText"] = json!(v.trim());
        }
        out.push(item);
    }
    if !out.is_empty() {
        ctx.search_cache.lock().unwrap().insert(key, (Instant::now(), out.clone()));
    }
    out
}

pub async fn deezer_chart(ctx: &Ctx, limit: usize) -> Vec<Value> {
    let url = format!("https://api.deezer.com/chart/0/tracks?limit={limit}");
    let Ok(resp) = ctx.client.get(url).timeout(Duration::from_secs(6)).send().await else { return Vec::new() };
    let Ok(root) = resp.json::<Value>().await else { return Vec::new() };
    root.get("data")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .enumerate()
                .map(|(i, it)| {
                    let cover = ["cover_xl", "cover_big", "cover_medium"]
                        .iter()
                        .map(|k| text(it.pointer(&format!("/album/{k}"))))
                        .find(|s| !s.is_empty())
                        .unwrap_or("")
                        .to_string();
                    json!({
                        "id": format!("dz-{}", it.get("id").and_then(Value::as_u64).unwrap_or(0)),
                        "title": text(it.get("title")),
                        "artist": text(it.pointer("/artist/name")),
                        "thumbnail": cover, "cover": cover,
                        "duration": it.get("duration").and_then(Value::as_u64).unwrap_or(0),
                        "platform": "ytmusic", "url": "",
                        "rank": it.get("rank").cloned().unwrap_or(Value::Null), "chartPosition": i + 1
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

// Direct audio URL for a page URL (cached for a few hours).
// The page (YouTube watch URL...) a track plays from. Tracks that only have a name are looked up once and remembered.
pub async fn page_url_for(ctx: &Arc<Ctx>, id: &str, url: &str, query: &str) -> Result<String, String> {
    if !url.is_empty() && !id.starts_with("dz-") {
        return Ok(url.to_string());
    }
    let q = query.trim();
    if q.is_empty() {
        return Err("could not find the track on YouTube".into());
    }
    let key = format!("search:{}", q.to_lowercase());
    if let Some((t, u)) = ctx.stream_cache.lock().unwrap().get(&key) {
        if t.elapsed() < STREAM_TTL {
            return Ok(u.clone());
        }
    }
    let found = ytm_search(ctx, q).await.unwrap_or_default();
    let first = found.first().ok_or("could not find the track on YouTube")?;
    let page = text(first.get("url")).to_string();
    ctx.stream_cache.lock().unwrap().insert(key, (Instant::now(), page.clone()));
    Ok(page)
}

fn cached_stream(ctx: &Ctx, page_url: &str) -> Option<String> {
    let cache = ctx.stream_cache.lock().unwrap();
    cache.get(page_url).filter(|(t, _)| t.elapsed() < STREAM_TTL).map(|(_, u)| u.clone())
}

// Resolves several YouTube pages with ONE yt-dlp start (starting the tool costs more than a page).
async fn resolve_batch(ctx: &Arc<Ctx>, pages: Vec<String>) {
    let todo: Vec<String> = {
        let mut busy = ctx.resolving.lock().unwrap();
        pages.into_iter().filter(|p| cached_stream(ctx, p).is_none() && busy.insert(p.clone())).collect()
    };
    if todo.is_empty() {
        return;
    }
    let (ctx2, urls) = (ctx.clone(), todo.clone());
    let out = tauri::async_runtime::spawn_blocking(move || -> String {
        let mut cmd = tools::command(tools::tool_path(&ctx2, "yt-dlp"));
        cmd.args(["-q", "--no-warnings", "--no-playlist", "--ignore-errors", "-f", "bestaudio[ext=m4a]/bestaudio", "--print", "%(id)s|%(url)s"]);
        cmd.args(&urls);
        // One broken page makes the tool exit with an error although the others were printed: keep what it printed.
        cmd.output().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    {
        let mut cache = ctx.stream_cache.lock().unwrap();
        for line in out.lines() {
            let Some((id, direct)) = line.trim().split_once('|') else { continue };
            if !direct.starts_with("http") {
                continue;
            }
            if let Some(page) = todo.iter().find(|p| crate::cache::key_for(p) == id) {
                cache.insert(page.clone(), (Instant::now(), direct.to_string()));
            }
        }
    }
    let mut busy = ctx.resolving.lock().unwrap();
    for p in &todo {
        busy.remove(p);
    }
}

// Gets streams ready before the listener presses play (search results, the next tracks, a hovered row).
pub async fn prefetch(ctx: Arc<Ctx>, tracks: Vec<Value>) {
    let mut pages: Vec<String> = Vec::new();
    for t in tracks.iter().take(12) {
        let id = text(t.get("id"));
        if !id.is_empty() && crate::cache::find(&ctx, id).is_some() {
            continue;
        }
        let query = format!("{} {}", text(t.get("artist")), text(t.get("title")));
        let Ok(page) = page_url_for(&ctx, id, text(t.get("url")), &query).await else { continue };
        if crate::cache::find(&ctx, &crate::cache::key_for(&page)).is_some() || cached_stream(&ctx, &page).is_some() {
            continue;
        }
        if page.contains("youtube.com/watch") {
            if !pages.contains(&page) {
                pages.push(page);
            }
        } else {
            let _ = resolve_direct(&ctx, &page).await;
        }
    }
    for chunk in pages.chunks(4) {
        resolve_batch(&ctx, chunk.to_vec()).await;
    }
}

pub async fn resolve_direct(ctx: &Arc<Ctx>, page_url: &str) -> Result<String, String> {
    if let Some(u) = cached_stream(ctx, page_url) {
        return Ok(u);
    }
    // A background preparation of this page may already be running: wait for it instead of starting the tool twice.
    for _ in 0..80 {
        if !ctx.resolving.lock().unwrap().contains(page_url) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if let Some(u) = cached_stream(ctx, page_url) {
        return Ok(u);
    }
    let ctx2 = ctx.clone();
    let url = page_url.to_string();
    let direct = tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let mut cmd = tools::command(tools::tool_path(&ctx2, "yt-dlp"));
        cmd.args(["-q", "--no-warnings", "--no-playlist", "-f", "bestaudio[ext=m4a]/bestaudio", "-g"]).arg(&url);
        let out = tools::run_cmd(cmd)?;
        out.lines()
            .map(str::trim)
            .find(|l| l.starts_with("http"))
            .map(String::from)
            .ok_or_else(|| "yt-dlp returned no stream URL".to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    ctx.stream_cache.lock().unwrap().insert(page_url.to_string(), (Instant::now(), direct.clone()));
    Ok(direct)
}

pub async fn stream_info(ctx: &Arc<Ctx>, q: &std::collections::HashMap<String, String>) -> Result<Value, String> {
    let get = |k: &str| q.get(k).cloned().unwrap_or_default();
    let mut url = get("url");
    let for_share = get("forShare") == "1";
    if !for_share && get("forceYtDlp") != "1" {
        let id = get("id");
        let cached = if id.is_empty() { None } else { crate::cache::find(ctx, &id) }
            .or_else(|| if url.is_empty() { None } else { crate::cache::find(ctx, &crate::cache::key_for(&url)) });
        if let Some((key, _)) = cached {
            let local = format!("http://127.0.0.1:{}/api/local-track?id={}", ctx.port, urlencode(&key));
            return Ok(json!({ "success": true, "isLocal": true, "streamUrl": local, "rawStreamUrl": local, "proxyUrl": local, "isHls": false }));
        }
    }
    if url.is_empty() || get("id").starts_with("dz-") {
        let query = if !get("q").is_empty() { get("q") } else { format!("{} {}", get("artist"), get("title")) };
        url = page_url_for(ctx, &get("id"), "", &query).await?;
        // The track may already be saved for offline under its YouTube id.
        if !for_share && get("forceYtDlp") != "1" {
            if let Some((key, _)) = crate::cache::find(ctx, &crate::cache::key_for(&url)) {
                let local = format!("http://127.0.0.1:{}/api/local-track?id={}", ctx.port, urlencode(&key));
                return Ok(json!({ "success": true, "isLocal": true, "streamUrl": local, "rawStreamUrl": local, "proxyUrl": local, "isHls": false }));
            }
        }
    }
    let raw = resolve_direct(ctx, &url).await?;
    let is_hls = raw.contains(".m3u8");
    let proxied = format!("http://127.0.0.1:{}/api/audio-proxy?url={}", ctx.port, urlencode(&raw));
    if for_share {
        return Ok(json!({ "success": true, "streamUrl": raw, "shareWebUrl": url }));
    }
    // Nothing is saved here: tracks go to the offline library only when they are liked (see offline_save).
    Ok(json!({
        "success": true, "isLocal": false,
        "streamUrl": if is_hls { raw.clone() } else { proxied.clone() },
        "rawStreamUrl": raw, "proxyUrl": proxied, "isHls": is_hls
    }))
}

pub fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn custom_lyrics_path(ctx: &Ctx, title: &str, artist: &str) -> std::path::PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{}\u{1}{}", title.trim().to_lowercase(), artist.trim().to_lowercase()).hash(&mut h);
    let dir = ctx.data_dir.join("lyrics-custom");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("{:016x}.lrc", h.finish()))
}

/// "Song (feat. X) - 2011 Remaster" -> "song": what two spellings of a title have in common.
fn base_title(t: &str) -> String {
    let mut out = String::new();
    let mut depth = 0i32;
    for c in t.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let low = out.to_lowercase();
    let cut = [" - ", " – ", " — "].iter().filter_map(|d| low.find(d)).min().unwrap_or(low.len());
    low[..cut].chars().filter(|c| c.is_alphanumeric()).collect()
}

/// The LRCLIB entry that is really this recording: same title, same artist and (above all) the same length, so the timing fits.
/// Returns the text, whether it is timed and how many seconds its length differs from the track's.
async fn lrclib_best(ctx: &Ctx, title: &str, artist: &str, dur: f64) -> Option<(String, bool, f64)> {
    let ua = "Jeem";
    let mut items: Vec<Value> = Vec::new();
    let get_url = format!(
        "https://lrclib.net/api/get?track_name={}&artist_name={}{}",
        urlencode(title),
        urlencode(artist),
        if dur > 0.0 { format!("&duration={}", dur.round()) } else { String::new() }
    );
    let search_url = format!("https://lrclib.net/api/search?track_name={}&artist_name={}", urlencode(title), urlencode(artist));
    let loose_url = format!("https://lrclib.net/api/search?q={}", urlencode(&format!("{artist} {title}")));
    let (g, sr, lo) = tokio::join!(
        async { ctx.client.get(get_url).header("User-Agent", ua).timeout(Duration::from_secs(8)).send().await.ok()?.json::<Value>().await.ok() },
        async { ctx.client.get(search_url).header("User-Agent", ua).timeout(Duration::from_secs(8)).send().await.ok()?.json::<Value>().await.ok() },
        async { ctx.client.get(loose_url).header("User-Agent", ua).timeout(Duration::from_secs(8)).send().await.ok()?.json::<Value>().await.ok() }
    );
    if let Some(v) = g.filter(|v| v.get("id").is_some()) {
        items.push(v);
    }
    for list in [sr, lo].into_iter().flatten() {
        if let Value::Array(a) = list {
            items.extend(a);
        }
    }
    let want_title = base_title(title);
    let want_artists: Vec<String> = artist
        .split([',', '&', ';', '/'])
        .map(|a| a.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect::<String>())
        .filter(|a| !a.is_empty())
        .collect();
    let mut best: Option<(f64, &Value, f64)> = None;
    for it in &items {
        if it.get("instrumental").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        let synced = !text(it.get("syncedLyrics")).is_empty();
        let plain = !text(it.get("plainLyrics")).is_empty();
        if !synced && !plain {
            continue;
        }
        let got = base_title(text(it.get("trackName")));
        let mut score = if got == want_title {
            4.0
        } else if !got.is_empty() && !want_title.is_empty() && (got.contains(&want_title) || want_title.contains(&got)) {
            2.0
        } else {
            -6.0
        };
        let who: String = text(it.get("artistName")).to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
        score += if want_artists.is_empty() || want_artists.iter().any(|a| who.contains(a.as_str()) || a.contains(who.as_str())) { 2.0 } else { -2.0 };
        if synced {
            score += 2.0;
        }
        let their = it.get("duration").and_then(Value::as_f64).unwrap_or(0.0);
        let diff = if dur > 0.0 && their > 0.0 { (their - dur).abs() } else { 0.0 };
        if dur > 0.0 && their > 0.0 {
            score += if diff <= 1.0 { 5.0 } else if diff <= 2.0 { 4.0 } else if diff <= 4.0 { 2.0 } else if diff <= 8.0 { -1.0 } else { -4.0 };
        }
        if best.as_ref().is_none_or(|b| score > b.0) {
            best = Some((score, it, diff));
        }
    }
    let (score, it, diff) = best?;
    if score < 1.0 {
        return None;
    }
    let synced = text(it.get("syncedLyrics"));
    if !synced.is_empty() {
        Some((synced.to_string(), true, diff))
    } else {
        Some((text(it.get("plainLyrics")).to_string(), false, diff))
    }
}

/// Lyrics for a track. The sources are tried in turn until one has them: LRCLIB (the recording with the same length), NetEase, then
/// plain text from LRCLIB or Genius. `approx` is set when the timing may be a little off.
pub async fn lyrics(ctx: &Ctx, title: &str, artist: &str, duration: &str) -> Value {
    let empty = json!({ "success": true, "lyrics": "", "isCustom": false, "isExplicit": false });
    if title.trim().is_empty() {
        return empty;
    }
    if let Ok(custom) = std::fs::read_to_string(custom_lyrics_path(ctx, title, artist)) {
        return json!({ "success": true, "lyrics": custom, "isCustom": true, "isExplicit": false });
    }
    let dur = duration.parse::<f64>().unwrap_or(0.0);
    let is_timed = |t: &str| t.lines().filter(|l| l.trim_start().starts_with('[') && l.contains(':')).count() >= 4;
    let mut found = String::new();
    let mut approx = false;
    if let Some((text, timed, diff)) = lrclib_best(ctx, title, artist, dur).await {
        approx = timed && dur > 0.0 && diff > 3.5;
        found = text;
    }
    // LRCLIB has no timed text, or only for a recording of another length: the second source is asked.
    if !is_timed(&found) || approx {
        if let Some(lrc) = crate::extras::netease_lrc(ctx, title, artist, dur).await {
            found = lrc;
            approx = false;
        }
    }
    // Still nothing: plain text from Genius, so the track at least shows its words.
    if found.trim().is_empty() {
        if let Some(text) = crate::extras::genius_plain(ctx, title, artist).await {
            found = text;
        }
    }
    json!({ "success": true, "lyrics": found, "isCustom": false, "isExplicit": false, "approx": approx })
}

fn dz_track(it: &Value, i: usize) -> Value {
    let cover = ["cover_xl", "cover_big", "cover_medium"]
        .iter()
        .map(|k| text(it.pointer(&format!("/album/{k}"))))
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string();
    json!({
        "id": format!("dz-{}", it.get("id").and_then(Value::as_u64).unwrap_or(0)),
        "title": text(it.get("title")), "artist": text(it.pointer("/artist/name")),
        "thumbnail": cover, "cover": cover,
        "duration": it.get("duration").and_then(Value::as_u64).unwrap_or(0),
        "platform": "ytmusic", "url": "", "rank": i
    })
}

// "66.1M" for "Artist • 66.1M monthly audience" on YouTube Music; None when the artist is not found there.
pub async fn ytm_monthly_audience(ctx: &Ctx, name: &str) -> Option<String> {
    let body = json!({
        "context": { "client": { "clientName": "WEB_REMIX", "clientVersion": "1.20250101.01.00", "hl": "en", "gl": "US" } },
        "query": name,
        "params": "EgWKAQIgAWoMEA4QChADEAQQCRAF"
    });
    let root: Value = ctx
        .client
        .post("https://music.youtube.com/youtubei/v1/search?prettyPrint=false")
        .header("Origin", "https://music.youtube.com")
        .header("Referer", "https://music.youtube.com/")
        .header("User-Agent", UA)
        .timeout(Duration::from_secs(8))
        .json(&body)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let want: String = name.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
    let sections = root
        .pointer("/contents/tabbedSearchResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents")
        .and_then(Value::as_array)?;
    for section in sections {
        for wrap in section.pointer("/musicShelfRenderer/contents").and_then(Value::as_array).into_iter().flatten() {
            let Some(cols) = wrap.pointer("/musicResponsiveListItemRenderer/flexColumns").and_then(Value::as_array) else { continue };
            let col = |i: usize| runs_text(cols.get(i).and_then(|c| c.pointer("/musicResponsiveListItemFlexColumnRenderer/text/runs"))).concat();
            let got: String = col(0).to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
            if got != want {
                continue;
            }
            let sub = col(1);
            let part = sub.split(" • ").find(|p| p.contains("monthly audience"))?;
            return Some(part.replace("monthly audience", "").trim().to_string());
        }
    }
    None
}

pub async fn artist_info(ctx: &Ctx, name: &str) -> Value {
    let clean = name.replace(" - Topic", "").trim().to_string();
    let mut artist = json!({
        "name": clean, "bio": "", "avatar": "", "banner": "", "albums": [],
        "isVerified": false, "isKnownArtist": false, "fans": 0, "monthlyListeners": "", "tracks": []
    });
    if clean.is_empty() {
        return artist;
    }
    let search = format!("https://api.deezer.com/search/artist?q={}&limit=1", urlencode(&clean));
    let Ok(r) = ctx.client.get(search).timeout(Duration::from_secs(6)).send().await else { return artist };
    let Ok(root) = r.json::<Value>().await else { return artist };
    let Some(a) = root.pointer("/data/0") else { return artist };
    let id = a.get("id").and_then(Value::as_u64).unwrap_or(0);
    artist["name"] = json!(text(a.get("name")));
    artist["avatar"] = json!(text(a.get("picture_xl")));
    artist["banner"] = json!(text(a.get("picture_xl")));
    artist["fans"] = json!(a.get("nb_fan").and_then(Value::as_u64).unwrap_or(0));
    artist["isKnownArtist"] = json!(true);
    artist["deezerId"] = json!(id);
    let top = format!("https://api.deezer.com/artist/{id}/top?limit=50");
    let display_name = text(a.get("name")).to_string();
    let (top_res, releases, audience) = tokio::join!(
        async { ctx.client.get(top).timeout(Duration::from_secs(8)).send().await.ok()?.json::<Value>().await.ok() },
        dz_releases(ctx, id, &display_name),
        ytm_monthly_audience(ctx, &display_name)
    );
    if let Some(a) = audience {
        artist["monthlyListeners"] = json!(format!("{a} monthly listeners"));
        artist["monthlyAudience"] = json!(a);
    }
    if let Some(items) = top_res.as_ref().and_then(|v| v.get("data")).and_then(Value::as_array) {
        artist["tracks"] = Value::Array(items.iter().enumerate().map(|(i, it)| dz_track(it, i)).collect());
    }
    artist["albums"] = Value::Array(releases);
    artist
}

async fn dz_releases(ctx: &Ctx, id: u64, artist_name: &str) -> Vec<Value> {
    let url = format!("https://api.deezer.com/artist/{id}/albums?limit=100");
    let Some(v) = async { ctx.client.get(url).timeout(Duration::from_secs(8)).send().await.ok()?.json::<Value>().await.ok() }.await else {
        return Vec::new();
    };
    let mut list: Vec<(String, Value)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for it in v.get("data").and_then(Value::as_array).cloned().unwrap_or_default() {
        let title = text(it.get("title")).to_string();
        let kind = text(it.get("record_type")).to_string();
        if title.is_empty() || !seen.insert(format!("{}:{}", title.to_lowercase(), kind)) {
            continue;
        }
        let date = text(it.get("release_date")).to_string();
        let cover = ["cover_xl", "cover_big", "cover_medium"].iter().map(|k| text(it.get(*k))).find(|s| !s.is_empty()).unwrap_or("").to_string();
        list.push((date.clone(), json!({
            "id": it.get("id").and_then(Value::as_u64).map(|n| n.to_string()).unwrap_or_default(),
            "title": title, "artist": artist_name, "cover": cover,
            "year": date.chars().take(4).collect::<String>(), "releaseDate": date, "recordType": kind
        })));
    }
    list.sort_by(|a, b| b.0.cmp(&a.0));
    list.into_iter().map(|(_, v)| v).collect()
}

/// Newest releases of the given artists (up to three per artist), newest first. The caller keeps only the fresh ones.
pub async fn new_releases(ctx: &Ctx, artists: &[String]) -> Vec<Value> {
    let mut handles = Vec::new();
    for name in artists.iter().take(14) {
        let client = ctx.client.clone();
        let name = name.replace(" - Topic", "").trim().to_string();
        if name.is_empty() {
            continue;
        }
        handles.push(tauri::async_runtime::spawn(async move {
            let want: String = name.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
            let search = format!("https://api.deezer.com/search/artist?q={}&limit=5", urlencode(&name));
            let root: Value = client.get(search).timeout(Duration::from_secs(8)).send().await.ok()?.json().await.ok()?;
            let hit = root.get("data").and_then(Value::as_array)?.iter().find(|a| {
                let got: String = text(a.get("name")).to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
                got == want
            })?;
            let id = hit.get("id").and_then(Value::as_u64)?;
            let display = text(hit.get("name")).to_string();
            let url = format!("https://api.deezer.com/artist/{id}/albums?limit=25");
            let v: Value = client.get(url).timeout(Duration::from_secs(8)).send().await.ok()?.json().await.ok()?;
            let mut out: Vec<Value> = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for it in v.get("data").and_then(Value::as_array).cloned().unwrap_or_default() {
                let title = text(it.get("title")).to_string();
                let date = text(it.get("release_date")).to_string();
                let key: String = title.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
                if title.is_empty() || date.is_empty() || !seen.insert(key) {
                    continue;
                }
                let cover = ["cover_xl", "cover_big", "cover_medium"].iter().map(|k| text(it.get(*k))).find(|s| !s.is_empty()).unwrap_or("").to_string();
                out.push(json!({
                    "id": it.get("id").and_then(Value::as_u64).map(|n| n.to_string()).unwrap_or_default(),
                    "title": title, "artist": display, "cover": cover,
                    "year": date.chars().take(4).collect::<String>(), "releaseDate": date,
                    "recordType": text(it.get("record_type")), "source": "deezer"
                }));
            }
            out.sort_by(|a, b| text(b.get("releaseDate")).cmp(text(a.get("releaseDate"))));
            out.truncate(3);
            Some(out)
        }));
    }
    let mut all: Vec<Value> = Vec::new();
    for h in handles {
        if let Ok(Some(list)) = h.await {
            all.extend(list);
        }
    }
    all.sort_by(|a, b| text(b.get("releaseDate")).cmp(text(a.get("releaseDate"))));
    all
}

/// Every track from the artist's releases, newest release first.
pub async fn artist_all_tracks(ctx: &Ctx, id: u64, artist_name: &str) -> Vec<Value> {
    if id == 0 {
        return Vec::new();
    }
    let releases: Vec<Value> = dz_releases(ctx, id, artist_name).await.into_iter().take(40).collect();
    let mut handles = Vec::new();
    for rel in &releases {
        let client = ctx.client.clone();
        let rid = text(rel.get("id")).to_string();
        handles.push(tauri::async_runtime::spawn(async move {
            let url = format!("https://api.deezer.com/album/{rid}/tracks?limit=100");
            client.get(url).timeout(Duration::from_secs(10)).send().await.ok()?.json::<Value>().await.ok()
        }));
    }
    let mut out: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (rel, h) in releases.iter().zip(handles) {
        let Ok(Some(v)) = h.await else { continue };
        for it in v.get("data").and_then(Value::as_array).cloned().unwrap_or_default() {
            let title = text(it.get("title")).to_string();
            let key: String = title.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
            if title.is_empty() || !seen.insert(key) {
                continue;
            }
            let who = text(it.pointer("/artist/name"));
            out.push(json!({
                "id": format!("dz-{}", it.get("id").and_then(Value::as_u64).unwrap_or(0)),
                "title": title, "artist": if who.is_empty() { artist_name } else { who },
                "thumbnail": text(rel.get("cover")), "cover": text(rel.get("cover")),
                "album": text(rel.get("title")), "releaseDate": text(rel.get("releaseDate")),
                "duration": it.get("duration").and_then(Value::as_u64).unwrap_or(0),
                "platform": "ytmusic", "url": ""
            }));
        }
    }
    out
}

// ---- "Top moment": the most replayed part of a track, from YouTube's own "Most replayed" data ----

fn moments_path(ctx: &Ctx) -> std::path::PathBuf {
    ctx.data_dir.join("top-moments.json")
}

fn moments_load(ctx: &Ctx) -> serde_json::Map<String, Value> {
    std::fs::read_to_string(moments_path(ctx))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// Returns `{ time, views }`: `time` is null when YouTube has no replay data for the video.
pub async fn top_moment(ctx: &Arc<Ctx>, id: &str, url: &str, query: &str) -> Value {
    let Ok(page) = page_url_for(ctx, id, url, query).await else {
        return json!({ "success": false });
    };
    if !page.contains("youtube.com/watch") {
        return json!({ "success": true, "time": null });
    }
    let key = crate::cache::key_for(&page);
    if let Some(saved) = moments_load(ctx).get(&key) {
        return json!({ "success": true, "time": saved.get("time").cloned().unwrap_or(Value::Null), "views": saved.get("views").cloned().unwrap_or(Value::Null) });
    }
    let ctx2 = ctx.clone();
    let info = tauri::async_runtime::spawn_blocking(move || -> Option<Value> {
        let mut cmd = tools::command(tools::tool_path(&ctx2, "yt-dlp"));
        cmd.args(["-J", "--no-warnings", "--no-playlist", "--skip-download"]).arg(&page);
        serde_json::from_str::<Value>(&tools::run_cmd(cmd).ok()?).ok()
    })
    .await
    .ok()
    .flatten();
    let Some(info) = info else { return json!({ "success": false }) };
    let duration = info.get("duration").and_then(Value::as_f64).unwrap_or(0.0);
    // The first seconds are always "most replayed" (everyone starts there): look past the intro.
    let mut best: Option<(f64, f64)> = None;
    for seg in info.get("heatmap").and_then(Value::as_array).into_iter().flatten() {
        let start = seg.get("start_time").and_then(Value::as_f64).unwrap_or(0.0);
        let value = seg.get("value").and_then(Value::as_f64).unwrap_or(0.0);
        if duration > 0.0 && (start < duration * 0.08 || start > duration * 0.95) {
            continue;
        }
        if best.map_or(true, |(_, v)| value > v) {
            best = Some((start, value));
        }
    }
    let time = best.filter(|(_, v)| *v >= 0.15).map(|(t, _)| json!(t)).unwrap_or(Value::Null);
    let views = info.get("view_count").cloned().unwrap_or(Value::Null);
    let mut all = moments_load(ctx);
    if all.len() > 3000 {
        all.clear();
    }
    all.insert(key, json!({ "time": time, "views": views }));
    let _ = std::fs::write(moments_path(ctx), Value::Object(all).to_string());
    json!({ "success": true, "time": time, "views": views })
}
