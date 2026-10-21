# Android 远控接入

当前已接入 Linux 电脑的真实局域网控制链路：会话列表、打开及新建会话、历史分页、
发送文本、停止当前响应和单次工具审批。电脑显式生成设备授权资料，手机导入后通过
固定主机证书的 TLS 连接；设备撤销后旧连接和新连接都失去访问权。
手机仅通过 `orbit-client` 的 `remote` 功能访问服务，不依赖 Agent、工具、模型或账号模块。
GPUI Kit 负责界面，Android 宿主采用上游 `GpuiInputActivity`。

这是接入版本，尚非完整移动产品。会话事件和审批参数目前以有界原始内容展示；
扫码、Keystore、持久草稿、公网穿透、附件、队列及计划审阅尚未接入。
连接资料只存在于本次进程内，切到后台停止手机连接，电脑任务继续；重新连接不自动重发消息。
若发送结果未确认，先检查主机会话，不直接另发一条。该版本没有操作回执查询界面。

独立 Cargo workspace 避免移动试验改变桌面锁文件与 renderer 补丁。
共享客户端已提供不包含主机部署依赖的 remote 构建；后续仍需统一完整会话投影并将
移动项目纳入仓库的完整依赖边界检查。没有复制桌面会话恢复逻辑。

## 连接电脑

电脑先构建 `cargo build -p orbit-server --locked`。下列路径使用绝对路径，并替换为
电脑现有的配置、数据库、凭据和项目目录；父目录必须已经存在。
设备授权资料包含私密凭据，只交给该手机。`--control` 授予发送和停止，`--approve`
授予单次审批；两个参数都省略则只读。不提供长期记住审批或修改主机配置的接口。

```sh
target/debug/orbit-workspace-server \
  --config /path/to/config.toml --database /path/to/orbit.db --auth-dir /path/to/auth \
  remote --state /path/to/remote-state --workspace /path/to/project \
  authorize --address 192.168.1.10:9443 --output /path/to/phone.json --control --approve

target/debug/orbit-workspace-server \
  --config /path/to/config.toml --database /path/to/orbit.db --auth-dir /path/to/auth \
  remote --state /path/to/remote-state --workspace /path/to/project \
  listen --bind 192.168.1.10:9443
```

手机粘贴 `phone.json` 内容后连接。电脑必须保持网关进程运行；关闭网关不会停止已有
Agent。该网关附着现有共享服务，不另开一套会话数据库。
电脑授权命令输出设备 ID；用相同全局参数和 `remote` 参数执行 `revoke DEVICE_ID`
撤销访问。撤销只删除设备登记，不删除会话，也不回滚已经执行的操作。
授权限定指定工作区和数据库；工作区不是操作系统沙箱，任务仍以电脑用户权限执行。

## 固定依赖

| 项目 | 版本 |
| --- | --- |
| GPUI Kit | Orbit fork `d7b441dbc0f6dac2cdea76d9df5eb44ffb13f4e4` |
| GPUI Mobile / 移动 renderer | `c24a1308e1dcb47ce94d9ef8d8d05ca4d77d2aa7` |
| GPUI core | 0.3.9，实际传递依赖见 Cargo.lock |
| Android ABI / 最低 API | arm64-v8a / 26，待真机验收 |
| Android compile / target SDK | 35 |
| Android Gradle Plugin / Gradle | 8.9.2 / 8.11.1 |
| Java | JDK 17 |
| 本次链接验证 NDK | r30（30.0.16248370） |

Mobile manifest 包含 renderer 的 `offscreen` 能力引用，因此此实验显式固定其配套
renderer，而不覆盖桌面使用的本仓库补丁。正式合并前需核对两者补丁与字体回退。
Android Activity 和剪贴板桥从同一固定上游源码读取，不复制整个示例及演示权限。
构建脚本拒绝不匹配的上游提交。上游许可选用 Apache-2.0，打包时附带其许可文件。

