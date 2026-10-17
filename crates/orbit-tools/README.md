# orbit-tools

Agent 的文件、命令、网络、文档与 MCP 工具。工具实现 `orbit-agent::Tool`，由服务按会话权限注册；参数及限制以各工具的 schema 和源码为准。

## 工具目录

目录中的短名是模型实际调用的名称，注册、权限筛选、记录和两端展示使用同一名称，不注册旧名称别名。
内置工具统一首字母大写。MCP 使用 `MCP_服务名_工具名`，例如 `MCP_Files_ReadFile`；下划线分词转为大写开头的单词。
规范化重名或超过 64 字节时附加来源摘要，避免覆盖其他工具；请求上游仍使用其原始名称。
MCP 禁用与刷新按注册器中的来源处理，不依赖名称前缀。工作区 RPC 方法名不属于模型工具目录。

`ReadFile`、`ListFiles`、`Grep`、`EditFile`、`WebFetch` 和 `CommandOutput` 使用具名参数结构体及
`#[orbit_agent::tool]`。schema 随类型生成，数值约束使用所属模块的常量；
执行时仍校验跨字段条件和资源边界。其他工具及动态 MCP 沿用 `Tool` 接口，迁移时
必须同时保留默认值、错误语义、能力声明与取消行为。

| 类别 | 入口 |
| --- | --- |
| 文件浏览与搜索 | [ReadFile](src/read_file.rs)、[ListFiles](src/list_files.rs)、[Grep](src/search_text.rs) |
| 项目概览与符号 | [ProjectOverview / CodeSymbols](src/project_overview.rs) |
| 文件写入与编辑 | [WriteFile](src/write_file.rs)、[EditFile](src/edit_file.rs) |
| 图片与文档 | [ReadImage](src/read_image.rs)、[ReadDocument](src/read_document.rs) |
| 命令与日志 | [Exec](src/run_command.rs)、[CommandStatus / WaitCommands / StopCommands](src/command_jobs.rs)、[CommandOutput](src/read_command_output.rs) |
| 网页与搜索 | [WebFetch / WebSearch](src/web_fetch.rs) |
| 外部路径授权 | [RequestAccess](src/request_capability.rs) |
| MCP | [mcp](src/mcp.rs) |

服务另外提供以下会话工具：

| 类别 | 名称与用途 |
| --- | --- |
| 子 Agent | `SpawnAgent` 创建、`ReadAgent` 查看、`SendAgentMessage` 追加消息、`WaitAgents` 等待、`StopAgents` 取消 |
| 计划与问答 | `EnterPlan` 进入规划、`UpdatePlan` 保存计划、`SubmitPlan` 提交审阅、`AskUser` 提问 |
| 项目任务 | `ListTasks` 列出任务、`UpdateTask` 创建或修改任务 |
| 会话记忆 | `ListMemories` 列出、`SearchMemory` 搜索、`SaveMemory` 保存、`ForgetMemory` 停用 |
| 上下文与证据 | `ReadHistory` 读取压缩前历史、`LoadSkill` 加载技能、`CheckEvidence` 检查已有执行证据 |

`CommandStatus` 查看后台命令，`WaitCommands` 等待命令完成，`StopCommands` 取消命令；它们使用 `job_ids`。
子 Agent 工具使用 `agent_id` 或 `agent_ids`，与后台命令分开管理。

## 执行与审批

命令与 MCP 服务按目标用户权限运行，审批声明不提供文件系统或网络隔离。内置文件工具另行校验工作区、外部授权和保护路径；写入使用原子提交。

- `ask`：修改、命令、MCP 和新增外部授权按规则确认。
- `auto`：自动批准工作区内写入及从工作区启动的命令；MCP 和新增外部授权继续审批。
- `yolo`：自动批准工具请求，内置文件路径保护仍生效。

后台命令支持状态、等待和取消，不跨服务重启恢复。`WaitCommands` 默认等待 30 秒，最长 60 秒；等待超时不会取消命令。
服务将命令原始日志保存在所属会话中，任务结束与服务重启后可继续读取；每路最多 16 MiB，每会话约 128 MiB，容量不足时按命令回收旧日志。
`CommandOutput` 分别报告已有日志是否还有后页（`has_more`）和原始采集是否完整（`capture_complete`）；尚未完成或没有采集收据时后者为 `null`。
删除会话同时删除其日志。文档页图与 MCP 图片使用临时附件，持久图片另由附件库管理。

