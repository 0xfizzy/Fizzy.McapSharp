# 源码构建、测试与消费者集成

[English](development.md) | 简体中文

## 环境与目录

在仓库根目录执行下列 PowerShell 命令。需要 Windows x64、.NET 8 SDK、Rust 1.98.1，以及包含 Windows SDK 的 Visual C++ MSVC 构建工具。工具链声明见 [rust-toolchain.toml](../rust-toolchain.toml)，完整依赖锁定在 `native/Cargo.lock`。

| 路径 | 职责 |
| --- | --- |
| `src/Fizzy.McapSharp/` | 公共 .NET API、P/Invoke 和 SafeHandle |
| `native/src/lib.rs` | Rust MCAP 封装、私有 C ABI、文件映射与校验 |
| `tests/Fizzy.McapSharp.Tests/` | 托管功能与资源生命周期测试 |
| `tests/Interop/`、`tests/interop.py` | .NET 与官方 Python MCAP 双向互操作 |
| `scripts/` | 构建、打包与隔离包测试 |
| `.github/workflows/` | CI 构建与手动发布 |

`Build.ps1` 优先使用仓库 `.tools/cargo/bin/cargo.exe`，存在时设置对应 CARGO_HOME/RUSTUP_HOME；否则使用 PATH 中的 Cargo。脚本不安装工具。`.tools/` 为忽略目录，不提交本机工具。

## 构建与测试

```powershell
./scripts/Build.ps1 -Test -Pack
./scripts/Test-Package.ps1
```

`Build.ps1` 始终先执行 `cargo build --release --locked`，再构建托管 Release 项目。`-Test` 运行 xUnit，`-Pack` 输出 NuGet 包到 `artifacts/packages/`；省略开关只构建。原生 DLL 位于 `native/target/release/fizzy_mcap_native.dll`，托管项目会复制它。脚本没有显式 `--target`，应在 Windows x64 MSVC 环境运行，不要将不同目标的输出混用。

`Test-Package.ps1` 在 `artifacts/smoke-<GUID>/` 建立独立应用与包缓存，从指定目录还原脚本中配置版本的包，验证原生加载和消息往返；可用 `-PackageDirectory` 指定包目录。它验证本地构建包，不代表 NuGet.org 上的包已验证。

现有 xUnit 覆盖三种压缩、时间与 Topic 过滤、元数据与附件、无索引读取、未完成文件、CRC 损坏、恢复结果、写入失败终止、不覆盖文件、枚举释放与无消息声明。测试使用临时文件，无需设备。

### Python 双向互操作

在准备好 Python 的环境中执行（CI 使用 Python 3.12）：

```powershell
python -m pip install mcap==1.3.1 lz4==4.4.5 zstandard==0.25.0
$interopDirectory = Join-Path 'artifacts' ('interop-' + [Guid]::NewGuid().ToString('N'))
dotnet run --project tests/Interop -c Release -- write $interopDirectory
python tests/interop.py $interopDirectory
dotnet run --project tests/Interop -c Release -- read $interopDirectory
```

顺序不能交换：先由 .NET 写文件，再由 Python 校验并生成文件，最后由 .NET 校验 Python 输出。覆盖 None、Lz4、Zstd，以及查询、元数据和附件。每次使用新目录，因为 Writer 不覆盖已有文件。`Build.ps1 -Test` 本身不包含互操作测试。

## 包内容

包 ID 为 `Fizzy.McapSharp`，目标框架为 net8.0。包包含托管程序集、README、第三方声明及 `runtimes/win-x64/native/fizzy_mcap_native.dll`。原生 DLL 不存在时，项目的 `RequireNativeForPackage` 目标会阻止打包；它只检查文件存在，因此打包前必须从当前源码构建原生 DLL。

输出目录为 `artifacts/packages/`，本地脚本不发布包。包版本由项目声明，但不代表该版本已在 NuGet.org 上可用。维护者发布规则见 [AGENTS.md](../AGENTS.md#publishing)。

## 消费者集成

默认使用 `PackageReference`。源码调试由消费者的 MSBuild 条件配置选择引用：

- `UseFizzyMcapSharpSource=true` 时使用指向本仓库项目的 `ProjectReference`，根路径由 `FizzyMcapSharpRoot` 提供。
- 关闭源码模式时使用 `PackageReference`，同一项目不得同时引用两者。
- 这些属性是消费者集成约定，本库项目不会自行替消费者切换引用。

源码模式先运行本仓库 `./scripts/Build.ps1`，再 restore/build 消费者。托管项目会将已有的原生 DLL 传播到源码消费者输出；直接 `dotnet build` 不会编译 Rust。使用当前 checkout，不擅自拉取或切换分支；切换 Source/Package 模式后重新 restore，共享输出串行构建。本机路径和切换配置不提交。

在联合工作区中，API 或行为变化还应验证 RobotController、Parallax 的 Source 构建及相关测试，具体入口以父工作区 README 为准；独立使用本仓库不要求这些消费者存在。报告需区分源码、本地包和已发布包的验证范围。