## 构建

安装 Android SDK 35、Build Tools 35.0.0、NDK r30、JDK 17、Gradle 8.11.1，
以及 Rust `aarch64-linux-android` target。以下命令在仓库根执行。
`ANDROID_NDK_HOME` 指向 NDK 根目录；`ANDROID_HOME` 指向 SDK，`JAVA_HOME` 指向 JDK 17。

Linux 主机无需 cargo-ndk 即可构建动态库：

```sh
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android26-clang"
export CC_aarch64_linux_android="$CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER"
export AR_aarch64_linux_android="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-ar"
cargo build --manifest-path mobile/Cargo.toml --target aarch64-linux-android --lib --release --locked
mkdir -p mobile/android/app/src/main/jniLibs/arm64-v8a
cp mobile/target/aarch64-linux-android/release/liborbit_mobile_probe.so mobile/android/app/src/main/jniLibs/arm64-v8a/
```

准备固定 Android 宿主源码并打包。`gpuiMobileSource` 必须为绝对路径：

```sh
git clone https://github.com/longbridge/gpui-mobile.git target/gpui-mobile
git -C target/gpui-mobile checkout c24a1308e1dcb47ce94d9ef8d8d05ca4d77d2aa7
gradle -p mobile/android -PgpuiMobileSource="$PWD/target/gpui-mobile" assembleDebug
```

产物：`mobile/android/app/build/outputs/apk/debug/app-debug.apk`。
Debug APK 仅用于设备验收，不是正式签名发布包。打包前检查动态库存在，避免生成空壳 APK。
桌面预览可用 `cargo run --manifest-path mobile/Cargo.toml --features preview`，
必须在隔离 Xvfb 和临时 HOME 下验收，不能用桌面运行替代 Android 验收。

## 自动检查

```sh
cargo fmt --manifest-path mobile/Cargo.toml -- --check
cargo clippy --manifest-path mobile/Cargo.toml --target aarch64-linux-android --lib --locked -- -D warnings
cargo tree --manifest-path mobile/Cargo.toml --target aarch64-linux-android -i gpui-pre --locked
```

## 验收门槛

此前的平台原型已完成 Android arm64 Debug 动态库链接和 APK 打包，验证了 Activity、
输入法入口及 16 KB LOAD 对齐。本次远控接入已通过 Android Clippy；尚未重新打包 APK，
原型 APK 不包含本次远控功能。
共享客户端到真实服务的 TLS、发送、断线续接、审批、去重及撤销已由集成测试验证，
模型使用合成替身。尚未完成 Android 真机启动、视觉和交互验收。

编译和链接通过只能证明构建链路可用。以下项目必须记录真实结果，不预先打勾：

- 两台不同 GPU/系统的 arm64 设备正常启动，release 动态库可加载。
- 中文组合输入、候选选择、emoji、粘贴、删除、多行输入正确。
- 键盘、安全区、旋转、分屏、前后台与 surface 重建不遮挡主要操作。
- Markdown、表格、代码、长文本的滚动、选择、复制可用。
- 深浅模式、系统字体放大、TalkBack 与系统返回操作可用。
- APK 中每个动态库的 ABI、依赖、16 KB LOAD 对齐及 ZIP 对齐合格。

## 后续接入边界

桌面 `orbit-client::protocol` 的事件和审批类型仍引用 agent/tools/runtime；
移动 remote 构建使用通用协议负载进行初期接入，不自行解释工具执行结果。
下一阶段应将纯数据契约归入 protocol，再复用完整 conversation/activity/execution 投影。

网络网关在 service/server，显式设备授权在共享服务，登记和撤销在 store。
手机只附着主机已有数据源，不上传配置覆盖主机，不持有模型账号；未经配对不得进入
普通业务分发。补齐交互式配对、回执查询和移动恢复后，再选择一个公网穿透方案。

[项目总览](../README.md)
