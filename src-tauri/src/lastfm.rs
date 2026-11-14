// Last.fm scrobbling. Uses the user's own API key and secret (nothing is bundled).
use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Value, json};

use crate::Ctx;

const MISSING_KEY: &str = "Add your own Last.fm API key and secret in the settings (create them at last.fm/api/account/create)";

fn sign(params: &BTreeMap<String, String>, secret: &str) -> String {
    let mut s = String::new();
    for (k, v) in params {
        if k != "format" && k != "callback" {
            s.push_str(k);
            s.push_str(v);
        }
    }
    s.push_str(secret);
    format!("{:x}", md5::compute(s.as_bytes()))
}

async fn call(ctx: &Ctx, mut params: BTreeMap<String, String>, secret: &str, post: bool) -> Value {
    let sig = sign(&params, secret);
    params.insert("api_sig".into(), sig);
    params.insert("format".into(), "json".into());
    let url = "https://ws.audioscrobbler.com/2.0/";
    let req = if post { ctx.client.post(url).form(&params) } else { ctx.client.get(url).query(&params) };
    match req.header("User-Agent", "Jeem").timeout(Duration::from_secs(10)).send().await {
        Ok(r) => r.json::<Value>().await.unwrap_or_else(|_| json!({ "error": 1, "message": "Invalid Last.fm response" })),
        Err(e) => json!({ "error": 1, "message": e.to_string() }),
    }
}

fn p(pairs: &[(&str, String)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

fn text(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

pub async fn route(ctx: &Ctx, path: &str, body: &Value) -> Value {
    let key = text(body, "apiKey").trim().to_string();
    let secret = text(body, "apiSecret").trim().to_string();
    if key.is_empty() || secret.is_empty() {
        return json!({ "success": false, "error": MISSING_KEY });
    }
    let session_from = |r: &Value| {
        let sk = r.pointer("/session/key").and_then(Value::as_str).unwrap_or("").to_string();
        json!({
            "success": !sk.is_empty(), "sessionKey": sk,
            "username": r.pointer("/session/name").and_then(Value::as_str).unwrap_or(""),
            "error": r.get("message").and_then(Value::as_str).unwrap_or("")
        })
    };
    match path {
        "/api/lastfm/token" => {
            let r = call(ctx, p(&[("method", "auth.getToken".into()), ("api_key", key.clone())]), &secret, false).await;
            let token = r.get("token").and_then(Value::as_str).unwrap_or("").to_string();
            json!({
                "success": !token.is_empty(), "token": token,
                "authUrl": if token.is_empty() { String::new() } else { format!("https://www.last.fm/api/auth/?api_key={key}&token={token}") },
                "error": r.get("message").and_then(Value::as_str).unwrap_or("")
            })
        }
        "/api/lastfm/session" => {
            let r = call(ctx, p(&[("method", "auth.getSession".into()), ("api_key", key), ("token", text(body, "token"))]), &secret, false).await;
            session_from(&r)
        }
        "/api/lastfm/mobile-session" => {
            let params = p(&[
                ("method", "auth.getMobileSession".into()),
                ("api_key", key),
                ("username", text(body, "username").trim().to_string()),
                ("password", text(body, "password")),
            ]);
            session_from(&call(ctx, params, &secret, true).await)
        }
        "/api/lastfm/now-playing" | "/api/lastfm/scrobble" => {
            let (sk, artist, track) = (text(body, "sk"), text(body, "artist"), text(body, "track"));
            if sk.is_empty() || artist.is_empty() || track.is_empty() {
                return json!({ "success": false, "error": "Missing sk/artist/track" });
            }
            let dur = body.get("duration").and_then(Value::as_f64).filter(|d| *d > 0.0).map(|d| d.round().to_string());
            let params = if path.ends_with("now-playing") {
                let mut m = p(&[("method", "track.updateNowPlaying".into()), ("api_key", key), ("sk", sk), ("artist", artist), ("track", track)]);
                if !text(body, "album").is_empty() {
                    m.insert("album".into(), text(body, "album"));
                }
                if let Some(d) = dur {
                    m.insert("duration".into(), d);
                }
                m
            } else {
                let ts = body
                    .get("timestamp")
                    .map(|t| t.to_string().trim_matches('"').to_string())
                    .filter(|t| !t.is_empty() && t != "null")
                    .unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()).to_string());
                let mut m = p(&[
                    ("method", "track.scrobble".into()), ("api_key", key), ("sk", sk),
                    ("artist[0]", artist), ("track[0]", track), ("timestamp[0]", ts),
                ]);
                if !text(body, "album").is_empty() {
                    m.insert("album[0]".into(), text(body, "album"));
                }
                if let Some(d) = dur {
                    m.insert("duration[0]".into(), d);
                }
                m
            };
            let r = call(ctx, params, &secret, true).await;
            json!({ "success": r.get("error").is_none(), "data": r, "error": r.get("message").and_then(Value::as_str).unwrap_or("") })
        }
        _ => json!({ "success": false, "error": "unknown route" }),
    }
}
