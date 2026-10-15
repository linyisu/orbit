# orbit-auth

API Key、OAuth 登录、凭据存储和令牌刷新。支持 Google、Antigravity、ChatGPT 与 Anthropic；凭据绑定服务地址，不进入模型上下文。模型发现和协议解码属于 [orbit-model](../orbit-model/README.md)。

## 使用

TUI 用 `/connect`，GUI 用账号设置，完成登录、API Key 管理和账号查看。

配置用 `credential` 引用保存的凭据，`auto` 使用账号返回的模型目录：

```toml
[providers.google]
api_format = "google_code_assist"
base_url = "https://cloudcode-pa.googleapis.com"
credential = "google"
models = "auto"

[providers.chatgpt]
api_format = "chatgpt_codex"
base_url = "https://api.openai.com/v1/"
credential = "chatgpt"
models = "auto"
```

默认凭据目录为 `$XDG_CONFIG_HOME/orbit/auth`，未设置时使用 `~/.config/orbit/auth`。Unix 目录与文件权限分别为 `0700`、`0600`，刷新通过锁和原子替换协调。API Key 与连接配置联动提交时持有凭据锁，配置写入失败会恢复旧凭据；这不承诺跨文件的崩溃原子性。

ChatGPT 新登录使用 [Sign in with ChatGPT](https://developers.openai.com/siwc/token-sharing-open-source/sign-in) 的动态客户端注册，校验 ID token 后保存账号及注册身份。套餐调用需要用户授予直接使用额度的权限。旧 Codex 凭据保持原地址约束；重新授权后才迁移到公开接口。新流程尚无已接入的公开额度查询接口。

## 入口与验证

[oauth/](src/oauth/) 协调登录与回调，[providers/](src/providers/) 封装各供应商的授权、请求认证和刷新，[store.rs](src/store.rs) 管理存储与锁。

```sh
cargo test -p orbit-auth
```

[项目总览](../../README.md)
