//! Native GPUI startup shell. All panel content is explicitly placeholder UI.
//! No filesystem enumeration, device discovery, or transfer is performed.

use gpui::{
    App, Application, Bounds, Context, FocusHandle, KeyBinding, Menu, MenuItem, TitlebarOptions,
    Window, WindowBounds, WindowOptions, actions, div, prelude::*, px, rgb, size,
};

actions!(mtp, [Quit]);

struct MtpShell {
    focus_handle: FocusHandle,
}

/// Render an empty panel rather than inventing files or a connected device.
fn placeholder_panel(
    title: &'static str,
    subtitle: &'static str,
    empty_title: &'static str,
    empty_detail: &'static str,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .h_full()
        .bg(rgb(0x181e29))
        .border_1()
        .border_color(rgb(0x30394a))
        .rounded_md()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_4()
                .border_b_1()
                .border_color(rgb(0x30394a))
                .child(div().text_lg().text_color(rgb(0xe6edf7)).child(title))
                .child(div().text_sm().text_color(rgb(0x96a3b8)).child(subtitle)),
        )
        .child(
            div()
                .flex()
                .justify_between()
                .px_4()
                .py_2()
                .text_sm()
                .text_color(rgb(0x96a3b8))
                .border_b_1()
                .border_color(rgb(0x30394a))
                .child("名称 / Name")
                .child("大小 / Size"),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .justify_center()
                .items_center()
                .gap_3()
                .p_4()
                .child(div().text_lg().text_color(rgb(0xc3cede)).child(empty_title))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x96a3b8))
                        .child(empty_detail),
                ),
        )
}

impl Render for MtpShell {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x10141d))
            .text_color(rgb(0xe6edf7))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .py_3()
                    .bg(rgb(0x141a25))
                    .border_b_1()
                    .border_color(rgb(0x30394a))
                    .child(div().text_xl().child("MTP · Native"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(0xe9bc73))
                            .child("GPUI PoC · 设备尚未接入"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap_3()
                    .p_4()
                    .child(placeholder_panel(
                        "本地 / Local",
                        "目录浏览占位 · 尚未读取本地文件",
                        "本地面板尚未接入",
                        "这里不会展示虚构的目录或文件。",
                    ))
                    .child(placeholder_panel(
                        "设备 / Remote",
                        "MTP 设备占位 · 未实现设备发现",
                        "设备尚未接入",
                        "尚未连接设备，也没有执行任何传输。",
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .py_2()
                    .bg(rgb(0x141a25))
                    .border_t_1()
                    .border_color(rgb(0x30394a))
                    .text_sm()
                    .text_color(rgb(0x96a3b8))
                    .child("仅启动壳 · 本地目录 / 设备 / 传输均未接入")
                    .child("⌘Q 退出"),
            )
    }
}

fn quit(_: &Quit, cx: &mut App) {
    cx.quit();
}

fn main() {
    Application::new().run(|cx: &mut App| {
        cx.on_action(quit);
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.set_menus(vec![Menu {
            name: "MTP".into(),
            items: vec![MenuItem::action("退出 MTP", Quit)],
        }]);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1100.0), px(700.0)), cx);
        let result = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(800.0), px(500.0))),
                titlebar: Some(TitlebarOptions {
                    title: Some("MTP — GPUI PoC".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    let focus_handle = cx.focus_handle();
                    focus_handle.focus(window);
                    MtpShell { focus_handle }
                })
            },
        );
        if let Err(error) = result {
            eprintln!("无法创建 GPUI 窗口: {error:#}");
            cx.quit();
            return;
        }
        cx.activate(true);
    });
}
