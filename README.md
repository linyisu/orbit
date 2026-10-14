# Orbit

Rust 编写的编码 Agent，提供终端界面、命令行和实验性桌面客户端。支持多供应商模型、文件编辑、命令执行、持久化会话和子 Agent，可连接本地、SSH 与 WSL 工作区。

## 快速开始

目前支持 Linux 客户端。源码构建需要 Rust stable、`musl-tools`、`binutils` 及对应架构的 musl target；以下以 x86-64 为例：

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release -p orbit-tui -p orbit-cli --locked
cp config.example.toml config.toml
./target/release/orbit
```

修改 [配置示例](config.example.toml) 中的连接，或在 TUI 用 `/connect` 登录和管理模型。客户端会记住模型选择，历史会话可用 `orbit --resume SESSION_ID` 恢复。

```sh
./target/release/orbit-cli chat --model provider::model "检查当前项目的测试失败"
./target/release/orbit-cli sessions list
```

客户端内置同架构的静态工作区服务，首次构建较慢，使用时无需额外安装服务。桌面客户端的依赖与启动方式见 [GUI README](crates/orbit-gui/README.md)。

## 使用

- `/model` 切换模型，`/model refresh` 更新可用列表；配置规则见 [orbit-config](crates/orbit-config/README.md)。
- `/plan` 规划，`/plan view` 审阅，批准后执行；`/build` 返回执行模式，工具审批独立生效。
- `/ssh HOST PATH`、`/wsl DISTRO PATH` 打开远端标签；连接与配置传递见 [orbit-client](crates/orbit-client/README.md)。
- `/file`、`/review`、`/tasks`、`/queue` 管理文件、变更、任务和暂存消息。两端入口见 [功能对照](FRONTEND_PARITY.md)。

命令以目标用户权限执行；审批声明不提供操作系统隔离，内置文件工具另有路径保护。详见 [工具与审批](crates/orbit-tools/README.md)。

## 工作区

```text
CLI / TUI / GUI → client → server → service → agent / tools / store
```

| Crate | 用途 |
| --- | --- |
| [orbit-cli](crates/orbit-cli/README.md) | 命令行入口与单次任务 |
| [orbit-tui](crates/orbit-tui/README.md) | 终端界面与交互 |
| [orbit-gui](crates/orbit-gui/README.md) | GPUI Kit 桌面客户端 |
| [orbit-client](crates/orbit-client/README.md) | 工作区连接与服务部署 |
| [orbit-protocol](crates/orbit-protocol/README.md) | 请求、事件和分帧 |
| [orbit-service](crates/orbit-service/README.md) | 会话与任务协调 |
| [orbit-server](crates/orbit-server/README.md) | 无界面服务入口 |
| [orbit-agent](crates/orbit-agent/README.md) | 模型与工具执行循环 |
| [orbit-model](crates/orbit-model/README.md) | 模型协议与流式适配 |
| [orbit-auth](crates/orbit-auth/README.md) | 凭据与 OAuth |
| [orbit-config](crates/orbit-config/README.md) | 配置与模型目录 |
| [orbit-runtime](crates/orbit-runtime/README.md) | 进程、文件和权限策略 |
| [orbit-tools](crates/orbit-tools/README.md) | Agent 工具实现 |
| [orbit-store](crates/orbit-store/README.md) | SQLite 持久化 |
| [orbit-syntax](crates/orbit-syntax/README.md) | 共用语法与语言识别 |
| [orbit-build](crates/orbit-build/README.md) | 构建并内嵌静态服务 |
| [orbit-xtask](crates/orbit-xtask/README.md) | 仓库检查与发布工具 |

## 开发

```sh
cargo fmt --all -- --check
cargo xtask check
cargo clippy --workspace --all-targets --features orbit-gui/test-support --locked -- -D warnings
cargo test --workspace --features orbit-gui/test-support --locked
```

测试使用临时数据；部分测试需要本地回环监听，真实网络测试单独标记为 ignored。[CI](.github/workflows/ci.yml) 使用同一组检查，便携打包见 [orbit-xtask](crates/orbit-xtask/README.md)。

按职责维护模块和共享接口，注释说明约束与原因。修改后运行相关回归；新增或移动模块时同步 crate README，并检查链接。配置与参数细节以示例、`--help` 和所属模块源码为准。

仓库维护与编码约定见 [AGENTS.md](AGENTS.md)。
