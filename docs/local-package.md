# 本地开发 `.app` 打包与 Finder 启动

## 状态与边界

这是 **local developer bundle**，不是正式发行、签名认证、license clearance 或真机授权验收。

父代理已实际验证先前 UI 十项测试通过、debug app/worker/probe 构建成功，并用真实 Kalam 配置启动窗口；当前没有看到手机，不能声称真机浏览或传输已验收。本 slice 新增四项纯 startup 配置测试，等待父代理统一 Cargo/clippy/build；本代理没有运行 Cargo、没有实际生成 `.app` 或 ZIP、没有启动新窗口。

本代理只做了只读输入检查、stable `rustfmt`、脚本 `bash -n` / `--help` 与 diff 检查。当前四个输入都是 thin **arm64** Mach-O：

- `target/debug/app-gpui`
- `target/debug/mtp-worker`
- `target/native/arm64/kalam.dylib`
- `target/native/arm64/libusb.dylib`

当前两动态库的 `LC_BUILD_VERSION minos` 都是 **26.0**（Kalam SDK 27.2，libusb SDK 26.4）。脚本从全部四个输入检测 deployment target，并将实际最大值（不低于 13.0）写进 `LSMinimumSystemVersion`；不以 plist 或二进制 patch 伪造 macOS 13 支持。父当前环境为 macOS 26.6；macOS 13 是未来未验证目标，Intel/Rosetta 未验且脚本明确拒绝非 arm64 host 和非 thin arm64 输入。

外部 Go 依赖、libusb 及全部上游许可/来源尚待完整确认，仅限本地开发。无任何 public commit、上传或公开传输。

## 脚本接口

`bash scripts/package-local.sh --help` 不构建、不打包。

父代理先完成新增 startup 代码的统一构建，再使用已验证二进制打包：

```sh
# 不触碰 Cargo 锁；由父代理在统一 build 之后执行。
bash scripts/package-local.sh --skip-build

# 显式本地 ad-hoc 签名：只签新 bundle 中的副本，不改输入。
bash scripts/package-local.sh --skip-build --ad-hoc-sign

# 可选仅本地 ZIP；不上传、不公开。
bash scripts/package-local.sh --skip-build --ad-hoc-sign --zip
```

默认输入 debug。已有 release artifact 可用 `--profile release --skip-build`；自定义 target 路径可用 `--binary-dir /absolute/path/to/debug --skip-build`。`--native-dir` 可选，但必须解析到本 repo 的 `target/native/` 下，拒绝以旧 app/external library 为隐式打包输入。

不带 `--skip-build` 时脚本运行正常 `cargo build --locked -p app-gpui -p mtp-worker`，release profile 加 `--release`，不绕过 GPUI Metal shader 构建；尊重 `CARGO_HOME` / `CARGO_TARGET_DIR`。**父代理当前负责统一检查，本轮不运行此默认模式。** 例如未来独立本地开发时：

```sh
RUSTUP_TOOLCHAIN=stable bash scripts/package-local.sh
```

脚本不构建 native 库；输入必须已由当前本地 native 构建流程产生。没有 Node、npm、旧 Electron app 依赖、替代 server 或自动下载动作。

## 输出与安全规则

每次成功生成：

```text
target/bundles/<uuid>-MTP-Native.app/
  Contents/
    Info.plist
    MacOS/
      app-gpui
      mtp-worker
    Resources/
      kalam.dylib
      libusb.dylib
      Kalam-LICENSE.txt
      LOCAL-DEVELOPER-NOTICE.txt
      SOURCE-SHA256.txt
target/bundles/<uuid>-MTP-Native.app.payload-sha256.txt
# 仅 --zip 时：
target/bundles/<uuid>-MTP-Native.app.zip
```

