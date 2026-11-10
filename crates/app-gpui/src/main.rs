//! Read-only local and worker-backed remote browsing with viewport-virtualized rows.
//! No transfer, write, delete, or simulated device UI is implemented.

mod local;
mod remote;

use std::path::PathBuf;

use gpui::{
    App, Application, Bounds, ClickEvent, Context, Div, FocusHandle, KeyBinding, Menu, MenuItem,
    MouseButton, Stateful, TitlebarOptions, UniformListScrollHandle, Window, WindowBounds,
    WindowOptions, actions, div, prelude::*, px, rgb, size, uniform_list,
};
use local::LocalBrowser;
use mtp_local_fs::{EntryKind, read_directory};
use remote::{RemoteBrowser, RemoteCommand, RemoteConfig, RemoteLocation};

actions!(
    mtp,
    [
        Quit,
        RefreshLocal,
        ParentLocal,
        RefreshRemote,
        ParentRemote,
        NextPane,
        PreviousPane
    ]
);

struct MtpShell {
    focus_handle: FocusHandle,
    local_focus: FocusHandle,
    remote_focus: FocusHandle,
    local: LocalBrowser,
    scroll: UniformListScrollHandle,
    remote: RemoteBrowser,
    remote_scroll: UniformListScrollHandle,
}

fn toolbar_button(id: &'static str, label: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_sm()
        .bg(rgb(0x263245))
        .text_sm()
        .cursor_pointer()
        .hover(|this| this.bg(rgb(0x344763)))
        .child(label)
}

fn kind_label(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Directory => "DIR",
        EntryKind::File => "FILE",
        EntryKind::Symlink => "LINK",
        EntryKind::Other => "OTHER",
        EntryKind::Unknown => "?",
    }
}

impl MtpShell {
    fn new(
        path: PathBuf,
        remote_config: RemoteConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut shell = Self::state(path.clone(), remote_config, window, cx);
        shell.request_directory(path, cx);
        shell
    }

    fn state(
        path: PathBuf,
        remote_config: RemoteConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let local_focus = cx.focus_handle().tab_index(1).tab_stop(true);
        local_focus.focus(window);
        Self {
            focus_handle: cx.focus_handle(),
            local_focus,
            remote_focus: cx.focus_handle().tab_index(2).tab_stop(true),
            local: LocalBrowser::new(path),
            scroll: UniformListScrollHandle::new(),
            remote: RemoteBrowser::new(remote_config),
            remote_scroll: UniformListScrollHandle::new(),
        }
    }

