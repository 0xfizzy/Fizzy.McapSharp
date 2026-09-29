# 原生回归输入

[English](README.md) | 简体中文

此目录保存已确认解析或 FFI 缺陷的缩减 MCAP 输入。PR 的 robustness 套件和原生深度驱动都会先重放全部 `*.mcap`，再运行生成的变异。若回归要求不止是没有崩溃、panic 或非预期错误，还应增加确定性 xUnit 语义断言。

在输入旁记录来源、预期行为和许可证。不保存私有 payload 或超过探针 8 MiB 限制的文件。目录为空不会跳过生成的 robustness 用例。原始失败输入在审查和缩减前保留于 CI artifacts。
