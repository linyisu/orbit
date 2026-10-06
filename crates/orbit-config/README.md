# orbit-config

[项目总览](../../README.md)

客户端与服务共享的配置解析、校验、模型目录和连接编辑。配置按提供的凭据目录解析，生成各领域模块使用的运行选项。

## 连接与模型选择

`[providers.<连接名称>]` 描述 API 协议、地址、认证引用和可用模型；`models` 中的每项描述模型 ID、容量及能力。模型的完整标识为 `连接名称::模型 ID`，列表顺序仅用于配置组织。

客户端在首次使用时展示模型选择器，并记住用户确认的连接和模型。新建本地、SSH、WSL 会话使用上次选择；恢复会话使用会话保存的模型。上次选择已从配置中移除时，启动会重新展示选择器。命令行通过 `--model 连接名称::模型 ID` 完成首次选择。

选择记录存放在认证目录相邻的 `preferences/model-<配置路径摘要>.json`，内容为 `provider` 和 `model`，按本地配置文件隔离。配置文件描述可用资源，会话记录保存当前使用的模型，客户端偏好保存下次新建会话的选择。

## 模型目录

模型接口提供的能力用于更新模型目录；对于只返回模型 ID 的官方 DeepSeek 接口，Orbit 为 `deepseek-flash` 和 `deepseek-v4-pro` 补充已核对的 `none / low / high / max` 思考档位，默认 `high`。手动管理的模型列表同样获得这些默认能力，显式配置的思考档位优先；其他端点和未知模型的思考协议需由服务端或连接高级设置提供能力。

TUI 每秒检测配置文件变化，并每 15 分钟异步刷新所有连接的模型目录（账号登录、API Key 及无需认证的自定义服务）；`/model refresh` 可立即刷新并打开模型选择器。能力变化同步到所有标签，保留仍受支持的当前思考档位；正在进行的回复使用原客户端和额度，结束后应用更新。自动更新不会打开选择器。

各连接独立刷新，失败时保留对应缓存或现有目录；缓存按服务地址、协议和认证身份隔离。普通 API 会读取完整分页，并保留接口提供的能力信息；Claude 的容量、图像输入与 adaptive thinking 思考档位直接来自模型接口。Google Gemini CLI 从配额接口补充模型 ID，缺少某个配额项不会删除已配置模型。

公开模型规格来自 [models.dev](https://models.dev/)，由 [model_metadata.rs](src/models/model_metadata.rs) 补齐 API 模型缺少的上下文、输入与输出容量、视觉能力，以及已支持的官方思考档位。服务地址和模型 ID 必须精确匹配；共用地址的目录若对同一模型给出冲突规格，则不自动采用。优先级为显式配置、账号或 API 返回的能力、公共规格；公共规格不会新增可用模型，也不代表订阅额度或访问权限。输出容量仅作为上限，不提高用户设置的输出预算。

公共规格缓存位于认证目录下的 `model-metadata/catalog.json`，有效期 24 小时。刷新复用网页工具的代理和地址检查，不发送账号凭据，等待最多三秒；失败时使用旧缓存或 [内置快照](src/models/model_metadata.json)。所有 API 连接均关闭 `auto_refresh_models` 时不联网更新规格。维护者可运行 `orbit model-metadata crates/orbit-config/src/models/model_metadata.json` 更新内置快照；导出命令遵循 `[web]` 配置，保留来源信息；MIT 许可见 [LICENSE.models-dev](src/models/LICENSE.models-dev)，不需要登录。快照按模型逐行保存，方便审查数据差异。

[账号起始目录](src/models/account_models/bootstrap.json)作为 OAuth 账号首次连接时的离线起始目录；后续模型可用性由账号接口刷新，API 模型由对应服务的发现接口提供，不能用初始目录推断全部支持模型。

通过 `/connect` 手动启用的列表（`managed_models = true`）保持模型选择与显式能力设置，仍同步这些模型缺少的能力；自动目录会同步新增与下线的模型。服务不提供模型目录时，可在对应 `[providers.<名称>]` 下设置 `auto_refresh_models = false`，继续使用手动配置。
