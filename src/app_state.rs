use crate::config;
use crate::ipmsg_core::{
    Event, TextEncoding, detect_self_addr, protocol::PORT, set_text_encoding, set_user_info,
    start_ipmsg,
};
use once_cell::sync::{Lazy, OnceCell};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Clone, Serialize, Deserialize)]
pub struct OnlineUser {
    pub name: String,
    pub group: String,
    pub host: String,
    pub addr: SocketAddr,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub packet_no: u32,
    pub file_id: u32,
    pub name: String,
    pub size: u64,
    pub saved: bool,
    pub received: u64,
    pub is_dir: bool,
    pub local_path: Option<String>,
    pub current_file: Option<String>,
    #[serde(default)]
    pub error: bool,
    #[serde(default)]
    pub canceled: bool,
    #[serde(default)]
    pub sending: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub from: SocketAddr,
    pub to: SocketAddr,
    pub is_me: bool,
    pub text: String,
    pub time: String,
    pub file: Option<FileInfo>,
}

#[derive(Clone)]
pub struct CoreState {
    pub online_users: HashMap<SocketAddr, OnlineUser>,
    pub messages: Vec<ChatMessage>,
    pub unread_counts: HashMap<SocketAddr, u32>,
    pub self_addr: Option<SocketAddr>,
}

impl CoreState {
    fn new() -> Self {
        Self {
            online_users: HashMap::new(),
            messages: Vec::new(),
            unread_counts: HashMap::new(),
            self_addr: None,
        }
    }
}

pub enum StateCmd {
    InitSelf {
        user: String,
        group: String,
        host: String,
        addr: SocketAddr,
    },
    ApplyEvent(Event),
    PushOutgoing(ChatMessage),
    UpdateProgress {
        from: SocketAddr,
        file_id: u32,
        packet_no: u32,
        target_outgoing: Option<bool>,
        progress: u64,
        file_name: Option<String>,
        local_path: Option<String>,
        saved: Option<bool>,
        error: Option<bool>,
        canceled: Option<bool>,
    },
    ClearUnread {
        addr: SocketAddr,
    },
}

pub static STATE_SNAPSHOT: Lazy<Arc<Mutex<CoreState>>> =
    Lazy::new(|| Arc::new(Mutex::new(CoreState::new())));
pub static STATE_SEQ: Lazy<AtomicU64> = Lazy::new(|| AtomicU64::new(0));
static STATE_CMD_TX: OnceCell<mpsc::Sender<StateCmd>> = OnceCell::new();

fn state_cmd_tx() -> &'static mpsc::Sender<StateCmd> {
    STATE_CMD_TX.get().expect("STATE_CMD_TX not initialized")
}

fn history_path() -> PathBuf {
    config::app_config_dir().join("history.json")
}

fn legacy_history_path() -> PathBuf {
    PathBuf::from(".").join("history.json")
}

fn normalize_addr(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip(), PORT)
}

fn default_self_addr() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), PORT)
}

