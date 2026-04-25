use crate::app_state::{self, ChatMessage, StateCmd};
use crate::config::{self, AppConfig, LanguageEncoding};
use crate::ipmsg_core;
use once_cell::sync::OnceCell;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::future::pending;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};
use tokio::runtime::{Builder, Handle};
use tokio::task::JoinHandle;

static RUNTIME_HANDLE: OnceCell<Handle> = OnceCell::new();
static STARTED: OnceCell<()> = OnceCell::new();
static ACTIVE_DOWNLOADS: Lazy<Mutex<HashMap<String, JoinHandle<()>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn download_key(from: SocketAddr, packet_no: u32, file_id: u32) -> String {
    format!("{}-{}-{}", from, packet_no, file_id)
}

pub fn ensure_started() {
    if STARTED.get().is_some() {
        return;
    }
    STARTED.get_or_init(|| {
        thread::spawn(|| {
            let runtime = Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime");
            let _ = RUNTIME_HANDLE.set(runtime.handle().clone());
            runtime.block_on(async {
                app_state::init_state();
                pending::<()>().await;
            });
        });
    });
}

pub fn send_text(to: SocketAddr, text: String) {
    if text.trim().is_empty() {
        return;
    }
    let Some(handle) = RUNTIME_HANDLE.get() else {
        return;
    };

    handle.spawn(async move {
        if ipmsg_core::send_message(to, text.clone()).await.is_ok() {
            if let Some(me) = app_state::get_self_addr_info() {
                let msg = ChatMessage {
                    from: me.addr,
                    to,
                    is_me: true,
                    text,
                    time: "现在".to_string(),
                    file: None,
                };
                app_state::dispatch_cmd(StateCmd::PushOutgoing(msg));
            }
        }
    });
}

pub fn send_files(to: SocketAddr, paths: Vec<String>) {
    if paths.is_empty() {
        return;
    }
    let Some(handle) = RUNTIME_HANDLE.get() else {
        return;
    };

    handle.spawn(async move {
        if let Ok(sent_items) = ipmsg_core::send_files(to, paths.clone()).await {
            if let Some(me) = app_state::get_self_addr_info() {
                for item in sent_items {
                    let msg = ChatMessage {
                        from: me.addr,
                        to,
                        is_me: true,
                        text: String::new(),
                        time: "现在".to_string(),
                        file: Some(app_state::FileInfo {
                            packet_no: item.packet_no,
                            file_id: item.file_id,
                            name: item.name,
                            size: item.size,
                            saved: false,
                            received: 0,
                            is_dir: false,
                            local_path: Some(item.path),
                            current_file: None,
                            error: false,
                            canceled: false,
                            sending: false,
                        }),
                    };
                    app_state::dispatch_cmd(StateCmd::PushOutgoing(msg));
                }
            }
        }
    });
}

pub fn send_folder(to: SocketAddr, path: String) {
    if path.trim().is_empty() {
        return;
    }
    let Some(handle) = RUNTIME_HANDLE.get() else {
        return;
    };

    handle.spawn(async move {
        if let Ok(item) = ipmsg_core::send_folder(to, path.clone()).await {
            if let Some(me) = app_state::get_self_addr_info() {
                let msg = ChatMessage {
                    from: me.addr,
                    to,
                    is_me: true,
                    text: format!("[文件夹] {}", item.name),
                    time: "现在".to_string(),
                    file: Some(app_state::FileInfo {
                        packet_no: item.packet_no,
                        file_id: item.file_id,
                        name: item.name,
                        size: item.size,
                        saved: false,
                        received: 0,
                        is_dir: item.is_dir,
                        local_path: Some(item.path),
                        current_file: None,
                        error: false,
                        canceled: false,
                        sending: false,
                    }),
                };
                app_state::dispatch_cmd(StateCmd::PushOutgoing(msg));
            }
        }
    });
}

pub fn download_file(
    from: SocketAddr,
    packet_no: u32,
    file_id: u32,
    size: u64,
    save_path: String,
) {
    let Some(handle) = RUNTIME_HANDLE.get() else {
        return;
    };
    let key = download_key(from, packet_no, file_id);
    if let Some(previous) = ACTIVE_DOWNLOADS.lock().unwrap().remove(&key) {
        previous.abort();
    }
    let save_path_for_state = save_path.clone();
    let key_for_task = key.clone();
    let join = handle.spawn(async move {
        let mut last_update = Instant::now();
        let result = ipmsg_core::recv_file(from, packet_no, file_id, size, save_path, |progress| {
            if last_update.elapsed() >= Duration::from_millis(100) || progress == size {
                app_state::dispatch_cmd(StateCmd::UpdateProgress {
                    from,
                    file_id,
                    packet_no,
                    target_outgoing: Some(false),
                    progress,
                    file_name: None,
                    local_path: None,
                    saved: None,
                    error: Some(false),
                    canceled: None,
                });
                last_update = Instant::now();
            }
        })
        .await;

        match result {
            Ok(_) => {
                app_state::dispatch_cmd(StateCmd::UpdateProgress {
                    from,
                    file_id,
                    packet_no,
                    target_outgoing: Some(false),
                    progress: size,
                    file_name: None,
                    local_path: Some(save_path_for_state.clone()),
                    saved: Some(true),
                    error: Some(false),
                    canceled: Some(false),
                });
            }
            Err(_) => {
                app_state::dispatch_cmd(StateCmd::UpdateProgress {
                    from,
                    file_id,
                    packet_no,
                    target_outgoing: Some(false),
                    progress: 0,
                    file_name: None,
                    local_path: None,
                    saved: Some(false),
                    error: Some(true),
                    canceled: None,
                });
            }
        }
        ACTIVE_DOWNLOADS.lock().unwrap().remove(&key_for_task);
    });
    ACTIVE_DOWNLOADS.lock().unwrap().insert(key, join);
}

