# 命令执行环境

`run_command` 使用 Linux Bubblewrap 与 `/usr/bin/prlimit`。需要非特权用户命名空间；启动失败会返回错误，不回退到宿主 Shell。Windows、macOS 暂未提供同等隔离后端。

## 工作区、缓存与产物

原项目直接挂载到 `/workspace`，默认只读。命令显式请求 `workspace_access = "read_write"` 并通过当前审批模式后，可直接修改原项目；这些修改不会随命令结束而丢弃，也不经过 `write_file`、`edit_file` 的逐文件差异确认。受保护路径、Git 元数据及其识别出的硬链接别名在命令启动前遮蔽。工作区中的其他文件，包括 `target`、`node_modules`，仍按挂载权限可见。

`/scratch` 在 TUI 和 `chat` 中按会话保存，恢复会话可继续使用；不同会话隔离。配置了 scratch 时，`HOME=/scratch/home`，写入 HOME 的配置和缓存也会跨命令保留；未配置 scratch 时，`HOME=/tmp/home` 仍是一次性的。后台进程不会保留。编译与测试可以在同一命令内完成，或把需要复用的输出放在 `/scratch`。

容量与验证入口在用户配置中设置：

```toml
[command]
default_timeout_seconds = 120
max_timeout_seconds = 600
max_scan_mib = 64
max_scan_entries = 10000
# 按实际安装位置选择；不会继承 rustup 的宿主 HOME。
# rust_toolchain = "/home/me/.rustup/toolchains/stable-x86_64-unknown-linux-gnu"

# 仅挂载显式配置的目录；模型不能增加挂载。
# dependency_caches = [{ name = "packages", path = "/home/me/offline-packages" }]

[command.verification_commands]
format = "cargo fmt --all --check"
test = "cargo test --offline --workspace --target-dir /scratch/target"
```

`max_scan_mib` 可配置为 1–1024 MiB，`max_scan_entries` 为 1–100000 项，用于命令前后的输入版本核对，不限制直接挂载的工作区大小。扫描失败时命令仍可运行，工具结果会明确给出 `verification_error`，且不能声称输入或输出版本已经核实。旧的 `snapshot_mib`、`snapshot_entries` 配置名暂作别名；`writable_snapshot` 工具参数已拒绝。单个输出文件仍受 64 MiB 的进程文件大小限制。

命名缓存只读挂载在 `/opt/orbit-cache/<name>`。如包管理器需要写缓存，先将需要的部分复制到 `/scratch`，再在命令里显式选择该目录。不会继承宿主凭据、代理或 HOME；**当前命令默认允许联网**。用户应只配置可提供给任务的依赖目录。

`verification_commands` 是用户声明的待执行入口，显示在工具说明里；不会因启动 Orbit 自动执行，也不意味着检查已通过。Python 可以声明 `python3 -m unittest discover -s tests`，Node 可以声明项目的 `npm test` 或 `node --test`；应根据实际清单选择。

需要交付文件时先保存在 `/scratch`，再调用：

```json
{"path":"report.csv","name":"report.csv"}
```

上述参数用于 `export_artifact`。它接受二进制或文本文件，拒绝路径穿越与符号链接，将文件保存到 Orbit 私有产物目录，返回真实宿主路径、字节数与 SHA-256。每次生成独立文件，不覆盖项目或已有产物。限制为单文件 64 MiB、每会话 128 个文件及 256 MiB；达到限制时保留已有文件并拒绝新增。`chat --disable command` 同时禁用执行、命令日志查询、验证状态和产物导出。

命令沙盒遮蔽 `.git`，因此 `run_command` 不能执行 `git init`、`git add` 或 `git commit`。已有 HEAD 提交的仓库可调用 `git_export_patch {}`，将工作树改动和新增文件导出为完整补丁，供用户在宿主环境应用或提交；这不会修改 Git 元数据。尚未初始化的项目可将交付文件写入 `/scratch` 并用 `export_artifact` 导出。`chat --disable git` 同时禁用只读 Git 检查和补丁导出。

### 保护路径与不透明运行时根目录（Opaque Runtime Roots）

