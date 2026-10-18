# orbit-config

客户端与服务共用的配置解析、校验、连接编辑和模型目录。配置文件只保存用户声明，发现结果保存在缓存中。声明和解析后的目录通过只读接口访问，修改声明时同步重建投影。
连接保存先校验完整变更，再提交；后续目录刷新失败返回诊断，不伪装成保存失败。

## 连接与模型选择

模型标识为 `连接名::模型 ID`。`models = "auto"` 跟随服务端目录，数组固定启用项及顺序，空数组不启用模型。仅在需要覆盖能力时填写 `model_overrides`：

```toml
[providers.local]
base_url = "http://localhost:11434/v1"
models = "auto"

[providers.local.model_overrides]
"my-model" = { context_tokens = 32768, supports_vision = false }
```

新会话使用客户端记住的选择，恢复会话使用自身保存的模型。完整配置见 [config.example.toml](../../config.example.toml)。

## 模型目录

`/model refresh` 手动更新目录；自动刷新由 `auto_refresh_models` 控制。各连接独立刷新，缓存绑定凭据、注册身份和地址，结果不回写配置。失败时保留身份匹配的缓存或显式声明；没有目录时不补造可用模型。

完整目录替换旧条目，部分结果只更新已观测项。Google Code Assist 的额度接口仅提供部分模型观测，刷新诊断会说明此限制。目录获取成功但缓存写入失败时，当前结果仍生效并附带诊断。

能力优先级为显式配置、账号或 API 返回值、公共规格。公共规格来自 models.dev，仅补充能力，不授予模型访问权限；Antigravity 的可用项以账号 Agent 列表为准，禁用或下线模型不会由内置规格恢复。

## 入口与验证

[config/](src/config/) 解析配置，[config/connections/](src/config/connections/) 保存连接，[models/](src/models/) 管理目录、缓存和刷新；供应商响应由 orbit-model 解码。

```sh
cargo test -p orbit-config
```

[项目总览](../../README.md)

[directories.rs](src/directories.rs) 统一客户端与服务的本机用户目录。Windows 使用
USERPROFILE、APPDATA 和 LOCALAPPDATA，无需设置 Unix HOME；已有 Unix 路径保持兼容，
显式 XDG 目录覆盖仍可用于隔离配置。
