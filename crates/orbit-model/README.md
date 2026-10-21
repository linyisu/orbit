# orbit-model

统一消息、工具调用、流式事件和模型协议。支持 OpenAI Chat Completions / Responses、ChatGPT Codex、Anthropic Messages 与 Google Code Assist。

## 模块职责

- [types.rs](src/types.rs)：消息、附件、工具定义与流式事件。
- [client.rs](src/client.rs)：模型请求入口。
- [stream.rs](src/stream.rs)：工具增量、分片用量与终止证据的统一聚合。
- [openai/](src/openai/)、[anthropic/](src/anthropic/)、[google.rs](src/google.rs)：供应商编码与流式解析。
- [reasoning.rs](src/reasoning.rs)：思考参数适配。
- [tool_purpose.rs](src/tool_purpose.rs)：兼容展示元数据读取；展示字段不进入工具定义或模型历史。
- [discovery.rs](src/discovery.rs)：带认证、分页及容量限制的实时目录获取和 API Key 验证。
- [catalog/](src/catalog/)：供应商目录、能力及公共规格解码，区分完整目录和部分观测。
- [account_catalog.rs](src/account_catalog.rs)：固定来源的能力规格，仅补充已发现条目。

## 目录与请求契约

公共能力快照保存在 [catalog/public_metadata.json](src/catalog/public_metadata.json)，来源与许可随数据保留。

配置、缓存和刷新调度属于 [orbit-config](../orbit-config/README.md)。附件共用 `Attachment` 的持久化契约；文件独立于视觉能力，文本内容投影到请求，二进制文件保留目标路径供工具读取。

模型可用性以当前连接返回的目录为准，不按内置名称过滤模型。ChatGPT 套餐使用公开 `/v1/models` 与 `/v1/responses`；旧 Codex 路由单独保留兼容处理。供应商差异集中在协议适配模块。

流必须带有效结束原因，单独的用量更新不代表结束；缺失终止证据时返回错误，不执行部分工具调用或自动重发请求。

## 验证

```sh
cargo test -p orbit-model
```

[项目总览](../../README.md)
