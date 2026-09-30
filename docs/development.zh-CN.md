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


分类枚举验收在三种压缩模式下比较 32 条与 4096 条固定大小的无关消息，目标记录集合保持不变。测量覆盖完整枚举，允许的额外托管分配最多为 4096 字节，以拒绝逐记录分配增长；自有结果不要求零分配。

完成测试区分普通刷新与显式文件同步，覆盖保留句柄的所有权及同步失败后的终止状态，使用仅测试启用的原生故障注入和 FileStream 重写。测试验证调用路径与失败契约，不证明断电持久性。ABI 变化后须从相同源码重建三个平台的原生资产，再验证完整包。

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

原生差分测试在所有压缩模式下对比六种切片模式与锁定上游实现。惰性读取检查断言构造时未推进、最多保留一条待交付记录；状态及容量检查排除输入副本，不代表完整分配器或解压器内存测量。托管测试覆盖独立 Chunk 生命周期、延迟错误、调用者索引和禁止缓存排序。Release 分配门禁还测量独立惰性 Chunk 推进。

### 外部契约测试套件

维护者验证互操作和错误处理时，应先构建本机原生 Release 库，安装互操作章节列出的 Python 依赖；conformance 另需 Node 24。每次使用新的输出目录，保留失败数据以便复现。

```powershell
python scripts/test_suites.py conformance --output artifacts/check-conformance
python scripts/test_abi.py
python scripts/test_suites.py differential --no-build --seed 1 --samples 32 --output artifacts/check-differential
python scripts/test_suites.py robustness --no-build --seed 1 --samples 32 --output artifacts/check-robustness
python scripts/test_suites.py stress --no-build --budget 10 --output artifacts/check-stress
```

第一条命令构建托管契约执行器；`--no-build` 复用已有构建，两者均不构建 Rust。Conformance 下载 `tests/conformance-lock.json` 固定的提交，验证归档及每个 Git LFS 对象的 SHA-256，保留上游 MIT 许可证。Node 直接导入官方预期结果和 Rust 支持规则，无需构建上游多语言工程或安装 npm 依赖。清单要求 416 次顺序读取、32 次索引读取及 208 次逐字节写入比较。其余 384 个索引用例缺少官方 Rust runner 要求的前提；208 个带 padding 的写入变体无法由上游 Writer 生成。不支持项记录具体原因；数量变化、数据缺失和应支持项失败均阻断验证。

差分测试采用固定 xorshift32 算法，32 个固定种子、三种压缩设置及 .NET/Python 两个写入端。每个文件最多 256 条消息，限制为 8 MiB。检查 payload、声明、metadata、附件、顺序/缓冲/异步读取、索引排序与随机访问。跨 chunk 的相同时间戳顺序未规定：结果必须时间单调且同时间组内消息完全匹配。奇数种子关闭 chunk，偶数种子覆盖压缩和索引。xUnit 还覆盖小文件的每个截断位置、极值字段、长度限制、短读、I/O 故障注入和会话隔离；原有异步取消及所有权测试继续保留。

Robustness 从合法文件生成变异，每个解析探针在独立进程中运行，超时为 30 秒。预期 MCAP 错误可以接受，非预期异常、panic、崩溃和超时均失败；严格解析设置 8 MiB 记录长度限制。`--samples` 控制用例数，正数 `--budget` 则改为按秒运行。`report.json` 保存配置、提交、RID、运行时版本和当前输入。按记录的种子和配置重放时必须换用新输出目录，不直接复用报告中的旧目录。

`test_abi.py` 核对所有托管 P/Invoke 的原生导出名，并在独立进程中用注入的缺失库解析器及测试专用的不兼容 Rust 库检查拒绝行为。伪库仅位于被忽略的 artifacts 中，不进入包资产。

### 每周及手动深度检查

PR 和 main 提交在三个原生平台运行固定套件。每周日 02:00 UTC 额外运行 Linux x64 原生变异 20 分钟、Valgrind 10 分钟、生命周期压力 10 分钟和真实超过 4 GiB 的文件检查。手动触发支持 `deep`、`seed`、变异 `budget`（1–1800 秒）。发布要求同提交的深度检查通过，仍发布同一个候选包。定时运行以 workflow 运行编号作为轮换种子并记录。没有每日任务；托管定时任务实际启动时间可能延后。

Linux x64 已安装固定 Rust 工具链、.NET、Valgrind 并完成原生库构建后运行：

```sh
python scripts/test_deep.py --seed 1 --budget 1200 --valgrind-budget 600 --stress-budget 600 --output artifacts/deep-check
```

独立 Rust 驱动链接真实私有 C ABI，检查布局，并以有效句柄覆盖记录/chunk 解析、待交付缓冲读取、快照和索引操作。原生变异子进程限制 1 GiB 地址空间及 30 秒执行时间。崩溃时保留原输入并尝试有预算的差分缩减，不保证获得全局最小样本。Valgrind 拒绝非法内存访问及确定/间接泄漏。这是变异测试，不是覆盖率引导的 fuzz，也不构成内存安全证明。

