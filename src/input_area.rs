use super::ChatShell;
use gpui::*;
use gpui_component::{
    ActiveTheme, Disableable, IconName, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    input::Input,
};

impl ChatShell {
    pub(super) fn render_input_area(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let has_text = !self.compose_text.trim().is_empty();
        let send_button = if has_text {
            Button::new("send")
                .xsmall()
                .primary()
                .rounded_lg()
                .px_2()
                .h(px(24.))
                .label(t!("chat.send").to_string())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.send_current_message(window, cx);
                }))
        } else {
            Button::new("send")
                .xsmall()
                .ghost()
                .rounded_lg()
                .px_2()
                .h(px(24.))
                .disabled(true)
                .bg(theme.secondary.opacity(0.4))
                .text_color(theme.muted_foreground.opacity(0.85))
                .border_1()
                .border_color(theme.border.opacity(0.65))
                .label(t!("chat.send").to_string())
        };

        div()
            .v_flex()
            .w_full()
            .h_full()
            .min_h_0()
            .p_3()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .v_flex()
                    .w_full()
                    .h_full()
                    .min_h_0()
                    .rounded_2xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .min_h_0()
                            .overflow_hidden()
                            .px_4()
                            .pt_3()
                            .pb_3()
                            .child(
                                Input::new(&self.compose_input)
                                    .w_full()
                                    .h_full()
                                    .appearance(false)
                                    .bordered(false)
                                    .focus_bordered(false)
                                    .cleanable(false),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .h_flex()
                            .items_center()
                            .justify_between()
                            .px_3()
                            .py_1()
                            .border_t_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .h_flex()
                                    .items_center()
                                    .gap_4()
                                    .child(
                                        Button::new("file")
                                            .xsmall()
                                            .ghost()
                                            .rounded_full()
                                            .w(px(24.))
                                            .h(px(24.))
                                            .text_color(theme.muted_foreground)
                                            .icon(IconName::File)
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.send_files_for_selected(window, cx);
                                            })),
                                    )
                                    .child(
                                        Button::new("folder")
                                            .xsmall()
                                            .ghost()
                                            .rounded_full()
                                            .w(px(24.))
                                            .h(px(24.))
                                            .text_color(theme.muted_foreground)
                                            .icon(IconName::Folder)
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.send_folder_for_selected(window, cx);
                                            })),
                                    ),
                            )
                            .child(send_button),
                    ),
            )
    }
}
