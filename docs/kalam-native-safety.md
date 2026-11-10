# Kalam native SafetyVersion 2 — local MVP

**仅 local developer build，非正式再分发。没有真实设备传输验证。**

源码来源、Ganesh Rathinavel 的原 MIT 授权及固定依赖见
[来源说明](../native/kalam/SOURCE.md)、[原 LICENSE](../native/kalam/LICENSE)、
[go.mod](../native/kalam/go.mod)、[go.sum](../native/kalam/go.sum)。
固定 go-mtpx 下载模块未发现 LICENSE，授权尚未确认；OpenMTP 的 MIT 不能
自动覆盖独立依赖。不得据此将 dylib 纳入正式发布物。

## API 接线（最终 FD ABI）

[exports](../native/kalam/kalam.go) 中 `uint32_t SafetyVersion(void)` 返回 **2**。
version 1 曾用于临时同步/路径 ABI，本机也生成过旧四参数 SafeUploadSingle
产物，不能将 v1 的同名 symbol 当作当前 FD ABI。v2 明确对应本文件的 FD
签名；Rust 上传/下载只允许 marker==2+相应 safe symbols，旧库/v1 只能用于读。
这是锁/回调/所有权与本文件定义的 FD 安全 API 标识，不是设备兼容、原子
持久性、恢复能力或“所有数据安全”的认证。缺 marker 或 safe symbol 必须
fail closed；不得降级调用旧 UploadFiles 或 DownloadFiles。

```c
typedef void (*on_cb_result_t)(char *);
uint32_t SafetyVersion(void);
void SafeUploadSingle(char *json, int source_fd,
    on_cb_result_t *preprocess, on_cb_result_t *progress, on_cb_result_t *done);
void SafeDownloadSingle(char *json, int output_fd,
    on_cb_result_t *progress, on_cb_result_t *done);
```

**兼容旧 Kalam 的 callback 指针形状**：参数值就是函数地址 cast 到
`on_cb_result_t*`，不是 `&function_pointer`，C shim 不解引用 callback 参数。
Rust 可按该机器 ABI 传 Option<extern "C" fn(*mut c_char)> 地址，具体接线必须
验证 generated C header。JSON 输入是 NUL 结尾、调用期间保持存活的借用字符串。

所有 device operation 都在各 export 内 Lock + defer Unlock；锁覆盖 JSON
解析、MTP 操作、所有同步 progress/preprocess callbacks、done 到函数返回。
SafetyVersion 仅常量，不访问 session。回调不能重入任何 Kalam operation，
不能跨 C 边界 unwind，不能持有需要 worker 获取的锁等待 worker。

每个非空 callback 接收一个独立 `C.CString`：**Rust 立即复制/解析并使用
libc::free 恰好释放一次**，包括错误、progress、preprocess 和 done。Go 不再
二次释放。callback 为空时 [C shim](../native/kalam/send_to_js/main.go) free。
回调是同步事件，无共享 pInterface、goroutine 或 500ms 轮询；指向上游可变
ProgressInfo 的 Go 指针从不交给 Rust。done 才是成功判据，100%/Completed
进度不能替代 done。

FD 原值由 Rust 保持存活至同步调用 return；Go 立即 Dup、CloseOnExec、NewFile，
只关 own duplicate，在 done 前检查 Close 错误。**Dup 共享 offset**，Rust 必须
独占 staged FD，不能同时读写/seek；原 FD 不会被 Go 关闭。上传从偏移 0 读取，
下载向偏移 0 写入。传入 FD 不得是目录、pipe、socket；下载拒非空及 O_APPEND。
没有任一 safe API 根据 ambient pathname 打开/创建文件。

### SafeUploadSingle

输入（camelCase，policy/requestedName/parentId 必填，parentId 可显式 null）：

```json
{"storageId":65537,"destination":"/DCIM","parentId":456,"requestedName":"photo.jpg",
 "policy":"keepBoth","expectedSize":12345}
```

- source_fd 必须是 Rust 为本次请求建立的 immutable upload-stage snapshot，
  regular，fstat 长度必须等于 expectedSize。可选 `source` 仅 display label，
  即使不存在、变为 symlink、重绑定也绝不用于 IO。
- destination 仅是已存在 canonical absolute MTP 目录的 label/一致性检查，
  **不是授权重新选择当前同名目录**。parentId 字段必须出现：null 明确 root，
  显式 0/ffffffff 规范化为 root，仅接受 destination:"/"；省略字段拒绝。
  非 root 必须传不可变 RemoteDirectoryRef 的 original object ID。先按该 ID
  GetObjectInfo 验证仍为目录且 storage 匹配，再按 label 解析结果必须同 ID；
  旧目录被 rename/delete、同名新目录换 ID 时 ErrorStaleSession/NeedsReview，
  **不调用 SendObjectInfo/SendObject**。direct SendObjectInfo 的 parent 使用
  supplied original ID，不采用新目录 ID。不自动 mkdir。
