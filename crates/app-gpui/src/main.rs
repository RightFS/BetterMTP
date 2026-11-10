//! Local and worker-backed browsing with capability-gated single-file transfers.
//! No recursive transfer, delete, overwrite, or simulated device operations.

mod context_ui;
mod local;
mod remote;
mod task_ui;
mod transfers;
mod ui;

use std::path::PathBuf;

use gpui::{
    App, Application, Bounds, ClickEvent, Context, Div, FocusHandle, KeyBinding, Menu, MenuItem,
    MouseButton, Stateful, TitlebarOptions, UniformListScrollHandle, Window, WindowBounds,
    WindowOptions, actions, div, prelude::*, px, rgb, size, uniform_list,
};
use local::LocalBrowser;
use mtp_local_fs::{EntryKind, read_directory};
use remote::{RemoteBrowser, RemoteCommand, RemoteConfig, RemoteLocation};
use transfers::{TransferPlan, TransferState};

actions!(
    mtp,
    [
        Quit,
        RefreshLocal,
        ParentLocal,
        RefreshRemote,
        ParentRemote,
        NextPane,
        PreviousPane,
        PreviousLocal,
        NextLocal,
        FirstLocal,
        LastLocal,
        EnterLocal,
        PreviousRemote,
        NextRemote,
        FirstRemote,
        LastRemote,
        EnterRemote,
        DismissContext
    ]
);

use ui::SelectionMove;

#[derive(Clone, PartialEq)]
enum ContextIdentity {
    Local(PathBuf, u64),
    Remote(mtp_domain::RemoteRef, u64),
}

#[derive(Clone)]
struct ContextTicket {
    identity: ContextIdentity,
    token: u64,
}

struct ContextPopover {
    ticket: ContextTicket,
    position: gpui::Point<gpui::Pixels>,
}

