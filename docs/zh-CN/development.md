# 源码构建、测试与消费者集成

[English](../en/development.md) | 简体中文

面向源码贡献者：准备工具链、构建本机库、验证完整包，并为消费项目选择引用方式。仅使用 NuGet 的应用开发者请从[入门教程](usage.md)开始。

## 环境与目录

在仓库根目录执行命令。需要 Python 3.12、.NET SDK 10.0.401、Rust 1.98.1 和原生编译器。Windows x64 使用包含 Windows SDK 的 Visual C++ MSVC 构建工具；Linux x64/ARM64 使用 Ubuntu 22.04、GCC/build-essential 和 binutils，并安装下表对应的 Rust target。Windows 本地开发不需要虚拟机、WSL、Docker 或交叉编译器。工具链声明见 [rust-toolchain.toml](../../rust-toolchain.toml)，完整依赖锁定在 `native/Cargo.lock`。

SDK 由 `global.json` 选择（`10.0.401`、`latestFeature`、禁止预览版）。验证需要同时安装 .NET 8 与 .NET 10 运行时。核心 xUnit 与分配门禁按框架串行运行，测试结果分别保存。辅助工具保持 net8.0；示例验证覆盖两个框架。还原使用已提交的 `packages.lock.json` 并启用锁定模式；有意更新依赖时运行 `dotnet restore <project> -p:RestoreLockedMode=false --force-evaluate`，审阅锁文件差异，再运行 `dotnet restore <project> --locked-mode`。

| 路径 | 职责 |
| --- | --- |
| `src/Fizzy.McapSharp/Fizzy.McapSharp.csproj` | 库项目与包元数据；通过 SDK 默认规则编译项目目录中的源码 |
| `src/Fizzy.McapSharp/` | 公共 .NET API；能力 partial 按所属类型命名，Native 互操作声明独立组织 |
| `src/Fizzy.McapSharp/McapWriterOptions.cs`、`McapMessageFilter.cs`、`McapQuery.cs`、`McapReadContracts.cs` | 写入配置、消息筛选与具体读取结果 |
| `src/Fizzy.McapSharp/McapDeclarations.cs`、`McapMessageModels.cs`、`McapDataRecords.cs`、`McapSummaryModels.cs` | 声明、消息、数据记录与摘要／索引模型；不依赖具体 reader 实现 |
| `native/src/lib.rs`、`protocol.rs` | 共享 ABI／错误边界及具名协议数值 |
| `native/src/reader.rs`、`writer.rs`、`summary.rs`、`io.rs` | 文件／Stream 读取会话与查询、writer 状态、摘要序列化及源／回调 I/O |
| `native/src/engine.rs`、`snapshot.rs`、`prepared_write.rs`、`record_access.rs` | Sans-I/O 引擎、独立索引快照、预准备写入和原始记录工具 |
| `tests/Fizzy.McapSharp.Tests/` | 托管功能与资源生命周期测试 |
| `tests/Interop/`、`tests/interop.py` | .NET 与官方 Python MCAP 双向互操作 |
| `scripts/` | 构建、打包与隔离包测试 |
| `.github/workflows/` | CI 构建与手动发布 |

`Build.ps1` 调用的 `build.py` 优先使用仓库 `.tools/cargo/bin/cargo.exe`，存在时设置对应 CARGO_HOME/RUSTUP_HOME；否则使用 PATH 中的 Cargo。脚本不安装工具。`.tools/` 为忽略目录，不提交本机工具。

## 构建与测试

文档构建、翻译与示例遵循[文档维护](documentation.md)，版本归档、发布与恢复遵循[发布 SOP](release.md)。公开 API/XML 和指南变更须运行 `./scripts/Build-Docs.ps1`；发布及归档工具变更须运行 `./scripts/Test-Release.ps1`。这些门禁补充而不替代下述 native 及包检查。

公开 API 的 XML 文档由 CS1591 编译门禁要求；在相关成员上说明单位、所有权、适用模式和失败行为。Python 测试同时核对 `src/Fizzy.McapSharp/Native.Protocol.cs` 与 `native/src/protocol.rs` 的私有协议常量；即使数值相同，也保持不同操作域和状态域独立。

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

构建脚本先执行 `cargo build --release --locked --target <target>`，再构建托管 Release 项目。原生产物按目标隔离在 `build/obj/native/<target>/release/`，并暂存到 `artifacts/native/<rid>/`，清单记录提交、源码指纹、目标和二进制哈希。`-Test` 运行构建/打包回归检查和 xUnit，默认只构建宿主平台。脚本不安装工具。

Linux 构建检查 ELF 架构、动态依赖和 GLIBC 符号版本（不得高于 2.35），拒绝外部压缩库依赖和嵌入的库搜索路径，并拒绝自定义 CPU 优化的 Rust 标志。Linux ARM64 使用原生 ARM64 runner 编译和测试。macOS、musl、Ubuntu 20.04、Windows ARM64、32 位、NativeAOT 和单文件发布不属于验证矩阵。

`Test-Package.ps1` 调用 `test_package.py`，在 `artifacts/smoke-<GUID>/` 建立独立应用与包缓存。从项目元数据读取版本，检查所有原生资产的架构，仅从指定包目录还原，并验证普通运行和显式 RID 的 framework-dependent publish 后运行，均覆盖全部压缩模式。可通过 `-PackageDirectory` 或 `--package-directory` 指定包目录。这验证本地包，不代表 NuGet.org 上的包已验证。 检查两个目标的 DLL/XML；隔离消费者分别在 .NET 8 与 .NET 10 运行，核对 NuGet 编译/运行时资产选择，并逐字节比较输出 DLL 与对应包条目。

