# UI local slice — GPUI 0.2.2

## 本轮范围与状态

只修改 `crates/app-gpui`（包含其 Cargo manifest）并新增本文档；没有修改其他 crate、workspace root 或提交 Git commit。

左面板为真实、只读的本地目录浏览器；本地 baseline 已由父代理在正常 MetalToolchain 下编译通过。右面板本轮接入真实 `mtp-application::WorkerClient` 的 Kalam 只读浏览（显式连接后列存储和目录），**remote 新增代码与测试尚待父代理统一 Cargo 校验和真机验收**。未配置时明确显示尚未配置，配置后不自动连接；没有 Fake 后端入口、虚构真机文件、传输结果或删除写入功能。

本代理按父代理要求 **没有运行 Cargo**，避免锁竞争；只使用固定 `gpui-0.2.2` 官方 examples/源码核对 API，并通过已安装 stable 工具链直接格式化 scoped Rust 源码。父代理已正常完成 Metal shader 构建及本地 baseline 检查，本轮 remote 和测试结果以随后父代理统一报告为准，不提前声称通过。不跳过 shader 构建，不伪造 Metal 编译结果。

## API 与实现

- `mtp-local-fs.workspace = true`；调用现有 `read_directory(&Path, show_hidden)`，不重复实现 adapter。
- 启动前调用 `std::env::var_os("HOME")`，保留原生 `OsString`。仅接受非空绝对路径；缺失或无效时使用 `/`。环境读取不在 `Render`，无启动时同步目录枚举。
- 启动和交互事件调用 `request_directory`。枚举、元数据获取、隐藏项过滤和目录优先排序全部在 `cx.background_executor().spawn(...)` 中执行。
- GPUI `Context::spawn` 等待后台结果，使用 `WeakEntity::update` 回写 UI state 并 `notify`。关闭窗口后实体消失不会 panic。
- `LocalBrowser::begin` 用 `checked_add` 为每次请求递增 revision；回写时 `complete` 严格匹配最新 revision。迟到的成功 **和错误** 都不能覆盖当前地址、loading 或结果。计数耗尽进入终止错误态，拒绝所有新请求和待完成回写、保留最后地址并显示重启提示，不重用 revision。旧任务允许后台完成，暂不做物理取消或并发限制。
- 新请求立即清空旧条目、选择和错误，避免新地址下展示/操作旧快照；刷新和隐藏项切换也清空选择、重置滚动。
- 使用官方 `uniform_list`、`cx.processor` 和 `UniformListScrollHandle`。固定行高 32px，受面板剩余高度和 `overflow_hidden` 容器约束；仅为 GPUI 请求的视口 range 创建行元素。目录快照仍一次性保存在内存中，**不是分页枚举**。
- 文件名和地址仅用 `to_string_lossy()` 展示；选择和导航保存、传递 adapter 返回的实际 `PathBuf`。不以展示字符串重建文件操作路径。
- 单击选择；`ClickEvent::click_count() == 2` 且类型为 `Directory` 时进入目录。普通文件、符号链接、未知类型不会被打开或跟随。
- 上一级按钮 / `⌘B` 使用实际路径的 `parent()`，根目录无操作；刷新按钮 / `⌘R` 重读当前目录；隐藏项按钮切换后重读。
- 地址只读显示；loading、目录错误、空目录状态互斥显示。条目元数据错误显示 `!`，选中后底部显示错误文字。未知大小显示 `—`，零字节文件显示 `0`。
- 两个面板各自持有 GPUI `FocusHandle`，设置 tab index / tab stop。启动聚焦本地，鼠标点击聚焦对应面板，`Tab` / `Shift-Tab` 使用 `Window::focus_next/prev`。焦点面板显示蓝色边框。`⌘R` / `⌘B` 分别限定 `LocalPane` 和 `RemotePane` key context，不跨面板导航；未连接设备时 remote 快捷键不启动本地 IO。
- 保留应用菜单 Quit、`⌘Q` 和最后一个窗口关闭后退出。
- helper opaque return 使用 Rust 2024 precise `use<>` capture，避免 render 构建两个面板时意外延长窗口/context 的借用。

## 自动测试（待统一执行）

`crates/app-gpui/src/local.rs` 包含四个纯状态测试，无文件写入：

