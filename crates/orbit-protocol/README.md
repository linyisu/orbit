# orbit-protocol

[项目总览](../../README.md)

工作区请求、事件封装、版本协商和长度分帧。协议类型使用 Serde 编解码；帧具有大小上限。领域事件和审批载荷通过泛型参数绑定，协议包不依赖运行时、数据库或界面。

协议 20 增加 `Tasks` 请求与结构化 `TaskSnapshot { tasks, changed }`，`changed` 标识本次修改的任务，查询时为空。`TaskAction::Create` / `Update` 支持标题、说明、优先级与依赖；原有 `SessionAction::Task` 文本报告保留，终端与桌面共用同一套任务规则。

`Resources`、`Plugins` 与 `Tasks` 请求可携带 `expected_workspace`，防止会话被其他客户端移动后，旧设置或任务表单写入新的项目。资源与任务操作在会话 actor 执行时校验；插件操作校验后固定使用捕获的工作区。旧客户端省略该字段时保持原有行为。

`search` 提供补全与选择器共用的匹配优先级：前缀、子串、子序列。匹配忽略大小写，同级保持调用方提供的顺序。
