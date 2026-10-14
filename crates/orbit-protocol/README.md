# orbit-protocol

工作区请求、响应、事件、版本协商和有界长度分帧。使用 Serde 编解码，不依赖运行时、存储或界面。

## 入口

- [lib.rs](src/lib.rs)：核心类型、协议版本与帧读写。
- [resources.rs](src/resources.rs)、[files.rs](src/files.rs)、[project.rs](src/project.rs)：资源、文件与项目操作。
- [plan.rs](src/plan.rs)、[subagents.rs](src/subagents.rs)、[user_input.rs](src/user_input.rs)：计划、子 Agent 与问答。
- [search.rs](src/search.rs)：选择器与补全的共用匹配规则。

写操作携带会话及工作区身份，服务校验过期请求。更改协议时同步检查客户端、服务端及兼容回归。`SubmitAttachments` 统一提交或暂存文件与图片，回放和队列分别保留两类附件元数据。

```sh
cargo test -p orbit-protocol
```

[项目总览](../../README.md)
