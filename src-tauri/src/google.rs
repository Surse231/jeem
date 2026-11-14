// Google account via the official OAuth flow in your system browser (PKCE, loopback redirect).
// Needs YOUR Google Cloud OAuth client (type "Desktop app") with the YouTube Data API v3 enabled:
// put "googleClientId" and "googleClientSecret" into settings.json.
use std::collections::HashMap;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tauri_plugin_opener::OpenerExt;

use crate::ytm::urlencode;
use crate::{Ctx, shell};

const SCOPE: &str = "https://www.googleapis.com/auth/youtube.readonly";
const API: &str = "https://www.googleapis.com/youtube/v3";
const MISSING: &str = "Add your own Google OAuth client: set googleClientId and googleClientSecret in settings.json (Google Cloud Console, Desktop app, YouTube Data API v3 enabled)";

#[derive(Default, Clone)]
pub struct Pending {
    pub verifier: String,
    pub state: String,
    pub done: Option<Result<(), String>>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn random_b64(n: usize) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut bytes = Vec::new();
    while bytes.len() < n {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        bytes.extend_from_slice(&h.finish().to_le_bytes());
    }
    bytes.truncate(n);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn client(ctx: &Ctx) -> Option<(String, String)> {
    let id = std::env::var("JEEM_GOOGLE_CLIENT_ID").ok().or_else(|| shell::get(ctx, "googleClientId").as_str().map(String::from))?;
    let secret = std::env::var("JEEM_GOOGLE_CLIENT_SECRET").ok().or_else(|| shell::get(ctx, "googleClientSecret").as_str().map(String::from)).unwrap_or_default();
    if id.trim().is_empty() { None } else { Some((id.trim().to_string(), secret.trim().to_string())) }
}

fn redirect(ctx: &Ctx) -> String {
    format!("http://127.0.0.1:{}/api/google-callback", ctx.port)
}

async fn token_request(ctx: &Ctx, mut form: Vec<(&str, String)>) -> Result<Value, String> {
    let (id, secret) = client(ctx).ok_or(MISSING)?;
    form.push(("client_id", id));
    if !secret.is_empty() {
        form.push(("client_secret", secret));
    }
    let r = ctx.client.post("https://oauth2.googleapis.com/token").form(&form).timeout(Duration::from_secs(30)).send().await.map_err(|e| e.to_string())?;
    let status = r.status();
    let data: Value = r.json().await.unwrap_or(Value::Null);
    if status.is_success() && data.get("access_token").is_some() {
        Ok(data)
    } else {
        let d = data.get("error_description").or_else(|| data.get("error")).and_then(Value::as_str).map(String::from).unwrap_or_else(|| format!("HTTP {status}"));
        Err(format!("Google token request failed: {d}"))
    }
}

fn save_tokens(ctx: &Ctx, data: &Value) {
    let mut auth = shell::get(ctx, "googleAuth");
    if !auth.is_object() {
        auth = json!({});
    }
    auth["accessToken"] = data["access_token"].clone();
    auth["expiresAt"] = json!(now_ms() + data.get("expires_in").and_then(Value::as_u64).unwrap_or(3600) * 1000);
    if let Some(r) = data.get("refresh_token") {
        auth["refreshToken"] = r.clone();
    }
    shell::set(ctx, "googleAuth", auth);
}

pub async fn login(ctx: &Ctx) -> Value {
    let Some((client_id, _)) = client(ctx) else { return json!({ "success": false, "error": MISSING }) };
    let verifier = random_b64(32);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_b64(32);
    *ctx.google.lock().unwrap() = Pending { verifier, state: state.clone(), done: None };
    let url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&code_challenge={}&code_challenge_method=S256&access_type=offline&prompt={}",
        urlencode(&client_id), urlencode(&redirect(ctx)), urlencode(SCOPE), urlencode(&state), challenge, urlencode("select_account consent")
    );
    if ctx.app.opener().open_url(url, None::<&str>).is_err() {
        return json!({ "success": false, "error": "Could not open the browser" });
    }
    for _ in 0..360 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Some(done) = ctx.google.lock().unwrap().done.clone() {
            return match done {
                Ok(()) => json!({ "success": true, "loggedIn": true }),
                Err(e) => json!({ "success": false, "loggedIn": false, "error": e }),
            };
        }
    }
    json!({ "success": false, "error": "Google sign-in timed out. Complete the authorization in your browser and try again." })
}

