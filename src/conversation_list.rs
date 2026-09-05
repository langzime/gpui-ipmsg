use super::ChatShell;
use gpui::*;
use gpui::prelude::FluentBuilder;
use gpui_component::{
    ActiveTheme, Sizable, StyledExt,
    button::Button,
    input::Input,
};

impl ChatShell {
    pub(super) fn render_conversation_list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let search = self.search_text.trim().to_lowercase();
        let conversations = self.ui_state.read(cx).conversations().to_vec();
        let selected_id = self.selected_id.clone();

        let mut list = div().v_flex().gap_1().px_2();
        for conv in &conversations {
            if !search.is_empty() {
                let haystack = format!("{} {}", conv.name.to_lowercase(), conv.subtitle.to_lowercase());
                if !haystack.contains(&search) {
                    continue;
                }
            }

            let is_selected = selected_id.as_ref() == Some(&conv.id);
            let avatar = conv
                .name
                .chars()
                .next()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".to_string());
            let unread_badge = conv.unread.to_string();
            let conv_id = conv.id.clone();

            list = list.child(
                div()
                    .id(format!("conv-{}", conv.id))
                    .w_full()
                    .p_2()
                    .rounded_md()
                    .bg(if is_selected {
                        theme.secondary_active
                    } else {
                        theme.background
                    })
                    .child(
                        div()
                            .h_flex()
                            .items_start()
                            .gap_2()
                            .w_full()
                            .child(
                                div()
                                    .h_flex()
                                    .w(px(34.))
                                    .h(px(34.))
                                    .flex_none()
                                    .rounded_md()
                                    .bg(theme.secondary)
                                    .items_center()
                                    .justify_center()
                                    .text_sm()
                                    .child(avatar),
                            )
                            .child(
                                div()
                                    .v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .child(
                                        div()
                                            .h_flex()
                                            .justify_between()
                                            .w_full()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .text_sm()
                                                    .overflow_hidden()
                                                    .text_ellipsis()
                                                    .whitespace_nowrap()
                                                    .child(conv.name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(conv.last_time.clone()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .h_flex()
                                            .justify_between()
                                            .w_full()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .text_xs()
                                                    .overflow_hidden()
                                                    .text_ellipsis()
                                                    .whitespace_nowrap()
                                                    .text_color(theme.muted_foreground)
                                                    .child(conv.subtitle.clone()),
                                            )
                                            .when(conv.unread > 0, |this| {
                                                this.child(
                                                    div()
                                                        .flex_none()
                                                        .text_xs()
                                                        .px_2()
                                                        .rounded_lg()
                                                        .bg(theme.primary)
                                                        .text_color(theme.primary_foreground)
                                                        .child(unread_badge.clone()),
                                                )
                                            }),
                                    ),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_conversation(conv_id.clone(), cx);
                    })),
            );
        }

        div()
            .v_flex()
            .w_full()
            .h_full()
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .child(Input::new(&self.search_input).small())
                    .child(Button::new("new-chat").small().label("+")),
            )
            .when(self.search_text.trim().is_empty() && conversations.is_empty(), |this| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t!("chat.no_conversations").to_string()),
                )
            })
            // .when(!self.search_text.trim().is_empty(), |this| {
            //     this.child(
            //         div()
            //             .px_3()
            //             .py_1()
            //             .text_xs()
            //             .text_color(theme.muted_foreground)
            //             .child(format!("结果: {}", self.search_text)),
            //     )
            // })
            .child(list)
    }
}
