use crate::config;
use crate::ipmsg_core::{self, Event, TextEncoding, detect_self_addr, protocol::PORT};
use gpui::Global;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OnlineUser {
    pub name: String,
    pub group: String,
    pub host: String,
    pub addr: SocketAddr,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub from: SocketAddr,
    pub to: SocketAddr,
    pub is_me: bool,
    pub text: String,
    pub time: String,
    pub file: Option<FileInfo>,
    /// Outgoing message could not be handed to the network. Surface it in the
    /// UI instead of silently dropping the message (P0-5 in docs/architecture.md).
    #[serde(default)]
    pub failed: bool,
    /// packet_no of the SENDMSG we sent (for outgoing messages). Used to match
    /// the peer's RECVMSG delivery confirmation.
    #[serde(default)]
    pub packet_no: u32,
    /// Peer's client confirmed receipt via IPMSG_RECVMSG.
    #[serde(default)]
    pub delivered: bool,
    /// 本进程内定位消息的句柄：`MessageUpdated` 原地替换与重试流程按 id 定位。
    /// 跨重启无消费者，故不持久化（`#[serde(skip)]`：加载时为 0，由状态 actor
    /// 统一重编号，保证运行中全局唯一）。
    #[serde(skip)]
    pub id: u64,
}

#[derive(Clone)]
pub struct CoreState {
    pub online_users: HashMap<SocketAddr, OnlineUser>,
    pub messages: Vec<ChatMessage>,
    pub unread_counts: HashMap<SocketAddr, u32>,
    pub self_addr: Option<SocketAddr>,
    /// 本机用户名与主机名。部分客户端（如 Android 版飞鸽）会把收到的报文头
    /// 原样回带（username/host 字段填的是我们的信息），建对端条目前需要比对，
    /// 避免把对端条目存成本机昵称。
    pub self_name: String,
    pub self_host: String,
    /// packet_nos of SENDMSG packets whose RECVMSG ack arrived before the
    /// matching outgoing message was pushed to state (race guard). Bounded.
    pub acked_packets: HashSet<u32>,
}