struct MtpShell {
    focus_handle: FocusHandle,
    local_focus: FocusHandle,
    remote_focus: FocusHandle,
    local: LocalBrowser,
    scroll: UniformListScrollHandle,
    remote: RemoteBrowser,
    remote_scroll: UniformListScrollHandle,
    transfer: TransferState,
    context_menu: Option<ContextPopover>,
    context_serial: u64,
    task_expanded: bool,
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
        cache: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut shell = Self::state(path.clone(), remote_config, window, cx);
        shell.transfer.cache = cache;
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
        let shell = Self {
            focus_handle: cx.focus_handle(),
            local_focus,
            remote_focus: cx.focus_handle().tab_index(2).tab_stop(true),
            local: LocalBrowser::new(path),
            scroll: UniformListScrollHandle::new(),
            remote: RemoteBrowser::new(remote_config),
            remote_scroll: UniformListScrollHandle::new(),
            transfer: TransferState::new(None),
            context_menu: None,
            context_serial: 0,
            task_expanded: false,
        };
        cx.on_release(|this, _| this.remote.shared.request_stop())
            .detach();
        cx.on_app_quit(|this, _| {
            this.remote.shared.request_stop();
            async {}
        })
        .detach();
        shell
    }

    /// Called at startup or by events only. Render never starts filesystem work.
    fn request_directory(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.context_menu = None;
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
        self.context_menu = None;
        window.focus_next();
        cx.notify();
    }

    fn previous_pane(&mut self, _: &PreviousPane, window: &mut Window, cx: &mut Context<Self>) {
        self.context_menu = None;
        window.focus_prev();
        cx.notify();
    }

    fn remote_interaction_allowed(&self) -> bool {
        self.transfer.remote_requests_allowed()
            && !matches!(self.remote.config, RemoteConfig::LocalOnly)
    }

    fn move_local(&mut self, movement: SelectionMove, cx: &mut Context<Self>) {
        if self.local.loading || self.local.error.is_some() {
            return;
        }
        let current = self.local.selected.as_ref().and_then(|path| {
            self.local
                .entries
                .iter()
                .position(|entry| &entry.path == path)
        });
        if let Some(index) = ui::moved_index(current, self.local.entries.len(), movement) {
            self.context_menu = None;
            self.local.selected = Some(self.local.entries[index].path.clone());
            self.scroll.scroll_to_item(index, gpui::ScrollStrategy::Top);
            cx.notify();
        }
    }

    fn move_remote(&mut self, movement: SelectionMove, cx: &mut Context<Self>) {
        if !self.remote_interaction_allowed() || self.remote.loading {
            return;
        }
        let current = self.remote.selected.as_ref().and_then(|reference| {
            self.remote
                .entries
                .iter()
                .position(|entry| &entry.reference == reference)
        });
        if let Some(index) = ui::moved_index(current, self.remote.entries.len(), movement) {
            self.context_menu = None;
            self.remote.selected = Some(self.remote.entries[index].reference.clone());
            self.remote_scroll
                .scroll_to_item(index, gpui::ScrollStrategy::Top);
            cx.notify();
        }
    }

    fn enter_local(&mut self, _: &EnterLocal, _window: &mut Window, cx: &mut Context<Self>) {
        if self.local.loading {
            return;
        }
        if let Some(entry) = self
            .local
            .selected_entry()
            .filter(|entry| entry.kind == EntryKind::Directory)
        {
            self.request_directory(entry.path.clone(), cx);
        }
    }

    fn enter_remote(&mut self, _: &EnterRemote, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.remote_interaction_allowed() || self.remote.loading {
            return;
        }
        let entry = self
            .remote
            .selected
            .as_ref()
            .and_then(|reference| {
                self.remote
                    .entries
                    .iter()
                    .find(|entry| &entry.reference == reference)
            })
            .filter(|entry| entry.directory);
        if let Some(entry) = entry {
            let directory = mtp_domain::RemoteDirectoryRef::from(entry.reference.clone());
            let label = entry.name.clone();
            self.remote.locations.push(RemoteLocation {
                directory: directory.clone(),
                label,
            });
            self.request_remote(RemoteCommand::List(directory), cx);
        }
    }

    fn navigate_remote_ancestor(
        &mut self,
        index: usize,
        epoch: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.remote_interaction_allowed()
            || self.remote.loading
            || self.remote.transfer_epoch() != epoch
        {
            return;
        }
        if let Some(location) = self.remote.locations.get(index) {
            let directory = location.directory.clone();
            self.remote.locations.truncate(index + 1);
            self.remote_focus.focus(window);
            self.request_remote(RemoteCommand::List(directory), cx);
        }
    }

    fn dismiss_context(&mut self, _: &DismissContext, window: &mut Window, cx: &mut Context<Self>) {
        if self.context_menu.take().is_none() {
            if self.local_focus.is_focused(window) {
                self.local.selected = None;
            } else if self.remote_focus.is_focused(window) && self.remote_interaction_allowed() {
                self.remote.selected = None;
            }
        }
        cx.notify();
    }

    fn click_entry(
        &mut self,
        path: PathBuf,
        revision: u64,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.local.loading || self.local.snapshot_revision() != revision {
            return;
        }
        self.context_menu = None;
        self.local_focus.focus(window);
        let Some(entry) = self.local.entries.iter().find(|entry| entry.path == path) else {
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
                if entry.name.to_str().is_none() {
                    format!(
                        "名称含非 UTF-8 原生字节；展示有替代字符，不能上传或无损文本复制 · {}",
                        entry.path.to_string_lossy()
                    )
                } else if let Some(error) = &entry.metadata_error {
                    format!("元数据错误: {} — {error}", entry.path.to_string_lossy())
                } else {
                    format!(
                        "{} · {}",
                        kind_label(entry.kind),
                        entry.path.to_string_lossy()
                    )
                }
            })
            .unwrap_or_else(|| "浏览不打开文件、不跟随链接；单文件传输须显式点击".into());

        div()
            .id("local-pane")
            .track_focus(&self.local_focus)
            .key_context("LocalPane")
            .on_action(cx.listener(Self::refresh))
            .on_action(cx.listener(Self::parent))
            .on_action(cx.listener(|this, _: &PreviousLocal, _, cx| this.move_local(SelectionMove::Previous, cx)))
            .on_action(cx.listener(|this, _: &NextLocal, _, cx| this.move_local(SelectionMove::Next, cx)))
            .on_action(cx.listener(|this, _: &FirstLocal, _, cx| this.move_local(SelectionMove::First, cx)))
            .on_action(cx.listener(|this, _: &LastLocal, _, cx| this.move_local(SelectionMove::Last, cx)))
            .on_action(cx.listener(Self::enter_local))
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
                    .gap_1()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(0x30394a))
                    .child(div().text_lg().child(if self.local_focus.is_focused(window) { "本地 / Local · 当前键盘面板" } else { "本地 / Local" }))
                    .child(div().flex().gap_1().overflow_hidden().children({
                        let crumbs = ui::local_breadcrumbs(&self.local.path);
                        let start = crumbs.len().saturating_sub(3);
                        let revision = self.local.snapshot_revision();
                        crumbs.into_iter().enumerate().skip(start).map(|(index, (path, label))| {
                            div().id(("local-crumb", index)).px_1().rounded_sm().text_sm().cursor_pointer().text_ellipsis()
                                .bg(rgb(0x263245)).child(format!("{label} ›"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if this.local.snapshot_revision() != revision { return; }
                                    this.local_focus.focus(window); this.request_directory(path.clone(), cx);
                                }))
                        }).collect::<Vec<_>>()
                    }))
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
                                    .text_color(rgb(if self.local.error.is_some() { 0xe9bc73 } else if self.local.loading { 0x8fa9cf } else { 0x96a3b8 }))
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
                                            let non_utf8 = entry.name.to_str().is_none();
                                            let row_path = entry.path.clone();
                                            let menu_path = row_path.clone();
                                            let revision = this.local.snapshot_revision();
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
                                                .hover(move |this| this.bg(rgb(if selected { 0x314f79 } else { 0x29374d })))
                                                .child(div().w(px(8.0)).flex_shrink_0().child(if selected { ">" } else { "" }))
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
                                                .when(non_utf8, |this| this.child(div().text_color(rgb(0xe9bc73)).child("! UTF-8")))
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
                                                        this.click_entry(row_path.clone(), revision, event, window, cx);
                                                    },
                                                ))
                                                .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                                                    this.open_context(ContextIdentity::Local(menu_path.clone(), revision), event.position, window, cx);
                                                }))
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
        if matches!(self.remote.config, RemoteConfig::LocalOnly) {
            return;
        }
        self.context_menu = None;
        if !self.remote_interaction_allowed() {
            return;
        }
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
        if !self.remote_interaction_allowed() {
            return;
        }
        self.remote_focus.focus(window);
        if !self.remote.loading && matches!(&self.remote.config, RemoteConfig::Ready { .. }) {
            self.request_remote(RemoteCommand::Connect(self.remote.config.clone()), cx);
        }
    }

    fn disconnect_remote(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.remote_interaction_allowed() {
            return;
        }
        self.remote_focus.focus(window);
        // begin clears every remote reference synchronously, even during connect/list.
        self.request_remote(RemoteCommand::Disconnect, cx);
    }

    fn refresh_remote(&mut self, _: &RefreshRemote, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.remote_interaction_allowed() {
            return;
        }
        if let Some(directory) = self.remote.current().cloned() {
            self.request_remote(RemoteCommand::List(directory), cx);
        }
    }

    fn parent_remote(&mut self, _: &ParentRemote, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.remote_interaction_allowed() {
            return;
        }
        if self.remote.locations.len() > 1 {
            self.remote.locations.pop();
            if let Some(directory) = self.remote.current().cloned() {
                self.request_remote(RemoteCommand::List(directory), cx);
            }
        }
    }

    fn storage_snapshot_root(
        &self,
        id: mtp_domain::StorageId,
        epoch: u64,
    ) -> Option<RemoteLocation> {
        if !self.remote_interaction_allowed()
            || self.remote.loading
            || self.remote.transfer_epoch() != epoch
        {
            return None;
        }
        let index = self
            .remote
            .storages
            .iter()
            .position(|storage| storage.id == id.0)?;
        self.remote.root_location(index)
    }

    fn select_storage(
        &mut self,
        id: mtp_domain::StorageId,
        epoch: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.storage_snapshot_root(id, epoch) else {
            return;
        };
        self.remote_focus.focus(window);
        let directory = root.directory.clone();
        self.remote.locations = vec![root];
        self.request_remote(RemoteCommand::List(directory), cx);
    }

    fn root_remote(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.remote_interaction_allowed() {
            return;
        }
        if let Some(id) = self.remote.current().map(|directory| directory.storage()) {
            self.select_storage(id, self.remote.transfer_epoch(), window, cx);
        }
    }

    fn click_remote_entry(
        &mut self,
        reference: mtp_domain::RemoteRef,
        epoch: u64,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.remote_interaction_allowed() {
            return;
        }
        if self.remote.loading || self.remote.transfer_epoch() != epoch {
            return;
        }
        self.context_menu = None;
        self.remote_focus.focus(window);
        let Some(entry) = self
            .remote
            .entries
            .iter()
            .find(|entry| entry.reference == reference)
        else {
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

    fn upload_plan(&self) -> Result<TransferPlan, &'static str> {
        if matches!(self.remote.config, RemoteConfig::LocalOnly) {
            return Err("仅本地预览：设备访问与上传已禁用。");
        }
        if !self.remote_interaction_allowed() || self.remote.loading {
            return Err("设备正忙或待核对，不能上传。");
        }
        let session = self.remote.session.as_ref().ok_or("请先连接设备。")?;
        if !session.capabilities.upload {
            return Err("HELLO 未提供已验证上传能力；下载能力另行判断。");
        }
        let entry = self
            .local
            .selected_entry()
            .ok_or("请先选中一个本地普通文件。");
        let entry = entry?;
        let destination = self
            .remote
            .current()
            .cloned()
            .ok_or("请先选择远端存储或目录。");
        let destination = destination?;
        let cache = self
            .transfer
            .cache
            .clone()
            .ok_or("HOME 不可用，未配置安全传输缓存。");
        let cache = cache?;
        TransferPlan::upload(entry, destination, self.remote.address(), cache)
    }

    fn download_plan(&self) -> Result<TransferPlan, &'static str> {
        if matches!(self.remote.config, RemoteConfig::LocalOnly) {
            return Err("仅本地预览：设备访问与下载已禁用。");
        }
        if !self.remote_interaction_allowed() || self.remote.loading {
            return Err("设备正忙或待核对，不能下载。");
        }
        let session = self.remote.session.as_ref().ok_or("请先连接设备。")?;
        if !session.capabilities.download {
            return Err("HELLO 未提供已验证下载能力；上传能力另行判断。");
        }
        if self.local.loading || self.local.error.is_some() {
            return Err("请等待本地目标目录加载成功。");
        }
        let entry = self
            .remote
            .selected
            .as_ref()
            .and_then(|reference| {
                self.remote
                    .entries
                    .iter()
                    .find(|entry| &entry.reference == reference)
            })
            .ok_or("请先选中一个远端普通文件。");
        let entry = entry?;
        if entry.directory {
            return Err("当前仅支持普通文件下载；目录暂不支持，不会递归。");
        }
        Ok(TransferPlan::Download {
            source: entry.reference.clone(),
            source_name: entry.name.clone(),
            source_address: self.remote.address(),
            destination: self.local.path.clone(),
            policy: self.transfer.download_policy,
        })
    }

    fn start_upload(&mut self, cx: &mut Context<Self>) {
        if self.transfer.busy() || self.transfer.requires_review {
            return;
        }
        match self.upload_plan() {
            Ok(plan) => self.submit_transfer(plan, cx),
            Err(message) => {
                self.transfer.reject(message);
                cx.notify();
            }
        }
    }

    fn start_download(&mut self, cx: &mut Context<Self>) {
        if self.transfer.busy() || self.transfer.requires_review {
            return;
        }
        match self.download_plan() {
            Ok(plan) => self.submit_transfer(plan, cx),
            Err(message) => {
                self.transfer.reject(message);
                cx.notify();
            }
        }
    }

    fn submit_transfer(&mut self, plan: TransferPlan, cx: &mut Context<Self>) {
        if matches!(self.remote.config, RemoteConfig::LocalOnly) {
            return;
        }
        self.context_menu = None;
        let id = match self.transfer.begin(plan.clone()) {
            Ok(id) => id,
            Err(message) => {
                self.transfer.reject(message);
                cx.notify();
                return;
            }
        };
        let epoch = self.remote.transfer_epoch();
        let shared = self.remote.shared.clone();
        cx.notify();
        let task = cx
            .background_executor()
            .spawn(async move { remote::run_transfer(shared, epoch, plan) });
        cx.spawn(async move |this, cx| {
            let report = task.await;
            // Not gated by either pane's browse revision: local navigation does
            // not discard terminal transfer results or the captured destination.
            let _ = this.update(cx, |this, cx| {
                if this.transfer.complete(id, report.outcome) {
                    if !report.session_usable {
                        this.remote.clear_references();
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn acknowledge_transfer_review(&mut self, cx: &mut Context<Self>) {
        if self.transfer.acknowledge_review() {
            self.remote.clear_references();
            cx.notify();
        }
    }

    fn transfer_panel(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        task_ui::panel(self, cx)
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
        let status = if self.transfer.requires_review {
            "RequiresReview：远端请求已锁定，请在任务区核对并人工确认。".into()
        } else if self.transfer.busy() {
            "单文件处理中：远端操作暂锁，本地浏览可继续。".into()
        } else if self.remote.loading {
            "设备请求处理中… / Loading".to_owned()
        } else if self.remote.error.is_some() {
            "设备错误 · 修正配置或手动重连；不自动重试。".into()
        } else if connected {
            format!(
                "已连接 · {} 存储 · {} 项",
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
            .unwrap_or_else(|| "单击选择 · 双击目录 · 单文件按 HELLO 能力传输 · 不删除".into());
        let can_connect = matches!(&self.remote.config, RemoteConfig::Ready { .. })
            && !self.remote.loading
            && self.remote_interaction_allowed();
        let can_disconnect =
            (connected || self.remote.loading) && self.remote_interaction_allowed();

        div()
            .id("remote-pane")
            .track_focus(&self.remote_focus)
            .key_context("RemotePane")
            .on_action(cx.listener(Self::refresh_remote))
            .on_action(cx.listener(Self::parent_remote))
            .on_action(cx.listener(|this, _: &PreviousRemote, _, cx| this.move_remote(SelectionMove::Previous, cx)))
            .on_action(cx.listener(|this, _: &NextRemote, _, cx| this.move_remote(SelectionMove::Next, cx)))
            .on_action(cx.listener(|this, _: &FirstRemote, _, cx| this.move_remote(SelectionMove::First, cx)))
            .on_action(cx.listener(|this, _: &LastRemote, _, cx| this.move_remote(SelectionMove::Last, cx)))
            .on_action(cx.listener(Self::enter_remote))
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
                    .gap_1()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(0x30394a))
                    .child(div().text_lg().text_ellipsis().child(format!("{title}{}", if self.remote_focus.is_focused(window) { " · 当前键盘面板" } else { "" })))
                    .child(div().flex().gap_1().overflow_hidden().children({
                        let epoch = self.remote.transfer_epoch();
                        self.remote.locations.iter().enumerate().skip(self.remote.locations.len().saturating_sub(3)).map(|(index, location)| {
                            div().id(("remote-crumb", index)).px_1().rounded_sm().text_sm().cursor_pointer().text_ellipsis()
                                .bg(rgb(0x263245)).when(!self.remote_interaction_allowed() || self.remote.loading, |this| this.opacity(0.4))
                                .child(format!("{} ›", location.label))
                                .on_click(cx.listener(move |this, _, window, cx| this.navigate_remote_ancestor(index, epoch, window, cx)))
                        }).collect::<Vec<_>>()
                    }))
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
                                    .when(
                                        self.remote.current().is_none()
                                            || !self.remote_interaction_allowed(),
                                        |this| this.opacity(0.4),
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.remote_focus.focus(window);
                                        this.refresh_remote(&RefreshRemote, window, cx);
                                    })),
                            )
                            .child(
                                toolbar_button("parent-remote", "上级 ⌘B")
                                    .when(
                                        self.remote.locations.len() < 2
                                            || !self.remote_interaction_allowed(),
                                        |this| this.opacity(0.4),
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.remote_focus.focus(window);
                                        this.parent_remote(&ParentRemote, window, cx);
                                    })),
                            )
                            .child(
                                toolbar_button("root-remote", "根目录")
                                    .when(
                                        !connected || !self.remote_interaction_allowed(),
                                        |this| this.opacity(0.4),
                                    )
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
                                    let storage_id = mtp_domain::StorageId(storage.id);
                                    let storage_epoch = self.remote.transfer_epoch();
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
                                        .when(!self.remote_interaction_allowed() || self.remote.loading, |this| {
                                            this.opacity(0.4)
                                        })
                                        .child(storage.name.clone())
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.select_storage(storage_id, storage_epoch, window, cx)
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
                                    .text_color(rgb(if self.remote.error.is_some() { 0xe9bc73 } else if self.remote.loading { 0x8fa9cf } else { 0x96a3b8 }))
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
                                            let row_reference = entry.reference.clone();
                                            let menu_reference = row_reference.clone();
                                            let epoch = this.remote.transfer_epoch();
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
                                                .hover(move |this| this.bg(rgb(if selected { 0x314f79 } else { 0x29374d })))
                                                .child(div().w(px(8.0)).flex_shrink_0().child(if selected { ">" } else { "" }))
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
                                                            row_reference.clone(), epoch, event, window, cx,
                                                        )
                                                    },
                                                ))
                                                .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                                                    this.open_context(ContextIdentity::Remote(menu_reference.clone(), epoch), event.position, window, cx);
                                                }))
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
            .on_action(cx.listener(Self::dismiss_context))
            .relative()
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
                        if self.transfer.requires_review {
                            "RequiresReview · 设备请求已锁定"
                        } else if matches!(self.remote.config, RemoteConfig::LocalOnly) {
                            "仅本地预览 · 不访问设备"
                        } else if self.remote.session.is_some() {
                            "本地 / MTP 目录 · 已连接"
                        } else if self.remote.loading {
                            "本地 / MTP 目录 · 请求中"
                        } else {
                            "本地 / MTP 目录 · 未连接"
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
            .child(self.transfer_panel(cx))
            .when(self.context_menu.is_some(), |this| {
                this.child(gpui::deferred(context_ui::menu(self, cx)))
            })
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
                    .child("安全单文件 · 不覆盖 / 不删除")
                    .child("↑↓ 选择 · Enter 进目录 · Tab 切面板 · ⌘R 刷新"),
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
        KeyBinding::new("up", PreviousLocal, Some("LocalPane")),
        KeyBinding::new("down", NextLocal, Some("LocalPane")),
        KeyBinding::new("home", FirstLocal, Some("LocalPane")),
        KeyBinding::new("end", LastLocal, Some("LocalPane")),
        KeyBinding::new("enter", EnterLocal, Some("LocalPane")),
        KeyBinding::new("backspace", ParentLocal, Some("LocalPane")),
        KeyBinding::new("alt-up", ParentLocal, Some("LocalPane")),
        KeyBinding::new("up", PreviousRemote, Some("RemotePane")),
        KeyBinding::new("down", NextRemote, Some("RemotePane")),
        KeyBinding::new("home", FirstRemote, Some("RemotePane")),
        KeyBinding::new("end", LastRemote, Some("RemotePane")),
        KeyBinding::new("enter", EnterRemote, Some("RemotePane")),
        KeyBinding::new("backspace", ParentRemote, Some("RemotePane")),
        KeyBinding::new("alt-up", ParentRemote, Some("RemotePane")),
        KeyBinding::new("escape", DismissContext, None),
        KeyBinding::new("tab", NextPane, None),
        KeyBinding::new("shift-tab", PreviousPane, None),
    ]);
}

