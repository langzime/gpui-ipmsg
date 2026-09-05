use super::ChatShell;
use crate::app_state;
use crate::config::{LanguageEncoding, UiLanguage};
use crate::logic;
use gpui::*;
use gpui::prelude::FluentBuilder;
use gpui_component::{
    ActiveTheme, IconName, Root, Sizable, StyledExt, TitleBar,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement,
};

/// Settings 窗口句柄：以 GPUI Global 收拢，替代原先的静态 `Mutex`（中期项 1）。
#[derive(Default)]
struct SettingsWindowHandle(Option<AnyWindowHandle>);
impl Global for SettingsWindowHandle {}

struct SettingsWindowView {
    username_input: Entity<InputState>,
    group_input: Entity<InputState>,
    settings_scroll_handle: ScrollHandle,
    username: String,
    group: String,
    language: LanguageEncoding,
    ui_language: UiLanguage,
    status_text: String,
    _subscriptions: Vec<Subscription>,
}

impl SettingsWindowView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let config = logic::get_config();
        let username = config.user.username;
        let group = config.user.group;
        let language = config.language;
        let ui_language = config.ui_language;

        let username_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("settings.username").to_string()));
        let group_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("settings.group").to_string()));

        username_input.update(cx, |state, cx| state.set_value(&username, window, cx));
        group_input.update(cx, |state, cx| state.set_value(&group, window, cx));

        let mut this = Self {
            username_input,
            group_input,
            settings_scroll_handle: ScrollHandle::default(),
            username,
            group,
            language,
            ui_language,
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
            self.status_text = t!("status.username_required").to_string();
            cx.notify();
            return;
        }
        match logic::save_settings(
            username,
            group,
            self.language,
            self.ui_language,
            cx.global::<app_state::AppStateGlobal>().arc(),
        ) {
            Ok(()) => self.status_text = t!("status.saved_and_broadcasted").to_string(),
            Err(error) => {
                self.status_text = t!("status.save_failed", error = error).to_string();
            }
        }
        cx.notify();
    }

    fn set_encoding(&mut self, language: LanguageEncoding, cx: &mut Context<Self>) {
        self.language = language;
        cx.notify();
    }

    fn set_ui_language(&mut self, ui_language: UiLanguage, cx: &mut Context<Self>) {
        self.ui_language = ui_language;
        rust_i18n::set_locale(ui_language.as_locale());
        cx.notify();
    }
}

