// Track credits (Deezer, Genius) and word-by-word lyrics (NetEase).
use std::time::Duration;

use serde_json::{Value, json};

use crate::{Ctx, ytm::urlencode};

fn s(v: Option<&Value>) -> &str {
    v.and_then(Value::as_str).unwrap_or("")
}

async fn get_json(ctx: &Ctx, url: &str, referer: Option<&str>) -> Option<Value> {
    let mut req = ctx
        .client
        .get(url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
        .timeout(Duration::from_secs(8));
    if let Some(r) = referer {
        req = req.header("Referer", r);
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    // Some of these APIs answer with a non-JSON content type, so decode the text ourselves.
    serde_json::from_str(&resp.text().await.ok()?).ok()
}

fn push_unique(list: &mut Vec<String>, name: &str) {
    let name = name.trim();
    if !name.is_empty() && !list.iter().any(|x| x == name) {
        list.push(name.to_string());
    }
}

pub async fn credits(ctx: &Ctx, artist: &str, title: &str) -> Value {
    let (artist, title) = (artist.trim(), title.trim());
    let mut writers: Vec<String> = Vec::new();
    let mut composers: Vec<String> = Vec::new();
    let mut producers: Vec<String> = Vec::new();
    let mut performers: Vec<String> = Vec::new();
    push_unique(&mut performers, artist);
    let mut release = String::new();

    let q = format!("{artist} {title}").trim().to_string();
    if !q.is_empty() {
        let search = get_json(ctx, &format!("https://api.deezer.com/search?q={}&limit=3", urlencode(&q)), None).await;
        if let Some(id) = search.as_ref().and_then(|d| d.pointer("/data/0/id")).and_then(Value::as_u64) {
            if let Some(t) = get_json(ctx, &format!("https://api.deezer.com/track/{id}"), None).await {
                release = s(t.get("release_date")).to_string();
                for c in t.get("contributors").and_then(Value::as_array).cloned().unwrap_or_default() {
                    let name = s(c.get("name"));
                    let role = s(c.get("role")).to_lowercase();
                    if role.contains("composer") {
                        push_unique(&mut composers, name);
                    } else if role.contains("lyricist") || role.contains("author") || role.contains("writer") {
                        push_unique(&mut writers, name);
                    } else if role.contains("producer") {
                        push_unique(&mut producers, name);
                    } else if role.contains("main") || role.contains("artist") {
                        push_unique(&mut performers, name);
                    }
                }
            }
        }
        if writers.is_empty() && producers.is_empty() {
            let hits = get_json(ctx, &format!("https://genius.com/api/search/song?q={}", urlencode(&q)), None).await;
            let song_id = hits
                .as_ref()
                .and_then(|h| h.pointer("/response/sections/0/hits/0/result/id").or_else(|| h.pointer("/response/hits/0/result/id")))
                .and_then(Value::as_u64);
            if let Some(id) = song_id {
                if let Some(d) = get_json(ctx, &format!("https://genius.com/api/songs/{id}"), None).await {
                    if let Some(song) = d.pointer("/response/song") {
                        if release.is_empty() {
                            release = s(song.get("release_date")).to_string();
                        }
                        for (key, list) in [("writer_artists", &mut writers), ("producer_artists", &mut producers)] {
                            for a in song.get(key).and_then(Value::as_array).cloned().unwrap_or_default() {
                                push_unique(list, s(a.get("name")));
                            }
                        }
                    }
                }
            }
        }
    }
    json!({ "success": true, "credits": {
        "writers": writers, "composers": composers, "producers": producers,
        "performers": performers, "releaseDate": release
    } })
}

/* ---------- plain lyrics from Genius (last resort, no timing) ---------- */

fn html_unescape(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut rest = t;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..end];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// Text of every <div data-lyrics-container="true"> on a Genius song page; page furniture
// marked data-exclude-from-selection (headers, ads inside the lyrics) is skipped.
fn genius_page_lyrics(html: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let marker = "data-lyrics-container=\"true\"";
    let mut pos = 0;
    while let Some(found) = html[pos..].find(marker) {
        let start = pos + found;
        let Some(open_end) = html[start..].find('>') else { break };
        let mut i = start + open_end + 1;
        let mut depth = 1usize;
        let mut skip_depth: Option<usize> = None;
        let mut text = String::new();
        while i < html.len() && depth > 0 {
            if html.as_bytes()[i] == b'<' {
                let Some(close) = html[i..].find('>') else { break };
                let tag = &html[i..i + close + 1];
                let lower = tag.to_ascii_lowercase();
                if lower.starts_with("</div") {
                    depth -= 1;
                    if skip_depth == Some(depth + 1) {
                        skip_depth = None;
                    }
                } else if lower.starts_with("<div") && !lower.ends_with("/>") {
                    depth += 1;
                    if skip_depth.is_none() && lower.contains("data-exclude-from-selection=\"true\"") {
                        skip_depth = Some(depth);
                    }
                } else if lower.starts_with("<br") && skip_depth.is_none() {
                    text.push('\n');
                }
                i += close + 1;
                continue;
            }
            let next = html[i..].find('<').map_or(html.len(), |n| i + n);
            if skip_depth.is_none() {
                text.push_str(&html[i..next]);
            }
            i = next;
        }
        for line in html_unescape(&text).lines() {
            let l = line.trim();
            // Section labels like [Chorus] are not sung lines.
            if l.starts_with('[') && l.ends_with(']') {
                if lines.last().is_some_and(|x| !x.is_empty()) {
                    lines.push(String::new());
                }
                continue;
            }
            lines.push(l.to_string());
        }
        lines.push(String::new());
        pos = i.max(start + 1);
    }
    let mut out: Vec<String> = Vec::new();
    for l in lines {
        if l.is_empty() && out.last().is_none_or(|x: &String| x.is_empty()) {
            continue;
        }
        out.push(l);
    }
    out.join("\n").trim().to_string()
}

// Genius protects its song pages with Cloudflare, which turns away some HTTP clients.
// When the built-in client is refused, the page is fetched with the system curl (part of Windows 10+).
async fn genius_fetch_page(ctx: &Ctx, url: &str) -> Option<String> {
    const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36";
    let marker = "data-lyrics-container";
    if let Ok(resp) = ctx.client.get(url).header("User-Agent", UA).timeout(Duration::from_secs(10)).send().await {
        if resp.status().is_success() {
            if let Ok(t) = resp.text().await {
                if t.contains(marker) {
                    return Some(t);
                }
            }
        }
    }
    let url = url.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let mut cmd = crate::tools::command("curl.exe");
        cmd.args(["-s", "-L", "-m", "15", "-A", UA, &url]);
        let out = cmd.output().ok()?;
        let t = String::from_utf8_lossy(&out.stdout).to_string();
        if t.contains(marker) { Some(t) } else { None }
    })
    .await
    .ok()
    .flatten()
}