impl CoreState {
    fn new() -> Self {
        Self {
            online_users: HashMap::new(),
            messages: Vec::new(),
            unread_counts: HashMap::new(),
            self_addr: None,
            self_name: String::new(),
            self_host: String::new(),
            acked_packets: HashSet::new(),
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
    /// 文本消息发出后 N 秒仍未收到对端 RECVMSG 确认（对端离线/端口不可达，
    /// UDP send_to 不会本地报错），把该气泡标记为失败以提供重试入口。
    AckTimeout {
        packet_no: u32,
    },
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
    /// 上翻加载某会话更早的历史消息。`before_id` 为 UI 侧当前最旧消息的 id
    /// （id 顺序即插入顺序，actor 取 `id < before_id` 的最近 `limit` 条），
    /// 由 actor 内存全量直接应答，无需回读 history.json。
    LoadMore {
        addr: SocketAddr,
        before_id: u64,
        limit: usize,
    },
    /// Result of a retry send (user clicked 重试 on a failed bubble).
    RetryFinished {
        id: u64,
        ok: bool,
        packet_no: u32,
        file_id: u32,
    },
    /// 设置已保存（UI 语言等可能已切换），通知 UI 刷新本地化文案。
    SettingsSaved,
}

/// 状态增量：状态 actor 推给 UI 的载荷，用于消除 250ms 轮询
/// （docs/architecture.md §4.2）。状态层运行在 tokio 线程上，无法直接持有
/// GPUI 上下文，因此经由无界通道跨线程，由 UI 侧桥接任务在主线程转发给
/// `UiState` Entity（EventEmitter）。
#[derive(Clone, Debug)]
pub enum StateDelta {
    /// 全量消息同步（actor 启动时发出，通道先缓冲保序，UI 后接入也能拿到）。
    /// 每会话最多 `SYNC_WINDOW` 条，`has_more` 标记各会话是否还有更早未同步
    /// 的消息（上翻经 LoadMore 按需拉取）。
    Sync {
        messages: Vec<ChatMessage>,
        has_more: HashMap<SocketAddr, bool>,
    },
    /// 在线用户变化（自条目始终包含在内，显示名即配置昵称）。
    UsersChanged {
        users: Vec<OnlineUser>,
    },
    /// 追加一条消息（收到的文本/文件请求、成功发出的消息）。
    MessageAdded {
        message: ChatMessage,
    },
    /// 单条消息字段更新（送达确认、传输进度、失败标记、重试成功）。
    /// 以 `message.id` 定位，接收方原地替换。
    MessageUpdated {
        message: ChatMessage,
    },
    /// 上翻加载到的更早历史消息（chronological 序，prepend 到会话列表头部）。
    /// `has_more=false` 表示该会话已到底。
    MessagesLoaded {
        addr: SocketAddr,
        messages: Vec<ChatMessage>,
        has_more: bool,
    },
    /// 某会话未读数变化。
    UnreadChanged {
        addr: SocketAddr,
        unread: u32,
    },
    /// 设置已保存，UI 需刷新本地化文案。
    SettingsChanged,
}

/// 状态层唯一实例：持有命令/增量通道、快照、消息 id 序列与下载任务注册表
/// （docs/architecture.md §4.2 中期项 1：全局静态收拢）。状态 actor 与协议
/// pump 运行在 tokio 线程，经 `Arc` 共享；UI 主线程经 GPUI Global 注入。
pub struct AppState {
    cmd_tx: mpsc::Sender<StateCmd>,
    cmd_rx: Mutex<Option<mpsc::Receiver<StateCmd>>>,
    delta_tx: mpsc::UnboundedSender<StateDelta>,
    delta_rx: Mutex<Option<mpsc::UnboundedReceiver<StateDelta>>>,
    snapshot: Arc<Mutex<CoreState>>,
    next_message_id: AtomicU64,
    active_downloads: Mutex<HashMap<String, JoinHandle<()>>>,
}

/// GPUI Global 包装：`Arc<AppState>` 是外部类型，按孤儿规则不能直接实现
/// `Global`，因此套一层 newtype（GPUI Global 文档推荐做法）。
#[derive(Clone)]
pub struct AppStateGlobal(pub Arc<AppState>);

impl Global for AppStateGlobal {}

impl AppStateGlobal {
    pub fn arc(&self) -> &Arc<AppState> {
        &self.0
    }
}

static APP_STATE: OnceLock<Arc<AppState>> = OnceLock::new();

pub fn set_instance(app_state: Arc<AppState>) {
    let _ = APP_STATE.set(app_state);
}

/// The registered state instance. Panics if `set_instance` has not been called
/// (i.e. before `logic::ensure_started` completes its synchronous part).
pub fn app_state() -> &'static Arc<AppState> {
    APP_STATE.get().expect("app state not initialized")
}

const ACKED_PACKETS_MAX: usize = 4096;

/// 初始同步每会话的窗口大小：更早的历史上翻时经 `LoadMore` 按需加载，
/// 避免启动即把全部历史克隆到 UI。
const SYNC_WINDOW: usize = 10;
/// 每次 `LoadMore` 返回的条数（UI 侧引用此常量派发命令）。
pub(crate) const LOAD_MORE_LIMIT: usize = 10;

/// 文本消息发出后等待对端 RECVMSG 确认的时限。局域网内正常应答在毫秒级；
/// 超时即视为对端未收到（UDP send_to 对已关闭端口依然返回成功）。
const ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

impl AppState {
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel(1024);
        let (delta_tx, delta_rx) = mpsc::unbounded_channel();
        Self {
            cmd_tx,
            cmd_rx: Mutex::new(Some(cmd_rx)),
            delta_tx,
            delta_rx: Mutex::new(Some(delta_rx)),
            snapshot: Arc::new(Mutex::new(CoreState::new())),
            next_message_id: AtomicU64::new(1),
            active_downloads: Mutex::new(HashMap::new()),
        }
    }

    // ---- 对外命令入口 ----

