use crate::{app_state, logic};
use gpui::*;
use gpui_component::{
    ActiveTheme, Root, StyledExt, TitleBar,
    input::{InputEvent, InputState},
    resizable::{h_resizable, resizable_panel},
};
use std::collections::HashMap;
use std::net::SocketAddr;

#[derive(Clone)]
pub(crate) struct Conversation {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) subtitle: String,
    pub(crate) last_time: String,
    pub(crate) unread: u32,
}

#[derive(Clone)]
pub(crate) struct ChatMessage {
    pub(crate) id: u64,
    pub(crate) to: SocketAddr,
    pub(crate) from_me: bool,
    pub(crate) text: String,
    pub(crate) file: Option<FileTransfer>,
    pub(crate) failed: bool,
    pub(crate) delivered: bool,
}

#[derive(Clone)]
pub(crate) struct FileTransfer {
    pub(crate) from: SocketAddr,
    pub(crate) packet_no: u32,
    pub(crate) file_id: u32,
    pub(crate) name: String,
    pub(crate) size: u64,
    pub(crate) is_dir: bool,
    pub(crate) saved: bool,
    pub(crate) received: u64,
    pub(crate) current_file: Option<String>,
    pub(crate) local_path: Option<String>,
    pub(crate) error: bool,
    pub(crate) canceled: bool,
    pub(crate) sending: bool,
}

/// UI 侧共享状态的 Entity：由状态 actor 的 `StateDelta` 推送驱动更新，
/// 并通过 `EventEmitter<UiEvent>` 通知订阅者。ChatShell 由此只保留选择、
/// 滚动、输入草稿等本地态（docs/architecture.md §4.2：事件推送替代轮询）。
pub(crate) struct UiState {
    conversations: Vec<Conversation>,
    messages_by_conversation: HashMap<String, Vec<ChatMessage>>,
    unread: HashMap<SocketAddr, u32>,
    users: Vec<app_state::OnlineUser>,
}

#[derive(Debug)]
pub(crate) enum UiEvent {
    /// 新消息进入某会话（订阅方据此决定是否自动滚动到底部）。
    MessageAdded { conv_id: String },
    /// 设置已保存，UI 语言可能已切换。
    SettingsChanged,
    /// 其余数据变化（初始同步、用户列表、传输进度、未读）。
    Changed,
}