pub async fn run_state_manager(mut rx: mpsc::Receiver<StateCmd>) {
    let mut state = CoreState::new();
    let history = history_path();
    let legacy_history = legacy_history_path();
    let history_content = if history.exists() {
        fs::read_to_string(&history).ok()
    } else {
        fs::read_to_string(&legacy_history).ok()
    };
    if let Some(content) = history_content {
        if let Ok(msgs) = serde_json::from_str::<Vec<ChatMessage>>(&content) {
            state.messages = msgs
                .into_iter()
                .map(|mut m| {
                    m.from = normalize_addr(m.from);
                    m.to = normalize_addr(m.to);
                    m
                })
                .collect();
            let _ = persist_history(&history, &state.messages);
        }
    }

    while let Some(cmd) = rx.recv().await {
        let mut changed = false;
        let mut should_persist = false;
        match cmd {
            StateCmd::InitSelf {
                user,
                group,
                host,
                addr,
            } => {
                let addr_norm = normalize_addr(addr);
                state.self_addr = Some(addr_norm);
                state.online_users.insert(
                    addr_norm,
                    OnlineUser {
                        name: user,
                        group,
                        host,
                        addr: addr_norm,
                    },
                );
                changed = true;
                should_persist = true;
            }
            StateCmd::ApplyEvent(ev) => match ev {
                Event::Online {
                    user,
                    group,
                    host,
                    addr,
                } => {
                    let addr_norm = normalize_addr(addr);
                    if Some(addr_norm) != state.self_addr {
                        state.online_users.insert(
                            addr_norm,
                            OnlineUser {
                                name: user,
                                group,
                                host,
                                addr: addr_norm,
                            },
                        );
                        changed = true;
                        should_persist = true;
                    }
                }
                Event::Offline { addr, .. } => {
                    let addr_norm = normalize_addr(addr);
                    if state.online_users.remove(&addr_norm).is_some() {
                        changed = true;
                        should_persist = true;
                    }
                }
                Event::Message {
                    from,
                    user,
                    host,
                    text,
                } => {
                    let from_norm = normalize_addr(from);
                    let to = state.self_addr.unwrap_or_else(default_self_addr);
                    state.messages.push(ChatMessage {
                        from: from_norm,
                        to,
                        is_me: false,
                        text,
                        time: "现在".into(),
                        file: None,
                    });
                    state.online_users.entry(from_norm).or_insert_with(|| OnlineUser {
                        name: user,
                        group: String::new(),
                        host,
                        addr: from_norm,
                    });
                    *state.unread_counts.entry(from_norm).or_insert(0) += 1;
                    changed = true;
                    should_persist = true;
                }
                Event::FileOffer {
                    from,
                    user,
                    host,
                    packet_no,
                    file_id,
                    name,
                    size,
                    is_dir,
                } => {
                    let from_norm = normalize_addr(from);
                    let to = state.self_addr.unwrap_or_else(default_self_addr);
                    let text = if is_dir {
                        format!("[文件夹] {}", name)
                    } else {
                        format!("[文件] {} ({})", name, format_size(size))
                    };
                    state.messages.push(ChatMessage {
                        from: from_norm,
                        to,
                        is_me: false,
                        text,
                        time: "现在".into(),
                        file: Some(FileInfo {
                            packet_no,
                            file_id,
                            name,
                            size,
                            saved: false,
                            received: 0,
                            is_dir,
                            local_path: None,
                            current_file: None,
                            error: false,
                            canceled: false,
                            sending: false,
                        }),
                    });
                    state.online_users.entry(from_norm).or_insert_with(|| OnlineUser {
                        name: user,
                        group: String::new(),
                        host,
                        addr: from_norm,
                    });
                    *state.unread_counts.entry(from_norm).or_insert(0) += 1;
                    changed = true;
                    should_persist = true;
                }
                Event::FileServed {
                    to,
                    packet_no,
                    file_id,
                    ..
                } => {
                    let to_norm = normalize_addr(to);
                    for message in state.messages.iter_mut().rev() {
                        if !message.is_me || normalize_addr(message.to) != to_norm {
                            continue;
                        }
                        if let Some(file) = &mut message.file {
                            if file.packet_no == packet_no && file.file_id == file_id {
                                file.sending = false;
                                file.saved = true;
                                file.error = false;
                                file.canceled = false;
                                file.received = file.size;
                                changed = true;
                                should_persist = true;
                                break;
                            }
                        }
                    }
                }
                Event::FileServingStarted {
                    to,
                    packet_no,
                    file_id,
                    ..
                } => {
                    let to_norm = normalize_addr(to);
                    for message in state.messages.iter_mut().rev() {
                        if !message.is_me || normalize_addr(message.to) != to_norm {
                            continue;
                        }
                        if let Some(file) = &mut message.file {
                            if file.packet_no == packet_no && file.file_id == file_id {
                                file.sending = true;
                                file.saved = false;
                                file.error = false;
                                file.canceled = false;
                                changed = true;
                                should_persist = true;
                                break;
                            }
                        }
                    }
                }
                _ => {}
            },
            StateCmd::PushOutgoing(msg) => {
                state.messages.push(msg);
                changed = true;
                should_persist = true;
            }
            StateCmd::UpdateProgress {
                from,
                file_id,
                packet_no,
                target_outgoing,
                progress,
                file_name,
                local_path,
                saved,
                error,
                canceled,
            } => {
                let from_norm = normalize_addr(from);
                for message in state.messages.iter_mut().rev() {
                    if let Some(target) = target_outgoing
                        && message.is_me != target
                    {
                        continue;
                    }
                    if let Some(file) = &mut message.file {
                        let matched_peer = if message.is_me {
                            normalize_addr(message.to) == from_norm
                        } else {
                            message.from == from_norm
                        };
                        if matched_peer && file.packet_no == packet_no && file.file_id == file_id {
                            file.received = progress;
                            if let Some(name) = file_name {
                                file.current_file = Some(name);
                            }
                            if let Some(path) = local_path {
                                file.local_path = Some(path);
                            }
                            if let Some(is_saved) = saved {
                                file.saved = is_saved;
                            }
                            if let Some(has_error) = error {
                                file.error = has_error;
                            }
                            if let Some(is_canceled) = canceled {
                                file.canceled = is_canceled;
                            }
                            if saved.unwrap_or(false)
                                || error.unwrap_or(false)
                                || canceled.unwrap_or(false)
                            {
                                should_persist = true;
                            }
                            break;
                        }
                    }
                }
                changed = true;
            }
            StateCmd::ClearUnread { addr } => {
                let addr_norm = normalize_addr(addr);
                if state.unread_counts.remove(&addr_norm).is_some() {
                    changed = true;
                }
            }
        }

        if changed {
            {
                let mut snap = STATE_SNAPSHOT.lock().unwrap();
                *snap = state.clone();
            }
            STATE_SEQ.fetch_add(1, Ordering::Relaxed);
            if should_persist {
                let _ = persist_history(&history, &state.messages);
            }
        }
    }
}

