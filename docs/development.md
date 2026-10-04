# 开发与代码整理

[返回项目 README](../README.md)

## 模块边界

按职责定位实现，再决定是否复用。跨 crate 的共享类型和策略应由拥有该语义的 crate 提供；调用方复用接口，不复制定义。局部实现默认私有，需要跨模块调用时使用最窄的可见性。

| 职责 | 实现入口 |
| --- | --- |
| 模型请求与供应商协议 | [model/client.rs](../crates/orbit-model/src/client.rs)、`model/openai/`、`model/anthropic/`、`model/google.rs` |
| Agent 执行与上下文生命周期 | [agent/agent.rs](../crates/orbit-agent/src/agent.rs)、`autocompact.rs`、`compaction.rs` |
| 文件和进程权限 | [runtime/filesystem/workspace.rs](../crates/orbit-runtime/src/filesystem/workspace.rs)、`execution/` |
| 工具参数与回执 | [tools/src](../crates/orbit-tools/src) 中各工具模块 |
| 数据库和消息恢复 | [store/store.rs](../crates/orbit-store/src/store.rs)、`schema.rs`、`session_log.rs`、`execution.rs` |
| CLI 组装和子任务宿主 | [cli/main.rs](../crates/orbit-cli/src/main.rs)、`runtime.rs`、`session.rs`、`subagents.rs` |
| 对话排版、选择与渲染 | [cli/tui/transcript.rs](../crates/orbit-cli/src/tui/transcript.rs)、`render/`、`selection.rs`、`media/` |

## 常量与默认值

- 常量按领域归类，不建立包含所有数值、URL、颜色和超时的杂项文件。值相同不意味着语义相同。
- `SCREAMING_SNAKE_CASE` 命名表达用途；字节、像素、行、条目等数值带单位。时间限制使用 `Duration`，避免混淆秒和毫秒。
- 参数默认值、JSON Schema 上限和实际校验使用同一来源。可调整的用户策略保留在配置中，安全边界与协议限制由所属模块维护。
- 关联数据策略可使用类型的关联常量；跨工具共享的预算放入该 crate 的领域模块。供应商 URL、协议字段及历史数据库版本号留在对应适配器或迁移中。
- `0`、`1`、UTF-8 边界计算和协议状态码等有明确上下文的字面量不机械提取成常量。

目前统一的来源：

| 策略 | 来源 | 调用方 |
| --- | --- | --- |
| 图片 20 MiB 上限与有界文件读取 | `Attachment::MAX_IMAGE_BYTES` / `read_image_bytes` | 模型编码、附件导入、TUI 本地解码、网络图片读取 |
| 文本页 32 KiB、JSON 元数据预留、目录扫描条目预算 | `orbit-tools/src/limits.rs` | 文件读取、搜索、目录浏览、命令日志分页 |
| OAuth code、回调头部与 IO 超时 | `orbit-auth/src/callback.rs` | 通用 OAuth 与 Anthropic 回调 |
| 认证请求超时和 token 回执上限 | `orbit-auth/src/http.rs` | 通用认证请求和 Anthropic 交换 |
| 模型发现超时 | `orbit-model/src/discovery.rs` | 模型目录请求与分页总预算 |
| 执行日志刷盘批量与间隔 | `orbit-agent/src/agent.rs` | Agent 的执行 journal |
| SQLite 锁等待 / 当前 schema 版本 | `orbit-store/src/store.rs` / `schema.rs` | 写连接、WAL 读连接 / 迁移入口 |
| TUI 队列与解码预算 | `tui/media.rs` / `media/decode.rs` | 本地和网络媒体工作线程 |

图片导入最长边与终端栅格最长边虽然都为 2048 像素，仍是两个独立策略。命令尾部输出与工具分页文本虽然都为 32 KiB，也分别属于执行层和工具层；修改其中一个不应隐式改变另一个。

## 逻辑分块与文件规模

