use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::Ctx;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const YTDLP_URLS: [&str; 1] = ["https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe"];
const FFMPEG_URLS: [&str; 2] = [
    "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip",
    "https://github.com/BtbN/FFmpeg-Builds/releases/latest/download/ffmpeg-master-latest-win64-gpl.zip",
];

#[derive(Default, Clone)]
pub struct ToolState {
    pub status: String,
    pub tool: String,
    pub percent: u32,
    pub error: String,
}

pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

pub fn run_cmd(mut cmd: Command) -> Result<String, String> {
    let out = cmd.output().map_err(|e| format!("Cannot start tool: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(4).collect();
        return Err(tail.into_iter().rev().collect::<Vec<_>>().join("\n"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub fn tool_path(ctx: &Ctx, name: &str) -> PathBuf {
    let installed = ctx.tools_dir.join(format!("{name}.exe"));
    if installed.exists() { installed } else { PathBuf::from(name) }
}

pub fn installed(ctx: &Ctx, name: &str) -> bool {
    ctx.tools_dir.join(format!("{name}.exe")).exists()
}

pub fn status(ctx: &Ctx) -> Value {
    let st = ctx.tool_state.lock().unwrap().clone();
    let yt = installed(ctx, "yt-dlp");
    let ff = installed(ctx, "ffmpeg");
    let step = if st.status == "downloading" { format!("downloading {}", st.tool) } else { String::new() };
    json!({
        "ytDlp": yt, "ytdlp": yt, "ffmpeg": ff,
        "percent": st.percent, "step": step, "error": st.error,
        "progress": { "status": st.status, "tool": st.tool, "percent": st.percent, "error": st.error },
        "ffmpegOptional": false, "ffmpegError": ""
    })
}

fn set_state(ctx: &Ctx, f: impl FnOnce(&mut ToolState)) {
    f(&mut ctx.tool_state.lock().unwrap());
}

async fn download(ctx: &Ctx, urls: &[&str], dest: &Path, tool: &str) -> Result<(), String> {
    let mut last_err = String::from("No download source");
    for url in urls {
        match download_one(ctx, url, dest, tool).await {
            Ok(()) => return Ok(()),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

async fn download_one(ctx: &Ctx, url: &str, dest: &Path, tool: &str) -> Result<(), String> {
    let mut resp = ctx.client.get(url).send().await.map_err(|e| format!("Network error: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Download failed (HTTP {})", resp.status().as_u16()));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut got: u64 = 0;
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("Network error: {e}"))? {
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        got += chunk.len() as u64;
        if total > 0 {
            let p = (got * 100 / total).min(100) as u32;
            set_state(ctx, |s| {
                s.tool = tool.to_string();
                s.percent = p;
            });
        }
    }
    Ok(())
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(found) = find_file(&p, name) {
                return Some(found);
            }
        } else if p.file_name().is_some_and(|n| n.eq_ignore_ascii_case(name)) {
            return Some(p);
        }
    }
    None
}

const YTDLP_UPDATE_EVERY_SECS: u64 = 24 * 60 * 60;

pub fn spawn_ytdlp_updater(ctx: Arc<Ctx>) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        loop {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let last = crate::shell::get(&ctx, "ytdlpUpdatedAt").as_u64().unwrap_or(0);
            if installed(&ctx, "yt-dlp") && now.saturating_sub(last) >= YTDLP_UPDATE_EVERY_SECS {
                let exe = tool_path(&ctx, "yt-dlp");
                let res = tauri::async_runtime::spawn_blocking(move || {
                    let mut cmd = command(exe);
                    cmd.arg("-U");
                    run_cmd(cmd)
                })
                .await;
                match res {
                    Ok(Ok(out)) => {
                        eprintln!("[jeem] yt-dlp update: {}", out.lines().last().unwrap_or("").trim());
                        crate::shell::set(&ctx, "ytdlpUpdatedAt", json!(now));
                    }
                    Ok(Err(e)) => eprintln!("[jeem] yt-dlp update failed: {e}"),
                    Err(e) => eprintln!("[jeem] yt-dlp update failed: {e}"),
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(60 * 60)).await;
        }
    });
}

pub async fn install(ctx: Arc<Ctx>) {
    if ctx.tool_state.lock().unwrap().status == "downloading" {
        return;
    }
    set_state(&ctx, |s| {
        *s = ToolState { status: "downloading".into(), ..Default::default() };
    });
    let result = install_inner(&ctx).await;
    match result {
        Ok(()) => set_state(&ctx, |s| *s = ToolState { status: "ready".into(), percent: 100, ..Default::default() }),
        Err(e) => set_state(&ctx, |s| {
            s.status = "error".into();
            s.error = e;
        }),
    }
}

async fn install_inner(ctx: &Arc<Ctx>) -> Result<(), String> {
    let dir = ctx.tools_dir.clone();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    if !installed(ctx, "yt-dlp") {
        set_state(ctx, |s| {
            s.tool = "yt-dlp".into();
            s.percent = 0;
        });
        let part = dir.join("yt-dlp.exe.part");
        if let Err(e) = download(ctx, &YTDLP_URLS, &part, "yt-dlp").await {
            let _ = std::fs::remove_file(&part);
            return Err(format!("yt-dlp: {e}"));
        }
        std::fs::rename(&part, dir.join("yt-dlp.exe")).map_err(|e| e.to_string())?;
    }

    if !installed(ctx, "ffmpeg") {
        set_state(ctx, |s| {
            s.tool = "ffmpeg".into();
            s.percent = 0;
        });
        let zip = dir.join("ffmpeg.zip.part");
        if let Err(e) = download(ctx, &FFMPEG_URLS, &zip, "ffmpeg").await {
            let _ = std::fs::remove_file(&zip);
            return Err(format!("ffmpeg: {e}"));
        }
        let dir2 = dir.clone();
        tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
            let tmp = dir2.join("ffmpeg-unpack");
            let _ = std::fs::remove_dir_all(&tmp);
            std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
            let mut cmd = command(Path::new(&root).join("System32").join("tar.exe"));
            cmd.arg("-xf").arg(dir2.join("ffmpeg.zip.part")).arg("-C").arg(&tmp);
            run_cmd(cmd)?;
            let found = find_file(&tmp, "ffmpeg.exe").ok_or("ffmpeg.exe not found in the archive")?;
            std::fs::rename(&found, dir2.join("ffmpeg.exe")).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_dir_all(&tmp);
            let _ = std::fs::remove_file(dir2.join("ffmpeg.zip.part"));
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("ffmpeg: {e}"))?;
    }
    Ok(())
}
