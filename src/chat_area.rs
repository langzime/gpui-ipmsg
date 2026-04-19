use super::ChatShell;
use gpui::*;
use gpui_component::{
    ActiveTheme, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    resizable::{resizable_panel, v_resizable},
    scroll::ScrollableElement,
};

impl ChatShell {
    pub(super) fn render_chat_area(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let current_name = self
            .selected_conversation()
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "会话".to_string());
        let messages = self.messages_for_selected();
        let peer_avatar = current_name
            .chars()
            .next()
            .map(|ch| ch.to_string())
            .unwrap_or_else(|| "?".to_string());

        let mut message_list = div().v_flex().gap_2().p_4();
        for (index, message) in messages.iter().enumerate() {
            let text = message.text.clone();
            let mut bubble = div()
                .max_w(px(460.))
                .px_3()
                .py_2()
                .rounded_md()
                .bg(if message.from_me {
                    theme.primary.opacity(0.18)
                } else {
                    theme.secondary
                });
            if !text.is_empty() {
                bubble = bubble.child(
                    div()
                        .max_w(px(420.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(text),
                );
            }

            if let Some(transfer) = &message.file {
                let is_receiving = !transfer.saved
                    && !transfer.error
                    && !transfer.canceled
                    && (transfer.received > 0 || transfer.current_file.is_some());
                let status = if message.from_me {
                    if transfer.canceled {
                        "已取消".to_string()
                    } else if transfer.error {
                        "发送失败".to_string()
                    } else if transfer.saved {
                        "已发送".to_string()
                    } else if transfer.sending {
                        "发送中".to_string()
                    } else {
                        "待对方接收".to_string()
                    }
                } else if transfer.canceled {
                    "已取消".to_string()
                } else if transfer.error {
                    "下载失败".to_string()
                } else if transfer.saved {
                    "已保存".to_string()
                } else if is_receiving {
                    if transfer.is_dir {
                        "接收中".to_string()
                    } else if transfer.size > 0 {
                        let percent =
                            ((transfer.received as f64 / transfer.size as f64) * 100.0).clamp(0.0, 100.0);
                        format!(
                            "接收中: {}/{} ({:.0}%)",
                            transfer.received,
                            transfer.size,
                            percent
                        )
                    } else {
                        format!("接收中: {}", transfer.received)
                    }
                } else {
                    "待接收".to_string()
                };

                let mut status_row = div()
                    .mt_1()
                    .h_flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .w_full()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(status),
                    );

                if is_receiving && !message.from_me {
                    let transfer = transfer.clone();
                    status_row = status_row.child(
                        Button::new(format!(
                            "cancel-{}-{}",
                            transfer.packet_no, transfer.file_id
                        ))
                        .xsmall()
                        .ghost()
                        .label("取消接收")
                        .on_click(cx.listener(move |this, _, _, _cx| {
                            this.cancel_transfer(transfer.clone());
                        })),
                    );
                } else if transfer.local_path.is_some() && (message.from_me || transfer.saved) {
                    let transfer = transfer.clone();
                    status_row = status_row.child(
                        Button::new(format!(
                            "open-folder-{}-{}",
                            transfer.packet_no, transfer.file_id
                        ))
                        .xsmall()
                        .ghost()
                        .label("在文件夹中打开")
                        .on_click(cx.listener(move |this, _, _, _cx| {
                            this.open_transfer_in_folder(transfer.clone());
                        })),
                    );
                }

                bubble = bubble.child(status_row);

                if !message.from_me && !transfer.saved {
                    let transfer = transfer.clone();
                    if is_receiving {
                        if transfer.is_dir {
                            bubble = bubble.child(
                                div()
                                    .mt_2()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(
                                        transfer
                                            .current_file
                                            .clone()
                                            .map(|name| format!("正在接收文件: {}", name))
                                            .unwrap_or_else(|| "正在接收文件夹内容...".to_string()),
                                    ),
                            );
                        } else {
                            let bar_width = 280.0_f32;
                            let total = transfer.size.max(1) as f32;
                            let ratio = (transfer.received as f32 / total).clamp(0.0, 1.0);
                            bubble = bubble.child(
                                div()
                                    .mt_2()
                                    .v_flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .w(px(bar_width))
                                            .h(px(8.))
                                            .rounded_sm()
                                            .bg(theme.border)
                                            .child(
                                                div()
                                                    .h_full()
                                                    .rounded_sm()
                                                    .bg(theme.primary)
                                                    .w(px(bar_width * ratio)),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(format!("{:.0}%", ratio as f64 * 100.0)),
                                    ),
                            );
                        }
                    } else {
                        bubble = bubble.child(
                            div().mt_2().child(
                                Button::new(format!("recv-{}-{}", transfer.packet_no, transfer.file_id))
                                    .xsmall()
                                    .primary()
                                    .label("接收")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.receive_attachment(transfer.clone(), window, cx);
                                    })),
                            ),
                        );
                    }
                }
            }

            let avatar = if message.from_me {
                div()
                    .h_flex()
                    .w(px(28.))
                    .h(px(28.))
                    .mt(px(4.))
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(theme.primary)
                    .text_color(theme.primary_foreground)
                    .text_sm()
                    .child("我")
            } else {
                div()
                    .h_flex()
                    .w(px(28.))
                    .h(px(28.))
                    .mt(px(4.))
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(theme.secondary_hover)
                    .text_color(theme.secondary_foreground)
                    .text_sm()
                    .child(peer_avatar.clone())
            };

            let row = if message.from_me {
                div()
                    .h_flex()
                    .items_start()
                    .justify_end()
                    .gap_2()
                    .child(bubble)
                    .child(avatar)
            } else {
                div()
                    .h_flex()
                    .items_start()
                    .justify_start()
                    .gap_2()
                    .child(avatar)
                    .child(bubble)
            };

            message_list = message_list.child(div().id(format!("msg-{}", index)).child(row));
        }

        div()
            .v_flex()
            .size_full()
            .min_h_0()
            .child(
                div()
                    .h_flex()
                    .flex_none()
                    .h(px(48.))
                    .items_center()
                    .justify_between()
                    .px_4()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(current_name)
                    .child(Button::new("more").small().ghost().label("...")),
            )
            .child(
                div().v_flex().flex_1().min_h_0().w_full().child(
                    v_resizable("chat-vertical-resizable")
                        .child(
                            resizable_panel()
                                .size_range(px(140.)..Pixels::MAX)
                                .child(
                                    div()
                                        .v_flex()
                                        .flex_1()
                                        .min_h_0()
                                        .overflow_hidden()
                                        .child(
                                            div()
                                                .id("message-scroll")
                                                .v_flex()
                                                .size_full()
                                                .track_scroll(&self.message_scroll_handle)
                                                .overflow_y_scroll()
                                                .vertical_scrollbar(&self.message_scroll_handle)
                                                .child(message_list),
                                        ),
                                ),
                        )
                        .child(
                            resizable_panel()
                                .size(px(220.))
                                .size_range(px(180.)..px(420.))
                                .child(self.render_input_area(window, cx)),
                        ),
                ),
            )
    }
}
