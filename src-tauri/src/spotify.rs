// Spotify account (OAuth with PKCE, using YOUR Spotify app's client id): library import and full playlists.
// Redirect URI to register in the Spotify dashboard: http://127.0.0.1:38472/api/spotify-callback
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{Ctx, shell};

const SCOPES: &str = "user-library-read playlist-read-private playlist-read-collaborative";

#[derive(Default, Clone)]
pub struct Auth {
    pub verifier: String,
    pub state: String,
    pub client_id: String,
    pub token: String,
    pub refresh: String,
    pub expires_at: u64,
    pub error: String,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn random_bytes(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n + 8);
    while out.len() < n {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        out.extend_from_slice(&h.finish().to_le_bytes());
    }
    out.truncate(n);
    out
}

pub fn redirect_uri(ctx: &Ctx) -> String {
    format!("http://127.0.0.1:{}/api/spotify-callback", ctx.port)
}

pub fn load(ctx: &Ctx) {
    let saved = shell::get(ctx, "spotifyAuth");
    let mut a = ctx.spotify.lock().unwrap();
    if a.client_id.is_empty() {
        a.client_id = saved.get("clientId").and_then(Value::as_str).unwrap_or("").to_string();
        a.refresh = saved.get("refreshToken").and_then(Value::as_str).unwrap_or("").to_string();
    }
}

fn persist(ctx: &Ctx) {
    let a = ctx.spotify.lock().unwrap().clone();
    shell::set(ctx, "spotifyAuth", if a.refresh.is_empty() { Value::Null } else { json!({ "clientId": a.client_id, "refreshToken": a.refresh }) });
}

pub fn auth_start(ctx: &Ctx, client_id: &str) -> (u16, Value) {
    if client_id.len() != 32 || !client_id.chars().all(|c| c.is_ascii_hexdigit()) {
        return (400, json!({ "success": false, "error": "Invalid Client ID" }));
    }
    let verifier = URL_SAFE_NO_PAD.encode(random_bytes(48));
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = URL_SAFE_NO_PAD.encode(random_bytes(16));
    *ctx.spotify.lock().unwrap() = Auth { verifier, state: state.clone(), client_id: client_id.to_string(), ..Default::default() };
    let redirect = redirect_uri(ctx);
    let url = format!(
        "https://accounts.spotify.com/authorize?response_type=code&client_id={}&scope={}&redirect_uri={}&state={}&code_challenge_method=S256&code_challenge={}",
        crate::ytm::urlencode(client_id),
        crate::ytm::urlencode(SCOPES),
        crate::ytm::urlencode(&redirect),
        crate::ytm::urlencode(&state),
        challenge
    );
    (200, json!({ "success": true, "url": url, "redirectUri": redirect }))
}

async fn token_request(ctx: &Ctx, form: &[(&str, String)]) -> Result<Value, String> {
    let r = ctx
        .client
        .post("https://accounts.spotify.com/api/token")
        .form(form)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = r.status();
    let data: Value = r.json().await.unwrap_or(Value::Null);
    if status.is_success() {
        Ok(data)
    } else {
        Err(data.get("error_description").or_else(|| data.get("error")).and_then(Value::as_str).map(String::from).unwrap_or_else(|| format!("HTTP {status}")))
    }
}

fn store_tokens(ctx: &Ctx, data: &Value) {
    let mut a = ctx.spotify.lock().unwrap();
    a.token = data.get("access_token").and_then(Value::as_str).unwrap_or("").to_string();
    if let Some(r) = data.get("refresh_token").and_then(Value::as_str) {
        a.refresh = r.to_string();
    }
    let secs = data.get("expires_in").and_then(Value::as_u64).filter(|s| *s > 0).unwrap_or(3600);
    a.expires_at = now_ms() + secs * 1000;
}

