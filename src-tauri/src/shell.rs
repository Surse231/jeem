// Desktop integration: settings file, tray, window state, mini player, media keys and global shortcuts.
use std::sync::Arc;

use serde_json::{Value, json};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use crate::Ctx;

pub fn load_settings(dir: &std::path::Path) -> Value {
    std::fs::read_to_string(dir.join("settings.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

pub fn get(ctx: &Ctx, key: &str) -> Value {
    ctx.settings.lock().unwrap().get(key).cloned().unwrap_or(Value::Null)
}

pub fn set(ctx: &Ctx, key: &str, value: Value) {
    let mut s = ctx.settings.lock().unwrap();
    if value.is_null() {
        s.as_object_mut().map(|o| o.remove(key));
    } else {
        s[key] = value;
    }
    let _ = std::fs::write(ctx.data_dir.join("settings.json"), s.to_string());
}

pub fn emit(app: &AppHandle, name: &str, data: &str) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.eval(format!("window.__jeemEmit&&window.__jeemEmit('{name}','{data}')"));
    }
}

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

pub fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show Jeem", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let mut b = TrayIconBuilder::new()
        .tooltip("Jeem")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, ev| match ev.id.as_ref() {
            "show" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        b = b.icon(icon.clone());
    }
    b.build(app)?;
    Ok(())
}

pub fn tray_on_close(ctx: &Ctx) -> bool {
    get(ctx, "trayOnClose").as_bool().unwrap_or(true)
}

pub fn save_window(ctx: &Ctx, window: &tauri::Window) {
    let maximized = window.is_maximized().unwrap_or(false);
    let mut state = get(ctx, "window");
    if !state.is_object() {
        state = json!({});
    }
    state["max"] = json!(maximized);
    // Only remember the normal (not maximized, not minimized, not mini-player) geometry.
    if !maximized && !window.is_minimized().unwrap_or(false) && ctx.mini.lock().unwrap().is_none() {
        if let (Ok(p), Ok(s)) = (window.outer_position(), window.outer_size()) {
            if s.width >= 800 && s.height >= 540 {
                state["x"] = json!(p.x);
                state["y"] = json!(p.y);
                state["w"] = json!(s.width);
                state["h"] = json!(s.height);
            }
        }
    }
    set(ctx, "window", state);
}

pub fn restore_window(ctx: &Ctx, window: &tauri::WebviewWindow) {
    let st = get(ctx, "window");
    let n = |k: &str| st.get(k).and_then(Value::as_i64);
    if let (Some(x), Some(y), Some(w), Some(h)) = (n("x"), n("y"), n("w"), n("h")) {
        let on_screen = window
            .available_monitors()
            .map(|ms| {
                ms.iter().any(|m| {
                    let (mp, ms) = (m.position(), m.size());
                    x + 100 > mp.x as i64 && x + 100 < mp.x as i64 + ms.width as i64 && y >= mp.y as i64 - 10 && y < mp.y as i64 + ms.height as i64 - 50
                })
            })
            .unwrap_or(false);
        if on_screen {
            let _ = window.set_size(PhysicalSize::new(w as u32, h as u32));
            let _ = window.set_position(PhysicalPosition::new(x as i32, y as i32));
        }
    }
    if st.get("max").and_then(Value::as_bool) == Some(true) {
        let _ = window.maximize();
    }
}

/* ---------- mini player: the main window shrinks and stays on top ---------- */

pub fn toggle_mini(ctx: &Ctx) -> bool {
    let Some(w) = ctx.app.get_webview_window("main") else { return false };
    let mut mini = ctx.mini.lock().unwrap();
    if mini.is_none() {
        let was_max = w.is_maximized().unwrap_or(false);
        if was_max {
            let _ = w.unmaximize();
        }
        if let (Ok(p), Ok(s)) = (w.outer_position(), w.outer_size()) {
            *mini = Some((p, s, was_max));
        }
        let _ = w.set_min_size(Some(tauri::LogicalSize::new(280.0, 115.0)));
        let _ = w.set_size(tauri::LogicalSize::new(380.0, 160.0));
        let _ = w.set_always_on_top(true);
        true
    } else {
        let (p, s, was_max) = mini.take().unwrap();
        let _ = w.set_always_on_top(false);
        let _ = w.set_min_size(Some(tauri::LogicalSize::new(800.0, 540.0)));
        let _ = w.set_size(s);
        let _ = w.set_position(p);
        if was_max {
            let _ = w.maximize();
        }
        false
    }
}

pub fn set_mini_lyrics(ctx: &Ctx, enabled: bool) -> bool {
    let Some(w) = ctx.app.get_webview_window("main") else { return false };
    if ctx.mini.lock().unwrap().is_none() {
        return false;
    }
    let scale = w.scale_factor().unwrap_or(1.0);
    let width = w.outer_size().map(|s| s.width as f64 / scale).unwrap_or(380.0);
    if enabled {
        let _ = w.set_min_size(Some(tauri::LogicalSize::new(320.0, 240.0)));
        let _ = w.set_size(tauri::LogicalSize::new(width, 420.0));
    } else {
        let _ = w.set_min_size(Some(tauri::LogicalSize::new(320.0, 140.0)));
        let _ = w.set_size(tauri::LogicalSize::new(width, 160.0));
    }
    true
}

/* ---------- media keys and global shortcuts ---------- */

pub fn sync_media_keys(ctx: &Arc<Ctx>) {
    let want = get(ctx, "mediaKeys").as_bool().unwrap_or(true) && ctx.media_keys_active.load(std::sync::atomic::Ordering::Relaxed);
    let gs = ctx.app.global_shortcut();
    for (key, cmd) in [("MediaPlayPause", "toggle"), ("MediaTrackNext", "next"), ("MediaTrackPrevious", "prev"), ("MediaStop", "stop")] {
        let _ = gs.unregister(key);
        if want {
            let app = ctx.app.clone();
            let _ = gs.on_shortcut(key, move |_, _, ev| {
                if ev.state() == ShortcutState::Pressed {
                    emit(&app, "onMediaCommand", cmd);
                }
            });
        }
    }
}

const SHORTCUTS: [(&str, &str); 5] = [
    ("CommandOrControl+Shift+Space", "play-pause"),
    ("CommandOrControl+Shift+Right", "next"),
    ("CommandOrControl+Shift+Left", "prev"),
    ("CommandOrControl+Shift+Up", "seek-forward"),
    ("CommandOrControl+Shift+Down", "seek-back"),
];

pub fn sync_global_shortcuts(ctx: &Arc<Ctx>) {
    let want = get(ctx, "globalShortcuts").as_bool().unwrap_or(false);
    let gs = ctx.app.global_shortcut();
    for (accel, action) in SHORTCUTS {
        let _ = gs.unregister(accel);
        if want {
            let app = ctx.app.clone();
            let _ = gs.on_shortcut(accel, move |_, _, ev| {
                if ev.state() == ShortcutState::Pressed {
                    emit(&app, "onMediaControl", action);
                }
            });
        }
    }
}