函数内部按“解析与校验 → 准备资源 → 执行/等待 → 整理回执”留空行。结构体、impl、函数及独立逻辑阶段之间保持空行；不在每条赋值后添加空行。较长提示词使用 `concat!` 分句，保留原始字节，避免改变注入指纹或行为。

按职责拆文件，避免仅按行数拆出 `part1` / `utils`。保留原入口作为明确的 facade，调用方不需要知道内部文件分布。大量测试移入同名模块目录的 `tests.rs`，仍通过 `use super::*` 检查原模块私有行为，不为测试扩大公共 API。

本轮调整：

- `tui/render.rs` 保留帧与视口协调；`render/tool_cards.rs`、`thinking.rs`、`text.rs` 分别负责工具卡片、思考样式与安全终端文本。
- `tui/attachments.rs` 负责路径、剪贴板与附件状态，不再混在主事件循环中。
- `store/schema.rs` 负责有序数据库迁移；`store/writer.rs` 负责写入租约的运行开始、journal 创建和终态提交。
- `runtime/execution/sandbox/isolation.rs` 负责 Bubblewrap 命名空间、系统挂载和 Seccomp；挂载授权及 inode 检查仍由 `sandbox.rs` 负责。
- Agent、TUI 主循环、渲染器、对话框和沙箱的大段测试移入独立文件，测试模块身份保持一致。

## 重复逻辑审查

| 发现 | 处理 |
| --- | --- |
| 通用 OAuth 和 Anthropic 重复校验回调 URL、state、error、code | 复用 `callback::authorization_code`，保留校验顺序和错误语义 |
| Bing、Google、DuckDuckGo 重复提取 DOM 属性与文本 | 复用 `web_fetch/dom.rs`；各渠道结果筛选和跳转规则继续独立 |
| 图片大小限制分散，导入先无界读取再检查大小 | 共用附件策略，读取最多上限加一字节后拒绝超额文件 |
| 工具页预算与 schema/验证的数值重复 | 共用预算，默认页大小、深度和匹配数由各工具拥有 |
| 测试、UI 子部件、SQLite 迁移与运行协调混在长文件内 | 按职责拆分，并保留调用入口 |
| 一项 ignored 的旧快照测试仍期待越界链接被跳过 | 更新为先验证执行前拒绝，再对无越界链接的快照验证只读和敏感文件屏蔽；未修改执行策略 |

相似代码中保留的边界：文件游标使用文件版本与字节位置，文档和子代理结果使用 Unicode 字符游标，不能强行合并分页语义。各模型协议的请求构造和事件解码也不能因结构类似而共用同一字段映射。Git 挂载计划与真实执行挂载属于不同路径，后者仍需逐项权限和 inode 校验。

主 TUI 事件循环和沙箱挂载授权仍比较长：其中许多分支共享运行状态、取消和权限生命周期。本轮分离了独立部件，未把协调流程拆成大量互相访问状态的通用 helper。后续修改应继续沿事件处理、会话切换与挂载职责拆分，保持取消、回放和授权检查的回归覆盖。

## 验证

常规门禁与 [.github/workflows/ci.yml](../.github/workflows/ci.yml) 一致：

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

本地依赖已缓存时加 `--offline`。测试中的本地 HTTP 服务需要回环监听权限，不需要真实供应商账号。沙箱专项测试依赖 Linux 用户命名空间；编码循环测试还需要 Python、Node 及 `ORBIT_TEST_RUST_TOOLCHAIN` 指向可挂载的 Rust 工具链。不要把普通测试的 ignored 项报告为已验证。

```sh
cargo test -p orbit-tools --test sandbox -- --ignored
cargo test -p orbit-tools --test coding_loop -- --ignored
```

重构时检查最终 diff、原有公共入口和回执结构，优先运行相关回归，再执行工作区门禁。文档中的本地文件链接也应校验存在；新增或移动模块时同步修改相应 README。
