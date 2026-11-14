use crate::{shell, Ctx};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tauri_plugin_updater::{Update, UpdaterExt};

const FIRST_DELAY_MS: u64 = 1500;
const CHECK_EVERY_SECS: u64 = 6 * 60 * 60;
const RELEASES_URL: &str = "https://github.com/Surse231/jeem/releases/latest";

#[derive(Default)]
struct State {
    update: Option<Update>,
    bytes: Option<Vec<u8>>,
    downloading: bool,
}

static STATE: Mutex<State> = Mutex::new(State { update: None, bytes: None, downloading: false });

fn emit_obj(ctx: &Ctx, name: &str, data: Value) {
    if let Some(w) = tauri::Manager::get_webview_window(&ctx.app, "main") {
        let _ = w.eval(format!("window.__jeemEmit&&window.__jeemEmit('{name}',{data})"));
    }
}

pub fn spawn_loop(ctx: Arc<Ctx>) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(FIRST_DELAY_MS)).await;
        loop {
            let _ = check(&ctx, false).await;
            tokio::time::sleep(std::time::Duration::from_secs(CHECK_EVERY_SECS)).await;
        }
    });
}

fn ignored_version(ctx: &Ctx) -> String {
    ctx.settings.lock().unwrap().get("ignoredUpdate").and_then(|v| v.as_str()).unwrap_or("").to_string()
}

pub async fn check(ctx: &Arc<Ctx>, manual: bool) -> Value {
    let updater = match ctx.app.updater() {
        Ok(u) => u,
        Err(e) => return fail(ctx, manual, e.to_string()),
    };
    match updater.check().await {
        Ok(Some(update)) => {
            let version = update.version.clone();
            if !manual && ignored_version(ctx) == version {
                return json!({ "ok": true, "available": false });
            }
            let info = json!({
                "version": version,
                "releaseNotes": update.body.clone().unwrap_or_default(),
                "htmlUrl": RELEASES_URL,
                "canInstall": true
            });
            {
                let mut st = STATE.lock().unwrap();
                let same = st.update.as_ref().map(|u| u.version == version).unwrap_or(false);
                if !same {
                    st.bytes = None;
                }
                st.update = Some(update);
            }
            emit_obj(ctx, "onUpdateAvailable", info);
            json!({ "ok": true, "available": true })
        }
        Ok(None) => {
            if manual {
                emit_obj(ctx, "onUpdateNotAvailable", json!({}));
            }
            json!({ "ok": true, "available": false })
        }
        Err(e) => fail(ctx, manual, e.to_string()),
    }
}

fn fail(ctx: &Ctx, manual: bool, msg: String) -> Value {
    eprintln!("[jeem] update check failed: {msg}");
    if manual {
        emit_obj(ctx, "onUpdateError", json!({ "message": msg }));
    }
    json!({ "ok": false, "error": msg })
}

pub async fn download(ctx: Arc<Ctx>) {
    let update = {
        let mut st = STATE.lock().unwrap();
        if st.downloading {
            return;
        }
        if st.bytes.is_some() {
            drop(st);
            emit_obj(&ctx, "onUpdateDownloaded", json!({}));
            return;
        }
        match st.update.clone() {
            Some(u) => {
                st.downloading = true;
                u
            }
            None => return,
        }
    };
    let mut got: u64 = 0;
    let mut last = -1i64;
    let ctx2 = ctx.clone();
    let res = update
        .download(
            move |chunk, total| {
                got += chunk as u64;
                if let Some(t) = total.filter(|t| *t > 0) {
                    let pct = (got * 100 / t) as i64;
                    if pct != last {
                        last = pct;
                        emit_obj(&ctx2, "onUpdateProgress", json!({ "percent": pct }));
                    }
                }
            },
            || {},
        )
        .await;
    let mut st = STATE.lock().unwrap();
    st.downloading = false;
    match res {
        Ok(bytes) => {
            st.bytes = Some(bytes);
            drop(st);
            emit_obj(&ctx, "onUpdateDownloaded", json!({}));
        }
        Err(e) => {
            drop(st);
            eprintln!("[jeem] update download failed: {e}");
            emit_obj(&ctx, "onUpdateError", json!({ "message": e.to_string() }));
        }
    }
}

pub fn install_now(ctx: &Arc<Ctx>) -> Value {
    let (update, bytes) = {
        let mut st = STATE.lock().unwrap();
        (st.update.clone(), st.bytes.take())
    };
    match (update, bytes) {
        (Some(u), Some(b)) => match u.install(b) {
            Ok(()) => {
                ctx.app.restart();
            }
            Err(e) => {
                emit_obj(ctx, "onUpdateError", json!({ "message": e.to_string() }));
                json!({ "ok": false, "error": e.to_string() })
            }
        },
        _ => json!({ "ok": false, "error": "no update downloaded" }),
    }
}

pub fn action(ctx: &Arc<Ctx>, what: &str) -> Value {
    match what {
        "now" => {
            let ready = STATE.lock().unwrap().bytes.is_some();
            if ready {
                return install_now(ctx);
            }
            tauri::async_runtime::spawn(download(ctx.clone()));
            json!({ "ok": true })
        }
        "ignore" => {
            let v = STATE.lock().unwrap().update.as_ref().map(|u| u.version.clone());
            if let Some(v) = v {
                shell::set(ctx, "ignoredUpdate", json!(v));
            }
            json!({ "ok": true })
        }
        _ => json!({ "ok": true }),
    }
}