    /// Called at startup or by events only. Render never starts filesystem work.
    fn request_directory(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Ok(revision) = self.local.begin(path.clone()) else {
            cx.notify();
            return;
        };
        let show_hidden = self.local.show_hidden;
        self.scroll = UniformListScrollHandle::new();
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            read_directory(&path, show_hidden).map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            // A weak entity means closing the window is harmless. Obsolete requests
            // can finish, but neither their data nor errors may replace newer state.
            let _ = this.update(cx, |this, cx| {
                if this.local.complete(revision, result) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn refresh(&mut self, _: &RefreshLocal, _window: &mut Window, cx: &mut Context<Self>) {
        self.request_directory(self.local.path.clone(), cx);
    }

    fn parent(&mut self, _: &ParentLocal, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(parent) = self.local.path.parent() {
            self.request_directory(parent.to_path_buf(), cx);
        }
    }

    fn next_pane(&mut self, _: &NextPane, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_next();
        cx.notify();
    }

    fn previous_pane(&mut self, _: &PreviousPane, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_prev();
        cx.notify();
    }

    fn click_entry(
        &mut self,
        index: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.local_focus.focus(window);
        let Some(entry) = self.local.entries.get(index) else {
            return;
        };
        let path = entry.path.clone();
        let is_directory = entry.kind == EntryKind::Directory;
        self.local.selected = Some(path.clone());
        if event.click_count() == 2 && is_directory {
            self.request_directory(path, cx);
        } else {
            cx.notify();
        }
    }

    fn local_panel(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let focus_color = if self.local_focus.is_focused(window) {
            0x668ed1
        } else {
            0x30394a
        };
        let status = if self.local.loading {
            "正在读取目录… / Loading".to_owned()
        } else if self.local.error.is_some() {
            "读取失败 / Error · 可刷新重试或返回上一级".to_owned()
        } else {
            format!("{} 项 · 单击选择，双击目录进入", self.local.entries.len())
        };
        let detail = self
            .local
            .selected_entry()
            .map(|entry| {
                if let Some(error) = &entry.metadata_error {
                    format!("元数据错误: {} — {error}", entry.path.to_string_lossy())
                } else {
                    format!(
                        "{} · {}",
                        kind_label(entry.kind),
                        entry.path.to_string_lossy()
                    )
                }
            })
            .unwrap_or_else(|| "只读浏览 · 不打开文件，不跟随符号链接，不执行传输".into());

        div()
            .id("local-pane")
            .track_focus(&self.local_focus)
            .key_context("LocalPane")
            .on_action(cx.listener(Self::refresh))
            .on_action(cx.listener(Self::parent))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.local_focus.focus(window);
                    cx.notify();
                }),
            )
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(rgb(0x181e29))
            .border_1()
            .border_color(rgb(focus_color))
            .rounded_md()
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_4()
                    .border_b_1()
                    .border_color(rgb(0x30394a))
                    .child(div().text_lg().child("本地 / Local · 只读"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(0xb8c5da))
                            .text_ellipsis()
                            .child(self.local.path.to_string_lossy().into_owned()),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                toolbar_button("parent", "上一级 ⌘B")
                                    .when(self.local.path.parent().is_none(), |this| {
                                        this.opacity(0.4)
                                    })
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.local_focus.focus(window);
                                        this.parent(&ParentLocal, window, cx);
                                    })),
                            )
                            .child(toolbar_button("refresh", "刷新 ⌘R").on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.local_focus.focus(window);
                                    this.refresh(&RefreshLocal, window, cx);
                                },
                            )))
                            .child(
                                toolbar_button(
                                    "hidden",
                                    if self.local.show_hidden {
                                        "隐藏项: 显示"
                                    } else {
                                        "隐藏项: 隐藏"
                                    },
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.local_focus.focus(window);
                                        this.local.show_hidden = !this.local.show_hidden;
                                        this.request_directory(this.local.path.clone(), cx);
                                    },
                                )),
                            ),
                    ),
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
                    .child("类型 / 名称")
                    .child("大小 (bytes)"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .when(
                        self.local.loading
                            || self.local.error.is_some()
                            || self.local.entries.is_empty(),
                        |this| {
                            let message = if self.local.loading {
                                "正在后台读取目录…".to_owned()
                            } else if let Some(error) = &self.local.error {
                                format!("无法读取目录: {error}")
                            } else {
                                "此目录没有可见条目（可尝试显示隐藏项）".to_owned()
                            };
                            this.child(
                                div()
                                    .p_4()
                                    .text_sm()
                                    .text_color(rgb(0xe9bc73))
                                    .child(message),
                            )
                        },
                    )
                    .when(!self.local.entries.is_empty(), |this| {
                        this.child(
                            uniform_list(
                                "local-entries",
                                self.local.entries.len(),
                                cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                                    range
                                        .map(|index| {
                                            let entry = &this.local.entries[index];
                                            let selected =
                                                this.local.selected.as_ref() == Some(&entry.path);
                                            let label = entry.name.to_string_lossy().into_owned();
                                            let kind = kind_label(entry.kind);
                                            let size = entry
                                                .byte_size
                                                .map(|size| size.to_string())
                                                .unwrap_or_else(|| "—".into());
                                            let metadata_error = entry.metadata_error.is_some();
                                            div()
                                                .id(index)
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .h(px(32.0))
                                                .px_4()
                                                .w_full()
                                                .text_sm()
                                                .cursor_pointer()
                                                .bg(rgb(if selected { 0x2b446a } else { 0x181e29 }))
                                                .hover(|this| this.bg(rgb(0x29374d)))
                                                .child(
                                                    div()
                                                        .w(px(45.0))
                                                        .flex_shrink_0()
                                                        .text_color(rgb(0x8fa9cf))
                                                        .child(kind),
                                                )
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .text_ellipsis()
                                                        .child(label),
                                                )
                                                .when(metadata_error, |this| {
                                                    this.child(
                                                        div().text_color(rgb(0xe9bc73)).child("!"),
                                                    )
                                                })
                                                .child(
                                                    div()
                                                        .flex_shrink_0()
                                                        .text_color(rgb(0x96a3b8))
                                                        .child(size),
                                                )
                                                .on_click(cx.listener(
                                                    move |this, event, window, cx| {
                                                        this.click_entry(index, event, window, cx);
                                                    },
                                                ))
                                        })
                                        .collect()
                                }),
                            )
                            .track_scroll(self.scroll.clone())
                            .flex_1()
                            .min_h_0()
                            .w_full(),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(0x30394a))
                    .text_sm()
                    .text_color(rgb(0x96a3b8))
                    .child(status)
                    .child(div().text_ellipsis().child(detail)),
            )
    }

    fn request_remote(&mut self, command: RemoteCommand, cx: &mut Context<Self>) {
        let Some(revision) = self.remote.begin(&command) else {
            cx.notify();
            return;
        };
        self.remote_scroll = UniformListScrollHandle::new();
        let shared = self.remote.shared.clone();
        cx.notify();
        let task = cx
            .background_executor()
            .spawn(async move { remote::run(shared, revision, command) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(result) = result {
                let _ = this.update(cx, |this, cx| {
                    if this.remote.complete(revision, result) {
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    fn connect_remote(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.remote_focus.focus(window);
        if !self.remote.loading && matches!(&self.remote.config, RemoteConfig::Ready { .. }) {
            self.request_remote(RemoteCommand::Connect(self.remote.config.clone()), cx);
        }
    }

    fn disconnect_remote(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.remote_focus.focus(window);
        // begin clears every remote reference synchronously, even during connect/list.
        self.request_remote(RemoteCommand::Disconnect, cx);
    }

    fn refresh_remote(&mut self, _: &RefreshRemote, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(directory) = self.remote.current().cloned() {
            self.request_remote(RemoteCommand::List(directory), cx);
        }
    }

    fn parent_remote(&mut self, _: &ParentRemote, _window: &mut Window, cx: &mut Context<Self>) {
        if self.remote.locations.len() > 1 {
            self.remote.locations.pop();
            if let Some(directory) = self.remote.current().cloned() {
                self.request_remote(RemoteCommand::List(directory), cx);
            }
        }
    }

    fn select_storage(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.remote_focus.focus(window);
        if let Some(root) = self.remote.root_location(index) {
            let directory = root.directory.clone();
            self.remote.locations = vec![root];
            self.request_remote(RemoteCommand::List(directory), cx);
        }
    }

    fn root_remote(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let index = self.remote.current().and_then(|directory| {
            self.remote
                .storages
                .iter()
                .position(|storage| storage.id == directory.storage().0)
        });
        if let Some(index) = index {
            self.select_storage(index, window, cx);
        }
    }

    fn click_remote_entry(
        &mut self,
        index: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remote_focus.focus(window);
        let Some(entry) = self.remote.entries.get(index) else {
            return;
        };
        let reference = entry.reference.clone();
        let is_directory = entry.directory;
        let label = entry.name.clone();
        self.remote.selected = Some(reference.clone());
        if is_directory && event.click_count() == 2 {
            let directory = mtp_domain::RemoteDirectoryRef::from(reference);
            self.remote.locations.push(RemoteLocation {
                directory: directory.clone(),
                label,
            });
            self.request_remote(RemoteCommand::List(directory), cx);
        } else {
            cx.notify();
        }
    }

    fn remote_panel(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let connected = self.remote.session.is_some();
        let title = self
            .remote
            .session
            .as_ref()
            .map(|session| session.display_name.clone())
            .unwrap_or_else(|| "设备 / Remote".into());
        let status = if self.remote.loading {
            "设备请求处理中… / Loading".to_owned()
        } else if self.remote.error.is_some() {
            "设备错误 · 请查看详情并重连".into()
        } else if connected {
            format!(
                "已连接 · {} 存储 · {} 项 · 只读",
                self.remote.storages.len(),
                self.remote.entries.len()
            )
        } else {
            self.remote.config.message().into()
        };
        let selection = self
            .remote
            .selected
            .as_ref()
            .and_then(|selected| {
                self.remote
                    .entries
                    .iter()
                    .find(|entry| &entry.reference == selected)
            })
            .map(|entry| {
                format!(
                    "已选择 {} · {}",
                    if entry.directory { "目录" } else { "文件" },
                    entry.name
                )
            })
            .unwrap_or_else(|| "单击选择 · 双击目录 · 不执行上传/下载/删除".into());
        let can_connect =
            matches!(&self.remote.config, RemoteConfig::Ready { .. }) && !self.remote.loading;
        let can_disconnect = connected || self.remote.loading;

        div()
            .id("remote-pane")
            .track_focus(&self.remote_focus)
            .key_context("RemotePane")
            .on_action(cx.listener(Self::refresh_remote))
            .on_action(cx.listener(Self::parent_remote))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.remote_focus.focus(window);
                    cx.notify();
                }),
            )
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_hidden()
            .bg(rgb(0x181e29))
            .border_1()
            .border_color(rgb(if self.remote_focus.is_focused(window) {
                0x668ed1
            } else {
                0x30394a
            }))
            .rounded_md()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_4()
                    .border_b_1()
                    .border_color(rgb(0x30394a))
                    .child(div().text_lg().text_ellipsis().child(title))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(0xb8c5da))
                            .text_ellipsis()
                            .child(self.remote.address()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                toolbar_button(
                                    "connect-remote",
                                    if connected { "重新连接" } else { "连接" },
                                )
                                .when(!can_connect, |this| this.opacity(0.4))
                                .on_click(cx.listener(
                                    |this, _, window, cx| this.connect_remote(window, cx),
                                )),
                            )
                            .child(
                                toolbar_button("disconnect-remote", "断开")
                                    .when(!can_disconnect, |this| this.opacity(0.4))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if can_disconnect {
                                            this.disconnect_remote(window, cx);
                                        }
                                    })),
                            )
                            .child(
                                toolbar_button("refresh-remote", "刷新 ⌘R")
                                    .when(self.remote.current().is_none(), |this| this.opacity(0.4))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.remote_focus.focus(window);
                                        this.refresh_remote(&RefreshRemote, window, cx);
                                    })),
                            )
                            .child(
                                toolbar_button("parent-remote", "上级 ⌘B")
                                    .when(self.remote.locations.len() < 2, |this| this.opacity(0.4))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.remote_focus.focus(window);
                                        this.parent_remote(&ParentRemote, window, cx);
                                    })),
                            )
                            .child(
                                toolbar_button("root-remote", "根目录")
                                    .when(!connected, |this| this.opacity(0.4))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.root_remote(window, cx)
                                    })),
                            ),
                    )
                    .child(
                        div().flex().flex_wrap().gap_2().children(
                            self.remote
                                .storages
                                .iter()
                                .enumerate()
                                .map(|(index, storage)| {
                                    let active = self
                                        .remote
                                        .current()
                                        .map(|directory| directory.storage().0)
                                        == Some(storage.id);
                                    div()
                                        .id(("storage", index))
                                        .px_2()
                                        .py_1()
                                        .rounded_sm()
                                        .text_sm()
                                        .bg(rgb(if active { 0x2b446a } else { 0x263245 }))
                                        .cursor_pointer()
                                        .child(storage.name.clone())
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.select_storage(index, window, cx)
                                        }))
                                }),
                        ),
                    ),
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
                    .child("类型 / 名称")
                    .child("大小 (bytes)"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .when(
                        self.remote.loading
                            || self.remote.error.is_some()
                            || self.remote.entries.is_empty(),
                        |this| {
                            let message = if self.remote.loading {
                                "正在后台处理设备请求…".to_owned()
                            } else if let Some(error) = &self.remote.error {
                                error.clone()
                            } else if connected && self.remote.storages.is_empty() {
                                "设备没有返回可浏览的存储。".into()
                            } else if connected {
                                "此远端目录为空。".into()
                            } else {
                                self.remote.config.message().into()
                            };
                            this.child(
                                div()
                                    .p_4()
                                    .text_sm()
                                    .text_color(rgb(0xe9bc73))
                                    .child(message),
                            )
                        },
                    )
                    .when(!self.remote.entries.is_empty(), |this| {
                        this.child(
                            uniform_list(
                                "remote-entries",
                                self.remote.entries.len(),
                                cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                                    range
                                        .map(|index| {
                                            let entry = &this.remote.entries[index];
                                            let selected = this.remote.selected.as_ref()
                                                == Some(&entry.reference);
                                            let label = entry.name.clone();
                                            let kind = if entry.directory { "DIR" } else { "FILE" };
                                            let size = entry
                                                .byte_size
                                                .map(|size| size.to_string())
                                                .unwrap_or_else(|| "—".into());
                                            div()
                                                .id(index)
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .h(px(32.0))
                                                .px_4()
                                                .w_full()
                                                .text_sm()
                                                .cursor_pointer()
                                                .bg(rgb(if selected { 0x2b446a } else { 0x181e29 }))
                                                .hover(|this| this.bg(rgb(0x29374d)))
                                                .child(
                                                    div()
                                                        .w(px(45.0))
                                                        .flex_shrink_0()
                                                        .text_color(rgb(0x8fa9cf))
                                                        .child(kind),
                                                )
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .text_ellipsis()
                                                        .child(label),
                                                )
                                                .child(
                                                    div()
                                                        .flex_shrink_0()
                                                        .text_color(rgb(0x96a3b8))
                                                        .child(size),
                                                )
                                                .on_click(cx.listener(
                                                    move |this, event, window, cx| {
                                                        this.click_remote_entry(
                                                            index, event, window, cx,
                                                        )
                                                    },
                                                ))
                                        })
                                        .collect()
                                }),
                            )
                            .track_scroll(self.remote_scroll.clone())
                            .flex_1()
                            .min_h_0()
                            .w_full(),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(0x30394a))
                    .text_sm()
                    .text_color(rgb(0x96a3b8))
                    .child(status)
                    .child(div().text_ellipsis().child(selection)),
            )
    }
}

