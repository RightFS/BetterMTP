# UI single-file transfers — capability-gated MVP

## 范围与验收状态

本 slice 只修改 `crates/app-gpui`（含 manifest）并新增本文档；没有修改 worker/application/backend/transfer crate、root、README 或打包脚本，没有运行 Cargo、执行真实传输、Git commit 或重新打包旧 binary。

父代理确认此前 Rust core 153 tests、UI 14 tests 与 clippy 已通过，baseline 已提交 `e331014`。本轮 UI 新增 single-file handlers / 状态 / 测试以父代理随后统一 Cargo 校验为准，**尚不声称本轮测试或真机传输已通过**。本代理仅依据已落盘 API 完成接线、直接 stable `rustfmt` 和 diff 检查。

本轮支持：明确点击「上传单文件 →」与「← 下载单文件」。按钮依实际连接的 HELLO `upload` / `download` 能力启用；legacy 或能力未验证连接仍只有浏览，不以 UI 自行推断 v2 可用性。没有目录递归、批量、覆盖、删除或假的 USB Cancel 功能。

## 精确服务 API

所有执行都在 `cx.background_executor().spawn` 中，沿用独立共享 worker 的业务 mutex 串行调用：

```rust
WorkerClient::upload_file(
    source: &Path,
    requested_name: &str,
    dest: &RemoteDirectoryRef,
    cache: &Path,
) -> Result<TransferReceipt, ApplicationError>

WorkerClient::download_file(
    source: &RemoteRef,
    dest: &Path,
    policy: FileConflictPolicy, // KeepBoth or Skip
) -> Result<TransferReceipt, ApplicationError>
```

实际 `TransferReceipt` 字段是 `direction / actual_name / bytes / object_id: Option<u32> / local_path: Option<LocalPath> / journal_path: LocalPath`，UI 使用 `LocalPath::to_path_buf()` 保留原生路径，绝不把标签转回操作路径。

- 成功只依据 service 的已验证 receipt，显示 actual name、bytes、实际本地发布路径（download）与 journal 位置；没有百分比/进度 bytes 的虚构上报，也不声称 receipt 证明内容 hash 或设备耐久性。
- 上传使用 KeepBoth 安全语义，actual name 是 backend v2 认可的 crypto suffix 名称（`-mtp-` 加随机 hex suffix），UI 显示实际 final name，不声称 original-name rename 或覆盖成功。
- 下载首 MVP 默认 `KeepBoth`，可在非 busy / 非 review 时切换 `Skip`。policy 在点击时快照捕获，不在执行中修改。
- `Skip` 的服务结果是 `ApplicationError::Backend { code: ErrorCode::Skipped, requires_review: false, .. }`。UI 单独显示 **Skipped / 已跳过，不是 Success**，不生成假的完成 receipt。

## 不可变源 / 目标快照

`transfers.rs::TransferPlan` 在点击时捕获并 clone：

- 上传：选中 adapter 返回的实际 `LocalEntry.path: PathBuf`、原生 `name.to_str()`、当前远端 `RemoteDirectoryRef`（包括 storage root）、地址标签以及 startup cache path。
- 下载：选中普通文件的实际 `RemoteRef`、仅用于显示的 source name、当前本地目录实际 `PathBuf` 与 conflict policy。

上传不使用 `to_string_lossy()` 生成 requested name；basename 非 UTF-8、控制字符、分隔符、空名/点名或过长时明确拒绝。Linux 包含非法 UTF-8 basename 的纯测试；父目录即便不是 UTF-8，也仍使用原生 `PathBuf` 传给 LocalPath，不从展示文本重建。

本地 source 必须是 adapter 标记的普通文件且 metadata 无错误；符号链接、目录、未知类型明确不支持。remote selection 仅接受 `directory == false` 的条目。目标是本地 **当前目录**，不是本地 selected 文件/目录；remote 目标是当前 navigation reference，而不是字符串地址。

最终源文件、session/reference、NOFOLLOW、真实对象类型和发布权限仍由 worker/transfer service 在执行边界再次验证；UI 缓存的 metadata 不能授予写入权限。

## 独立任务 ID 与 busy 屏障

