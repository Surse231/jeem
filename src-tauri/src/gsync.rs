// Account sync through the user's own Google Drive (hidden "appDataFolder", scope drive.appdata).
// Sign-in is the official OAuth flow in the system browser (PKCE + loopback redirect).
// The OAuth client (type "Desktop app") comes from, in order: environment variables, build-time
// variables JEEM_GOOGLE_CLIENT_ID / JEEM_GOOGLE_CLIENT_SECRET, or googleClientId / googleClientSecret in settings.json.
use std::collections::HashMap;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tauri_plugin_opener::OpenerExt;

use crate::google::Pending;
use crate::ytm::urlencode;
use crate::{Ctx, shell};

const SCOPE: &str = "openid email profile https://www.googleapis.com/auth/drive.appdata";
const FILE_NAME: &str = "jeem-account.json";
const MISSING: &str = "Google sign-in is not configured in this build (no OAuth client).";

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
    let pick = |env: &str, built: Option<&'static str>, key: &str| -> Option<String> {
        std::env::var(env)
            .ok()
            .or_else(|| built.map(String::from))
            .or_else(|| shell::get(ctx, key).as_str().map(String::from))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let id = pick("JEEM_GOOGLE_CLIENT_ID", option_env!("JEEM_GOOGLE_CLIENT_ID"), "googleClientId")?;
    let secret = pick("JEEM_GOOGLE_CLIENT_SECRET", option_env!("JEEM_GOOGLE_CLIENT_SECRET"), "googleClientSecret").unwrap_or_default();
    Some((id, secret))
}

fn redirect(ctx: &Ctx) -> String {
    format!("http://127.0.0.1:{}/api/gsync-callback", ctx.port)
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
    let mut auth = shell::get(ctx, "gsyncAuth");
    if !auth.is_object() {
        auth = json!({});
    }
    auth["accessToken"] = data["access_token"].clone();
    auth["expiresAt"] = json!(now_ms() + data.get("expires_in").and_then(Value::as_u64).unwrap_or(3600) * 1000);
    if let Some(r) = data.get("refresh_token") {
        auth["refreshToken"] = r.clone();
    }
    shell::set(ctx, "gsyncAuth", auth);
}

async fn save_profile(ctx: &Ctx, access: &str) {
    let r = ctx.client.get("https://openidconnect.googleapis.com/v1/userinfo").bearer_auth(access).timeout(Duration::from_secs(15)).send().await;
    if let Ok(resp) = r {
        if let Ok(v) = resp.json::<Value>().await {
            let mut auth = shell::get(ctx, "gsyncAuth");
            if auth.is_object() {
                auth["email"] = v.get("email").cloned().unwrap_or(Value::Null);
                auth["name"] = v.get("name").cloned().unwrap_or(Value::Null);
                auth["picture"] = v.get("picture").cloned().unwrap_or(Value::Null);
                shell::set(ctx, "gsyncAuth", auth);
            }
        }
    }
}

pub fn configured(ctx: &Ctx) -> bool {
    client(ctx).is_some()
}

pub async fn login(ctx: &Ctx) -> Value {
    let Some((client_id, _)) = client(ctx) else { return json!({ "success": false, "error": MISSING }) };
    let verifier = random_b64(32);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_b64(32);
    *ctx.gsync.lock().unwrap() = Pending { verifier, state: state.clone(), done: None };
    let url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&code_challenge={}&code_challenge_method=S256&access_type=offline&prompt={}",
        urlencode(&client_id), urlencode(&redirect(ctx)), urlencode(SCOPE), urlencode(&state), challenge, urlencode("select_account consent")
    );
    if ctx.app.opener().open_url(url, None::<&str>).is_err() {
        return json!({ "success": false, "error": "Could not open the browser" });
    }
    for _ in 0..360 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Some(done) = ctx.gsync.lock().unwrap().done.clone() {
            return match done {
                Ok(()) => {
                    let mut st = status(ctx);
                    st["success"] = json!(true);
                    st
                }
                Err(e) => json!({ "success": false, "loggedIn": false, "error": e }),
            };
        }
    }
    json!({ "success": false, "error": "Google sign-in timed out. Complete the authorization in your browser and try again." })
}