pub async fn callback(ctx: &Ctx, q: &HashMap<String, String>) -> (u16, String) {
    let page = |m: &str| format!("<!DOCTYPE html><html><body><p>{m}</p></body></html>");
    let (state, client_id, verifier) = {
        let a = ctx.spotify.lock().unwrap();
        (a.state.clone(), a.client_id.clone(), a.verifier.clone())
    };
    if state.is_empty() || q.get("state") != Some(&state) {
        return (400, page("Invalid Spotify login session."));
    }
    ctx.spotify.lock().unwrap().state.clear();
    let Some(code) = q.get("code").filter(|_| !q.contains_key("error")) else {
        ctx.spotify.lock().unwrap().error = "Spotify authorization was denied or failed.".into();
        return (400, page("Spotify login failed. Return to Jeem for details."));
    };
    let form = [
        ("grant_type", "authorization_code".to_string()),
        ("code", code.clone()),
        ("redirect_uri", redirect_uri(ctx)),
        ("client_id", client_id),
        ("code_verifier", verifier),
    ];
    match token_request(ctx, &form).await {
        Ok(data) => {
            store_tokens(ctx, &data);
            persist(ctx);
            (200, page("Jeem is connected to Spotify. You can close this tab."))
        }
        Err(e) => {
            ctx.spotify.lock().unwrap().error = format!("Spotify token exchange failed: {e}");
            (400, page("Spotify login failed. Return to Jeem for details."))
        }
    }
}

// A valid access token, refreshing it when it has expired.
pub async fn access_token(ctx: &Ctx) -> Result<String, String> {
    load(ctx);
    let (token, expires, refresh, client_id) = {
        let a = ctx.spotify.lock().unwrap();
        (a.token.clone(), a.expires_at, a.refresh.clone(), a.client_id.clone())
    };
    if !token.is_empty() && now_ms() + 30_000 < expires {
        return Ok(token);
    }
    if refresh.is_empty() || client_id.is_empty() {
        return Err("Not connected to Spotify".into());
    }
    let form = [("grant_type", "refresh_token".to_string()), ("refresh_token", refresh), ("client_id", client_id)];
    let data = token_request(ctx, &form).await?;
    store_tokens(ctx, &data);
    persist(ctx);
    Ok(ctx.spotify.lock().unwrap().token.clone())
}

pub async fn status(ctx: &Ctx) -> Value {
    load(ctx);
    let err = match access_token(ctx).await {
        Ok(_) => String::new(),
        Err(e) => if e == "Not connected to Spotify" { String::new() } else { e },
    };
    let a = ctx.spotify.lock().unwrap().clone();
    let connected = !a.token.is_empty() && now_ms() < a.expires_at;
    json!({ "connected": connected, "error": if err.is_empty() { a.error } else { err } })
}

pub fn logout(ctx: &Ctx) {
    *ctx.spotify.lock().unwrap() = Auth::default();
    shell::set(ctx, "spotifyAuth", Value::Null);
}

async fn api_get(ctx: &Ctx, token: &str, url: &str) -> Result<Value, (u16, String)> {
    for attempt in 0..3 {
        let r = ctx
            .client
            .get(url)
            .bearer_auth(token)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| (502, e.to_string()))?;
        let status = r.status().as_u16();
        if status == 429 && attempt < 2 {
            let wait = r.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).unwrap_or(1).min(10);
            tokio::time::sleep(Duration::from_secs(wait)).await;
            continue;
        }
        if !(200..300).contains(&status) {
            return Err((status, format!("Spotify API error (HTTP {status})")));
        }
        return r.json::<Value>().await.map_err(|e| (502, e.to_string()));
    }
    Err((429, "Spotify rate limited".into()))
}

fn track_of(it: &Value) -> Option<Value> {
    let t = it.get("item").or_else(|| it.get("track"))?;
    if t.is_null() || t.get("is_local").and_then(Value::as_bool) == Some(true) || t.get("type").and_then(Value::as_str) == Some("episode") {
        return None;
    }
    let title = t.get("name").and_then(Value::as_str).unwrap_or("");
    if title.is_empty() {
        return None;
    }
    let artist = t.pointer("/artists/0/name").and_then(Value::as_str).unwrap_or("");
    let dur = t.get("duration_ms").and_then(Value::as_f64).map(|d| (d / 1000.0).round());
    Some(json!({ "title": title, "artist": artist, "duration": dur }))
}