pub fn init_state() {
    let (tx, rx) = mpsc::channel(1024);
    let _ = STATE_CMD_TX.set(tx);
    let current_config = config::load_config();
    set_text_encoding(match current_config.language {
        config::LanguageEncoding::Utf8 => TextEncoding::Utf8,
        config::LanguageEncoding::Gb18030 => TextEncoding::Gb18030,
    });
    set_user_info(&current_config.user.username, &current_config.user.group);

    tokio::spawn(async move {
        tokio::spawn(run_state_manager(rx));
        if let Ok((rx, port)) = start_ipmsg().await {
            let mut ipmsg_rx = rx;
            let self_name = current_config.user.username.clone();
            let self_group = current_config.user.group.clone();
            let self_host = hostname::get()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let self_addr = detect_self_addr(port)
                .unwrap_or_else(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port));
            let _ = state_cmd_tx()
                .send(StateCmd::InitSelf {
                    user: self_name,
                    group: self_group,
                    host: self_host,
                    addr: self_addr,
                })
                .await;

            while let Ok(ev) = ipmsg_rx.recv().await {
                let _ = state_cmd_tx().send(StateCmd::ApplyEvent(ev)).await;
            }
        }
    });
}

pub fn dispatch_cmd(cmd: StateCmd) {
    if let Err(err) = state_cmd_tx().try_send(cmd) {
        match err {
            mpsc::error::TrySendError::Full(cmd) => {
                let tx = state_cmd_tx().clone();
                std::thread::spawn(move || {
                    let _ = tx.blocking_send(cmd);
                });
            }
            mpsc::error::TrySendError::Closed(_) => {}
        }
    }
}

pub fn list_online_users() -> Vec<OnlineUser> {
    let state = STATE_SNAPSHOT.lock().unwrap();
    let mut users: Vec<OnlineUser> = state.online_users.values().cloned().collect();
    users.sort_by(|a, b| a.addr.cmp(&b.addr));
    users
}

pub fn list_messages() -> Vec<ChatMessage> {
    let state = STATE_SNAPSHOT.lock().unwrap();
    state.messages.clone()
}

pub fn get_self_addr_info() -> Option<OnlineUser> {
    let state = STATE_SNAPSHOT.lock().unwrap();
    if let Some(addr) = state.self_addr {
        state.online_users.get(&addr).cloned()
    } else {
        None
    }
}

pub fn list_unread_counts() -> HashMap<SocketAddr, u32> {
    let state = STATE_SNAPSHOT.lock().unwrap();
    state.unread_counts.clone()
}

pub fn state_seq() -> u64 {
    STATE_SEQ.load(Ordering::Relaxed)
}

fn format_size(size: u64) -> String {
    let s = size as f64;
    if s < 1024.0 {
        format!("{:.0} B", s)
    } else if s < 1024.0 * 1024.0 {
        format!("{:.1} KB", s / 1024.0)
    } else if s < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", s / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", s / (1024.0 * 1024.0 * 1024.0))
    }
}

fn persist_history(path: &PathBuf, messages: &[ChatMessage]) -> std::io::Result<()> {
    let json = serde_json::to_string(messages).map_err(std::io::Error::other)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, json)
}
