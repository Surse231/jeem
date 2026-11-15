// Local HTTP server: serves the Jeem interface, its /api routes and the /__ipc bridge (replaces Electron IPC).
use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use axum::routing::any;
use serde_json::{Value, json};
use tauri::Manager;

use crate::{Ctx, addons, cache, discord, google, gsync, library, social, spotify, shell, catalog, download, extras, import, lastfm, local, tools, ytm};

const CHUNK: u64 = 2 * 1_048_576;
const INDEX: &str = "/src/renderer/index.html";

pub async fn serve(ctx: Arc<Ctx>, listener: std::net::TcpListener) {
    let _ = listener.set_nonblocking(true);
    let Ok(listener) = tokio::net::TcpListener::from_std(listener) else { return };
    let app = Router::new().fallback(any(handle)).with_state(ctx);
    let _ = axum::serve(listener, app).await;
}

fn json_response(status: u16, v: Value) -> Response {
    Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(v.to_string()))
        .unwrap()
}

fn ok(v: Value) -> Response {
    json_response(200, v)
}

fn reply((status, v): (u16, Value)) -> Response {
    json_response(status, v)
}

fn fail(status: u16, msg: &str) -> Response {
    json_response(status, json!({ "success": false, "error": msg }))
}

fn with_cors(mut resp: Response) -> Response {
    let h = resp.headers_mut();
    h.insert("Access-Control-Allow-Origin", HeaderValue::from_static("*"));
    h.insert("Access-Control-Allow-Headers", HeaderValue::from_static("*"));
    h.insert("Access-Control-Allow-Methods", HeaderValue::from_static("GET, POST, OPTIONS"));
    h.insert("Access-Control-Expose-Headers", HeaderValue::from_static("Content-Range, Content-Length, Accept-Ranges"));
    resp
}

fn parse_query(q: &str) -> HashMap<String, String> {
    reqwest::Url::parse(&format!("http://x/?{q}"))
        .map(|u| u.query_pairs().map(|(k, v)| (k.to_string(), v.to_string())).collect())
        .unwrap_or_default()
}

async fn handle(State(ctx): State<Arc<Ctx>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    if parts.method == Method::OPTIONS {
        return with_cors(Response::builder().status(204).body(Body::empty()).unwrap());
    }
    let path = parts.uri.path().to_string();
    let query = parse_query(parts.uri.query().unwrap_or(""));
    let resp = if path == "/__file" {
        local::serve_file(&ctx, &query, &parts.headers).await
    } else if let Some(cmd) = path.strip_prefix("/__ipc/") {
        // the whole saved state of the app travels in one request, so the limit is generous (and a refusal is never silent)
        let bytes = match axum::body::to_bytes(body, 1024 * 1024 * 1024).await {
            Ok(b) => b,
            Err(e) => {
                log_line(&ctx, &format!("request to {cmd} refused: {e}"));
                return with_cors(fail(413, "request too large"));
            }
        };
        ipc(&ctx, &parts, bytes, cmd).await
    } else if path.starts_with("/api/") {
        let bytes = axum::body::to_bytes(body, 64 * 1024 * 1024).await.unwrap_or_default();
        api(&ctx, &parts, &query, bytes, &path).await
    } else {
        static_file(&ctx, &path)
    };
    with_cors(resp)
}

fn static_file(ctx: &Ctx, path: &str) -> Response {
    let path = if path == "/" { INDEX } else { path };
    let resolver = ctx.app.asset_resolver();
    let asset = resolver.get(path.to_string()).or_else(|| resolver.get(path.trim_start_matches('/').to_string()));
    let Some(asset) = asset else {
        return Response::builder().status(404).body(Body::from("not found")).unwrap();
    };
    let mut bytes = asset.bytes;
    if asset.mime_type.starts_with("text/html") {
        let html = String::from_utf8_lossy(&bytes).to_string();
        let inject = format!(
            "<head><script>window.__JEEM__={{\"token\":\"{}\"}}</script><script src=\"/src/renderer/jeem-shim.js\"></script>",
            ctx.token
        );
        bytes = html.replacen("<head>", &inject, 1).into_bytes();
    }
    Response::builder()
        .header(header::CONTENT_TYPE, asset.mime_type)
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(bytes))
        .unwrap()
}

