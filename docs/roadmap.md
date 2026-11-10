# 第一版工程阶段计划

目标：原生 Rust/GPUI app + Rust worker，不含 Electron/Chromium/Node；Kalam/Go/libusb 可以过渡，不同时重写协议内核。

## 已完成的初始化切片

- [x] 独立目录/Git main 分支，不修改旧仓库。
- [x] 固定工具链、依赖锁及 UI/core 测试边界。
- [x] 设备/会话/storage/object 引用，storage 根目录，无溢出 u64 汇总。
- [x] 来源/目标快照；终态不可改写；未知结果禁止调度后续任务。
- [x] 有界 IPC、显式 fake backend、子进程握手/错误/失效/退出测试。
- [x] 本地只读目录 adapter 与 CLI 验证。
- [x] GPUI 固定发布版双面板启动源码。
- [x] 本地核心测试与验证脚本；CI 配置已写，但尚未运行远程 CI。

## 下一个切片：可运行 UI + 只读设备原型

1. 安装/确认可用的 Xcode Metal Toolchain，完成 GPUI check/build；保留真实失败日志，不跳过 shader 冒充通过。
2. 接入后台本地浏览与真正虚拟列表，处理焦点/selection/revision；本地目录载入失败不显示为空目录。
3. 验证中文 IME 和真实 Finder 文件拖入。静态中文 label 不作为 IME 证据。
4. 在专用绑定层用 C fixture 确认 Kalam callback ABI/释放/唯一结果；把真实库加载放独立 worker。
5. 核实上游授权与构建输入，迁移 Go/libusb 构建入口，确保不依赖 Node/zx。
6. 一台真机只读连接、storage、目录浏览，明确 busy/锁屏/授权/占用与断开状态。

## 随后：安全传输 MVP

- 逐项计划和冲突策略；本地 staging→验证→commit；远端安全接口改造。
- 真实 MTP upload/download、mkdir/rename/delete；歧义对象拒绝破坏操作。
- 队列停止/真实取消能力区分；deadline、worker 故障、持久 journal 和结果核对。
- 下载失败原目标不损坏；写入未知结果不重发；只清理任务归属临时文件。
- >4 GiB 真正双向文件传输与校验；至少三类 Android/内部存储/真实 SD。

## 发布门槛

两架构和最低 OS 发布包、rpath/动态库闭包、TCC/睡眠/退出、签名/公证/干净机；保护旧 profile；默认无 telemetry；依赖许可/SBOM 完整。没有真机、许可或安装包证据时，不宣称首版产品已完成。

## 不承诺

断点续传、同设备并行、多设备、设备→Finder、远端预览、Windows/Linux、移除 Go。Linux 核心 CI 只是纯 Rust 测试，不代表跨平台 MTP 产品支持。