pub async fn callback(ctx: &Ctx, q: &HashMap<String, String>) -> (u16, String) {
    let page = |m: &str| format!("<!doctype html><meta charset=\"utf-8\"><title>Jeem</title><body style=\"font-family:sans-serif;background:#111;color:#eee;display:flex;align-items:center;justify-content:center;height:100vh\"><p>{m}</p>");
    let (state, verifier) = {
        let p = ctx.gsync.lock().unwrap();
        (p.state.clone(), p.verifier.clone())
    };
    if state.is_empty() || q.get("state") != Some(&state) {
        return (400, page("Authorization could not be verified. Return to Jeem and try again."));
    }
    let finish = |r: Result<(), String>| ctx.gsync.lock().unwrap().done = Some(r);
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
        Ok(data) if !data.get("scope").and_then(Value::as_str).unwrap_or("").contains("auth/drive.appdata") => {
            finish(Err("Google did not grant access to Jeem's hidden Drive folder. Sign in again and tick the Google Drive permission.".into()));
            (400, page("The Google Drive permission was not granted. Return to Jeem and try again, ticking the Drive checkbox."))
        }
        Ok(data) if data.get("refresh_token").is_some() => {
            let access = data["access_token"].as_str().unwrap_or("").to_string();
            save_tokens(ctx, &data);
            save_profile(ctx, &access).await;
            finish(Ok(()));
            (200, page("Signed in. You can close this tab and return to Jeem."))
        }
        Ok(_) => {
            finish(Err("Google did not provide a refresh token. Remove Jeem from your Google account permissions and try again.".into()));
            (400, page("Sign-in incomplete. Return to Jeem."))
        }
        Err(e) => {
            finish(Err(e));
            (400, page("Google sign-in failed. Return to Jeem for details."))
        }
    }
}

pub fn status(ctx: &Ctx) -> Value {
    let a = shell::get(ctx, "gsyncAuth");
    let logged = a.get("refreshToken").and_then(Value::as_str).is_some_and(|r| !r.is_empty());
    json!({
        "configured": configured(ctx),
        "loggedIn": logged,
        "email": if logged { a.get("email").cloned().unwrap_or(Value::Null) } else { Value::Null },
        "name": if logged { a.get("name").cloned().unwrap_or(Value::Null) } else { Value::Null },
        "picture": if logged { a.get("picture").cloned().unwrap_or(Value::Null) } else { Value::Null }
    })
}

pub async fn logout(ctx: &Ctx) -> Value {
    let a = shell::get(ctx, "gsyncAuth");
    shell::set(ctx, "gsyncAuth", Value::Null);
    if let Some(t) = a.get("refreshToken").and_then(Value::as_str) {
        let _ = ctx.client.post("https://oauth2.googleapis.com/revoke").form(&[("token", t)]).timeout(Duration::from_secs(10)).send().await;
    }
    json!({ "success": true })
}

async fn access_token(ctx: &Ctx) -> Result<String, String> {
    let a = shell::get(ctx, "gsyncAuth");
    let refresh = a.get("refreshToken").and_then(Value::as_str).filter(|r| !r.is_empty()).ok_or("Not signed in to Google")?.to_string();
    let valid = a.get("accessToken").and_then(Value::as_str).filter(|t| !t.is_empty());
    let expires = a.get("expiresAt").and_then(Value::as_u64).unwrap_or(0);
    if let Some(t) = valid {
        if expires > now_ms() + 60_000 {
            return Ok(t.to_string());
        }
    }
    let data = token_request(ctx, vec![("grant_type", "refresh_token".to_string()), ("refresh_token", refresh)]).await.map_err(|e| {
        if e.contains("invalid_grant") {
            shell::set(ctx, "gsyncAuth", Value::Null);
            "Google access expired. Sign in again.".to_string()
        } else {
            e
        }
    })?;
    save_tokens(ctx, &data);
    Ok(data["access_token"].as_str().unwrap_or("").to_string())
}

struct Remote {
    id: String,
    modified: String,
}

