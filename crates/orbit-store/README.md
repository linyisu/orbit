# orbit-store

基于 SeaORM 和 SQLite 的会话、项目与任务存储，使用 WAL、外键和跨进程写入租约。

## 数据与入口

- `remote.rs`：Linux 设备授权的私有不可变登记；只存凭据摘要，撤销删除登记并同步落盘。

- [store.rs](src/store.rs)、[store/writer.rs](src/store/writer.rs)：会话读写与运行事务。
- [session_log.rs](src/session_log.rs)、[context_archive.rs](src/context_archive.rs)：规范消息与压缩前原文；压缩只替换模型上下文投影。
- [subagents.rs](src/subagents.rs)、[tasks.rs](src/tasks.rs)、[plans.rs](src/plans.rs)：子 Agent、项目任务与计划。
- [extensions.rs](src/extensions.rs)：会话应用的扩展包快照，独立于项目插件的历史登记。
- [migrations/](src/migrations/)：SeaORM Rust 迁移；已发布迁移保持稳定，后续变更追加版本。

会话、检查点和子任务结果按事务提交；运行终态区分成功历史、失败和取消。
模型任务和记忆列表通过 ID 升序的有界查询续读；界面的任务排序及完整管理接口保持各自用途。
记忆检索支持分类筛选与中文相邻双字匹配，仍是词法检索。数据库预筛选后最多对 512 条较新候选排序；
候选超预算时明确返回 `search_truncated`，不声称搜索覆盖全部记忆。
会话 writer 退出时显式释放租约，子进程临时继承的描述符不能延长该 writer 的所有权。
运行投影由 store 维护证据和游标，调用方只更新会话元数据；恢复不会自动重跑中断命令。项目身份跟踪同文件系统的移动与改名，跨盘移动需通过项目关联接口显式处理。

## 存储位置

Linux 默认为 `$XDG_DATA_HOME/orbit/orbit.db`，未设置时使用 `~/.local/share/orbit/orbit.db`，可用 `--database` 覆盖。Unix 数据库权限为 `0600`。

会话扩展快照随会话恢复，删除会话时通过外键级联释放；未应用的全局定义不写入会话数据库。旧项目插件登记保留，但不再自动启用。

持久图片按内容去重并记录会话归属，删除最后一个引用会话后回收。文档页图和命令原始日志使用临时存储，旁路提问只保存在内存中。

## 验证

```sh
cargo test -p orbit-store
```

[项目总览](../../README.md)
