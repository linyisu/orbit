# orbit-cli

[项目总览](../../README.md)

`orbit-cli` 提供 `orbit` 可执行文件，负责命令行参数、输入输出、信号处理与入口分发。应用功能由 [orbit-app](../orbit-app/README.md) 提供，终端界面由 [orbit-tui](../orbit-tui/README.md) 提供。

## 命令

```sh
orbit tui
orbit tui --resume SESSION_ID
orbit chat "分析当前项目的测试失败"
orbit chat --stream --mode auto --image diagram.png "解释图片"
orbit chat --prompt-file task.txt --model provider::model
orbit auth login google
orbit auth list
orbit sessions list
orbit sessions diagnostics SESSION_ID
orbit projects list
```

全局选项 `--config` 指定本地配置文件，`--database` 指定会话数据库，`--auth-dir` 指定凭据目录。配置示例见 [config.example.toml](../../config.example.toml)。

`chat` 支持 `read-only`、`auto`、`yolo` 权限模式，`--disable` 可限制工具类别，`--allow-mcp-start` 允许启动已配置的 MCP 程序。`--prompt-file -` 从标准输入读取任务。`--swe-instance-id` 输出包含工作区补丁的 SWE-bench 预测 JSON。

`auth set-key NAME --base-url URL` 以隐藏输入保存 API Key；通过管道输入时使用 `--stdin`。`auth models --config PATH` 刷新模型目录。`sessions` 提供历史、占用、诊断和压缩记录查询，`sessions delete ID --yes` 删除已关闭的会话。`projects link ID` 将移动后的目录关联到原项目。

## 工作区连接

```sh
orbit connect --workspace /path/to/project
orbit connect --ssh dev-host --workspace /home/me/project
orbit connect --wsl Ubuntu --wsl-user me --workspace /home/me/project
orbit connect --ssh dev-host --workspace /home/me/project --resume SESSION_ID
orbit connect --ssh dev-host --workspace /home/me/project --list
orbit connect --workspace /path/to/project --prompt '检查测试失败原因'
orbit connect --workspace /path/to/project --stop
```

`connect` 可进入工作区标签界面、列出会话、执行无交互任务或停止空闲服务。`--prompt` 使用自动审批策略，需要额外确认的请求会被拒绝。`--ssh-user`、`--ssh-port`、`--identity` 指定 SSH 参数；`--server` 指定目标端可执行文件，`--upload-server` 指定本地兼容的 Linux 服务程序。

连接传输、部署、配置传递及服务生命周期见 [应用层工作区服务](../orbit-app/README.md#工作区服务)，TUI 连接弹窗及标签操作见 [终端工作区连接](../orbit-tui/README.md#工作区连接)。

## 入口与构建

| 文件 | 职责 |
| --- | --- |
| [main.rs](src/main.rs) | 参数定义、入口分发、任务输出和信号处理 |
| [auth_cli.rs](src/auth_cli.rs) | 认证子命令与隐藏终端输入 |
| [connect_args.rs](src/connect_args.rs) | 连接与服务参数 |
| [connect.rs](src/connect.rs) | 连接命令输出与无交互事件呈现 |
| [build/portable.rs](build/portable.rs) | 构建并验证内置静态服务文件 |

普通 Linux 构建将 [orbit-server](../orbit-server/README.md) 的 musl 二进制作为资源内置到客户端，启动时交给应用层的部署接口。发布包包含一个 `orbit` 可执行文件。

```sh
cargo build --release -p orbit-cli
cargo test --offline -p orbit-cli
```

CLI 集成测试通过真实进程验证服务协议和文档转换 worker。界面与应用服务各自的测试位于对应 crate。