工具在操作成功后填充 `ToolOutput` 的文件收据，并单独报告执行结果；这些内部证据不从展示 JSON 反推。
模型收到的执行错误统一包含 `error.code`、`message`、`recovery` 和 `retryable`；
参数错误、权限拒绝、策略禁止、输入冲突、缺失依赖及取消保持明确分类，未知原因保留为执行失败。
恢复建议不授权自动重放。`Exec` 保留操作系统终止信号及核心转储标记，不推测信号发送者。
完成、失败和取消随事件持久化，两端优先使用明确结果；旧记录沿用兼容读取。
MCP 的 `isError` 映射为失败结果，保留内容及可用连接，后续调用仍按审批规则执行。

Git 操作统一使用 `Exec`；内部 Git 检查仍为审阅和执行证据服务，不注册模型工具。

## 文件与网页结果

- `EditFile` 支持单次替换或 `edits` 有序批次；全部替换在内存中成功后才进行一次审批和原子提交，失败不写入文件。
  成功回执包含已提交差异、应用编辑数和增删行数；模型差异最多 16 KiB，完整差异仍由内部文件收据提供给两端。
- `ListTasks`、`ListMemories` 默认返回 20 条，最多 100 条且每页不超过 64 KiB；
  按 ID 升序排列，使用 `next_after_id` 作为下一次的 `after_id`，保持原筛选条件。
  分页反映当前数据，不是冻结快照。任务仍用 `UpdateTask(action=update, status=completed)` 收尾；
  `release` 是交接未完成任务，不能绕过领取和依赖检查。
- `CheckEvidence` 区分新增、删除和修改；没有直接写入收据的变化标记为来源未确认，
  不把命令运行期间观察到的文件自动归为本会话所有。执行权限仅在前后文件均存在时比较。
- `Grep.output_mode` 支持 `content`、`files`、`count`；内容模式每个匹配行只返回一次，分页按返回项计数。两端详情保留上下文行。
- `ListFiles.glob` 按路径过滤结果，仍遍历未匹配的目录；原有深度、保护路径及扫描预算继续生效。
- 网页与图片请求默认只访问公网，复用环境代理和 `[web]` 配置。HTML 优先读取唯一的 `main` 或 `article`，没有明确正文区域时保留全文。
- `WebFetch` 返回 `snapshot_id`，后页使用该 ID 和 `next_start_char`，不会重新请求 URL。引用链接随相关内容返回，不重复附上全页链接表。
  服务将快照保存到所属会话，重启后仍可读取；上限 64 份、32 MiB，删除会话时一并删除。
  回收后明确报告失效，重新抓取才会创建新快照。工具详情按快照读取完整正文，不受模型单次返回页的长度限制。

## MCP

配置位于 `[mcp.servers.<名称>]`，当前支持 stdio。启动需显式允许：TUI 使用 `--allow-mcp-start`，GUI 使用 MCP 设置开关。

GUI 保存后点击“应用到当前会话”重新连接；当前任务保留原工具，下一次发送使用更新结果。导入先识别配置和启动参数，确认后安装依赖；失败显示完整错误链与有界 stderr。

`timeout_seconds` 默认 120 秒，覆盖单次请求的发送和执行；长任务需同时检查该值与服务器自身的超时设置。正常业务错误保留连接；超时、取消或协议故障会关闭连接，先通知取消并留出清理时间，再终止所属进程组。下一次获批调用可重新初始化，结果标记 `connection_restarted`；旧服务器会话状态可能失效，失败操作不会自动重放。工具契约改变时需重新应用配置。

工具目录完整读取分页，拒绝重复名称、循环游标及超预算目录。支持目录变更通知的服务器在执行前重新核对契约；改变后要求重新应用配置。
结果保留 `structuredContent`；图片块按当前模型的视觉能力转换为原生附件，无法转换时说明原因，不把 base64 当作文字发送给模型。

进程组回收覆盖仍在组内的子进程。服务器主动脱离进程组的后台进程及其临时文件由服务器管理。

## 验证

```sh
cargo test -p orbit-tools
```

[项目总览](../../README.md)
