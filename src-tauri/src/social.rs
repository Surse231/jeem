// VK and TikTok imports. VK: pasted text or a public playlist page; TikTok: audio downloaded with yt-dlp.
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::{Ctx, cache, tools, ytm};

type Reply = (u16, Value);

fn fail(status: u16, msg: &str) -> Reply {
    (status, json!({ "success": false, "error": msg }))
}

fn duration_secs(t: &str) -> u64 {
    let parts: Vec<u64> = t.trim().split(':').filter_map(|p| p.parse().ok()).collect();
    match parts.as_slice() {
        [m, s] => m * 60 + s,
        [h, m, s] => h * 3600 + m * 60 + s,
        _ => 0,
    }
}

fn is_duration(t: &str) -> bool {
    let p: Vec<&str> = t.split(':').collect();
    p.len() == 2 && p[0].len() <= 2 && p[1].len() == 2 && p.iter().all(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit()))
}

fn decode_entities(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    decode_entities(&out).split_whitespace().collect::<Vec<_>>().join(" ")
}

// "Title (3:45)" -> ("Title", 225)
fn trailing_duration(s: &str) -> (String, u64) {
    let t = s.trim();
    let trimmed = t.trim_end_matches([')', ']']);
    if let Some(idx) = trimmed.rfind(|c: char| c == ' ' || c == '(' || c == '[') {
        let tail = &trimmed[idx + 1..];
        if is_duration(tail) {
            return (trimmed[..idx].trim().to_string(), duration_secs(tail));
        }
    }
    (t.to_string(), 0)
}

fn parse_text(text: &str) -> Vec<(String, String, u64)> {
    let skip = ["мне нравится", "слушать всё", "слушать все", "перемешать", "плейлист"];
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !skip.contains(&l.to_lowercase().as_str()) && !l.chars().all(|c| c.is_ascii_digit()) && !matches!(*l, "e" | "E" | "е" | "Е"))
        .collect();
    let mut single = Vec::new();
    for line in &lines {
        if is_duration(line) {
            continue;
        }
        if let Some(i) = line.find(['-', '–', '—', '\t']) {
            let artist = line[..i].trim().trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')' || c == ' ').trim().to_string();
            let (title, dur) = trailing_duration(line[i..].trim_start_matches(['-', '–', '—', '\t']).trim());
            if !artist.is_empty() && !title.is_empty() && artist.chars().count() < 100 && title.chars().count() < 150 {
                single.push((title, artist, dur));
            }
        }
    }
    let mut multi = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if is_duration(l) && i >= 2 && !is_duration(lines[i - 2]) && !is_duration(lines[i - 1]) {
            multi.push((lines[i - 2].to_string(), lines[i - 1].to_string(), duration_secs(l)));
        }
    }
    if multi.len() > single.len() { multi } else if !single.is_empty() { single } else { multi }
}

async fn enrich(ctx: &Arc<Ctx>, raw: Vec<(String, String, u64)>) -> Vec<Value> {
    let items: Vec<(usize, (String, String, u64))> = raw.into_iter().take(100).enumerate().collect();
    let mut out: Vec<Value> = Vec::new();
    for batch in items.chunks(6) {
        let mut handles = Vec::new();
        for (idx, (title, artist, dur)) in batch.iter().cloned() {
            let ctx = ctx.clone();
            handles.push(tauri::async_runtime::spawn(async move {
                let query = format!("{artist} - {title}");
                if let Ok(hits) = ytm::ytm_search(&ctx, &query).await {
                    if let Some(hit) = hits.first() {
                        let mut h = hit.clone();
                        let thumb = h.get("thumbnail").and_then(Value::as_str).unwrap_or("").to_string();
                        h["cover"] = json!(thumb);
                        h["platform"] = json!("ytmusic");
                        return h;
                    }
                }
                json!({
                    "id": format!("vk_{idx}"), "title": title, "artist": artist,
                    "duration": if dur == 0 { 180 } else { dur }, "thumbnail": "", "cover": "",
                    "searchQuery": format!("{artist} {title}"), "platform": "ytmusic"
                })
            }));
        }
        for h in handles {
            if let Ok(v) = h.await {
                out.push(v);
            }
        }
    }
    out
}

