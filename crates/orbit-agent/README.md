# orbit-agent

模型与工具的执行循环，负责流式事件、补充输入、后台任务、取消和上下文整理。通过 trait 接入模型、工具与持久化，不持有界面或数据库连接。

## 入口

- [agent.rs](src/agent.rs)：运行循环和任务边界。
- [tool.rs](src/tool.rs)、[events.rs](src/events.rs)：工具注册与执行事件。
- [context.rs](src/context.rs)、[autocompact.rs](src/autocompact.rs)、[compaction.rs](src/compaction.rs)：预算估算、输出精简与摘要。
- [inbox.rs](src/inbox.rs)、[background.rs](src/background.rs)、[checkpoint.rs](src/checkpoint.rs)：补充消息、后台等待和检查点。

压缩前先归档原文，保留完整工具调用与回执配对。取消不会重放已完成的副作用；子 Agent 调度由 [orbit-service](../orbit-service/README.md) 负责。
已结束的最后一轮也可归档并摘要。系统指令、当前运行状态和最新用户请求保留原文；这些内容与工具定义超过窗口时，返回预算明细，不丢弃内容强行压缩。

## 上下文整理与恢复

有效上下文与原始会话分开保存，整理失败不覆盖原文。自动整理失败且原上下文仍在预算内时，
继续当前任务并延后下一次尝试；窗口已满或手动整理时，模型摘要失败可退回带来源的归档摘录。
摘录保留较早用户要求、部分摘要和工具回执的有限原文，明确缺失范围及 `ReadHistory` 入口，
不会被标记成完整模型摘要。取消、归档失败或无法容纳固定输入时保留现有上下文，由服务提供恢复提示。

整理分为旧工具输出精简、历史交接和归档摘录恢复。近期后缀保留完整调用配对；
压缩收益只针对可回收历史计算，不要求系统指令、工具定义或最新请求也缩小。
旧交接作为可分块的来源重新处理，避免切换小窗口后旧摘要本身堵住所有请求。
文件收据在审计记录中保持完整，模型上下文只携带有界文件索引。

摘要使用具名章节的 Markdown，读取边界同时接受已有 JSON 交接。空回复、异常终止、
工具调用、输出截断和超长交接均不能发布。每批最多连续尝试三次，另受总时限约束；
输入超限、截断和超时可缩小来源范围，限流和其他上游错误不在压缩层密集重试。
摘要请求显式采用低推理强度，生成预算与最终交接预算分开，并预留同一模型的生成空间。
成功批次按不可变来源指纹保存游标，取消不会发布半成品或重放已完成工具。

设计参考：
[OpenCode](https://dev.opencode.ai/v2/docs/compaction/) 的近期原文保留与固定输入边界、
[Gemini CLI](https://github.com/google-gemini/gemini-cli/blob/main/packages/core/src/context/chatCompressionService.ts)
的输出整理及失败退避、
[Claude](https://platform.claude.com/docs/en/build-with-claude/compaction-on-demand) 的终止原因与缺失摘要处理。
[OpenAI 原生压缩](https://developers.openai.com/api/docs/guides/compaction) 的不透明状态有专门的协议语义，
本实现不将普通文字摘要冒充供应商原生状态，也不自动启用其端点。

## 工具契约

工具通过 `ToolOutput` 的类型化结果和文件收据报告执行证据；模型输出文本不决定任务状态。
前台与后台完成事件都保存明确的完成、失败或取消结果；旧事件未提供结果字段时由读取边界兼容。
不向工具参数 schema 注入展示专用字段，界面默认使用业务参数摘要，不额外调用模型。
旧调用中的 `_orbit_purpose` 在校验和权限检查前提取为独立展示元数据；
已有业务同名字段和组合契约保持原参数，旧目的说明不再回填到模型历史。
前台和后台错误共享结构化模型回执，保留错误类别和恢复建议；服务包装不能丢失已有取消或权限语义。
参数错误保留严格校验，并附上工具声明的顶层字段和必填项。

静态工具可以在 `impl Tool` 内使用 `#[orbit_agent::tool(name = "…")]`：
具名参数类型派生 `Deserialize` 和 `JsonSchema`，方法文档提供模型说明。
宏生成定义及参数解码，原有能力声明和执行函数继续负责授权约束、取消与结果收据。
旧事件缺少文件收据时，仅在恢复入口按旧格式读取。模型流的聚合与结束校验由 `orbit-model` 负责。

## 验证

```sh
cargo test -p orbit-agent
```

[项目总览](../../README.md)