impl Render for SettingsWindowView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        div()
            .v_flex()
            .size_full()
            .min_h_0()
            .bg(theme.background)
            .child(
                TitleBar::new().child(
                    div()
                        .h_flex()
                        .h_full()
                        .items_center()
                        .px_1()
                        .child(div().text_sm().font_semibold().child(t!("settings.title").to_string())),
                ),
            )
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        div()
                            .id("settings-scroll")
                            .v_flex()
                            .size_full()
                            .track_scroll(&self.settings_scroll_handle)
                            .overflow_y_scroll()
                            .vertical_scrollbar(&self.settings_scroll_handle)
                            .child(
                                div()
                                    .v_flex()
                                    .p_4()
                                    .gap_3()
                                    .child(div().text_sm().child(t!("settings.username").to_string()))
                                    .child(Input::new(&self.username_input))
                                    .child(div().text_sm().child(t!("settings.group").to_string()))
                                    .child(Input::new(&self.group_input))
                                    .child(div().text_sm().child(t!("settings.encoding").to_string()))
                                    .child(
                                        div()
                                            .h_flex()
                                            .gap_2()
                                            .child(
                                                if self.language == LanguageEncoding::Utf8 {
                                                    Button::new("lang-utf8")
                                                        .small()
                                                        .primary()
                                                        .label("UTF-8")
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.set_encoding(LanguageEncoding::Utf8, cx);
                                                        }))
                                                } else {
                                                    Button::new("lang-utf8")
                                                        .small()
                                                        .ghost()
                                                        .label("UTF-8")
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.set_encoding(LanguageEncoding::Utf8, cx);
                                                        }))
                                                },
                                            )
                                            .child(
                                                if self.language == LanguageEncoding::Gb18030 {
                                                    Button::new("lang-gb18030")
                                                        .small()
                                                        .primary()
                                                        .label("GB18030")
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.set_encoding(LanguageEncoding::Gb18030, cx);
                                                        }))
                                                } else {
                                                    Button::new("lang-gb18030")
                                                        .small()
                                                        .ghost()
                                                        .label("GB18030")
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.set_encoding(LanguageEncoding::Gb18030, cx);
                                                        }))
                                                },
                                            ),
                                    )
                                    .child(div().text_sm().child(t!("settings.ui_language").to_string()))
                                    .child(
                                        div()
                                            .h_flex()
                                            .gap_2()
                                            .child(
                                                if self.ui_language == UiLanguage::ZhCn {
                                                    Button::new("ui-lang-zh-cn")
                                                        .small()
                                                        .primary()
                                                        .label(t!("settings.ui_language_zh_cn").to_string())
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.set_ui_language(UiLanguage::ZhCn, cx);
                                                        }))
                                                } else {
                                                    Button::new("ui-lang-zh-cn")
                                                        .small()
                                                        .ghost()
                                                        .label(t!("settings.ui_language_zh_cn").to_string())
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.set_ui_language(UiLanguage::ZhCn, cx);
                                                        }))
                                                },
                                            )
                                            .child(
                                                if self.ui_language == UiLanguage::En {
                                                    Button::new("ui-lang-en")
                                                        .small()
                                                        .primary()
                                                        .label(t!("settings.ui_language_en").to_string())
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.set_ui_language(UiLanguage::En, cx);
                                                        }))
                                                } else {
                                                    Button::new("ui-lang-en")
                                                        .small()
                                                        .ghost()
                                                        .label(t!("settings.ui_language_en").to_string())
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.set_ui_language(UiLanguage::En, cx);
                                                        }))
                                                },
                                            ),
                                    )
                                    .child(
                                        div()
                                            .h_flex()
                                            .justify_end()
                                            .child(
                                                Button::new("save-settings")
                                                    .small()
                                                    .primary()
                                                    .label(t!("settings.save").to_string())
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
                            ),
                    ),
            )
    }
}

impl ChatShell {
    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let avatar_top_offset = if cfg!(target_os = "macos") {
            px(20.)
        } else {
            px(30.)
        };

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
                    .mt(avatar_top_offset)
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
                        let existing = cx.global_mut::<SettingsWindowHandle>().0;
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
                            // 窗口已销毁，清掉陈旧句柄后重新打开。
                            cx.global_mut::<SettingsWindowHandle>().0 = None;
                        }
                        let main_size = window.viewport_size();
                        let settings_size = size(main_size.width / 2., main_size.height * 0.70);
                        let options = WindowOptions {
                            window_bounds: Some(WindowBounds::centered(settings_size, cx)),
                            titlebar: Some(TitlebarOptions {
                                title: None,
                                appears_transparent: true,
                                traffic_light_position: None,
                            }),
                            ..Default::default()
                        };

                        match cx.open_window(options, |window, cx| {
                            let view = cx.new(|cx| SettingsWindowView::new(window, cx));
                            cx.new(|cx| Root::new(view, window, cx))
                        }) {
                            Ok(handle) => {
                                cx.global_mut::<SettingsWindowHandle>().0 = Some(handle.into());
                                let _ = handle.update(cx, |_, window, _| {
                                    window.activate_window();
                                });
                            }
                            Err(_) => {
                                cx.global_mut::<SettingsWindowHandle>().0 = None;
                            }
                        }
                        cx.stop_propagation();
                    }),
            )
    }
}