async fn api(ctx: &Arc<Ctx>, parts: &Parts, q: &HashMap<String, String>, body: Bytes, path: &str) -> Response {
    let get = |k: &str| q.get(k).cloned().unwrap_or_default();
    let num = |k: &str, d: usize| q.get(k).and_then(|v| v.parse::<usize>().ok()).unwrap_or(d);
    match path {
        "/api/net-check" => ok(json!({ "online": true })),
        "/api/tools/status" | "/api/tools-status" => ok(tools::status(ctx)),
        "/api/tools/install" | "/api/tools-install" => {
            tauri::async_runtime::spawn(tools::install(ctx.clone()));
            ok(json!({ "started": true }))
        }
        "/api/search" => {
            let platform = if get("platform").is_empty() { "ytmusic".to_string() } else { get("platform") };
            ok(ytm::search(ctx, &get("q"), &platform, num("limit", 12).max(1), num("offset", 0)).await)
        }
        "/api/trending" => {
            let (chart, mut tracks) = ytm::yt_chart(ctx, &get("region")).await;
            if tracks.is_empty() {
                tracks = ytm::deezer_chart(ctx, 40).await;
            }
            tracks.truncate(35);
            ok(json!({ "success": true, "tracks": tracks, "chart": chart }))
        }
        "/api/hot" => {
            let limit = num("limit", 12).clamp(1, 25);
            let mut tracks = ytm::deezer_chart(ctx, 40).await;
            tracks.truncate(limit);
            ok(json!({ "success": true, "title": "Hot right now", "tracks": tracks }))
        }
        "/api/stream-info" => match ytm::stream_info(ctx, q).await {
            Ok(v) => ok(v),
            Err(e) => fail(500, &e),
        },
        "/api/audio-proxy" => audio_proxy(ctx, parts, &get("url")).await,
        "/api/image-proxy" => image_proxy(ctx, &get("url")).await,
        "/api/lyrics" => ok(ytm::lyrics(ctx, &get("title"), &get("artist"), &get("duration")).await),
        "/api/artist" => ok(json!({ "success": true, "artist": ytm::artist_info(ctx, &get("name")).await })),
        "/api/artist-tracks" => {
            let id = get("id").parse::<u64>().unwrap_or(0);
            ok(json!({ "success": true, "tracks": ytm::artist_all_tracks(ctx, id, &get("name")).await }))
        }
        "/api/new-releases" => {
            let names: Vec<String> = get("artists").split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
            ok(json!({ "success": true, "releases": ytm::new_releases(ctx, &names).await }))
        }
        "/api/ai-test" => {
            let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            ok(crate::ai::test(ctx, &b).await)
        }
        "/api/vibe" => {
            let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let limit = b.get("limit").and_then(Value::as_u64).map(|n| n as usize).unwrap_or_else(|| num("limit", 12)).clamp(4, 40);
            ok(json!({ "success": true, "tracks": crate::vibe::recommend(ctx, &b, limit).await }))
        }
        "/api/download" => ok(download::download(ctx, q).await),
        "/api/encode" => ok(download::encode(ctx, q, body.to_vec()).await),
        "/api/playlist-import" => reply(import::playlist_import(ctx, &get("url")).await),
        "/api/spotify-import" => reply(import::spotify_import(ctx, &get("url")).await),
        "/api/apple-import" => reply(import::apple_import(ctx, &get("url")).await),
        "/api/yandex-import" => reply(import::yandex_import(ctx, &get("url")).await),
        "/api/vk-import" => {
            let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let field = |k: &str| b.get(k).and_then(Value::as_str).map(String::from).unwrap_or_else(|| get(k));
            reply(social::vk_import(ctx, &field("url"), &field("text"), &field("name")).await)
        }
        "/api/tiktok-import" => {
            let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let url = b.get("url").and_then(Value::as_str).map(String::from).unwrap_or_else(|| get("url"));
            reply(social::tiktok_import(ctx, &url).await)
        }
        "/api/track-analysis" => {
            if parts.method == Method::POST {
                let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                if let (Some(id), Some(a)) = (b.get("id").and_then(Value::as_str), b.get("analysis")) {
                    cache::analysis_set(ctx, id, a.clone());
                }
                ok(json!({ "success": true }))
            } else {
                ok(json!({ "success": true, "analysis": cache::analysis_get(ctx, &get("id")) }))
            }
        }
        "/api/gsync-callback" => {
            let (status, html) = gsync::callback(ctx, q).await;
            Response::builder().status(status).header(header::CONTENT_TYPE, "text/html; charset=utf-8").body(Body::from(html)).unwrap()
        }
        "/api/google-callback" => {
            let (status, html) = google::callback(ctx, q).await;
            Response::builder().status(status).header(header::CONTENT_TYPE, "text/html; charset=utf-8").body(Body::from(html)).unwrap()
        }
        p if p.starts_with("/api/lastfm/") => {
            let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            ok(lastfm::route(ctx, p, &b).await)
        }
        "/api/albums/search" => reply(catalog::album_search(ctx, &get("q")).await),
        "/api/playlists/search" => reply(catalog::playlist_search(ctx, &get("q")).await),
        "/api/albums/tracks" | "/api/album" | "/api/album-tracks" => {
            reply(catalog::album_tracks(ctx, &get("id"), &get("title"), &get("artist")).await)
        }
        "/api/track-credits" => ok(extras::credits(ctx, &get("artist"), &get("title")).await),
        "/api/lyrics-words" => ok(extras::words(ctx, &get("title"), &get("artist"), get("duration").parse::<f64>().unwrap_or(0.0)).await),
        "/api/save-custom-lyrics" => {
            let lyrics = serde_json::from_slice::<Value>(&body).ok().and_then(|v| v.get("lyrics").and_then(Value::as_str).map(String::from));
            match lyrics {
                Some(l) => match std::fs::write(ytm::custom_lyrics_path(ctx, &get("title"), &get("artist")), l) {
                    Ok(()) => ok(json!({ "success": true })),
                    Err(e) => fail(500, &e.to_string()),
                },
                None => fail(400, "No lyrics provided"),
            }
        }
        "/api/devsize-memes" => {
            let list = ctx
                .app
                .asset_resolver()
                .get("/assets/devsize/index.json".to_string())
                .and_then(|a| serde_json::from_slice::<Vec<String>>(&a.bytes).ok())
                .unwrap_or_default();
            let files: Vec<String> = list.iter().map(|f| format!("/assets/devsize/{}", ytm::urlencode(f))).collect();
            ok(json!({ "success": true, "files": files }))
        }
        "/api/library" => ok(library::list(ctx)),
        "/api/library-add" => ok(library::add(ctx, &serde_json::from_slice(&body).unwrap_or(Value::Null))),
        "/api/library-update" => ok(library::update(ctx, &serde_json::from_slice(&body).unwrap_or(Value::Null))),
        "/api/library-remove" => ok(library::remove(ctx, &serde_json::from_slice(&body).unwrap_or(Value::Null), &get("id"))),
        "/api/library-file" => match library::file_path(ctx, &get("id")) {
            Some(path) => {
                let mut q2 = HashMap::new();
                q2.insert("t".to_string(), ctx.token.clone());
                q2.insert("p".to_string(), path.to_string_lossy().to_string());
                local::serve_file(ctx, &q2, &parts.headers).await
            }
            None => fail(404, "not found"),
        },
        "/api/track-cover" => ok(extras::track_cover(ctx, &get("artist"), &get("title")).await),
        "/api/prefetch" => {
            let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let tracks = b.get("tracks").and_then(Value::as_array).cloned().unwrap_or_default();
            if !tracks.is_empty() {
                tauri::async_runtime::spawn(ytm::prefetch(ctx.clone(), tracks));
            }
            ok(json!({ "success": true }))
        }
        "/api/offline-save" => ok(offline_save(ctx, q).await),
        "/api/top-moment" => {
            let query = format!("{} {}", get("artist"), get("title"));
            ok(ytm::top_moment(ctx, &get("id"), &get("url"), &query).await)
        }
        "/api/check-cache" => match crate::cache::find(ctx, &get("id")) {
            Some((key, _)) => ok(json!({
                "cached": true,
                "streamUrl": format!("http://127.0.0.1:{}/api/local-track?id={}", ctx.port, ytm::urlencode(&key))
            })),
            None => ok(json!({ "cached": false, "streamUrl": null })),
        },
        "/api/local-track" => match crate::cache::find(ctx, &get("id")) {
            Some((_, path)) => {
                let mut q2 = HashMap::new();
                q2.insert("t".to_string(), ctx.token.clone());
                q2.insert("p".to_string(), path.to_string_lossy().to_string());
                local::serve_file(ctx, &q2, &parts.headers).await
            }
            None => fail(404, "not cached"),
        },
        "/api/offline-library" => ok(crate::cache::library(ctx, get("podcasts") == "1")),
        "/api/podcast-search" => {
            if get("type") == "episodes" {
                ok(json!({ "success": true, "episodes": ytm::podcast_episode_search(ctx, &get("q")).await }))
            } else {
                ok(json!({ "success": true, "shows": ytm::podcast_shows(ctx, &get("q"), num("limit", 18).clamp(1, 40)).await }))
            }
        }
        "/api/podcast-show" => ok(ytm::podcast_show(ctx, &get("id"), num("limit", 60).clamp(1, 200)).await),
        "/api/podcast-download" => ok(podcast_download(ctx, q).await),
        "/api/podcast-status" => {
            let id = get("id");
            let key = crate::cache::key_for(&format!("https://www.youtube.com/watch?v={id}"));
            ok(json!({
                "downloaded": crate::cache::find(ctx, &key).is_some(),
                "downloading": crate::cache::is_caching(ctx, &key)
            }))
        }
        "/api/offline-delete" => ok(json!({ "success": crate::cache::delete(ctx, &get("id")) })),
        "/api/clear-offline-library" => {
            crate::cache::clear(ctx);
            ok(json!({ "success": true }))
        }
        "/api/state" => ok(json!({ "success": true })),
        "/api/default-settings" => {
            let defaults = std::fs::read_to_string(ctx.data_dir.join("default-settings.json"))
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .unwrap_or_else(|| json!({}));
            ok(json!({ "success": true, "defaults": defaults, "isOwnerEnv": false }))
        }
        "/api/save-default-settings" => {
            if let Ok(v) = serde_json::from_slice::<Value>(&body) {
                let defaults = v.get("defaults").cloned().unwrap_or_else(|| json!({}));
                let _ = std::fs::write(ctx.data_dir.join("default-settings.json"), defaults.to_string());
            }
            ok(json!({ "success": true }))
        }
        "/api/audio-output-info" => ok(json!({})),
        "/api/proxy-config" | "/api/proxy-test" => ok(json!({ "success": true })),
        "/api/spotify-auth-start" => reply(spotify::auth_start(ctx, &get("clientId"))),
        "/api/spotify-callback" => {
            let (status, html) = spotify::callback(ctx, q).await;
            Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                .body(Body::from(html))
                .unwrap()
        }
        "/api/spotify-status" => ok(spotify::status(ctx).await),
        "/api/spotify-logout" => {
            spotify::logout(ctx);
            ok(json!({ "success": true }))
        }
        "/api/spotify-library" => {
            let on = |k: &str| matches!(get(k).as_str(), "1" | "true");
            reply(spotify::library(ctx, on("likes"), on("playlists")).await)
        }
        "/api/translate" => {
            let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let lines: Vec<String> = b
                .get("texts")
                .or_else(|| b.get("lines"))
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect())
                .unwrap_or_default();
            let to = b.get("targetLang").or_else(|| b.get("to")).and_then(Value::as_str).unwrap_or("en").to_string();
            ok(json!({ "success": true, "translations": extras::translate(ctx, lines, &to).await }))
        }
        _ => fail(404, "This feature is not available in Jeem yet"),
    }
}

