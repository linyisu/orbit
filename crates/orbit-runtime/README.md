# orbit-runtime

目标环境中的进程执行、工作区文件访问、Git 检查和权限策略。工具层决定参数与审批，本库执行实际操作。

## 入口

- [execution/](src/execution/)：目标 shell 环境、进程组、后台任务、超时、取消与有界输出。
- [filesystem/](src/filesystem/)：文件描述符访问和原子写入。
- [policy.rs](src/policy.rs)、[capability.rs](src/capability.rs)：文件访问策略与外部授权。
- [git/](src/git/)、[verification.rs](src/verification.rs)：Git 分类与执行证据。

工作区指纹默认累计扫描 512 MiB、10000 项，文件内容按 64 KiB 块计算摘要，
不把整个文件装入内存；`max_scan_mib` 和 `max_scan_entries` 可配置。
扫描预算不构成命令执行前提，指纹不可用时命令仍可执行并报告证据缺失。

命令使用目标用户的真实目录、HOME 和工具链，按操作系统用户权限运行。`workspace_access`、`network` 用于审批，不是进程隔离。
文件工具没有内置路径黑名单；YOLO 允许直接访问系统用户可访问的路径，其他模式保留工作区与外部授权边界。
审批模式通过共享策略实时生效，切出 YOLO 不会遗留由该模式产生的文件授权。

`ExecutionEnvironment` 是工作目录和环境变量的唯一来源；`ExecutionRequest` 只携带命令、执行预算及验证输入。

命令输出持续排空并限制保留量。服务为每个会话提供独立的持久日志目录，保存 stdout、stderr 和采集收据，跨任务及服务重启可读取；
容量不足时回收完整命令日志组，删除会话由 store 清理。未绑定持久目录的调用方仍使用临时日志，任务及 I/O 租约结束后删除。
后台进程本身不跨服务重启恢复。
命令结果分别保留退出码、操作系统信号和核心转储标记；信号终止不会伪装成超时或用户取消。

`execution::process::ProcessGroup` 为命令与 MCP 提供进程组所有权；释放时回收仍在组内的子进程，不追踪主动脱离该组的后台进程。

## 验证

```sh
cargo test -p orbit-runtime
```

[项目总览](../../README.md)
