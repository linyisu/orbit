# orbit-model

[项目总览](../../README.md) · [开发与整理说明](../../docs/development.md)

`orbit-model` 是 Orbit 的多供应商模型接入与流式协议抽象库。它为上层 Agent 和 CLI 提供统一的消息格式、流式事件管道、思考推理控制以及工具调用协议适配，隔离各服务商底层 API 的格式差异。

## 共用策略与调用边界

图片字节上限由 [types.rs](src/types.rs) 的 `Attachment::MAX_IMAGE_BYTES` 统一维护。`Attachment::read_image_bytes` 校验普通文件，并限制实际读取字节数；模型编码、附件导入和 TUI 解码共用这一入口。格式解码和缩放由调用方负责，SVG 的终端预览不代表模型协议接受 SVG 附件。

[discovery.rs](src/discovery.rs) 拥有模型目录请求和分页的超时策略；[account_catalog.rs](src/account_catalog.rs) 合并账号目录能力。CLI 负责配置覆盖、缓存和刷新调度，模型库负责协议能力解析，不仅为某一个供应商同步模型。

## 核心设计与模块职责

- **协议抽象与适配** (`client.rs`, `openai/`, `anthropic/`, `google.rs`):
  - 统一将各供应商的专有 API 映射为标准化请求与响应流。
  - 支持五类核心 API 格式：
    1. `openai_chat_completions`：标准 OpenAI Chat Completions 协议（`/chat/completions`）。
    2. `openai_responses`：OpenAI Responses API 协议（`/responses`）。
    3. `chatgpt_codex`：ChatGPT Codex 协议（基于 SSE 的流式端点）。
    4. `anthropic_messages`：Anthropic Messages 协议（`/v1/messages`）。
    5. `google_code_assist`：Google Cloud Code Assist / Gemini CLI 协议。
- **流式事件流** (`types.rs`):
  - 统一解析 SSE 数据分块，向调用者输出类型化 `StreamEvent`：
    - `TextDelta`：模型生成的增量文本内容。
    - `ReasoningDelta`：模型的内部思考链（Reasoning / Thinking）增量文本。
    - `ToolCallDelta`：增量工具调用字段与可选 `metadata`；Google thought signature 通过工具调用元数据传递。工具开始/完成事件属于 Agent 生命周期，不是模型 `StreamEvent`。
    - `Completed`：响应结束标记，携带结束原因（`stop`、`tool_use`、`max_tokens` 等）。
    - Usage 是 `Completed.usage` 的可选字段，不是独立流事件。
- **思考推理适配器** (`reasoning.rs`):
  - 针对不同厂商的思考配置提供标准化抽象，支持 `none`、`minimal`、`low`、`medium`、`high`、`xhigh`、`max` 七档思考强度。
  - 自动转译为目标协议参数：OpenAI Chat 的 `reasoning_effort`、OpenAI Responses 的 `reasoning.effort`、Anthropic Messages 的 `thinking` 与 `output_config.effort`、Google 的 `thinkingLevel`。
- **多模态与视觉支持** (`types.rs`):
  - 支持本地图片附件（PNG、JPEG、GIF、WebP，最大 20 MiB）。
  - 根据模型的 `supports_vision` 声明进行动态投影：视觉模型发送原生图片 content part，非视觉模型自动回退为结构化文字占位，避免非法请求。
- **模型发现与内置目录** (`discovery.rs`, `account_catalog.rs`, `antigravity_models.json`):
  - 提供远程 `/models` 接口探测与解析能力。
  - 内置主流账号体系（如 Antigravity）的预设兼容模型清单，提供开箱即用的上下文容量与视觉能力声明。

## 统一数据结构

### 消息体系 (`ChatMessage`)

下面仅示意常用字段；完整定义以 `types.rs` 为准。

```rust
pub enum ChatMessage {
    System { content: String },
    User { content: String, attachments: Vec<Attachment> },
    Assistant { content: Option<String>, tool_calls: Vec<ToolCall> },
    Tool { tool_call_id: String, content: String },
}
```

- 保证工具调用（`Assistant { tool_calls }`）与工具回执（`Tool { tool_call_id, content }`）的协议配对完整性。
- `User` 消息可携带图片附件（`Attachment::Image`），请求编码支持附件大小/MIME 校验；CLI/tools 的图片导入路径额外解码验证内容并按需缩放。

### 工具定义 (`ToolDefinition`)

符合 JSON Schema 规范的工具描述结构，自动转译为各协议的 Function Calling 定义格式。

## 配置与使用示例

在 Orbit 的 `config.toml` 中，通过模型档案显式声明各项参数与推理能力：

```toml
[providers.openai]
api_format = "openai_responses"
base_url = "https://api.openai.com/v1"
credential = "openai-key"

[[providers.openai.models]]
id = "gpt-5.6-sol"
context_tokens = 128000
max_output_tokens = 16384
supports_vision = true
reasoning = { adapter = "openai_responses", efforts = ["low", "medium", "high"], default = "high" }

[providers.google]
api_format = "google_code_assist"
base_url = "https://cloudcode-pa.googleapis.com"
credential = "google"

[[providers.google.models]]
id = "gemini-2.5-pro"
supports_vision = true
reasoning = { adapter = "google", efforts = ["minimal", "low", "medium", "high"], default = "medium" }
```

### 容量与预算模型

- `context_tokens`：声明模型支持的最大总上下文窗口大小。
- `input_context_tokens`：针对输入输出配额独立的模型（例如 DeepSeek 或部分新型 API），明确指定独立输入上限，避免双重扣除输出预算。
- `max_output_tokens`：单次请求允许生成的最大输出 token 数量。
- `reasoning.extra`：推理配置下的扩展 TOML 表。CLI 校验禁止覆盖核心请求字段及已配置的 effort；库内的合并函数本身可覆盖字段，直接调用库的调用方需要自行约束扩展内容。

## 测试与质量验证

```sh
cargo test -p orbit-model --offline
```

单元测试覆盖各 API 格式的请求体序列化、SSE 流式事件分包解析、多模态附件编码、思考等级转换以及工具签名提取。
