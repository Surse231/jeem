// Optional AI help for "My Vibe": a free Gemini or Groq key of the listener re-orders the candidate tracks.
// Nothing is sent anywhere unless the listener entered a key in the My Vibe settings.
use std::time::Duration;

use serde_json::{Value, json};

use crate::Ctx;

fn s<'a>(v: Option<&'a Value>) -> &'a str {
    v.and_then(Value::as_str).unwrap_or("")
}

/// Asks the model and returns its answer as JSON.
async fn ask(ctx: &Ctx, ai: &Value, prompt: &str) -> Result<Value, String> {
    let provider = s(ai.get("provider"));
    let key = s(ai.get("key")).trim();
    if key.is_empty() {
        return Err("no key".into());
    }
    let text = match provider {
        "groq" => {
            let body = json!({
                "model": "llama-3.3-70b-versatile",
                "messages": [{ "role": "user", "content": prompt }],
                "response_format": { "type": "json_object" },
                "temperature": 0.4
            });
            let resp = ctx
                .client
                .post("https://api.groq.com/openai/v1/chat/completions")
                .bearer_auth(key)
                .timeout(Duration::from_secs(12))
                .json(&body)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            let root: Value = resp.json().await.map_err(|e| e.to_string())?;
            if !status.is_success() {
                return Err(s(root.pointer("/error/message")).to_string());
            }
            s(root.pointer("/choices/0/message/content")).to_string()
        }
        _ => {
            // The free Gemini models change names from time to time, so a few are tried in turn. The small "lite" models answer in 1-2 seconds, the bigger ones can take a minute.
            let mut last = String::from("no answer");
            let mut out = String::new();
            for model in ["gemini-flash-lite-latest", "gemini-3.5-flash-lite", "gemini-3.1-flash-lite", "gemini-2.5-flash-lite"] {
                let url = format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent");
                let body = json!({
                    "contents": [{ "parts": [{ "text": prompt }] }],
                    "generationConfig": { "responseMimeType": "application/json", "temperature": 0.4 }
                });
                let resp = ctx
                    .client
                    .post(url)
                    .header("x-goog-api-key", key)
                    .timeout(Duration::from_secs(10))
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                let status = resp.status();
                let root: Value = resp.json().await.map_err(|e| e.to_string())?;
                if status.is_success() {
                    out = s(root.pointer("/candidates/0/content/parts/0/text")).to_string();
                    break;
                }
                last = s(root.pointer("/error/message")).to_string();
                // a model that this key cannot use (not found, no access) is skipped; any other error ends the try
                if !matches!(status.as_u16(), 403 | 404) {
                    return Err(last);
                }
            }
            if out.is_empty() {
                return Err(last);
            }
            out
        }
    };
    let cleaned = text.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
    serde_json::from_str(cleaned).map_err(|e| format!("bad answer: {e}"))
}

/// Indices of `cands` in the order that fits the listener best (tracks that clearly do not fit are left out).
pub async fn rerank(ctx: &Ctx, ai: &Value, liked: &[String], disliked: &[String], cands: &[Value], settings: &Value) -> Option<Vec<usize>> {
    if cands.len() < 6 {
        return None;
    }
    let mut prompt = String::from(
        "You choose the next songs for a listener's endless music stream.\nTracks the listener LIKED:\n",
    );
    for l in liked {
        prompt.push_str(&format!("- {l}\n"));
    }
    if !disliked.is_empty() {
        prompt.push_str("Tracks and artists the listener DISLIKED (avoid similar ones):\n");
        for d in disliked {
            prompt.push_str(&format!("- {d}\n"));
        }
    }
    let wanted: Vec<String> = ["activity", "mood", "language", "familiarity"]
        .iter()
        .filter_map(|k| {
            let v = s(settings.get(*k));
            if v.is_empty() { None } else { Some(format!("{k}: {v}")) }
        })
        .collect();
    if !wanted.is_empty() {
        prompt.push_str(&format!("Requested by the listener: {}\n", wanted.join(", ")));
    }
    prompt.push_str("\nCandidates:\n");
    for (i, c) in cands.iter().enumerate() {
        prompt.push_str(&format!("{i}: {} - {}\n", s(c.get("artist")), s(c.get("title"))));
    }
    prompt.push_str(
        "\nReturn only JSON like {\"order\":[3,0,7],\"drop\":[5]}. \"order\" lists up to 25 candidate numbers, the best fit for this listener first, \
         with some variety (not many songs by one artist). \"drop\" lists numbers that clearly do not fit. Use only the numbers above.",
    );
    let answer = ask(ctx, ai, &prompt).await.ok()?;
    let drop: Vec<usize> = answer.get("drop").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_u64).map(|n| n as usize).collect();
    let order: Vec<usize> = answer
        .get("order")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(Value::as_u64)
        .map(|n| n as usize)
        .filter(|i| *i < cands.len() && !drop.contains(i))
        .collect();
    if order.is_empty() { None } else { Some(order) }
}

/// For the "Check" button next to the key: does the key work?
pub async fn test(ctx: &Ctx, ai: &Value) -> Value {
    match ask(ctx, ai, "Reply with this JSON and nothing else: {\"ok\":true}").await {
        Ok(_) => json!({ "success": true }),
        Err(e) => json!({ "success": false, "error": e }),
    }
}
