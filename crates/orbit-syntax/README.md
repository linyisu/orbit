# orbit-syntax

[项目总览](../../README.md)

`orbit-syntax` 为 TUI 和 GUI 提供同一份 Syntect 语法包和语言识别规则，不依赖任何界面或应用服务。颜色、文字样式、选区和排版由前端负责。

## 语言识别

`syntax_for_label` 接受代码围栏里的语言 ID、别名、显示名称和文件名，并处理大小写、`language-` 前缀、`{.语言}` 和标签后的属性。`syntax_for_path` 根据文件名或扩展名识别文件，无需读取文件。`label_key` 提供统一的标签标识，`is_markdown_label` 判断 Markdown 标签。

固定版本的 Shiki 语言目录位于 [assets/languages.json](assets/languages.json)，其中 `syntect` 指定实际语法，`null` 表示尚无对应语法。已知但不支持的语言不会被相近扩展名误认；例如 `v` 是 V 语言，`module.v` 才按 Verilog 文件识别。Verilog 与 SystemVerilog 使用各自的语法。

## 构建与渲染约定

[build.rs](build.rs) 在构建时链接 two-face 语法，并加入 Markdown 行内扩展和 Visual Basic .NET 语法。`SYNTAXES` 首次使用时只反序列化缓存，不重新链接语法。该语法包接受不带换行符的单行输入；调用方保留跨行解析状态，并在计算原文范围时计入换行字节。

自带的语法、目录来源和许可证保存在 [assets/](assets)。更新目录时需同时检查每个语法目标，并运行 `cargo test -p orbit-syntax`；测试覆盖所有目录 ID、名称、别名及文件识别边界。
