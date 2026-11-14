// Local music: file pickers, tag/cover extraction through ffmpeg, and serving files to the page.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use tauri_plugin_dialog::DialogExt;

use crate::{Ctx, tools, ytm};

const AUDIO_EXTS: [&str; 8] = ["mp3", "flac", "wav", "ogg", "m4a", "aac", "opus", "wma"];

pub fn file_url(ctx: &Ctx, path: &Path) -> String {
    format!(
        "http://127.0.0.1:{}/__file?t={}&p={}",
        ctx.port,
        ctx.token,
        ytm::urlencode(&path.to_string_lossy())
    )
}

fn hash_hex(s: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn tag(stderr: &str, key: &str) -> String {
    for line in stderr.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix(key) {
            if let Some(v) = rest.trim_start().strip_prefix(':') {
                return v.trim().to_string();
            }
        }
    }
    String::new()
}

fn parse_duration(stderr: &str) -> f64 {
    let Some(i) = stderr.find("Duration: ") else { return 0.0 };
    let t: String = stderr[i + 10..].chars().take_while(|c| c.is_ascii_digit() || *c == ':' || *c == '.').collect();
    t.split(':').fold(0.0, |acc, p| acc * 60.0 + p.parse::<f64>().unwrap_or(0.0))
}

fn metadata(ctx: &Arc<Ctx>, path: &Path) -> Option<Value> {
    if !path.is_file() {
        return None;
    }
    let covers = ctx.data_dir.join("local-covers");
    let _ = std::fs::create_dir_all(&covers);
    let hash = hash_hex(&path.to_string_lossy());
    let cover = covers.join(format!("{hash}.jpg"));

    let mut cmd = tools::command(tools::tool_path(ctx, "ffmpeg"));
    cmd.args(["-hide_banner", "-i"]).arg(path);
    let out = cmd.output().ok()?;
    let info = String::from_utf8_lossy(&out.stderr).to_string();

    if !cover.exists() && info.contains("Video:") {
        let mut c = tools::command(tools::tool_path(ctx, "ffmpeg"));
        c.args(["-y", "-v", "error", "-i"]).arg(path).args(["-an", "-frames:v", "1", "-vcodec", "mjpeg"]).arg(&cover);
        let _ = c.output();
    }
    let mut title = tag(&info, "title");
    if title.is_empty() {
        title = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    }
    let artist = tag(&info, "artist");
    let cover_url = if cover.exists() { file_url(ctx, &cover) } else { String::new() };
    Some(json!({
        "id": format!("local_{hash}"),
        "title": title,
        "artist": if artist.is_empty() { "Unknown Artist".to_string() } else { artist },
        "album": tag(&info, "album"),
        "trackNumber": tag(&info, "track").split('/').next().and_then(|n| n.parse::<u32>().ok()).unwrap_or(0),
        "duration": parse_duration(&info).round(),
        "path": path.to_string_lossy(),
        "url": file_url(ctx, path),
        "thumbnail": cover_url, "cover": cover_url,
        "platform": "local"
    }))
}

pub async fn describe(ctx: &Arc<Ctx>, paths: Vec<PathBuf>) -> Value {
    let ctx = ctx.clone();
    let tracks = tauri::async_runtime::spawn_blocking(move || {
        paths.iter().filter_map(|p| metadata(&ctx, p)).collect::<Vec<Value>>()
    })
    .await
    .unwrap_or_default();
    Value::Array(tracks)
}

pub async fn pick_files(ctx: &Arc<Ctx>) -> Value {
    let app = ctx.app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog().file().add_filter("Audio", &AUDIO_EXTS).blocking_pick_files()
    })
    .await
    .ok()
    .flatten();
    let paths: Vec<PathBuf> = picked.unwrap_or_default().into_iter().filter_map(|p| p.into_path().ok()).collect();
    describe(ctx, paths).await
}

fn walk(dir: &Path, depth: u8, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if depth < 8 {
                walk(&p, depth + 1, out);
            }
        } else if p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| AUDIO_EXTS.contains(&e.to_lowercase().as_str()))
        {
            out.push(p);
        }
    }
}

pub async fn pick_dir(ctx: &Arc<Ctx>) -> Option<PathBuf> {
    let app = ctx.app.clone();
    tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
        .await
        .ok()
        .flatten()
        .and_then(|p| p.into_path().ok())
}

