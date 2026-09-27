# orbit-auth

Orbit 的独立认证库，支持 API Key，以及 Anthropic、Gemini CLI、Antigravity、ChatGPT 的浏览器 OAuth 登录、自动刷新和本地退出。目录和 Google 项目由主机显式传入，不读取其他客户端的账号缓存。

## 登录与配置

TUI 中输入 `/connect`，选择 Google Antigravity、Google Gemini CLI 或 ChatGPT 后直接打开浏览器授权，无需输入模型 ID。成功后沿用已有账号模型配置；未配置时追加内置兼容模型目录并打开模型选择器，无需重启。内置目录不代表账号一定拥有各模型的使用权限。Google 未能自动确定 Cloud Code Assist 项目时，弹窗会在授权后请求项目 ID，并使用内存中的授权继续，无需重新打开浏览器；取消即丢弃未完成的授权。Esc 可关闭或取消登录弹窗。

```sh
orbit auth login antigravity
orbit auth login google
orbit auth login chatgpt
orbit auth login anthropic
orbit auth list
orbit auth logout google
```

登录默认打开浏览器；`--no-browser` 仅打印链接，`--manual` 改为粘贴完整回调 URL。回调分别使用 Google 的 8085、ChatGPT 的 1455、Anthropic 的 54545 本机端口。端口占用时明确报错，不擅自换端口。`--name work` 可另存一份账号。

Google 接入 Gemini CLI / Cloud Code Assist 通道，登录后发现或初始化项目；需要显式指定项目的账号使用 `orbit auth login google --project <项目ID>`。Antigravity 使用独立的 OAuth 客户端、51121 回调端口和 daily-cloudcode 服务，自动初始化托管项目，不要求手填项目 ID。当前内置目录仅提供已接入的 Gemini 文本/图片输入模型，不宣称支持 Antigravity 的 Claude、GPT-OSS 或图片生成模型。ChatGPT 使用 Codex Responses 通道。

在 `config.toml` 中配置供应商和账号可用的模型，`credential` 替代 `api_key`：

```toml
default_provider = "google"

[providers.google]
api_format = "google_code_assist"
base_url = "https://cloudcode-pa.googleapis.com"
credential = "google"

[[providers.google.models]]
id = "gemini-2.5-pro"
supports_vision = true

[providers.chatgpt]
api_format = "chatgpt_codex"
base_url = "https://chatgpt.com/backend-api/codex"
credential = "chatgpt"

[[providers.chatgpt.models]]
id = "gpt-5.6-sol"
supports_vision = true
reasoning = { adapter = "openai_responses", efforts = ["low", "medium", "high"], default = "high" }
```

模型 ID 按账号实际可用模型填写。Google 未配置 `reasoning` 时使用服务端默认思考策略；支持 `thinkingLevel` 的模型可配置 `adapter = "google"`，efforts 从 `minimal/low/medium/high` 中选择该模型支持的值。此适配器不把 `none` 偷换成最小思考，也不适用于仅支持 `thinkingBudget` 的模型。

`chat`、TUI、摘要和标题请求共用认证路径。两种新通道均使用 SSE，非交互调用会收集为完整结果；Google 工具签名保存在会话中，只由 Google 请求适配器回传。ChatGPT 请求不会发送该端点不支持的 API 采样参数和输出上限。

API Key 仍可独立保存，交互输入不会回显：

```sh
orbit auth set-key my-provider --base-url https://api.example.com/v1
```

之后填写 `credential = "my-provider"`。自动化可用 `--stdin` 从管道读取密钥，不把密钥放入命令行参数。

## 存储与验证范围

默认目录为 `$XDG_CONFIG_HOME/orbit/auth`，未设置时为 `~/.config/orbit/auth`；可用全局 `--auth-dir` 指定。凭据是权限受限的明文 JSON，Unix 目录 0700、文件 0600，通过文件锁和原子替换串行化刷新。此目录受 Orbit 工具和沙盒保护，token 不进入会话记录。

API Key 绑定保存时的服务地址；OAuth 绑定各自官方服务及协议，不能把账号 token 转发到任意自定义网关。不会在 OAuth 失败后切换成 API Key 计费。ChatGPT 刷新若返回不同账号 ID，会要求重新登录，避免静默切换账号。

本地测试覆盖授权参数、PKCE/state、刷新响应、项目初始化、请求头、流式工具调用和签名回传。真实账号授权、额度及模型可用性仍需登录实测。当前未实现 device-code 登录。Anthropic 保留为实验性兼容实现，未移植 Claude Code 身份伪装及计费指纹，登录成功不代表套餐推理可用。

## 参考

按 [oh-my-pi](https://github.com/can1357/oh-my-pi/tree/533826a30a11d3fcac9fa59f6feb1ec60a1954d4) 的 `packages/ai/src/registry/oauth`、`packages/catalog/src/compat/rules/auth`、Google 和 Codex 请求适配重新实现；参考 MIT 许可证保留在 `LICENSE-REFERENCE`。

账号流程参考 [Gemini CLI 认证文档](https://geminicli.com/docs/get-started/authentication/) 和 [OpenAI 认证文档](https://developers.openai.com/codex/auth/)。Google 的安装式客户端身份属于公开客户端配置，不是用户凭据。