pub async fn callback(ctx: &Ctx, q: &HashMap<String, String>) -> (u16, String) {
    let page = |m: &str| format!("<!doctype html><title>Jeem</title><p>{m}</p>");
    let (state, verifier) = {
        let p = ctx.google.lock().unwrap();
        (p.state.clone(), p.verifier.clone())
    };
    if state.is_empty() || q.get("state") != Some(&state) {
        return (400, page("Authorization could not be verified. Return to Jeem and try again."));
    }
    let finish = |r: Result<(), String>| ctx.google.lock().unwrap().done = Some(r);
    if let Some(err) = q.get("error") {
        finish(Err(if err == "access_denied" { "Google sign-in was cancelled".into() } else { format!("Google authorization failed: {err}") }));
        return (200, page("Google sign-in was cancelled. You can close this tab."));
    }
    let Some(code) = q.get("code") else {
        finish(Err("Google did not return an authorization code".into()));
        return (400, page("Google did not return an authorization code."));
    };
    let form = vec![("code", code.clone()), ("code_verifier", verifier), ("grant_type", "authorization_code".to_string()), ("redirect_uri", redirect(ctx))];
    match token_request(ctx, form).await {
        Ok(data) if data.get("refresh_token").is_some() => {
            save_tokens(ctx, &data);
            finish(Ok(()));
            (200, page("Google account connected. You can close this tab and return to Jeem."))
        }
        Ok(_) => {
            finish(Err("Google did not provide a refresh token. Revoke Jeem's access in your Google account and try again.".into()));
            (400, page("Sign-in incomplete. Return to Jeem."))
        }
        Err(e) => {
            finish(Err(e));
            (400, page("Google sign-in failed. Return to Jeem for details."))
        }
    }
}

pub fn status(ctx: &Ctx) -> Value {
    let a = shell::get(ctx, "googleAuth");
    json!({ "loggedIn": a.get("refreshToken").and_then(Value::as_str).is_some_and(|r| !r.is_empty()) })
}

pub async fn logout(ctx: &Ctx) -> Value {
    let a = shell::get(ctx, "googleAuth");
    shell::set(ctx, "googleAuth", Value::Null);
    let mut warning = "";
    if let Some(t) = a.get("refreshToken").and_then(Value::as_str) {
        let ok = ctx.client.post("https://oauth2.googleapis.com/revoke").form(&[("token", t)]).timeout(Duration::from_secs(10)).send().await.map(|r| r.status().is_success()).unwrap_or(false);
        if !ok {
            warning = "Signed out locally, but Google did not confirm token revocation.";
        }
    }
    json!({ "success": true, "warning": warning })
}

async fn access_token(ctx: &Ctx) -> Result<String, String> {
    let a = shell::get(ctx, "googleAuth");
    let refresh = a.get("refreshToken").and_then(Value::as_str).unwrap_or("").to_string();
    if refresh.is_empty() {
        return Err("Sign in to Google before importing your YouTube library".into());
    }
    if let (Some(t), Some(exp)) = (a.get("accessToken").and_then(Value::as_str), a.get("expiresAt").and_then(Value::as_u64)) {
        if exp > now_ms() + 60_000 {
            return Ok(t.to_string());
        }
    }
    let data = token_request(ctx, vec![("refresh_token", refresh), ("grant_type", "refresh_token".to_string())]).await?;
    save_tokens(ctx, &data);
    Ok(data["access_token"].as_str().unwrap_or("").to_string())
}

fn thumb(t: Option<&Value>) -> String {
    for k in ["maxres", "standard", "high", "medium", "default"] {
        if let Some(u) = t.and_then(|t| t.pointer(&format!("/{k}/url"))).and_then(Value::as_str) {
            return u.to_string();
        }
    }
    String::new()
}

fn iso_duration(v: &str) -> u64 {
    let Some(rest) = v.strip_prefix("PT") else { return 0 };
    let (mut total, mut num) = (0u64, String::new());
    for c in rest.chars() {
        match c {
            '0'..='9' => num.push(c),
            'H' => { total += num.parse::<u64>().unwrap_or(0) * 3600; num.clear() }
            'M' => { total += num.parse::<u64>().unwrap_or(0) * 60; num.clear() }
            'S' => { total += num.parse::<u64>().unwrap_or(0); num.clear() }
            _ => {}
        }
    }
    total
}

async fn list_all(ctx: &Ctx, token: &str, path: &str, params: &str) -> Result<(Vec<Value>, u64), String> {
    let mut items = Vec::new();
    let mut page = String::new();
    let mut total = 0;
    for _ in 0..200 {
        let url = format!("{API}/{path}?{params}{}", if page.is_empty() { String::new() } else { format!("&pageToken={}", urlencode(&page)) });
        let r = ctx.client.get(&url).bearer_auth(token).timeout(Duration::from_secs(30)).send().await.map_err(|e| e.to_string())?;
        let ok = r.status().is_success();
        let data: Value = r.json().await.unwrap_or(Value::Null);
        if !ok {
            return Err(data.pointer("/error/message").and_then(Value::as_str).unwrap_or("YouTube API request failed").to_string());
        }
        items.extend(data.get("items").and_then(Value::as_array).cloned().unwrap_or_default());
        total = total.max(data.pointer("/pageInfo/totalResults").and_then(Value::as_u64).unwrap_or(0));
        page = data.get("nextPageToken").and_then(Value::as_str).unwrap_or("").to_string();
        if page.is_empty() {
            break;
        }
    }
    Ok((items, total))
}

