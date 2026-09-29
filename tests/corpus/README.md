# Native regression inputs

English | [简体中文](README.zh-CN.md)

Keep minimized MCAP inputs from confirmed parser/FFI defects here. Both the PR
robustness suite and the native deep driver replay every `*.mcap` file before
generated mutations. Add a deterministic semantic assertion in xUnit when the
regression requires more than absence of crashes, panics or unexpected errors.

Document each input's origin, expected behavior and license alongside it. Do not
store private payloads or files larger than the 8 MiB probe limit. An empty corpus
does not skip generated robustness cases. Original failing inputs stay in CI
artifacts until they have been reviewed and reduced.
