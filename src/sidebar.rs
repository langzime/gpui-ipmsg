use super::ChatShell;
use crate::logic;
use gpui::*;
use gpui::prelude::FluentBuilder;
use gpui_component::{
    ActiveTheme, IconName, Root, Sizable, StyledExt, TitleBar,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
};
use std::sync::Mutex;

static SETTINGS_WINDOW_HANDLE: Mutex<Option<AnyWindowHandle>> = Mutex::new(None);

struct SettingsWindowView {
    username_input: Entity<InputState>,
    group_input: Entity<InputState>,
    username: String,
    group: String,
    status_text: String,
    _subscriptions: Vec<Subscription>,
}

impl SettingsWindowView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let config = logic::get_config();
        let username = config.user.username;
        let group = config.user.group;

        let username_input = cx.new(|cx| InputState::new(window, cx).placeholder("昵称"));
        let group_input = cx.new(|cx| InputState::new(window, cx).placeholder("分组"));

        username_input.update(cx, |state, cx| state.set_value(&username, window, cx));
        group_input.update(cx, |state, cx| state.set_value(&group, window, cx));

        let mut this = Self {
            username_input,
            group_input,
            username,
            group,
            status_text: String::new(),
            _subscriptions: Vec::new(),
        };

        let username_sub = cx.subscribe_in(
            &this.username_input,
            window,
            |this, input_state, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.username = input_state.read(cx).value().to_string();
                    cx.notify();
                }
            },
        );
        this._subscriptions.push(username_sub);

        let group_sub = cx.subscribe_in(
            &this.group_input,
            window,
            |this, input_state, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.group = input_state.read(cx).value().to_string();
                    cx.notify();
                }
            },
        );
        this._subscriptions.push(group_sub);

        this
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let username = self.username.trim().to_string();
        let group = self.group.trim().to_string();
        if username.is_empty() {
            self.status_text = "昵称不能为空".to_string();
            cx.notify();
            return;
        }
        match logic::save_settings(username, group) {
            Ok(()) => self.status_text = "设置已保存并已广播上线信息".to_string(),
            Err(error) => self.status_text = format!("保存失败: {}", error),
        }
        cx.notify();
    }
}

impl Drop for SettingsWindowView {
    fn drop(&mut self) {
        *SETTINGS_WINDOW_HANDLE.lock().unwrap() = None;
    }
}

impl Render for SettingsWindowView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        div()
            .v_flex()
            .size_full()
            .bg(theme.background)
            .child(
                TitleBar::new().child(
                    div()
                        .h_flex()
                        .h_full()
                        .items_center()
                        .px_1()
                        .child(div().text_sm().font_semibold().child("设置")),
                ),
            )
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .p_4()
                    .gap_3()
                    .child(div().text_sm().child("昵称"))
                    .child(Input::new(&self.username_input))
                    .child(div().text_sm().child("分组"))
                    .child(Input::new(&self.group_input))
                    .child(
                        div()
                            .h_flex()
                            .justify_end()
                            .child(
                                Button::new("save-settings")
                                    .small()
                                    .primary()
                                    .label("保存")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.save(cx);
                                    })),
                            ),
                    )
                    .when(!self.status_text.is_empty(), |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(self.status_text.clone()),
                        )
                    }),
            )
    }
}

impl ChatShell {
    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();

        div()
            .v_flex()
            .w(px(64.))
            .h_full()
            .items_center()
            .py_3()
            .bg(theme.secondary)
            .child(
                div()
                    .h_flex()
                    .w(px(36.))
                    .h(px(36.))
                    .flex_none()
                    .rounded_md()
                    .bg(theme.primary)
                    .text_color(theme.primary_foreground)
                    .items_center()
                    .justify_center()
                    .child("M"),
            )
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .window_control_area(WindowControlArea::Drag),
            )
            .child(
                Button::new("settings")
                    .small()
                    .ghost()
                    .icon(IconName::Settings)
                    .on_click(move |_, window, cx| {
                        let existing = SETTINGS_WINDOW_HANDLE.lock().unwrap().as_ref().copied();
                        if let Some(handle) = existing {
                            if handle
                                .update(cx, |_, window, _| {
                                    window.activate_window();
                                })
                                .is_ok()
                            {
                                cx.stop_propagation();
                                return;
                            }
                            *SETTINGS_WINDOW_HANDLE.lock().unwrap() = None;
                        }
                        let main_size = window.viewport_size();
                        let settings_size = size(main_size.width / 2., main_size.height / 2.);
                        let mut options = WindowOptions::default();
                        options.window_bounds = Some(WindowBounds::centered(settings_size, cx));
                        options.titlebar = Some(TitlebarOptions {
                            title: None,
                            appears_transparent: true,
                            traffic_light_position: None,
                        });

                        match cx.open_window(options, |window, cx| {
                            let view = cx.new(|cx| SettingsWindowView::new(window, cx));
                            cx.new(|cx| Root::new(view, window, cx))
                        }) {
                            Ok(handle) => {
                                *SETTINGS_WINDOW_HANDLE.lock().unwrap() = Some(handle.into());
                                let _ = handle.update(cx, |_, window, _| {
                                    window.activate_window();
                                });
                            }
                            Err(_) => {
                                *SETTINGS_WINDOW_HANDLE.lock().unwrap() = None;
                            }
                        }
                        cx.stop_propagation();
                    }),
            )
    }
}
