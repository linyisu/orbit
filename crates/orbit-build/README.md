# orbit-build

客户端的构建期辅助库，编译、校验并内嵌静态工作区服务，不参与会话执行。

## 入口

[lib.rs](src/lib.rs) 提供 `embed_workspace_server` 与 `verify_static_server`，由 TUI、GUI 的构建脚本调用。

默认在独立的 `target/workspace-server/` 缓存中构建同架构 musl 服务，并解析 ELF 校验不依赖动态加载器或共享库。需要 Rust musl target 与 `musl-gcc`；校验本身无需外部命令。

设置 `ORBIT_WORKSPACE_SERVER=/absolute/path/to/orbit-workspace-server` 可复用已有静态服务，文件仍需通过校验。

```sh
cargo check -p orbit-build
```

[项目总览](../../README.md)

## 原生服务

Linux 继续使用同架构 musl 静态服务；macOS 使用 Apple 原生目标，Windows 使用 MSVC
目标并静态链接 C 运行库。构建时校验格式、CPU 架构和动态依赖；macOS/Windows 只接受
系统库，不将依赖未打包运行库的服务当作独立资产。

`ORBIT_WORKSPACE_SERVER` 在原生构建中必须指向对应平台的服务。可另设
`ORBIT_REMOTE_WORKSPACE_SERVER` 指向同架构 Linux 静态服务，供 SSH/WSL 部署。
Linux 默认共用内置资产；其他平台没有此资产时，远端连接需显式指定上传服务或已安装服务。
