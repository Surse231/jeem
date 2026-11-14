// Imported audio library: files copied into the app folder and listed by the interface.
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Map, Value, json};
use tauri_plugin_dialog::DialogExt;

use crate::Ctx;

const EXTS: [&str; 10] = ["mp3", "wav", "flac", "opus", "ogg", "oga", "m4a", "aac", "webm", "weba"];

pub fn dir(ctx: &Ctx) -> PathBuf {
    let d = ctx.data_dir.join("library");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn load(ctx: &Ctx) -> Map<String, Value> {
    std::fs::read_to_string(dir(ctx).join("index.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn save(ctx: &Ctx, idx: &Map<String, Value>) {
    let _ = std::fs::write(dir(ctx).join("index.json"), Value::Object(idx.clone()).to_string());
}

fn valid_file(f: &str) -> bool {
    // local_<12 hex>.<ext>
    let Some(rest) = f.strip_prefix("local_") else { return false };
    let Some((id, ext)) = rest.split_once('.') else { return false };
    id.len() == 12 && id.chars().all(|c| c.is_ascii_hexdigit()) && !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric())
}

fn short_hash(s: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())[..12].to_string()
}

pub async fn import_files(ctx: &Arc<Ctx>) -> Value {
    let app = ctx.app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || app.dialog().file().add_filter("Audio", &EXTS).blocking_pick_files())
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let d = dir(ctx);
    let mut out = Vec::new();
    for p in picked.into_iter().filter_map(|p| p.into_path().ok()) {
        let Ok(meta) = std::fs::metadata(&p) else { continue };
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let id = format!("local_{}", short_hash(&format!("{name}{}", meta.len())));
        let file = format!("{id}.{ext}");
        if !d.join(&file).exists() && std::fs::copy(&p, d.join(&file)).is_err() {
            continue;
        }
        let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        out.push(json!({ "id": id, "file": file, "ext": ext, "name": stem }));
    }
    Value::Array(out)
}

pub fn add(ctx: &Ctx, body: &Value) -> Value {
    let mut idx = load(ctx);
    let d = dir(ctx);
    let mut count = 0;
    for item in body.get("items").and_then(Value::as_array).cloned().unwrap_or_default() {
        let id = item.get("id").and_then(Value::as_str).unwrap_or("");
        let file = item.get("file").and_then(Value::as_str).unwrap_or("");
        if id.is_empty() || !valid_file(file) || !d.join(file).exists() {
            continue;
        }
        let raw = item.get("name").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let trimmed = raw.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches(['.', ' ', '-']).trim().to_string();
        let clean = if trimmed.is_empty() { if raw.is_empty() { "Untitled".to_string() } else { raw.clone() } } else { trimmed };
        let (artist, title) = match clean.split_once(" - ") {
            Some((a, t)) if !a.trim().is_empty() && !t.trim().is_empty() => (a.trim().to_string(), t.trim().to_string()),
            _ => ("Unknown artist".to_string(), clean.clone()),
        };
        let ext = item.get("ext").and_then(Value::as_str).map(String::from).unwrap_or_else(|| file.rsplit('.').next().unwrap_or("").to_string());
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |t| t.as_millis() as u64);
        idx.insert(id.to_string(), json!({ "id": id, "file": file, "ext": ext, "title": title, "artist": artist, "duration": 0, "addedAt": now }));
        count += 1;
    }
    save(ctx, &idx);
    json!({ "success": true, "count": count })
}

pub fn update(ctx: &Ctx, body: &Value) -> Value {
    let mut idx = load(ctx);
    let id = body.get("id").and_then(Value::as_str).unwrap_or("");
    if let (Some(item), Some(dur)) = (idx.get_mut(id), body.get("duration").and_then(Value::as_f64)) {
        item["duration"] = json!(dur);
        save(ctx, &idx);
    }
    json!({ "success": true })
}

pub fn list(ctx: &Ctx) -> Value {
    let mut items: Vec<Value> = load(ctx).into_values().collect();
    items.sort_by_key(|i| std::cmp::Reverse(i.get("addedAt").and_then(Value::as_u64).unwrap_or(0)));
    Value::Array(
        items
            .iter()
            .map(|i| {
                json!({
                    "id": i.get("id"), "platform": "local", "title": i.get("title"), "artist": i.get("artist"),
                    "duration": i.get("duration").and_then(Value::as_f64).unwrap_or(0.0), "thumbnail": ""
                })
            })
            .collect(),
    )
}

pub fn file_path(ctx: &Ctx, id: &str) -> Option<PathBuf> {
    let idx = load(ctx);
    let file = idx.get(id)?.get("file")?.as_str()?.to_string();
    let p = dir(ctx).join(file);
    p.is_file().then_some(p)
}

pub fn remove(ctx: &Ctx, body: &Value, query_id: &str) -> Value {
    let mut idx = load(ctx);
    let d = dir(ctx);
    let kill = |idx: &mut Map<String, Value>, id: &str| {
        if let Some(item) = idx.remove(id) {
            if let Some(f) = item.get("file").and_then(Value::as_str) {
                let _ = std::fs::remove_file(d.join(f));
            }
        }
    };
    if body.get("clearAll").and_then(Value::as_bool) == Some(true) {
        let ids: Vec<String> = idx.keys().cloned().collect();
        for id in ids {
            kill(&mut idx, &id);
        }
    } else if let Some(ids) = body.get("ids").and_then(Value::as_array) {
        for id in ids.iter().filter_map(Value::as_str) {
            kill(&mut idx, id);
        }
    } else {
        let id = body.get("id").and_then(Value::as_str).unwrap_or(query_id).to_string();
        kill(&mut idx, &id);
    }
    save(ctx, &idx);
    json!({ "success": true })
}
