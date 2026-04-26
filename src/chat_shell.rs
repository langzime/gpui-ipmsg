use crate::{app_state, config, logic};
use gpui::*;
use gpui_component::{
    ActiveTheme, Root, StyledExt, TitleBar,
    input::{InputEvent, InputState},
    resizable::{h_resizable, resizable_panel},
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

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
    pub(crate) from_me: bool,
    pub(crate) text: String,
    pub(crate) file: Option<FileTransfer>,
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

pub(crate) struct ChatShell {
    pub(crate) search_input: Entity<InputState>,
    pub(crate) compose_input: Entity<InputState>,
    pub(crate) message_scroll_handle: ScrollHandle,
    pub(crate) conversations: Vec<Conversation>,
    pub(crate) selected_conversation: usize,
    messages_by_conversation: HashMap<String, Vec<ChatMessage>>,
    pub(crate) search_text: String,
    pub(crate) compose_text: String,
    current_ui_language: config::UiLanguage,
    last_state_seq: u64,
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

    fn clear_selected_unread_if_needed(&mut self) {
        if !self.stick_to_bottom {
            return;
        }
        if let Some(conv) = self.conversations.get_mut(self.selected_conversation) {
            if conv.unread == 0 {
                return;
            }
            conv.unread = 0;
            if let Ok(addr) = conv.id.parse::<SocketAddr>() {
                app_state::dispatch_cmd(app_state::StateCmd::ClearUnread { addr });
            }
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
        let startup_config = config::load_config();
        let search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("chat.search_placeholder").to_string())
        });
        let compose_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("")
                .multi_line(true)
        });

        logic::ensure_started();

        let mut this = Self {
            search_input,
            compose_input,
            message_scroll_handle: ScrollHandle::default(),
            conversations: Vec::new(),
            selected_conversation: 0,
            messages_by_conversation: HashMap::new(),
            search_text: String::new(),
            compose_text: String::new(),
            current_ui_language: startup_config.ui_language,
            last_state_seq: 0,
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

        let _ = this.refresh_from_state();
        this.last_state_seq = app_state::state_seq();
        let poll_task = cx.spawn_in(window, async move |entity, cx| loop {
            cx.background_executor().timer(Duration::from_millis(250)).await;
            let seq = app_state::state_seq();
            let _ = entity.update_in(cx, |this, window, cx| {
                let latest_config = config::load_config();
                if this.current_ui_language != latest_config.ui_language {
                    this.current_ui_language = latest_config.ui_language;
                    this.update_localized_placeholders(window, cx);
                    cx.notify();
                }
                if this.last_state_seq != seq {
                    this.last_state_seq = seq;
                    let should_scroll_bottom = this.refresh_from_state();
                    if should_scroll_bottom && this.stick_to_bottom {
                        this.pending_scroll_to_bottom_frames = 2;
                    }
                    this.clear_selected_unread_if_needed();
                    cx.notify();
                }
            });
        });
        this._tasks.push(poll_task);
        this
    }

    pub(crate) fn selected_conversation(&self) -> Option<&Conversation> {
        self.conversations.get(self.selected_conversation)
    }

    pub(crate) fn messages_for_selected(&self) -> Vec<ChatMessage> {
        if let Some(conv) = self.selected_conversation() {
            return self
                .messages_by_conversation
                .get(&conv.id)
                .cloned()
                .unwrap_or_default();
        }
        Vec::new()
    }

    pub(crate) fn select_conversation(&mut self, index: usize) {
        if index < self.conversations.len() {
            self.selected_conversation = index;
            if let Some(conv) = self.conversations.get_mut(index) {
                conv.unread = 0;
                if let Ok(addr) = conv.id.parse::<SocketAddr>() {
                    app_state::dispatch_cmd(app_state::StateCmd::ClearUnread { addr });
                }
            }
            self.message_scroll_handle.scroll_to_bottom();
        }
    }

    pub(crate) fn send_current_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.compose_text.trim().to_string();
        if text.is_empty() {
            return;
        }

        let conv_index = self.selected_conversation;
        if conv_index >= self.conversations.len() {
            return;
        }
        let conv_id = self.conversations[conv_index].id.clone();
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
        let Some(conv) = self.selected_conversation() else {
            return;
        };
        let Ok(to) = conv.id.parse::<SocketAddr>() else {
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
        let Some(conv) = self.selected_conversation() else {
            return;
        };
        let Ok(to) = conv.id.parse::<SocketAddr>() else {
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

    fn refresh_from_state(&mut self) -> bool {
        let prev_selected_id = self.selected_conversation().map(|c| c.id.clone());
        let prev_selected_len = prev_selected_id
            .as_ref()
            .and_then(|id| self.messages_by_conversation.get(id))
            .map(std::vec::Vec::len)
            .unwrap_or(0);
        let users = app_state::list_online_users();
        let messages = app_state::list_messages();
        let unread_counts = app_state::list_unread_counts();
        let self_addr = app_state::get_self_addr_info().map(|u| u.addr);
        let current_cfg = config::load_config();

        let mut conversations = HashMap::<String, Conversation>::new();
        for user in users {
            let is_self = Some(user.addr) == self_addr;
            let id = user.addr.to_string();
            let display_name = if is_self {
                current_cfg.user.username.clone()
            } else {
                user.name.clone()
            };
            let display_group = if is_self {
                current_cfg.user.group.clone()
            } else {
                user.group.clone()
            };
            let subtitle = if display_group.is_empty() {
                format!("{} - {}", user.addr, user.host)
            } else {
                format!("{} - {}", user.addr, display_group)
            };
            conversations.insert(
                id.clone(),
                Conversation {
                    id,
                    name: display_name,
                    subtitle,
                    last_time: String::new(),
                    unread: unread_counts.get(&user.addr).copied().unwrap_or(0),
                },
            );
        }

        let mut messages_by_conversation = HashMap::<String, Vec<ChatMessage>>::new();
        for message in messages {
            let file_data = message.file.clone();
            let peer = if message.is_me {
                message.to
            } else {
                message.from
            };
            let id = peer.to_string();
            let text = if !message.text.is_empty() {
                message.text
            } else if let Some(file) = &file_data {
                if file.is_dir {
                    t!("file.folder_prefix", name = file.name.clone()).to_string()
                } else {
                    t!("file.file_prefix", name = file.name.clone()).to_string()
                }
            } else {
                String::new()
            };
            messages_by_conversation
                .entry(id.clone())
                .or_default()
                .push(ChatMessage {
                    from_me: message.is_me,
                    text: text.clone(),
                    file: file_data.map(|f| FileTransfer {
                        from: if message.is_me { message.to } else { message.from },
                        packet_no: f.packet_no,
                        file_id: f.file_id,
                        name: f.name,
                        size: f.size,
                        is_dir: f.is_dir,
                        saved: f.saved,
                        received: f.received,
                        current_file: f.current_file,
                        local_path: f.local_path,
                        error: f.error,
                        canceled: f.canceled,
                        sending: f.sending,
                    }),
                });

            conversations
                .entry(id.clone())
                .or_insert_with(|| Conversation {
                    id: id.clone(),
                    name: id.clone(),
                    subtitle: String::new(),
                    last_time: String::new(),
                    unread: unread_counts.get(&peer).copied().unwrap_or(0),
                })
                .subtitle = text;
        }

        let mut list: Vec<Conversation> = conversations.into_values().collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        let mut selected_index = 0usize;
        if let Some(selected_id) = prev_selected_id {
            if let Some(index) = list.iter().position(|c| c.id == selected_id) {
                selected_index = index;
            }
        }
        self.conversations = list;
        self.messages_by_conversation = messages_by_conversation;
        self.selected_conversation = selected_index.min(self.conversations.len().saturating_sub(1));
        let new_selected_len = self
            .selected_conversation()
            .and_then(|c| self.messages_by_conversation.get(&c.id))
            .map(std::vec::Vec::len)
            .unwrap_or(0);
        new_selected_len > prev_selected_len
    }
}

impl Drop for ChatShell {
    fn drop(&mut self) {
        std::process::exit(0);
    }
}

impl Render for ChatShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.stick_to_bottom = self.is_message_scroll_near_bottom();
        self.clear_selected_unread_if_needed();
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