    /// 向状态 actor 派发一条命令（tokio 线程与 UI 主线程均可调用）。
    pub fn dispatch(&self, cmd: StateCmd) {
        if let Err(err) = self.cmd_tx.try_send(cmd) {
            match err {
                mpsc::error::TrySendError::Full(cmd) => {
                    let tx = self.cmd_tx.clone();
                    std::thread::spawn(move || {
                        let _ = tx.blocking_send(cmd);
                    });
                }
                mpsc::error::TrySendError::Closed(_) => {}
            }
        }
    }

    /// UI 侧取走 delta 接收端（只应调用一次；桥接任务与状态 actor 同生命周期）。
    /// 无论 UI 与 actor 谁先访问，通道都在 `AppState::new` 一次性建好，不存在竞态。
    pub fn take_delta_rx(&self) -> Option<mpsc::UnboundedReceiver<StateDelta>> {
        self.delta_rx.lock().unwrap().take()
    }

    pub fn get_self_addr_info(&self) -> Option<OnlineUser> {
        let state = self.snapshot.lock().unwrap();
        if let Some(addr) = state.self_addr {
            state.online_users.get(&addr).cloned()
        } else {
            None
        }
    }

    // ---- 下载任务注册表（原 logic::ACTIVE_DOWNLOADS） ----

    pub fn take_download(&self, key: &str) -> Option<JoinHandle<()>> {
        self.active_downloads.lock().unwrap().remove(key)
    }

    pub fn insert_download(&self, key: String, handle: JoinHandle<()>) {
        self.active_downloads.lock().unwrap().insert(key, handle);
    }

    // ---- 启动 ----

    /// 在 tokio runtime 内启动状态 actor 与协议事件泵（原 `init_state`）。
    /// `AppState` 与 `set_instance` 已在 `logic::ensure_started` 同步创建。
    pub fn init(self: &Arc<Self>) {
        let current_config = config::load_config();
        rust_i18n::set_locale(current_config.ui_language.as_locale());
        let self_name = current_config.user.username.clone();
        let self_group = current_config.user.group.clone();
        let self_host = hostname::get()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let language = current_config.language;

        let me = Arc::clone(self);
        tokio::spawn(async move { me.run_state_manager().await; });

        let me = Arc::clone(self);
        tokio::spawn(async move {
            me.pump_events(language, self_name, self_group, self_host)
                .await;
        });
    }

    async fn pump_events(
        self: &Arc<Self>,
        language: config::LanguageEncoding,
        self_name: String,
        self_group: String,
        self_host: String,
    ) {
        let service = match ipmsg_core::start_ipmsg().await {
            Ok(service) => service,
            Err(e) => {
                log::warn!("start_ipmsg failed: {}", e);
                return;
            }
        };
        service.set_text_encoding(match language {
            config::LanguageEncoding::Utf8 => TextEncoding::Utf8,
            config::LanguageEncoding::Gb18030 => TextEncoding::Gb18030,
        });
        service.set_user_info(&self_name, &self_group);
        if let Err(e) = service.spawn().await {
            log::warn!("ipmsg service spawn failed: {}", e);
            return;
        }
        let port = service.port;
        let self_addr = detect_self_addr(port)
            .unwrap_or_else(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port));
        let _ = self
            .cmd_tx
            .send(StateCmd::InitSelf {
                user: self_name,
                group: self_group,
                host: self_host,
                addr: self_addr,
            })
            .await;

