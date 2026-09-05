use crate::app_state::{self, AppState};
use crate::{logic};
use gpui::*;
use gpui_component::{
    ActiveTheme, Root, StyledExt, TitleBar,
    input::{InputEvent, InputState},
    resizable::{h_resizable, resizable_panel},
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct Conversation {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) subtitle: String,
    pub(crate) last_time: String,
    pub(crate) unread: u32,
}

/// UI 侧共享状态的 Entity：由状态 actor 的 `StateDelta` 推送驱动更新，
/// 并通过 `EventEmitter<UiEvent>` 通知订阅者。ChatShell 由此只保留选择、
/// 滚动、输入草稿等本地态（docs/architecture.md §4.2：事件推送替代轮询）。
/// 消息直接使用 `app_state::ChatMessage` 领域模型，仅做投影（会话列表项、
/// 展示文本），不再维护第三套消息表示（中期项 2）。
pub(crate) struct UiState {
    conversations: Vec<Conversation>,
    messages_by_conversation: HashMap<String, Vec<app_state::ChatMessage>>,
    unread: HashMap<SocketAddr, u32>,
    users: Vec<app_state::OnlineUser>,
    /// 各会话是否还有未同步的更早历史（上翻经 LoadMore 按需加载）。
    has_more: HashMap<SocketAddr, bool>,
}

#[derive(Debug)]
pub(crate) enum UiEvent {
    /// 新消息进入某会话（订阅方据此决定是否自动滚动到底部）。
    MessageAdded { conv_id: String },
    /// 上翻加载到更早历史；`count` 为实际 prepend 条数，订阅方据此恢复滚动位置。
    MessagesLoaded { conv_id: String, count: usize },
    /// 设置已保存，UI 语言可能已切换。
    SettingsChanged,
    /// 其余数据变化（初始同步、用户列表、传输进度、未读）。
    Changed,
}

impl EventEmitter<UiEvent> for UiState {}

/// 会话归属：以对端地址为稳定 id。
fn peer_addr(message: &app_state::ChatMessage) -> SocketAddr {
    if message.is_me {
        message.to
    } else {
        message.from
    }
}

/// 展示文本投影：领域消息不携带本地化气泡文案时，依据附件类型生成。
pub(crate) fn display_text(message: &app_state::ChatMessage) -> String {
    if !message.text.is_empty() {
        message.text.clone()
    } else if let Some(file) = &message.file {
        if file.is_dir {
            t!("file.folder_prefix", name = file.name.clone()).to_string()
        } else {
            t!("file.file_prefix", name = file.name.clone()).to_string()
        }
    } else {
        String::new()
    }
}

impl UiState {
    /// 由 users + messages + unread 重建会话列表（按名称排序、取每会话
    /// 最后一条消息做预览）。成本 O(#users + #convs)，与消息总量无关。
    fn rebuild_conversations(&mut self) {
        let mut convs = HashMap::<String, Conversation>::new();
        for user in &self.users {
            let id = user.addr.to_string();
            let subtitle = if user.group.is_empty() {
                format!("{} - {}", user.addr, user.host)
            } else {
                format!("{} - {}", user.addr, user.group)
            };
            convs.insert(
                id.clone(),
                Conversation {
                    id,
                    name: user.name.clone(),
                    subtitle,
                    last_time: String::new(),
                    unread: 0,
                },
            );
        }
        for (id, msgs) in &self.messages_by_conversation {
            let subtitle = msgs.last().map(display_text).unwrap_or_default();
            convs.entry(id.clone())
                .or_insert_with(|| Conversation {
                    id: id.clone(),
                    name: id.clone(),
                    subtitle: String::new(),
                    last_time: String::new(),
                    unread: 0,
                })
                .subtitle = subtitle;
        }
        for conv in convs.values_mut() {
            if let Ok(addr) = conv.id.parse::<SocketAddr>() {
                conv.unread = self.unread.get(&addr).copied().unwrap_or(0);
            }
        }
        let mut list: Vec<Conversation> = convs.into_values().collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        self.conversations = list;
    }

