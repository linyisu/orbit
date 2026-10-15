# orbit-auth

API Key、OAuth 登录、凭据存储和令牌刷新。支持 Google、Antigravity、ChatGPT Codex 与 Anthropic；凭据绑定服务地址，不进入模型上下文。

## 使用

TUI 用 `/connect`，GUI 用账号设置，完成登录、API Key 管理和账号查看。

配置用 `credential` 引用保存的凭据，模型 ID 按账号实际目录填写：

```toml
[providers.google]
api_format = "google_code_assist"
base_url = "https://cloudcode-pa.googleapis.com"
credential = "google"
models = ["gemini-2.5-pro"]

[providers.chatgpt]
api_format = "chatgpt_codex"
base_url = "https://chatgpt.com/backend-api/codex"
credential = "chatgpt"
models = ["gpt-5.6-sol"]
```

默认凭据目录为 `$XDG_CONFIG_HOME/orbit/auth`，未设置时使用 `~/.config/orbit/auth`。Unix 目录与文件权限分别为 `0700`、`0600`，刷新通过锁和原子替换协调。API Key 与连接配置联动提交时持有凭据锁，配置写入失败会恢复旧凭据；这不承诺跨文件的崩溃原子性。

## 入口与验证

[oauth.rs](src/oauth.rs) 处理登录，[callback.rs](src/callback.rs) 校验回调，[store.rs](src/store.rs) 管理存储与刷新。

```sh
cargo test -p orbit-auth
```

[项目总览](../../README.md)
