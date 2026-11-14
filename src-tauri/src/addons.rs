// User plugins (JS) and themes (CSS) stored in the app data folder; the page runs/injects them.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value, json};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use crate::Ctx;

const PLUGIN_MAX: u64 = 1024 * 1024;
const THEME_MAX: u64 = 2 * 1024 * 1024;

const EXAMPLE: &str = "/**\n * @name Hello plugin\n * @author Jeem\n * @version 1.0.0\n * @description Example plugin. Edit or delete this file.\n */\nconsole.log('[plugin] Hello from the example plugin');\n";

fn plugins_dir(ctx: &Ctx) -> PathBuf {
    ctx.data_dir.join("plugins")
}

fn themes_dir(ctx: &Ctx) -> PathBuf {
    ctx.data_dir.join("themes")
}

fn safe_name(n: &str) -> bool {
    n.to_lowercase().ends_with(".js")
        && !n.contains("..")
        && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' || c == ' ')
}

fn safe_id(id: &str) -> bool {
    id.len() >= 5 && id.len() <= 25 && id.starts_with('t') && id[1..].chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

fn meta(code: &str, key: &str) -> String {
    let Some(start) = code.find("/*") else { return String::new() };
    let Some(len) = code[start..].find("*/") else { return String::new() };
    let block = &code[start..start + len];
    let tag = format!("@{key}");
    for line in block.lines() {
        if let Some(i) = line.find(&tag) {
            let rest = &line[i + tag.len()..];
            if rest.starts_with([' ', '\t']) {
                return rest.trim().trim_end_matches('*').trim().to_string();
            }
        }
    }
    String::new()
}

fn read_obj(path: &Path, key: &str) -> Map<String, Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get(key).and_then(Value::as_object).cloned())
        .unwrap_or_default()
}

/* ---------- plugins ---------- */

fn ensure_plugins(ctx: &Ctx) -> PathBuf {
    let d = plugins_dir(ctx);
    if !d.exists() {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join("example-hello.js"), EXAMPLE);
    }
    d
}

fn write_enabled(ctx: &Ctx, enabled: Map<String, Value>) {
    let d = ensure_plugins(ctx);
    let _ = std::fs::write(d.join("plugins.json"), json!({ "enabled": enabled }).to_string());
}

pub fn plugins_list(ctx: &Ctx) -> Value {
    let d = ensure_plugins(ctx);
    let enabled = read_obj(&d.join("plugins.json"), "enabled");
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&d) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !safe_name(&name) || e.metadata().map(|m| m.len() > PLUGIN_MAX).unwrap_or(true) {
                continue;
            }
            let Ok(code) = std::fs::read_to_string(e.path()) else { continue };
            let n = meta(&code, "name");
            out.push(json!({
                "file": name,
                "name": if n.is_empty() { name.trim_end_matches(".js").to_string() } else { n },
                "author": meta(&code, "author"), "version": meta(&code, "version"),
                "description": meta(&code, "description"),
                "enabled": enabled.contains_key(&name), "code": code
            }));
        }
    }
    Value::Array(out)
}

pub fn plugin_set_enabled(ctx: &Ctx, file: &str, on: bool) {
    if !safe_name(file) {
        return;
    }
    let mut s = read_obj(&ensure_plugins(ctx).join("plugins.json"), "enabled");
    if on {
        s.insert(file.to_string(), json!(true));
    } else {
        s.remove(file);
    }
    write_enabled(ctx, s);
}

pub fn plugins_disable_all(ctx: &Ctx) {
    write_enabled(ctx, Map::new());
}

pub fn plugin_remove(ctx: &Ctx, file: &str) {
    if !safe_name(file) {
        return;
    }
    let _ = std::fs::remove_file(ensure_plugins(ctx).join(file));
    plugin_set_enabled(ctx, file, false);
}

pub fn open_plugins_folder(ctx: &Ctx) {
    let d = ensure_plugins(ctx);
    let _ = ctx.app.opener().open_path(d.to_string_lossy(), None::<&str>);
}

async fn pick(ctx: &Arc<Ctx>, label: &'static str, ext: &'static str) -> Vec<PathBuf> {
    let app = ctx.app.clone();
    tauri::async_runtime::spawn_blocking(move || app.dialog().file().add_filter(label, &[ext]).blocking_pick_files())
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| p.into_path().ok())
        .collect()
}

