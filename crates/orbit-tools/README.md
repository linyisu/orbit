# orbit-tools

当前提供 `Workspace`、读取工具 `read_file`、全文写入工具 `write_file`、局部编辑工具 `edit_file` 和沙箱命令工具 `run_command`。修改文件和执行命令需要逐次确认。`Workspace` 的根目录在 CLI 启动时取当前目录。

## 调用链

```text
模型生成 ToolCall
  → Agent 发出 ToolStarted
  → ToolRegistry 找到 read_file
  → ReadFile 校验参数
  → Workspace 打开允许读取的文件
  → 返回带行号的 JSON 结果
  → Agent 发出 ToolFinished，并把结果写入模型上下文
  → 模型继续回答
```

`orbit-agent` 只依赖 `Tool` 接口，不需要知道文件怎么打开。文件访问逻辑集中在这里，CLI 负责创建工作区并注册工具。

## read_file 参数

```json
{"path":"Cargo.toml","start_line":1,"max_lines":40}
```

- `path` 必填，使用工作区相对路径。
- `start_line` 从 1 开始，默认 1。
- `max_lines` 默认 200，允许 1–500。
- 文件必须是普通 UTF-8 文本，最大 1 MiB。
- 返回字段包含 `path`、`start_line`、`end_line`、`total_lines`、`content`、`truncated`。
- `content` 最大 32 KiB（JSON 编码后的完整响应可能更大）。超出行数或内容限制时标记 `truncated`；单个超长行也可能被截断。

## 工作区边界

当前文件打开实现面向 Unix，使用 Rust `rustix` 的目录文件描述符与 `openat`。每一级路径都不跟随符号链接，拒绝绝对路径和 `..`，不采用“检查路径后再按原路径打开”的方式。

默认拒绝 `.git`、`.zcode`、`.codex`、`.env`、`.env.*` 和 `config.toml` 路径。TUI 还会显式保护实际加载的配置文件、会话数据库、数据库附属文件和会话锁目录；受保护文件的硬链接同样拒绝读取。`.orbit-write-` 前缀为工具内部临时文件保留。文件工具使用上述边界，命令工具的进程隔离详见 [SANDBOX.md](SANDBOX.md)。

## 无 API 验证

在仓库根目录运行：

```bash
cargo run -p orbit-cli -- tui --demo
```

输入 `read Cargo.toml`：演示模型生成工具调用，工具读取真实文件，再生成明确标注的演示回答。

输入 `read ../outside.txt` 或 `read missing.txt` 可以观察失败路径。`Ctrl+O` 展开工具结果，鼠标滚轮浏览输出，直接拖选文本后 Ctrl+C 复制；鼠标操作不会移动输入光标。

```bash
cargo test -p orbit-tools --offline
```

## write_file 参数与确认

新建文件或有意整体重写时使用 `write_file`；日常修改已有文件优先使用 `edit_file`。

```json
{"path":"orbit-demo.txt","content":"你好\n"}
```

- 在已有目录中创建或替换一个文件，旧内容和新内容各不超过 32 KiB，仅支持无 NUL 的 UTF-8 文本。
- 先读取旧内容，在聊天下方展示内联修改预览，保留对话和输入草稿；折叠修改范围外的相同内容，保留附近三行上下文。`Ctrl+Y` 只批准当前一次写入，`Ctrl+N` / `Esc` 拒绝，`Ctrl+C` 取消本轮。Enter、粘贴内容不会批准。
- 预览可用鼠标滚轮、方向键、PageUp / PageDown、Home / End 翻阅；控制字符转义显示，明确标注文件末尾是否有换行。
- 确认前不创建文件、目录或临时文件。没有确认接收方或确认界面关闭时，拒绝写入。
- 写入前重新检查旧内容、inode、时间戳及父目录。确认期间文件发生变化时返回冲突，必须重新生成预览；拒绝符号链接、硬链接、二进制文件和受保护路径。
- 在同一父目录创建排他的临时文件，写入并同步后使用 rename 提交；新建文件用 NOREPLACE，避免覆盖确认期间出现的文件。保留替换前的普通 Unix 权限位，新文件权限为 0600。不会保留旧 inode 的所有权、扩展属性等其他元数据。
- 写入结果包含 `written_bytes`、`created`、`directory_synced`。取消不回滚已经提交的修改；崩溃恢复不会重放确认或工具调用。进程在写入临时文件时被强制终止，可能遗留 `.orbit-write-*` 文件。

