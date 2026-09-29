# 源码构建、测试与消费者集成

[English](development.md) | 简体中文

## 环境与目录

在仓库根目录执行命令。需要 Python 3.12、.NET 8 SDK、Rust 1.98.1 和原生编译器。Windows x64 使用包含 Windows SDK 的 Visual C++ MSVC 构建工具；Linux x64/ARM64 使用 Ubuntu 22.04、GCC/build-essential 和 binutils，并安装下表对应的 Rust target。Windows 本地开发不需要虚拟机、WSL、Docker 或交叉编译器。工具链声明见 [rust-toolchain.toml](../rust-toolchain.toml)，完整依赖锁定在 `native/Cargo.lock`。

| 路径 | 职责 |
| --- | --- |
| `Fizzy.McapSharp.csproj` | 库项目与包元数据；仅编译 `src/` 下的源码 |
| `src/` | 公共 .NET API、P/Invoke 和 SafeHandle |
| `native/src/lib.rs` | Rust MCAP 封装、私有 C ABI、文件映射与校验 |
| `tests/Fizzy.McapSharp.Tests/` | 托管功能与资源生命周期测试 |
| `tests/Interop/`、`tests/interop.py` | .NET 与官方 Python MCAP 双向互操作 |
| `scripts/` | 构建、打包与隔离包测试 |
| `.github/workflows/` | CI 构建与手动发布 |

`Build.ps1` 调用的 `build.py` 优先使用仓库 `.tools/cargo/bin/cargo.exe`，存在时设置对应 CARGO_HOME/RUSTUP_HOME；否则使用 PATH 中的 Cargo。脚本不安装工具。`.tools/` 为忽略目录，不提交本机工具。

## 构建与测试

```powershell
./scripts/Build.ps1 -Test
# 收齐同一源码构建的三平台原生资产后：
./scripts/Build.ps1 -Test -Pack
./scripts/Test-Package.ps1
```

Linux 使用 `python scripts/build.py build --test`。收齐资产后用 `python scripts/build.py pack` 直接打包，不重复编译；用 `python scripts/test_package.py` 验证完整包。CI 安装 Python 3.12；Linux 环境中的 `python` 应指向 Python 3。

| RID | Rust target | 原生构建 runner |
| --- | --- | --- |
| `win-x64` | `x86_64-pc-windows-msvc` | `windows-2022` |
| `linux-x64` | `x86_64-unknown-linux-gnu` | `ubuntu-22.04` |
| `linux-arm64` | `aarch64-unknown-linux-gnu` | `ubuntu-22.04-arm` |

构建脚本先执行 `cargo build --release --locked --target <target>`，再构建托管 Release 项目。原生产物按目标隔离在 `native/target/<target>/release/`，并暂存到 `artifacts/native/<rid>/`，清单记录提交、源码指纹、目标和二进制哈希。`-Test` 运行构建/打包回归检查和 xUnit，默认只构建宿主平台。脚本不安装工具。

Linux 构建检查 ELF 架构、动态依赖和 GLIBC 符号版本（不得高于 2.35），拒绝外部压缩库依赖和嵌入的库搜索路径，并拒绝自定义 CPU 优化的 Rust 标志。Linux ARM64 使用原生 ARM64 runner 编译和测试。macOS、musl、Ubuntu 20.04、Windows ARM64、32 位、NativeAOT 和单文件发布不属于验证矩阵。

`Test-Package.ps1` 调用 `test_package.py`，在 `artifacts/smoke-<GUID>/` 建立独立应用与包缓存。从项目元数据读取版本，检查所有原生资产的架构，仅从指定包目录还原，并验证普通运行和显式 RID 的 framework-dependent publish 后运行，均覆盖全部压缩模式。可通过 `-PackageDirectory` 或 `--package-directory` 指定包目录。这验证本地包，不代表 NuGet.org 上的包已验证。

现有 xUnit 覆盖三种压缩、时间与 Topic 过滤、元数据与附件、无索引读取、未完成文件、CRC 损坏、恢复结果、写入失败终止、不覆盖文件、枚举释放与无消息声明。测试使用临时文件，无需设备。

### 分配验收

`Build.ps1 -Test` 还会运行 `dotnet run --project tests/Allocations -c Release`。门禁在 Writer/Reader 预热后测量 GC.GetAllocatedBytesForCurrentThread，排除准备和报告，要求消息循环托管分配严格为零，这是库热路径的强制契约。覆盖 None/Lz4/Zstd、原生文件 I/O、实际 FileStream、可寻址/非寻址 span 流，以及跨 Chunk、多 Channel、中途声明、空/大消息、缓冲不足重试、重复 EOF 和查询。程序报告吞吐量及平均每条耗时，这是特定负载测量，不是延迟分位数或通用性能保证。功能测试另外检查重试正确性和回调失败。

只使用 Release。调用方扩容、初始化、描述/摘要快照、错误及自有记录便利 API 不属于门禁范围。用户 Stream 可能分配；桥接专用测试使用预分配 span 流，并在数组回退时抛异常。原生分配需另用原生分析器。基线数据保存在忽略的 artifacts 中，不写入 API 文档。

