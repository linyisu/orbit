# orbit-xtask

仓库依赖检查、发布打包与隔离验收工具，通过 `.cargo/config.toml` 的 `cargo xtask` 别名运行。

## 依赖检查

```sh
cargo xtask check
```

检查 GUI、TUI 和独立 Android 项目的普通、开发及目标平台依赖，禁止前端构造领域服务。
Android 只使用共享客户端的 `remote` 功能。认证供应商、模型发现与配置模块也受职责边界检查。

## TUI 打包

```sh
cargo xtask bundle
cargo xtask verify-native-bundle target/bundle
cargo xtask verify-bundle target/bundle
cargo xtask archive target/bundle target/orbit-linux-x86_64.tar.gz
```

`bundle` 构建本机平台的 TUI，收集内嵌服务作为验收资产；`bundle debug` 使用调试构建。
Windows 可执行文件保留 `.exe` 后缀，默认归档名包含系统与架构。

`verify-native-bundle` 在临时工作区检查服务握手、会话存储、关闭和客户端启动，
无需 Docker 或 WSL。`verify-bundle` 在 Ubuntu 20.04 Docker 容器检查 Linux 部署、协议和缓存复用。
`archive` 仅归档内嵌服务的 `orbit`，保留可执行权限。

## GUI 打包

```sh
cargo xtask bundle-gui
cargo xtask verify-gui-bundle
cargo xtask archive-gui
```

`bundle-gui` 构建到 `target/bundle-gui`，可加 `debug`。macOS 生成本地签名的 `.app`，
Windows 保留 `.exe` 图标资源，Linux 附带桌面启动项。`archive-gui` 排除独立服务验收资产。
平台依赖、签名和安装说明见 [GUI README](../orbit-gui/README.md#桌面发布包)。

`verify-gui-bundle` 在独立用户目录检查内嵌服务、协议、真实窗口连接与渲染，然后关闭窗口和服务。
Linux 使用 Xvfb。构建目录模式要求独立服务验收资产；解压后的分发包使用：

```sh
cargo xtask verify-gui-bundle /path/to/package --standalone
```

分发包模式从隔离缓存读取应用实际释放的服务，校验完整内嵌字节、协议与关闭行为。

## Windows 安装包

Windows x64 安装包使用 NSIS 3.13，按用户安装到本地应用目录，提供开始菜单和卸载入口，
不修改会话数据。使用 `NSISDIR` 指定便携包根目录，或由工具查找系统安装目录及 PATH。

```sh
cargo xtask bundle-gui debug
cargo xtask installer-gui target/bundle-gui target/orbit-gui-debug-windows-x64-setup.exe
cargo xtask verify-gui-installer target/orbit-gui-debug-windows-x64-setup.exe
```

构建生成相邻的 `.exe.sha256`，交付 ZIP 包含安装包及校验文件。验收先检查 SHA-256，
保留 NSIS 强制完整性检查，在没有框架源码或 Cargo 缓存的独立 Windows 主机安装、启动并卸载。
开始菜单的启动目录为用户目录。CI 从官方 ZIP 获取 NSIS 并校验固定 SHA-256。

## 发布验收契约

[Build](../../.github/workflows/build.yml) 手动构建三平台 TUI、GUI Debug 包和 Android APK。
Linux 远端服务与客户端来自同一工作流、同一提交。
[Tests](../../.github/workflows/test.yml) 负责普通推送和 PR 的静态检查与回归。

桌面包覆盖已有配置和首次无配置两种启动。Windows 首次启动仅保留系统 PATH，
不传配置或模型参数，检查添加模型连接引导；安装验收使用全新 runner，不检出源码或恢复缓存。

已有配置场景通过本机 Rust 替身检测模型、更换测试密钥、保存并返回会话，再从同一隔离目录
重启检查凭据与配置恢复。账号页面覆盖供应商、ChatGPT、Google、官方密钥和自定义 API 表单。
Windows 验收覆盖窗口最大化边界，主线程栈为 8 MiB；该栈预算不改变工具执行限制。

Debug 包的框架使用构建期内嵌着色器和图标，用户机器无需 Cargo 源码、HLSL 工具或构建机目录。
Linux 可设置 `ORBIT_GUI_CAPTURE_DIR`，由 Xvfb 内的 ffmpeg 保存页面截图；普通启动不安装验收驱动。

## 模块职责与验证

- [architecture.rs](src/architecture.rs)：依赖及职责检查。
- [bundle.rs](src/bundle.rs)：打包与发布资产。
- [remote.rs](src/remote.rs)：部署验收。

```sh
cargo test -p orbit-xtask
```

[项目总览](../../README.md)