pub async fn genius_plain(ctx: &Ctx, title: &str, artist: &str) -> Option<String> {
    let q = clean_query(&format!("{title} {artist}"));
    let hits = get_json(ctx, &format!("https://genius.com/api/search/song?q={}", urlencode(&q)), None).await?;
    let list = hits
        .pointer("/response/sections/0/hits")
        .or_else(|| hits.pointer("/response/hits"))
        .and_then(Value::as_array)?
        .clone();
    let nt = norm(&clean_query(title));
    let targets: Vec<String> = artist.split([',', '&', '/', '+']).map(norm).filter(|a| !a.is_empty()).collect();
    let url = list.iter().take(6).find_map(|h| {
        let r = h.get("result")?;
        let name = norm(s(r.get("title")));
        let who = norm(s(r.pointer("/primary_artist/name")));
        let artist_ok = targets.is_empty() || targets.iter().any(|t| who.contains(t.as_str()) || t.contains(who.as_str()));
        let title_ok = !nt.is_empty() && !name.is_empty() && (name.contains(&nt) || nt.contains(&name));
        if artist_ok && title_ok && s(r.get("lyrics_state")) != "unreleased" { Some(s(r.get("url")).to_string()) } else { None }
    })?;
    if !url.starts_with("https://genius.com/") {
        return None;
    }
    let html = genius_fetch_page(ctx, &url).await?;
    let lyrics = genius_page_lyrics(&html);
    if lyrics.lines().filter(|l| !l.trim().is_empty()).count() < 2 { None } else { Some(lyrics) }
}

