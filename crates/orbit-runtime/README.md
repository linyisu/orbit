# orbit-runtime

[项目总览](../../README.md)

`orbit-runtime` 提供原生命令执行、文件访问策略、后台任务、Git 检查与工作区核验。工具层负责参数与审批，运行时负责实际进程和文件操作。

## 命令执行

`ExecutionRequest` 描述命令、真实工作目录、超时和核验输入；`ExecutionEnvironment` 提供目标用户的 shell 环境；`ExecutionPolicy` 保存进程资源限制和显式环境覆盖。`ExecutionManager` 统一前台执行与后台任务，`NativeBackend` 通过目标 shell 启动进程组。

本地、SSH 和 WSL 均使用目标用户的真实 HOME、PATH、工具链、缓存及项目目录。不创建持久会话 scratch，不映射 `/workspace`，不复制工作区。文件产出由用户管理。模型供应商等敏感环境变量仍经过环境过滤；原生命令的实际文件和网络权限来自操作系统。

审批模式和 Plan 工具限制由服务及工具层处理。`workspace_access`、`network` 是审批声明，不是原生进程的访问隔离。超时或取消时终止进程组；进程资源支持地址空间、单文件大小及文件描述符限制，进程数沿用宿主设置。

## 模块职责

| 模块 | 职责 |
|---|---|
| [execution/backend/native.rs](src/execution/backend/native.rs) | shell 启动、进程组与资源限制 |
| [execution/environment.rs](src/execution/environment.rs)、[shell_environment.rs](src/execution/shell_environment.rs) | 真实路径与目标用户环境 |
| [execution/supervisor.rs](src/execution/supervisor.rs) | 等待、超时、取消与输出排空 |
| [execution/output.rs](src/execution/output.rs) | 有界输出日志与分页读取 |
| [execution/manager.rs](src/execution/manager.rs) | 前台命令和托管后台任务 |
| [execution/scan.rs](src/execution/scan.rs) | 有界工作区内容指纹扫描，不复制文件 |
| [filesystem/](src/filesystem/) | 文件描述符访问、原子写入、保护路径及 inode 校验 |
| [policy.rs](src/policy.rs)、[capability.rs](src/capability.rs) | 文件工具的能力授权与路径策略 |
| [git/](src/git/) | 仓库发现及用于审批的命令分类 |
| [verification.rs](src/verification.rs) | 命令执行与文件指纹核验证据 |

文件工具仍拒绝未授权越界和受保护文件别名；该策略不限制原生 shell 进程。Git 分类用于审批辅助，不能证明任意脚本的实际效果。

## 后台与输出

后台命令由当前服务管理，支持等待、查询和取消，不跨服务重启恢复。输出持续排空，工具结果和临时日志分别受预算限制。原始日志按任务暂存在 `/tmp` 私有目录，通过接口分页读取；任务完成、失败或取消后失效，读写租约释放后删除。异常残留通过进程文件锁在下次创建日志时回收，不影响其他进程仍在使用的目录。

## 测试

```sh
cargo test -p orbit-runtime
```

[native_execution.rs](tests/native_execution.rs) 验证真实目录写入、HOME、子进程、输出日志以及超时和取消时的子进程清理。测试使用临时目录，不依赖 Bubblewrap 或用户命名空间。
