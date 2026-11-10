# UI 与交互收口：仅本地预览、快照身份与任务证据

## 本轮范围与证据边界

这是单文件传输切片后的 UI/交互完善，不是手机验收或 MVP 完成证明。当前用户明确暂停实机验证：本轮没有打开实际 GUI、连接手机、初始化 Kalam、启动真实 MTP worker、读取 UID 或进行 USB/MTP 调用。测试仅使用纯状态模型与 GPUI 受控测试窗口；其中连接、对象和回执是显式测试数据，不是生产运行时的“成功假设备”回退。

本轮只修改 app-gpui 源码并新增本说明；不调整服务层、worker、native backend、依赖清单或锁文件。独立静态审阅依据见 [可用性审阅](ui-usability-review.md)。源码和测试已落盘；操作系统窗口视觉、真实鼠标/键盘、文本长行及手机行为仍需分别验证，不能从纯测试推断已通过。

## 安全的 UI 预览入口

编译完成后，以明确参数启动，而不是直接打开可能携带设备配置的旧进程：

```bash
./target/debug/app-gpui --local-only
```

`--local-only` 是强优先模式：即使同时给出 `--library`、环境中的 library、bundle 资源或损坏/缺值的 library 参数，也选择仅本地模式，忽略其余设备配置；不探测 bundle library。界面提示“--local-only 优先 · 不启动 worker / 不访问 USB”。

防线分别位于配置解析、RemoteBrowser.begin、远端请求分发、所有远端交互入口、上传/下载计划与提交入口。受控测试即使人为提供 session/capability/ref，也不能在这个模式中发布新远端 epoch、导航远端或提交传输。不能通过 UI 解除该模式以连接手机。

本地文件系统浏览仍是真实、只读的本机访问，不是假目录。下一次手机验证需要新的明确授权，并重新启动正常配置的进程；本轮不做重连接、UID 探测或“试一下”的隐式设备动作。

## 双面板键盘与焦点

| 操作 | 行为 |
| --- | --- |
| Tab / Shift-Tab | 在既有两面板 tab-stop 中循环，关闭已打开的上下文菜单 |
| ↑ / ↓ | 按当前快照的原生身份选择上一/下一行，并滚动至可见；不循环越界 |
| Home / End | 选择首/末行；空列表、加载态不会产生索引越界或恢复旧条目 |
| Enter | **只进入真实目录类型**；普通文件、链接与未知类型不打开、不传输 |
| Backspace / Alt-↑ / ⌘B | 对焦点所在面板执行上级导航，仍受原有请求/安全门控 |
| ⌘R | 刷新焦点面板；远端忙或 RequiresReview 时无远端状态变更 |
| Escape | 优先关闭菜单，否则清除当前面板选择；不是任务 Cancel，也不会断开连接 |

活动面板标题和边框使用真实 FocusHandle 状态；选择行另有标记，hover 不覆盖选择反馈。工具栏按钮没有新增完整键盘 tab-stop，因此这里不是“所有控件完全键盘可访问”的声明。

## 路径、行事件、面包屑与存储身份

- 本地操作保留 PathBuf/OsString；有非 UTF-8 字节时使用替代字符展示，但禁止把 lossy 展示文字用作复制的“原始路径”或上传名字。列表/选择提示明确标识非 UTF-8。目录原生路径仍可用于只读导航。
- 本地面包屑由原生 Path components 累积生成，每个按钮捕获实际 PathBuf 与快照 revision；显示标签仅用于 UI。
- 远端目录面包屑保留 DirectoryRef，事件捕获位置及 render epoch；执行前验证 epoch、loading 和安全门控，之后才截断当前目录栈。不会从展示地址恢复 refs。
- 虚拟行事件捕获本地 path+revision 或 RemoteRef+epoch，而不是执行时查找“当前 index 指向的条目”。
- 存储按钮捕获 **StorageId+render epoch**，先拒绝远端锁定、loading 与旧 epoch，再从当前 session 的当前 storage 集合按 ID 查 root。列表同 epoch 换位不改目标；重连后即使 ID 相同也拒绝旧 callback。ElementId 的 index 只是渲染位置，不是操作权威。

## 右键菜单：一次打开一次 ticket

菜单只提供已有实现：进入目录、普通单文件上传/下载、无损 UTF-8 名称/本地路径复制。目录传输、链接及不安全来源的传输保持禁用并说明原因。没有远端改名、删除、替换、目录递归、拖拽或批量任务。

每次打开生成 checked_add 的单次 token，与原生 ContextIdentity（含 revision/epoch）组成 ContextTicket。copy、activate、外侧 dismiss 的每个 callback 都捕获自己的 ticket，先比对当前菜单，再校验 **expected** 原生身份；只有通过后才清空/执行。

因此旧菜单 A 的 callback 在新菜单 B 打开后：不复制 B、不传输 B、不导航 B，也不关闭 B。即使原生身份与 epoch 完全相同，重新打开的菜单也有不同 token，旧 callback 仍被拒绝。菜单计数器不 wrap 或复用。Escape 是用户当前菜单的通用关闭操作，不是一个旧菜单 A 的动作。

菜单限制到 viewport 内，具有外侧点击关闭和垂直滚动。具体小窗位置、字体、滚动感受还需 OS 窗口验证。

## 上传/下载能力与点击前预览