- requestedName 必须是 basename，拒 `/`、`\\`、NUL、控制字符、`.`、`..`。
- 最终对象名始终为 `stem-mtp-<crypto/rand 128bit hex>.extension`，保守限制
  UTF-8 stem/extension 长度；原名不交上游覆盖路径。
- done envelope：

```json
{"errorType":"","error":"","data":{"committed":true,
 "actualName":"photo-mtp-<32 hex>.jpg","objectId":123,"bytes":12345,
 "path":"/DCIM/photo-mtp-<32 hex>.jpg","needsReview":false}}
```

错误前置验证可以沿用 `data:null` 的旧 error envelope；进入 safe core 后 data
保留同一字段结构。create 开始后任何错误，包括 unknown objectId=0，均
`committed:false,needsReview:true,errorType:"ErrorNeedsReview"`。bytes 是实际
从 staged FD 消费的字节，不是未验证的 expectedSize。失败对象保留，不按 path
删除、不自动重试、不宣称原名已提交；父 worker 必须进入 NeedsReview。

实现见 [safe upload](../native/kalam/safe_upload.go)：列出目标 parent 的同名
对象；另快照 **all storage/all parent** 的所有已有 object ID；选择随机未占名；
直接 SendObjectInfo（不经过 go-mtpx handleMakeFile），验证新 ID 非已有、非
special，返回 storage/parent 对齐，再 SendObject。按 ID 读回 filename、parent、
storage、format、size；核对本地 FD 长度/mtime 与消费 count，重新列 parent 排除
同名别 ID。全部通过才 committed=true，表示**最终 KeepBoth 新对象 metadata
验证完成**，不是原 basename rename 成功、hash 校验或持久性承诺。

### SafeDownloadSingle

```json
{"storageId":65537,"objectId":123,"expectedSize":12345,"parentId":456}
```

parentId 可省略：只检查 object ID+storage+size，不暗含 root；显式 0 与 ffffffff
按 root 等价处理。output_fd 必须由 Rust 通过已绑定目录 FD、NOFOLLOW、EXCL
创建的 private empty staged regular 文件获得。实现 [safe download](../native/kalam/safe_download.go)
先绑定 Dup，读 source metadata 拒目录/storage/parent/size 错误；直接 GetObject
写 bounded countingWriter，禁止超 expectedSize 字节；核对 count+stage fstat
长度，并重读 source metadata/name/parent/mtime 检查变化，Sync stage，关闭
own duplicate，然后 done。没有 os.Create、path traversal 或 stage 路径重新打开。

```json
{"errorType":"","error":"","data":{"committed":true,"objectId":123,"bytes":12345}}
```

此 committed=true **仅表示 staged 文件完成**，不是最终 destination 发布成功。
Rust 还必须从持有的目录 FD 进行原子 NOREPLACE commit，并核对 destination/
stage inode 身份和记录 journal。failed stage 可由 Rust 基于自己的 ownership
处理；native 不删除用户文件。此 ABI 不接受目标 pathname。

> expectedSize 使用 u64 输入，但当前 Go/Rust byte-count 接口只支持 <=MaxInt64。
> 超范围必须明确拒绝，绝不 truncate。远端 CompressedSize=ffffffff 时通过
> 固定 mtpx.GetFileSize 的 OPC_ObjectSize u64 查询读取实际大小，不把 sentinel
> 当作实际长度。真实 >4GiB 传输未验证。

## 固定上游审查，不是换 flag 宣称安全

go-mtpx 固定提交 `18f12db021cc`：

- main.go:473–475 UploadFiles 强制 `handleMakeFile(...,true,...)`。
- helpers.go:229–273，overwriteExisting=true 时查同名后 DeleteFile 再
  SendObjectInfo；false 时若存在反而直接返回**已有 ID,nil**，没有上传却可能
  假成功。因此改变 flag 既不是安全 commit，也不是可靠上传语义。
- main.go:240–263 RenameFile 直接 SetObjectPropValue(ObjectFileName)，无目标
  collision/no-clobber 保证；RCError 0x2002 GeneralError 被忽略当成功。
- download helper 用 os.Create 可跟随 symlink/祖先重绑定、截断用户已有文件；
  事后 inode 检查无法撤销这些写入。private stage pathname 本身也不够。