async fn find_file(ctx: &Ctx, token: &str) -> Result<Option<Remote>, String> {
    let url = format!(
        "https://www.googleapis.com/drive/v3/files?spaces=appDataFolder&pageSize=1&fields={}&q={}",
        urlencode("files(id,modifiedTime)"),
        urlencode(&format!("name='{FILE_NAME}' and trashed=false"))
    );
    let r = ctx.client.get(url).bearer_auth(token).timeout(Duration::from_secs(20)).send().await.map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("Google Drive request failed (HTTP {})", r.status().as_u16()));
    }
    let v: Value = r.json().await.map_err(|e| e.to_string())?;
    Ok(v.pointer("/files/0").map(|f| Remote {
        id: f.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
        modified: f.get("modifiedTime").and_then(Value::as_str).unwrap_or("").to_string(),
    }))
}

/// Information about the cloud copy without downloading it.
pub async fn info(ctx: &Ctx) -> Value {
    let token = match access_token(ctx).await {
        Ok(t) => t,
        Err(e) => return json!({ "success": false, "error": e }),
    };
    match find_file(ctx, &token).await {
        Ok(Some(f)) => json!({ "success": true, "exists": true, "modifiedTime": f.modified }),
        Ok(None) => json!({ "success": true, "exists": false }),
        Err(e) => json!({ "success": false, "error": e }),
    }
}

pub async fn pull(ctx: &Ctx) -> Value {
    let token = match access_token(ctx).await {
        Ok(t) => t,
        Err(e) => return json!({ "success": false, "error": e }),
    };
    let remote = match find_file(ctx, &token).await {
        Ok(Some(f)) => f,
        Ok(None) => return json!({ "success": true, "exists": false }),
        Err(e) => return json!({ "success": false, "error": e }),
    };
    let url = format!("https://www.googleapis.com/drive/v3/files/{}?alt=media", remote.id);
    match ctx.client.get(url).bearer_auth(&token).timeout(Duration::from_secs(120)).send().await {
        Ok(r) if r.status().is_success() => match r.text().await {
            Ok(text) => json!({ "success": true, "exists": true, "data": text, "modifiedTime": remote.modified }),
            Err(e) => json!({ "success": false, "error": e.to_string() }),
        },
        Ok(r) => json!({ "success": false, "error": format!("Google Drive download failed (HTTP {})", r.status().as_u16()) }),
        Err(e) => json!({ "success": false, "error": e.to_string() }),
    }
}

pub async fn push(ctx: &Ctx, data: &str) -> Value {
    let token = match access_token(ctx).await {
        Ok(t) => t,
        Err(e) => return json!({ "success": false, "error": e }),
    };
    let remote = match find_file(ctx, &token).await {
        Ok(r) => r,
        Err(e) => return json!({ "success": false, "error": e }),
    };
    let resp = match remote {
        Some(f) => {
            let url = format!("https://www.googleapis.com/upload/drive/v3/files/{}?uploadType=media&fields=modifiedTime", f.id);
            ctx.client.patch(url).bearer_auth(&token).header("Content-Type", "application/json").body(data.to_string()).timeout(Duration::from_secs(120)).send().await
        }
        None => {
            let boundary = format!("jeem{}", random_b64(12));
            let meta = json!({ "name": FILE_NAME, "parents": ["appDataFolder"], "mimeType": "application/json" }).to_string();
            let body = format!(
                "--{b}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{meta}\r\n--{b}\r\nContent-Type: application/json\r\n\r\n{data}\r\n--{b}--",
                b = boundary
            );
            ctx.client
                .post("https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart&fields=modifiedTime")
                .bearer_auth(&token)
                .header("Content-Type", format!("multipart/related; boundary={boundary}"))
                .body(body)
                .timeout(Duration::from_secs(120))
                .send()
                .await
        }
    };
    match resp {
        Ok(r) if r.status().is_success() => {
            let v: Value = r.json().await.unwrap_or(Value::Null);
            json!({ "success": true, "modifiedTime": v.get("modifiedTime").cloned().unwrap_or(Value::Null) })
        }
        Ok(r) => json!({ "success": false, "error": format!("Google Drive upload failed (HTTP {})", r.status().as_u16()) }),
        Err(e) => json!({ "success": false, "error": e.to_string() }),
    }
}
