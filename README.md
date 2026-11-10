# MTP Native

OpenMTP 重构的独立 Rust 工程。首版目标：移除 Electron / Chromium / Node 的应用运行时及构建依赖，使用 GPUI 原生界面、Rust 应用层和隔离 worker；允许 Kalam（Go + libusb）作为过渡后端。

原始方案：[Rust + GPUI 重构方案](../docs/OpenMTP-Rust-GPUI-重构技术方案.md)（此链接仅在当前父工作区有效）。旧源码是回归参考，不作为本工程的 Node 构建依赖。

## 已有代码与运行方式

- 领域引用、存储根目录、不可变复制快照、逐项/批次状态机。
- 1 MiB 限额的版本化 IPC，显式 fake worker 和真实子进程合同测试。
- 本地只读目录浏览；保留原始路径、不跟随 symlink 元数据。
- GPUI 0.2.2 原生双空面板源码，**尚未通过编译/窗口运行验收**。

```bash
# 核心：格式化检查、单元/子进程测试、严格 clippy、fake probe
bash scripts/check.sh

# 本地浏览（上一步已生成二进制）
target/debug/mtp-probe local /path/to/directory

# 独立 fake worker 探针；没有 USB 或真实传输
cargo run --locked -p mtp-probe -- fake

# 需要真实可用的 Xcode Metal Toolchain
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer bash scripts/check.sh --ui
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer cargo run --locked -p app-gpui
```

Cargo 不在 PATH 时先 `export PATH="$HOME/.cargo/bin:$PATH"`。初始开发机仅有名为 stable 的已安装工具链，但其实际版本为 1.99.1，因此本轮用 `RUSTUP_TOOLCHAIN=stable bash scripts/check.sh` 验证；脚本会核对实际 rustc 版本，不接受漂移。其他机器按固定 [toolchain](rust-toolchain.toml) 安装对应版本。脚本默认把 Cargo 缓存放工程内忽略目录，不更改系统包管理器。

详细证据与未完成项：[验证记录](docs/validation.md)、[GPUI PoC](docs/gpui-poc.md)、[Kalam 接入门禁](docs/kalam-integration.md)、[阶段计划](docs/roadmap.md)。

## 开发状态

这是技术验证工程，不是已经可用的手机传输产品。真实 MTP 写入在安全 ABI、许可及真机测试完成前不开放。阶段能力和命令随可运行实现更新；不把 fake backend 测试等同于真机通过。

## 架构边界

- `domain`：设备/条目身份、任务状态和冲突安全规则，不依赖 UI/USB。
- `backend-api`：有界、版本化 worker 通信；stdout 只承载协议。
- `local-fs`：本地浏览，阻塞 IO 由调用方放后台执行。
- `mtp-worker`：单会话 owner；初始 fake adapter，不执行用户文件写入。
- `probe`：真实子进程端到端探针，验证握手、会话和错误行为。
- `app-gpui`：原生窗口技术验证；核心测试不依赖 GPU 窗口环境。

## 工具链

Rust 1.99.1（当前开发机实测），Go 只在后续构建 Kalam 时需要。新版不使用 Node/npm/yarn/zx。macOS 最低版本、Intel/Apple Silicon 发布兼容性仍须 PoC 实测。

## 提交原则

每次提交只包含一个可解释的工程变化；提交正文记录动机、验证命令/结果及已知边界。无真机结果不得描述为“传输验证通过”，未修复旧 Kalam 的安全缺陷不得宣称安全覆盖。
