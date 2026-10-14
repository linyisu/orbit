# Orbit 维护约定

Orbit 是 Rust workspace，包含 CLI、TUI、GPUI Kit 桌面客户端和工作区服务。
先读根 README 和所改 crate 的 README；依赖边界以 `orbit-xtask` 的检查为准。

## 开始修改

- 先检查工作区差异，阅读完整的目标函数、调用方和已有测试。
- 按能力归属修改：客户端负责交互，service 协调任务，agent 驱动模型与工具，
  runtime 管理执行与文件边界，store 持久化，protocol 定义两端契约。
- 共用行为放在已有的责任模块；不为一次调用创建抽象、通用工具箱或新 crate。
- 格式整理与行为变更分开审阅；保留公开接口、协议字段、错误语义和执行顺序。

## 格式与组织

- 使用 stable Rust 和根 `rustfmt.toml`：100 列、4 空格缩进，链式表达式阈值为 40 列。
- 多步 builder、异步调用和迭代器链按步骤分行；不要压缩闭包中的多个语句。
- 相邻函数和实现块之间留一行空白，相关声明集中放置。
- `json!` 的复杂对象和数组分层排版；宏内部也应能直接审阅字段与分支。
  格式器不会覆盖所有宏，提交前仍需检查宏内部和字符串内容。
- 长提示、错误消息和测试文本在源文件中分段，保持运行时内容逐字一致。
  不为换行改变空白、转义、格式参数或隐式捕获；不可拆分的数据允许例外。
- 导入由 rustfmt 排序；避免仅为了缩短一行而隐藏类型归属或创建无意义别名。

## 文档与注释

- 函数文档使用 `///`，说明职责、输入约束、返回语义和不明显的副作用。
  对外接口按需要补充错误、取消、持久化或权限约定。
- 模块文档使用 `//!` 描述职责与边界。简单转发、显然的访问器和测试无需逐个补注释。
- 实现中的注释只解释局部算法约束、兼容原因、竞态或生命周期；能描述整个函数的
  内容放到函数文档。避免行尾旁注、逐行复述、步骤编号和装饰性分隔线。
- `SAFETY:` 紧邻相关 unsafe 操作，明确有效指针、句柄、线程或布局依据；
  许可、协议来源和必要的局部不变量说明应保留。
- 注释使用简洁英文，用户文案和仓库维护文档使用自然中文。名称保持源码中的写法。
- 注释记录长期约束，变更历史交给 Git；TODO 应指向具体待办或清楚的完成条件。

## 常量与配置

- 常量和静态值使用 `SCREAMING_SNAKE_CASE`；名称表达业务含义，而非 `VALUE`、`LIMIT`。
- 长度和预算明确 `_BYTES`、`_CHARS`、`_TOKENS`、`_PIXELS` 或 `_REM`。
  时间值优先使用 `Duration`，裸数时间必须标明单位；四位以上整数用下划线分组。
- 协议标记、容量上限、超时和重复出现的业务阈值使用具名常量；数值保持原有含义。
- 常量放在最窄的责任作用域。仅供单个函数使用时可在函数内定义；跨函数复用时
  放到所属模块；关联类型的固定值优先作为关联常量。
- 相同数值不代表相同策略。只有语义和调整周期相同的值才共享，避免全局 `constants.rs`。
- `const` 表示编译期值；需要唯一地址、原子状态或延迟初始化时使用 `static`。
  用户可调整的行为应进入既有配置，而非增加新的硬编码常量。
- 不把索引 0、步长 1、清楚的算术或常规布局步骤全部提取成常量。
  公共常量保留兼容名称，通过文档说明单位、边界和用途。

## GUI 与第三方代码

- GUI 使用 GPUI Kit 的实际 API，修改前读当前依赖源码及对应组件文档。
- Entity 持有跨帧状态，render 负责组合视图；I/O、任务启动和业务变更由事件处理。
  使用稳定业务 ID，保留订阅，并明确焦点、滚动和弹窗的归属。
- 主题通过语义 token 和 rem 尺度表达。调整外观或交互时检查实际渲染及相关状态。
- GPUI Kit 使用 Cargo 锁定的 fork 提交，来源见 `patches/README.md`。
  框架修复在 fork 中维护，发布后更新固定提交；不重新复制整包到 vendor。
- vendor 和生成的模型、语法目录保留上游格式、许可和来源。只修改有明确目的的补丁，
  不把它们纳入批量风格重写。

## 验证与交付

```sh
cargo fmt --all -- --check
cargo xtask check
cargo clippy --workspace --all-targets --features orbit-gui/test-support --locked -- -D warnings
cargo test --workspace --features orbit-gui/test-support --locked
```

- 从相关 crate 的检查开始；全仓整理运行 workspace 检查，必要时覆盖 doctest 和传输测试。
- 测试使用独立编造或程序生成的数据。用户消息、截图、附件、日志和个人目录
  不得转成单元测试、示例、截图夹具或手工验收数据。
- 原生 GUI 验收使用 Xvfb、临时 HOME、配置、数据库和工作区，不操作用户桌面或会话。
- 等待命令完成并检查实际输出；说明环境限制、跳过项及验证范围，不把编译成功当作视觉验证。
- 交付前检查最终差异、残留调试代码、文档链接和依赖来源。按功能组织提交，推送遵循用户授权。

## 参考

- [Rust Style Guide](https://doc.rust-lang.org/style-guide/expressions.html)：表达式和调用链。
- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)：命名、函数契约和文档。
- [ripgrep searcher](https://github.com/BurntSushi/ripgrep/tree/master/crates/searcher/src/searcher)：
  聚焦接口用途、约束与边界的函数文档。
- [GPUI Kit Coding Guides](https://gpui-kit.com/docs/coding-guides.md)：GUI 状态与职责归属。