1. 旧成功结果不能清除较新请求的 loading。
2. 旧失败结果不能替换较新成功结果。
3. 新请求清空旧选择和错误。
4. `u64::MAX` 后计数不回绕，拒绝新请求且阻止待完成成功/失败覆盖耗尽态。

建议由父代理在正常 MetalToolchain 可用后统一执行 `cargo check -p app-gpui` 与 `cargo test -p app-gpui`；命令仅记录于此，本轮未执行。

## Manual 测试清单

- [ ] 启动显示 `$HOME` 下真实条目；隐藏项默认不可见；目录优先，名称排序与 adapter 一致。
- [ ] 单击普通文件出现选择高亮和实际地址；大小正确区分 `0`、已知 bytes 与 `—`。
- [ ] 双击目录进入；地址同步变化；上一级按钮和本地焦点下 `⌘B` 返回 parent；到 `/` 后无越界导航。
- [ ] `⌘R` 与刷新按钮真实重读；点击后先出现 loading，之后恢复列表或错误。
- [ ] 连续切换隐藏项、刷新和上一级，旧请求结果不将新地址对应列表或状态覆盖。极快目录 loading 可能只短暂出现。
- [ ] 显示隐藏项后可见点开头条目，再关闭后不可见。
- [ ] 双击普通文件及符号链接均不打开、不跟随、不传输；选中时类型准确。
- [ ] 大目录滚动只绘制可见 range，保持固定行高；缩小窗口后列表在剩余区域滚动，底部状态不会被无限长列表挤走。
- [ ] 鼠标点击左右面板、`Tab` / `Shift-Tab` 可切换焦点，蓝色边框跟随；右面板 `⌘R` / `⌘B` 仅刷新/返回远端，未连接时无操作且不触发本地枚举。
- [ ] 使用已存在的权限不足目录（或在浏览期间由外部移除目录）确认错误不被伪装成空目录；返回 parent / 刷新可恢复。GUI 本身不创建或删除测试文件。
- [ ] 现有包含中文、emoji、长名的目录可浏览；长名称/地址省略而非扩撑面板。非法 UTF-8 名称的真实文件系统验收限支持该名称的系统（APFS 不支持）；adapter 已有 Linux 覆盖。
- [ ] 验证选中 metadata-error 条目显示 `!` 和错误详情（若现有环境可提供此条目）。
- [ ] 枚举未结束时关闭窗口，安全退出；菜单 Quit 与 `⌘Q` 正常。
- [ ] 未配置/未连接/断开时状态真实，连接后仅展示实际返回目录；没有任何 Fake 设备数据、写操作、删除或传输入口。

## Remote 只读 slice

### 已实现 API / 生命周期

- App manifest 使用 workspace `mtp-application`、`mtp-domain`、`mtp-backend-api`，没有改这些 crate 或 root。
- startup（非 Render）解析 `--library ABSOLUTE_PATH`，优先于 `MTP_KALAM_LIBRARY`。重复参数、未知参数、缺失值或相对路径报明确配置错误；无配置清楚显示尚未配置，不启 worker，也不切到 Fake。
- worker 路径由 startup `current_exe()` 的同目录 `mtp-worker` 得到。动态库存在性、worker 启动和握手均在用户点击连接后的后台任务验证。
- 连接调用 `WorkerClient::spawn(..., &BackendConfig::Kalam { library_path })` → `connect()` → `storages()` → 第一存储 `SessionInfo::root(StorageId)` → `list()`。浏览能力不支持时明确错误；连接但无存储时明确空存储状态。
- `Arc<SharedWorker>` 内 `Mutex<Option<WorkerClient>>` 串行 worker 方法，UI/Render 不拿锁。`AtomicU64 desired` 是所有后台任务共享的请求 epoch；每任务拿锁后、连接各阻塞阶段之间和回写前检查，跳过被新请求超越的 connect/list/disconnect，避免非 FIFO 调度让过期断连破坏新 client。
- UI revision 也严格匹配最新请求。`checked_add` 耗尽时不重用编号，清所有引用、进入终止错误态；atomic 设保留值 0，让未结束请求也失效。
- 断开/重连 begin 同步清 `SessionInfo`、storage、所有导航引用、条目和选择；后台 disconnect 可以等待当前阻塞调用，但迟到结果不能恢复旧引用。任何 worker 操作错误保守清连接和引用，必须重连，不把超时视为成功。
- WorkerClient 的 Drop 会 kill/wait，正常任务中的替换/失败 drop 都在后台。最后共享句柄释放时 `SharedWorker::drop` 将仍存活 client 交给清理线程，避免窗口关闭时在 UI 上阻塞。
- 存储按钮切换该存储 root；双击目录使用 `RemoteDirectoryRef::from(entry.reference.clone())`，选中保留 `RemoteRef`。导航栈保留真实引用，上一级 pop，根目录回存储 root；可见地址由标签组成但从不用于对象操作。
- remote 也使用 32px `uniform_list` 视口列表。连接/断开、刷新、上级、根目录和存储按钮已接真实 API；普通文件仅选中。未提供上传/下载按钮，不调用未验证写接口。
- 顶部仅展示 `SessionInfo.display_name`，不打印或显示 DeviceKey、序列号、整个 session/reference 的 Debug。原始 backend error message 可能含标识符，UI 只展示脱敏分类/错误码。

