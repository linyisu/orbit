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
`cargo xtask verify-native-bundle target/bundle` 在临时工作区验证服务的握手、会话存储和
关闭，再检查客户端启动；不需要 Docker 或 WSL。Linux 容器兼容性验收仍使用 `verify-bundle`。
默认归档名包含本机系统与架构。

桌面客户端使用 `cargo xtask bundle-gui`（可加 `debug`）构建到 `target/bundle-gui`，
macOS 生成 `.app` 并本地签名，Windows 保留 `.exe` 图标资源，Linux 附带桌面启动项。
`cargo xtask verify-gui-bundle` 先验证内嵌服务，再在独立用户目录打开真实窗口、等待连接会话与渲染，最后关闭窗口和服务；
Linux 需由 Xvfb 提供显示环境。`cargo xtask archive-gui` 只归档桌面启动资产，排除验收用的独立服务。
解压后的分发包使用 `cargo xtask verify-gui-bundle <目录> --standalone` 验收；
驱动从隔离缓存读取应用实际释放的服务，再验证完整内嵌字节、协议和关闭行为。
默认模式继续要求构建目录中的独立服务验收资产，不自动退回分发包模式。
详细平台依赖、签名边界及安装说明见 [GUI README](../orbit-gui/README.md#桌面发布包)。

Windows x64 测试安装包由 NSIS 3.13 构建，先执行 `cargo xtask bundle-gui debug`，再执行：

```sh
cargo xtask installer-gui target/bundle-gui target/orbit-gui-debug-windows-x64-setup.exe
cargo xtask verify-gui-installer target/orbit-gui-debug-windows-x64-setup.exe
```

可用 `NSISDIR` 指定官方便携包的根目录；未指定时查找系统安装目录或 PATH。
CI 直接下载官方 ZIP 并验证固定 SHA-256，不依赖 Chocolatey 软件源。

第二条命令必须在没有 GPUI Windows 源码的独立 Windows 主机执行。验收会安装到临时目录，
构建同时生成相邻的 `.exe.sha256`，交付 ZIP 包含安装包及校验文件；验收先验证下载后的 SHA-256，
NSIS 保持强制完整性检查，不绕过损坏文件。开始菜单启动目录使用用户目录。
验证实际安装程序的服务连接、窗口渲染和退出，最后卸载。CI 使用全新 runner，不检出仓库或恢复 Cargo 缓存。
安装包按用户安装到本地应用目录，提供开始菜单入口和系统卸载入口，不修改会话数据。
验收从隔离缓存取出程序实际释放的服务，再检查完整内嵌字节、协议和关闭行为。
CI 的[Build](../../.github/workflows/build.yml)入口手动触发，三平台分别构建 TUI 与 GUI Debug 包。
内嵌的 Linux 远端服务来自同一工作流、同一提交；不跨提交复用应用产物。
桌面窗口、服务协议与干净 Windows 主机安装验收是构建流程中的必经步骤。
普通推送只运行[Tests](../../.github/workflows/test.yml)，无需使用提交消息标记选择构建模式。
Debug 包仅关闭 `gpui-pre-windows` 的调试断言，使其采用上游已有的构建期内嵌着色器路径；
Orbit 本身保持 Debug，用户机器无需 Cargo 源码、HLSL 编译工具或构建机目录。
框架默认图标在 Debug 中同样内嵌；窗口验收检查所有注册图标均可直接读取静态资源。
每个包分别验收已有配置与首次无配置的真实窗口；Windows 首次启动只保留系统 PATH，
不传配置或模型参数，并等待添加模型连接引导，避免开发机的 Git、Cargo 或配置掩盖部署问题。
已有配置场景还会打开账号页，通过本机 Rust 替身检测模型、更换测试密钥、保存并返回会话，
关闭后从同一隔离目录再次启动，验证新凭据与配置可以恢复；安装后的独立 runner 也执行该流程。
账号流程还逐帧展示供应商、ChatGPT、Google、官方密钥和自定义 API 表单。
Linux 隔离验收可通过 `ORBIT_GUI_CAPTURE_DIR` 保存这些页面截图，使用 Xvfb 内的 ffmpeg 抓取，
不操作用户桌面；普通启动不会安装验收驱动或抓取屏幕。
Windows GUI 为主线程预留 8 MiB 栈，覆盖 Debug 视图构造的较大栈帧；不会改动执行工具的资源限制。
