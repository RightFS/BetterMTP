# Kalam 过渡后端接入门禁

## 当前事实

2026-11-10 在 arm64 macOS 上只做静态检查，未加载 dylib、未连接手机、未执行旧后端构建。

参考源码基线：OpenMTP `5da350b69c6d0d4aed67a063498ee89c7448400a`。旧仓库保持未修改，本仓库没有复制或再分发 Kalam/libusb 二进制。

- `file` 确认旧 arm64 `kalam.dylib` 和 `kalam-seg5.dylib` 都是 arm64 Mach-O 动态库。
- `otool -L` 确认 seg5 库引用 `@loader_path/libusb-seg5.dylib`、系统 libresolv 和 libSystem。
- `nm -gU` 确认 11 个业务导出：Initialize、FetchDeviceInfo、FetchStorages、MakeDirectory、FileExists、DeleteFile、RenameFile、Walk、UploadFiles、DownloadFiles、Dispose；未发现 Cancel 导出。
- 旧生成 header 声明回调形参为 `on_cb_result_t*`。C shim 将这个值强转成函数指针再调用，而非解引用函数指针槽位。Rust 绑定必须明确这项非直觉 ABI，并以可控 C fixture 验证，不能只凭生成 header 猜调用方式。
- 旧 callback payload 使用 `C.CString`；所有权/释放必须通过窄 shim 或明确释放契约实现。当前 Rust core 禁止 unsafe；未来 unsafe 只能在专用绑定 crate 中经审查开放。

本地证据：[生成 header](../../openmtp/build/mac/bin/arm64/kalam.h)、[C callback shim](../../openmtp/ffi/kalam/native/send_to_js/main.go)、[导出与进度](../../openmtp/ffi/kalam/native/kalam.go)、[Go 依赖](../../openmtp/ffi/kalam/native/go.mod)。这些父工作区链接在独立 clone 后可能不存在，基线提交与本页结论保留。

## 接入顺序

1. 在新仓库建立经授权的 Kalam 来源获取和构建机制，锁定 Go/上游版本与 libusb 输入；用 shell/Rust 构建入口取代 Node/zx，不修改参考仓库。
2. 先用可控 C fixture 测试回调 ABI、JSON 限额、字符串释放、重复终态和错误输出。
3. 在独立 worker 中验证库加载/退出，不默认加载未经验证的历史变体，不凭文件名宣称系统兼容。
4. native 单 owner 串行；修复旧锁覆盖范围、进度共享状态及关闭引用清理。所有错误进入唯一 Result。
5. 加入设备枚举和只读目录 contract；使用真实 device/storage/session generation，执行时复验引用。
6. 传输前完成逐项接口及 staging 安全设计；下载不能直接截断最终用户文件，上传不能先删除原目标再写新对象。
7. 当前文件取消仅在确有底层能力时开放；否则只停止后续项，超时/强杀标记待核对，禁止自动重发危险写。
8. 真机与两架构发布测试通过后才能宣称支持。go-mtpx 授权核实是再分发门禁，主仓库 MIT 不代表依赖授权已确认。

## 尚未交付

真实 Kalam adapter、Go 构建脚本迁移、任何真实 USB/MTP 操作、安全 Replace、任务持久 journal、签名/公证及安装包。当前 worker 仅有显式 `--backend fake`，上传/下载能力均为 false；不存在自动 Legacy fallback。
