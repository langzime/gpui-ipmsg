#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
extern crate rust_i18n;

use gpui::*;
use gpui_component::Root;

mod app_state;
mod chat_shell;
mod chat_area;
mod config;
mod conversation_list;
mod input_area;
mod ipmsg_core;
mod logic;
mod sidebar;
pub(crate) use chat_shell::ChatShell;

i18n!("locales", fallback = "zh-CN");

fn main() {
    let startup_config = config::load_config();
    rust_i18n::set_locale(startup_config.ui_language.as_locale());

    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);

    app.run(|cx| {
        gpui_component::init(cx);

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(800.), px(600.)), cx)),
            titlebar: Some(TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: None,
            }),
            ..Default::default()
        };

        cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| ChatShell::new(window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("failed to open window");
    });
}
