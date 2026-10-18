# orbit-xtask

仓库维护与发布工具，通过 `.cargo/config.toml` 中的 `cargo xtask` 别名运行。

## 命令

```sh
cargo xtask check
cargo xtask bundle
cargo xtask verify-bundle target/bundle
cargo xtask archive target/bundle target/orbit-linux-x86_64.tar.gz
```

`check` 检查依赖与职责边界，覆盖 GUI/TUI 的开发及平台依赖，禁止前端构造领域服务；同时检查认证供应商、模型发现与配置模块的归属。

`bundle` 构建 TUI 并收集其内嵌服务作为验收资产，`bundle debug` 使用调试构建。`verify-bundle` 需要 Docker，在 Ubuntu 20.04 容器中验证部署、协议与缓存复用；`archive` 仅打包内嵌服务的 `orbit`，保留可执行权限。

## 入口与验证

[architecture.rs](src/architecture.rs) 检查仓库，[bundle.rs](src/bundle.rs) 打包，[remote.rs](src/remote.rs) 验证部署。

```sh
cargo test -p orbit-xtask
```

[项目总览](../../README.md)

`bundle` 按本机平台收集可执行文件，Windows 保留 `.exe` 后缀。
`cargo xtask verify-native-bundle target/bundle` 在临时工作区验证原生服务的握手、会话存储和
关闭，再检查客户端启动；不需要 Docker 或 WSL。Linux 容器兼容性验收仍使用 `verify-bundle`。
默认归档名包含本机系统与架构。