### CLI 使用（命令仅记录，未由本代理执行）

父代理统一构建 app 与 worker，确保两个可执行文件同目录；按需选择相同 debug 或 release profile。

```sh
cargo build -p app-gpui -p mtp-worker
# 显式参数优先；将此路径替换为实际 Kalam 动态库绝对路径。
./target/debug/app-gpui --library /absolute/path/to/libkalam.dylib
# 或通过环境变量配置：
MTP_KALAM_LIBRARY=/absolute/path/to/libkalam.dylib ./target/debug/app-gpui
# 不配置也可安全运行本地浏览：
./target/debug/app-gpui
```

配置后仍需点击「连接」。没有 fake 模式 CLI；如使用自定义 Cargo target 目录，以父代理实际 artifact 路径为准。

### 新测试与待验项

保留 local 四个状态测试；新增 remote 五个测试及 GPUI 一项焦点测试（总计十项，等待统一执行）：

1. CLI 显式路径优先、相对路径和缺少参数拒绝、无配置不回退 Fake。
2. 实际填充 session/storage/root/entry/selection 测试 fixture 后断开，立即清全部引用；迟到 connect 成功、list 成功和错误都被拒。
3. 共享 Arc epoch：让旧 connect 阻塞于锁，发布较新 disconnect 后放锁，旧任务必须跳过而非启动 worker。
4. 原始 backend message 中测试标识符/serial 文本不进入显示错误。
5. remote revision 耗尽不回绕，拒绝迟到结果。
6. GPUI `test-support` 的真实窗口测试：初始本地焦点、Tab/Shift-Tab 和循环；remote 快捷键不改变本地地址或触发本地 loading。使用 state-only constructor，不枚举文件系统、不启动 worker。dev-dependency 增加 GPUI `test-support`，需要父代理统一解析其测试依赖。

Manual：

- [ ] 无配置启动：本地仍可用、设备显示尚未配置、连接按钮不可用；相对/重复/缺值 CLI 显示配置错误。
- [ ] 正确动态库与同目录 worker：点击连接出现 loading，显示 actual `display_name`、storage 和第一 root；没有设备时真实错误，不能 fake 成功。
- [ ] 切换 storage、双击目录、刷新、上级和根目录保持实际引用；普通文件只读选中，未知 bytes 显示 `—`。
- [ ] remote 大目录虚拟滚动；Tab 和点击的焦点边框正确，快捷键仅作用当前面板。
- [ ] connect/list 处理中断开：引用立即消失，迟到结果不恢复；重连不沿用旧 generation/object refs。
- [ ] 连续切存储/刷新/上级时旧结果不覆盖新地址；拔设备、worker 异常退出/超时后清所有 refs，显示错误并可重连。
- [ ] 已连接时关闭窗口/Quit 无 UI kill/wait 阻塞；不泄露 devicekey/serial 到界面或日志。
- [ ] UI 无上传/下载/覆盖/删除可用入口；安全传输 API 尚待父代理提供。

## 刻意未实现

可编辑地址栏 / 完整 IME 文本输入、键盘行选择 / Enter 导航、本地历史、持久化 UI 偏好、文件预览、多选、watcher、分页、后台阻塞操作硬取消、多设备选择与上传下载。remote 真实可用性仍需正常编译和真机验收，不声称完整真机 MVP 已完成。
