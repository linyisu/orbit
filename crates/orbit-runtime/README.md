# orbit-runtime

`orbit-runtime` 是 Orbit 的底层执行与安全子系统，负责工作区安全抽象、细粒度权限控制、文件系统边界隔离、沙箱容器化执行以及工作区状态验证。

它确保 Agent 能够在合法的已授权边界内自由进行代码编辑、构建和测试，同时通过操作系统级沙箱严格防范权限越界、敏感凭据泄露以及恶意命令攻击。

## 架构与核心子系统

```text
orbit-runtime/
├── execution/       # 执行请求、执行环境、执行策略与沙箱后端抽象
│   ├── request.rs       # ExecutionRequest & ExecutionResult
│   ├── environment.rs   # ExecutionEnvironment (工作区、CWD、环境变量、挂载)
│   ├── policy.rs        # ExecutionPolicy (访问模式、外部授权、网络、工具链)
│   ├── resources.rs     # ProcessLimits & ResourceLimits (内存、文件、进程限制)
│   ├── manager.rs       # ExecutionManager (执行流程生命周期管理)
│   ├── sandbox.rs       # 挂载组装、inode 收集与受保护路径遮蔽
│   └── backend/         # Linux Bubblewrap 隔离后端实现
├── filesystem/      # 工作区安全抽象、路径解析与 Inode 审计
│   ├── workspace.rs     # Workspace 结构体与保护路径注册
│   ├── path_security.rs # 符号链接穿越防护与规范化
│   └── inodes.rs        # Inode 收集、深度限制与硬链接延迟扫描
├── policy.rs        # PolicyEngine、EffectivePolicy、SessionGrantStore
├── capability.rs    # FilesystemCapability、EnvironmentId、NetworkPolicy
└── verification.rs  # FileFingerprint、WorkspaceFingerprint、版本证据对比
```

---

## 1. 统一执行体系 (`execution`)

执行体系将操作的“执行上下文”、“权限范围”与“底层机制”解耦：

- **`ExecutionRequest`**：
  封装纯粹的命令执行意图，包含执行命令行（`command_line`）、指定工作目录（`cwd`）、超时时限（`timeout`）与协作取消令牌（`cancellation_token`）。不负责挂载计算与环境发现。
- **`ExecutionEnvironment`**：
  封装目标执行环境的静态属性：
  - `workspace_root`：工作区根目录的规范路径。
  - `cwd`：经过容器内部路径映射后的有效执行目录（正确映射工作区或外部挂载目标）。
  - `env_vars`：经过严格脱敏的环境变量集合，自动过滤 `OPENAI_API_KEY`、`ANTHROPIC_API_KEY` 等敏感凭据；对 `PATH`、`HOME`、`CARGO_HOME`、`RUSTUP_HOME` 根据容器挂载目标进行精确重映射。
  - `mounts`：经过安全校验的只读或读写挂载映射表（`MountSpec`）。
- **`ExecutionPolicy`**：
  定义本次执行的动态授权约束：
  - `workspace_access`：`ReadOnly`（默认）或 `ReadWrite`。
  - `network`：`Allow` 或 `Deny`。
  - `external_read` / `external_read_write`：外部路径授权。
  - `rust_toolchain`：显式指定的 Rust 工具链路径。
  - `resource_limits`：显式资源限制（文件大小、内存地址空间、进程数、文件描述符）。
- **`ExecutionResult`**：
  包含子进程的退出状态码（`exit_code`）、分别保留末尾 32 KiB 的标准输出与标准错误文本、实际输出的总字节数、截断指示标记（`truncated`）以及执行耗时。

---

## 2. 沙箱隔离与后端实现 (`sandbox.rs`, `backend/linux.rs`)

在 Linux 环境下，`BwrapBackend` 基于 Bubblewrap 与 `/usr/bin/prlimit` 构建多层防御体系：

### 命名空间与隔离边界
- 独立 PID、IPC、UTS 与 User 命名空间；清空 Linux capabilities，禁用嵌套命名空间。
- 网络控制：当网络策略为 `Deny` 时，强制创建独立 Network 命名空间（仅回环接口，无网络链路）。
- 基础根系统：`/usr`、`/bin`、`/sbin`、`/lib`、`/lib64` 及动态链接库缓存以只读模式绑定挂载。`/tmp` 为独立的 64 MiB 临时内存文件系统；`/dev` 仅挂载最小化基础设备节点（`null`, `zero`, `urandom` 等）。
- 宿主凭据隔离：不暴露宿主 `$HOME`、SSH Agent、Git 凭据助手或系统密钥环。

