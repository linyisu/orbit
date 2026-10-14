# orbit-model

统一消息、工具调用、流式事件和模型协议。支持 OpenAI Chat Completions / Responses、ChatGPT Codex、Anthropic Messages 与 Google Code Assist。

## 入口

- [types.rs](src/types.rs)：消息、附件、工具定义与流式事件。
- [client.rs](src/client.rs)：模型请求入口。
- [openai/](src/openai/)、[anthropic/](src/anthropic/)、[google.rs](src/google.rs)：供应商编码与流式解析。
- [reasoning.rs](src/reasoning.rs)：思考参数适配。
- [discovery.rs](src/discovery.rs)、[account_catalog.rs](src/account_catalog.rs)：模型目录与能力解析。

配置、缓存和刷新调度属于 [orbit-config](../orbit-config/README.md)。附件共用 `Attachment` 的持久化契约；文件独立于视觉能力，文本内容投影到请求，二进制文件保留目标路径供工具读取。

## 验证

```sh
cargo test -p orbit-model
```

[项目总览](../../README.md)