// Saves a liked track for offline listening; the file is written in the background.
async fn offline_save(ctx: &Arc<Ctx>, q: &HashMap<String, String>) -> Value {
    let get = |k: &str| q.get(k).cloned().unwrap_or_default();
    let id = get("id");
    if !id.is_empty() && crate::cache::find(ctx, &id).is_some() {
        return json!({ "success": true, "downloaded": true });
    }
    let query = format!("{} {}", get("artist"), get("title"));
    let url = match ytm::page_url_for(ctx, &id, &get("url"), &query).await {
        Ok(u) => u,
        Err(e) => return json!({ "success": false, "error": e }),
    };
    let key = crate::cache::key_for(&url);
    if crate::cache::find(ctx, &key).is_some() {
        return json!({ "success": true, "downloaded": true });
    }
    if crate::cache::is_caching(ctx, &key) {
        return json!({ "success": true, "downloading": true });
    }
    let raw = match ytm::resolve_direct(ctx, &url).await {
        Ok(r) if !r.contains(".m3u8") => r,
        Ok(_) => return json!({ "success": false, "error": "this track cannot be saved" }),
        Err(e) => return json!({ "success": false, "error": e }),
    };
    crate::cache::remember(
        ctx,
        &key,
        json!({
            "id": id, "title": get("title"), "artist": get("artist"),
            "duration": get("duration").parse::<f64>().unwrap_or(0.0), "thumbnail": get("thumbnail"),
            "platform": if get("platform").is_empty() { "youtube".to_string() } else { get("platform") }, "url": url
        }),
    );
    tauri::async_runtime::spawn(crate::cache::store(ctx.clone(), key, raw));
    json!({ "success": true, "downloading": true })
}

