# 初始化开发验证记录

日期：2026-11-10。范围：原生工程第一批代码，不是完整 MTP 产品验收。

## 环境

- 主机架构：arm64 / macOS。
- `rustc 1.99.1 (58cf8f9c3 2026-10-13)`，`cargo 1.99.1 (5f94df478 2026-08-27)`。
- 已安装工具链别名为 stable；本轮以 `RUSTUP_TOOLCHAIN=stable` 使用上述实际固定版本，没有安装/修改全局 Rust 工具链。
- Cargo 下载和编译缓存位于本工程忽略目录，未安装 Node 依赖。
- active developer directory 为 CommandLineTools；完整 Xcode 27.2 已存在。

## 核心验证通过

执行：

```bash
cargo fmt --all
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
cargo build --locked -p mtp-worker -p mtp-probe
target/debug/mtp-probe fake
target/debug/mtp-probe local crates
```

上述命令最终均退出 0。默认包不包含 GPUI，核心通过不能用于宣称 UI 构建通过。

| 测试层 | 通过数量 | 重点 |
| --- | ---: | --- |
| domain | 62 | 会话/根目录/filename/overflow/快照/取消竞态/未知结果阻断 |
| backend-api | 12 | 分帧/超限/截断/JSON/错版本/重放/u64 |
| local-fs | 5 | 目录优先/中文名/隐藏/缺失/零字节/不跟随链接 |
| worker 单元 | 8 | 单owner/重连/错误/代数溢出/失效/唯一terminal |
| worker 子进程 | 8 | 真正spawn/协议拒绝/kill隔离/未知结果/正常退出 |
| 合计 | **95** | 无忽略测试 |

fake probe 成功完成握手、storage、Unicode/>4GiB **元数据**、重连、旧代数拒绝、review-required 错误和有界退出；没有读取 USB、没有传输任何文件。local probe 实际列出工程 crates 目录。

### 首轮失败及修复

首次测试退出 101：试图在 macOS 文件系统中创建无效 UTF-8 文件名返回 `Operation not permitted`。这不是浏览 adapter 失败，也没有重试到其他路径绕过限制。把实际无效 UTF-8 文件名创建测试限定为支持该行为的 Linux 环境；macOS 保留中文/原始 OsString 和 symlink 覆盖。Linux 条件测试尚未在本机执行。

## 无 Node 工具链验证

额外把 PATH 限定为 `$HOME/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin`，逐一确认 `node`、`npm`、`yarn`、`zx`、`electron` 均不可达；设置全新的工程内 `CARGO_TARGET_DIR=.cache/no-node-target`，执行 `bash -n scripts/check.sh` 与 `bash scripts/check.sh`。

实际从头编译核心包：**95 个测试通过**，fmt check、严格 clippy、二进制构建、fake 子进程 probe 和实际 local probe 均退出 0。`otool -L` 检查两个生成二进制，仅依赖系统 `libSystem.B.dylib`，不依赖 Electron/Node 动态库。

这是核心无 Node 的构建与运行证据，不是整个 GPUI/Go 过渡构建链或干净机发布验收。主机本来装有 Node，但此次命令不可访问它；没有冒充“主机完全未安装 Node”。

## GPUI 构建尚未通过

两次真实 `cargo check -p app-gpui --locked` 都退出 101：

1. 使用默认 developer directory：`xcrun: unable to find utility metal`。
2. 仅为命令设置 `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`：能定位 metal，但编译报 `cannot execute tool 'metal' due to missing Metal Toolchain`。

局部 Xcode 检查退出 0，确认为 Xcode 27.2/27B52。没有更改全局 xcode-select、安装系统组件、跳过 Metal shader 或启动未经验证的窗口。GPUI 应用 Rust 类型检查尚未完整到达，不能从源码核查宣称编译成功。

下一步需要用户允许安装系统 Metal Toolchain，或在已安装该组件的机器上执行：

```bash
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer xcodebuild -downloadComponent MetalToolchain
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer bash scripts/check.sh --ui
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer cargo run --locked -p app-gpui
```

第一条为系统组件安装说明，本轮未执行；可能需要下载大量数据。UI、IME、拖放、大列表和最低 OS 均继续保留未完成状态。

## Kalam 验证边界

只检查旧 arm64 Mach-O 的架构、依赖、导出和生成 header/C shim。详细记录见 [Kalam 接入门禁](kalam-integration.md)。未加载、未编译旧库，也未再分发上游二进制。

## CI 与发布

[CI](../.github/workflows/ci.yml) 已配置 Linux/macOS core 与独立 macOS GPUI build gate；没有远程仓库或推送，因此没有远程 CI 运行结果。当前没有签名 app/DMG、Intel 发布验收、真实设备、大文件传输或性能结果。