impl EventEmitter<UiEvent> for UiState {}

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
            let subtitle = msgs.last().map(|m| m.text.clone()).unwrap_or_default();
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
            app_state::StateDelta::Sync { messages } => {
                let mut grouped = HashMap::<String, Vec<ChatMessage>>::new();
                for message in &messages {
                    let view = to_view_message(message);
                    grouped.entry(view.to.to_string()).or_default().push(view);
                }
                self.messages_by_conversation = grouped;
                self.rebuild_conversations();
                cx.emit(UiEvent::Changed);
            }
            app_state::StateDelta::UsersChanged { users } => {
                self.users = users;
                self.rebuild_conversations();
                cx.emit(UiEvent::Changed);
            }
            app_state::StateDelta::MessageAdded { message } => {
                let view = to_view_message(&message);
                let conv_id = view.to.to_string();
                self.messages_by_conversation
                    .entry(conv_id.clone())
                    .or_default()
                    .push(view);
                self.rebuild_conversations();
                cx.emit(UiEvent::MessageAdded { conv_id });
            }
            app_state::StateDelta::MessageUpdated { message } => {
                let view = to_view_message(&message);
                let conv_id = view.to.to_string();
                if let Some(list) = self.messages_by_conversation.get_mut(&conv_id)
                    && let Some(pos) = list.iter().position(|m| m.id == view.id)
                {
                    list[pos] = view;
                }
                cx.emit(UiEvent::Changed);
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

    pub(crate) fn messages_for(&self, conv_id: &str) -> &[ChatMessage] {
        self.messages_by_conversation
            .get(conv_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

fn to_view_message(message: &app_state::ChatMessage) -> ChatMessage {
    let peer = if message.is_me {
        message.to
    } else {
        message.from
    };
    let text = if !message.text.is_empty() {
        message.text.clone()
    } else if let Some(file) = &message.file {
        if file.is_dir {
            t!("file.folder_prefix", name = file.name.clone()).to_string()
        } else {
            t!("file.file_prefix", name = file.name.clone()).to_string()
        }
    } else {
        String::new()
    };
    ChatMessage {
        id: message.id,
        to: peer,
        from_me: message.is_me,
        text,
        file: message.file.as_ref().map(|f| FileTransfer {
            from: peer,
            packet_no: f.packet_no,
            file_id: f.file_id,
            name: f.name.clone(),
            size: f.size,
            is_dir: f.is_dir,
            saved: f.saved,
            received: f.received,
            current_file: f.current_file.clone(),
            local_path: f.local_path.clone(),
            error: f.error,
            canceled: f.canceled,
            sending: f.sending,
        }),
        failed: message.failed,
        delivered: message.delivered,
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
    pending_scroll_to_bottom_frames: u8,
    stick_to_bottom: bool,
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
            app_state::dispatch_cmd(app_state::StateCmd::ClearUnread { addr });
        }
    }

    fn is_message_scroll_near_bottom(&self) -> bool {
        // In this scroll model, current offset is negative and max_offset is positive.
        // Near bottom means their sum is near zero (e.g. -6296 + 6296 ~= 0).
        let current = self.message_scroll_handle.offset().y;
        let bottom = self.message_scroll_handle.max_offset().y;
        (current + bottom).abs() <= px(80.)
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

        let ui_state = cx.new(|_| UiState {
            conversations: Vec::new(),
            messages_by_conversation: HashMap::new(),
            unread: HashMap::new(),
            users: Vec::new(),
        });

        let mut this = Self {
            ui_state,
            search_input,
            compose_input,
            message_scroll_handle: ScrollHandle::default(),
            selected_id: None,
            search_text: String::new(),
            compose_text: String::new(),
            pending_scroll_to_bottom_frames: 0,
            stick_to_bottom: true,
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
                match event {
                    UiEvent::MessageAdded { conv_id } => {
                        if this.selected_id.as_ref() == Some(conv_id) && this.stick_to_bottom {
                            this.pending_scroll_to_bottom_frames = 2;
                        }
                    }
                    UiEvent::SettingsChanged => {
                        this.update_localized_placeholders(window, cx);
                    }
                    UiEvent::Changed => {}
                }
                cx.notify();
            },
        );
        this._subscriptions.push(ui_sub);

        // 状态 actor → UiState 桥接：actor 推 StateDelta，本任务在主线程
        // 应用到 UiState Entity 并经 EventEmitter 通知。替代原先的 250ms 轮询。
        if let Some(mut delta_rx) = app_state::take_delta_rx() {
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
            app_state::dispatch_cmd(app_state::StateCmd::ClearUnread { addr });
        }
        self.message_scroll_handle.scroll_to_bottom();
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
            logic::send_text(to, text);
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
        cx.spawn_in(window, async move |_, _| {
            let Some(paths) = rfd::AsyncFileDialog::new().pick_files().await else {
                return;
            };
            let selected = paths
                .into_iter()
                .map(|p| p.path().to_string_lossy().to_string())
                .collect::<Vec<_>>();
            if !selected.is_empty() {
                logic::send_files(to, selected);
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
        cx.spawn_in(window, async move |_, _| {
            let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await else {
                return;
            };
            logic::send_folder(to, folder.path().to_string_lossy().to_string());
        })
        .detach();
    }

    pub(crate) fn receive_attachment(
        &mut self,
        transfer: FileTransfer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if transfer.saved {
            return;
        }
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
                logic::download_folder(transfer.from, transfer.packet_no, transfer.file_id, save_path);
            } else {
                let Some(path) = rfd::AsyncFileDialog::new()
                    .set_file_name(&transfer.name)
                    .save_file()
                    .await
                else {
                    return;
                };
                logic::download_file(
                    transfer.from,
                    transfer.packet_no,
                    transfer.file_id,
                    transfer.size,
                    path.path().to_string_lossy().to_string(),
                );
            }
        })
        .detach();
    }

    pub(crate) fn open_transfer_in_folder(&mut self, transfer: FileTransfer) {
        if let Some(path) = transfer.local_path {
            logic::open_in_folder(path, transfer.is_dir);
        }
    }

    pub(crate) fn cancel_transfer(&mut self, transfer: FileTransfer, from_me: bool) {
        if from_me {
            logic::cancel_upload(transfer.from, transfer.packet_no, transfer.file_id);
        } else {
            logic::cancel_download(transfer.from, transfer.packet_no, transfer.file_id);
        }
    }

    pub(crate) fn retry_message(&mut self, message: ChatMessage) {
        if !message.failed {
            return;
        }
        let file = message
            .file
            .as_ref()
            .and_then(|f| f.local_path.clone().map(|p| (p, f.is_dir)));
        logic::retry_message(message.id, message.to, message.text, file);
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