// Starts saving a podcast episode for offline listening; the file is written in the background.
async fn podcast_download(ctx: &Arc<Ctx>, q: &HashMap<String, String>) -> Value {
    let get = |k: &str| q.get(k).cloned().unwrap_or_default();
    let url = get("url");
    if !url.starts_with("https://www.youtube.com/watch?v=") {
        return json!({ "success": false, "error": "bad episode url" });
    }
    let key = crate::cache::key_for(&url);
    if crate::cache::find(ctx, &key).is_some() {
        return json!({ "success": true, "downloaded": true });
    }
    if crate::cache::is_caching(ctx, &key) {
        return json!({ "success": true, "downloading": true });
    }
    let raw = match ytm::resolve_direct(ctx, &url).await {
        Ok(r) if !r.contains(".m3u8") => r,
        Ok(_) => return json!({ "success": false, "error": "this episode cannot be downloaded" }),
        Err(e) => return json!({ "success": false, "error": e }),
    };
    crate::cache::remember(
        ctx,
        &key,
        json!({
            "id": get("id"), "title": get("title"), "artist": get("artist"), "album": get("album"),
            "duration": get("duration").parse::<f64>().unwrap_or(0.0), "thumbnail": get("thumbnail"),
            "platform": "youtube", "url": url, "podcast": true
        }),
    );
    tauri::async_runtime::spawn(crate::cache::store_limited(ctx.clone(), key, raw, 3 * 1024 * 1024 * 1024));
    json!({ "success": true, "downloading": true })
}

