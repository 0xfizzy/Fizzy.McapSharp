# 文档构建与维护

[English](../documentation.md) | 简体中文

成员契约放在英文 XML 注释中，跨 API 契约维护在 [api.md](api.md)，任务流程放在[使用指南](usage.md)。对照实现审阅约束、单位、所有权、取消、失败及清理；XML 覆盖不等于语义正确。

## 本地流程

使用 Python 3.12、PowerShell 7、Node.js 22、.NET 8 SDK、固定 Rust 工具链和[开发指南](development.md)指定的本机编译器。DocFX 由本地工具清单固定版本，`dotnet tool restore` 只恢复该工具，不安装全局工具。

```powershell
./scripts/Test-Documentation.ps1
./scripts/Build-Docs.ps1
./scripts/Test-Samples.ps1
./scripts/Serve-Docs.ps1 -Port 8087
```

Build-Docs 先构建 native Release，再构建 managed Release、检查 XML、生成 API 页面、校验站点并运行固定版本 Chromium 冒烟门禁。共享输出目录的构建必须串行。Serve-Docs 支持 `-SiteDirectory`，通过本机 HTTP 在 `/Fizzy.McapSharp/dev/` 前缀下提供已有站点，Ctrl+C 停止；不要使用 file URL。检查双语页面、导航、搜索及代表性 API 页面。

已有验证通过的完整包时，`Build-Docs.ps1 -Package <nupkg绝对路径> -Revision 0 -ReleaseCommit <sha>` 使用其 DLL/XML。包版本必须匹配项目元数据；这是候选包文档，不代表已发布包验证。比较候选包与公开 NuGet 并执行隔离的公开源包测试，运行 `./scripts/Test-Package.ps1 -PackageDirectory artifacts/release -Published`。

## 翻译与变更

英文指南位于 `docs/`，同名中文指南位于 `docs/zh-CN/`，分别维护导航和语言切换链接；根 README 对应 `README.zh-CN.md`。人工复核译文后显式接受英文修订：

```powershell
./scripts/Test-Documentation.ps1 -AcceptTranslation docs/usage.md
```

规范化源文件哈希记录复核状态，不衡量翻译质量；不得未经复核就接受哈希。双语页面共用示例源码和英文 API 参考。

公开及受保护 API 声明、XML、指南、翻译、README、导航、模板及文档工具变化必须运行完整文档构建。内部变更仅在影响生成参考或文档契约时需要。示例代码或相关 API 变化后编译执行示例；发布、归档、修订或恢复逻辑变化后运行 `Test-Release.ps1`。文档检查不替代 native、分配、互操作及包测试。

## 产物与诊断

API YAML、站点、docs.zip、文件哈希及成员清单位于 `artifacts/docs/`，报告位于 `artifacts/reports/`；失败返回非零退出码。生成 HTML 不提交到源码分支。源检查发现翻译漂移和缺失文件；最终 HTML 检查验证链接、锚点、搜索索引及项目路径安全。Chromium 验证双语与 API 页面加载、语言切换、一个 API 搜索结果、版本切换、缺页语言回退、页面脚本错误及必要资源，输出 JSON 结果，仅失败时保存截图；静态检查覆盖全站链接与锚点。

文档工作流校验 PR，仅从 main 更新 `/dev/`，不发布 NuGet。在部署锁内，归档前和部署前均核对产物提交与远程 main，明确拒绝过期构建。不可变版本归档、远程配置、修订和恢复见[发布 SOP](release.md)。