- `TransferState` 使用独立、`checked_add` 不回绕的 task ID，保存 pending immutable plan 和 terminal outcome；不复用本地或远端 browse revision。terminal 完成时丢弃 pending 中的可执行 RemoteRef/DirectoryRef，只保留捕获目标/源的展示记录及 receipt/journal，避免断连或 review 后历史 UI 继续保存旧 session 引用。
- 从 begin 到 terminal 回写期间，connect、disconnect、refresh、上级、root、storage selection、remote row navigation 和第二次 transfer 全部禁用。
- 禁用不只有按钮 opacity：所有会改变 remote 状态的 handler 及 central `request_remote` 都在入口 early return。`Parent` / storage 的引用栈变更也发生在 guard 之后，shortcut 不会绕过。
- busy 时 remote epoch 不变；后台 service 仍验证捕获 epoch 与 reference session，不能在新会话中使用旧对象。
- **本地浏览可继续。** 即使用户进入其他 local folder，terminal result 仍依据独立 task ID 记录，而不是被 local/browse revision 当作迟到结果丢弃。
- 显示「捕获上传/下载目标」与实际发布路径，不自动跳回目标、不自动刷新当前浏览、不自动重试。需要用户在合适面板显式刷新。

## 默认 cache：只创建应用目录，不跟随 symlink

startup 仅解析有效绝对 HOME 并派生：

```text
HOME/Library/Caches/MTP-Native/transfers
```

环境读取不在 Render；HOME 缺失/无效时上传禁用，不退到 root/cwd/旧 app cache。

`ensure_cache` 只在后台执行。App manifest 添加与现有 transfer crate 相同的 pinned `rustix = 1.1.6` `fs` feature：

1. 从 `/` 的 directory FD 逐级 `openat(RDONLY | DIRECTORY | NOFOLLOW | CLOEXEC)`。
2. 既有 HOME、Library、Caches 等所有祖先必须是可访问的真实目录；缺失/符号链接则报错。
3. 只允许 `mkdirat` 创建最后两个应用目录 `MTP-Native` 与 `transfers`，0700 mode；不 `create_dir_all` 创建任意祖先。
4. 每步以已绑定 FD 进行相对操作，创建后再次 NOFOLLOW 打开，不 canonicalize 后逃逸到其他目录。
5. worker 的安全 transfer staging 仍独立验证目录绑定、文件身份、journal/stage ownership 和 publication 边界。

UI 不清理 journal、payload 或用户文件。已有 cache 不被覆盖删除；后台异常仅报告脱敏错误。

## RequiresReview / 错误与显式恢复

这些结果不可显示 Cancelled 或 Success：

- `ApplicationError::UnknownOutcome { journal_path: Option<LocalPath> }`。
- service timeout / connection lost / protocol or receipt ambiguity。
- backend `requires_review == true` 或 `ErrorCode::UnknownOutcome`（包括带 review 标记的 Skipped；不能当作普通 Skip）。

UI 显示 **RequiresReview / 结果未知，不要重试或删除文件**，保留 journal 路径（若 service 提供）与捕获目标。原始 `ApplicationError::Display`、backend message、DeviceKey、serial 或整个 session/reference Debug 不进入 UI 或日志；只显示静态分类 / ErrorCode 和本地 journal 定位路径。

review outcome 保守清掉 worker client 及所有 remote refs。所有后续 transfer 和 remote request 均被独立 review barrier 阻断；普通按钮或 shortcut 不会自动清屏障。

用户必须先核对 journal、实际本地/设备文件，再显式点「**我已核对，允许重新连接**」。该动作只确认人工检查、解除屏障并清旧 refs；**不会伪造成功，不删除 evidence，不重试原任务，不自动连接**。必须之后再手动重新连接，取得新 session/generation 才能选择新的 transfer。

local preflight 名称/metadata/cache 拒绝等可证明未 dispatch 的失败显示「未完成」而非 RequiresReview。失去 session 后清 references；不把 IO error 一概成功或自动 retry。

## 关闭窗口与 Quit：process stop ≠ USB Cancel

长 transfer 持有 business `Mutex<Option<WorkerClient>>`，因此仅在最终 `Arc<SharedWorker>::drop` 清理不足以及时停止进程。

本 slice 接已提供的 `WorkerStopHandle`：

```rust
WorkerClient::stop_handle() -> WorkerStopHandle
WorkerStopHandle::request_stop() -> io::Result<()>
```