async fn audio_proxy(ctx: &Arc<Ctx>, parts: &Parts, target: &str) -> Response {
    if !target.starts_with("http") {
        return fail(400, "bad url");
    }
    // Range requests are capped so playback starts quickly and seeking stays cheap.
    let (start, end) = parts
        .headers
        .get(header::RANGE)
        .and_then(|h| h.to_str().ok())
        .map(|v| {
            let spec = v.trim().trim_start_matches("bytes=");
            let mut it = spec.splitn(2, '-');
            let s = it.next().and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
            let e = it.next().and_then(|x| x.parse::<u64>().ok());
            (s, e)
        })
        .unwrap_or((0, None));
    let last = end.map_or(start + CHUNK - 1, |e| e.min(start + CHUNK - 1));
    let resp = ctx
        .client
        .get(target)
        .header("User-Agent", ytm::UA)
        .header("Range", format!("bytes={start}-{last}"))
        .send()
        .await;
    let Ok(resp) = resp else { return fail(502, "upstream error") };
    let status = resp.status().as_u16();
    let mut builder = Response::builder().status(status);
    for name in ["content-type", "content-length", "content-range", "accept-ranges"] {
        if let Some(v) = resp.headers().get(name) {
            builder = builder.header(name, v.clone());
        }
    }
    builder.body(Body::from_stream(resp.bytes_stream())).unwrap()
}

