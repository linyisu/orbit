# orbit-cli

命令行入口，提供单次任务、认证、会话和项目管理。通过 [orbit-client](../orbit-client/README.md) 调用工作区服务。

## 使用

```sh
cargo build --release -p orbit-cli --locked
./target/release/orbit-cli chat --model provider::model "分析当前项目"
./target/release/orbit-cli auth login antigravity
./target/release/orbit-cli sessions list
./target/release/orbit-cli connect --ssh dev-host --workspace /home/me/project
```

`--config`、`--auth-dir` 指向本机配置与凭据，`--database` 指向目标端数据。`chat --prompt-file -` 从标准输入读取任务；其他参数见 `orbit-cli --help` 和各子命令帮助。

## 入口与验证

[main.rs](src/main.rs) 分发命令，[auth_cli.rs](src/auth_cli.rs) 处理认证，[connect.rs](src/connect.rs) 处理工作区连接。

```sh
cargo test -p orbit-cli --locked
```

[项目总览](../../README.md)