pub fn download_folder(from: SocketAddr, packet_no: u32, file_id: u32, save_path: String) {
    let Some(handle) = RUNTIME_HANDLE.get() else {
        return;
    };
    let key = download_key(from, packet_no, file_id);
    if let Some(previous) = ACTIVE_DOWNLOADS.lock().unwrap().remove(&key) {
        previous.abort();
    }
    let save_path_for_state = save_path.clone();
    let key_for_task = key.clone();
    let join = handle.spawn(async move {
        let mut last_update = Instant::now();
        let mut last_file_name = String::new();
        let result = ipmsg_core::recv_folder(from, packet_no, file_id, save_path, |progress, current_file| {
            let file_changed = current_file != last_file_name;
            if file_changed || last_update.elapsed() >= Duration::from_millis(100) {
                app_state::dispatch_cmd(StateCmd::UpdateProgress {
                    from,
                    file_id,
                    packet_no,
                    target_outgoing: Some(false),
                    progress,
                    file_name: Some(current_file.clone()),
                    local_path: None,
                    saved: None,
                    error: Some(false),
                    canceled: None,
                });
                last_file_name = current_file;
                last_update = Instant::now();
            }
        })
        .await;

        match result {
            Ok(_) => {
                app_state::dispatch_cmd(StateCmd::UpdateProgress {
                    from,
                    file_id,
                    packet_no,
                    target_outgoing: Some(false),
                    progress: 0,
                    file_name: None,
                    local_path: Some(save_path_for_state.clone()),
                    saved: Some(true),
                    error: Some(false),
                    canceled: Some(false),
                });
            }
            Err(_) => {
                app_state::dispatch_cmd(StateCmd::UpdateProgress {
                    from,
                    file_id,
                    packet_no,
                    target_outgoing: Some(false),
                    progress: 0,
                    file_name: None,
                    local_path: None,
                    saved: Some(false),
                    error: Some(true),
                    canceled: None,
                });
            }
        }
        ACTIVE_DOWNLOADS.lock().unwrap().remove(&key_for_task);
    });
    ACTIVE_DOWNLOADS.lock().unwrap().insert(key, join);
}

pub fn cancel_download(from: SocketAddr, packet_no: u32, file_id: u32) {
    let key = download_key(from, packet_no, file_id);
    if let Some(handle) = ACTIVE_DOWNLOADS.lock().unwrap().remove(&key) {
        handle.abort();
    }
    app_state::dispatch_cmd(StateCmd::UpdateProgress {
        from,
        file_id,
        packet_no,
        target_outgoing: Some(false),
        progress: 0,
        file_name: None,
        local_path: None,
        saved: Some(false),
        error: Some(false),
        canceled: Some(true),
    });
}

pub fn cancel_upload(to: SocketAddr, packet_no: u32, file_id: u32) {
    let _ = ipmsg_core::cancel_send(to, packet_no, file_id);
    app_state::dispatch_cmd(StateCmd::UpdateProgress {
        from: to,
        file_id,
        packet_no,
        target_outgoing: Some(true),
        progress: 0,
        file_name: None,
        local_path: None,
        saved: Some(false),
        error: Some(false),
        canceled: Some(true),
    });
}

pub fn open_in_folder(path: String, is_dir: bool) {
    if path.trim().is_empty() {
        return;
    }
    let path_buf = PathBuf::from(&path);

    #[cfg(target_os = "windows")]
    {
        let normalized = std::fs::canonicalize(&path_buf).unwrap_or(path_buf.clone());
        if is_dir {
            let target_dir = if normalized.is_dir() {
                normalized
            } else {
                normalized
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| path_buf.clone())
            };
            let _ = Command::new("explorer").arg(target_dir).spawn();
        } else if normalized.is_file() {
            let _ = Command::new("explorer")
                .arg("/select,")
                .arg(&normalized)
                .spawn();
        } else {
            let fallback_dir = normalized
                .parent()
                .map(|p| p.to_path_buf())
                .or_else(|| path_buf.parent().map(|p| p.to_path_buf()))
                .unwrap_or_else(|| PathBuf::from("."));
            let _ = Command::new("explorer").arg(fallback_dir).spawn();
        }
        return;
    }

    #[cfg(target_os = "macos")]
    {
        if is_dir {
            let _ = Command::new("open").arg(&path_buf).spawn();
        } else {
            let _ = Command::new("open").arg("-R").arg(&path_buf).spawn();
        }
        return;
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let target = if is_dir {
            path_buf
        } else {
            path_buf
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."))
        };
        let _ = Command::new("xdg-open").arg(target).spawn();
    }
}

pub fn get_config() -> AppConfig {
    config::load_config()
}

pub fn save_settings(
    username: String,
    group: String,
    language: LanguageEncoding,
) -> Result<(), String> {
    let mut current = config::load_config();
    current.user.username = username.clone();
    current.user.group = group.clone();
    current.language = language;
    config::save_config(&current).map_err(|e| e.to_string())?;

    ipmsg_core::set_text_encoding(match language {
        LanguageEncoding::Utf8 => ipmsg_core::TextEncoding::Utf8,
        LanguageEncoding::Gb18030 => ipmsg_core::TextEncoding::Gb18030,
    });
    ipmsg_core::set_user_info(&username, &group);
    if let Some(handle) = RUNTIME_HANDLE.get() {
        handle.spawn(async move {
            let _ = ipmsg_core::send_broadcast_entry().await;
        });
    }

    if let Some(me) = app_state::get_self_addr_info() {
        app_state::dispatch_cmd(StateCmd::InitSelf {
            user: username,
            group,
            host: me.host,
            addr: me.addr,
        });
    }

    Ok(())
}