fn map_track(id: &str, snippet: &Value, platform: &str, duration: u64) -> Value {
    let cover = thumb(snippet.get("thumbnails"));
    let artist = ["videoOwnerChannelTitle", "channelTitle"].iter().filter_map(|k| snippet.get(*k).and_then(Value::as_str)).find(|s| !s.is_empty()).unwrap_or("");
    json!({
        "title": snippet.get("title").and_then(Value::as_str).unwrap_or("untitled"), "artist": artist, "duration": duration,
        "id": id, "url": format!("https://www.youtube.com/watch?v={id}"), "platform": platform, "thumbnail": cover, "cover": cover
    })
}

pub async fn import_library(ctx: &Ctx, opts: &Value) -> Value {
    let platform = if opts.get("platform").and_then(Value::as_str) == Some("ytmusic") { "ytmusic" } else { "youtube" };
    let do_likes = opts.get("doLikes").and_then(Value::as_bool) != Some(false);
    let do_playlists = opts.get("doPlaylists").and_then(Value::as_bool) != Some(false);
    let result: Result<Value, String> = async {
        let token = access_token(ctx).await?;
        let mut likes = Vec::new();
        let mut total_likes = 0;
        if do_likes {
            let (items, total) = list_all(ctx, &token, "videos", "part=snippet,contentDetails&myRating=like&maxResults=50").await?;
            total_likes = total;
            for v in &items {
                if let (Some(id), Some(sn)) = (v.get("id").and_then(Value::as_str), v.get("snippet")) {
                    likes.push(map_track(id, sn, platform, iso_duration(v.pointer("/contentDetails/duration").and_then(Value::as_str).unwrap_or(""))));
                }
            }
        }
        let mut playlists = Vec::new();
        if do_playlists {
            let (lists, _) = list_all(ctx, &token, "playlists", "part=snippet,contentDetails&mine=true&maxResults=50").await?;
            for pl in lists {
                let (Some(id), Some(sn)) = (pl.get("id").and_then(Value::as_str), pl.get("snippet")) else { continue };
                let (items, _) = list_all(ctx, &token, "playlistItems", &format!("part=snippet,contentDetails&playlistId={}&maxResults=50", urlencode(id))).await?;
                let tracks: Vec<Value> = items
                    .iter()
                    .filter_map(|i| {
                        let vid = i.pointer("/contentDetails/videoId").or_else(|| i.pointer("/snippet/resourceId/videoId")).and_then(Value::as_str)?;
                        Some(map_track(vid, i.get("snippet")?, platform, 0))
                    })
                    .collect();
                playlists.push(json!({ "id": id, "name": sn.get("title").and_then(Value::as_str).unwrap_or("Playlist"), "cover": thumb(sn.get("thumbnails")), "tracks": tracks }));
            }
            // Durations for playlist tracks, 50 ids per request.
            let ids: Vec<String> = playlists.iter().flat_map(|p| p["tracks"].as_array().cloned().unwrap_or_default()).filter_map(|t| t["id"].as_str().map(String::from)).collect();
            let mut durations: HashMap<String, u64> = HashMap::new();
            for chunk in ids.chunks(50) {
                let (vids, _) = list_all(ctx, &token, "videos", &format!("part=contentDetails&id={}", urlencode(&chunk.join(",")))).await?;
                for v in vids {
                    if let Some(id) = v.get("id").and_then(Value::as_str) {
                        durations.insert(id.to_string(), iso_duration(v.pointer("/contentDetails/duration").and_then(Value::as_str).unwrap_or("")));
                    }
                }
            }
            for p in playlists.iter_mut() {
                if let Some(arr) = p["tracks"].as_array_mut() {
                    for t in arr {
                        let d = t["id"].as_str().and_then(|i| durations.get(i)).copied().unwrap_or(0);
                        t["duration"] = json!(d);
                    }
                }
            }
        }
        Ok(json!({
            "success": true, "likes": likes, "playlists": playlists,
            "warning": if total_likes > 1000 { "YouTube API only provides the most recent 1,000 liked videos." } else { "" }
        }))
    }
    .await;
    result.unwrap_or_else(|e| json!({ "success": false, "error": e }))
}
