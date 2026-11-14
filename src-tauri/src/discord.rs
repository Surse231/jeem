// Discord Rich Presence over the local IPC named pipe. Needs your own Discord application id:
// set "discordClientId" in settings.json (or the JEEM_DISCORD_CLIENT_ID environment variable).
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{Value, json};

use crate::{Ctx, shell};

enum Msg {
    Update(Value),
    Clear,
    Reconnect,
}

struct Handle {
    tx: Mutex<Option<Sender<Msg>>>,
    enabled: AtomicBool,
    connected: Arc<AtomicBool>,
}

static HANDLE: OnceLock<Handle> = OnceLock::new();

fn handle() -> &'static Handle {
    HANDLE.get_or_init(|| Handle { tx: Mutex::new(None), enabled: AtomicBool::new(true), connected: Arc::new(AtomicBool::new(false)) })
}

fn frame(op: u32, payload: &Value) -> Vec<u8> {
    let body = payload.to_string().into_bytes();
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(&op.to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

fn read_frame(pipe: &mut File) -> std::io::Result<Value> {
    let mut head = [0u8; 8];
    pipe.read_exact(&mut head)?;
    let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
    let mut body = vec![0u8; len.min(1 << 20)];
    pipe.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body).unwrap_or(Value::Null))
}

fn connect(client_id: &str) -> Option<File> {
    for i in 0..10 {
        let Ok(mut pipe) = OpenOptions::new().read(true).write(true).open(format!(r"\\.\pipe\discord-ipc-{i}")) else { continue };
        if pipe.write_all(&frame(0, &json!({ "v": 1, "client_id": client_id }))).is_err() {
            continue;
        }
        if read_frame(&mut pipe).is_ok() {
            return Some(pipe);
        }
    }
    None
}

fn activity(info: &Value) -> Value {
    let text = |k: &str, d: &str| {
        let v = info.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(d);
        let mut s: String = v.chars().take(128).collect();
        while s.chars().count() < 2 {
            s.push(' ');
        }
        s
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let start = info.get("startTimestamp").and_then(Value::as_u64).filter(|s| *s > 0).unwrap_or(now);
    let artist_in_status = info.get("artistInStatus").and_then(Value::as_bool) != Some(false);
    let mut act = json!({
        "type": 2,
        "details": text("title", "Unknown Track"),
        "state": if artist_in_status { text("artist", "Unknown Artist") } else { format!("by {}", text("artist", "Unknown Artist")) },
        "timestamps": { "start": start },
        "instance": false
    });
    if let Some(t) = info.get("thumbnail").and_then(Value::as_str).filter(|t| !t.is_empty()) {
        act["assets"] = json!({ "large_image": t });
    }
    if let Some(d) = info.get("duration").and_then(Value::as_f64).filter(|d| *d > 0.0) {
        act["timestamps"]["end"] = json!(start + d.floor() as u64);
    }
    if let Some(u) = info.get("url").and_then(Value::as_str).filter(|u| u.starts_with("https://") && u.len() <= 512) {
        act["buttons"] = json!([{ "label": "Listen", "url": u }]);
    }
    if artist_in_status {
        act["status_display_type"] = json!(1);
    }
    act
}

fn set_activity(pipe: &mut File, act: Option<Value>) -> bool {
    let payload = json!({
        "cmd": "SET_ACTIVITY",
        "args": { "pid": std::process::id(), "activity": act },
        "nonce": format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()))
    });
    pipe.write_all(&frame(1, &payload)).is_ok() && read_frame(pipe).is_ok()
}

fn client_id(ctx: &Ctx) -> Option<String> {
    std::env::var("JEEM_DISCORD_CLIENT_ID")
        .ok()
        .or_else(|| shell::get(ctx, "discordClientId").as_str().map(String::from))
        .filter(|s| !s.trim().is_empty())
}

fn ensure_thread(ctx: &Arc<Ctx>) -> Sender<Msg> {
    let h = handle();
    let mut guard = h.tx.lock().unwrap();
    if let Some(tx) = guard.as_ref() {
        return tx.clone();
    }
    let (tx, rx) = channel::<Msg>();
    *guard = Some(tx.clone());
    let ctx = ctx.clone();
    let connected = h.connected.clone();
    std::thread::spawn(move || {
        let mut pipe: Option<File> = None;
        let mut last: Option<Value> = None;
        loop {
            let msg = rx.recv_timeout(Duration::from_secs(15));
            let enabled = handle().enabled.load(Ordering::Relaxed);
            match msg {
                Ok(Msg::Update(info)) => last = Some(info),
                Ok(Msg::Clear) => last = None,
                Ok(Msg::Reconnect) => {
                    pipe = None;
                    connected.store(false, Ordering::Relaxed);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
            if !enabled {
                if let Some(p) = pipe.as_mut() {
                    let _ = set_activity(p, None);
                }
                pipe = None;
                connected.store(false, Ordering::Relaxed);
                continue;
            }
            if pipe.is_none() && last.is_some() {
                if let Some(id) = client_id(&ctx) {
                    pipe = connect(&id);
                    connected.store(pipe.is_some(), Ordering::Relaxed);
                }
            }
            if let Some(p) = pipe.as_mut() {
                let ok = set_activity(p, last.as_ref().map(activity));
                if !ok {
                    pipe = None;
                    connected.store(false, Ordering::Relaxed);
                }
            }
        }
    });
    tx
}

pub fn update(ctx: &Arc<Ctx>, info: Value) {
    if info.is_object() {
        let _ = ensure_thread(ctx).send(Msg::Update(info));
    }
}

pub fn clear(ctx: &Arc<Ctx>) {
    let _ = ensure_thread(ctx).send(Msg::Clear);
}

pub fn set_enabled(ctx: &Arc<Ctx>, on: bool) {
    handle().enabled.store(on, Ordering::Relaxed);
    let _ = ensure_thread(ctx).send(Msg::Clear);
}

pub fn reconnect(ctx: &Arc<Ctx>) {
    let _ = ensure_thread(ctx).send(Msg::Reconnect);
}

pub fn enabled() -> bool {
    handle().enabled.load(Ordering::Relaxed)
}

pub fn connected() -> bool {
    handle().connected.load(Ordering::Relaxed)
}
