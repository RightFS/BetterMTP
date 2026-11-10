# GPUI safe-MVP 可用性审计（静态建议）

## 范围、证据与限制

本审计仅阅读源码和已有文档；没有修改 app 文件，没有运行 Cargo、GUI、USB、worker、真机命令或 Git commit，也没有访问设备。UI 实施由另一代理负责；以下行号是审计读取时的快照，后续实现可能移动。

- 核查：[主界面](<../crates/app-gpui/src/main.rs>)、[本地状态](<../crates/app-gpui/src/local.rs>)、[远端状态](<../crates/app-gpui/src/remote.rs>)、[传输状态](<../crates/app-gpui/src/transfers.rs>)。
- 交互约定：[本地 slice](<ui-local-slice.md#L11-L26>)、[单文件 UI](<ui-file-transfers.md#L30-L57>)、[Review 恢复](<ui-file-transfers.md#L79-L113>)、[服务边界](<file-transfer-service.md#L5-L50>)。
- [验证记录](<validation.md#L90-L100>)记载既有 **24 UI tests**，不等于本轮重新验证或真实键鼠/视觉验收；固定 GPUI 0.2.2 / Rust 1.99.1。本报告不把源码推断写成已复现视觉 bug。

**总体判断：安全状态机已有坚实基础，最急需补的是“用户能看懂为何不可操作、到底传到哪里、结束后证据仍可找到”，不是增加危险功能。** 保留真实 HELLO upload/download gates、单个普通文件、不可变 plan、独立 task ID、busy/review handler 屏障、脱敏错误。不得引入递归目录、Replace、remote delete/rename、批量、drag/drop 或 USB Cancel；没有真实进度事件时只显示处理中，不画虚构百分比。

## 高优先级行动（8 项）

### 1. P0：传输结果不应被无效点击覆盖；保留近期记录

**位置/问题：** [灰传输按钮](<../crates/app-gpui/src/main.rs#L660-L710>)仍绑定点击，预检失败进入 [start handlers](<../crates/app-gpui/src/main.rs#L596-L620>) → [reject](<../crates/app-gpui/src/transfers.rs#L179-L187>)，替换 terminal 且清 last_target。上一条成功的 actual name/path/journal 会从 UI 消失；[begin](<../crates/app-gpui/src/transfers.rs#L140-L155>)也在开始下一项时清掉上项。当前并非“无记录”，而是仅保留最近单条且可被非任务覆盖。

**建议：** 预检提示使用独立字段，不改已有 terminal。新任务前把完成/Skip/失败/Review 保存为有界的“本次窗口近期记录”（例如最近 10 项）；只存方向、显示源/目标快照、policy、actual name/path、bytes、终态、journal，不保存可执行 RemoteRef/DirectoryRef。先做到本次会话保留即可，不伪装已有重启恢复；不要自动扫描历史 stage/journal 或清理证据。

**验收：** 完成后点击能力禁用按钮、刷新、断开、人工 ack、再开始一项，上一条证据仍可看；Skip 不是完成；不要把 UI 预检失败伪装成已 dispatch 的任务。完成卡可提示“目录不会自动刷新；在对应面板显式刷新”，不自动改变当前目录。

### 2. P0：点击前就明确源、目标与冻结规则

**位置/问题：** [upload/download plan](<../crates/app-gpui/src/main.rs#L534-L594>)捕获当前 remote/local 目录，下载目标不是本地选中条目；目前 [target_summary](<../crates/app-gpui/src/transfers.rs#L227-L232>)只有 pending 或 last_target，点击前没有明确配对预览。[目标说明](<../crates/app-gpui/src/transfers.rs#L50-L67>)下载仅显示源文件名，不显示源所在存储/目录；两边都可能同名。

**建议：** 按钮旁提供纯展示的方向预览：“本地源文件 → 当前设备/存储/目录”和“设备/存储/目录/源文件 → 本地当前目录”，明确“选中目录不会成为下载目标，先进入该目录”。开始后将预览换成任务捕获快照，并常驻“目标已固定；本地继续浏览不会更改本任务”。下载可额外快照 source address label，用于展示，不据此重建 remote reference。

**验收：** 两个方向同时可用时不会混淆；下载启动后切换本地目录，仍显示原目标与最终 actual path；最终名称只依 receipt，不提前承诺原名或内容 hash。

### 3. P0：RequiresReview 做成独立安全告警，人工 ack 不是成功

**位置/问题：** [Review 按钮](<../crates/app-gpui/src/main.rs#L740-L795>)已有人工确认与 journal 提示；[summary](<../crates/app-gpui/src/transfers.rs#L200-L224>)由 terminal 的 requires_review 标记显示“不要重试”，但 [ack](<../crates/app-gpui/src/transfers.rs#L190-L198>)只清 barrier。ack 后旧 terminal 仍是未知结果，界面可能同时允许连接并继续说“不要重试”，缺少“已人工确认恢复连接资格”的层次。

**建议：** 告警优先于“已连接/未连接”状态：明确“结果未知，远端请求及新传输已锁定；文件可能已经创建，不要重试/删除”。展示捕获目标、完整 journal 定位和简短核对清单。ack 必须明确“仅解除锁定，原任务仍未知，不重试、不删除、不自动连接”，可用二次说明确认防止误点。ack 后显示“已人工确认；原任务仍待核对记录，需手动重新连接”，保留原始 outcome 和证据；不得把未知结果改成成功。

**验收：** busy/review 下鼠标、快捷键、breadcrumb 和 storage 切换均不能绕过现有 guard；缺 journal 时明确“位置未回传”并显示捕获目录/缓存提示，不把缺证据解释为无副作用。

### 4. P1：禁用应有可见原因，能力按方向表达；冲突策略用人话

**位置/问题：** [plan gates](<../crates/app-gpui/src/main.rs#L534-L594>)有具体 Err，但 [transfer_panel](<../crates/app-gpui/src/main.rs#L660-L738>)先丢为 bool，用户只见 opacity；[remote 状态](<../crates/app-gpui/src/main.rs#L815-L827>)用 upload OR download 显示“单文件能力已确认”，单方向连接容易被理解为双向。KeepBoth/Skip 为英文点击轮换，不说明作用范围。

**建议：** 直接复用 plan Err 作为按钮下方/详情的禁用原因，不另建较宽松能力推断。方向 badge 分别显示“上传可用/未验证”“下载可用/未验证”。冲突控件明确仅下载：“保留两份（KeepBoth）：空闲用原名，冲突用安全随机新名”“已有目标则跳过（Skip）：不覆盖，不算完成”；上传策略固定，说明总是创建带 crypto suffix 的新对象，以 receipt actual name 为准。真实 disabled 交互仍保留 handler 二次 guard。

**验收：** 未连接、legacy、仅 upload、仅 download、目录/链接源、metadata error、HOME/cache 缺失、busy/review 都有准确解释；切 policy 不影响已捕获任务；绝不出现 Replace/Cancel 菜单。

### 5. P1：键盘能完成选择与目录导航，不把 Enter 变成传输

**位置/问题：** [bindings](<../crates/app-gpui/src/main.rs#L1183-L1192>)只有 Tab、⌘R、⌘B、⌘Q；[本地选择](<../crates/app-gpui/src/main.rs#L154-L173>)与[远端选择](<../crates/app-gpui/src/main.rs#L504-L532>)仅鼠标。既有 [焦点测试](<../crates/app-gpui/src/main.rs#L1414-L1442>)只覆盖两面板，不代表按钮可键盘操作。

**建议：** 在各 Pane key context 加 Up/Down 单行选择与滚入可见范围，Enter 只进入真正目录（文件/链接绝不打开或传输），Esc 清选择但不取消任务。传输仍需明确可聚焦按钮后 Space/Enter 激活，或本轮保持明确鼠标按钮而不宣传全键盘可用。若增加工具按钮 tab_stop，Tab 文案与 NextPane 必须区分“控件遍历”和“左右面板切换”，避免新控件破坏已有循环。

**验收：** loading/empty 时方向键无越界；刷新清选择；滚动后选中行可见；快捷键只作用焦点面板；busy/review 下 remote 导航键无效且原因可见；普通文件 Enter 不启动 IO transfer。

### 6. P1：active pane、选中项、breadcrumb 三者分清

**位置/问题：** [本地焦点边框/地址](<../crates/app-gpui/src/main.rs#L175-L242>)、[远端边框/地址](<../crates/app-gpui/src/main.rs#L855-L897>)主要用颜色和省略路径；[行 hover](<../crates/app-gpui/src/main.rs#L330-L352>)及[远端 hover](<../crates/app-gpui/src/main.rs#L1041-L1061>)覆盖 selected 背景。长路径没有可读完整入口；地址是只读标签。

**建议：** 由现有 FocusHandle 派生“当前键盘面板”文字/图标，不额外维护易漂移 active enum；与行“已选择”标记、存储 active 各自区分。hover 不覆盖选中反馈。breadcrumb 本地持有原生 PathBuf，远端按 locations 中真实 directory ref/栈位置导航；点击祖先仍走相同 guards，不从文字拼路径。完整路径可以展开或复制展示，但复制的 lossy 文本不成为操作 authority；本轮不必增加可编辑地址框/IME。

**验收：** 两侧各有选中项时也能看出焦点、传输方向和当前存储；长/中文/emoji 名称不只靠颜色；breadcrumb 在 busy/review 不能改远端 epoch/栈。

### 7. P1：loading、空目录、配置错、连接错给不同恢复出口

**位置/问题：** [local 消息](<../crates/app-gpui/src/main.rs#L302-L320>)已有三态；[remote 消息](<../crates/app-gpui/src/main.rs#L1008-L1030>)把 loading、错误、无存储、空目录、未连接都用同一警告色。[remote error](<../crates/app-gpui/src/remote.rs#L280-L302>)会清全部 refs，因此目录失败后的“刷新/上级”不能恢复，需要重连；[local begin](<../crates/app-gpui/src/local.rs#L32-L49>)有意清旧快照，不能为美化 loading 把旧条目重新变成可操作项。

**建议：** loading 用中性处理中（连接/列目录等已有真实请求类型，不编造阶段/百分比）；空目录是中性结果，本地带“显示隐藏项”入口；配置无效明确“修正参数后重启”；连接/目录错误展示脱敏分类并给手动重连入口；未配置清楚说明本地可用且不会自动访问设备。Review 必须压过普通重连建议。禁止自动 retry，也不要恢复旧 session refs 只为保住 breadcrumb。

**验收：** 不同状态不互相伪装；本地错误可刷新/上级，远端引用清除后要求重新连接；迟到结果仍遵守 revision；根目录按钮呈现不可操作原因，而非假成功。

### 8. P1：布局先保证安全状态和证据可读，再压缩装饰

**位置/问题：** [render](<../crates/app-gpui/src/main.rs#L1117-L1175>)顶部标题、可变高传输块、双面板和页脚共享高度；[最小窗口](<../crates/app-gpui/src/main.rs#L1222-L1228>)为 800×500。[目标/发布路径/journal](<../crates/app-gpui/src/main.rs#L763-L795>)都单行省略，错误正文也未提供详情展开；长名与 Review 同时出现时有可读性/列表剩余高度风险（静态推断，未启动确认）。

**建议：** 固定层次为：简洁应用/连接摘要 → 双面板及导航/选择 → 紧凑任务状态/可展开详情；或保留顶部任务区但限定普通态高度。Review 告警不能默认折叠。完整 actual path/journal 放可换行、可选择/复制或可滚动的详情，不能只剩省略号；列表继续 uniform_list/min_h_0，不被状态内容无限挤走。成功/Skip/失败/Review 的文字和色彩分层，但不以颜色替代状态。

**验收：** 明天人工验证 1100×700 和 800×500，长文件名/地址、metadata error、busy、Review 缺/有 journal 都不截掉恢复入口。此项是可视检查，不因 24 单测通过就标为完成。

## 建议实施顺序及明天验证边界

先做 1–4（证据、目标、安全解释），再做 5–8（键盘、导航、恢复、布局）。本轮允许另一个实施代理写 UI，但审计代理没有改源码。可由父代理协调离线状态测试；本审计不运行任何测试或 GUI。明天用户自行检查本地窗口交互；需要设备/传输时重新取得明确授权，不把这份审计当硬件授权。保留既有不可变 plan、busy/review、legacy/asymmetric capabilities、Skip≠Completed 回归；新增“无效预检不覆盖完成证据”“ack 不改变未知 outcome”“新导航/键盘不绕过 guards”的状态测试最有价值。