fn main() {
    // Resolve the initial native path once, before entering GPUI's render lifecycle.
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .filter(|home| home.is_absolute());
    let cache = home
        .as_ref()
        .map(|home| home.join("Library/Caches/MTP-Native/transfers"));
    let initial_path = home.unwrap_or_else(|| PathBuf::from("/"));

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
                    title: Some("MTP — 目录浏览 / 单文件传输".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| MtpShell::new(initial_path, remote_config, cache, window, cx)),
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

    fn seed_single_file_session(shell: &mut MtpShell) {
        let key = match mtp_domain::DeviceKey::new("private-ui-fixture") {
            Ok(key) => key,
            Err(_) => panic!("invalid test key"),
        };
        let session = mtp_application::SessionInfo {
            device_key: key.clone(),
            display_name: "Fixture display name".into(),
            generation: mtp_domain::SessionGeneration(42),
            capabilities: mtp_backend_api::Capabilities {
                simulated: false,
                browse: true,
                upload: true,
                download: true,
                cancel_current: false,
            },
        };
        let root = session.root(mtp_domain::StorageId(1));
        let source = mtp_domain::RemoteRef::new(
            key,
            session.generation,
            mtp_domain::StorageId(1),
            mtp_domain::ObjectId(3),
        );
        let revision = match shell
            .remote
            .begin(&RemoteCommand::Connect(RemoteConfig::Unconfigured))
        {
            Some(revision) => revision,
            None => panic!("unexpected test epoch rejection"),
        };
        assert!(shell.remote.complete(
            revision,
            Ok(remote::RemoteReply::Connected {
                session,
                storages: vec![mtp_backend_api::Storage {
                    id: 1,
                    name: "Storage".into(),
                    capacity: None
                }],
                root: Some(RemoteLocation {
                    directory: root.clone(),
                    label: "Storage".into()
                }),
                entries: vec![mtp_application::RemoteEntry {
                    reference: source.clone(),
                    name: "remote.txt".into(),
                    directory: false,
                    byte_size: Some(3)
                }],
            })
        ));
        shell.remote.locations.push(RemoteLocation {
            directory: root,
            label: "Folder".into(),
        });
        shell.remote.selected = Some(source);
        let path = shell.local.path.join("中文.txt");
        shell.local.entries = vec![mtp_local_fs::LocalEntry {
            name: "中文.txt".into(),
            path: path.clone(),
            kind: EntryKind::File,
            byte_size: Some(3),
            modified_at: None,
            metadata_error: None,
        }];
        shell.local.selected = Some(path);
        shell.transfer.cache = Some(PathBuf::from(
            "/home/test/Library/Caches/MTP-Native/transfers",
        ));
    }

    #[gpui::test]
    fn stale_menu_callback_never_operates_or_closes_newer_menu(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            MtpShell::state(
                PathBuf::from("/captured/local"),
                RemoteConfig::Unconfigured,
                window,
                cx,
            )
        });
        cx.update(|window, cx| {
            view.update(cx, |shell, cx| {
                seed_single_file_session(shell);
                let path_a = shell.local.entries[0].path.clone();
                let mut second = shell.local.entries[0].clone();
                second.name = "B.txt".into();
                second.path = shell.local.path.join(&second.name);
                let path_b = second.path.clone();
                shell.local.entries.push(second);
                let position = gpui::Point {
                    x: px(10.0),
                    y: px(10.0),
                };
                let revision = shell.local.snapshot_revision();
                shell.open_context(
                    ContextIdentity::Local(path_a, revision),
                    position,
                    window,
                    cx,
                );
                let expected_a = match shell.context_menu.as_ref() {
                    Some(menu) => menu.ticket.clone(),
                    None => panic!("missing menu A"),
                };
                shell.open_context(
                    ContextIdentity::Local(path_b.clone(), revision),
                    position,
                    window,
                    cx,
                );
                let expected_b = match shell.context_menu.as_ref() {
                    Some(menu) => menu.ticket.clone(),
                    None => panic!("missing menu B"),
                };
                cx.write_to_clipboard(gpui::ClipboardItem::new_string("sentinel".into()));
                assert!(!shell.context_copy(&expected_a, false, cx));
                assert!(!shell.context_activate(&expected_a, false, window, cx));
                assert!(!shell.context_dismiss(&expected_a, cx));
                assert!(!shell.transfer.busy());
                assert_eq!(
                    cx.read_from_clipboard().and_then(|item| item.text()),
                    Some("sentinel".into())
                );
                assert!(
                    shell
                        .context_menu
                        .as_ref()
                        .is_some_and(|menu| menu.ticket.token == expected_b.token)
                );
                shell.open_context(
                    ContextIdentity::Local(path_b, revision),
                    position,
                    window,
                    cx,
                );
                assert!(!shell.context_copy(&expected_b, true, cx));
                assert!(
                    shell
                        .context_menu
                        .as_ref()
                        .is_some_and(|menu| menu.ticket.token != expected_b.token)
                );
                assert_eq!(
                    cx.read_from_clipboard().and_then(|item| item.text()),
                    Some("sentinel".into())
                );
            })
        });
    }

    #[gpui::test]
    fn storage_callback_uses_id_and_fences_old_epoch_after_reconnect(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            MtpShell::state(
                PathBuf::from("/captured/local"),
                RemoteConfig::Unconfigured,
                window,
                cx,
            )
        });
        view.update(cx, |shell, _| {
            seed_single_file_session(shell);
            let epoch = shell.remote.transfer_epoch();
            shell.remote.storages.push(mtp_backend_api::Storage {
                id: 2,
                name: "Second storage".into(),
                capacity: None,
            });
            shell.remote.storages.swap(0, 1);
            let root = match shell.storage_snapshot_root(mtp_domain::StorageId(1), epoch) {
                Some(root) => root,
                None => panic!("missing storage by ID"),
            };
            assert_eq!(root.directory.storage(), mtp_domain::StorageId(1));
            assert_eq!(root.label, "Storage");
            seed_single_file_session(shell);
            let Some(session) = shell.remote.session.as_mut() else {
                panic!("missing fixture session");
            };
            session.generation = mtp_domain::SessionGeneration(43);
            assert!(
                shell
                    .storage_snapshot_root(mtp_domain::StorageId(1), epoch)
                    .is_none()
            );
            let fresh = match shell
                .storage_snapshot_root(mtp_domain::StorageId(1), shell.remote.transfer_epoch())
            {
                Some(root) => root,
                None => panic!("missing fresh root"),
            };
            assert_eq!(
                fresh.directory.generation(),
                mtp_domain::SessionGeneration(43)
            );
            shell.remote.loading = true;
            assert!(
                shell
                    .storage_snapshot_root(mtp_domain::StorageId(1), shell.remote.transfer_epoch())
                    .is_none()
            );
        });
    }

    #[gpui::test]
    fn keyboard_selection_uses_native_identity_and_file_enter_never_transfers(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(bind_keys);
        let (view, cx) = cx.add_window_view(|window, cx| {
            MtpShell::state(
                PathBuf::from("/captured/local"),
                RemoteConfig::Unconfigured,
                window,
                cx,
            )
        });
        view.update(cx, |shell, _| {
            seed_single_file_session(shell);
            let mut second = shell.local.entries[0].clone();
            second.name = "second.txt".into();
            second.path = shell.local.path.join(&second.name);
            shell.local.entries.push(second);
        });
        cx.update(|window, _| window.activate_window());
        cx.simulate_keystrokes("down enter tab enter");
        view.update(cx, |shell, _| {
            assert_eq!(
                shell.local.selected,
                Some(PathBuf::from("/captured/local/second.txt"))
            );
            assert!(!shell.local.loading && !shell.remote.loading);
            assert!(!shell.transfer.busy());
        });
        cx.simulate_keystrokes("shift-tab home end up escape");
        view.update(cx, |shell, _| assert!(shell.local.selected.is_none()));
    }

    #[gpui::test]
    fn busy_keyboard_breadcrumb_and_stale_context_cannot_change_remote_task(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(bind_keys);
        let (view, cx) = cx.add_window_view(|window, cx| {
            MtpShell::state(
                PathBuf::from("/captured/local"),
                RemoteConfig::Unconfigured,
                window,
                cx,
            )
        });
        let id = view.update(cx, |shell, _| {
            seed_single_file_session(shell);
            let plan = match shell.download_plan() {
                Ok(plan) => plan,
                Err(_) => panic!("missing fixture plan"),
            };
            match shell.transfer.begin(plan) {
                Ok(id) => id,
                Err(_) => panic!("unexpected fixture rejection"),
            }
        });
        cx.simulate_keystrokes("tab down home end enter backspace alt-up cmd-r");
        cx.update(|window, cx| {
            view.update(cx, |shell, cx| {
                let epoch = shell.remote.transfer_epoch();
                assert_eq!(shell.remote.locations.len(), 2);
                assert!(!shell.remote.loading);
                shell.navigate_remote_ancestor(0, epoch, window, cx);
                assert_eq!(shell.remote.locations.len(), 2);
                let Some(reference) = shell.remote.selected.clone() else {
                    panic!("missing fixture selection");
                };
                let remote_identity = ContextIdentity::Remote(reference, epoch);
                assert!(!shell.context_identity_valid(&remote_identity));
                let path = shell.local.entries[0].path.clone();
                let identity = ContextIdentity::Local(path, shell.local.snapshot_revision());
                assert!(shell.context_identity_valid(&identity));
                assert!(
                    shell
                        .local
                        .begin(PathBuf::from("/navigated/elsewhere"))
                        .is_ok()
                );
                assert!(!shell.context_identity_valid(&identity));
                assert!(
                    shell
                        .transfer
                        .complete(id, transfers::TransferOutcome::Skipped)
                );
                assert_eq!(
                    shell.transfer.target_summary().as_deref(),
                    Some("捕获下载目标: /captured/local · 源: /Storage/Folder/remote.txt")
                );
            })
        });
    }

    #[gpui::test]
    fn invalid_actions_after_completion_keep_receipt_path_journal_and_capture(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            MtpShell::state(
                PathBuf::from("/captured/local"),
                RemoteConfig::Unconfigured,
                window,
                cx,
            )
        });
        view.update(cx, |shell, cx| {
            seed_single_file_session(shell);
            let plan = match shell.download_plan() { Ok(plan) => plan, Err(_) => panic!("missing fixture plan") };
            let id = match shell.transfer.begin(plan) { Ok(id) => id, Err(_) => panic!("unexpected fixture rejection") };
            fn local_path(value: &str) -> mtp_backend_api::LocalPath {
                match mtp_backend_api::LocalPath::from_path(std::path::Path::new(value)) { Ok(path) => path, Err(_) => panic!("invalid fixture path") }
            }
            assert!(shell.transfer.complete(id, transfers::TransferOutcome::Completed(mtp_application::TransferReceipt {
                direction: mtp_backend_api::TransferDirection::Download, actual_name: "actual.txt".into(), bytes: 17, object_id: Some(3),
                local_path: Some(local_path("/captured/local/actual.txt")), journal_path: local_path("/captured/local/stage/journal.json"),
            })));
            let target = shell.transfer.target_summary();
            shell.local.entries[0].kind = EntryKind::Directory;
            assert!(shell.upload_plan().is_err());
            shell.start_upload(cx);
            let Some(session) = shell.remote.session.as_mut() else { panic!("missing fixture session"); };
            session.capabilities.download = false;
            shell.start_download(cx);
            assert!(!shell.transfer.busy());
            assert!(matches!(&shell.transfer.terminal, Some(transfers::TransferOutcome::Completed(receipt)) if receipt.actual_name == "actual.txt" && receipt.bytes == 17));
            assert_eq!(shell.transfer.published_path(), Some(PathBuf::from("/captured/local/actual.txt")));
            assert_eq!(shell.transfer.journal(), Some(PathBuf::from("/captured/local/stage/journal.json")));
            assert_eq!(shell.transfer.target_summary(), target);
        });
    }

    #[gpui::test]
    fn local_only_blocks_configured_requests_and_submission_even_with_fixture_refs(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            MtpShell::state(
                PathBuf::from("/captured/local"),
                RemoteConfig::Unconfigured,
                window,
                cx,
            )
        });
        cx.update(|window, cx| {
            view.update(cx, |shell, cx| {
                seed_single_file_session(shell);
                let plan = match shell.download_plan() {
                    Ok(plan) => plan,
                    Err(_) => panic!("missing fixture plan"),
                };
                let epoch = shell.remote.transfer_epoch();
                let current = shell.remote.current().cloned();
                shell.remote.config = RemoteConfig::LocalOnly;
                shell.remote.entries[0].directory = true;
                shell.connect_remote(window, cx);
                shell.disconnect_remote(window, cx);
                shell.refresh_remote(&RefreshRemote, window, cx);
                shell.parent_remote(&ParentRemote, window, cx);
                shell.select_storage(mtp_domain::StorageId(1), epoch, window, cx);
                shell.root_remote(window, cx);
                shell.navigate_remote_ancestor(0, epoch, window, cx);
                shell.enter_remote(&EnterRemote, window, cx);
                shell.request_remote(
                    RemoteCommand::Connect(RemoteConfig::Ready {
                        worker_path: PathBuf::from("/must-not-execute"),
                        library_path: PathBuf::from("/must-not-load.dylib"),
                    }),
                    cx,
                );
                shell.submit_transfer(plan, cx);
                assert_eq!(shell.remote.transfer_epoch(), epoch);
                assert_eq!(shell.remote.locations.len(), 2);
                assert!(shell.remote.current().cloned() == current);
                assert!(!shell.remote.loading && !shell.transfer.busy());
                assert!(shell.upload_plan().is_err() && shell.download_plan().is_err());
            })
        });
    }

    #[gpui::test]
    fn asymmetric_capabilities_and_selected_folder_do_not_change_download_destination(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            MtpShell::state(
                PathBuf::from("/captured/local"),
                RemoteConfig::Unconfigured,
                window,
                cx,
            )
        });
        view.update(cx, |shell, _| {
            seed_single_file_session(shell);
            let Some(session) = shell.remote.session.as_mut() else { panic!("missing fixture session"); };
            session.capabilities.upload = false; session.capabilities.download = true;
            shell.local.entries[0].kind = EntryKind::Directory;
            assert!(shell.upload_plan().is_err());
            let plan = match shell.download_plan() { Ok(plan) => plan, Err(_) => panic!("download capability should remain usable") };
            assert!(matches!(plan, TransferPlan::Download { destination, .. } if destination == std::path::Path::new("/captured/local")));
        });
    }

    #[gpui::test]
    fn single_file_busy_handlers_preserve_epoch_and_terminal_uses_captured_target(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            MtpShell::state(
                PathBuf::from("/captured/local"),
                RemoteConfig::Unconfigured,
                window,
                cx,
            )
        });
        cx.update(|window, cx| {
            view.update(cx, |shell, cx| {
                seed_single_file_session(shell);
                assert!(shell.upload_plan().is_ok());
                let plan = match shell.download_plan() {
                    Ok(plan) => plan,
                    Err(_) => panic!("expected download snapshot"),
                };
                let epoch = shell.remote.transfer_epoch();
                let current = shell.remote.current().cloned();
                let id = match shell.transfer.begin(plan) {
                    Ok(id) => id,
                    Err(_) => panic!("unexpected task rejection"),
                };
                shell.connect_remote(window, cx);
                shell.disconnect_remote(window, cx);
                shell.refresh_remote(&RefreshRemote, window, cx);
                shell.parent_remote(&ParentRemote, window, cx);
                shell.select_storage(mtp_domain::StorageId(1), epoch, window, cx);
                shell.root_remote(window, cx);
                shell.request_remote(RemoteCommand::Disconnect, cx);
                shell.start_upload(cx);
                shell.start_download(cx);
                assert_eq!(shell.remote.transfer_epoch(), epoch);
                assert!(shell.remote.current().cloned() == current);
                assert!(shell.remote.session.is_some());
                assert_eq!(shell.remote.locations.len(), 2);
                assert!(shell.transfer.busy());
                assert!(
                    shell
                        .local
                        .begin(PathBuf::from("/user/navigated/elsewhere"))
                        .is_ok()
                );
                assert!(
                    shell
                        .transfer
                        .complete(id, transfers::TransferOutcome::Skipped)
                );
                assert_eq!(shell.local.path, PathBuf::from("/user/navigated/elsewhere"));
                assert_eq!(
                    shell.transfer.target_summary().as_deref(),
                    Some("捕获下载目标: /captured/local · 源: /Storage/Folder/remote.txt")
                );
                assert!(shell.transfer.summary().contains("Skipped"));
            })
        });
    }

    #[gpui::test]
    fn legacy_capabilities_and_directory_selection_disable_single_file_plans(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            MtpShell::state(
                PathBuf::from("/captured/local"),
                RemoteConfig::Unconfigured,
                window,
                cx,
            )
        });
        view.update(cx, |shell, _| {
            seed_single_file_session(shell);
            let Some(session) = shell.remote.session.as_mut() else {
                panic!("missing fixture session")
            };
            session.capabilities.upload = false;
            session.capabilities.download = false;
            assert!(shell.upload_plan().is_err());
            assert!(shell.download_plan().is_err());
            let Some(session) = shell.remote.session.as_mut() else {
                panic!("missing fixture session")
            };
            session.capabilities.upload = true;
            session.capabilities.download = true;
            shell.local.entries[0].kind = EntryKind::Directory;
            shell.remote.entries[0].directory = true;
            assert!(shell.upload_plan().is_err());
            assert!(shell.download_plan().is_err());
        });
    }

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