    fn apply(&mut self, delta: app_state::StateDelta, cx: &mut Context<Self>) {
        match delta {
            app_state::StateDelta::Sync {
                messages,
                has_more,
            } => {
                let mut grouped = HashMap::<String, Vec<app_state::ChatMessage>>::new();
                for message in &messages {
                    grouped
                        .entry(peer_addr(message).to_string())
                        .or_default()
                        .push(message.clone());
                }
                self.messages_by_conversation = grouped;
                self.has_more = has_more;
                self.rebuild_conversations();
                cx.emit(UiEvent::Changed);
            }
            app_state::StateDelta::UsersChanged { users } => {
                self.users = users;
                self.rebuild_conversations();
                cx.emit(UiEvent::Changed);
            }
            app_state::StateDelta::MessageAdded { message } => {
                let conv_id = peer_addr(&message).to_string();
                self.messages_by_conversation
                    .entry(conv_id.clone())
                    .or_default()
                    .push(message);
                self.rebuild_conversations();
                cx.emit(UiEvent::MessageAdded { conv_id });
            }
            app_state::StateDelta::MessageUpdated { message } => {
                let conv_id = peer_addr(&message).to_string();
                let hit = if let Some(list) = self.messages_by_conversation.get_mut(&conv_id)
                    && let Some(pos) = list.iter().position(|m| m.id == message.id)
                {
                    list[pos] = message.clone();
                    true
                } else {
                    false
                };
                log::info!(
                    "ui: MessageUpdated id={} conv={} hit={} file_error={} sending={}",
                    message.id,
                    conv_id,
                    hit,
                    message.file.as_ref().map(|f| f.error).unwrap_or(false),
                    message.file.as_ref().map(|f| f.sending).unwrap_or(false),
                );
                cx.emit(UiEvent::Changed);
            }
            app_state::StateDelta::MessagesLoaded {
                addr,
                messages,
                has_more,
            } => {
                // 批量更早历史 prepend 到会话头部。按 id 去重：极端情况下同一
                // 游标可能被重复派发（会话切换等竞态），避免重复插入。
                let id = addr.to_string();
                self.has_more.insert(addr, has_more);
                let list = self
                    .messages_by_conversation
                    .entry(id.clone())
                    .or_default();
                let mut older = messages;
                let existing: std::collections::HashSet<u64> =
                    list.iter().map(|m| m.id).collect();
                older.retain(|m| !existing.contains(&m.id));
                let count = older.len();
                older.append(list);
                *list = older;
                cx.emit(UiEvent::MessagesLoaded { conv_id: id, count });
            }
            app_state::StateDelta::UnreadChanged { addr, unread } => {
                self.unread.insert(addr, unread);
                let id = addr.to_string();
                if let Some(conv) = self.conversations.iter_mut().find(|c| c.id == id) {
                    conv.unread = unread;
                }
                cx.emit(UiEvent::Changed);
            }
            app_state::StateDelta::SettingsChanged => {
                cx.emit(UiEvent::SettingsChanged);
            }
        }
    }

    pub(crate) fn conversations(&self) -> &[Conversation] {
        &self.conversations
    }

    pub(crate) fn conversation(&self, id: &str) -> Option<&Conversation> {
        self.conversations.iter().find(|c| c.id == id)
    }