生命周期压力在 GC 后采样私有内存和句柄；至少 30 个样本时丢弃前 1/3，比较中段及末段中位数。增长超过 32 个句柄或 256 MiB 时失败并要求排查；更小增长仅供诊断，原生分配器缓存也可能保留内存。大文件检查用有界缓冲写入 4097 MiB 未压缩 payload，完整校验并通过索引查询尾部，结束后删除文件。至少预留 6 GiB 磁盘空间；不要求自有快照 API 恒定内存。

报告保留 7 天，失败输入保留 14 天，成功的压力文件不上传。分配仍严格要求 0 B，吞吐不设置托管 runner 硬门槛；深度 job 超时 90 分钟。先检查失败报告并重放输入，再考虑修改预期。

`validate / verified` 是最终门禁，要求所有启用的 job 成功，包括发布启用的深度检查；建议将该检查设为分支保护必需项。取消或意外跳过不能生成已验证包。包任务只从候选 nupkg 恢复到隔离缓存，除普通/RID 发布 smoke 测试外，还针对包编译共享公共契约执行器，并用 `python scripts/test_package.py --fixtures artifacts/exchanged` 读取全部 18 个跨平台文件。此阶段不下载源码原生资产，也不使用源码 ProjectReference。仍需三个平台同源资产，本机单平台构建不能替代包验证。


### 原生内存验收

Release 原生测试包含仅用于测试的线程局部 Rust 分配计数器，以 None/Lz4/Zstd 运行固定的 4096 条、每条 1 KiB 消息用例（`memory_read_baseline`）。统计推进阶段申请／扩容次数及累计请求字节数，不含初始化和完整输入副本构造；包含上游 Rust 申请，不包含外部压缩库内部申请。使用 `--nocapture` 运行并将前后结果保存至忽略的 artifacts。门禁拒绝恢复为逐消息分配，并独立要求目标充足时封装交付缓冲不分配。受控容量峰值／复制计数用于补充，不替代操作系统工作集分析。

内存测试覆盖容量边界、重试复用、保留缓冲释放、映射子游标生命周期、原始尾部字节保留、快照位置恢复、排序描述／大 payload 和异步直接交付。托管 Release 门禁还要求映射游标读取及统计查询精确为 0 B。跨平台发布仍需 Linux 深度／Valgrind 检查及三个 RID 资产。

原生探针还覆盖 8192 条消息写入，组合可寻址／缓冲输出、开启／关闭索引及有限／无限 Chunk，以及重复随机读取、回退排序 arena 和时间重叠的索引 Chunk。缓存命中测试要求不再推进解析器。托管门禁包含映射 BufferReader、缓存随机读取及可复用随机记录 scratch。

进程内存诊断使用 `dotnet run --project tests/Allocations -c Release -- memory-profile 65536 > artifacts/memory-profile.jsonl`，条数至少 8192。各写入配置每 8192 条及完成／释放后采样，分别记录 Private Bytes、工作集、托管堆和耗时。输出写入计数 sink，排除录制文件存储成本。采样不等于精确原生活跃字节或峰值，分配器缓存及之前场景会影响后续结果。增加条数可延长运行，测量结果保留在忽略的 artifacts 中。

`DeliveryOptimizationTests` 覆盖预编译 Chunk 索引和同步自有结果交付。Release 便利读取门禁检查最终 payload 数组与结果对象开销，并要求 prepared 大索引调用为 0 B 托管分配。`prepared_index_cached_calls_allocate_nothing` 单独要求所有压缩模式下预热后的 prepared 缓存命中为零 Rust 分配，不包含描述符构造或外部库分配。重试测试检查保留容量与准确的输出复制增量。

## Vendor 存储补丁维护

native/vendor/mcap 保存固定 mcap 0.25.0 源码及官方 MIT 许可证。Cargo patch 选择该源码，保留 Cargo.lock 并使用 --locked。UPSTREAM.json 记录原始文件 SHA-256 与许可证来源，PATCHES.json 记录核验后的本地修改／新增文件指纹。修改后审查差异再更新补丁指纹；不要重新生成原始清单掩盖变更。`python scripts/check_vendor.py` 在 native 构建前检查文件集合、指纹和许可证。跨平台构建源码指纹包含 vendor。

`Build.ps1 -Test` 包含 BatchGate 的 borrowed／ReadBatch／WriteBatch 零托管分配门禁和 10,000 批次保留测试；完整内存验收仍需大载荷／边界矩阵、codec C 分配与进程 private bytes、三平台原生 runner 证据。域统计不能替代全分配器测量。Python 双向互操作、消费者 Source 验证和同源三 RID 打包仍按上文独立执行。