/* ---------- word-by-word lyrics from NetEase "yrc" ---------- */

fn is_cjk(t: &str) -> bool {
    t.chars().any(|c| ('\u{4e00}'..='\u{9fa5}').contains(&c) || ('\u{3040}'..='\u{30ff}').contains(&c))
}

fn norm(t: &str) -> String {
    t.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || ('\u{0400}'..='\u{04ff}').contains(c) || ('\u{4e00}'..='\u{9fa5}').contains(c))
        .collect()
}

// "(start,dur[,x])text" repeated; text runs until the next '('.
fn parse_words(rest: &str) -> Vec<Value> {
    let mut words: Vec<Value> = Vec::new();
    let mut cur: Option<(String, f64, f64)> = None;
    let flush = |cur: &mut Option<(String, f64, f64)>, words: &mut Vec<Value>| {
        if let Some((t, st, en)) = cur.take() {
            let t = t.trim().to_string();
            if !t.is_empty() {
                words.push(json!({ "text": t, "start": st, "end": en }));
            }
        }
    };
    let mut i = 0;
    let b = rest.as_bytes();
    while i < b.len() {
        if b[i] != b'(' {
            i += 1;
            continue;
        }
        let Some(close) = rest[i..].find(')') else { break };
        let head = &rest[i + 1..i + close];
        let nums: Vec<&str> = head.split(',').collect();
        let parsed = (nums.first().and_then(|x| x.parse::<f64>().ok()), nums.get(1).and_then(|x| x.parse::<f64>().ok()));
        let text_start = i + close + 1;
        let text_end = rest[text_start..].find('(').map_or(rest.len(), |p| text_start + p);
        i = text_end;
        let (Some(st_ms), Some(dur_ms)) = parsed else { continue };
        if nums.len() < 2 || nums.len() > 3 {
            continue;
        }
        let raw = &rest[text_start..text_end];
        let (st, en) = (st_ms / 1000.0, (st_ms + dur_ms) / 1000.0);
        let cjk = is_cjk(raw);
        if cur.as_ref().is_some_and(|(t, _, _)| cjk || is_cjk(t)) {
            flush(&mut cur, &mut words);
        }
        match cur.as_mut() {
            None => cur = Some((raw.to_string(), st, en)),
            Some((t, _, e)) => {
                t.push_str(raw);
                *e = e.max(en);
            }
        }
        if raw.chars().any(char::is_whitespace) || cjk {
            flush(&mut cur, &mut words);
        }
    }
    flush(&mut cur, &mut words);
    words
}

fn parse_yrc(text: &str) -> Vec<Value> {
    let mut lines = Vec::new();
    for raw in text.lines() {
        let t = raw.trim();
        let Some(body) = t.strip_prefix('[') else { continue };
        let Some(end) = body.find(']') else { continue };
        let nums: Vec<&str> = body[..end].split(',').collect();
        if nums.len() != 2 {
            continue;
        }
        let (Ok(start_ms), Ok(dur_ms)) = (nums[0].parse::<f64>(), nums[1].parse::<f64>()) else { continue };
        let words = parse_words(&body[end + 1..]);
        if words.is_empty() {
            continue;
        }
        let full: String = words.iter().map(|w| s(w.get("text"))).collect::<Vec<_>>().join(" ").trim().to_string();
        if full.is_empty() || full.starts_with("作词") || full.starts_with("作曲") || full.starts_with("制作人") {
            continue;
        }
        lines.push(json!({ "start": start_ms / 1000.0, "end": (start_ms + dur_ms) / 1000.0, "text": full, "words": words }));
    }
    lines
}

