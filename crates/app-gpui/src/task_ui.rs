//! Compact bottom task area. Every label is a snapshot or receipt, never IO authority.
use super::{MtpShell, RemoteConfig, toolbar_button, transfers::TransferOutcome};
use gpui::{Context, IntoElement, div, prelude::*, px, rgb};
use mtp_backend_api::FileConflictPolicy;

fn outcome_line(outcome: &TransferOutcome) -> String {
    match outcome {
        TransferOutcome::Completed(receipt) => {
            format!("已完成 · {} · {} bytes", receipt.actual_name, receipt.bytes)
        }
        TransferOutcome::Skipped => "Skipped / 已跳过 · 未覆盖，不是完成".into(),
        TransferOutcome::Failed {
            message,
            requires_review,
            ..
        } => format!(
            "{} · {message}",
            if *requires_review {
                "结果未知 / RequiresReview"
            } else {
                "未完成"
            }
        ),
    }
}

fn journal(outcome: &TransferOutcome) -> Option<std::path::PathBuf> {
    match outcome {
        TransferOutcome::Completed(receipt) => Some(receipt.journal_path.to_path_buf()),
        TransferOutcome::Failed { journal_path, .. } => journal_path.clone(),
        TransferOutcome::Skipped => None,
    }
}

fn evidence_path(
    id: &'static str,
    path: std::path::PathBuf,
    cx: &mut Context<MtpShell>,
) -> impl IntoElement + use<> {
    let text = path.to_str().map(str::to_owned);
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().child(format!("Journal: {}", path.to_string_lossy())))
        .child(
            toolbar_button(
                id,
                if text.is_some() {
                    "复制 Journal 原始路径"
                } else {
                    "Journal 路径非 UTF-8：不可无损文本复制"
                },
            )
            .when(text.is_none(), |this| this.opacity(0.4))
            .when_some(text, |this, text| {
                this.on_click(cx.listener(move |_, _, _, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
                }))
            }),
        )
}

