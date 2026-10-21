# orbit-tool-macros

为 `orbit-agent::Tool` 提供编译期适配。`#[orbit_agent::tool(name = "…")]` 标记
`impl Tool` 内的 `async fn execute(&self, arguments: Arguments, cancellation: CancellationToken)`，
返回 `Result<ToolOutput, ToolError>`。函数文档成为模型说明，参数结构体通过
`serde::Deserialize` 与 `schemars::JsonSchema` 生成解码与 schema。

## 宏与执行器边界

宏只生成 `definition` 和装箱的 `execute`；能力声明、权限、取消、后台任务及文件边界
由 Tool 实现和执行器负责。动态 MCP schema 使用运行时注册。
字段的约束、默认值和说明写在参数类型中；值之间的约束仍需执行函数检查。

## 验证

```sh
cargo test -p orbit-tool-macros -p orbit-agent
```

[项目总览](../../README.md)
