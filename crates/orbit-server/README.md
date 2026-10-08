# orbit-server

[项目总览](../../README.md)

`orbit-server` 提供 `orbit-workspace-server`，用于运行无界面的工作区服务。它解析启动参数并调用 `orbit-service`，进程中只包含应用服务和底层功能库。

## 启动

```sh
orbit-workspace-server workspace-service --workspace /path/to/project \
  --config /path/to/config.toml --auth-dir /path/to/auth
```

标准输入与标准输出用于工作区协议，标准错误用于服务日志。服务支持会话恢复、事件分页、审批、取消、文件访问与子代理查询。`--daemon` 是传输入口使用的后台启动参数。文档转换和 Git 推送的隔离 worker 由共用的 `platform::workers` 入口分发。

## 静态服务构建

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release -p orbit-server --target x86_64-unknown-linux-musl
```

构建需要 `musl-tools` 和相应架构的 Rust target。客户端的 [server.rs](../../build-support/server.rs) 使用独立的 `target/workspace-server/` 构建缓存，编译本包的静态二进制，验证 ELF 加载器和动态库依赖后将文件内置到 `orbit`。部署时由客户端提供此资源，客户端处理目标检测、上传和校验。

服务使用目标用户的普通进程在真实工作目录执行命令，无需 Bubblewrap。静态服务可用于 glibc 版本较旧的 Linux 系统。

## 验证

工作区协议集成测试位于 [orbit-cli/tests/workspace_service.rs](../orbit-cli/tests/workspace_service.rs)。[test-remote-bundle.py](../../scripts/test-remote-bundle.py) 在隔离的 Ubuntu 20.04 容器中验证独立客户端的服务部署、配置传递、握手和缓存复用。
