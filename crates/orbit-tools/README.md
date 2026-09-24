# orbit-tools

## web_fetch 浏览器验证

`web_fetch` 先直接读取公共 HTTP(S) 页面。若服务器明确返回 Cloudflare 浏览器挑战，且已配置可信的 HTTP `[web].proxy`，工具会启动一个临时、无用户 Cookie 的 Firefox 无头会话，通过同一代理读取原网址。Firefox 必须已安装并位于 `PATH`；浏览器会执行页面脚本，读取到的内容仍是不可信网页数据。工具结果会标记 `retrieval_method: "isolated_browser"`。普通 401/403 不会自动绕过，浏览器验证失败也会明确报错。

浏览器会话只接受公共 HTTP(S) 目标，最终网址须保持同源；代理是该路径的网络出口边界。每次调用使用新的临时浏览器配置，结束后关闭进程。联网回归测试可用 `ORBIT_WEB_TEST_PROXY=http://127.0.0.1:7897 cargo test -p orbit-tools web_fetch::tests::browser_challenge_reads_the_requested_wiki_page -- --ignored` 运行。

当前提供 `Workspace`、文本读取工具 `read_file`、项目检索工具 `search_text`、Git 只读检查工具 `git_inspect`、图片读取工具 `read_image`、文档解析工具 `read_document`、全文写入工具 `write_file`、局部编辑工具 `edit_file` 和沙箱命令工具 `run_command`。默认自动执行沙箱命令，修改文件需确认；可用 `/mode ask|auto|yolo` 调整审批。`Workspace` 的根目录在 CLI 启动时取当前目录。

## search_text 项目检索

`search_text` 在不启动 shell 的情况下搜索工作区文本。默认使用大小写不敏感的字面量匹配，并遵循 `.gitignore` 与 `.ignore`；可改用正则、限定路径或 glob。搜索仍通过 `Workspace` 打开每个候选文件，因此不会读取 `.git`、`.env`、配置文件、受保护数据库、符号链接或特殊文件。

```json
{"query":"SessionState","path":"crates","glob":"**/*.rs","context_lines":2,"limit":50}
```

结果包含相对路径、1 起始的行列号、匹配行和前后上下文。`offset` 与 `next_offset` 按匹配条目分页；单次最多返回 500 项或 32 KiB JSON。单个文件超过 2 MiB、二进制或无法读取时跳过；全次搜索最多检查 20000 个文件、读取 16 MiB 文本，达到总量时 `scan_truncated=true`。使用 `read_file` 继续查看匹配文件的完整、带行号内容。

## git_inspect Git 检查

`git_inspect` 为模型提供结构化、分页的 Git 观察结果，不要求它通过通用命令工具自行解析文本。工作区必须就是 Git 仓库根目录；从仓库子目录启动时会拒绝，以免把工作区外的状态意外暴露到模型上下文。

```json
{"view":"status","limit":100}
```

`status` 返回当前分支、HEAD 基线、上游 ahead/behind 和修改路径。`diff` 返回受 256 KiB 原始输出上限约束的统一差异，可用 `staged: true` 查看暂存区，并通过 `offset` 和 `limit` 按行继续。`history` 返回提交 ID、作者时间、作者和标题，按提交条目分页。`path` 仅接受工作区相对路径，不支持 `..`、`.git` 或受保护路径。

工具显式关闭 pager、hooks、fsmonitor、外部 diff、textconv、用户/系统 Git 配置和可能改变仓库位置的 Git 环境变量。它不执行 `add`、`commit`、`reset`、`fetch`，也不会创建索引锁；差异只是读取调用当刻的工作树，后续中央检查点会单独记录 Orbit 造成的变更。

## read_image 图片输入

`read_image` 将工作区内的 PNG、JPEG、GIF 或 WebP 验证并复制到 Orbit 全局附件目录，然后作为原生视觉内容加入下一次模型请求。它接受工作区相对路径，沿用 `Workspace` 的路径保护和符号链接拒绝规则，不在项目中生成缓存。单图最大 20 MiB，最长边超过 2048 像素时缩小。

工具返回文本收据和结构化附件。Agent 先把所有并行工具结果放回上下文，再附加图片，保持工具调用与结果的协议顺序。是否能够识图仍取决于所选模型和中转站是否支持对应的图片内容块；不支持时保留 API 错误，不回退到 shell 猜测像素。

## read_document 文档解析