    pub(crate) fn messages_for(&self, conv_id: &str) -> &[app_state::ChatMessage] {
        self.messages_by_conversation
            .get(conv_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// 上翻判定输入：该会话是否还有更早历史（has_more），以及当前最旧消息的
    /// id（作为 LoadMore 的 `before_id` 游标）。
    pub(crate) fn oldest_id_and_has_more(&self, conv_id: &str) -> (bool, Option<u64>) {
        let has_more = self
            .messages_by_conversation
            .get(conv_id)
            .and_then(|_| {
                let addr = conv_id.parse::<SocketAddr>().ok()?;
                self.has_more.get(&addr).copied()
            })
            .unwrap_or(false);
        let oldest = self
            .messages_by_conversation
            .get(conv_id)
            .and_then(|msgs| msgs.first().map(|m| m.id));
        (has_more, oldest)
    }
}

pub(crate) struct ChatShell {
    pub(crate) ui_state: Entity<UiState>,
    pub(crate) search_input: Entity<InputState>,
    pub(crate) compose_input: Entity<InputState>,
    pub(crate) message_scroll_handle: ScrollHandle,
    /// 选中会话的稳定 id（SocketAddr 字符串）。选择是 UI 本地态。
    pub(crate) selected_id: Option<String>,
    pub(crate) search_text: String,
    pub(crate) compose_text: String,
    pub(crate) app_state: Arc<AppState>,
    pending_scroll_to_bottom_frames: u8,
    stick_to_bottom: bool,
    /// 上翻加载更早历史：节流标记（防重复派发）与滚动锚点（prepend 后恢复
    /// 视口位置，记录的是派发时顶部可见行的子元素索引）。
    loading_more: bool,
    load_more_anchor: Option<usize>,
    _subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

impl ChatShell {
    fn update_localized_placeholders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_input.update(cx, |input, cx| {
            input.set_placeholder(t!("chat.search_placeholder").to_string(), window, cx);
        });
        self.compose_input.update(cx, |input, cx| {
            input.set_placeholder(String::new(), window, cx);
        });
    }

    fn clear_selected_unread_if_needed(&mut self, cx: &mut Context<Self>) {
        if !self.stick_to_bottom {
            return;
        }
        let Some(selected_id) = self.selected_id.clone() else {
            return;
        };
        let cleared = self.ui_state.update(cx, |state, _| {
            state
                .conversations
                .iter_mut()
                .find(|c| c.id == selected_id)
                .map(|conv| std::mem::replace(&mut conv.unread, 0) > 0)
                .unwrap_or(false)
        });
        if cleared && let Ok(addr) = selected_id.parse::<SocketAddr>() {
            self.app_state
                .dispatch(app_state::StateCmd::ClearUnread { addr });
        }
    }

    fn is_message_scroll_near_bottom(&self) -> bool {
        // In this scroll model, current offset is negative and max_offset is positive.
        // Near bottom means their sum is near zero (e.g. -6296 + 6296 ~= 0).
        let current = self.message_scroll_handle.offset().y;
        let bottom = self.message_scroll_handle.max_offset().y;
        (current + bottom).abs() <= px(80.)
    }

    /// 上翻接近顶部且该会话还有更早历史时，按需加载一批（LoadMore）。
    /// 与 `stick_to_bottom` 同模式在 render 里检测；`loading_more` 节流保证
    /// 同一时刻最多一个在途请求。内容不足一屏时 offset 恒为 0，也会命中此
    /// 分支——这正好让短会话自动加载完整个历史。
    fn maybe_load_more_history(&mut self, cx: &mut Context<Self>) {
        if self.loading_more {
            return;
        }
        // offset <= 0，接近顶部即接近 0。
        if self.message_scroll_handle.offset().y >= px(-24.) {
            let Some(selected_id) = self.selected_id.clone() else {
                return;
            };
            let Ok(addr) = selected_id.parse::<SocketAddr>() else {
                return;
            };
            let (has_more, oldest) = self
                .ui_state
                .read(cx)
                .oldest_id_and_has_more(&selected_id);
            let Some(before_id) = oldest else {
                return;
            };
            if !has_more {
                return;
            }
            self.load_more_anchor = Some(self.message_scroll_handle.top_item());
            self.loading_more = true;
            self.app_state.dispatch(app_state::StateCmd::LoadMore {
                addr,
                before_id,
                limit: app_state::LOAD_MORE_LIMIT,
            });
        }
    }

    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("chat.search_placeholder").to_string())
        });
        let compose_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("")
                .multi_line(true)
        });

        logic::ensure_started();
        // 状态实例在 `ensure_started` 中同步创建；注入 GPUI Global 供设置窗口等
        // 任意主线程代码读取，UI 侧不再触碰任何全局静态（中期项 1）。
        let app_state = app_state::app_state().clone();
        cx.set_global(app_state::AppStateGlobal(app_state.clone()));

        let ui_state = cx.new(|_| UiState {
            conversations: Vec::new(),
            messages_by_conversation: HashMap::new(),
            unread: HashMap::new(),
            users: Vec::new(),
            has_more: HashMap::new(),
        });

        let mut this = Self {
            ui_state,
            search_input,
            compose_input,
            message_scroll_handle: ScrollHandle::default(),
            selected_id: None,
            search_text: String::new(),
            compose_text: String::new(),
            app_state,
            pending_scroll_to_bottom_frames: 0,
            stick_to_bottom: true,
            loading_more: false,
            load_more_anchor: None,
            _subscriptions: Vec::new(),
            _tasks: Vec::new(),
        };

        let search_sub = cx.subscribe_in(
            &this.search_input,
            window,
            |this, input_state, event, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.search_text = input_state.read(cx).value().to_string();
                    cx.notify();
                }
            },
        );
        this._subscriptions.push(search_sub);

        let compose_sub = cx.subscribe_in(
            &this.compose_input,
            window,
            |this, input_state, event, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.compose_text = input_state.read(cx).value().to_string();
                    cx.notify();
                }
                if matches!(event, InputEvent::PressEnter { secondary: false }) {
                    this.compose_text = input_state.read(cx).value().to_string();
                    this.send_current_message(window, cx);
                }
            },
        );
        this._subscriptions.push(compose_sub);

        // UiState → ChatShell：数据变化通知 + 选中会话的自动滚动策略。
        let ui_sub = cx.subscribe_in(
            &this.ui_state,
            window,
            |this, _, event, window, cx| {
                log::info!("chat_shell: UiEvent {:?}", event);
                match event {
                    UiEvent::MessageAdded { conv_id } => {
                        if this.selected_id.as_ref() == Some(conv_id) && this.stick_to_bottom {
                            this.pending_scroll_to_bottom_frames = 2;
                        }
                    }
                    UiEvent::SettingsChanged => {
                        this.update_localized_placeholders(window, cx);
                    }
                    UiEvent::MessagesLoaded { conv_id, count } => {
                        // 恢复视口：新行插到列表头部，原锚点行（派发时顶部可见）
                        // 位移到 anchor+count，FirstVisible 策略最小滚动保持可见。
                        if this.selected_id.as_ref() == Some(conv_id) {
                            if this.stick_to_bottom {
                                // 处于底部（内容不足一屏自动加载等）：保持钉底，
                                // 否则加载完历史后视口会停在最旧消息处。
                                this.message_scroll_handle.scroll_to_bottom();
                            } else if let Some(anchor) = this.load_more_anchor {
                                this.message_scroll_handle.scroll_to_item(anchor + count);
                            }
                        }
                        // 节流与锚点复位；若结果对应已切换的会话，select_conversation
                        // 已重置过这两个标记，这里再清一次无副作用。
                        this.load_more_anchor = None;
                        this.loading_more = false;
                    }
                    UiEvent::Changed => {}
                }
                cx.notify();
            },
        );
        this._subscriptions.push(ui_sub);

        // 状态 actor → UiState 桥接：actor 推 StateDelta，本任务在主线程
        // 应用到 UiState Entity 并经 EventEmitter 通知。替代原先的 250ms 轮询。
        if let Some(mut delta_rx) = this.app_state.take_delta_rx() {
            let ui_state = this.ui_state.clone();
            let bridge = cx.spawn_in(window, async move |_, cx| {
                while let Some(delta) = delta_rx.recv().await {
                    let _ = ui_state.update_in(cx, |state, _, cx| state.apply(delta, cx));
                }
            });
            this._tasks.push(bridge);
        }

        this
    }

    pub(crate) fn select_conversation(&mut self, conv_id: String, cx: &mut Context<Self>) {
        self.selected_id = Some(conv_id.clone());
        self.ui_state.update(cx, |state, _| {
            if let Some(conv) = state.conversations.iter_mut().find(|c| c.id == conv_id) {
                conv.unread = 0;
            }
        });
        if let Ok(addr) = conv_id.parse::<SocketAddr>() {
            self.app_state
                .dispatch(app_state::StateCmd::ClearUnread { addr });
        }
        self.message_scroll_handle.scroll_to_bottom();
        // 切换会话：清掉上翻加载的在途标记，避免旧会话的结果污染新会话。
        self.loading_more = false;
        self.load_more_anchor = None;
        cx.notify();
    }

    pub(crate) fn send_current_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.compose_text.trim().to_string();
        if text.is_empty() {
            return;
        }

        let Some(conv_id) = self.selected_id.clone() else {
            return;
        };
        if let Ok(to) = conv_id.parse::<SocketAddr>() {
            logic::send_text(to, text, &self.app_state);
        }
        self.compose_text.clear();
        self.compose_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        cx.notify();
    }

    pub(crate) fn send_files_for_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(conv_id) = self.selected_id.clone() else {
            return;
        };
        let Ok(to) = conv_id.parse::<SocketAddr>() else {
            return;
        };
        let app_state = self.app_state.clone();
        cx.spawn_in(window, async move |_, _| {
            let Some(paths) = rfd::AsyncFileDialog::new().pick_files().await else {
                return;
            };
            let selected = paths
                .into_iter()
                .map(|p| p.path().to_string_lossy().to_string())
                .collect::<Vec<_>>();
            if !selected.is_empty() {
                logic::send_files(to, selected, &app_state);
            }
        })
        .detach();
    }

    pub(crate) fn send_folder_for_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(conv_id) = self.selected_id.clone() else {
            return;
        };
        let Ok(to) = conv_id.parse::<SocketAddr>() else {
            return;
        };
        let app_state = self.app_state.clone();
        cx.spawn_in(window, async move |_, _| {
            let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await else {
                return;
            };
            logic::send_folder(to, folder.path().to_string_lossy().to_string(), &app_state);
        })
        .detach();
    }

    pub(crate) fn receive_attachment(
        &mut self,
        peer: SocketAddr,
        transfer: app_state::FileInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if transfer.saved {
            return;
        }
        let app_state = self.app_state.clone();
        cx.spawn_in(window, async move |_, _| {
            if transfer.is_dir {
                let Some(parent) = rfd::AsyncFileDialog::new().pick_folder().await else {
                    return;
                };
                let save_path = parent
                    .path()
                    .join(&transfer.name)
                    .to_string_lossy()
                    .to_string();
                logic::download_folder(peer, transfer.packet_no, transfer.file_id, save_path, &app_state);
            } else {
                let Some(path) = rfd::AsyncFileDialog::new()
                    .set_file_name(&transfer.name)
                    .save_file()
                    .await
                else {
                    return;
                };
                logic::download_file(
                    peer,
                    transfer.packet_no,
                    transfer.file_id,
                    transfer.size,
                    path.path().to_string_lossy().to_string(),
                    &app_state,
                );
            }
        })
        .detach();
    }

    pub(crate) fn open_transfer_in_folder(&mut self, transfer: app_state::FileInfo) {
        if let Some(path) = transfer.local_path {
            logic::open_in_folder(path, transfer.is_dir);
        }
    }

    pub(crate) fn cancel_transfer(
        &mut self,
        peer: SocketAddr,
        transfer: app_state::FileInfo,
        from_me: bool,
    ) {
        if from_me {
            logic::cancel_upload(peer, transfer.packet_no, transfer.file_id, &self.app_state);
        } else {
            logic::cancel_download(peer, transfer.packet_no, transfer.file_id, &self.app_state);
        }
    }

    pub(crate) fn retry_message(&mut self, message: app_state::ChatMessage) {
        if !message.failed {
            return;
        }
        let file = message
            .file
            .as_ref()
            .and_then(|f| f.local_path.clone().map(|p| (p, f.is_dir)));
        logic::retry_message(
            message.id,
            peer_addr(&message),
            display_text(&message),
            file,
            &self.app_state,
        );
    }
}