async fn image_proxy(ctx: &Arc<Ctx>, target: &str) -> Response {
    if !target.starts_with("http") {
        return Response::builder().status(400).body(Body::empty()).unwrap();
    }
    let dir = ctx.data_dir.join("thumbnails");
    let _ = std::fs::create_dir_all(&dir);
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        target.hash(&mut h);
        format!("{:016x}", h.finish())
    };
    let cache = dir.join(format!("proxy-{key}.img"));
    let mut bytes = std::fs::read(&cache).ok();
    if bytes.is_none() {
        if let Ok(r) = ctx.client.get(target).header("User-Agent", ytm::UA).timeout(std::time::Duration::from_secs(6)).send().await {
            if r.status().is_success() {
                let ct = r.headers().get("content-type").cloned();
                if let Ok(b) = r.bytes().await {
                    if b.len() > 500 {
                        let _ = std::fs::write(&cache, &b);
                        let _ = ct;
                        bytes = Some(b.to_vec());
                    }
                }
            }
        }
    }
    match bytes {
        Some(b) => {
            let mime = if b.starts_with(&[0x89, b'P', b'N', b'G']) {
                "image/png"
            } else if b.starts_with(b"RIFF") {
                "image/webp"
            } else if b.starts_with(b"GIF8") {
                "image/gif"
            } else {
                "image/jpeg"
            };
            Response::builder()
                .header(header::CONTENT_TYPE, mime)
                .header(header::CACHE_CONTROL, "public, max-age=31536000")
                .body(Body::from(b))
                .unwrap()
        }
        None => Response::builder().status(302).header(header::LOCATION, target).body(Body::empty()).unwrap(),
    }
}

/// One line in `jeem.log` in the data folder (what "Open logs folder" shows), for things that must not fail silently.
pub fn log_line(ctx: &Ctx, msg: &str) {
    use std::io::Write;
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let path = ctx.data_dir.join("jeem.log");
    // keep the log small
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 512 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{secs}] {msg}");
    }
}

/* ---------- /__ipc: replacement for Electron's ipcRenderer ---------- */