        // Lagged must NOT terminate the pump: it only means some events
        // were dropped because we fell behind. Exiting the loop here would
        // permanently silence all network events (P0-1 in docs/architecture.md).
        let mut ipmsg_rx = service.events.subscribe();
        loop {
            match ipmsg_rx.recv().await {
                Ok(ev) => {
                    let _ = self.cmd_tx.send(StateCmd::ApplyEvent(ev)).await;
                }
                Err(RecvError::Lagged(missed)) => {
                    log::warn!("ipmsg event pump lagged, {} events lost", missed);
                }
                Err(RecvError::Closed) => break,
            }
        }
    }

    fn next_message_id(&self) -> u64 {
        self.next_message_id.fetch_add(1, Ordering::Relaxed)
    }

    fn emit_delta(&self, delta: StateDelta) {
        // 无界通道的 send 只在接收端被丢弃时失败；通道保序，UI 按序应用增量。
        let _ = self.delta_tx.send(delta);
    }

    /// 为一条等待确认的文本消息安装超时检查：到期仍未确认则派发 AckTimeout，
    /// 由状态线程把气泡标记为失败（已确认的消息不受影响）。
    fn arm_ack_timer(&self, packet_no: u32) {
        let tx = self.cmd_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(ACK_TIMEOUT).await;
            let _ = tx.send(StateCmd::AckTimeout { packet_no }).await;
        });
    }

    /// 状态 actor 主循环：消费 `StateCmd`，维护 `CoreState` 快照并推送增量。
    async fn run_state_manager(self: &Arc<Self>) {
        let mut rx = self
            .cmd_rx
            .lock()
            .unwrap()
            .take()
            .expect("state actor already started");
        let mut state = CoreState::new();
        let history = history_path();
        let legacy_history = legacy_history_path();
        let history_content = if history.exists() {
            fs::read_to_string(&history).ok()
        } else {
            fs::read_to_string(&legacy_history).ok()
        };
        if let Some(content) = history_content
            && let Ok(msgs) = serde_json::from_str::<Vec<ChatMessage>>(&content)
        {
            state.messages = msgs
                .into_iter()
                .map(|mut m| {
                    // 无条件重编号：id 不持久化（serde skip），是进程内句柄。
                    // 历史文件里残留旧版本的跨会话重复 id，若沿用会按 id 定位
                    // 更新（送达/进度/失败/重试）时命中错误消息。加载时统一
                    // 领用新序号，运行中全局唯一。不再回写磁盘（id 无跨重启
                    // 消费者，启动重写 history.json 是纯副作用）。
                    m.id = self.next_message_id();
                    m.from = normalize_addr(m.from);
                    m.to = normalize_addr(m.to);
                    m
                })
                .collect();
        }

        // 初始全量同步：通道先缓冲、UI 后接入也能拿到历史（保序）。每会话
        // 只发最近 SYNC_WINDOW 条，更早的上翻时经 LoadMore 按需拉取，避免
        // 启动即克隆全部历史。actor 内存仍持有全量（快照/持久化用它）。
        let mut sync_messages = Vec::new();
        let mut has_more = HashMap::<SocketAddr, bool>::new();
        {
            let mut grouped = HashMap::<SocketAddr, Vec<ChatMessage>>::new();
            for m in &state.messages {
                grouped
                    .entry(message_peer_addr(m))
                    .or_default()
                    .push(m.clone());
            }
            for (addr, mut msgs) in grouped {
                let more = msgs.len() > SYNC_WINDOW;
                has_more.insert(addr, more);
                if more {
                    msgs.drain(..msgs.len() - SYNC_WINDOW);
                }
                sync_messages.extend(msgs);
            }
        }
        self.emit_delta(StateDelta::Sync {
            messages: sync_messages,
            has_more,
        });

        while let Some(cmd) = rx.recv().await {
            let mut changed = false;
            let mut should_persist = false;
            // 本条命令若恰好更新了一条消息，记录其克隆用于 MessageUpdated 增量。
            let mut updated_message: Option<ChatMessage> = None;
            match cmd {
                StateCmd::InitSelf {
                    user,
                    group,
                    host,
                    addr,
                } => {
                    let addr_norm = normalize_addr(addr);
                    state.self_addr = Some(addr_norm);
                    state.self_name = user.clone();
                    state.self_host = host.clone();
                    state.online_users.insert(
                        addr_norm,
                        OnlineUser {
                            name: user,
                            group,
                            host,
                            addr: addr_norm,
                        },
                    );
                    self.emit_delta(StateDelta::UsersChanged {
                        users: sorted_users(&state),
                    });
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
                        // 回显报文头防御：username+host 都与本机一致时，说明对端
                        // 回带了我们的报文头（或本机多网卡广播回环），不能据此
                        // 建立对端条目，否则对端会话会显示成本机昵称。
                        if Some(addr_norm) != state.self_addr
                            && !packet_header_echoes_self(&state, &user, &host)
                        {
                            state.online_users.insert(
                                addr_norm,
                                OnlineUser {
                                    name: user,
                                    group,
                                    host,
                                    addr: addr_norm,
                                },
                            );
                            self.emit_delta(StateDelta::UsersChanged {
                                users: sorted_users(&state),
                            });
                            changed = true;
                            should_persist = true;
                        }
                    }
                    Event::Offline { addr, .. } => {
                        let addr_norm = normalize_addr(addr);
                        if state.online_users.remove(&addr_norm).is_some() {
                            self.emit_delta(StateDelta::UsersChanged {
                                users: sorted_users(&state),
                            });
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
                        let msg = ChatMessage {
                            from: from_norm,
                            to,
                            is_me: false,
                            text,
                            time: t!("time.now").to_string(),
                            file: None,
                            failed: false,
                            packet_no: 0,
                            delivered: false,
                            id: self.next_message_id(),
                        };
                        state.messages.push(msg.clone());
                        self.emit_delta(StateDelta::MessageAdded { message: msg });
                        // 对端回带本机报文头时，报文里的 user/host 不可信，
                        // 不要据此建立对端条目（会话列表将回退显示地址）。
                        if !packet_header_echoes_self(&state, &user, &host)
                            && !state.online_users.contains_key(&from_norm)
                        {
                            state.online_users.insert(
                                from_norm,
                                OnlineUser {
                                    name: user,
                                    group: String::new(),
                                    host,
                                    addr: from_norm,
                                },
                            );
                            self.emit_delta(StateDelta::UsersChanged {
                                users: sorted_users(&state),
                            });
                        }
                        let unread = {
                            let entry = state.unread_counts.entry(from_norm).or_insert(0);
                            *entry += 1;
                            *entry
                        };
                        self.emit_delta(StateDelta::UnreadChanged {
                            addr: from_norm,
                            unread,
                        });
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
                            t!("file.folder_prefix", name = name.clone()).to_string()
                        } else {
                            t!("file.file_with_size", name = name.clone(), size = format_size(size)).to_string()
                        };
                        let msg = ChatMessage {
                            from: from_norm,
                            to,
                            is_me: false,
                            text,
                            time: t!("time.now").to_string(),
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
                            failed: false,
                            packet_no: 0,
                            delivered: false,
                            id: self.next_message_id(),
                        };
                        state.messages.push(msg.clone());
                        self.emit_delta(StateDelta::MessageAdded { message: msg });
                        // 同 Event::Message：回带本机报文头时不建立对端条目。
                        if !packet_header_echoes_self(&state, &user, &host)
                            && !state.online_users.contains_key(&from_norm)
                        {
                            state.online_users.insert(
                                from_norm,
                                OnlineUser {
                                    name: user,
                                    group: String::new(),
                                    host,
                                    addr: from_norm,
                                },
                            );
                            self.emit_delta(StateDelta::UsersChanged {
                                users: sorted_users(&state),
                            });
                        }
                        let unread = {
                            let entry = state.unread_counts.entry(from_norm).or_insert(0);
                            *entry += 1;
                            *entry
                        };
                        self.emit_delta(StateDelta::UnreadChanged {
                            addr: from_norm,
                            unread,
                        });
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
                            if let Some(file) = &mut message.file
                                && file.packet_no == packet_no
                                && file.file_id == file_id
                            {
                                file.sending = false;
                                file.saved = true;
                                file.error = false;
                                file.canceled = false;
                                file.received = file.size;
                                changed = true;
                                should_persist = true;
                                updated_message = Some(message.clone());
                                break;
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
                            if let Some(file) = &mut message.file
                                && file.packet_no == packet_no
                                && file.file_id == file_id
                            {
                                file.sending = true;
                                file.saved = false;
                                file.error = false;
                                file.canceled = false;
                                changed = true;
                                should_persist = true;
                                updated_message = Some(message.clone());
                                break;
                            }
                        }
                    }
                    Event::FileServeFailed {
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
                            if let Some(file) = &mut message.file
                                && file.packet_no == packet_no
                                && file.file_id == file_id
                            {
                                // 对端中止接收等导致发送中断：从"发送中"转为失败。
                                // 我方主动取消（cancel_upload 已置 canceled）时此事件
                                // 不会发出，这里仍加 canceled 守卫防竞态。
                                if !file.canceled {
                                    file.sending = false;
                                    file.saved = false;
                                    file.error = true;
                                    changed = true;
                                    should_persist = true;
                                    updated_message = Some(message.clone());
                                    log::info!(
                                        "FileServeFailed -> error set, to_norm={} pkt={:x} fid={:x}",
                                        to_norm, packet_no, file_id
                                    );
                                }
                                break;
                            }
                        }
                    }
                    Event::Delivered { from, packet_no } => {
                        // Peer's RECVMSG: delivery confirmation for an outgoing
                        // message/file offer with the given packet_no.
                        if packet_no != 0 {
                            if state.acked_packets.len() >= ACKED_PACKETS_MAX {
                                state.acked_packets.clear();
                            }
                            state.acked_packets.insert(packet_no);
                        }
                        let from_norm = normalize_addr(from);
                        for message in state.messages.iter_mut().rev() {
                            if !message.is_me || normalize_addr(message.to) != from_norm {
                                continue;
                            }
                            let matches = message.packet_no == packet_no
                                || message
                                    .file
                                    .as_ref()
                                    .map(|f| f.packet_no == packet_no)
                                    .unwrap_or(false);
                            if matches {
                                message.delivered = true;
                                // 确认迟到（晚于 ACK_TIMEOUT 标记失败）时恢复气泡状态。
                                message.failed = false;
                                changed = true;
                                should_persist = true;
                                updated_message = Some(message.clone());
                                break;
                            }
                        }
                    }
                    _ => {}
                },
                StateCmd::PushOutgoing(mut msg) => {
                    // 所有调用方（logic.rs）都传 id=0，此处无条件分配；与历史
                    // 加载、收到的消息/文件共用同一计数器，actor 单任务内顺序
                    // 执行，id 天然不重不漏。
                    msg.id = self.next_message_id();
                    // Race guard: the peer's RECVMSG can be processed by the event
                    // pump before this task dispatches PushOutgoing, in which case
                    // the ack was recorded in acked_packets with no message to
                    // attach it to yet.
                    if msg.packet_no != 0 && state.acked_packets.contains(&msg.packet_no) {
                        msg.delivered = true;
                    }
                    // 纯文本消息走 SENDCHECKOPT：超时未确认要标记失败并提供重试
                    // （文件消息有自己的传输状态流，不在此列）。
                    let watch_ack = msg.packet_no != 0 && !msg.delivered && msg.file.is_none();
                    let watch_packet_no = msg.packet_no;
                    state.messages.push(msg.clone());
                    self.emit_delta(StateDelta::MessageAdded { message: msg });
                    changed = true;
                    should_persist = true;
                    if watch_ack {
                        self.arm_ack_timer(watch_packet_no);
                    }
                }
                StateCmd::AckTimeout { packet_no } => {
                    for message in state.messages.iter_mut().rev() {
                        if message.is_me
                            && message.packet_no == packet_no
                            && message.file.is_none()
                        {
                            if !message.delivered && !message.failed {
                                message.failed = true;
                                changed = true;
                                should_persist = true;
                                updated_message = Some(message.clone());
                            }
                            break;
                        }
                    }
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
                                changed = true;
                                updated_message = Some(message.clone());
                                break;
                            }
                        }
                    }
                }
                StateCmd::ClearUnread { addr } => {
                    let addr_norm = normalize_addr(addr);
                    if state.unread_counts.remove(&addr_norm).is_some() {
                        self.emit_delta(StateDelta::UnreadChanged {
                            addr: addr_norm,
                            unread: 0,
                        });
                        changed = true;
                    }
                }
                StateCmd::LoadMore {
                    addr,
                    before_id,
                    limit,
                } => {
                    let addr_norm = normalize_addr(addr);
                    // state.messages 按插入顺序（id 递增），过滤保序，取最旧侧
                    // 的最近 limit 条；只改增量、不触快照/持久化。
                    let older: Vec<ChatMessage> = state
                        .messages
                        .iter()
                        .filter(|m| message_peer_addr(m) == addr_norm && m.id < before_id)
                        .cloned()
                        .collect();
                    let has_more = older.len() > limit;
                    let start = older.len().saturating_sub(limit);
                    self.emit_delta(StateDelta::MessagesLoaded {
                        addr: addr_norm,
                        messages: older[start..].to_vec(),
                        has_more,
                    });
                }
                StateCmd::RetryFinished {
                    id,
                    ok,
                    packet_no,
                    file_id,
                } => {
                    if !ok {
                        continue;
                    }
                    for message in state.messages.iter_mut().rev() {
                        if message.id == id {
                            message.failed = false;
                            message.delivered = false;
                            message.packet_no = packet_no;
                            if let Some(file) = &mut message.file {
                                file.packet_no = packet_no;
                                file.file_id = file_id;
                            }
                            // Same race guard as PushOutgoing: the peer may have
                            // acked the retried packet before we got here.
                            if packet_no != 0 && state.acked_packets.contains(&packet_no) {
                                message.delivered = true;
                            }
                            changed = true;
                            should_persist = true;
                            updated_message = Some(message.clone());
                            // 重发后同样要等待确认，超时再次标记失败。
                            if packet_no != 0 && !message.delivered && message.file.is_none() {
                                self.arm_ack_timer(packet_no);
                            }
                            break;
                        }
                    }
                }
                StateCmd::SettingsSaved => {
                    self.emit_delta(StateDelta::SettingsChanged);
                }
            }

            if let Some(message) = updated_message {
                self.emit_delta(StateDelta::MessageUpdated { message });
            }

            if changed {
                {
                    let mut snap = self.snapshot.lock().unwrap();
                    *snap = state.clone();
                }
                if should_persist {
                    let _ = persist_history(&history, &state.messages);
                }
            }
        }
    }
}

