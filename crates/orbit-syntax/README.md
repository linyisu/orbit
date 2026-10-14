# orbit-syntax

TUI 与 GUI 共用的 Syntect 语法包、语言别名和文件识别。配色与排版由前端负责。

## 入口

[lib.rs](src/lib.rs) 提供 `syntax_for_label`、`syntax_for_path` 、`label_for_path`、`display_label` 和 `SYNTAXES`；[build.rs](build.rs) 构建语法缓存。[assets/](assets/) 保存语言目录、语法来源和许可证。

已知但不支持的语言保留纯文本。更新目录或语法时一起检查别名、文件名与扩展名识别。

```sh
cargo test -p orbit-syntax
```

[项目总览](../../README.md)
