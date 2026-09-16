# orbit-tools

当前提供 `Workspace` 和只读工具 `read_file`。`Workspace` 的根目录在 CLI 启动时取当前目录。

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

默认拒绝 `.git`、`.zcode`、`.codex`、`.env`、`.env.*` 和 `config.toml` 路径。CLI 还会显式保护实际加载的配置文件及其硬链接。这里只实现了文件工具的访问边界，尚未实现进程沙箱或交互式授权系统。

## 无 API 验证

在仓库根目录运行：

```bash
cargo run -p orbit-cli -- tui --demo
```

输入 `read Cargo.toml`：演示模型生成工具调用，工具读取真实文件，再生成明确标注的演示回答。

输入 `read ../outside.txt` 或 `read missing.txt` 可以观察失败路径。`Ctrl+O` 展开工具结果，鼠标滚轮滚动对话。

```bash
cargo test -p orbit-tools --offline
```