- 不 `rm`、不 `mv`、不覆盖旧 bundle；UUID 输出目录须全新，输出祖先不允许 symlink，resolved output 必须仍在 repo 的 `target/bundles`。
- 失败保留本次新建的 partial bundle 用于检查，非零退出且不打印成功；不会为了清理而删除旧目录。
- 复制前检查 arm64、install name、Mach-O dependencies、LC_RPATH 和 minimum OS。仅允许系统库或预期 bundle-local loader path，发现 Homebrew/旧 app/外部依赖立即拒绝，不偷偷 `install_name_tool` 改库。
- 当前 Kalam 使用 `@rpath/kalam.dylib` install ID，实际依赖 `@loader_path/libusb.dylib`；worker 按绝对路径加载其自身 bundle 的 Kalam，无需伪造外部 rpath。
- 复制后 `cmp` 验证 input/copy 一致，SHA-256 检测打包期间输入变化，保存源输入 hashes。最终 payload hashes 在 `.app` 外保存，避免签名后修改 sealed Resources。
- `--ad-hoc-sign` 明确触发系统 `codesign --sign -`：先 native libs、worker、app，再 bundle；然后 `codesign --verify --deep --strict`。没有 Developer ID、notarization、production hardened runtime 或 App Store sandbox 权限声明。
- `Kalam-LICENSE.txt` 保留仓库中的 Kalam MIT notice；开发 notice 明确 libusb、Go 及其他依赖各自许可证待完整审查，此 notice 不替代合法再分发文件或扩大权利。
- ZIP 仅 opt-in，用系统 `ditto` 保留 app resource metadata；生成不会传输或公开。hash/provenance 可能包含开发机绝对路径，所以也不应直接公开。

## Finder 启动配置

父代理生成 bundle 后可用 Finder 双击，或：

```sh
open '/absolute/path/to/<uuid>-MTP-Native.app'
```

不自动连接设备。应先显示「Kalam 已配置 · 尚未连接设备」，再由用户点击连接；无手机时是实际错误，不是 Fake 成功。

优先级为：

0. **`--local-only` 最高优先**：忽略所有 library/env/bundle 配置，不探测资源、不启动 worker、不访问手机或 USB；本阶段 UI 预览必须用此参数。
1. 明确 `--library ABSOLUTE_PATH`。
2. 明确非空 `MTP_KALAM_LIBRARY`。
3. 仅当前可执行文件位于 `*.app/Contents/MacOS/app-gpui` 时，检查 **自身** `Contents/Resources/kalam.dylib` 普通文件（不自动跟随 resource symlink）。
4. 没有自身 bundle resource，或运行裸 `target/debug/app-gpui`，保持 `Unconfigured`。

错误/相对的 explicit 配置不回退到 bundle。不会搜索旧 OpenMTP、Homebrew、cwd、repo native 路径、其他 app 或 Fake 后端。所有 env/args/current_exe/file check 在 startup，不在 Render。worker 始终是 app 可执行文件同目录 `mtp-worker`。

临时 override 可直接运行 bundle 内 binary：

```sh
'/absolute/path/to/<uuid>-MTP-Native.app/Contents/MacOS/app-gpui' --library /absolute/path/to/kalam.dylib
```

此方式仍仅本地开发；不要通过全局 launchctl env 把配置影响其他 app。普通 Finder 从本 bundle 自身 Resources 获取配置，无需全局 env。

## 新增纯测试（待父统一执行）

startup resolution 提供可注入 args/env/exe/resource-check 的纯函数，四个新测试不访问真实文件系统、不启动 worker：

- CLI 显式路径胜过 env 和 bundle，并证明不探测 resource；env 路径也优先且不探测。
- Finder bundle 仅使用自身 Resources，并选择同目录 worker。
- resource 缺失、裸 executable 或 exe 未知时不回退其他位置。
- invalid explicit 路径即报错，不隐式使用 bundle。

## 父代理实测记录

父代理统一验证新增 startup 后：GPUI **14/14** 测试、严格 workspace clippy 及 debug app/worker/probe build 均通过。随后实际执行 `bash scripts/package-local.sh --skip-build --ad-hoc-sign`，退出 0；生成唯一 bundle、plist lint、四输入 hashes/cmp、实际 minimum macOS **26.0**、逐文件与 bundle 签名验证均通过。该次包使用已验证的只读浏览二进制；后续文件传输服务/UI 完成后须重新构建和再打包，不能将这个快照说成已验收的完整 MVP。

实测输出为本 repo `target/bundles/58e46906-653c-4d26-894b-458ac9bc4b5a-MTP-Native.app`，没有生成 ZIP、没有上传或公开发布。新 bundle Finder 启动、重复打包隔离、真机浏览/传输继续由后续验收补充。