两个方向独立显示 HELLO 能力与当前不可用原因；不把“某方向未授权”说成整个连接只能浏览。灰色上传/下载按钮与菜单动作在不可用时 **没有 on_click handler**，同时执行入口仍重新校验，防止旧可用帧或直接调用。

- **上传**：本地选中的单个普通文件 → 当前设备存储/目录。最终对象是新文件，采用安全随机后缀；最终名与字节数以真实回执为准，不承诺对原文件 rename 或覆盖。
- **下载**：设备选中的普通文件 → 本地 **当前目录**。选中一个文件夹不会把它自动变为目标；要下载到它，先进入它。
- KeepBoth：本地空闲时原名，冲突时安全新名，保留两份。
- Skip：已有则跳过、不覆盖；Skipped 是中性结果，**不是 Completed**。

紧凑区显示下次明确点击的源/目标预览，展开后可查看完整文字与原因。已开始任务保留独立、不可变的捕获计划；继续本地浏览不会改变它。下载捕获信息包含当时的设备存储/目录展示上下文，但该文字不是操作路径或远端 ref。

## 底部任务与证据

任务区位于双面板之下，默认紧凑。详情可展开、垂直滚动；先显示当前任务捕获目标、完整结果名/字节数、实际下载路径与 journal，再显示下一次预览和近期记录。处理或待核对时收去不可用的新传输操作条，让证据保持优先并为小窗列表保留空间；详情可视高度约束为 100 px，**不是所有证据已同时可见**的承诺。

关键状态模型分开：

1. preflight_notice 只描述未提交的新交互，例如目录上传不支持或 capability 不足；不会覆盖旧 terminal、目标、回执、实际文件路径或 journal。
2. 接受下一条新任务后，才把上一条 terminal+捕获展示信息归档到本窗口最多 8 项近期记录；拒绝、busy/review 和 task-id 耗尽不先删证据。
3. 近期记录只包含不可执行的展示信息/回执路径，没有可再次执行的 RemoteRef；不扫描磁盘、不恢复任务、不删除或清理 journal。超过 8 项仅丢弃内存 UI 引用，磁盘证据不受影响。
4. Journal 原始路径只有能无损表达为 UTF-8 时才能复制；复制只是剪贴板文字，不读取/打开 journal，也不把它用于重试/清理权限。
5. 不展示虚构百分比；无真实进度事件时只显示真实任务阶段。任务后不自动刷新目录，需要明确点击对应面板刷新。

## RequiresReview 与唯一人工解除路径

RequiresReview 在全局标题与任务区具有优先提示，详情强制可见。告知“文件可能已创建、结果未知”，禁止新传输及远端连接/断开/刷新/导航/选择等请求；本地只读浏览可以继续。

唯一主动解除按钮是“我已核对，解除锁定后手动重连”。它只解除 barrier、丢弃旧可操作远端 refs，要求之后手动连接；不会重试、删除 journal、自动连接或把未知原任务变成成功。原 Failed/Unknown 的结果与捕获证据仍可查看。Journal 没回传时也不能断言没有发生写入。

## 验证与后续检查

最终实际结果：40 个受控 app tests 通过；`cargo clippy -p app-gpui --all-targets -- -D warnings` 通过；`cargo build -p app-gpui` 通过。三项均使用 offline/locked、stable 工具链，组合命令最终 exit 0。strict clippy 首轮的 3 个样式/测试比较 lint 已修复，并重新执行全部三项。第三方 block/proc-macro-error2 的 future-incompat 提示仍存在，不是 app lint 失败；本轮未改变其依赖。

覆盖包括：bounded selection、原生字节面包屑、Enter 普通文件不传输、busy 导航/键盘/旧菜单身份拒绝、完成后非法点击保持证据、task-id 耗尽保持证据、归档上条回执、近期上限、人工确认仍未知、方向能力不对称、选中文件夹不改变下载目标、local-only 多层零 dispatch、菜单 A→B 与相同身份重开 token、存储 reorder 与重连旧 epoch。

最终只读检查命令：

```bash
cargo test --offline --locked -p app-gpui
cargo clippy --offline --locked -p app-gpui --all-targets -- -D warnings
cargo build --offline --locked -p app-gpui
```

### 父会话后续的仅本地 OS 检查（本轮未执行）

- 构建后的二进制明确以 `--local-only` 开窗；顶栏、远端空态和按钮应持续说明不访问设备。
- 在 800×500 与 1100×700 检查标题/恢复工具是否可见、列表是否仍能滚动、底部详情不压没全部本地行。
- 选择/hover/focus 应有不同反馈；实际键盘循环、Enter 文件无动作、Escape 不取消任务。
- 长名称、长路径与非 UTF-8 本地名称的警告、无损复制禁用；详情完整文字的换行/滚动需要实际检查。
- 右键边缘 clamp、外侧关闭、同一条目菜单重开；鼠标顺序行为仍不由受控 tests 代替。
- Loading/Error/Empty/未配置等真实 OS 显示，以及 RequiresReview 的高度/强调需要单独视觉检查；纯测试仅证明状态/门控，不证明实际像素或焦点系统。

### 手机验证（等待新的明确授权）

本轮不执行任何一项。授权后独立验证连接/HELLO、真实目录/存储导航、单文件 upload/download、KeepBoth/Skip 与 journal/实际文件核对，并复核长任务阶段的锁定行为。旧 UnknownOutcome 上传不得为了“复现 UI”而再执行；查验与新任务授权是两件事。
