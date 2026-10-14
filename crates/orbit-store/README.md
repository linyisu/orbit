# orbit-store

基于 SeaORM 和 SQLite 的会话、项目与任务存储，使用 WAL、外键和跨进程写入租约。

## 数据与入口

- [store.rs](src/store.rs)、[store/writer.rs](src/store/writer.rs)：会话读写与运行事务。
- [session_log.rs](src/session_log.rs)、[context_archive.rs](src/context_archive.rs)：规范消息与压缩前原文；压缩只替换模型上下文投影。
- [subagents.rs](src/subagents.rs)、[tasks.rs](src/tasks.rs)、[plans.rs](src/plans.rs)：子 Agent、项目任务与计划。
- [migrations/](src/migrations/)：SeaORM Rust 迁移；已发布迁移保持稳定，后续变更追加版本。

会话、检查点和子任务结果按事务提交；恢复不会自动重跑中断命令。项目身份跟踪同文件系统的移动与改名，跨盘移动可用 `orbit-cli projects link ID` 显式关联。

## 存储位置

Linux 默认为 `$XDG_DATA_HOME/orbit/orbit.db`，未设置时使用 `~/.local/share/orbit/orbit.db`，可用 `--database` 覆盖。Unix 数据库权限为 `0600`。

持久图片按内容去重并记录会话归属，删除最后一个引用会话后回收。文档页图和命令原始日志使用临时存储，旁路提问只保存在内存中。

```sh
orbit-cli sessions list
orbit-cli sessions diagnostics SESSION_ID
```

## 验证

```sh
cargo test -p orbit-store
```

[项目总览](../../README.md)