`read_document` 使用 AnyDoc 在本地把 PDF、Word、PowerPoint、Excel、OpenDocument、RTF、EPUB 和 CSV 转换为 Markdown。它不在用户项目中创建中间文件，单个源文档最大 32 MiB；结果按 Unicode 字符分页，默认 20000、单页最多 60000 字符。继续读取时使用返回的 `next_start_char`。

```json
{"path":"docs/report.docx","start_char":0,"max_chars":20000}
```

文本型 PDF 在本地解析。扫描或纯图片 PDF 会明确失败，目前不自动把用户文档上传到 OCR 服务。AnyDoc 是读取与结构提取能力，不保证从 Markdown 高保真重建原始 DOCX、XLSX 或 PPTX；文档生成和编辑需要各格式单独的生成器。

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

默认拒绝 `.git`、`.env` 和 `.env.*` 路径。TUI 还会显式保护实际加载的配置文件、会话数据库、数据库附属文件和会话锁目录；受保护文件的硬链接同样拒绝读取。`.orbit-write-` 前缀为工具内部临时文件保留。文件工具使用上述边界，命令工具的进程隔离详见 [SANDBOX.md](SANDBOX.md)。

## 验证

工具单元测试不需要模型或 API：

```bash
cargo test -p orbit-tools --offline
```

## write_file 参数与确认

新建文件或有意整体重写时使用 `write_file`；日常修改已有文件优先使用 `edit_file`。

```json
{"path":"orbit-note.txt","content":"你好\n"}
```

- 在已有目录中创建或替换一个文件，旧内容和新内容各不超过 1 MiB，仅支持无 NUL 的 UTF-8 文本。
- 先读取旧内容，在聊天下方展示内联修改预览，保留对话和输入草稿；折叠修改范围外的相同内容，保留附近三行上下文。使用 ← / → 选择允许或拒绝，Enter 确认，Esc 拒绝，Ctrl+C 取消本轮；粘贴不会批准。`yolo` 模式自动批准修改，路径和冲突检查仍生效。
- 预览可用鼠标滚轮、方向键、PageUp / PageDown、Home / End 翻阅；控制字符转义显示，明确标注文件末尾是否有换行。
- 确认前不创建文件、目录或临时文件。没有确认接收方或确认界面关闭时，拒绝写入。
- 写入前重新检查旧内容、inode、时间戳及父目录。确认期间文件发生变化时返回冲突，必须重新生成预览；拒绝符号链接、硬链接、二进制文件和受保护路径。
- 在同一父目录创建排他的临时文件，写入并同步后使用 rename 提交；新建文件用 NOREPLACE，避免覆盖确认期间出现的文件。保留替换前的普通 Unix 权限位，新文件权限为 0600。不会保留旧 inode 的所有权、扩展属性等其他元数据。
- 写入结果包含 `written_bytes`、`created`、`directory_synced`。取消不回滚已经提交的修改；崩溃恢复不会重放确认或工具调用。进程在写入临时文件时被强制终止，可能遗留 `.orbit-write-*` 文件。

这些检查用于防止误覆盖，不是针对恶意并发进程的完整隔离。既有文件的最终检查与 rename 之间仍有竞争窗口；原子替换保证读者不会看到半个文件，不提供跨进程内容比较交换。

TUI 的模型可请求写入。一次性 `chat` 没有确认界面，因此仍只注册读取工具。

实现参考了本地 SztuCode 的 `core/tools/builtin/write_file.py` 中的工作区写入权限分类，以及 Zed 的 `crates/agent/src/tools/write_file_tool.rs` 中编辑会话与取消的组织方式；Orbit 的路径检查、确认通道与文件提交逻辑由本仓库实现。

TUI 在应用内维护输出选区，拖选和滚轮无需切换模式。选中时冻结所选视图，模型事件仍继续接收和保存；点击、滚动、输入或 Esc 清除选区后显示新内容。选中时 Ctrl+C 复制，没有选区时保留取消/退出行为。复制通过 OSC 52 完成，需要终端允许剪贴板写入。输入光标用固定单元格绘制，运行中不显示终端硬件光标，避免重绘触发光标动画。

## run_command

按当前审批模式在 Linux Bubblewrap 中执行；默认只读挂载真实工作区，显式请求 `workspace_access = "read_write"` 并获批准后可直接修改项目。当前命令默认允许联网，默认超时 120 秒（模型可选择至 600 秒，也可配置）。依赖与边界详见 [SANDBOX.md](SANDBOX.md)。