impl Render for MtpShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("mtp-shell")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::next_pane))
            .on_action(cx.listener(Self::previous_pane))
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
                    .child(div().text_sm().text_color(rgb(0xe9bc73)).child(
                        if self.remote.session.is_some() {
                            "只读本地 / MTP 目录 · 已连接"
                        } else if self.remote.loading {
                            "只读本地 / MTP 目录 · 请求中"
                        } else {
                            "只读本地 / MTP 目录 · 未连接"
                        },
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap_3()
                    .p_4()
                    .child(self.local_panel(window, cx))
                    .child(self.remote_panel(window, cx)),
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
                    .child("只读本地 / 设备浏览 · 无上传下载 / 无删除")
                    .child("Tab 切换面板 · ⌘Q 退出"),
            )
    }
}

fn quit(_: &Quit, cx: &mut App) {
    cx.quit();
}

fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-r", RefreshLocal, Some("LocalPane")),
        KeyBinding::new("cmd-b", ParentLocal, Some("LocalPane")),
        KeyBinding::new("cmd-r", RefreshRemote, Some("RemotePane")),
        KeyBinding::new("cmd-b", ParentRemote, Some("RemotePane")),
        KeyBinding::new("tab", NextPane, None),
        KeyBinding::new("shift-tab", PreviousPane, None),
    ]);
}

