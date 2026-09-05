use crate::app_state::{self, AppState, ChatMessage, FileInfo, StateCmd};
use crate::config::{self, AppConfig, LanguageEncoding, UiLanguage};
use crate::ipmsg_core;
use once_cell::sync::OnceCell;
use std::future::pending;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tokio::runtime::{Builder, Handle};

static RUNTIME_HANDLE: OnceCell<Handle> = OnceCell::new();
static STARTED: OnceCell<()> = OnceCell::new();

fn download_key(from: SocketAddr, packet_no: u32, file_id: u32) -> String {
    format!("{}-{}-{}", from, packet_no, file_id)
}

/// Best-effort name/size/dir classification of a local path, used to render
/// and retry failed file offers.
fn file_meta(path: &str) -> (String, u64, bool) {
    let p = PathBuf::from(path);
    let name = p
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    let meta = std::fs::metadata(&p).ok();
    let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
    let size = meta.map(|m| m.len()).unwrap_or(0);
    (name, size, is_dir)
}

pub fn ensure_started() {
    if STARTED.get().is_some() {
        return;
    }
    STARTED.get_or_init(|| {
        // 状态实例与协议服务在 tokio runtime 之外同步创建并注册，
        // 主线程随后可从 GPUI Global / `app_state()` 无竞态取用。
        let app_state = Arc::new(AppState::new());
        app_state::set_instance(Arc::clone(&app_state));
        thread::spawn(move || {
            let runtime = Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime");
            let _ = RUNTIME_HANDLE.set(runtime.handle().clone());
            runtime.block_on(async {
                app_state.init();
                pending::<()>().await;
            });
        });
    });
}

/// Graceful shutdown: broadcast BR_EXIT so peers drop us from their user
/// lists immediately, then exit. `process::exit` is still required because
/// GPUI keeps the process alive after the last window closes on some
/// platforms, but it now runs only after the offline broadcast was sent
/// (P0-4 in docs/architecture.md).
pub fn shutdown() {
    if let Some(handle) = RUNTIME_HANDLE.get() {
        handle.block_on(async {
            if let Some(service) = ipmsg_core::try_service() {
                let _ = service.send_exit().await;
            }
        });
    }
    std::process::exit(0);
}

pub fn send_text(to: SocketAddr, text: String, app_state: &Arc<AppState>) {
    if text.trim().is_empty() {
        return;
    }
    let Some(handle) = RUNTIME_HANDLE.get() else {
        return;
    };
    let app_state = Arc::clone(app_state);

    handle.spawn(async move {
        let service = ipmsg_core::service();
        let sent = service.send_message(to, text.clone()).await;
        if let Err(err) = &sent {
            log::warn!("send_message to {} failed: {}", to, err);
        }
        if let Some(me) = app_state.get_self_addr_info() {
            let msg = ChatMessage {
                from: me.addr,
                to,
                is_me: true,
                text,
                time: t!("time.now").to_string(),
                file: None,
                failed: sent.is_err(),
                packet_no: sent.unwrap_or(0),
                delivered: false,
                id: 0,
            };
            app_state.dispatch(StateCmd::PushOutgoing(msg));
        }
    });
}

