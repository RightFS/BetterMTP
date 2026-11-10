# GPUI 原生启动壳 PoC

## 结论边界

这是去 Electron / Node 的 Rust 原生 UI **启动壳**，不是已完成的 MTP 客户端，也不是 GPUI 选型验收通过。

- 已核查发布版本与示例 API，并编写双面板启动源码。
- 源码实现时仅核查 API；集成阶段已执行两次 `cargo check -p app-gpui --locked`，均因 Metal 编译环境缺失退出 101。尚未启动窗口或取得交互证据，详见 [实际验证记录](validation.md)。
- 本地文件、设备发现、目录枚举、传输、进度、输入框和拖放均未接入；不会展示虚构文件、假设备或假传输成功。
- 中文文字能显示不代表中文 IME 可用；空面板能显示不代表 100,000 行列表性能已达标。

## 固定依赖与 API 依据

[crate 清单](../crates/app-gpui/Cargo.toml) 使用 `gpui = "=0.2.2"`，edition 2024，保留该版本默认 features。无 git / main 分支依赖，无 Node / Electron / WebView，无其他业务 crate 依赖。

已读取的发布资料：

1. [crates.io 0.2.2 元数据](https://crates.io/api/v1/crates/gpui/0.2.2)：版本存在，`yanked: false`，edition 2024。该接口的发布日期与 docs.rs 页眉不一致，因此不从页眉推导发布日期；版本存在性以 registry 元数据为准。
2. [0.2.2 发布 Cargo 清单](https://docs.rs/crate/gpui/0.2.2/source/Cargo.toml)：默认 features 为 `font-kit`、`wayland`、`x11`、`windows-manifest`，保留发布版默认配置，不自行猜测替代 feature。
3. [0.2.2 hello_world](https://docs.rs/crate/gpui/0.2.2/source/examples/hello_world.rs)：`Application::new().run`、`App::open_window`、`Context::new`、`Render` 和 `div` 布局。
4. [0.2.2 set_menus](https://docs.rs/crate/gpui/0.2.2/source/examples/set_menus.rs)：`actions!`、`App::on_action`、菜单和 `App::quit`。
5. [0.2.2 on_window_close_quit](https://docs.rs/crate/gpui/0.2.2/source/examples/on_window_close_quit.rs)：`KeyBinding::new`、`FocusHandle`、`track_focus`、末窗关闭回调与 `.detach()`。
6. [0.2.2 WindowOptions](https://docs.rs/gpui/0.2.2/gpui/struct.WindowOptions.html)、[TitlebarOptions](https://docs.rs/gpui/0.2.2/gpui/struct.TitlebarOptions.html)、[Styled](https://docs.rs/gpui/0.2.2/gpui/trait.Styled.html)：窗口标题、最小尺寸和布局/颜色配置。

这些资料来自固定的已发布版本，不照抄当前 Zed 主分支。GPUI 本身仍为 pre-1.0，后续升级需要重新核查接口与构建环境。

## 源码范围

[启动源码](../crates/app-gpui/src/main.rs) 仅包含：

- 居中的 1100 × 700 原生窗口，配置 800 × 500 最小尺寸。
- 左 Local / 本地、右 Remote / 设备两块空占位，名称/大小表头。
- 深色**应用内**标题条和状态条；保留系统窗口装饰及系统标题栏，不声称已经定制系统深色标题栏。
- 明确状态：本地目录尚未读取，设备尚未接入，未执行传输。
- `Quit` action、`cmd-q` 绑定、原生菜单退出；关闭最后一个窗口时退出。
- 窗口创建失败时打印错误并请求退出，不伪造成功窗口。

Root workspace 的 members / default-members 由集成方维护：加入 `app-gpui`，但从 default-members 排除，使核心测试不必构建 GPUI。仅排除 default-members **不会**保证 workspace 依赖解析或 `cargo fetch` 避开 GPUI；锁文件由集成方统一生成，不与核心工作并发执行 Cargo。

## 构建与启动步骤（待执行）

从项目根目录执行以下命令，由同一集成方串行操作；这里是操作说明，不是成功记录：

```sh
rustc --version
cargo fetch
cargo check -p app-gpui
cargo build -p app-gpui
cargo run -p app-gpui
```

需要桌面会话与可用 GPU。根据 [GPUI 0.2.2 官方说明](https://docs.rs/gpui/0.2.2/gpui/)，macOS Metal 构建需要完整 Xcode、macOS 组件、命令行工具，以及正确的 active developer directory。仅安装 Command Line Tools 不保证 Metal shader 编译器可用。

检查 `xcode-select -p` 和 `xcrun --find metal`，记录真实输出。不要自动更改系统 Xcode 选择、安装工具或绕过 shader 构建；如缺少环境，应报告具体缺失条件。这里未执行这些检查。其他平台也需要对应 GPU / 窗口系统依赖，不能由 macOS 原型推断可用。

## PoC 验收清单

### A. 版本/API 与代码

- [x] registry 确认 0.2.2 发布且未 yank。
- [x] 对照同版本官方发布示例核查启动、菜单、退出与焦点 API。
- [x] 编写 Local / Remote 空占位与真实能力边界提示。
- [x] 不引入 Electron、Node、假设备或假传输。
- [ ] `cargo check -p app-gpui` 成功并记录 toolchain、目标平台、退出码。
- [ ] `cargo build -p app-gpui` 成功，确认实际链接与 Metal shader 构建。

### B. 原生窗口与基本行为（真实桌面验证）

- [ ] 启动时两个面板可见，中文显示无缺字，空状态说明不被截断。
- [ ] 调整至最小窗口尺寸、最大化、Retina/非 Retina 和多屏切换；布局与状态栏无溢出。
- [ ] 窗口聚焦时 Cmd+Q 与菜单退出正常；关闭末窗后进程退出。
- [ ] 重复启动/退出，无悬挂后台进程。记录截图、环境和问题。

### C. 中文 IME（**尚未实现输入控件，未完成**）

先依据固定版 [input 示例](https://docs.rs/crate/gpui/0.2.2/source/examples/input.rs) 接入真实输入控件，再测试：

- [ ] macOS 简体拼音实际 composition/marked text、候选词窗口定位、选词与提交。
- [ ] 组合输入期间方向键、退格、Esc、Enter；组合取消不误执行应用命令。
- [ ] 中文/英文切换、全角标点、emoji、代理对字符的 UTF-16 selection 行为。
- [ ] 选区替换、复制粘贴、失焦后重新聚焦、缩放和多屏下的候选框位置。

不能把静态中文 label、模拟按键或 ASCII 输入标为真实中文 IME 验收通过。

### D. Finder 外部拖入（**未实现，未完成**）

- [ ] 使用真实 Finder 拖入文件/目录，验证系统外部路径事件，而非仅应用内拖拽。
- [ ] 多选、中文/空格/emoji 文件名、目录、符号链接、不可读路径、重复路径。
- [ ] 未接入设备时只解释不可传输，不自动创建传输任务或显示成功。
- [ ] 真实设备与传输接入后另验目标目录、冲突策略、取消、错误和断连；不以接收路径事件代替传输成功。

### E. 100,000 行列表（**未实现，未完成**）

- [ ] 接入发布版 `uniform_list` 或 `list` 的虚拟化列表；只渲染可见行，不在每帧构建全部 100,000 行元素。
- [ ] 使用明确标记的合成 100,000 条数据测试 UI，不把合成数据伪装成真实设备目录。
- [ ] 测量首次显示、快速滚动、跳首尾、选区、排序/过滤后的帧时间、内存峰值和输入响应；记录机器、构建模式与数据分布。
- [ ] 将性能目标事先约定并提供采样结果，例如 60 Hz 环境下帧预算约 16.7 ms；不得在无测量时声称达标。
- [ ] 后续真实目录枚举采用后台任务/增量状态更新；与 synthetic 列表测试分别记录。

### F. 真实业务接入（不属于本启动壳）

- [ ] 本地目录浏览及错误/权限状态。
- [ ] MTP 发现、连接、设备/存储列表、断连状态。
- [ ] 真实读写、传输取消/失败、进度与校验，事件驱动 UI，不伪造能力。

## 验证记录模板

每次真实验证追加：toolchain / OS / GPU / Xcode / build mode / 命令 / 退出码 / 测量或截图 / 复现步骤 / 未通过原因。失败与环境阻碍应保留，不能把“源码已写”替换为“已编译运行”。

当前记录：固定发布版 API 已核查，源码已写；最初直接调用 rustfmt 因 PATH 缺失返回 127，集成方随后使用已安装 Rust 1.99.1 成功完成 `cargo fmt --all`。两次 GPUI check 均退出 101（首次找不到 metal，局部选择完整 Xcode 后确认缺少 Metal Toolchain 组件），未跳过 shader 或启动窗口。中文 IME、Finder 拖入与 100,000 行列表均未实现/未验收。环境和命令结果见 [验证记录](validation.md)。