// Removes "(feat. ...)", "[live ...]" style groups and " - remaster..." suffixes from a search query.
fn clean_query(q: &str) -> String {
    let mut out = String::new();
    let mut depth_skip: Option<char> = None;
    let chars: Vec<char> = q.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(close) = depth_skip {
            if c == close {
                depth_skip = None;
            }
            i += 1;
            continue;
        }
        if c == '(' || c == '[' {
            let rest: String = chars[i + 1..].iter().take(12).collect::<String>().to_lowercase();
            if rest.starts_with("feat") || rest.starts_with("live") {
                depth_skip = Some(if c == '(' { ')' } else { ']' });
                i += 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    let lower = out.to_lowercase();
    if let Some(p) = lower.find(" - remaster") {
        out.truncate(p);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

async fn netease_lyric(ctx: &Ctx, id: u64) -> Option<Vec<Value>> {
    let url = format!("http://music.163.com/api/song/lyric?id={id}&lv=1&kv=1&tv=-1&yv=1");
    let d = get_json(ctx, &url, Some("https://music.163.com")).await?;
    let lines = parse_yrc(s(d.pointer("/yrc/lyric")));
    if lines.is_empty() { None } else { Some(lines) }
}

// Line-synced lyrics (LRC text) from NetEase: a second source for tracks LRCLIB does not have.
pub async fn netease_lrc(ctx: &Ctx, title: &str, artist: &str, duration: f64) -> Option<String> {
    let id = netease_song_id(ctx, title, artist, duration).await?;
    let url = format!("http://music.163.com/api/song/lyric?id={id}&lv=1&kv=1&tv=-1");
    let d = get_json(ctx, &url, Some("https://music.163.com")).await?;
    let lrc = s(d.pointer("/lrc/lyric")).trim().to_string();
    // Keep only real lyric lines: NetEase puts credits ("作词", "作曲"...) into the first lines.
    let lines: Vec<&str> = lrc.lines().filter(|l| !l.contains("作词") && !l.contains("作曲") && !l.contains("编曲") && !l.contains("制作人")).collect();
    let timed = lines.iter().filter(|l| l.trim_start().starts_with('[') && l.contains(':')).count();
    if timed < 4 { None } else { Some(lines.join("\n")) }
}

async fn netease_song_id(ctx: &Ctx, title: &str, artist: &str, duration: f64) -> Option<u64> {
    let query = clean_query(&format!("{title} {artist}"));
    let targets: Vec<String> = artist.split([',', '&', '/', '+']).map(norm).filter(|a| !a.is_empty()).collect();
    let urls = [
        format!("http://music.163.com/api/search/get/web?s={}&type=1&limit=5", urlencode(&query)),
        format!("http://music.163.com/api/search/get?s={}&type=1&limit=5", urlencode(&query)),
    ];
    for url in urls {
        let Some(d) = get_json(ctx, &url, Some("https://music.163.com")).await else { continue };
        let songs = d.pointer("/result/songs").and_then(Value::as_array).cloned().unwrap_or_default();
        let nt = norm(title);
        let matched = songs.iter().find(|song| {
            if duration > 0.0 {
                let dur = song.get("dt").or_else(|| song.get("duration")).and_then(Value::as_f64).unwrap_or(0.0) / 1000.0;
                if (dur - duration).abs() > 3.0 {
                    return false;
                }
            }
            let nc = norm(s(song.get("name")));
            if nt.is_empty() || nc.is_empty() || (!nt.contains(&nc) && !nc.contains(&nt)) || !crate::ytm::same_version(title, s(song.get("name"))) {
                return false;
            }
            let cands: Vec<String> = song
                .get("artists")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|x| norm(s(x.get("name")))).filter(|x| !x.is_empty()).collect())
                .unwrap_or_default();
            targets.is_empty() || cands.iter().any(|c| targets.iter().any(|t| c.contains(t.as_str()) || t.contains(c.as_str())))
        });
        if let Some(id) = matched.and_then(|m| m.get("id")).and_then(Value::as_u64) {
            return Some(id);
        }
    }
    None
}

pub async fn words(ctx: &Ctx, title: &str, artist: &str, duration: f64) -> Value {
    let query = clean_query(&format!("{title} {artist}"));
    let targets: Vec<String> = artist.split([',', '&', '/', '+']).map(norm).filter(|a| !a.is_empty()).collect();
    let urls = [
        format!("http://music.163.com/api/search/get/web?s={}&type=1&limit=5", urlencode(&query)),
        format!("http://music.163.com/api/search/get?s={}&type=1&limit=5", urlencode(&query)),
    ];
    for url in urls {
        let Some(d) = get_json(ctx, &url, Some("https://music.163.com")).await else { continue };
        let songs = d.pointer("/result/songs").and_then(Value::as_array).cloned().unwrap_or_default();
        let nt = norm(title);
        let matched = songs.iter().find(|song| {
            if duration > 0.0 {
                let dur = song.get("dt").or_else(|| song.get("duration")).and_then(Value::as_f64).unwrap_or(0.0) / 1000.0;
                if (dur - duration).abs() > 3.0 {
                    return false;
                }
            }
            let nc = norm(s(song.get("name")));
            if nt.is_empty() || nc.is_empty() || (!nt.contains(&nc) && !nc.contains(&nt)) || !crate::ytm::same_version(title, s(song.get("name"))) {
                return false;
            }
            let cands: Vec<String> = song
                .get("artists")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|x| norm(s(x.get("name")))).filter(|x| !x.is_empty()).collect())
                .unwrap_or_default();
            targets.is_empty() || cands.iter().any(|c| targets.iter().any(|t| c.contains(t.as_str()) || t.contains(c.as_str())))
        });
        if let Some(id) = matched.and_then(|m| m.get("id")).and_then(Value::as_u64) {
            if let Some(lines) = netease_lyric(ctx, id).await {
                return json!({ "success": true, "lines": lines });
            }
        }
    }
    json!({ "success": false, "lines": [] })
}