现有 xUnit 覆盖三种压缩、时间与 Topic 过滤、元数据与附件、无索引读取、未完成文件、CRC 损坏、恢复结果、写入失败终止、不覆盖文件、枚举释放与无消息声明。测试使用临时文件，无需设备。

## 包内容

包 ID 为 `Fizzy.McapSharp`，目标框架为 `net10.0` 和 `net8.0`。同一包包含两个框架的托管程序集与 XML 文档、README、官方 MCAP 图标及其 MIT 许可证、第三方声明及以下资产：

```text
runtimes/win-x64/native/fizzy_mcap_native.dll
runtimes/linux-x64/native/libfizzy_mcap_native.so
runtimes/linux-arm64/native/libfizzy_mcap_native.so
```

`RequireNativeForPackage` 拒绝资产或清单缺失，以及架构、二进制哈希、提交或源码指纹不匹配的情况。本地 `-Pack` 同样要求三平台资产齐全。从一次匹配的 CI 运行下载各个 `native-<rid>` artifact 到 `artifacts/native/<rid>/`，保留清单；不要混用不同运行的资产，收集后不要修改源码。源码指纹会归一化 Windows/Linux 文本换行。输出目录为 `artifacts/packages/`，脚本不发布或修改版本。

### CI 与发布验证

可复用验证工作流在三个原生 runner 上构建，运行 xUnit 和 Python 双向互操作，再统一打包一次。五个包验证任务分别在 Windows 2022、Ubuntu 22.04/24.04 x64/ARM64 上执行，使用独立缓存还原完整包，验证普通运行和 RID 发布运行，并读取所有构建平台生成的样例；缺少样例即失败。Ubuntu 是已配置的验证发行版；其他 glibc 发行版还需具备兼容系统库和 .NET 8 / 10。

PR、推送到 `main` 和手动触发均运行该流程，同一 PR/ref 的旧构建自动取消。中间产物保留一天，只有通过全部任务的完整包保留七天。Cargo 缓存按系统、架构、工具链、锁文件和原生源码区分。只使用标准托管 runner，不配置付费 larger runner 或额外缓存额度。按 GitHub 当前规则，公开仓库的标准 runner 运行时间免费，存储仍受账户额度约束。

手动发布工作流运行相同验证，将已验证的同一包交给受保护的发布任务，不重新打包。推包前持久保存包和对应文档；随后独立验证公开 NuGet 源上的包（三个 RID），再部署版本归档。Resume 复用原始已验证候选产物。操作遵循[发布 SOP](release.md)和 [AGENTS.md](../../AGENTS.md#publishing)。普通 build CI 仍仅验证本地候选包，不证明 NuGet.org 可用。

## 消费者集成

默认使用 `PackageReference`。本地源码开发时，消费项目可以改用指向本仓库项目的 `ProjectReference`。同一项目只能选择一种引用类型。引用选择属性、路径和构建入口由消费项目定义，本库不配置这些内容。

源码模式先运行本仓库 `./scripts/Build.ps1`，再 restore/build 消费者。托管项目优先使用显式 `RuntimeIdentifier`，否则使用 SDK 宿主 RID，只将对应的已有原生库传播到源码消费者输出；直接 `dotnet build` 不会编译 Rust。使用当前 checkout，不擅自拉取或切换分支；切换 Source/Package 模式后重新 restore，共享输出串行构建。本机路径和切换配置不提交。

报告需区分源码、本地包和已发布包的验证范围。测试已发布依赖不能验证当前源码。

## 测试专题入口

基本构建之后，按变更范围选择[验证与诊断](testing.md)。

- [分配验收](testing.md#分配验收)
- [单文件 writer 内存诊断](testing.md#单文件-writer-内存诊断)
- [Python 双向互操作](testing.md#python-双向互操作)
- [官方 API 与扩展分配门禁](testing.md#官方-api-与扩展分配门禁)
- [外部契约测试套件](testing.md#外部契约测试套件)
- [每周及手动深度检查](testing.md#每周及手动深度检查)
- [原生存储与补丁验证](testing.md#原生存储与补丁验证)

## 构建输出布局

应用和库输出统一位于本仓库 `build/Debug/` 或 `build/Release/`。独立库、工具按项目分目录，必要时继续按目标框架或 RID 隔离。测试程序及夹具位于 `build/<Configuration>/tests/<Project>/`，测试结果位于该项目的 `TestResults/` 子目录。中间文件及还原缓存位于 `build/obj/`，按项目和配置隔离。源码依赖使用各自仓库的输出根目录。

共享输出的构建串行执行。`artifacts/` 用于临时日志、报告、包暂存和发布校验。旧项目内 `bin/`、`obj/` 已排除在编译输入之外，仅在确认不再使用后清理。

## 运行仓库示例

[教程源码](../../samples/Documentation/QuickStartExamples.cs)验证入门教程的输出并清理临时文件；[示例入口](../../samples/Documentation/Program.cs)还运行写入、读取、转发及校验示例。源码贡献者可在仓库根目录运行 `./scripts/Test-Samples.ps1`，脚本先构建本机原生库，再验证当前源码。已有经过验证的完整包可用 `./scripts/Test-Samples.ps1 -PackageDirectory artifacts/packages` 检查；它不代表公开 NuGet 包验证。环境准备见[开发指南](development.md)。