### Python 双向互操作

在准备好 Python 的环境中执行（CI 使用 Python 3.12）：

```powershell
python -m pip install mcap==1.3.1 lz4==4.4.5 zstandard==0.25.0
$interopDirectory = Join-Path 'artifacts' ('interop-' + [Guid]::NewGuid().ToString('N'))
dotnet run --project tests/Interop -c Release -- write $interopDirectory
python tests/interop.py $interopDirectory
dotnet run --project tests/Interop -c Release -- read $interopDirectory
```

顺序不能交换：先由 .NET 写文件，再由 Python 校验并生成文件，最后由 .NET 校验 Python 输出。覆盖 None、Lz4、Zstd，以及查询、元数据和附件。每次使用新目录，因为 Writer 的路径重载仅创建新文件。`Build.ps1 -Test` 本身不包含互操作测试。

## 包内容

包 ID 为 `Fizzy.McapSharp`，目标框架为 net8.0。同一包包含托管程序集、README、第三方声明及以下资产：

```text
runtimes/win-x64/native/fizzy_mcap_native.dll
runtimes/linux-x64/native/libfizzy_mcap_native.so
runtimes/linux-arm64/native/libfizzy_mcap_native.so
```

`RequireNativeForPackage` 拒绝资产或清单缺失，以及架构、二进制哈希、提交或源码指纹不匹配的情况。本地 `-Pack` 同样要求三平台资产齐全。从一次匹配的 CI 运行下载各个 `native-<rid>` artifact 到 `artifacts/native/<rid>/`，保留清单；不要混用不同运行的资产，收集后不要修改源码。源码指纹会归一化 Windows/Linux 文本换行。输出目录为 `artifacts/packages/`，脚本不发布或修改版本。

### CI 与发布验证

可复用验证工作流在三个原生 runner 上构建，运行 xUnit 和 Python 双向互操作，再统一打包一次。五个包验证任务分别在 Windows 2022、Ubuntu 22.04/24.04 x64/ARM64 上执行，使用独立缓存还原完整包，验证普通运行和 RID 发布运行，并读取所有构建平台生成的样例；缺少样例即失败。Ubuntu 是已配置的验证发行版；其他 glibc 发行版还需具备兼容系统库和 .NET 8。

PR、推送到 `main` 和手动触发均运行该流程，同一 PR/ref 的旧构建自动取消。中间产物保留一天，只有通过全部任务的完整包保留七天。Cargo 缓存按系统、架构、工具链、锁文件和原生源码区分。只使用标准托管 runner，不配置付费 larger runner 或额外缓存额度。按 GitHub 当前规则，公开仓库的标准 runner 运行时间免费，存储仍受账户额度约束。

手动发布工作流运行相同验证，将已验证的同一包交给受保护的发布任务，不重新打包。维护者发布规则见 [AGENTS.md](../AGENTS.md#publishing)。CI 成功不等于 NuGet.org 已发布包通过验证。

## 消费者集成

默认使用 `PackageReference`。源码调试由消费者的 MSBuild 条件配置选择引用：

- `UseFizzyMcapSharpSource=true` 时使用指向本仓库项目的 `ProjectReference`，根路径由 `FizzyMcapSharpRoot` 提供。
- 关闭源码模式时使用 `PackageReference`，同一项目不得同时引用两者。
- 这些属性是消费者集成约定，本库项目不会自行替消费者切换引用。

源码模式先运行本仓库 `./scripts/Build.ps1`，再 restore/build 消费者。托管项目优先使用显式 `RuntimeIdentifier`，否则使用 SDK 宿主 RID，只将对应的已有原生库传播到源码消费者输出；直接 `dotnet build` 不会编译 Rust。使用当前 checkout，不擅自拉取或切换分支；切换 Source/Package 模式后重新 restore，共享输出串行构建。本机路径和切换配置不提交。

在联合工作区中，API 或行为变化还应验证 RobotController、Parallax 的 Source 构建及相关测试，具体入口以父工作区 README 为准；独立使用本仓库不要求这些消费者存在。报告需区分源码、本地包和已发布包的验证范围。


### 官方 API 与扩展分配门禁

`Build.ps1 -Test` 同时运行锁定依赖的 Release 原生差分测试和 `scripts/check_api_coverage.py`；[覆盖清单](coverage.zh-CN.md) 对照实际 Cargo 源码，包含可选 Tokio 公共声明。清单检查不能替代行为测试。已有锁定版本的 binrw 增为直接依赖，用于编码上游自有记录，没有升级依赖版本。

扩展分配测试在各压缩模式下覆盖完整预准备消息（含晚到声明）、控制记录、私有记录、附件、记录视图、直接缓冲区读取和随机索引/Metadata/Attachment。异步测试使用可复用源和专用 I/O 线程强制挂起，合计调用线程及工作线程分配，并覆盖内联完成通知下的直接 await。初始化、调用方扩容、自有便利对象和错误仍排除在外；原生快照内存不属于托管分配保证。
