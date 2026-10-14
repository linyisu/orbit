# orbit-build

客户端的构建期辅助库，编译、校验并内嵌静态工作区服务，不参与会话执行。

## 入口

[lib.rs](src/lib.rs) 提供 `embed_workspace_server` 与 `verify_static_server`，由 CLI、TUI、GUI 的构建脚本调用。

默认在独立的 `target/workspace-server/` 缓存中构建同架构 musl 服务，并用 `readelf` 校验不依赖动态加载器或共享库。需要 Rust musl target、`musl-gcc` 与 `binutils`。

设置 `ORBIT_WORKSPACE_SERVER=/absolute/path/to/orbit-workspace-server` 可复用已有静态服务，文件仍需通过校验。

```sh
cargo check -p orbit-build
```

[项目总览](../../README.md)