fn main() {
    // Resolve the initial native path once, before entering GPUI's render lifecycle.
    let initial_path = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .unwrap_or_else(|| PathBuf::from("/"));

    let remote_config = RemoteConfig::from_startup();

    Application::new().run(move |cx: &mut App| {
        cx.on_action(quit);
        bind_keys(cx);
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
                    title: Some("MTP — 只读目录浏览".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| MtpShell::new(initial_path, remote_config, window, cx)),
        );
        if let Err(error) = result {
            eprintln!("无法创建 GPUI 窗口: {error:#}");
            cx.quit();
            return;
        }
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn tab_focus_and_remote_shortcuts_do_not_touch_local(cx: &mut gpui::TestAppContext) {
        cx.update(bind_keys);
        let (view, cx) = cx.add_window_view(|window, cx| {
            MtpShell::state(
                PathBuf::from("/not-enumerated/child"),
                RemoteConfig::Unconfigured,
                window,
                cx,
            )
        });
        cx.update(|window, cx| {
            window.activate_window();
            assert!(view.read(cx).local_focus.is_focused(window));
        });
        cx.simulate_keystrokes("tab");
        cx.update(|window, cx| assert!(view.read(cx).remote_focus.is_focused(window)));
        cx.simulate_keystrokes("cmd-r cmd-b");
        view.update(cx, |shell, _| {
            assert_eq!(shell.local.path, PathBuf::from("/not-enumerated/child"));
            assert!(!shell.local.loading);
            assert!(shell.remote.session.is_none());
            assert!(!shell.remote.loading);
        });
        cx.simulate_keystrokes("shift-tab");
        cx.update(|window, cx| assert!(view.read(cx).local_focus.is_focused(window)));
        cx.simulate_keystrokes("tab tab");
        cx.update(|window, cx| assert!(view.read(cx).local_focus.is_focused(window)));
    }
}
