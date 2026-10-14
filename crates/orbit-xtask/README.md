# orbit-xtask

仓库维护与发布工具，通过 `.cargo/config.toml` 中的 `cargo xtask` 别名运行。

## 命令

```sh
cargo xtask check
cargo xtask bundle
cargo xtask verify-bundle target/bundle
cargo xtask archive target/bundle target/orbit-linux-x86_64.tar.gz
```

`check` 检查依赖与职责边界；`bundle` 构建并收集 CLI/TUI，`bundle debug` 使用调试构建。`verify-bundle` 需要 Docker，在 Ubuntu 20.04 容器中验证部署、协议与缓存复用；`archive` 保留可执行权限。

## 入口与验证

[architecture.rs](src/architecture.rs) 检查仓库，[bundle.rs](src/bundle.rs) 打包，[remote.rs](src/remote.rs) 验证部署。

```sh
cargo test -p orbit-xtask
```

[项目总览](../../README.md)