- SharedWorker 单独保存 `Mutex<Option<WorkerStopHandle>>`，与业务锁完全独立。
- spawn 返回后立即登记 stop handle，早于 connect/storages/list。
- MtpShell `Context::on_release` 和 `on_app_quit` 都立即调用 quick process stop：只取得独立短锁，request kill，不等待业务任务、USB 返回或 child reap。
- closed flag 与保留 epoch 0 使未执行任务失效；若 close 与 spawn/登记竞争，登记时看到 closed 就立即停止新 child，不再 connect。
- 下层 owning WorkerClient 在后台观察 EOF/UnknownOutcome 并负责 kill/wait/reap；最后 Arc 释放的 cleanup thread 仍作为 RAII 后备。
- 不使用裸 pid 发送信号、不冒充 USB cancellation、不显示 Cancelled。写期间关闭应按未知结果和持久 journal 做人工检查，下次启动不能视为旧任务成功。

有限边界：`WorkerClient::spawn` 自身握手在返回 stop handle 前仍受服务启动 deadline 约束；若窗口在该阶段关闭，返回并注册时立即发现 closed 而 kill，不执行 connect。没有声称 UI 可以取消一个尚未返回其受控句柄的 spawn。

## 新增测试（待父代理统一执行）

macOS 预期本轮新增十项，先前 baseline UI 14 项仍保留，预计 24 项；Linux 另有一个非法 UTF-8 basename 测试。数量/结果以最终 unified Cargo 输出为准。

纯 state / safe-fixture 测试不执行真实 USB、创建用户文件或提交 native transfer：

- immutable download target；local folder 更改不影响 captured target；busy 拒绝新任务与 remote requests。
- 错误 task ID 不能结束 pending；正确 terminal Skip 不等于 Completed。
- review barrier 拒绝新请求，普通 reject 不能覆盖 journal evidence；显式 acknowledgment 保留证据。
- 完成 receipt 显示 actualName / bytes / final path / journal。
- task ID 耗尽不回绕。
- upload basename 安全规则；Linux 非 UTF-8 拒绝且原生字节保持。
- 不符合默认 cache shape 的路径在任何 FS 创建之前拒绝。
- hold business mutex 时 process-stop 仍可使 closed/epoch 失效，不依业务锁。
- Skip、UnknownOutcome、timeout 和带 review 的 Backend error 区分。
- GPUI state-only fixture 验证所有 busy remote handlers 不改变 epoch/root/栈，local navigation 后终态保留捕获目标。
- GPUI legacy upload/download capability false 时 plan disabled；目录 selection 明确不支持。

父代理统一执行 Cargo/clippy/tests/build 后再运行更新的 binary 或包；本轮没有重新打包之前的已生成 binary。

## Manual 真机清单（仍待验收）

- [ ] legacy / v2 能力未确认设备连接仅可浏览，Upload/Download 不 dispatch。
- [ ] v2 已验证能力连接后选择本地普通 UTF-8 名文件与 remote storage root/目录，按钮按能力启用；目录/符号链接源明确不支持。
- [ ] 上传完成实际 receipt name 含 crypto suffix，bytes 与 journal 显示；不覆盖已有原名，不声称 original-name rename。
- [ ] 下载默认 KeepBoth；原名空闲时实际发布名可为原名，冲突时保留旧文件、发布新名；receipt actual path 正确。
- [ ] 切 Skip 并遇已存在 target，显示 Skipped 而非 Success，旧文件不被修改。
- [ ] transfer running 期间点击 connect/disconnect/root/storage、双击remote目录与 CmdR/CmdB 均无变化；本地浏览仍可用。
- [ ] 下载开始后导航到其他 local folder，完成仍显示原捕获目标和 actual path，不跳回、不静默改写新目标。
- [ ] 使用已有 unsafe cache ancestor（symlink）验证报错；不跟随、不 canonicalize、不在目标外 mkdir 或重用旧 app cache。
- [ ] 拔设备 / worker 中断 / timeout / malformed receipt 显示 RequiresReview，journal 保留，无 auto retry / Cancelled / Success。
- [ ] review 状态下新 transfer / remote requests 被阻断；人工核对后显式确认，再手动重连取得新 refs。
- [ ] long transfer 时关闭窗口 / CmdQ，worker process stop 独立触发，不等业务 mutex；留下 journal 供核对，不提供 fake USB Cancel。
- [ ] GUI / logs 不泄露 DeviceKey、serial 或原始 backend message；journal 路径仅用于本地人工定位。
- [ ] 只有后续父代理实际真机、权限、license 等验收后才扩大支持声明；此 slice 不代表正式发行。
