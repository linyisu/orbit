# orbit-tools

Agent 的文件、命令、网络、文档与 MCP 工具。工具实现 `orbit-agent::Tool`，由服务按会话权限注册；参数及限制以各工具的 schema 和源码为准。

## 工具目录

| 类别 | 入口 |
| --- | --- |
| 文件浏览与搜索 | [read_file](src/read_file.rs)、[list_files](src/list_files.rs)、[search_text](src/search_text.rs) |
| 项目概览与符号 | [project_overview / code_symbols](src/project_overview.rs) |
| 文件写入与编辑 | [write_file](src/write_file.rs)、[edit_file](src/edit_file.rs) |
| 图片与文档 | [read_image](src/read_image.rs)、[read_document](src/read_document.rs) |
| 命令与日志 | [run_command](src/run_command.rs)、[command_jobs](src/command_jobs.rs)、[read_command_output](src/read_command_output.rs) |
| 网页与搜索 | [web_fetch](src/web_fetch.rs) |
| Git 与外部授权 | [git_push](src/git_push.rs)、[request_capability](src/request_capability.rs) |
| MCP | [mcp](src/mcp.rs) |

## 执行与审批

命令与 MCP 服务按目标用户权限运行，审批声明不提供文件系统或网络隔离。内置文件工具另行校验工作区、外部授权和保护路径；写入使用原子提交。

- `ask`：修改、命令、MCP 和新增外部授权按规则确认。
- `auto`：自动批准工作区内写入及从工作区启动的命令；MCP 和新增外部授权继续审批。
- `yolo`：自动批准工具请求，内置文件路径保护仍生效。

后台命令支持状态、等待和取消，不跨服务重启恢复。命令原始日志有保留上限，任务结束后失效；文档页图使用临时文件，持久图片另由附件库管理。

网页与图片请求默认只访问公网，复用环境代理和 `[web]` 配置。`git_push` 仅支持明确的 HTTPS 目标，普通 Git 操作使用原生命令。

## MCP

配置位于 `[mcp.servers.<名称>]`，当前支持 stdio。启动需显式允许：CLI/TUI 使用 `--allow-mcp-start`，GUI 使用 MCP 设置开关。

GUI 保存后点击“应用到当前会话”重新连接；当前任务保留原工具，下一次发送使用更新结果。导入先识别配置和启动参数，确认后安装依赖；失败显示完整错误链与有界 stderr。

`timeout_seconds` 默认 120 秒，覆盖单次请求的发送和执行；长任务需同时检查该值与服务器自身的超时设置。正常业务错误保留连接；超时、取消或协议故障会关闭连接，先通知取消并留出清理时间，再终止所属进程组。下一次获批调用可重新初始化，结果标记 `connection_restarted`；旧服务器会话状态可能失效，失败操作不会自动重放。工具契约改变时需重新应用配置。

进程组回收覆盖仍在组内的子进程。服务器主动脱离进程组的后台进程及其临时文件由服务器管理。

## 验证

```sh
cargo test -p orbit-tools
```

[项目总览](../../README.md)
