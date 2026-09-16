# 会话存储

`orbit-store` 用 SQLite 保存 TUI 会话。数据库工作在 `spawn_blocking` 中执行，避免阻塞终端事件循环。

- `sessions`：工作区、模型名称、演示模式、标题，以及最后成功完成的模型上下文。
- `runs`：每次用户输入及运行状态。
- `events`：按顺序保存思考、回答和工具事件，用于恢复界面。

显示历史与模型上下文分开保存，参考本地 SztuCode 的 session store 的职责划分。这里使用事务把最终事件、运行状态和成功上下文一起提交。失败、取消和中断不会把半截工具调用放入下一次模型请求，也不会自动重跑工具。

启用 WAL、外键及 schema 版本检查。每个会话使用 Unix 文件锁保证只有一个写入者；进程退出时锁自动释放，恢复会话时才把遗留的 `running` 标记为 `interrupted`。只读检查不会改变运行状态。`/compact` 可替换当前模型上下文，原始事件仍完整保留；尚未实现记录归档。

流事件每约 250ms 或累计 256 条批量落盘，正常结束时立即提交。强制终止进程可能丢失尚未提交的少量事件，但不会产生半次上下文提交。运行尚未完成时已执行的工具操作不会因恢复上下文而回滚。

## 使用

在仓库根目录运行，无需 API：

```sh
cargo run -p orbit-cli -- tui --demo
```

输入 `read Cargo.toml`，完成后按 Ctrl+D 退出。查找并恢复会话：

```sh
cargo run -p orbit-cli -- sessions list
cargo run -p orbit-cli -- tui --demo --resume <ID>
```

`/status` 显示会话 ID、数据库路径及上下文数量；`/sessions` 列出会话；`/clear` 创建新会话并保留旧记录。

```sh
cargo run -p orbit-cli -- sessions show <ID>
cargo run -p orbit-cli -- --database /tmp/orbit-test.db tui --demo
```

默认数据库位置为 `$XDG_DATA_HOME/orbit/orbit.db`，未设置时使用 `$HOME/.local/share/orbit/orbit.db`。也可通过 `orbit_database` 或 `--database` 指定；所有相关命令应使用同一个路径。

恢复要求工作区及演示/真实模式一致。模型和连接沿用本次配置，数据库不保存 API Key。会话内容和工具输出为本地明文，数据库及其 WAL/SHM/journal 文件禁止通过该 TUI 的 `read_file` 读取。新建数据库文件权限为 0600。

持久化入口是 TUI；现有一次性 `chat` 命令仍不保存会话。`sessions show` 输出包含事件与上下文的 JSON，可用于调试。上下文预算与手动总结参见 [orbit-agent 使用说明](../orbit-agent/README.md)。
