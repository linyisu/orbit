# 命令执行沙箱

`run_command` 在 Linux 上调用 `/usr/bin/bwrap`（Bubblewrap）与 `/usr/bin/prlimit`。Orbit 的执行、确认、输出收集和生命周期管理均由 Rust 实现。运行环境必须允许非特权用户命名空间；沙箱配置失败时返回错误，绝不回退为宿主 Shell。

```json
{"command":"pwd; rg 'pub fn' crates","timeout_seconds":30}
```

默认 `auto` 模式自动执行沙箱命令；文件修改仍显示内联审批。`ask` 模式对修改和命令逐次确认；`yolo` 模式自动允许文件修改和沙箱命令。模式仅影响审批，不绕过工作区路径保护、文件冲突检查、网络隔离或资源限制。

通过 `/mode ask`、`/mode auto`、`/mode yolo` 设置，也可用 Shift+Tab 循环切换；底栏持续显示当前模式。切换到允许当前操作的模式，也会放行已展示的待审批操作。重启、恢复会话或 `/clear` 后回到默认 auto，不持久保存 YOLO 授权。切回 ask 只影响后续操作，已执行的修改不会回滚。

需要审批时，使用 ← / → 在“允许”和“拒绝”间切换，Enter 确认，Esc 拒绝。默认选中允许，但必须先完成预览绘制；粘贴不会触发确认。Ctrl+C 仍可取消本轮。一次性 `chat` 没有交互确认界面，因此不注册此工具。


## 文件与网络

每次获准后，以 `Workspace` 的无符号链接文件打开逻辑构造临时副本，只读挂载为 `/workspace`：

- 排除受保护的配置、数据库、会话锁、`.git` 等元数据及其已知硬链接别名。
- 不复制符号链接、套接字、FIFO 或设备。
- 排除 `target`、`node_modules`、`.venv`、`__pycache__`。
- 最多复制 64 MiB、遍历 10000 个条目及 64 层目录，超过限制直接失败。
- 返回实际复制文件数和跳过条目数。副本不是文件系统原子快照；准备期间发生的源文件变动可能影响读取结果。

原项目不挂载到沙箱中，也不回写副本。命令不能修改原项目；文件修改使用 `write_file` 的内容预览流程。副本未保留文件可执行位，项目脚本可以使用 `sh script.sh` 等显式解释器运行。

## 在临时可写副本中构建和测试

```json
{"command":"mkdir -p build && cc main.c -o build/main && ./build/main","writable_snapshot":true,"timeout_seconds":60}
```

`writable_snapshot` 默认为 `false`。设为 `true` 时，将过滤副本只读挂载为 `/orbit-source`，再复制到独立的 128 MiB tmpfs `/workspace` 中执行命令。这个总容量包括源文件和所有构建产物；单文件仍受 64 MiB 限制。网络、宿主目录和环境变量的隔离保持不变。复制准备也计入命令超时，准备失败不会继续执行用户命令。

命令可以修改副本、创建构建目录和运行测试，但所有修改在进程退出后丢弃，不回写原项目，也不在下一次命令中复用。因此构建与依赖它的测试必须组合为同一条命令。审批界面会明确标记临时可写和清理行为。

此模式适用于系统已安装工具、无需联网且体积较小的构建。尚未提供持久缓存、依赖下载、宿主 home 下的工具链或原项目直接可写挂载；不是所有项目都能在当前限制下构建。

沙箱只读挂载系统 `/usr`、`/bin`、`/sbin`、`/lib`、`/lib64` 和动态链接缓存；使用独立的进程、网络、IPC、用户和 UTS 命名空间，禁止嵌套用户命名空间，清空 Linux capabilities。`/proc` 只展示沙箱进程，`/dev` 为最小设备集合，`/tmp` 为本次执行独占的 64 MiB tmpfs。

子进程环境被清空，只设置 PATH、HOME、TMPDIR、LANG。不继承代理变量、SSH agent socket 或 API Key 等环境变量；宿主 home、临时目录和网络均不可访问。宿主用户目录安装的 Cargo、Node 版本管理器和缓存不会出现；`.git` 不可见，因此不能直接执行依赖真实仓库元数据的 Git 操作。

## 限制与生命周期

- 命令字符串最多 8 KiB，超时默认 120 秒，模型可选择至 600 秒；可在配置的 `[command]` 中通过 `default_timeout_seconds`、`max_timeout_seconds` 覆盖；超时从进程启动开始计算。
- stdout / stderr 分别最多 32 KiB。达到上限后继续排空管道并终止进程，不让无限输出持续占用内存。
- 输出保留退出码、超时、取消及截断标志，返回模型；TUI 将控制字符显示为转义文本，避免输出被终端当作控制序列执行。
- 超时、取消会终止 Bubblewrap；独立 PID 命名空间与父进程死亡处理负责清理后代进程。运行结果和工具事件随会话保存，恢复不会重跑命令。
- 每个进程限制 CPU 时间为本次命令超时向下取整加 1 秒、地址空间 4 GiB、文件大小 64 MiB、文件描述符 1024 个；NPROC 限额 1024 是按真实用户计数，可能受宿主已有线程数量影响。

这里没有 cgroup 总量预算、系统调用过滤或虚拟机隔离，不能声称防御所有拒绝服务和内核漏洞。系统工具和内核属于可信基础，命令遵循当前会话的审批设置。

## 离线验证

```sh
cargo run -p orbit-cli -- tui --demo
```

输入 `run pwd` 或 `run ls`，检查确认界面后批准。输入 `run echo changed > Cargo.toml` 会在只读副本中失败，原文件保持不变。

输入 `build mkdir -p build && echo hello > build/result.txt && cat build/result.txt`，可测试临时可写副本。随后输入 `run test ! -e build/result.txt && echo discarded`，确认产物没有写回原项目。这里 `build` 是离线演示的输入前缀，实际工具仍为 `run_command`。

普通测试和需要命名空间的专项测试分开运行：

```sh
cargo test --workspace --offline
cargo test -p orbit-tools --test sandbox --offline -- --ignored
```

专项测试覆盖文件保护、宿主隔离、网络阻断、输出限制、取消、超时及后台后代进程清理。沙箱无法在当前环境启动时，这些显式测试失败，不会伪造成功结果。

设计参考了本地 SztuCode `core/tools/builtin/bash.py` 的命令参数和输出限制，以及 Zed `crates/sandbox/src/linux_bubblewrap.rs` 的命名空间、只读挂载和失败关闭策略。本实现使用过滤副本，不直接沿用它们的工作区授权范围。