fn first_cover(tracks: &[Value]) -> String {
    tracks
        .first()
        .and_then(|t| t.get("cover").or_else(|| t.get("thumbnail")))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub async fn vk_import(ctx: &Arc<Ctx>, url: &str, text: &str, name: &str) -> Reply {
    let mut name = if name.is_empty() { "VK Music".to_string() } else { name.to_string() };
    if !text.trim().is_empty() {
        let raw = parse_text(text);
        if raw.is_empty() {
            return fail(400, "Could not parse any tracks from pasted text");
        }
        let tracks = enrich(ctx, raw).await;
        return (200, json!({ "success": true, "name": name, "cover": first_cover(&tracks), "tracks": tracks }));
    }
    let url = url.trim();
    if url.is_empty() {
        return fail(400, "Neither url nor text was provided");
    }
    let mut raw: Vec<(String, String, u64)> = Vec::new();
    let mut cover = String::new();
    let mut seen = std::collections::HashSet::new();
    if let Ok(r) = ctx.client.get(url).header("Accept-Language", "ru,en;q=0.8").timeout(Duration::from_secs(10)).send().await {
        let html = r.text().await.unwrap_or_default();
        if let Some(a) = html.find("<title>") {
            if let Some(b) = html[a..].find("</title>") {
                let t = strip_tags(&html[a + 7..a + b]);
                let t = t.split(" | ").next().unwrap_or(&t).trim().to_string();
                if !t.is_empty() {
                    name = t;
                }
            }
        }
        if let Some(a) = html.find("property=\"og:image\" content=\"") {
            cover = html[a + 29..].split('"').next().unwrap_or("").to_string();
        }
        let mut rest = html.as_str();
        while let Some(i) = rest.find("data-audio=") {
            let after = &rest[i + 11..];
            let Some(q) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
                rest = after;
                continue;
            };
            let Some(end) = after[1..].find(q) else { break };
            let json_str = decode_entities(&after[1..1 + end]);
            rest = &after[1 + end..];
            if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(&json_str) {
                if a.len() >= 5 {
                    let title = strip_tags(a[3].as_str().unwrap_or(""));
                    let artist = strip_tags(a[4].as_str().unwrap_or(""));
                    let dur = a.get(5).and_then(Value::as_u64).unwrap_or(0);
                    if !title.is_empty() && !artist.is_empty() && seen.insert(format!("{}-{}", artist.to_lowercase(), title.to_lowercase())) {
                        raw.push((title, artist, dur));
                    }
                }
            }
        }
    }
    if raw.is_empty() {
        let ctx2 = ctx.clone();
        let target = url.to_string();
        let data = tauri::async_runtime::spawn_blocking(move || -> Option<Value> {
            let mut cmd = tools::command(tools::tool_path(&ctx2, "yt-dlp"));
            cmd.args(["--flat-playlist", "--dump-single-json", "--no-warnings"]).arg(&target);
            serde_json::from_str(&tools::run_cmd(cmd).ok()?).ok()
        })
        .await
        .ok()
        .flatten();
        if let Some(d) = data {
            if name == "VK Music" {
                if let Some(t) = d.get("title").and_then(Value::as_str) {
                    name = t.to_string();
                }
            }
            let entries = d.get("entries").and_then(Value::as_array).cloned().unwrap_or_else(|| vec![d.clone()]);
            for it in entries {
                let s = |k: &str| it.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
                let title = if s("title").is_empty() { s("track") } else { s("title") };
                let artist = [s("artist"), s("uploader"), s("creator")].into_iter().find(|a| !a.is_empty()).unwrap_or_else(|| "Unknown Artist".into());
                if !title.is_empty() {
                    raw.push((title, artist, it.get("duration").and_then(Value::as_f64).unwrap_or(0.0).round() as u64));
                }
            }
        }
    }
    if raw.is_empty() {
        return fail(404, "No tracks found in VK link");
    }
    let tracks = enrich(ctx, raw).await;
    let cover = if cover.is_empty() { first_cover(&tracks) } else { cover };
    (200, json!({ "success": true, "name": name, "cover": cover, "tracks": tracks }))
}

pub async fn tiktok_import(ctx: &Arc<Ctx>, url: &str) -> Reply {
    let url = url.trim().to_string();
    if url.is_empty() {
        return fail(400, "Missing TikTok URL");
    }
    let ctx2 = ctx.clone();
    let res = tauri::async_runtime::spawn_blocking(move || -> Result<Value, String> {
        let mut cmd = tools::command(tools::tool_path(&ctx2, "yt-dlp"));
        cmd.args(["--no-playlist", "--dump-single-json", "--no-warnings"]).arg(&url);
        let info: Value = serde_json::from_str(&tools::run_cmd(cmd)?).map_err(|e| e.to_string())?;
        let s = |k: &str| info.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let raw_id = if s("id").is_empty() { "x".to_string() } else { s("id") };
        let tid = format!("tt_{}", raw_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect::<String>());
        if cache::file_for(&ctx2, &tid).is_none() {
            let out = cache::dir(&ctx2).join(format!("{tid}.%(ext)s"));
            let mut dl = tools::command(tools::tool_path(&ctx2, "yt-dlp"));
            dl.args(["-q", "--no-warnings", "--no-playlist", "-x", "--audio-format", "mp3"]).arg("--ffmpeg-location").arg(&ctx2.tools_dir).arg("-o").arg(out).arg(&url);
            tools::run_cmd(dl)?;
        }
        let strip_tags_hash = |t: &str| t.split_whitespace().filter(|w| !w.starts_with('#')).collect::<Vec<_>>().join(" ");
        let raw_title = if !s("track").is_empty() { s("track") } else { s("title") };
        let title = { let t = strip_tags_hash(&raw_title); if t.is_empty() { "TikTok Audio".to_string() } else { t } };
        let artist = [s("artist"), s("uploader"), s("creator")].into_iter().find(|a| !a.is_empty()).unwrap_or_else(|| "TikTok".into());
        Ok(json!({
            "tid": tid, "title": title, "artist": artist, "cover": s("thumbnail"),
            "duration": info.get("duration").and_then(Value::as_f64).unwrap_or(0.0).round()
        }))
    })
    .await;
    let info = match res {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return fail(502, &e),
        Err(e) => return fail(500, &e.to_string()),
    };
    let tid = info["tid"].as_str().unwrap_or("").to_string();
    cache::remember(
        ctx,
        &tid,
        json!({ "id": tid, "title": info["title"], "artist": info["artist"], "duration": info["duration"], "thumbnail": info["cover"], "platform": "tiktok" }),
    );
    let local = format!("http://127.0.0.1:{}/api/local-track?id={}", ctx.port, ytm::urlencode(&tid));
    (
        200,
        json!({
            "success": true, "name": info["title"], "cover": info["cover"],
            "tracks": [{
                "id": tid, "title": info["title"], "artist": info["artist"], "duration": info["duration"],
                "thumbnail": info["cover"], "cover": info["cover"], "isCached": true, "isTikTok": true,
                "staticAudio": true, "url": local, "platform": "tiktok"
            }]
        }),
    )
}
