# orbit-auth

[项目总览](../../README.md)

`orbit-auth` 是 Orbit 的独立认证与凭据管理库，支持 API Key 以及主流 AI 服务（Google Cloud Code Assist、Google Antigravity、ChatGPT Codex、Anthropic）的浏览器 OAuth 2.0 PKCE 授权登录、令牌自动刷新与本地退出。所有凭据由主机显式管理，不读取其他第三方客户端的未授权缓存。

## 实现入口与复用边界

- [oauth.rs](src/oauth.rs) / [anthropic.rs](src/anthropic.rs)：供应商授权参数与 token 交换；[callback.rs](src/callback.rs) 负责回调 URL、state 与授权码的统一校验。
- [http.rs](src/http.rs)：认证请求的 30 秒超时、64 KiB token 回执上限与通用响应处理。各供应商的请求形状仍在其适配器中。
- [store.rs](src/store.rs)：私有凭据存储、刷新互斥与目标服务绑定；模型目录发现由 `orbit-model` 负责，不属于凭据存储。

## 架构与核心职责

- **协议封装与认证流**：基于 PKCE（RFC 7636）实现安全的公有客户端授权码流程，内置本地回环 HTTP 服务器接收授权回调。
- **凭据生命周期管理**：提供凭据的持久化存储、令牌有效期检查、后台静默刷新、原子更新以及显式登出销毁。
- **服务范围与安全边界**：将凭据严格绑定至对应官方协议端点，防止授权令牌被泄露至非信任网关；敏感认证数据与会话记录隔离，禁止进入工具沙箱或模型上下文。

## 登录与配置

在 TUI 中输入 `/connect`，选择对应官方提供商（Google Antigravity、Google Gemini CLI、ChatGPT 等）后直接唤起系统默认浏览器进行授权。授权成功后即刻沿用账号模型配置；未配置时追加内置兼容模型目录并打开模型选择器，无需重启应用。

```sh
# 交互式 OAuth 登录
orbit-cli auth login antigravity
orbit-cli auth login google
orbit-cli auth login chatgpt
orbit-cli auth login anthropic

# 凭据管理
orbit-cli auth list
orbit-cli auth logout google

# 独立保存 API Key
orbit-cli auth set-key my-provider --base-url https://api.example.com/v1
```

- **回环端口与回调机制**：Google 使用 8085 端口，ChatGPT 使用 1455 端口，Anthropic 使用 54545 端口，Antigravity 使用 51121 端口。端口被占用时明确报错，不随意切换端口以保证 OAuth 回调安全性。
- **无浏览器环境支持**：`--no-browser` 仅打印授权链接；`--manual` 支持在远程或无桌面环境中手动粘贴重定向后的完整回调 URL。
- **多账号命名支持**：`--name <NAME>` 可为同一服务保存多份独立命名凭据。

### 供应商特性支持

- **Google (Gemini CLI / Cloud Code Assist)**：接入 Cloud Code Assist 协议通道，登录后自动发现并初始化云端项目；支持通过 `--project <PROJECT_ID>` 显式指定项目。
- **Google Antigravity**：使用独立 OAuth 客户端、51121 回调端口与 daily-cloudcode 服务，自动初始化托管项目，无需用户手动提供项目 ID。
- **ChatGPT**：接入 OpenAI Codex Responses 协议通道。刷新令牌时若服务端返回不一致的账号 ID，会要求重新登录以避免静默串号。
- **Anthropic**：提供标准 OAuth 流程兼容实现，使用官方客户端配置完成授权。

### 配置文件示例 (`config.toml`)

使用 `credential` 字段引用由 `orbit-auth` 管理的凭据项，取代明文硬编码：

```toml

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

API Key 可通过 `orbit-cli auth set-key` 的交互输入录入。使用 `--stdin` 时应从已有的安全输入源读取；不要把真实密钥写成 shell 命令字面量。以下 Bash 示例隐藏输入，命令历史不包含密钥正文：

```bash
read -r -s -p "API key: " orbit_api_key_input
printf '\n'
printf '%s' "$orbit_api_key_input" | orbit-cli auth set-key my-provider --base-url https://api.example.com/v1 --stdin
unset orbit_api_key_input
```

## 存储安全与隔离模型

1. **存储路径**：默认存放在 `$XDG_CONFIG_HOME/orbit/auth`（或 `~/.config/orbit/auth`），可通过全局参数 `--auth-dir` 自定义。
2. **文件权限与并发锁**：目录权限强制为 `0700`，凭据文件权限强制为 `0600`。多进程并发刷新时通过操作系统文件排他锁与原子临时文件重命名（atomic rename）串行化刷新并避免直接覆盖半写入文件；存储或刷新失败仍会返回错误。
3. **凭据隔离**：
   - 凭据目录受 Orbit 运行时保护系统严格监管，被列入沙箱受保护路径，禁止沙箱内命令或工具访问。
   - API Key 严格绑定录入时的目标服务地址；OAuth 令牌仅允许向各官方服务及协议端点发送，禁止转发到自定义网关。
   - 认证令牌在模型推理传输层即时注入请求头，不进入会话历史数据库或上下文归档。

## 验证与测试

库包含覆盖授权参数构造、PKCE 散列计算、OAuth 状态校验、刷新响应解析、项目初始化流程以及 SSE 认证请求头注入的单元与集成测试；本地 HTTP 测试需要回环监听权限：

```sh
cargo test -p orbit-auth --offline
```
