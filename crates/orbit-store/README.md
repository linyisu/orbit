# 会话存储

`orbit-store` 用 SQLite 保存 TUI 和 `orbit chat` 会话。数据库工作在 `spawn_blocking` 中执行，避免阻塞终端事件循环。

- `projects` / `project_paths`：稳定项目 ID、当前目录、文件系统身份及历史路径。仅保存在全局数据库，不向项目写入标记。
- `sessions`：项目 ID、创建时工作区、模型名称、标题，以及最后成功完成的模型上下文。
- `runs`：每次用户输入及运行状态。
- `events`：按顺序保存思考、回答和工具事件，用于恢复界面。

显示历史与模型上下文分开保存，参考本地 SztuCode 的 session store 的职责划分。这里使用事务把最终事件、运行状态和成功上下文一起提交。失败、取消和中断不会把半截工具调用放入下一次模型请求，也不会自动重跑工具。

启用 WAL、外键及 schema 版本检查。每个会话使用 操作系统文件锁保证只有一个写入者；进程退出时锁自动释放，恢复会话时才把遗留的 `running` 标记为 `interrupted`。只读检查不会改变运行状态。`/compact` 可替换当前模型上下文，原始事件、规范消息和已保存的上下文归档仍保留。

流事件每约 250ms 或累计 256 条批量落盘，正常结束时立即提交。强制终止进程可能丢失尚未提交的少量事件，但不会产生半次上下文提交。运行尚未完成时已执行的工具操作不会因恢复上下文而回滚。

## 上下文持久化与恢复

各类记录承担不同职责，不能把模型摘要或界面事件当作完整原文：

- `session_messages`：只追加的规范消息日志，保存新用户消息、模型回复及工具结果，读取时校验内容哈希。新消息与执行检查点在同一事务提交，压缩投影不会改写这些消息。
- `sessions.history`：最后成功提交的模型上下文投影，可被整理结果替换。目前恢复仍依赖此投影与执行检查点，尚未支持从规范消息日志完整重建投影。旧会话通过 `legacy_gap` 标记无法保证完整的历史，不把旧摘要补作原文。
- 上下文归档：精简前独立持久化的原始上下文快照；同一会话内相同内容按哈希复用归档 ID。当前按快照去重，并非按单条消息建立证据索引。`read_history` 仅能读取当前会话的归档，提供分页和文本搜索。
- 摘要检查点：保存已成功处理的结构化摘要、来源指纹与游标。失败或取消不删除已提交进度，但只能对相同来源前缀续接，不能作为任意新历史的摘要。
- 执行检查点：记录运行中的消息及工具执行状态。恢复失败或中断的运行时，用它识别尚未解决的工具调用，不自动重放已有副作用。

归档成功是移除投影中工具原文的前提；摘要进度保存不等于整个运行已成功提交。压缩审计用于解释处理过程，也不能替代执行恢复点。归档内容是历史证据，不具备新的指令权限；历史文件写入记录不保证当前磁盘内容仍相同。

## 使用

配置 `config.toml` 后，在仓库根目录运行：

```sh
cargo run -p orbit-cli -- tui
```

输入 `read Cargo.toml`，完成后按 Ctrl+D 退出。查找并恢复会话：

```sh
cargo run -p orbit-cli -- sessions list
cargo run -p orbit-cli -- tui --resume <ID>
```

`/status` 显示会话 ID、数据库路径及上下文数量；`/resume` 或 `/sessions` 打开当前项目的会话选择器；`/new` 创建新会话并保留旧记录。

```sh
cargo run -p orbit-cli -- sessions show <ID>
cargo run -p orbit-cli -- sessions compactions <ID>
cargo run -p orbit-cli -- --database /tmp/orbit-test.db tui
```

数据库默认不放在项目内。可用 `orbit_database` 或 `--database` 指定；所有相关命令应使用同一个路径。默认优先使用绝对路径 `$XDG_DATA_HOME/orbit/orbit.db`，否则：

- Linux：`$HOME/.local/share/orbit/orbit.db`
- macOS：`$HOME/Library/Application Support/orbit/orbit.db`
- Windows：`%LOCALAPPDATA%\orbit\orbit.db`

数据库、SQLite 辅助文件和会话锁均位于数据目录。Orbit 不自动创建项目内的 `.orbit`、身份文件或 Git 标记；显式指定项目内的 `--database` 路径仍会按指定位置写入。

会话通过稳定项目 ID 关联目录。Unix 读取设备号、inode 和可用的创建时间；Windows 适配读取卷标识、128 位 File ID 和创建时间。身份信息完整时，在同一文件系统内改名或移动后，从新目录打开 TUI 即可用 `/resume` 找回会话。复制目录不会自动共享会话，Git remote 也不参与自动合并。目录内修改文件不改变项目 ID。

跨文件系统移动、身份信息不足或设备标识变化时，使用显式关联。在新目录执行：

```sh
orbit projects list
orbit projects link <项目ID>
orbit tui
```

先关闭该项目的 TUI。关联要求原目录已不存在，目标不能已有其他项目的对话；不会自动合并两份历史。历史里的绝对路径文本保留原样。目录位置在重新打开项目时识别，不做后台扫描或运行中工作目录切换。

旧 schema 1 数据库会事务迁移，按原工作区路径分配项目 ID，保留会话、事件和模型上下文。第一次在原路径打开时补充目录身份；升级前已经移动的项目需要手动关联，无法凭旧路径判断它此前是否被其他目录替换。不同数据库拥有独立项目登记表。

当前仅在 Linux 实测。macOS 和 Windows 的目录身份与数据路径有条件编译适配，但尚未在对应系统验证；整个 Orbit 的工具执行与沙箱仍依赖 Unix/Linux，不能据此宣称 Windows 或 macOS 完整可用。网络文件系统和不支持创建时间的文件系统不会自动按弱身份恢复改名关系。

模型和连接沿用本次配置，数据库不保存 API Key。会话内容和工具输出为本地明文，数据库及其 WAL/SHM/journal 文件禁止通过该 TUI 的 `read_file` 读取。Unix 新建数据库文件权限为 0600；Windows 使用继承的系统 ACL。

TUI 和 `orbit chat` 共用会话持久化与执行检查点。`sessions show` 输出包含事件与上下文的 JSON，可用于调试。上下文预算与手动总结参见 [orbit-agent 使用说明](../orbit-agent/README.md)。

`scripts/try-live.sh` 为验收隔离数据，显式使用 `/tmp/orbit-live-XXXXXX/sessions.db`，测试项目位于同目录下的 `project/`。测试库不在项目目录内部，也不会自动合并进正常使用的全局数据库。

`sessions compactions <ID>` 只读返回该会话的结构化压缩审计事件，包括运行状态、摘要、保留边界和确定性文件操作记录。尚未完成或失败运行中的记录不会自动成为恢复点。旧版只有用量提示的压缩事件仍能读取，但不会凭空补造摘要或审计记录。