async fn all_tracks(ctx: &Ctx, token: &str, first: &str, cap: usize) -> Result<Vec<Value>, (u16, String)> {
    let mut out = Vec::new();
    let mut next = Some(first.to_string());
    while let Some(url) = next {
        let data = api_get(ctx, token, &url).await?;
        for it in data.get("items").and_then(Value::as_array).cloned().unwrap_or_default() {
            if let Some(t) = track_of(&it) {
                out.push(t);
            }
        }
        if out.len() >= cap {
            break;
        }
        next = data.get("next").and_then(Value::as_str).map(String::from);
    }
    Ok(out)
}

pub async fn library(ctx: &Ctx, likes: bool, playlists: bool) -> (u16, Value) {
    let token = match access_token(ctx).await {
        Ok(t) => t,
        Err(_) => return (401, json!({ "success": false, "error": "Unauthorized" })),
    };
    let fail = |(s, m): (u16, String)| (if s == 403 || s == 429 { s } else { 500 }, json!({ "success": false, "error": m }));
    let mut likes_out = Vec::new();
    let mut playlists_out = Vec::new();
    let mut skipped = Vec::new();
    if likes {
        match all_tracks(ctx, &token, "https://api.spotify.com/v1/me/tracks?limit=50", 5000).await {
            Ok(t) => likes_out = t,
            Err(e) => return fail(e),
        }
    }
    if playlists {
        let mut list = Vec::new();
        let mut next = Some("https://api.spotify.com/v1/me/playlists?limit=50".to_string());
        while let Some(url) = next {
            let data = match api_get(ctx, &token, &url).await { Ok(d) => d, Err(e) => return fail(e) };
            for pl in data.get("items").and_then(Value::as_array).cloned().unwrap_or_default() {
                if let (Some(id), Some(name)) = (pl.get("id").and_then(Value::as_str), pl.get("name").and_then(Value::as_str)) {
                    list.push((id.to_string(), name.to_string()));
                }
            }
            next = data.get("next").and_then(Value::as_str).map(String::from);
        }
        for (id, name) in list {
            let url = format!("https://api.spotify.com/v1/playlists/{id}/items?limit=50");
            let res = match all_tracks(ctx, &token, &url, usize::MAX).await {
                Err((400 | 404, _)) => all_tracks(ctx, &token, &format!("https://api.spotify.com/v1/playlists/{id}/tracks?limit=50"), usize::MAX).await,
                other => other,
            };
            match res {
                Ok(tracks) => playlists_out.push(json!({ "name": name, "tracks": tracks })),
                Err((403, m)) => skipped.push(json!({ "name": name, "error": m })),
                Err(e) => return fail(e),
            }
        }
    }
    (200, json!({ "success": true, "partial": !skipped.is_empty(), "likes": likes_out, "playlists": playlists_out, "skippedPlaylists": skipped }))
}

// Full playlist through the API (when connected); used by the playlist import before the public page fallback.
pub async fn playlist(ctx: &Ctx, id: &str) -> Option<Value> {
    let token = access_token(ctx).await.ok()?;
    let info = api_get(ctx, &token, &format!("https://api.spotify.com/v1/playlists/{id}?fields=name,images")).await.ok()?;
    let mut tracks = all_tracks(ctx, &token, &format!("https://api.spotify.com/v1/playlists/{id}/tracks?limit=100"), usize::MAX).await.ok()?;
    if tracks.is_empty() {
        return None;
    }
    let total = tracks.len();
    for t in tracks.iter_mut() {
        t["thumbnail"] = json!("");
    }
    Some(json!({
        "success": true, "name": info.get("name").and_then(Value::as_str).unwrap_or("Spotify Playlist"),
        "cover": info.pointer("/images/0/url").and_then(Value::as_str).unwrap_or(""),
        "tracks": tracks, "hasMore": false, "total": total, "limited": false
    }))
}