pub async fn plugins_import(ctx: &Arc<Ctx>) -> Value {
    let d = ensure_plugins(ctx);
    let mut added = 0;
    for file in pick(ctx, "JavaScript plugin", "js").await {
        let Some(name) = file.file_name().map(|n| n.to_string_lossy().to_string()) else { continue };
        if !safe_name(&name) || std::fs::metadata(&file).map(|m| m.len() > PLUGIN_MAX).unwrap_or(true) {
            continue;
        }
        if std::fs::copy(&file, d.join(&name)).is_ok() {
            added += 1;
        }
    }
    json!(added)
}

/* ---------- themes ---------- */

fn read_themes(ctx: &Ctx) -> Vec<Value> {
    std::fs::read_to_string(themes_dir(ctx).join("index.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("themes").and_then(Value::as_array).cloned())
        .unwrap_or_default()
}

fn write_themes(ctx: &Ctx, themes: &[Value]) {
    let d = themes_dir(ctx);
    let _ = std::fs::create_dir_all(&d);
    let _ = std::fs::write(d.join("index.json"), json!({ "themes": themes }).to_string());
}

pub fn themes_list(ctx: &Ctx) -> Value {
    let d = themes_dir(ctx);
    Value::Array(
        read_themes(ctx)
            .into_iter()
            .filter_map(|mut t| {
                let id = t.get("id").and_then(Value::as_str)?.to_string();
                let css = std::fs::read_to_string(d.join(format!("{id}.css"))).ok()?;
                t["css"] = json!(css);
                Some(t)
            })
            .collect(),
    )
}

pub async fn themes_import(ctx: &Arc<Ctx>) -> Value {
    let d = themes_dir(ctx);
    let _ = std::fs::create_dir_all(&d);
    let mut themes = read_themes(ctx);
    let mut added = Vec::new();
    for (i, file) in pick(ctx, "CSS theme", "css").await.into_iter().enumerate() {
        if std::fs::metadata(&file).map(|m| m.len() > THEME_MAX).unwrap_or(true) {
            continue;
        }
        let Ok(css) = std::fs::read_to_string(&file) else { continue };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |t| t.as_millis());
        let id = format!("t{:x}{:03x}", now, i);
        if std::fs::write(d.join(format!("{id}.css")), &css).is_err() {
            continue;
        }
        let n = meta(&css, "name");
        let name = if n.is_empty() { file.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default() } else { n };
        let entry = json!({ "id": id, "name": name, "author": meta(&css, "author"), "description": meta(&css, "description"), "enabled": false });
        themes.push(entry.clone());
        let mut with_css = entry;
        with_css["css"] = json!(css);
        added.push(with_css);
    }
    write_themes(ctx, &themes);
    Value::Array(added)
}

pub fn theme_set_enabled(ctx: &Ctx, id: &str, on: bool) {
    if !safe_id(id) {
        return;
    }
    let themes: Vec<Value> = read_themes(ctx)
        .into_iter()
        .map(|mut t| {
            if t.get("id").and_then(Value::as_str) == Some(id) {
                t["enabled"] = json!(on);
            }
            t
        })
        .collect();
    write_themes(ctx, &themes);
}

pub fn themes_disable_all(ctx: &Ctx) {
    let themes: Vec<Value> = read_themes(ctx).into_iter().map(|mut t| { t["enabled"] = json!(false); t }).collect();
    write_themes(ctx, &themes);
}

pub fn theme_remove(ctx: &Ctx, id: &str) {
    if !safe_id(id) {
        return;
    }
    let _ = std::fs::remove_file(themes_dir(ctx).join(format!("{id}.css")));
    let rest: Vec<Value> = read_themes(ctx).into_iter().filter(|t| t.get("id").and_then(Value::as_str) != Some(id)).collect();
    write_themes(ctx, &rest);
}

pub fn app_css(ctx: &Ctx) -> Value {
    let css = ctx
        .app
        .asset_resolver()
        .get("/src/renderer/styles.css".to_string())
        .or_else(|| ctx.app.asset_resolver().get("src/renderer/styles.css".to_string()))
        .map(|a| String::from_utf8_lossy(&a.bytes).to_string())
        .unwrap_or_default();
    json!(css)
}
