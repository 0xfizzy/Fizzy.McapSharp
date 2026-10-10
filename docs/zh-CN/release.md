# 发布与恢复 SOP

[English](../release.md) | 简体中文

本 SOP 面向获授权的维护者。从下一次发版开始，每个包保留原始双语指南和 API 参考，不自动补建历史版本。版本修改、提交、tag、推送、远程设置和发布均需要授权；本地检查不代表授权发布。

## 一次性仓库设置

1. 为本仓库、`publish.yml` 和 `nuget` 环境配置 NuGet Trusted Publisher，设置 `NUGET_USER`，不保存长期 NuGet key；配置预期的发布审批。
2. Settings → Pages → Build and deployment 选择 GitHub Actions。`github-pages` 允许 main、发布 tag `v*` 和明确批准的文档修订分支。`documentation-revision` 配置必需审阅者及允许的修订分支。
3. main 要求原有 build 和文档检查；禁止更新或删除发布 tag。`gh-pages` 禁止删除及强推，同时允许部署身份普通提交，不设置机器人无法满足的仅 PR 更新规则。
4. 按作业授予权限：构建只读；Release 归档和站点分支写入使用 contents write；Pages 部署使用 pages write 和 OIDC；NuGet 登录使用 OIDC。PR 不部署。
5. 本地编排需 `gh auth status` 成功，并具备 Python 3.12、Node.js 22、.NET 8 和固定 Rust 工具链，见[文档维护](documentation.md)。
6. 运行发布回归门禁、预览文档并验证首次 Pages 部署。验证作业证据随候选产物持久保存，Actions 历史过期不会阻止长期归档恢复。仅远程 ref 查询成功且确认分支不存在时初始化归档；认证及网络错误不能当作空归档。

## 每次发布

1. 经授权同步 managed 和 native Cargo 版本元数据，保留精确依赖及锁文件。native 标识行为若变化需检查；当前 writer 使用上游 library identifier。
2. 同步英文、中文、XML 及相关示例。运行文档、示例和发布门禁，并执行适用的当前源码及包验证；完整包需要三个平台同源 native 资产。
3. 经授权提交并推送 main，等待该提交的正常 build 和文档检查成功。在该提交创建并推送获授权的 `v<version>` tag，不移动已发布 tag。
4. 在该 tag 对应的干净 checkout 运行：

```powershell
./scripts/Release.ps1 Check -Tag v<version>
./scripts/Release.ps1 Start -Tag v<version>
./scripts/Release.ps1 Status -Tag v<version>
```

用元数据版本替换占位符。Check 核对干净源码、本地及远程 tag、版本一致性、main 祖先关系和对应提交正常构建成功。Start 重复检查并从 tag 触发 `publish.yml`，不在本机推包。触发成功不等于发布成功，需查看 Actions 和最终 Status。

工作流运行 deep 验证，汇集三个 native 平台，打包一次并在五种环境测试同一包。随后从包的 DLL/XML 生成文档、执行示例，在推 NuGet **之前**保存到草稿 Release。OIDC 发布使用同一个 nupkg。核对远程内容后，在三个支持 RID 上用公开 NuGet 和隔离缓存验证，再归档部署站点、检查线上资源，最后公开 Release。

候选产物包括 nupkg、docs.zip、validation.zip、validation-jobs.json 和 provenance.json。来源清单记录 tag、源码提交及指纹、native 哈希、包及文档哈希、完整站点文件哈希、工具版本及原始工作流。NuGet 可能添加仓库签名，因此比较包身份和除签名外的 ZIP 内容，不能只比较整个 nupkg 哈希。

## 版本存储与验收

`/dev/` 跟随 main 并标记未发布。`/v<version>/r0/` 为冻结原件，后续 `rN` 为不可变修订；`/v<version>/` 选择有效修订。`/latest/` 选择语义版本最高的稳定版，预发布和旧版本重试不降级 latest。根目录进入 latest，首个稳定版之前进入 dev。URL 版本来自包元数据。

`gh-pages` 保存完整站点历史，Release 资产提供独立持久恢复源；有过期时间的 Actions artifacts 不作为归档。所有 Pages 生产者共用串行部署入口，重新读取最新归档树、检查历史哈希，通过非强推和有界冲突重试更新。不得将旧完整站点覆盖当前归档。

只有 Actions 所有启用门禁成功、公开 NuGet 能恢复匹配包、双语及代表性 API 页面正常、项目路径下搜索和资源正常、版本及源码标识匹配、原始归档可下载后，才算发布完成。本地包测试不代表已发布包验证；工作流不验证消费者硬件或 UI。

## 失败恢复

脚本失败返回非零，并在 `artifacts/reports/` 写 JSON。诊断时保留工作流/run 身份，不输出凭据。恢复参数不允许跳过验证。

