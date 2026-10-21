# Android 平台验证

这是远控计划 G0-A 的独立验证工程，不是已接通电脑的手机客户端。
使用 GPUI Kit 构建界面，Android 宿主采用上游 `GpuiInputActivity`。
合成会话、Markdown、多行输入、工具详情和模拟审批均只存在于进程内；
不读取个人配置、启动 Agent、连接服务或保存模型凭据。Manifest 不声明网络权限。

独立 Cargo workspace 避免移动试验改变桌面锁文件与 renderer 补丁。
正式接入前必须完成共享客户端裁剪，再将手机客户端纳入仓库依赖边界检查。
现在没有复制桌面会话恢复或请求调度逻辑。

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

已完成 Android arm64 Debug 动态库链接、APK 打包、Clippy、格式及仓库依赖边界检查。
动态库导出了 Activity 与输入法入口，LOAD 段按 16 KB 对齐。
尚未完成真机启动、视觉和交互验收，也没有验证真实远控。

编译和链接通过只能证明构建链路可用。以下项目必须记录真实结果，不预先打勾：

- 两台不同 GPU/系统的 arm64 设备正常启动，release 动态库可加载。
- 中文组合输入、候选选择、emoji、粘贴、删除、多行输入正确。
- 键盘、安全区、旋转、分屏、前后台与 surface 重建不遮挡主要操作。
- Markdown、表格、代码、长文本的滚动、选择、复制可用。
- 深浅模式、系统字体放大、TalkBack 与系统返回操作可用。
- APK 中每个动态库的 ABI、依赖、16 KB LOAD 对齐及 ZIP 对齐合格。

## 远控后续接入边界

当前 `orbit-client::protocol` 的事件和审批类型仍引用 agent/tools/runtime；
`transport::Client` 持有 Child/ChildStdin/ChildStdout。下一阶段应先将纯数据契约
归入 protocol，保留兼容导出，再为 client 隔离本机部署与进程传输。

网络网关留在 service/server，配对与设备登记留在共享服务和 store。
手机只附着主机已有数据源，不上传配置覆盖主机，不持有模型账号；未经配对不得进入
普通业务分发。先验证局域网认证、撤销、审批和未确认请求，再选择一个公网穿透方案。

[项目总览](../README.md)