pub fn send_files(to: SocketAddr, paths: Vec<String>, app_state: &Arc<AppState>) {
    if paths.is_empty() {
        return;
    }
    let Some(handle) = RUNTIME_HANDLE.get() else {
        return;
    };
    let app_state = Arc::clone(app_state);

    handle.spawn(async move {
        let service = ipmsg_core::service();
        let send_result = service.send_files(to, paths.clone()).await;
        if let Err(err) = &send_result {
            log::warn!("send_files to {} failed: {}", to, err);
        }
        if let Some(me) = app_state.get_self_addr_info() {
            match send_result {
                Ok(sent_items) => {
                    for item in sent_items {
                        let msg = ChatMessage {
                            from: me.addr,
                            to,
                            is_me: true,
                            text: String::new(),
                            time: t!("time.now").to_string(),
                            file: Some(FileInfo {
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
                            failed: false,
                            packet_no: item.packet_no,
                            delivered: false,
                            id: 0,
                        };
                        app_state.dispatch(StateCmd::PushOutgoing(msg));
                    }
                }
                Err(_) => {
                    // send_files is all-or-nothing (one SENDMSG for the batch),
                    // so every path failed. Surface one retryable failed bubble
                    // per path (P0-5 in docs/architecture.md).
                    for path in &paths {
                        let (name, size, is_dir) = file_meta(path);
                        let msg = ChatMessage {
                            from: me.addr,
                            to,
                            is_me: true,
                            text: String::new(),
                            time: t!("time.now").to_string(),
                            file: Some(FileInfo {
                                packet_no: 0,
                                file_id: 0,
                                name,
                                size,
                                saved: false,
                                received: 0,
                                is_dir,
                                local_path: Some(path.clone()),
                                current_file: None,
                                error: false,
                                canceled: false,
                                sending: false,
                            }),
                            failed: true,
                            packet_no: 0,
                            delivered: false,
                            id: 0,
                        };
                        app_state.dispatch(StateCmd::PushOutgoing(msg));
                    }
                }
            }
        }
    });
}

pub fn send_folder(to: SocketAddr, path: String, app_state: &Arc<AppState>) {
    if path.trim().is_empty() {
        return;
    }
    let Some(handle) = RUNTIME_HANDLE.get() else {
        return;
    };
    let app_state = Arc::clone(app_state);

    handle.spawn(async move {
        let service = ipmsg_core::service();
        let send_result = service.send_folder(to, path.clone()).await;
        if let Err(err) = &send_result {
            log::warn!("send_folder to {} failed: {}", to, err);
        }
        if let Some(me) = app_state.get_self_addr_info() {
            let msg = match send_result {
                Ok(item) => ChatMessage {
                    from: me.addr,
                    to,
                    is_me: true,
                    text: t!("file.folder_prefix", name = item.name.clone()).to_string(),
                    time: t!("time.now").to_string(),
                    file: Some(FileInfo {
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
                    failed: false,
                    packet_no: item.packet_no,
                    delivered: false,
                    id: 0,
                },
                Err(_) => {
                    // Keep the local path so the failed bubble can offer retry.
                    let name = PathBuf::from(&path)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.clone());
                    ChatMessage {
                        from: me.addr,
                        to,
                        is_me: true,
                        text: String::new(),
                        time: t!("time.now").to_string(),
                        file: Some(FileInfo {
                            packet_no: 0,
                            file_id: 0,
                            name,
                            size: 0,
                            saved: false,
                            received: 0,
                            is_dir: true,
                            local_path: Some(path),
                            current_file: None,
                            error: false,
                            canceled: false,
                            sending: false,
                        }),
                        failed: true,
                        packet_no: 0,
                        delivered: false,
                        id: 0,
                    }
                }
            };
            app_state.dispatch(StateCmd::PushOutgoing(msg));
        }
    });
}

/// Retry a failed outgoing message: text is re-sent via SENDMSG, a file/folder
/// offer is re-advertised from its local path. The state layer updates the
/// original bubble on success (StateCmd::RetryFinished).
pub fn retry_message(
    id: u64,
    to: SocketAddr,
    text: String,
    file: Option<(String, bool)>,
    app_state: &Arc<AppState>,
) {
    let Some(handle) = RUNTIME_HANDLE.get() else {
        return;
    };
    let app_state = Arc::clone(app_state);
    handle.spawn(async move {
        let service = ipmsg_core::service();
        let (ok, packet_no, file_id) = if let Some((path, is_dir)) = file {
            let send_result = if is_dir {
                service
                    .send_folder(to, path)
                    .await
                    .map(|item| vec![item])
            } else {
                service.send_files(to, vec![path]).await
            };
            match send_result {
                Ok(mut items) if !items.is_empty() => {
                    let item = items.remove(0);
                    (true, item.packet_no, item.file_id)
                }
                Err(err) => {
                    log::warn!("retry send file to {} failed: {}", to, err);
                    (false, 0, 0)
                }
                _ => (false, 0, 0),
            }
        } else {
            let sent = service.send_message(to, text.clone()).await;
            if let Err(err) = &sent {
                log::warn!("retry send_message to {} failed: {}", to, err);
            }
            (sent.is_ok(), sent.unwrap_or(0), 0)
        };
        app_state.dispatch(StateCmd::RetryFinished {
            id,
            ok,
            packet_no,
            file_id,
        });
    });
}

pub fn download_file(
    from: SocketAddr,
    packet_no: u32,
    file_id: u32,
    size: u64,
    save_path: String,
    app_state: &Arc<AppState>,
) {
    let Some(handle) = RUNTIME_HANDLE.get() else {
        return;
    };
    let key = download_key(from, packet_no, file_id);
    if let Some(previous) = app_state.take_download(&key) {
        previous.abort();
    }
    let save_path_for_state = save_path.clone();
    let key_for_task = key.clone();
    let app_state_for_task = Arc::clone(app_state);
    let join = handle.spawn(async move {
        let service = ipmsg_core::service();
        let mut last_update = Instant::now();
        let result = service.recv_file(from, packet_no, file_id, size, save_path, |progress| {
            if last_update.elapsed() >= Duration::from_millis(100) || progress == size {
                app_state_for_task.dispatch(StateCmd::UpdateProgress {
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
                app_state_for_task.dispatch(StateCmd::UpdateProgress {
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
                app_state_for_task.dispatch(StateCmd::UpdateProgress {
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
        app_state_for_task.take_download(&key_for_task);
    });
    app_state.insert_download(key, join);
}

pub fn download_folder(
    from: SocketAddr,
    packet_no: u32,
    file_id: u32,
    save_path: String,
    app_state: &Arc<AppState>,
) {
    let Some(handle) = RUNTIME_HANDLE.get() else {
        return;
    };
    let key = download_key(from, packet_no, file_id);
    if let Some(previous) = app_state.take_download(&key) {
        previous.abort();
    }
    let save_path_for_state = save_path.clone();
    let key_for_task = key.clone();
    let app_state_for_task = Arc::clone(app_state);
    let join = handle.spawn(async move {
        let service = ipmsg_core::service();
        let mut last_update = Instant::now();
        let mut last_file_name = String::new();
        let result = service.recv_folder(from, packet_no, file_id, save_path, |progress, current_file| {
            let file_changed = current_file != last_file_name;
            if file_changed || last_update.elapsed() >= Duration::from_millis(100) {
                app_state_for_task.dispatch(StateCmd::UpdateProgress {
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
                app_state_for_task.dispatch(StateCmd::UpdateProgress {
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
                app_state_for_task.dispatch(StateCmd::UpdateProgress {
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
        app_state_for_task.take_download(&key_for_task);
    });
    app_state.insert_download(key, join);
}

pub fn cancel_download(from: SocketAddr, packet_no: u32, file_id: u32, app_state: &Arc<AppState>) {
    let key = download_key(from, packet_no, file_id);
    if let Some(handle) = app_state.take_download(&key) {
        handle.abort();
    }
    app_state.dispatch(StateCmd::UpdateProgress {
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

pub fn cancel_upload(to: SocketAddr, packet_no: u32, file_id: u32, app_state: &Arc<AppState>) {
    let _ = ipmsg_core::service().cancel_send(to, packet_no, file_id);
    app_state.dispatch(StateCmd::UpdateProgress {
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
    ui_language: UiLanguage,
    app_state: &Arc<AppState>,
) -> Result<(), String> {
    let mut current = config::load_config();
    current.user.username = username.clone();
    current.user.group = group.clone();
    current.language = language;
    current.ui_language = ui_language;
    config::save_config(&current).map_err(|e| e.to_string())?;
    rust_i18n::set_locale(ui_language.as_locale());

    let service = ipmsg_core::service();
    service.set_text_encoding(match language {
        LanguageEncoding::Utf8 => ipmsg_core::TextEncoding::Utf8,
        LanguageEncoding::Gb18030 => ipmsg_core::TextEncoding::Gb18030,
    });
    service.set_user_info(&username, &group);
    if let Some(handle) = RUNTIME_HANDLE.get() {
        handle.spawn(async move {
            let _ = ipmsg_core::service().send_broadcast_entry().await;
        });
    }

    if let Some(me) = app_state.get_self_addr_info() {
        app_state.dispatch(StateCmd::InitSelf {
            user: username,
            group,
            host: me.host,
            addr: me.addr,
        });
    }
    // 通知 UI 刷新本地化文案（UI 语言可能已切换），替代原先的 config 轮询探测。
    app_state.dispatch(StateCmd::SettingsSaved);

    Ok(())
}
