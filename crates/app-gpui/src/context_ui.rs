//! Small right-click popover; captures native identity and snapshot epoch.
use super::{
    ContextIdentity, ContextPopover, ContextTicket, EnterLocal, EnterRemote, MtpShell,
    toolbar_button,
};
use gpui::{Context, IntoElement, MouseButton, Window, div, prelude::*, px, rgb};

impl MtpShell {
    pub(super) fn context_identity_valid(&self, identity: &ContextIdentity) -> bool {
        match identity {
            ContextIdentity::Local(path, revision) => {
                !self.local.loading
                    && self.local.snapshot_revision() == *revision
                    && self.local.selected.as_ref() == Some(path)
                    && self.local.entries.iter().any(|entry| &entry.path == path)
            }
            ContextIdentity::Remote(reference, epoch) => {
                self.remote_interaction_allowed()
                    && !self.remote.loading
                    && self.remote.transfer_epoch() == *epoch
                    && self.remote.selected.as_ref() == Some(reference)
                    && self
                        .remote
                        .entries
                        .iter()
                        .any(|entry| &entry.reference == reference)
            }
        }
    }

    pub(super) fn open_context(
        &mut self,
        identity: ContextIdentity,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(token) = self.context_serial.checked_add(1) else {
            return;
        };
        match &identity {
            ContextIdentity::Local(path, revision) => {
                if self.local.loading
                    || self.local.snapshot_revision() != *revision
                    || !self.local.entries.iter().any(|entry| &entry.path == path)
                {
                    return;
                }
                self.local.selected = Some(path.clone());
                self.local_focus.focus(window);
            }
            ContextIdentity::Remote(reference, epoch) => {
                if !self.remote_interaction_allowed()
                    || self.remote.loading
                    || self.remote.transfer_epoch() != *epoch
                    || !self
                        .remote
                        .entries
                        .iter()
                        .any(|entry| &entry.reference == reference)
                {
                    return;
                }
                self.remote.selected = Some(reference.clone());
                self.remote_focus.focus(window);
            }
        }
        let viewport = window.viewport_size();
        let position = gpui::Point {
            x: px(f32::from(position.x)
                .min((f32::from(viewport.width) - 312.0).max(8.0))
                .max(8.0)),
            y: px(f32::from(position.y)
                .min((f32::from(viewport.height) - 240.0).max(8.0))
                .max(8.0)),
        };
        self.context_serial = token;
        self.context_menu = Some(ContextPopover {
            ticket: ContextTicket { identity, token },
            position,
        });
        cx.notify();
    }

    fn context_ticket_current(&self, expected: &ContextTicket) -> bool {
        self.context_menu.as_ref().is_some_and(|menu| {
            menu.ticket.token == expected.token && menu.ticket.identity == expected.identity
        })
    }

    pub(super) fn context_dismiss(
        &mut self,
        expected: &ContextTicket,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.context_ticket_current(expected) {
            return false;
        }
        self.context_menu = None;
        cx.notify();
        true
    }

