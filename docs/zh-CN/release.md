# 发布与恢复操作规程

[English](../release.md)

## 配置与包发布

为 `publish.yml`、`nuget` 环境和 `NUGET_USER` 配置 NuGet Trusted Publishing。Pages 使用 GitHub Actions，`github-pages` 允许 main 和发布 tag；为 `documentation-update` 配置必要审核。保留 `gh-pages` 的 Git 历史，禁止强制推送。Actions 产物用于传输，不承担长期存储。

修改发布工具前运行 `./scripts/Test-Release.ps1`、完整文档构建及受影响的示例检查。包发布获得授权后，提交版本变更，通过跨平台验证，创建对应 `v<version>` tag，再执行：

```powershell
./scripts/Release.ps1 Check -Tag v<version>
./scripts/Release.ps1 Start -Tag v<version>
./scripts/Release.ps1 Status -Tag v<version>
./scripts/Release.ps1 Resume -Tag v<version>
```

发布工作流验证各平台原生资产和同一包，在 OIDC 推送 NuGet 前将原始候选完整保存到 Release 资产，验证公开包，再部署绑定该包的文档。部署成功后才公开 Release。恢复复用原始资产，不重新打包或覆盖资产；部分上传从原始自包含 bundle 恢复。执行命令本身不替代版本、tag 和发布授权。

## 文档地址与存储

`/dev/` 跟随当前 main，明确标为未发布。`/v<version>/` 保存该包最新文档；更新整体替换目录，已删除页面也会移除。编号文档路径直接删除，不提供重定向。版本选择器仅列开发版和包版本。`/latest/` 选择成功部署的最高稳定语义版本；修改旧版文档不改变该选择。根入口进入 latest，没有稳定版时进入 dev。

全部生产者共享串行 Pages 部署锁。开发版在归档组装前和部署前两次核对远端 main。固定版携带准备时的归档 SHA；归档已变化则拒绝覆盖。候选的原始页面、源码 SHA、运行 ID 和内容哈希保存在 `gh-pages` 的 `.deployments/<run-id>/`，不进入公开站点。组装完整候选站点，仅替换目标版本。Pages 部署及线上身份和全部文件字节验证成功后，才新增成功记录并提交当前站点，不改写历史。失败候选不标记成功。构建或 Pages 部署失败保留原站点；Pages 已成功但线上验证失败时，需要恢复或回滚。

## 更新已有包文档

```powershell
./scripts/Update-Docs.ps1 Prepare -Tag v<version>
# 修改指南、翻译、示例或 XML 注释；审核并提交。
./scripts/Update-Docs.ps1 Check -Tag v<version> -SourceRef <docs-sha>
git push -u origin HEAD
./scripts/Update-Docs.ps1 Start -Tag v<version> -SourceRef <docs-sha>
./scripts/Update-Docs.ps1 Status -Tag v<version>
./scripts/Update-Docs.ps1 Status -Tag v<version> -RunId <run-id>
```

Prepare 要求干净工作区和公开 Release，验证原始包来源，拉取成功归档及其中当前文档源码 SHA，从它创建 `docs/v<version>-update` 分支。`-SourceRef` 可指定源码，`-Branch` 可改分支名。归档基线保存到 artifacts，不推送；应保留到 Start。没有准备状态时，Start 捕获当前归档 SHA。Check 和 Start 要求检出指定源码。Start 使用可信 main 工作流、呈现资源、浏览器测试及固定工具配置处理精确源码 SHA。脚本、呈现器、示例工程和工具配置不在文档更新白名单内。云端门禁使用可信 main 示例工程，仅复制示例 C# 源码到隔离包测试目录。

相对原始包提交，只允许指南、翻译、README/导航、文档示例 C# 源码和 XML 注释变化。拒绝运行时 C#、原生代码、包元数据及无关文件变化。API DLL 来自原始 nupkg；更新 XML 的成员 ID 必须一致。Check 完整构建文档、执行浏览器验证，并使用原包运行示例。文档更新不修改包、tag 或原始 Release 资产。

## 恢复与回滚

```powershell
./scripts/Update-Docs.ps1 Resume -Tag v<version> -RunId <failed-run-id>
./scripts/Update-Docs.ps1 Rollback -Tag v<version> -ArchiveCommit <successful-archive-sha>
```

开发版失败部署使用 `Resume -Tag dev -RunId <failed-run-id>`；仍然拒绝过期 main 源码，并在部署锁内重新核对恢复时的归档基线。

Resume 恢复持久化原始字节并校验哈希，不重建；拒绝已完成运行和被后续归档变化取代的候选。候选尚未持久化时，需要在同一源码 SHA 重跑原构建。Rollback 要求当前历史可达的完整归档 SHA，且包含指定版本成功部署；只恢复该版本，以新提交记录，其他版本保持当前状态。SHA 应选择成功完成提交，而不是候选提交。原始 Release bundle 仍是独立的不可变包恢复来源。

脚本失败返回非零，报告保存在 `artifacts/reports/`。必须检查运行完成状态和线上身份，分派成功不代表部署成功。离线回归覆盖覆盖/删除、其他版本保留、latest、过期拒绝、原字节恢复、成功标记、回滚和原包来源；离线测试不发包。
