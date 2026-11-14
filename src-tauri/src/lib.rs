mod addons;
mod cache;
mod catalog;
mod discord;
mod google;
mod gsync;
mod download;
mod extras;
mod import;
mod lastfm;
mod library;
mod local;
mod server;
mod shell;
mod social;
mod spotify;
mod tools;
mod updater;
mod vibe;
mod ytm;

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::Value;
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder};

pub struct Ctx {
    pub app: AppHandle,
    pub client: reqwest::Client,
    pub token: String,
    pub port: u16,
    pub data_dir: PathBuf,
    pub tools_dir: PathBuf,
    pub search_cache: Mutex<HashMap<String, (Instant, Vec<Value>)>>,
    pub stream_cache: Mutex<HashMap<String, (Instant, String)>>,
    pub resolving: Mutex<HashSet<String>>,
    pub tool_state: Mutex<tools::ToolState>,
    pub welcome_result: Mutex<Option<Value>>,
    pub cache_index: Mutex<HashMap<String, Value>>,
    pub caching: Mutex<HashSet<String>>,
    pub settings: Mutex<Value>,
    pub mini: Mutex<Option<(PhysicalPosition<i32>, PhysicalSize<u32>, bool)>>,
    pub media_keys_active: AtomicBool,
    pub spotify: Mutex<spotify::Auth>,
    pub google: Mutex<google::Pending>,
    pub gsync: Mutex<google::Pending>,
}

pub fn default_download_dir(ctx: &Ctx) -> PathBuf {
    let base = ctx
        .app
        .path()
        .audio_dir()
        .or_else(|_| ctx.app.path().download_dir())
        .unwrap_or_else(|_| ctx.data_dir.clone());
    base.join("Jeem")
}

pub fn download_dir(ctx: &Ctx) -> PathBuf {
    let dir = shell::get(ctx, "downloadDir")
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| default_download_dir(ctx));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn random_token() -> String {
    let mut out = String::new();
    for _ in 0..4 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if let Some(ctx) = window.app_handle().try_state::<Arc<Ctx>>() {
                    shell::save_window(&ctx, window);
                    if shell::tray_on_close(&ctx) {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
            }
        })
        .setup(|app| {
            // JEEM_DATA_DIR lets a development build run next to the installed app without sharing its data.
            let data_dir = match std::env::var("JEEM_DATA_DIR") {
                Ok(d) if !d.trim().is_empty() => PathBuf::from(d),
                _ => app.path().app_local_data_dir()?,
            };
            std::fs::create_dir_all(&data_dir)?;
            let listener = std::net::TcpListener::bind("127.0.0.1:38472").or_else(|_| std::net::TcpListener::bind("127.0.0.1:0"))?;
            let port = listener.local_addr()?.port();
            let ctx = Arc::new(Ctx {
                app: app.handle().clone(),
                client: reqwest::Client::builder().user_agent(ytm::UA).build()?,
                token: random_token(),
                port,
                tools_dir: data_dir.join("tools"),
                data_dir: data_dir.clone(),
                search_cache: Mutex::new(HashMap::new()),
                stream_cache: Mutex::new(HashMap::new()),
                resolving: Mutex::new(HashSet::new()),
                tool_state: Mutex::new(tools::ToolState::default()),
                welcome_result: Mutex::new(None),
                cache_index: Mutex::new(cache::load_index(&data_dir.join("cache"))),
                caching: Mutex::new(HashSet::new()),
                settings: Mutex::new(shell::load_settings(&data_dir)),
                mini: Mutex::new(None),
                media_keys_active: AtomicBool::new(false),
                spotify: Mutex::new(spotify::Auth::default()),
                google: Mutex::new(google::Pending::default()),
                gsync: Mutex::new(google::Pending::default()),
            });
            app.manage(ctx.clone());
            tauri::async_runtime::spawn(server::serve(ctx.clone(), listener));

            eprintln!("[jeem] server on http://127.0.0.1:{port}/");
            let page = if data_dir.join("first-run-done").exists() { "index.html" } else { "welcome.html" };
            let url = format!("http://127.0.0.1:{port}/src/renderer/{page}");
            let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url.parse()?))
                .title("Jeem")
                .inner_size(1280.0, 800.0)
                .min_inner_size(800.0, 540.0)
                .decorations(false)
                .background_color(tauri::window::Color(0, 0, 0, 255))
                .build()?;
            shell::restore_window(&ctx, &window);
            shell::setup_tray(app)?;
            shell::sync_global_shortcuts(&ctx);
            updater::spawn_loop(ctx.clone());
            tools::spawn_ytdlp_updater(ctx.clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