impl Drop for ChatShell {
    fn drop(&mut self) {
        logic::shutdown();
    }
}

impl Render for ChatShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.stick_to_bottom = self.is_message_scroll_near_bottom();
        // 列表首次填充后默认选中第一个会话（选择是 UI 本地态）；
        // 首次选中且该会话已有消息时滚到底部，保证启动首屏看到最新消息。
        if self.selected_id.is_none() {
            self.selected_id = self
                .ui_state
                .read(cx)
                .conversations()
                .first()
                .map(|c| c.id.clone());
            if self.stick_to_bottom
                && let Some(id) = self.selected_id.as_ref()
                && !self.ui_state.read(cx).messages_for(id).is_empty()
            {
                self.pending_scroll_to_bottom_frames = 2;
            }
        }
        self.clear_selected_unread_if_needed(cx);
        self.maybe_load_more_history(cx);
        if self.pending_scroll_to_bottom_frames > 0 {
            self.message_scroll_handle.scroll_to_bottom();
            self.pending_scroll_to_bottom_frames -= 1;
            if self.pending_scroll_to_bottom_frames > 0 {
                cx.notify();
            }
        }
        let sidebar = self.render_sidebar(cx);
        let conversation_list = self.render_conversation_list(cx);
        let chat_area = self.render_chat_area(window, cx);
        let theme = cx.theme();

        div()
            .h_flex()
            .size_full()
            .bg(theme.background)
            .child(sidebar)
            .child(
                div()
                    .v_flex()
                    .size_full()
                    .min_h_0()
                    .child(
                        TitleBar::new().child(
                            div()
                                .h_flex()
                                .h_full()
                                .items_center()
                                .px_1()
                                .child(div().text_sm().font_semibold().child("")),
                        ),
                    )
                    .child(
                        div().v_flex().flex_1().min_h_0().child(
                            h_resizable("chat-content-resizable")
                                .child(
                                    resizable_panel()
                                        .size(px(220.))
                                        .size_range(px(220.)..px(520.))
                                        .child(conversation_list),
                                )
                                .child(
                                    resizable_panel()
                                        .child(div().v_flex().flex_1().h_full().min_h_0().child(chat_area)),
                                ),
                        ),
                    ),
            )
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_sheet_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}