| 故障 | 操作 |
| --- | --- |
| 上传前验证失败 | 修复原因，不绕过门禁；源码变化后在发布前建立新的经审阅提交/tag。 |
| 候选资产部分上传 | Resume 通过 candidate.json 从原 run 的 release-bundle 取回缺失资产并完成上传。已有资产必须完全相同；原临时产物在持久化完成前过期时停止，由维护者审阅，不重新生成或覆盖冲突资产。 |
| 推包响应丢失或超时 | 先查 NuGet；内容匹配则继续，不存在才可重试，内容冲突阻断。 |
| NuGet 可见性超时 | 稍后在同一 tag Resume，每次轮询都有上限。 |
| NuGet 成功，包检查或 Pages 失败 | 修复基础设施或权限后 Resume，复用候选包，不重新打包或盲推重复包。 |
| Actions artifacts 过期 | 从草稿或公开 Release 恢复，核对来源清单和持久保存的原始成功验证作业证据。 |
| 归档提交后 Pages 失败 | Resume 部署最新完整归档，保留之后增加的版本。 |
| 已发布包错误 | 通过完整流程发布新修正版，不替换旧版本字节。 |

```powershell
./scripts/Release.ps1 Status -Tag v<version>
./scripts/Release.ps1 Resume -Tag v<version>
```

Resume 触发前核验候选包和远程 tag。candidate.json 将部分上传绑定到原始 run；Resume 从原 bundle 补齐上传并核验 provenance。缺少原始记录或输入字节、无法证明原始验证成功时阻断，不制造证据。部分上传且原始输入不可恢复时，保留证据并由维护者审阅处理，不能把新构建冒充原始产物。

## 文档修订与回退

使用脚本入口，不升级版本、不创建 tag、不发布包：

```powershell
./scripts/Revision-Docs.ps1 Prepare -Tag v<version>
# 在返回的修订分支修改、审阅并提交。
./scripts/Revision-Docs.ps1 Check -Tag v<version> -Revision <N>
# 获授权后推送已审阅分支。
./scripts/Revision-Docs.ps1 Start -Tag v<version> -Revision <N>
./scripts/Revision-Docs.ps1 Status -Tag v<version> -Revision <N>
./scripts/Revision-Docs.ps1 Resume -Tag v<version> -Revision <N>
./scripts/Revision-Docs.ps1 Rollback -Tag v<version> -Revision <previous-N>
```

Prepare 要求干净工作区、公开 Release 和匹配的本地/远程 tag 来源，选择下一个未占用修订号，核对当前有效修订的持久来源和线上身份，从该修订的文档提交创建分支以保留先前修正（本地缺少已验证提交时会从 origin 获取该确切 SHA，运行时代码差异仍对比原始包提交）；`-Branch` 可指定分支名，不自动 push。只允许指南、README/导航、示例、站点资源和 XML 注释行变化；拒绝可执行 C#（包括字符串内容）、包元数据及其他文件变化。API 程序集来自原始包，修订 XML 成员 ID 必须一致。Check 运行完整文档/浏览器及原包示例门禁，但不上传修订产物。

Start 核对干净提交分支与远程一致，再触发 docs-revision；documentation-revision 环境审批仍适用。工作流首先保存包含 `docs-rN.zip` 和 `provenance-rN.json` 的自包含 `revision-rN.zip`，然后保存兼容的独立附件，先部署不可变修订并验证固定地址，然后标记完成、激活并验证版本/latest 入口。已有修订内容必须完全一致，旧版本重试不会降低有效指针。Status 区分完整、部分、尚未上传并显示线上状态和最近工作流；触发成功不代表部署成功。

Resume 通过可信 main 恢复持久修订字节并完成激活，不重建、不降低更新的指针。自包含修订 bundle 也能恢复独立附件的部分上传。修订持久保存前失败时，应在同一提交重跑原始修订工作流，仍拒绝内容冲突。Rollback 只指向已验证的已有修订，不删除历史。部分上传且原始产物不可恢复时须维护者审阅，不能用新构建替换同一身份。

完成标记保存在不可变修订目录之外。所有正式部署先验证固定地址，再激活入口并二次部署验证；固定地址验证失败不会推进有效指针。dev 在部署锁内于归档前及实际部署前检查远程 main，拒绝过期产物。

## 回归和恢复演练

发布及归档逻辑变更前运行 `./scripts/Test-Release.ps1`。测试使用 artifacts 下的临时目录、模拟包和服务，不发布到真实 NuGet；覆盖多版本、不可变修订、dev 更新、预发布/latest、冲突拒绝、危险 ZIP、仓库签名比较及有界可用性重试。工具或内容变化后执行完整文档及相关示例门禁。

站点丢失时先保存幸存分支和 Release 资产，从可信 main 工作流运行 Actions → docs-restore，选择已有 tag 和当前指针丢失时要启用的修订。脚本恢复该 tag 全部原始及修订资产，核验文件清单，合并到最新归档，保留已有有效指针；对各缺失版本重复。只读本地演练运行 `python scripts/release.py recover-site --tag v<version> --revision 0`，重建树位于 `artifacts/recovered-archive`。不能将单个旧完整站点覆盖更新的线上树。仓库管理员负责访问控制、Release 保留及远程保护设置。

自包含 `release-bundle.zip` 保存 nupkg、docs.zip、validation.zip、validation-jobs.json 和 provenance.json，先于兼容的独立附件上传。恢复优先使用它并核验所有内容，不依赖 Actions 产物；该 bundle 尚未保存时，candidate.json 才用于定位原始 Actions bundle 补齐上传。