后续 FD 单文件版本（Rust 提交 `d010643`，legacy native 封口 `eb36d52`）完成 **200 core / 24 UI** 测试与严格 clippy 后，再执行 `--skip-build --ad-hoc-sign --zip`，退出 0。生成不同 UUID 的 `target/bundles/ca9b0c8f-025d-4c76-b5f5-d88f5727f222-MTP-Native.app` 及本地 ZIP，不覆盖前一个包，仍按四个 payload 检查 minOS **26.0**、依赖、cmp、hash 和深度签名。native 输入 hash 为 `c68a33c62e8c5eba0b1eda9da8f73d25573ce139a4e5fb663a780d942cbc34ea`。

没有 env/CLI library override 的 bundle 内 executable 已实际启动；CoreGraphics 查询应用名 `MTP Native (Local Developer)`，可见窗口 **2340**，1100 × 732。旧只读窗口已关闭。此方式检验自身 bundle 路径资源配置，不应混称 LaunchServices/Finder 双击交互已经逐项手测；screen capture/IME/拖放也不因窗口可见而自动通过。

同一包内 worker 与 Resources 库执行真实只读探针，成功识别三星 SM-S9280、两 storage 根目录并 clean shutdown，退出 0、不写文件。用户随后授权主存储 `/tmp` 独立小文件测试；后续真实写验收另见 [验收文档](device-acceptance.md)，须以实际结果为准，当前不能由打包成功推断双向传输已通过。

## UI 交互开发包复核（当前里程碑）

代码基线为 `1e45f2e`（UI/交互）与 `369d0f1`（dispatch/诊断安全修复）。232 核心 + 40 GPUI 受控测试、完整 workspace strict Clippy、fmt/build 与 fake/local 探针全部通过；具体未验收边界见 [验证记录](validation.md#ui-交互完善后的父会话回归)。

使用该 UI 代码构建并执行 `--skip-build --ad-hoc-sign --zip`，生成全新 `55caee1f-d8e3-4dab-ac42-cad4b05d0739-MTP-Native.app` 与本地 ZIP；不覆盖任何旧包。最低 macOS 26.0、thin arm64，本地 ad-hoc 签名，不是 Developer ID／notarized／许可已清理的正式发行。

后续目标轮次再次实际复核：`codesign --verify --deep --strict` 通过，`unzip -tq` 无压缩数据错误，ZIP SHA-256 为：

```text
20ff49ad649e772d10f8609c965cac329ada1875be750704e95af40badb4afbf
```

校验对应 repo 内 `target/bundles/55caee1f-d8e3-4dab-ac42-cad4b05d0739-MTP-Native.app.zip`。这里只检查本机已有文件，没有运行该包、连接手机、读取 UID、执行 inspect 或重试旧失败上传。

父会话以裸 binary 的 `--local-only` 实际开窗，查询到 onscreen 外框1100×732且 MTP worker 数量0，随后关闭自己的窗口。这个证据不等于 Finder／键鼠／最小窗口／剪贴板／IME／像素视觉或手机传输通过。预览必须明确 local-only；手机验证继续等用户恢复授权。原 RequiresReview、报告、快照与 journal 未被改写或清理。

## 初期父代理验收清单（历史 slice，非当前待办列表）

- [ ] 统一 `cargo test -p app-gpui`（原 10 + 新 4）、clippy 与 debug app/worker build，通过后再打包。
- [ ] 执行 `bash scripts/package-local.sh --skip-build --ad-hoc-sign`，检查 app 结构、plist lint、真实 minimum OS、native paths、签名验证及 hashes；脚本最后成功状态不等同真机验证。
- [ ] 再打包一次，输出不同 UUID app，旧 app 和输入 hashes 未被覆盖/改变。
- [ ] Finder 打开本 bundle，无 env/CLI 时本地 pane 可用、Kalam 是本 Resources 配置、worker 是本 MacOS sibling；没有自动连接/Fake。
- [ ] 没手机时实际 connect 错误；有授权设备时进一步验收，尚不声称 browse/transfer 全面通过。
- [ ] 不启用下载/上传 UI（服务 API 尚未提供本 slice）；不做写传输、安全边界或 license-cleared 正式发行承诺。
- [ ] 可选 ZIP 在本机检查 metadata 后保留本地；未经完整许可/真机/权限审核不公开。