为了防止沙箱逃逸或外部敏感数据泄露：
- **常规保护路径（Protected Paths）**：通过 `Workspace::protect` 注册的用户敏感路径（例如宿主配置、凭据目录或独立数据库文件）。如果位于工作区外部，运行时会对其递归扫描并收集后代 inode（深度上限 32，上限 20,000 项）；如果工作区内存在指向这些受保护文件的硬链接别名，沙箱在启动时会自动识别并挂载 `/dev/null`（或只读空 tmpfs）遮蔽（mask）。
- **不透明运行时根目录（Opaque Runtime Roots）**：通过 `Workspace::protect_opaque_runtime_root` 显式注册的运行时内部持久化根目录（例如 `orbit.workspaces`、`command-output`、`artifacts`、`session-locks` 等）。它们本身在沙箱中被阻断访问，也不能作为能力挂载。当工作区或外部能力挂载中出现可能指向该目录的硬链接时，运行时才按设备号扫描其后代 inode 并遮蔽或拒绝别名；若扫描超过 32 层或 20,000 项则拒绝本次访问。没有此类候选硬链接时，不递归扫描历史运行时目录。

## 审批与执行证据

`ask` 逐次审批；`auto` 自动执行离线只读命令和工作区内的文件工具操作，联网命令、Shell 写入及外部挂载需确认；`YOLO` 自动批准请求。这些模式都不取消隔离与文件冲突检查。TUI 的文件审批使用差异弹窗。

每个命令记录退出码、超时、取消、耗时与实际输入快照校验值。完整校验值保存在会话事件里，不塞入模型的工具结果。`verification_status` 和 `/status` 比对当前输入，区分输入未变、已过期及旧记录未知。`/changes` 对比任务开始基线、Orbit 写入收据和当前文件。

输入校验值只证明过滤后的工作区文件版本；命令运行期间工作区可能继续变化。系统工具、依赖缓存、持久 scratch、外部能力挂载及读写命令对真实工作区的修改也可能影响结果。扫描超限或失败时版本证据不可用，工具结果会说明原因。零退出码不等于运行过测试；最终结论必须核对具体命令与输出。旧会话没有开始基线时明确显示未知。

## 隔离和资源限制

- 系统 `/usr`、`/bin`、`/sbin`、`/lib`、`/lib64` 与动态链接缓存只读挂载。独立 PID、IPC、用户和 UTS 命名空间；清空 capabilities，禁止嵌套用户命名空间。只有网络策略为 `deny` 时才创建独立网络命名空间；可在 `[command]` 中设置 `network = "deny"`，默认仍为 `allow`。
- 环境清空，只设置 PATH、HOME、TMPDIR、LANG。`/tmp` 为 64 MiB 临时文件系统，`/dev` 为最小设备集；没有宿主 HOME 或 SSH agent。网络访问由当前策略决定。
- 地址空间 4 GiB、单文件 64 MiB、文件描述符 1024、NPROC 1024。CPU 不另设时间上限，命令受墙钟超时与取消控制。没有 cgroup 总内存预算、系统调用过滤或虚拟机隔离。
- 命令正文放入私有脚本文件，不受工具 8 KiB 或 execve 单参数限制。超时从进程启动开始计算，源快照准备不计入。
- stdout/stderr 各返回最后 32 KiB。完整日志按会话保存，单流最多 16 MiB、会话滚动预算 128 MiB；通过 `read_command_output` 分页回读。截断不会终止进程，工具会报告总字节数与截断状态。
- 取消、超时及父进程退出终止命名空间中的后代。执行检查点先于工具开始持久化；中断恢复不会直接重放副作用，结果未知的操作需先核实。

## 验证

```sh
cargo test --workspace --offline
ORBIT_TEST_RUST_TOOLCHAIN="$(rustc --print sysroot)" cargo test -p orbit-tools --test sandbox -- --ignored
ORBIT_TEST_RUST_TOOLCHAIN="$(rustc --print sysroot)" cargo test -p orbit-tools --test coding_loop -- --ignored
```

专项测试需要在允许用户命名空间的 Linux 环境运行。Rust、Python、Node 闭环夹具真实经历失败、读取、修改、复测通过，检查验证输入版本变化及宿主项目没有构建垃圾。模型决策使用固定脚本以隔离运行时测试，因此不能把它当作真实模型成功率或 SWE-bench 成绩。