pub async fn pick_folder(ctx: &Arc<Ctx>) -> Value {
    let Some(dir) = pick_dir(ctx).await else { return json!([]) };
    let mut files = Vec::new();
    walk(&dir, 0, &mut files);
    files.sort();
    describe(ctx, files).await
}

fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).as_deref() {
        Some("mp3") => "audio/mpeg",
        Some("flac") => "audio/flac",
        Some("wav") => "audio/wav",
        Some("ogg") | Some("opus") => "audio/ogg",
        Some("m4a") | Some("aac") => "audio/mp4",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

// Serves a local file (audio or cover) with Range support; protected by the per-run token.
pub async fn serve_file(
    ctx: &Ctx,
    q: &std::collections::HashMap<String, String>,
    headers: &axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::body::Body;
    use axum::http::header;
    let fail = |code: u16| axum::response::Response::builder().status(code).body(Body::empty()).unwrap();
    if q.get("t").map(String::as_str) != Some(ctx.token.as_str()) {
        return fail(403);
    }
    let path = PathBuf::from(q.get("p").cloned().unwrap_or_default());
    let range = headers.get(header::RANGE).and_then(|h| h.to_str().ok()).map(String::from);
    let ranged = range.is_some();
    let path2 = path.clone();
    let result = tauri::async_runtime::spawn_blocking(move || -> Option<(Vec<u8>, u64, u64, u64)> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(&path2).ok()?;
        let total = f.metadata().ok()?.len();
        if total == 0 {
            return Some((Vec::new(), 0, 0, 0));
        }
        let (start, end) = match range {
            Some(r) => {
                let spec = r.trim().trim_start_matches("bytes=").to_string();
                let mut it = spec.splitn(2, '-');
                let s = it.next().and_then(|x| x.parse::<u64>().ok()).unwrap_or(0).min(total - 1);
                let e = it.next().and_then(|x| x.parse::<u64>().ok()).unwrap_or(total - 1);
                (s, e.min(total - 1).min(s + 8 * 1024 * 1024))
            }
            None => (0, total - 1),
        };
        f.seek(SeekFrom::Start(start)).ok()?;
        let mut buf = vec![0u8; (end - start + 1) as usize];
        f.read_exact(&mut buf).ok()?;
        Some((buf, start, end, total))
    })
    .await
    .ok()
    .flatten();
    let Some((buf, start, end, total)) = result else { return fail(404) };
    let mut b = axum::response::Response::builder()
        .status(if ranged { 206 } else { 200 })
        .header(header::CONTENT_TYPE, mime_for(&path))
        .header("Accept-Ranges", "bytes");
    if ranged {
        b = b.header("Content-Range", format!("bytes {start}-{end}/{total}"));
    }
    b.body(Body::from(buf)).unwrap()
}

// Picks an image or video (for custom backgrounds) and returns a URL the page can load.
pub async fn pick_media(ctx: &Arc<Ctx>) -> Value {
    let app = ctx.app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog().file().add_filter("Media", &["png", "jpg", "jpeg", "webp", "gif", "mp4", "webm"]).blocking_pick_file()
    })
    .await
    .ok()
    .flatten()
    .and_then(|p| p.into_path().ok());
    picked.map_or(Value::Null, |p| json!(file_url(ctx, &p)))
}

// Saves text content through a "Save as" dialog.
pub async fn save_text(ctx: &Arc<Ctx>, name: String, label: &'static str, ext: &'static str, content: String) -> Value {
    let app = ctx.app.clone();
    let path = tauri::async_runtime::spawn_blocking(move || {
        app.dialog().file().add_filter(label, &[ext]).set_file_name(name).blocking_save_file()
    })
    .await
    .ok()
    .flatten()
    .and_then(|p| p.into_path().ok());
    let Some(path) = path else { return json!({ "canceled": true }) };
    match std::fs::write(&path, content) {
        Ok(()) => json!({ "success": true, "filePath": path.to_string_lossy() }),
        Err(e) => json!({ "success": false, "error": e.to_string() }),
    }
}

pub async fn open_text(ctx: &Arc<Ctx>, label: &'static str, ext: &'static str) -> Option<(PathBuf, String)> {
    let app = ctx.app.clone();
    let path = tauri::async_runtime::spawn_blocking(move || app.dialog().file().add_filter(label, &[ext]).blocking_pick_file())
        .await
        .ok()
        .flatten()
        .and_then(|p| p.into_path().ok())?;
    let text = std::fs::read_to_string(&path).ok()?;
    Some((path, text))
}
