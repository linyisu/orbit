# orbit-server

无界面工作区服务入口，生成 `orbit-workspace-server`，参数解析后交给 [orbit-service](../orbit-service/README.md)。

## 构建与运行

通常由客户端自动构建、内嵌和部署。手动调试：

```sh
cargo build -p orbit-server --locked
./target/debug/orbit-workspace-server workspace-service --workspace /path/to/project \
  --config /path/to/config.toml --auth-dir /path/to/auth
```

标准输入输出传输协议，标准错误记录日志。静态发布构建由 [orbit-build](../orbit-build/README.md) 处理，部署验收见 [orbit-xtask](../orbit-xtask/README.md)。

[项目总览](../../README.md)
