// Downloads (yt-dlp) and encoding of audio rendered by the interface (ffmpeg).
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::{Ctx, tools, ytm};

fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if r#"<>:"/\|?*"#.contains(c) || c.is_control() { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').to_string();
    if cleaned.is_empty() { "track".into() } else { cleaned }
}

fn out_dir(ctx: &Ctx, q: &HashMap<String, String>) -> PathBuf {
    if let Some(d) = q.get("customDir").filter(|d| !d.is_empty()) {
        let p = PathBuf::from(d);
        if std::fs::create_dir_all(&p).is_ok() {
            return p;
        }
    }
    crate::download_dir(ctx)
}

fn valid_ext(q: &HashMap<String, String>) -> String {
    let f = q.get("format").map(|s| s.to_lowercase()).unwrap_or_default();
    if ["mp3", "m4a", "flac", "opus", "wav", "ogg"].contains(&f.as_str()) { f } else { "mp3".into() }
}

fn bitrate(q: &HashMap<String, String>) -> &'static str {
    match q.get("quality").map(String::as_str) {
        Some("2") => "256k",
        Some("4") => "192k",
        Some("5") => "128k",
        _ => "320k",
    }
}

async fn fetch_cover(ctx: &Ctx, url: &str, dir: &std::path::Path) -> Option<PathBuf> {
    if !url.starts_with("http") {
        return None;
    }
    let bytes = ctx.client.get(url).send().await.ok()?.bytes().await.ok()?;
    if bytes.len() < 500 {
        return None;
    }
    let path = dir.join(format!("cover_{}.jpg", std::process::id()));
    std::fs::write(&path, &bytes).ok()?;
    Some(path)
}

pub async fn download(ctx: &Arc<Ctx>, q: &HashMap<String, String>) -> Value {
    let get = |k: &str| q.get(k).cloned().unwrap_or_default();
    let mut url = get("url");
    let (title, artist) = (get("title"), get("artist"));
    if url.is_empty() {
        let found = ytm::ytm_search(ctx, &format!("{artist} {title}")).await.unwrap_or_default();
        match found.first() {
            Some(t) => url = t.get("url").and_then(Value::as_str).unwrap_or("").to_string(),
            None => return json!({ "success": false, "error": "could not find the track" }),
        }
    }
    let dir = out_dir(ctx, q);
    let ext = valid_ext(q);
    let base = if artist.is_empty() { sanitize(&title) } else { format!("{} - {}", sanitize(&artist), sanitize(&title)) };
    let template = dir.join(format!("{base}.%(ext)s"));
    let with_cover = ["mp3", "m4a", "flac"].contains(&ext.as_str());
    let ctx2 = ctx.clone();
    let quality = get("quality");
    let result = tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let run = |cover: bool| {
            let mut cmd = tools::command(tools::tool_path(&ctx2, "yt-dlp"));
            cmd.args(["-q", "--no-warnings", "--no-playlist", "-x", "--audio-format", &ext, "--embed-metadata"]);
            if ext == "mp3" && !quality.is_empty() {
                cmd.args(["--audio-quality", &quality]);
            }
            if cover {
                cmd.args(["--embed-thumbnail", "--convert-thumbnails", "jpg"]);
            }
            cmd.arg("--ffmpeg-location").arg(&ctx2.tools_dir);
            cmd.arg("-o").arg(&template).args(["--print", "after_move:filepath"]).arg(&url);
            tools::run_cmd(cmd)
        };
        let out = if with_cover { run(true).or_else(|_| run(false))? } else { run(false)? };
        Ok(out.lines().last().unwrap_or("").trim().to_string())
    })
    .await;
    match result {
        Ok(Ok(path)) => {
            let p = PathBuf::from(&path);
            json!({
                "success": true,
                "directory": p.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default(),
                "filename": p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
            })
        }
        Ok(Err(e)) => json!({ "success": false, "error": e }),
        Err(e) => json!({ "success": false, "error": e.to_string() }),
    }
}

// The interface renders a WAV (trim / effects); this converts it to the chosen format with tags.
pub async fn encode(ctx: &Arc<Ctx>, q: &HashMap<String, String>, wav: Vec<u8>) -> Value {
    let get = |k: &str, d: &str| q.get(k).filter(|s| !s.is_empty()).cloned().unwrap_or_else(|| d.to_string());
    let (title, artist) = (get("title", "untitled"), get("artist", "unknown artist"));
    let ext = valid_ext(q);
    let dir = out_dir(ctx, q);
    let tmp = dir.join(".tmp");
    let _ = std::fs::create_dir_all(&tmp);
    let wav_path = tmp.join(format!("render_{}.wav", std::process::id()));
    if std::fs::write(&wav_path, &wav).is_err() {
        return json!({ "success": false, "error": "cannot write temporary file" });
    }
    let cover = fetch_cover(ctx, &get("thumbnail", ""), &tmp).await;
    let filename = format!("{} - {}.{ext}", sanitize(&artist), sanitize(&title));
    let out_path = dir.join(&filename);
    let br = bitrate(q);

    let ctx2 = ctx.clone();
    let (wav2, cover2, out2, ext2) = (wav_path.clone(), cover.clone(), out_path.clone(), ext.clone());
    let result = tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let mut cmd = tools::command(tools::tool_path(&ctx2, "ffmpeg"));
        cmd.args(["-y", "-v", "error", "-i"]).arg(&wav2);
        match (&cover2, ext2.as_str()) {
            (Some(c), "mp3") => {
                cmd.arg("-i").arg(c).args(["-map", "0:a:0", "-map", "1:v:0", "-c:v", "mjpeg", "-id3v2_version", "3"]);
            }
            (Some(c), "m4a" | "flac") => {
                cmd.arg("-i").arg(c).args(["-map", "0:a:0", "-map", "1:v:0", "-c:v", "mjpeg", "-disposition:v:0", "attached_pic"]);
            }
            _ => {}
        }
        if ["mp3", "m4a", "opus", "ogg"].contains(&ext2.as_str()) {
            cmd.args(["-b:a", br]);
        }
        cmd.arg("-metadata").arg(format!("title={title}"));
        cmd.arg("-metadata").arg(format!("artist={artist}"));
        cmd.arg(&out2);
        tools::run_cmd(cmd).map(|_| ())
    })
    .await;
    let _ = std::fs::remove_file(&wav_path);
    if let Some(c) = cover {
        let _ = std::fs::remove_file(c);
    }
    match result {
        Ok(Ok(())) => json!({ "success": true, "filename": filename, "directory": dir.to_string_lossy() }),
        Ok(Err(e)) => json!({ "success": false, "error": e }),
        Err(e) => json!({ "success": false, "error": e.to_string() }),
    }
}
