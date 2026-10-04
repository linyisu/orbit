# orbit-store

`orbit-store` 是 Orbit 的持久化存储系统，基于 SQLite 提供高效、事务安全且高并发的数据存储引擎。它负责持久化会话历史、模型上下文快照、规范消息日志、多窗口生命周期元数据以及跨目录改名的项目身份跟踪。

## 架构与核心组件

- **非阻塞数据管道**：SQLite 读写通过 `access` / `read_access` 在 `spawn_blocking` 中执行。目录、锁文件与部分侧车文件操作仍有同步路径，不应把这一点理解为所有文件 I/O 都异步。
- **并发与持久性模型**：
  - 开启 SQLite WAL（Write-Ahead Logging）模式，实现读写互不阻塞。
  - 强制启用外键约束，保证关联数据的完整性与级联安全。
  - 批量刷盘机制：流式事件每约 250ms 或累积 256 条批量落盘，任务正常结束时即时提交事务。
- **单写入者排他锁**：每个会话由操作系统级排他文件锁保护，同一时刻只允许一个进程/窗口向该会话提交新内容，有效防止会话状态产生并发写入分叉。

---

## 核心数据表与设计分层

```text
orbit.db
├── projects / project_paths  # 稳定项目 ID 与物理目录身份映射
├── sessions                  # 会话元数据与当前模型上下文投影 (sessions.history)
├── runs                      # 用户交互轮次与执行状态 (running, completed, interrupted)
├── events                    # 细粒度界面渲染事件流 (thinking, text, tool_call, tool_result)
├── session_messages          # 只追加的规范消息日志 (带 SHA-256 完整性哈希)
├── context_archives          # 原始上下文压缩前快照 (带哈希去重与分页检索)
├── compaction_progress       # 结构化增量摘要进度与来源指纹游标
├── agent_sessions            # 父子会话关系及只读角色
├── agent_invocations         # 子任务调用、持久结果与投递确认
├── agent_messages            # 子任务补充消息与接收确认
├── side_questions            # 独立的旁路提问与回答，不进入主会话上下文
├── execution_checkpoints     # 工具调用执行检查点 (用于故障中断识别)
└── tui_windows               # 活跃 TUI 窗口注册与跨会话恢复快照
```

### 上下文投影与消息日志分离

Orbit 将“展示事件”、“当前模型上下文”与“规范原始消息”严格分层：

1. **`session_messages`（规范日志）**：
   只追加记录所有用户输入、模型回复与工具返回内容。每条消息保存 SHA-256 内容摘要，回读时校验正文是否与已存哈希一致。它是规范历史来源，不是密码学意义上的不可篡改日志；具有数据库写权限的人可以同时改写正文与哈希。
2. **`sessions.history`（模型上下文投影）**：
   保存当前轮次模型所实际感知到的消息数组。在触发 `/compact` 或自动分层整理时，仅替换此上下文投影，而不修改 `session_messages` 规范日志。
3. **`context_archives`（上下文归档）**：
   在精简工具大文本前，将整段历史完整持久化为只读快照。相同内容通过哈希复用归档 ID，供模型通过 `read_history` 安全分页检索。
4. **`compaction_progress`（摘要检查点）**：
   记录已成功完成的结构化摘要内容、来源指纹及进度游标。当后续任务失败或中断时，已保存的有效摘要可被复用，无需从头重算。

---

## 子会话与结果回传

子 Agent 复用 `sessions`、`runs`、`events` 与规范消息日志，仅额外记录父子关系、调用结果和补充消息。子任务的终态、上下文与结果在同一事务中提交；父会话保存结果通知的检查点时确认投递，子会话保存补充消息的检查点时确认接收。确认按会话所有权限制，避免跨会话读取或误确认。

普通会话列表排除子会话，但可按 ID 查看完整记录。恢复中断调用只记录中断结果，不自动运行旧任务。显式取消父运行会丢弃该运行尚未投递的子结果，子日志仍保留用于查看和明确续跑。删除父会话先取得父与子会话的全部写入锁，再事务性删除记录并清理侧车目录，防止留下孤立子会话。

## 稳定项目身份与跨改名跟踪

为避免项目重命名或同名目录覆盖造成会话混乱，`orbit-store` 实现了基于文件系统底层身份的项目识别：

- **Unix / Linux**：读取目录的底层设备号（`st_dev`）、Inode 号（`st_ino`）以及文件系统创建时间（birthtime）。
- **Windows**：读取卷序列号（`VolumeSerialNumber`）、128 位文件索引（`FileIndex`）与创建时间。

### 改名与移动行为
- **自动找回**：在同一文件系统内对项目目录进行重命名或移动后，从新目录启动 Orbit，系统能够通过硬件身份自动识别原项目，并在 `/resume` 中直接列出历史会话，无需在项目目录内埋入任何 `.orbit` 标记文件。
- **跨盘显式关联**：若跨文件系统移动导致设备标识改变，可通过 CLI 显式绑定：
  ```sh
  orbit projects list
  orbit projects link <项目ID>
  ```

---

## 存储位置与环境隔离

默认情况下，Orbit 数据库完全独立于项目目录，保存在操作系统的标准数据目录中：

- **Linux**：`$XDG_DATA_HOME/orbit/orbit.db`（或 `~/.local/share/orbit/orbit.db`）
- **macOS**：`$HOME/Library/Application Support/orbit/orbit.db`
- **Windows**：`%LOCALAPPDATA%\orbit\orbit.db`

### 安全隔离
- 数据库文件权限在 Unix 上强制为 `0600`，仅当前用户可读写。
- 数据库路径与其 WAL、SHM 辅助文件均受运行时沙箱保护，沙箱内命令或工具无法读取或修改数据库文件。
- 支持通过 `--database <PATH>` 显式指定独立数据库路径（例如用于测试隔离或便携部署）。

---

## 常用查询与调试指令

```sh
# 列出全局所有已知项目
orbit projects list

# 列出当前项目的会话
orbit sessions list

# 查看指定会话的完整调试快照 (JSON 格式，包含事件与上下文)
orbit sessions show <SESSION_ID>

# 查看指定会话的上下文压缩审计日志
orbit sessions compactions <SESSION_ID>
```

---

## 测试与质量验证

```sh
# 运行 orbit-store 单元与集成测试（使用内存/临时 SQLite 验证事务与锁）
cargo test -p orbit-store --offline
```

旁路提问使用独立的 `side_questions` 表，保存已结束的问题、回答和状态，不追加到主会话的消息日志或模型历史。当前旁路上下文通过只读事务读取原始执行检查点与流式事件，不获取写入租约，不触发中断恢复。删除会话时旁路记录随之删除。