所以首 MVP **禁 Replace、禁原名 rename commit**。旧 UploadFiles、RenameFile、
DeleteFile exports 保留符号但一律拒绝。MakeDirectory 保留 bool，不提供对象
ownership，不能拿它做 destructive cleanup。旧 DownloadFiles 已改同步 callback
但仍是兼容 legacy，不获安全背书；Rust 永不调用它。safe transfer interface
没有 delete/rename/路径创建方法，避免意外回退。

### 原子性与剩余限制

MTP SendObjectInfo/SendObject 是“分配新对象 + 传字节”，**不是事务式原子
publish 或协议级 create-if-absent**；传输中设备可能显示部分对象，断线可能留下
残片。随机 final 名加 preflight/new-ID 检查避免本 wrapper 主动删除/覆盖已有
对象；不能证明所有固件对同名或异常对象 ID 的行为，也不能锁住其他 host/app。
如果设备不能完成严格 metadata/handle 校验，必须失败/NeedsReview，而不是
继续尝试旧覆盖路径。本实现未做读回 hash、持久性验证或跨进程恢复，mtime
检查也不是 immutable 内容密码学证明。Rust upload stage 应真正独占且稳定。

## 本地构建与验证

[Bash builder](../scripts/build-kalam.sh) 只用 Bash、Go、cc/cgo、可选 pkg-config
及 macOS 系统 Mach-O/signature 工具；无 Node/npm/zx/Cargo。缓存为 repo
`.cache/go-build`、`.cache/go-mod`、`.cache/kalam`，输出 target/native/arm64
或 target/native/amd64。Go >=1.26 避免旧 chained-fixups seg_count 加载错误；
无需二进制 patch、更换系统 linker 或修改 xcode-select。通过单次命令设置
DEVELOPER_DIR/SDKROOT，不改变全局工具链。

此机没有 pkg-config/libusb header 安装，使用**显式 local input**。header 本地
开发准备来自 https://raw.githubusercontent.com/libusb/libusb/v1.0.30/libusb/libusb.h；
不得将来源不明的旧头文件当成已审查发布依赖。

```bash
LIBUSB_DYLIB=/Users/zheng/Development/MTP/openmtp/build/mac/bin/arm64/libusb-seg5.dylib \
LIBUSB_INCLUDE_DIR="$PWD/.cache/libusb/include" \
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer \
bash scripts/build-kalam.sh arm64
```

local mode 使用 [有限 pkg-config shim](../native/kalam/pkg-config-local.sh)
仅响应 cgo 的 libusb-1.0 flags，不冒充完整 pkg-config。只把旧 dylib **作为
local build input 复制到输出目录**，绝不写旧项目/source tree；只修改复制品
install ID，再 ad-hoc sign。输入的架构、版本和来源由开发者明确选择。

2026-11-10 本机验证（不是 USB 传输）：

- macOS 26.6 build25G72，Go1.27.3 darwin/arm64，Xcode27.2 build27B52，
  单次 DEVELOPER_DIR 选用 MacOSX27.2.sdk；系统 active CLT 保持不变。
- arm64 c-shared 构建成功；nm 有 SafetyVersion/SafeUploadSingle/SafeDownloadSingle
  及 legacy 读符号；otool 显示 `@rpath/kalam.dylib` 与
  `@loader_path/libusb.dylib`，无旧 Homebrew placeholder/input 绝对路径依赖。
- codesign --verify --strict 校验 Kalam 与复制的 libusb；真实 dlopen RTLD_NOW
  成功，证明本机 loader 可加载此次产物，不能推导其他 macOS 都兼容。
- `go test -race ./...` mock 测试通过：新 ID拒复用、Replace 拒绝、metadata/count
  错误不假成功、source label不IO、原 FD 保持打开、parentId省略拒绝、原目录
  同名替换换ID时不Create/Send、FD下载路径被换为 symlink
  不触碰victim、超长/短数据、非空stage/目录拒绝；未调用 upstream 真机测试。
- [C smoke](../native/kalam/tests/abi_smoke.c) 验证 FD符号/ABI、marker=2、400次
  跨4pthread callback串行且立即free、1000空 callback 与禁止覆盖调用；不 Initialize
  设备，不执行真传输，Go runtime 留到进程退出不 dlclose。
- 原 libusb local input SHA256：
  `e69759d341ebe6bcd25d41c9bc6b0a2d2d07893dac5dd370391955bc7296d8ec`。
  header SHA256：`a61260ab145b051b86df2b0575956f01810190abe2c7df6aca831c33bdc8082c`。

**未验证**：真实USB upload/download/rename、设备固件no-clobber、断线/拔线/恢复、
真实大文件、数据hash、多个设备、amd64产物与运行、其他SDK/macOS、正式授权/
分发。不得写“传输测试通过”或“支持所有设备/平台”。
