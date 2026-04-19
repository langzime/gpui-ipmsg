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

fn main() {
    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);

    app.run(|cx| {
        gpui_component::init(cx);

        cx.spawn(async move |cx| {
            let mut options = WindowOptions::default();
            options.titlebar = Some(TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: None,
            });

            cx.open_window(options, |window, cx| {
                let view = cx.new(|cx| ChatShell::new(window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("failed to open window");
        })
        .detach();
    });
}