fn sorted_users(state: &CoreState) -> Vec<OnlineUser> {
    let mut users: Vec<OnlineUser> = state.online_users.values().cloned().collect();
    users.sort_by_key(|u| u.addr);
    users
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

/// 消息所属会话：发出的看 `to`，收到的看 `from`（与 chat_shell::peer_addr 一致）。
fn message_peer_addr(m: &ChatMessage) -> SocketAddr {
    if m.is_me {
        m.to
    } else {
        m.from
    }
}

fn default_self_addr() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), PORT)
}

/// 判断报文声称的 user/host 是否与本机完全一致。
/// 部分客户端（如 Android 版飞鸽）会把收到的报文头原样回带，
/// 此时报文里的身份信息不可信，不能用于建立对端条目。
fn packet_header_echoes_self(state: &CoreState, user: &str, host: &str) -> bool {
    !state.self_name.is_empty()
        && user == state.self_name
        && !state.self_host.is_empty()
        && host == state.self_host
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

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_self(name: &str, host: &str) -> CoreState {
        let mut state = CoreState::new();
        state.self_name = name.to_string();
        state.self_host = host.to_string();
        state
    }

    /// 对端回带本机报文头时（Android 版飞鸽等），不得据此建立对端条目，
    /// 否则会话列表会把对端显示成本机昵称。
    #[test]
    fn echoed_packet_header_is_detected() {
        let state = state_with_self("台式机", "DESKTOP-ABC");
        assert!(packet_header_echoes_self(&state, "台式机", "DESKTOP-ABC"));
        // 仅用户名相同（常见于同 OS 账号）不算回显。
        assert!(!packet_header_echoes_self(&state, "台式机", "Android-Phone"));
        // 仅主机名相同不算回显。
        assert!(!packet_header_echoes_self(&state, "LOMO", "DESKTOP-ABC"));
    }

    /// 本机信息未初始化时（self_name/self_host 为空）不应误判。
    #[test]
    fn uninitialized_self_never_matches() {
        let state = CoreState::new();
        assert!(!packet_header_echoes_self(&state, "", ""));
        assert!(!packet_header_echoes_self(&state, "台式机", "DESKTOP-ABC"));
    }
}
