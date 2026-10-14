# orbit-model

统一消息、工具调用、流式事件和模型协议。支持 OpenAI Chat Completions / Responses、ChatGPT Codex、Anthropic Messages 与 Google Code Assist。

## 入口

- [types.rs](src/types.rs)：消息、附件、工具定义与流式事件。
- [client.rs](src/client.rs)：模型请求入口。
- [openai/](src/openai/)、[anthropic/](src/anthropic/)、[google.rs](src/google.rs)：供应商编码与流式解析。
- [reasoning.rs](src/reasoning.rs)：思考参数适配。
- [discovery.rs](src/discovery.rs)、[account_catalog.rs](src/account_catalog.rs)：模型目录与能力解析。

配置、缓存和刷新调度属于 [orbit-config](../orbit-config/README.md)。图片读取共用 `Attachment` 的大小限制；视觉能力决定请求中的图片投影方式。

## 验证

```sh
cargo test -p orbit-model
```

[项目总览](../../README.md)