async fn ipc(ctx: &Arc<Ctx>, parts: &Parts, body: Bytes, cmd: &str) -> Response {
    let token_ok = parts.headers.get("x-jeem-token").and_then(|h| h.to_str().ok()) == Some(ctx.token.as_str());
    if !token_ok {
        return fail(403, "forbidden");
    }
    let payload: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let arg = |i: usize| payload.pointer(&format!("/args/{i}")).cloned().unwrap_or(Value::Null);
    let win = ctx.app.get_webview_window("main");
    let result: Value = match cmd {
        "getAppVersion" => json!(ctx.app.package_info().version.to_string()),
        "checkForUpdates" => crate::updater::check(ctx, true).await,
        "updateAction" => crate::updater::action(ctx, arg(0).as_str().unwrap_or("")),
        "installUpdateNow" => crate::updater::install_now(ctx),
        "getDataDir" => json!({ "dir": ctx.data_dir.to_string_lossy(), "defaultDir": ctx.data_dir.to_string_lossy(), "isCustom": false }),
        "getDownloadDir" => {
            let def = crate::default_download_dir(ctx);
            let custom = shell::get(ctx, "downloadDir").as_str().map(String::from);
            json!({ "dir": custom.clone().unwrap_or_else(|| def.to_string_lossy().to_string()), "defaultDir": def.to_string_lossy(), "isCustom": custom.is_some() })
        }
        "setDownloadDir" => {
            let dir = arg(0).as_str().unwrap_or("").to_string();
            if dir.is_empty() || std::fs::create_dir_all(&dir).is_err() {
                json!({ "ok": false, "error": "That folder is not writable" })
            } else {
                shell::set(ctx, "downloadDir", json!(dir));
                json!({ "ok": true, "dir": dir })
            }
        }
        "resetDownloadDir" => {
            shell::set(ctx, "downloadDir", Value::Null);
            json!({ "ok": true, "dir": crate::default_download_dir(ctx).to_string_lossy(), "reset": true })
        }
        "setDataDir" => json!({ "ok": false, "error": "Changing the data folder is not supported in Jeem" }),
        "restartApp" => {
            ctx.app.restart();
        }
        "setTrayOnClose" => {
            shell::set(ctx, "trayOnClose", json!(arg(0).as_bool().unwrap_or(true)));
            Value::Null
        }
        "getMultiInstance" => json!(shell::get(ctx, "multiInstance").as_bool().unwrap_or(false)),
        "setMultiInstance" => {
            shell::set(ctx, "multiInstance", json!(arg(0).as_bool().unwrap_or(false)));
            Value::Null
        }
        "getGlobalShortcuts" => json!(shell::get(ctx, "globalShortcuts").as_bool().unwrap_or(false)),
        "setGlobalShortcuts" => {
            shell::set(ctx, "globalShortcuts", json!(arg(0).as_bool().unwrap_or(false)));
            shell::sync_global_shortcuts(ctx);
            Value::Null
        }
        "setMediaKeys" => {
            shell::set(ctx, "mediaKeys", json!(arg(0).as_bool().unwrap_or(true)));
            shell::sync_media_keys(ctx);
            Value::Null
        }
        "mediaKeysActive" => {
            ctx.media_keys_active.store(true, std::sync::atomic::Ordering::Relaxed);
            shell::sync_media_keys(ctx);
            Value::Null
        }
        "toggleMiniWindow" => json!(shell::toggle_mini(ctx)),
        "setMiniLyrics" => json!(shell::set_mini_lyrics(ctx, arg(0).as_bool().unwrap_or(false))),
        "openLogsFolder" => {
            use tauri_plugin_opener::OpenerExt;
            let _ = ctx.app.opener().open_path(ctx.data_dir.to_string_lossy(), None::<&str>);
            Value::Null
        }
        "getDiagnostics" => json!({ "version": "0.1.0", "platform": "win32", "dataDir": ctx.data_dir.to_string_lossy(), "port": ctx.port }),
        "getGpuCompositing" => json!("enabled"),
        "storageInitSync" => std::fs::read_to_string(ctx.data_dir.join("storage.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .filter(|v| v.as_object().is_some_and(|o| !o.is_empty()))
            .unwrap_or(Value::Null),
        "storageSaveBatch" | "storageSaveSync" => {
            let data = arg(0);
            if !data.is_object() {
                log_line(ctx, "storage save skipped: the state was not an object");
                json!({ "ok": false, "error": "not an object" })
            } else {
                let text = data.to_string();
                let file = ctx.data_dir.join("storage.json");
                let tmp = ctx.data_dir.join("storage.json.tmp");
                let mut result = json!({ "ok": true, "bytes": text.len() });
                // write beside the file and swap, so a crash never leaves half a file; if the swap is refused, write the file itself
                let swapped = std::fs::write(&tmp, &text).and_then(|_| std::fs::rename(&tmp, &file));
                if let Err(e) = swapped {
                    log_line(ctx, &format!("storage swap failed ({e}), writing the file directly"));
                    if let Err(e2) = std::fs::write(&file, &text) {
                        log_line(ctx, &format!("storage save failed: {e2}"));
                        result = json!({ "ok": false, "error": e2.to_string() });
                    }
                }
                result
            }
        }
        "readCacheFile" => std::fs::read_to_string(ctx.data_dir.join("cache.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or(Value::Null),
        "writeCacheFile" => {
            let data = arg(0);
            if data.is_object() {
                let _ = std::fs::write(ctx.data_dir.join("cache.json"), data.to_string());
            }
            Value::Null
        }
        "minimizeWindow" => {
            if let Some(w) = &win {
                let _ = w.minimize();
            }
            Value::Null
        }
        "maximizeWindow" => {
            if let Some(w) = &win {
                if w.is_maximized().unwrap_or(false) { let _ = w.unmaximize(); } else { let _ = w.maximize(); }
            }
            Value::Null
        }
        "closeWindow" => {
            if let Some(w) = &win {
                let _ = w.close();
            }
            Value::Null
        }
        "windowDrag" => {
            if let Some(w) = &win {
                let _ = w.start_dragging();
            }
            Value::Null
        }
        "openExternal" => {
            let u = arg(0).as_str().unwrap_or("").to_string();
            if u.starts_with("https://") {
                use tauri_plugin_opener::OpenerExt;
                let _ = ctx.app.opener().open_url(u, None::<&str>);
            }
            Value::Null
        }
        "welcomeFinish" => {
            let detail = arg(0);
            let _ = std::fs::write(ctx.data_dir.join("first-run-done"), "1");
            if detail.is_object() && detail.get("skipped").and_then(Value::as_bool) != Some(true) {
                *ctx.welcome_result.lock().unwrap() = Some(detail);
            }
            if let Some(w) = &win {
                let _ = w.eval(format!("location.href='{INDEX}'"));
            }
            Value::Null
        }
        "takeWelcomeResult" => ctx.welcome_result.lock().unwrap().take().unwrap_or(Value::Null),
        "pickLocalFiles" => local::pick_files(ctx).await,
        "pickLocalFolder" => local::pick_folder(ctx).await,
        "rescanLocalMetadata" => {
            let paths: Vec<std::path::PathBuf> = arg(0)
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(std::path::PathBuf::from)).collect())
                .unwrap_or_default();
            local::describe(ctx, paths).await
        }
        "pickDownloadDir" | "pickDataDir" => {
            local::pick_dir(ctx).await.map_or(Value::Null, |d| json!(d.to_string_lossy()))
        }
        "getCacheSize" => json!({ "audio": crate::cache::size(ctx), "web": 0 }),
        "clearCache" => {
            crate::cache::clear(ctx);
            json!(true)
        }
        "savePlaylistsJson" => {
            let p = arg(0);
            let name = p.get("defaultName").and_then(Value::as_str).unwrap_or("playlists.json").to_string();
            let content = p.get("content").and_then(Value::as_str).unwrap_or("").to_string();
            local::save_text(ctx, name, "JSON", "json", content).await
        }
        "accountExport" => local::save_text(ctx, "jeem-account.rfacc".into(), "Account (.rfacc)", "rfacc", arg(0).to_string()).await,
        "accountImport" => match local::open_text(ctx, "Account (.rfacc)", "rfacc").await {
            Some((path, text)) => match serde_json::from_str::<Value>(&text) {
                Ok(data) => json!({ "success": true, "data": data, "filePath": path.to_string_lossy() }),
                Err(_) => json!({ "success": false, "error": "This is not a valid account file" }),
            },
            None => json!({ "canceled": true }),
        },
        "gsyncStatus" => gsync::status(ctx),
        "gsyncLogin" => gsync::login(ctx).await,
        "gsyncLogout" => gsync::logout(ctx).await,
        "gsyncInfo" => gsync::info(ctx).await,
        "gsyncPull" => gsync::pull(ctx).await,
        "gsyncPush" => gsync::push(ctx, arg(0).as_str().unwrap_or("")).await,
        "googleLogin" => google::login(ctx).await,
        "googleLogout" => google::logout(ctx).await,
        "googleAuthStatus" => google::status(ctx),
        "googleImportLibrary" => google::import_library(ctx, &arg(0)).await,
        "youtubePlaybackStatus" => json!({ "loggedIn": false }),
        "youtubePlaybackLogin" => json!({ "success": false, "loggedIn": false, "error": "Not supported in Jeem" }),
        "youtubePlaybackLogout" => json!({ "success": true }),
        "importAudioFiles" => library::import_files(ctx).await,
        "pickMedia" => local::pick_media(ctx).await,
        "listThemes" => addons::themes_list(ctx),
        "importThemes" => addons::themes_import(ctx).await,
        "setThemeEnabled" => {
            addons::theme_set_enabled(ctx, arg(0).as_str().unwrap_or(""), arg(1).as_bool().unwrap_or(false));
            Value::Null
        }
        "disableAllThemes" => {
            addons::themes_disable_all(ctx);
            Value::Null
        }
        "removeTheme" => {
            addons::theme_remove(ctx, arg(0).as_str().unwrap_or(""));
            Value::Null
        }
        "getAppCss" => addons::app_css(ctx),
        "listPlugins" => addons::plugins_list(ctx),
        "importPlugins" => addons::plugins_import(ctx).await,
        "setPluginEnabled" => {
            addons::plugin_set_enabled(ctx, arg(0).as_str().unwrap_or(""), arg(1).as_bool().unwrap_or(false));
            Value::Null
        }
        "disableAllPlugins" => {
            addons::plugins_disable_all(ctx);
            Value::Null
        }
        "removePlugin" => {
            addons::plugin_remove(ctx, arg(0).as_str().unwrap_or(""));
            Value::Null
        }
        "openPluginsFolder" => {
            addons::open_plugins_folder(ctx);
            Value::Null
        }
        "discordRpcGetEnabled" => json!(discord::enabled()),
        "discordRpcGetConnected" => json!(discord::connected()),
        "discordRpcUpdate" => {
            discord::update(ctx, arg(0));
            Value::Null
        }
        "discordRpcClear" => {
            discord::clear(ctx);
            Value::Null
        }
        "discordRpcSetEnabled" => {
            discord::set_enabled(ctx, arg(0).as_bool().unwrap_or(true));
            Value::Null
        }
        "discordRpcReconnect" => {
            discord::reconnect(ctx);
            Value::Null
        }
        _ => Value::Null,
    };
    ok(json!({ "result": result }))
}