### Seccomp 系统调用过滤
- 强制加载编译生成的 BPF 过滤器。
- 坚决阻断高危系统调用：`ptrace`、`bpf`、`perf_event_open`、`keyctl`、`add_key`、`request_key` 以及所有挂载与命名空间管理相关调用（`mount`, `umount2`, `unshare`, `setns` 等）。
- 过滤器加载失败时坚决拒绝启动子进程，确保 fail-closed。

### 资源配额 (`prlimit`)
- 单个文件写入上限：默认 64 MiB。
- 虚拟内存地址空间：默认 4 GiB。
- 最大打开文件描述符：默认 1024。
- 最大并发进程/线程数：默认 1024。
- 针对用户显式设置的无限制模式（0），进行精确的单向与往返转换，杜绝静默回退。

---

## 3. 工作区安全与 Inode 防护 (`filesystem`)

### 安全路径打开
- `Workspace` 面向 Unix 系统设计，内部使用 `rustix` 的目录描述符与 `openat` 配合 `O_NOFOLLOW` 逐级解析。
- 坚决杜绝符号链接逃逸与 `..` 相对路径跨出工作区，消除 TOCTOU（Time-of-check to time-of-use）竞争漏洞。

### 受保护路径（Protected Paths）与遮蔽（Masking）
- 默认保护路径：`.git`、`.env`、`.env.*`、加载的 Orbit 数据库文件、会话日志与会话锁文件。
- 直接在真实工作区内执行命令时，工作区内的受保护路径会在容器启动时挂载只读 `/dev/null`（或空 tmpfs）进行彻底遮蔽。
- 单链接与多链接 Inode 检查：
  - `collect_all_protected_inodes` 递归收集工作区内受保护文件与目录的 Inode。
  - 无论是单链接文件（如普通 `.env`）还是硬链接别名，均会纳入受保护 Inode 集合；任何自定义挂载源若包含这些 Inode，均会在挂载前被直接拒绝。

### 不透明运行时根目录（Opaque Runtime Roots）
- 针对体积庞大的历史数据根目录（例如 session 存储、历史输出缓存，可能包含数万个条目），通过 `protect_opaque_runtime_root` 登记。
- **按需延迟扫描**：在常规命令与工具链启动时，仅记录其根 Inode，坚决跳过耗时的子目录全量递归遍历；仅当发现同一设备上的文件具有多个硬链接（`nlink > 1`）时，才触发对对应根目录的有界安全扫描。这消除了大体积历史数据阻断正常命令启动的瓶颈。

### 挂载源与系统工具链校验
- **普通挂载源校验 (`validate_mount_source`)**：
  严格禁止挂载保留系统路径（`/`, `/proc`, `/dev`, `/sys`, `/etc`, `/usr` 等），杜绝外部容器挂载污染系统环境。
- **专用工具链挂载校验 (`validate_toolchain_source`)**：
  为 Rust 工具链提供专用校验逻辑：
  - 允许系统级安装的工具链（如位于 `/usr` 或其子目录下）。
  - 若工具链指向 `/usr`，仅对编译运行必需的 `bin` 与 `lib/rustlib` 目录执行精准 Inode 检查，不盲目遍历数十万个系统文件。
  - 坚决阻断虚拟文件系统（`/`, `/proc`, `/dev`, `/sys`）与系统敏感配置（`/etc`），并对所有受保护路径和 Inode 执行严格比对。

---

## 4. 权限与能力系统 (`policy`, `capability`)

- **策略决策分层**：
  - 沙箱层决定进程能够看到的物理路径与挂载。
  - 审批层（`PolicyEngine`）基于用户策略模式（`ask` / `auto` / `yolo`）决定本次操作是否需要用户确认。
  - 资源配额层决定单次操作的执行时长与空间消耗。
- **能力授权分类**：
  - `WorkspaceAccess::ReadOnly` / `ReadWrite`。
  - `FilesystemCapability`：区分外部只读路径与外部读写路径。
  - 会话授权作用域：`SessionGrantStore` 严格区分本地环境与远程会话，仅当 `grant.environment.is_local()` 为真时才在本机命令沙箱中转换为有效挂载，防止跨环境越权。

---

## 5. 工作区状态核验 (`verification`)

- **工作区指纹 (`WorkspaceFingerprint`)**：
  在执行修改前后，基于文件路径、大小、修改时间与 SHA-256 计算工作区内容摘要。
- **版本证据 (`VerificationEvidence`)**：
  在命令执行结果中附带实际校验指纹，清晰标识输入指纹是否发生变化、版本对比证据是否有效，向 Agent 提供确定性的执行结果反馈。

---

## 测试与质量验证

```sh
# 运行 orbit-runtime 完整测试套件
cargo test -p orbit-runtime --offline

# 运行代码规范与静态检查
cargo clippy -p orbit-runtime --all-targets -- -D warnings
```