pub async fn track_cover(ctx: &Ctx, artist: &str, title: &str) -> Value {
    let term = format!("{artist} {title}").trim().to_string();
    if term.is_empty() {
        return json!({ "success": false, "cover": "" });
    }
    if let Some(d) = get_json(ctx, &format!("https://itunes.apple.com/search?term={}&entity=song&limit=1", urlencode(&term)), None).await {
        let art = s(d.pointer("/results/0/artworkUrl100")).replace("100x100", "600x600");
        if !art.is_empty() {
            return json!({ "success": true, "cover": art });
        }
    }
    if let Some(d) = get_json(ctx, &format!("https://api.deezer.com/search?q={}&limit=1", urlencode(&term)), None).await {
        for k in ["cover_xl", "cover_big", "cover_medium"] {
            let c = s(d.pointer(&format!("/data/0/album/{k}")));
            if !c.is_empty() {
                return json!({ "success": true, "cover": c });
            }
        }
    }
    json!({ "success": false, "cover": "" })
}

async fn google_translate(ctx: &Ctx, text: &str, to: &str) -> Option<Vec<String>> {
    let to_clean = if to.eq_ignore_ascii_case("zh-cn") || to.eq_ignore_ascii_case("zh-tw") { to.to_string() } else { to.split(['-', '_']).next().unwrap_or(to).to_string() };
    let url = format!("https://translate.googleapis.com/translate_a/single?client=gtx&sl=auto&tl={}&dt=t", urlencode(&to_clean));
    let r = ctx
        .client
        .post(url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/131.0.0.0 Safari/537.36")
        .form(&[("q", text)])
        .timeout(Duration::from_secs(8))
        .send()
        .await
        .ok()?;
    if !r.status().is_success() {
        return None;
    }
    let data: Value = r.json().await.ok()?;
    let parts = data.get(0)?.as_array()?;
    let joined: String = parts.iter().map(|p| s(p.get(0)).to_string()).collect();
    Some(joined.split('\n').map(|l| l.trim().to_string()).collect())
}

// Translates lyric lines in chunks (Google Translate's public endpoint), keeping line count aligned.
pub async fn translate(ctx: &Ctx, lines: Vec<String>, to: &str) -> Vec<String> {
    let mut chunks: Vec<Vec<String>> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    let mut len = 0usize;
    for l in lines {
        if !cur.is_empty() && len + l.len() + 1 > 1800 {
            chunks.push(std::mem::take(&mut cur));
            len = 0;
        }
        len += l.len() + 1;
        cur.push(l);
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    let mut out = Vec::new();
    for chunk in chunks {
        match google_translate(ctx, &chunk.join("\n"), to).await {
            Some(t) if t.len() == chunk.len() => out.extend(t),
            _ => {
                for line in &chunk {
                    if line.trim().is_empty() {
                        out.push(String::new());
                    } else {
                        out.push(google_translate(ctx, line, to).await.and_then(|t| t.into_iter().next()).filter(|t| !t.is_empty()).unwrap_or_else(|| line.clone()));
                    }
                }
            }
        }
    }
    out
}