pub(super) fn panel(shell: &MtpShell, cx: &mut Context<MtpShell>) -> impl IntoElement + use<> {
    let upload = shell.upload_plan();
    let download = shell.download_plan();
    let can_upload = upload.is_ok();
    let can_download = download.is_ok();
    let local_only = matches!(shell.remote.config, RemoteConfig::LocalOnly);
    let upload_cap = shell
        .remote
        .session
        .as_ref()
        .is_some_and(|session| session.capabilities.upload);
    let download_cap = shell
        .remote
        .session
        .as_ref()
        .is_some_and(|session| session.capabilities.download);
    let upload_preview = shell
        .local
        .selected_entry()
        .map(|entry| {
            format!(
                "本地源: {} → 设备当前目录: {}",
                entry.path.to_string_lossy(),
                shell.remote.address()
            )
        })
        .unwrap_or_else(|| "选择本地普通文件 → 设备当前存储/目录".into());
    let download_preview = shell
        .remote
        .selected
        .as_ref()
        .and_then(|reference| {
            shell
                .remote
                .entries
                .iter()
                .find(|entry| &entry.reference == reference)
        })
        .map(|entry| {
            format!(
                "设备源: {}/{} → 本地当前目录: {}",
                shell.remote.address().trim_end_matches('/'),
                entry.name,
                shell.local.path.to_string_lossy()
            )
        })
        .unwrap_or_else(|| {
            format!(
                "选择设备普通文件 → 本地当前目录: {}",
                shell.local.path.to_string_lossy()
            )
        });
    let expanded = shell.task_expanded || shell.transfer.requires_review || shell.transfer.busy();
    let tone = match &shell.transfer.terminal {
        _ if shell.transfer.requires_review => 0xe9bc73,
        Some(TransferOutcome::Completed(_)) => 0x8ed4af,
        Some(TransferOutcome::Skipped) => 0xa9b6ca,
        Some(TransferOutcome::Failed { .. }) => 0xe9bc73,
        _ => 0xb8c5da,
    };
    div().flex().flex_col().flex_shrink_0().gap_1().px_4().py_2().bg(rgb(0x141a25))
        .border_t_1().border_color(rgb(if shell.transfer.requires_review { 0xe9bc73 } else { 0x30394a })).text_sm()
        .when(!shell.transfer.busy() && !shell.transfer.requires_review, |this| this.child(div().flex().gap_3()
            .child(div().flex().flex_col().flex_1().min_w_0().gap_1()
                .child(div().flex().items_center().gap_2()
                    .child(toolbar_button("upload-file", "上传单文件 →").when(!can_upload, |this| this.opacity(0.4))
                        .when(can_upload, |this| this.on_click(cx.listener(|this, _, _, cx| this.start_upload(cx)))))
                    .child(div().text_color(rgb(0x96a3b8)).child(if local_only { "上传 · 预览禁用" } else if upload_cap { "上传 · HELLO 已授权" } else { "上传 · 未验证/未授权" })))
                .child(div().text_color(rgb(0x96a3b8)).text_ellipsis().child(upload.as_ref().err().copied().unwrap_or("将创建随机安全后缀的新文件；最终名以回执为准。"))))
            .child(div().flex().flex_col().flex_1().min_w_0().gap_1()
                .child(div().flex().flex_wrap().items_center().gap_2()
                    .child(toolbar_button("download-file", "← 下载单文件").when(!can_download, |this| this.opacity(0.4))
                        .when(can_download, |this| this.on_click(cx.listener(|this, _, _, cx| this.start_download(cx)))))
                    .child(toolbar_button("download-conflict", match shell.transfer.download_policy {
                        FileConflictPolicy::KeepBoth => "下载：保留两份", FileConflictPolicy::Skip => "下载：已有则跳过",
                    }).when(!shell.transfer.remote_requests_allowed(), |this| this.opacity(0.4))
                        .when(shell.transfer.remote_requests_allowed(), |this| this.on_click(cx.listener(|this, _, _, cx| {
                            if !this.transfer.remote_requests_allowed() { return; }
                            this.transfer.download_policy = match this.transfer.download_policy { FileConflictPolicy::KeepBoth => FileConflictPolicy::Skip, FileConflictPolicy::Skip => FileConflictPolicy::KeepBoth };
                            cx.notify();
                        }))))
                    .child(div().text_color(rgb(0x96a3b8)).child(if local_only { "下载 · 预览禁用" } else if download_cap { "下载 · HELLO 已授权" } else { "下载 · 未验证/未授权" })))
                .child(div().text_color(rgb(0x96a3b8)).text_ellipsis().child(download.as_ref().err().copied().unwrap_or("目标是本地当前目录，不是选中的文件夹。"))))))
        .child(div().flex().items_center().justify_between().gap_2()
            .child(div().flex_1().min_w_0().text_color(rgb(tone)).text_ellipsis().child(shell.transfer.summary()))
            .child(toolbar_button("task-details", if shell.task_expanded { "收起详情 / 近期记录" } else { "源 / 目标 / 证据 / 近期记录" })
                .on_click(cx.listener(|this, _, _, cx| { this.task_expanded = !this.task_expanded; cx.notify(); }))))
        .when(shell.transfer.requires_review, |this| this.child(div().flex().flex_wrap().items_center().gap_2()
            .child(div().flex_1().text_color(rgb(0xe9bc73)).child("结果未知：文件可能已创建。新传输/远端请求已锁定；请核对 journal 与实际文件，不要重试或删除。"))
            .child(toolbar_button("ack-review", "我已核对，解除锁定后手动重连")
                .on_click(cx.listener(|this, _, _, cx| this.acknowledge_transfer_review(cx))))))
        .when(!shell.transfer.busy() && !shell.transfer.requires_review, |this| this.child(div().flex().gap_3().text_color(rgb(0x96a3b8))
            .child(div().flex_1().min_w_0().text_ellipsis().child(format!("下次上传预览 · {upload_preview}")))
            .child(div().flex_1().min_w_0().text_ellipsis().child(format!("下次下载预览 · {download_preview}")))))
        .when_some(shell.transfer.preflight_notice.clone(), |this, notice| this.child(div().text_color(rgb(0xe9bc73)).text_ellipsis().child(format!("交互提示（未提交新任务）: {notice}"))))
        .when(expanded, |this| this.child(div().id("task-evidence-scroll").max_h(px(100.0)).overflow_y_scroll().flex().flex_col().gap_2().py_2().text_color(rgb(0xb8c5da))
            .when_some(shell.transfer.target_summary(), |this, target| this.child(div().text_color(rgb(tone)).child(target)))
            .when(shell.transfer.busy(), |this| this.child(div().child("本任务目标已固定；继续本地浏览不会改变捕获目标。无真实进度事件，因此不展示虚构百分比。")))
            .when_some(shell.transfer.terminal.as_ref(), |this, outcome| this.child(div().text_color(rgb(tone)).child(outcome_line(outcome))))
            .when_some(shell.transfer.published_path(), |this, path| this.child(div().child(format!("实际下载文件: {}", path.to_string_lossy()))))
            .when_some(shell.transfer.journal(), |this, path| this.child(evidence_path("copy-current-journal", path, cx)))
            .when(shell.transfer.requires_review, |this| this.child(div().text_color(rgb(0xe9bc73)).child("人工确认仅解除锁定；原任务仍未知，不重试、不删除证据、不自动连接。")))
            .when(shell.transfer.requires_review && shell.transfer.journal().is_none(), |this| this.child(div().text_color(rgb(0xe9bc73)).child("Journal 位置未回传；请核对捕获目标及 HOME/Library/Caches/MTP-Native/transfers。缺少路径不代表未发生写入。")))
            .when(!shell.transfer.busy() && !shell.transfer.requires_review, |this| this
                .child(div().child(format!("下一次明确点击的源/目标预览: {upload_preview}")))
                .child(div().child(download_preview.clone()))
                .child(div().child("下载到本地当前目录；想下载到选中的文件夹，先进入它。KeepBoth 空闲用原名、冲突用安全随机新名；Skip 不覆盖且不算完成。")))
            .when_some(upload.as_ref().err().copied(), |this, reason| this.child(div().child(format!("上传不可用: {reason}"))))
            .when_some(download.as_ref().err().copied(), |this, reason| this.child(div().child(format!("下载不可用: {reason}"))))
            .when(!shell.transfer.recent.is_empty(), |this| this.child(div().text_color(rgb(0x96a3b8)).child("本窗口近期记录（最多 8 项；不扫描磁盘，不恢复任务，不清理 journal）")))
            .children(shell.transfer.recent.iter().map(|record| {
                let mut card = div().border_t_1().border_color(rgb(0x30394a)).py_2().flex().flex_col().gap_1()
                    .child(div().child(format!("任务 #{} · {}", record.id, outcome_line(&record.outcome))))
                    .when_some(record.target.clone(), |this, target| this.child(div().child(target)));
                if let TransferOutcome::Completed(receipt) = &record.outcome
                    && let Some(path) = &receipt.local_path {
                    card = card.child(div().child(format!("实际下载文件: {}", path.to_path_buf().to_string_lossy())));
                }
                if let Some(path) = journal(&record.outcome) {
                    // Child namespaces make the repeated local ID distinct per record.
                    card = card.child(evidence_path("copy-recent-journal", path, cx));
                }
                div().id(("recent-transfer", record.id as usize)).child(card)
            }))
            .child(div().text_color(rgb(0x96a3b8)).child("目录不会自动刷新；请在对应面板明确刷新。复制路径仅写剪贴板，不读取/打开/重试 journal。"))))
}