    pub(super) fn context_copy(
        &mut self,
        expected: &ContextTicket,
        path: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.context_ticket_current(expected)
            || !self.context_identity_valid(&expected.identity)
        {
            return false;
        }
        let text = match &expected.identity {
            ContextIdentity::Local(identity, _) => self
                .local
                .entries
                .iter()
                .find(|entry| &entry.path == identity)
                .and_then(|entry| {
                    if path {
                        entry.path.to_str()
                    } else {
                        entry.name.to_str()
                    }
                })
                .map(str::to_owned),
            ContextIdentity::Remote(identity, _) => self
                .remote
                .entries
                .iter()
                .find(|entry| &entry.reference == identity)
                .map(|entry| entry.name.clone()),
        };
        let Some(text) = text else {
            return false;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.context_menu = None;
        cx.notify();
        true
    }

    pub(super) fn context_activate(
        &mut self,
        expected: &ContextTicket,
        directory: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.context_ticket_current(expected)
            || !self.context_identity_valid(&expected.identity)
        {
            return false;
        }
        self.context_menu = None;
        match &expected.identity {
            ContextIdentity::Local(_, _) => {
                if directory {
                    self.enter_local(&EnterLocal, window, cx);
                } else {
                    self.start_upload(cx);
                }
            }
            ContextIdentity::Remote(_, _) => {
                if directory {
                    self.enter_remote(&EnterRemote, window, cx);
                } else {
                    self.start_download(cx);
                }
            }
        }
        cx.notify();
        true
    }
}

pub(super) fn menu(shell: &MtpShell, cx: &mut Context<MtpShell>) -> impl IntoElement + use<> {
    let Some(menu) = &shell.context_menu else {
        return div();
    };
    let expected = menu.ticket.clone();
    let valid = shell.context_identity_valid(&menu.ticket.identity);
    let local = matches!(menu.ticket.identity, ContextIdentity::Local(_, _));
    let (name, directory, copy_name, copy_path) = match &menu.ticket.identity {
        ContextIdentity::Local(path, _) => shell
            .local
            .entries
            .iter()
            .find(|entry| &entry.path == path)
            .map(|entry| {
                (
                    entry.name.to_string_lossy().into_owned(),
                    entry.kind == mtp_local_fs::EntryKind::Directory,
                    entry.name.to_str().is_some(),
                    entry.path.to_str().is_some(),
                )
            })
            .unwrap_or_else(|| ("条目已变化".into(), false, false, false)),
        ContextIdentity::Remote(reference, _) => shell
            .remote
            .entries
            .iter()
            .find(|entry| &entry.reference == reference)
            .map(|entry| (entry.name.clone(), entry.directory, true, false))
            .unwrap_or_else(|| ("条目已变化".into(), false, false, false)),
    };
    let plan = if local {
        shell.upload_plan()
    } else {
        shell.download_plan()
    };
    let can_transfer = valid && !directory && plan.is_ok();
    let reason = if !valid {
        "条目/快照已改变，请重新选择。"
    } else if directory {
        "目录传输暂不支持，不递归。"
    } else {
        plan.as_ref()
            .err()
            .copied()
            .unwrap_or("仅单个普通文件；不覆盖/不删除。")
    };
    div()
        .absolute()
        .left(px(0.0))
        .top(px(0.0))
        .size_full()
        .child(
            div()
                .id("context-backdrop")
                .absolute()
                .size_full()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener({
                        let expected = expected.clone();
                        move |this, _, _, cx| {
                            let _ = this.context_dismiss(&expected, cx);
                        }
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener({
                        let expected = expected.clone();
                        move |this, _, _, cx| {
                            let _ = this.context_dismiss(&expected, cx);
                        }
                    }),
                ),
        )
        .child(
            div()
                .id("entry-context-menu")
                .absolute()
                .left(menu.position.x)
                .top(menu.position.y)
                .w(px(300.0))
                .max_h(px(230.0))
                .overflow_y_scroll()
                .p_3()
                .rounded_md()
                .shadow_lg()
                .bg(rgb(0x202b3e))
                .border_1()
                .border_color(rgb(0x668ed1))
                .flex()
                .flex_col()
                .gap_2()
                .text_sm()
                .child(div().text_ellipsis().child(name))
                .when(directory, |this| {
                    this.child(
                        toolbar_button("context-enter", "进入目录 · Enter")
                            .when(!valid, |this| this.opacity(0.4))
                            .when(valid, |this| {
                                this.on_click(cx.listener({
                                    let expected = expected.clone();
                                    move |this, _, window, cx| {
                                        let _ = this.context_activate(&expected, true, window, cx);
                                    }
                                }))
                            }),
                    )
                })
                .child(
                    toolbar_button(
                        "context-transfer",
                        if local {
                            "上传此普通文件 →"
                        } else {
                            "← 下载此普通文件"
                        },
                    )
                    .when(!can_transfer, |this| this.opacity(0.4))
                    .when(can_transfer, |this| {
                        this.on_click(cx.listener({
                            let expected = expected.clone();
                            move |this, _, window, cx| {
                                let _ = this.context_activate(&expected, false, window, cx);
                            }
                        }))
                    }),
                )
                .child(div().text_color(rgb(0xa9b6ca)).child(reason))
                .child(
                    toolbar_button(
                        "context-copy-name",
                        if copy_name {
                            "复制原始名称"
                        } else {
                            "名称非 UTF-8：不能无损复制"
                        },
                    )
                    .when(!valid || !copy_name, |this| this.opacity(0.4))
                    .when(valid && copy_name, |this| {
                        this.on_click(cx.listener({
                            let expected = expected.clone();
                            move |this, _, _, cx| {
                                let _ = this.context_copy(&expected, false, cx);
                            }
                        }))
                    }),
                )
                .when(local, |this| {
                    this.child(
                        toolbar_button(
                            "context-copy-path",
                            if copy_path {
                                "复制本地原始路径"
                            } else {
                                "路径含非 UTF-8 字节：不可文本复制"
                            },
                        )
                        .when(!valid || !copy_path, |this| this.opacity(0.4))
                        .when(valid && copy_path, |this| {
                            this.on_click(cx.listener({
                                let expected = expected.clone();
                                move |this, _, _, cx| {
                                    let _ = this.context_copy(&expected, true, cx);
                                }
                            }))
                        }),
                    )
                }),
        )
}