## edit_file 局部编辑

```json
{"path":"src/main.rs","old_text":"println!(\"hello\");","new_text":"println!(\"你好\");"}
```

- 必须先读取文件，提交精确的旧文本和新文本，不需要模型重新生成整个文件。旧文本非空，默认只能匹配一个位置；多处匹配时应提供更多上下文，或者显式设置 `replace_all: true`。
- `new_text` 为空表示删除。插入时，将已有文本作为锚点，让新文本包含锚点及插入内容。不按易变化的行号定位，不进行模糊匹配或正则替换。
- 严格保留未修改的内容，包括空白、CRLF 和末尾换行；`replace_all` 从左向右替换不重叠的匹配。默认唯一性检查也会拒绝重叠的歧义匹配（例如在 `aaa` 中查找 `aa`）。
- 只编辑已存在的文件；没有匹配、没有实际变化、非法参数或结果超过 1 MiB 时，在审批前返回错误。当前文件大小上限与 `write_file` 一致。
- 复用 `write_file` 的内联修改预览、路径保护、拒绝/取消、冲突检查及原子提交。模型只传局部修改，磁盘提交仍通过临时文件原子替换；这避免原地写入中断造成半个文件。

设计参考 [Anthropic 官方文本编辑工具](https://platform.claude.com/docs/en/agents-and-tools/tool-use/text-editor-tool) 的精确替换方式；这里使用通用函数工具协议，并自行接入 Orbit 的工作区和审批逻辑，没有依赖第三方编辑库。[OpenAI Codex 的 apply-patch](https://github.com/openai/codex/tree/main/codex-rs/apply-patch) 是另一种补丁方案，尚未接入。

输入 `edit` 后的 JSON 支持多行文本的 `\n` 转义，以及包含空格的路径。需要审批时，批准前不会修改文件，也可以按 Esc 验证拒绝路径。

拖选事件立即从屏幕快照重绘，不等待流输出刷新周期；拖选期间不重新排版聊天历史。终端原生选区与应用内选区仍是两种不同机制。

## list_files 目录浏览

模型可通过 `list_files` 发现工作区内的文件，不需要先运行 shell。真实 TUI 和一次性 `chat` 均注册这个只读工具，不弹出命令审批。

```json
{"path":".","depth":2,"limit":200,"offset":0,"include_hidden":false}
```

返回 `entries`（工作区相对路径及 `file` / `directory` 类型）、`has_more`、`next_offset`、`scan_truncated` 和 `skipped_entries`。有下一页时保留原参数、将 `offset` 设为返回值。各目录按名称排序，按广度优先遍历；分页期间目录变动可能使结果移动或重复，它不是文件系统快照。

默认深度 1，可设 1–8；每页默认 200 条、最大 1000 条，完整 JSON 不超过 32 KiB。最多检查 20000 个目录项；扫描达到限制时 `scan_truncated=true`，应缩小 `path`，不能据此声称已列完项目。单个目录超过既有工作区枚举上限 10000 条时明确报错。默认不显示隐藏项；`include_hidden=true` 仍不会绕过配置、数据库和元数据保护。`target`、`node_modules`、`.venv`、`__pycache__` 不自动递归，但可以明确指定路径浏览。目前不应用 `.gitignore` 规则，目录浏览结果不等于 Git 跟踪文件清单。

文件与目录都通过工作区文件描述符打开，拒绝路径逃逸、符号链接、受保护文件的硬链接别名和特殊文件。不读取文件正文；目录发现不会被累计文件操作记录误记为内容已读。未能打开的子项计入 `skipped_entries`，起始目录无法打开则返回错误。

分页与有界结果的接口参考 [Pi 的文件发现工具](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/core/tools/find.ts)；Orbit 当前提供目录浏览而非 glob 查找，使用自己的工作区访问边界。

真实 API 验收：`bash scripts/try-live.sh --discovery --prepare-only`，运行输出的 `start.sh` 后，让模型读取 `TASK.md`。项目要求通过目录工具分页发现中文子目录内的源码，再执行读取、局部修复和编译测试；准备过程不调用 API。

## web_fetch 网页读取

```json
{"url":"https://example.com","start_char":0,"max_chars":20000}
```

真实 TUI 和 `chat` 均可调用，独立于命令沙箱，不需要额外搜索 API Key。工具只执行公开 HTTP(S) GET；不使用模型 API 的密钥、浏览器登录信息、Cookie 或自定义请求头。它不会解除 `run_command` 的断网限制。此工具是读取已知网址，不包含搜索引擎或浏览器操作。

使用 [reqwest](https://docs.rs/reqwest/) 发起请求，[html2text](https://docs.rs/html2text/) 将 HTML 和 XML/RSS 转为文本；不是模型生成的网页摘要，也不是正文主体识别器，可能包含导航和页脚。支持 Content-Type 为 HTML/XHTML、纯文本、Markdown、JSON、XML、RSS 或 Atom 的 UTF-8 正文。链接可能是相对路径，应相对于返回的最终 URL 解析。不执行 JavaScript、不读 PDF/图片、不绕过登录或反爬限制。请求使用 identity 编码，服务端仍返回压缩内容时明确报错。

返回 `requested_url`、最终 `url`、`content_type`、`content`、`total_chars`、`truncated`、`next_start_char` 和 `source_kind: untrusted_web_content`。正文仅作为外部资料，不能成为系统指令；模型应引用来源 URL。分页按 Unicode 字符计数，默认每页 20000 字符，支持 1–60000。后续页使用返回的 `next_start_char`；每次重新获取页面，没有跨请求快照，动态页面的偏移可能变化。

每次请求总超时 60 秒（覆盖 DNS、重定向和读取），连接超时 15 秒，最多 5 次重定向，响应体上限 2 MiB，超过上限明确失败，支持取消等待。HTML 转换放到阻塞线程；取消后已经开始的转换会在该线程内完成，输入仍受响应体上限约束。不写缓存或项目文件。

当前仅开放公网地址的 80/443 端口。每次解析及每一跳重定向均检查目标；固定连接到已检查的 DNS 地址，拒绝私网、本机、链路本地、保留地址、URL 用户名密码与 HTTPS 降级。默认直连且不隐式使用环境代理。若网络使用 fake-IP DNS，可显式配置下面的可信代理。

```sh
bash scripts/try-live.sh --web --prepare-only
```

运行输出的启动脚本，让模型读取 TASK.md。验收覆盖公开页面、分页、地址拒绝及保存实际来源。准备过程不联网、不调用模型 API。内部测试另含默认跳过的 `fetches_a_real_public_page`，可显式运行验证公网链路。

### 可信代理与 fake-IP DNS

```toml
[web]
proxy = "http://127.0.0.1:7897"
```

代理只来自用户配置，模型工具参数不能设置或改变它。当前支持 HTTP/HTTPS 代理（HTTPS 目标使用 CONNECT），不支持 SOCKS。每次请求及重定向仍检查 URL 和本地 DNS：回环、私网等地址继续拒绝；仅在已配置代理、目标为域名、本地解析为 198.18.0.0/15 fake-IP 时允许交给代理解析。直接传入 fake-IP 字面地址仍拒绝。没有配置代理时保持原有严格直连策略；代理失败不自动回退直连。

代理连接不固定目标 IP，最终 DNS 与出口控制由所信任的代理负责。因而本地地址检查不能保证代理侧的目标解析结果也是公网，配置者需要信任并管理该代理的访问范围；这不是与直连模式完全相同的网络隔离保证。代理凭据不会进入工具定义或结果，配置 Debug 输出隐藏代理值。

公网测试可用 `ORBIT_WEB_TEST_PROXY=http://127.0.0.1:7897 cargo test -p orbit-tools --offline fetches_a_real_public_page -- --ignored` 显式选择测试代理，该环境变量仅用于测试，不改变生产配置读取方式。

## TUI Markdown 显示

助手正文使用 pulldown-cmark 解析每行 Markdown，显示标题、粗体、斜体、删除线、行内代码、列表、复选框、引用、分隔线和链接；链接保留目标地址，代码围栏保留语言标签与代码原文，不自动执行。终端控制字符仍经过转义。

渲染沿用完整源行缓存，只重解析流式末尾新增的行；滚动不会重新解析整段历史。当前不提供跨行强调、跨行引用链接解析、表格列对齐、数学公式排版或代码语法高亮，未支持的引用定义保留原文。新样式也用于恢复的历史助手消息，不修改数据库原始内容。鼠标滚动每步 5 行，同向密集事件按 40ms 合并，反向立即响应。