这些检查用于防止误覆盖，不是针对恶意并发进程的完整隔离。既有文件的最终检查与 rename 之间仍有竞争窗口；原子替换保证读者不会看到半个文件，不提供跨进程内容比较交换。

TUI 的真实模型和离线演示均可请求写入。一次性 `chat` 没有确认界面，因此仍只注册读取工具。

离线测试：启动 `tui --demo` 后输入 `write orbit-demo.txt 你好`。这会申请**真实写入**当前工作区；确认后输入 `read orbit-demo.txt` 检查结果。再次请求写入同一路径可测试替换或拒绝。

实现参考了本地 SztuCode 的 `core/tools/builtin/write_file.py` 中的工作区写入权限分类，以及 Zed 的 `crates/agent/src/tools/write_file_tool.rs` 中编辑会话与取消的组织方式；Orbit 的路径检查、确认通道与文件提交逻辑由本仓库实现。

TUI 在应用内维护输出选区，拖选和滚轮无需切换模式。选中时冻结所选视图，模型事件仍继续接收和保存；点击、滚动、输入或 Esc 清除选区后显示新内容。选中时 Ctrl+C 复制，没有选区时保留取消/退出行为。复制通过 OSC 52 完成，需要终端允许剪贴板写入。输入光标用固定单元格绘制，运行中不显示终端硬件光标，避免重绘触发光标动画。

## run_command

通过完整命令确认后在 Linux Bubblewrap 中执行，原项目只读、无网络，默认 30 秒超时。离线演示输入 `run pwd` 即可测试。依赖与边界详见 [SANDBOX.md](SANDBOX.md)。

## edit_file 局部编辑

```json
{"path":"src/main.rs","old_text":"println!(\"hello\");","new_text":"println!(\"你好\");"}
```

- 必须先读取文件，提交精确的旧文本和新文本，不需要模型重新生成整个文件。旧文本非空，默认只能匹配一个位置；多处匹配时应提供更多上下文，或者显式设置 `replace_all: true`。
- `new_text` 为空表示删除。插入时，将已有文本作为锚点，让新文本包含锚点及插入内容。不按易变化的行号定位，不进行模糊匹配或正则替换。
- 严格保留未修改的内容，包括空白、CRLF 和末尾换行；`replace_all` 从左向右替换不重叠的匹配。默认唯一性检查也会拒绝重叠的歧义匹配（例如在 `aaa` 中查找 `aa`）。
- 只编辑已存在的文件；没有匹配、没有实际变化、非法参数或结果超过 32 KiB 时，在审批前返回错误。当前文件大小上限与 `write_file` 一致。
- 复用 `write_file` 的内联修改预览、路径保护、拒绝/取消、冲突检查及原子提交。模型只传局部修改，磁盘提交仍通过临时文件原子替换；这避免原地写入中断造成半个文件。

设计参考 [Anthropic 官方文本编辑工具](https://platform.claude.com/docs/en/agents-and-tools/tool-use/text-editor-tool) 的精确替换方式；这里使用通用函数工具协议，并自行接入 Orbit 的工作区和审批逻辑，没有依赖第三方编辑库。[OpenAI Codex 的 apply-patch](https://github.com/openai/codex/tree/main/codex-rs/apply-patch) 是另一种补丁方案，尚未接入。

无 API 测试：启动 `tui --demo`，依次输入并批准：

```text
write orbit-demo.txt 你好，世界
edit {"path":"orbit-demo.txt","old_text":"你好","new_text":"您好"}
read orbit-demo.txt
```

输入 `edit` 后的 JSON 支持多行文本的 `\n` 转义，以及包含空格的路径。批准前不会修改文件，也可以按 Esc 验证拒绝路径。

拖选事件立即从屏幕快照重绘，不等待流输出刷新周期；拖选期间不重新排版聊天历史。终端原生选区与应用内选区仍是两种不同机制。
