// Disk cache of played tracks: instant replay, offline playback and the "offline library".
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::{Ctx, ytm};

const EXTS: [&str; 4] = ["m4a", "webm", "opus", "mp3"];
const MAX_BYTES: u64 = 120 * 1024 * 1024;

pub fn dir(ctx: &Ctx) -> PathBuf {
    let d = ctx.data_dir.join("cache");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn load_index(dir: &std::path::Path) -> std::collections::HashMap<String, Value> {
    std::fs::read_to_string(dir.join("index.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_index(ctx: &Ctx) {
    let idx = ctx.cache_index.lock().unwrap();
    let _ = std::fs::write(dir(ctx).join("index.json"), serde_json::to_string(&*idx).unwrap_or_default());
}

fn safe(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect()
}

// Stable cache key: the YouTube video id when there is one, otherwise a hash of the page URL.
pub fn key_for(url: &str) -> String {
    if let Some(i) = url.find("v=") {
        let id: String = url[i + 2..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
        if id.len() == 11 {
            return id;
        }
    }
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    format!("u{:016x}", h.finish())
}

pub fn file_for(ctx: &Ctx, key: &str) -> Option<PathBuf> {
    let d = dir(ctx);
    let key = safe(key);
    EXTS.iter().map(|e| d.join(format!("{key}.{e}"))).find(|p| p.is_file())
}

// Finds a cached file by cache key or by the track id the interface uses.
pub fn find(ctx: &Ctx, id: &str) -> Option<(String, PathBuf)> {
    if let Some(p) = file_for(ctx, id) {
        return Some((safe(id), p));
    }
    let key = ctx
        .cache_index
        .lock()
        .unwrap()
        .iter()
        .find(|(_, m)| m.get("id").and_then(Value::as_str) == Some(id))
        .map(|(k, _)| k.clone())?;
    file_for(ctx, &key).map(|p| (key, p))
}

pub fn remember(ctx: &Ctx, key: &str, meta: Value) {
    {
        let mut idx = ctx.cache_index.lock().unwrap();
        let mut m = meta;
        m["lastPlayed"] = json!(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64));
        idx.insert(key.to_string(), m);
    }
    save_index(ctx);
}

pub fn is_caching(ctx: &Ctx, key: &str) -> bool {
    ctx.caching.lock().unwrap().contains(key)
}

// Downloads the resolved audio stream into the cache (background, best effort).
pub async fn store(ctx: Arc<Ctx>, key: String, raw_url: String) {
    store_limited(ctx, key, raw_url, MAX_BYTES).await;
}

pub async fn store_limited(ctx: Arc<Ctx>, key: String, raw_url: String, max: u64) {
    if file_for(&ctx, &key).is_some() || !ctx.caching.lock().unwrap().insert(key.clone()) {
        return;
    }
    let result = store_inner(&ctx, &key, &raw_url, max).await;
    ctx.caching.lock().unwrap().remove(&key);
    if result.is_err() {
        let _ = std::fs::remove_file(dir(&ctx).join(format!("{}.part", safe(&key))));
    }
}

async fn store_inner(ctx: &Arc<Ctx>, key: &str, raw_url: &str, max: u64) -> Result<(), String> {
    use std::io::Write;
    let part = dir(ctx).join(format!("{}.part", safe(key)));
    let mut file = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut pos: u64 = 0;
    let mut ext;
    loop {
        let end = pos + 4 * 1024 * 1024 - 1;
        let resp = ctx
            .client
            .get(raw_url)
            .header("User-Agent", ytm::UA)
            .header("Range", format!("bytes={pos}-{end}"))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        if status != 206 && status != 200 {
            return Err(format!("HTTP {status}"));
        }
        let ct = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        ext = if ct.contains("webm") { "webm" } else if ct.contains("mpeg") { "mp3" } else if ct.contains("ogg") { "opus" } else { "m4a" };
        let total: u64 = resp
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit('/').next())
            .and_then(|t| t.parse().ok())
            .unwrap_or(0);
        let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        pos += bytes.len() as u64;
        if status == 200 || total == 0 || pos >= total || bytes.is_empty() {
            break;
        }
        if total > max {
            return Err("too large".into());
        }
    }
    drop(file);
    std::fs::rename(&part, dir(ctx).join(format!("{}.{ext}", safe(key)))).map_err(|e| e.to_string())
}

// Offline library: music by default, or only downloaded podcast episodes when `podcasts` is set.
pub fn library(ctx: &Ctx, podcasts: bool) -> Value {
    let d = dir(ctx);
    let idx = ctx.cache_index.lock().unwrap().clone();
    let mut total = 0u64;
    let mut tracks = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&d) {
        for e in rd.flatten() {
            let p = e.path();
            let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("").to_string();
            if !EXTS.contains(&ext.as_str()) {
                continue;
            }
            let key = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            let m = idx.get(&key).cloned().unwrap_or_else(|| json!({}));
            let is_pod = m.get("podcast").and_then(Value::as_bool).unwrap_or(false);
            if is_pod != podcasts {
                continue;
            }
            total += size;
            let s = |k: &str| m.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            tracks.push(json!({
                "url": s("url"), "isPodcast": is_pod, "album": s("album"),
                "id": if s("id").is_empty() { key.clone() } else { s("id") },
                "cleanId": key,
                "title": if s("title").is_empty() { key.clone() } else { s("title") },
                "artist": if s("artist").is_empty() { "Unknown artist".to_string() } else { s("artist") },
                "duration": m.get("duration").cloned().unwrap_or(json!(0)),
                "thumbnail": s("thumbnail"), "localThumbnail": s("thumbnail"),
                "platform": if s("platform").is_empty() { "youtube".to_string() } else { s("platform") },
                "fileSize": size,
                "lastPlayed": m.get("lastPlayed").cloned().unwrap_or(json!(0)),
                "analysis": analysis_get(ctx, &key)
            }));
        }
    }
    json!({ "success": true, "count": tracks.len(), "totalBytes": total, "tracks": tracks })
}

pub fn delete(ctx: &Ctx, id: &str) -> bool {
    let Some((key, path)) = find(ctx, id) else { return false };
    let ok = std::fs::remove_file(path).is_ok();
    ctx.cache_index.lock().unwrap().remove(&key);
    save_index(ctx);
    ok
}

pub fn clear(ctx: &Ctx) {
    if let Ok(rd) = std::fs::read_dir(dir(ctx)) {
        for e in rd.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    ctx.cache_index.lock().unwrap().clear();
}

pub fn size(ctx: &Ctx) -> u64 {
    std::fs::read_dir(dir(ctx))
        .map(|rd| rd.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum())
        .unwrap_or(0)
}

// Audio analysis (tempo, key...) is computed by the interface; it is only stored here.
fn analysis_path(ctx: &Ctx) -> PathBuf {
    ctx.data_dir.join("analysis.json")
}

fn analysis_all(ctx: &Ctx) -> serde_json::Map<String, Value> {
    std::fs::read_to_string(analysis_path(ctx))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

pub fn analysis_get(ctx: &Ctx, id: &str) -> Value {
    analysis_all(ctx).get(&safe(id)).cloned().unwrap_or(Value::Null)
}

pub fn analysis_set(ctx: &Ctx, id: &str, analysis: Value) {
    let mut all = analysis_all(ctx);
    all.insert(safe(id), analysis);
    let _ = std::fs::write(analysis_path(ctx), Value::Object(all).to_string());
}
