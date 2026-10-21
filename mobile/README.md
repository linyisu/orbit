# Orbit Android

手机端使用 NSH Rust 组件，任务和模型账号留在电脑。远程传输使用 iroh 1.4.0，
优先直连，无法直连时通过公共中继传输端到端加密的数据；不需要自行部署服务器或安装 VPN。
公共中继不保证可用性，网络阻断、主机休眠或主机服务退出时无法连接。

## 配对与使用

1. 在电脑 Orbit 打开本机项目，进入 **设置 → 手机连接**，开启连接。
2. 开启后直接显示二维码，扫码授权包含项目、会话、附件与工具审批。
3. 手机打开连接页，点击 **扫描配对二维码**，授予相机权限，对准电脑二维码。
4. 识别二维码后立即关闭相机，显示连接进度；连接完成进入会话首页。二维码有效期 5 分钟，仅能使用一次；刷新二维码使旧邀请失效。
5. 手机保存设备授权并进入会话主页。新任务通过右上角选择项目；已有会话右上角直接打开工作区。不需要安全码或额外密码。
6. 通过左侧会话抽屉切换项目和会话，通过工作区页查看文件、Git、模型、计划、任务、
   子 Agent、待发送消息和扩展；新配对默认允许操作电脑；会话自身的工具审批模式仍生效。

扫码授权整台电脑，不绑定初始项目，也不会将主机模型密钥传给手机。
旧项目级授权不会自动扩大，需重新扫码配对。电脑端可撤销设备，撤销后已有及后续连接
均失去访问权，不回滚已执行的任务。关闭手机不取消任务。
启动首先显示连接页，选择已保存的电脑或扫码添加，连接成功后进入会话主页；已连接设备再次启动会自动恢复连接和会话。
会话菜单从左侧打开抽屉，支持返回和点击遮罩关闭；项目目录可输入或从已有会话选择。
会话抽屉底部的设置提供设备管理、断开与开源许可。连接监听和草稿保存由应用持有，切换页面不会取消。

附件通过 Android 系统文件选择器读取，最多 8 个、总计 20 MiB，分块上传并校验 SHA-256。
完整内容才进入既有会话附件提交路径；未完成上传在连接断开或过期后丢弃。
发送结果不确定时不自动重发。手机文本草稿保留；未发送附件仅保留在当前页面内存中，
退出会话或重新连接后需要重新选择。

重复扫码优先验证并复用同一电脑的有效授权，连接列表按电脑证书去重，不使用项目目录作为设备名称。
二维码使用紧凑的 v2 编码，手机仍接受 v1 二维码；扫码授权不需要电脑二次确认，需要电脑运行更新后的服务。

设备凭据和文本草稿保存于 Android 应用私有存储，应用禁止系统备份，无需单独设置应用密码。
旧版本加密保险箱不自动迁移，请重新扫码；手机不保存主机供应商凭据。

## 源码与平台边界

- `src/app.rs`：导航、扫码预览、连接页、会话与发送交互。
- `src/platform/android.rs`：Android 生命周期、权限和系统文件选择的 JNI 接口。
- `src/platform/camera.rs`：Rust 调用 Camera2 NDK 获取有界灰度帧，rqrr 识别二维码。
- `orbit-client/remote`：iroh、固定证书、配对、设备认证、附件传输和共享会话投影。
- `orbit-service/workspace/remote*`：主机开关、单次邀请、扫码授权与设备访问检查。

业务和界面使用 Rust，不引入 Go、JS/TS 页面或 JavaScript 运行时。
NSH 的 Android 控件仍通过系统 Java 桥接；`patches/nsh-android-activity.patch` 仅增加
Activity 生命周期、权限与选择结果转发，以及相机权限声明。配对、读取、识别和网络逻辑
均在 Rust 中；不复制 NSH 源码到仓库。
`patches/nsh-android-images.patch` 提供 Android 内嵌 Base64 图片加载，
图标、Logo 和扫码预览共用既有 SVG/位图解码器，不产生临时文件。
关闭淡入的连续图片源保留上一帧，直到新帧完成解码；相机预览不逐帧清空或淡入。

## 构建与验证

NSH 固定提交 `d5839d7a302e2061d54c58090588f0f953e366d0`。
需要 Rust stable、Android SDK / Build Tools 35、NDK r30、JDK 17，
以及 `aarch64-linux-android` target。最低 Android 9（API 28），发布 arm64-v8a。

在固定提交的 NSH checkout 中应用补丁，再构建工具和运行时：

```sh
git apply /path/to/orbit/mobile/patches/nsh-rust-runtime.patch
git apply /path/to/orbit/mobile/patches/nsh-android-activity.patch
git apply /path/to/orbit/mobile/patches/nsh-android-images.patch
git apply /path/to/orbit/mobile/patches/nsh-android-input.patch
cargo +stable build -p nsh-cli --locked
cargo +stable xtask android --abi arm64-v8a
```

设置 `ANDROID_HOME`、`ANDROID_NDK_HOME`、`JAVA_HOME`，将 JDK bin 加入 PATH；
然后在 Orbit 根目录用上述 CLI 打包：

```sh
nsh package mobile --target android --out target/android-nsh
cargo fmt --manifest-path mobile/Cargo.toml -- --check
cargo test --manifest-path mobile/Cargo.toml --locked
cargo clippy --manifest-path mobile/Cargo.toml --all-targets --locked -- -D warnings
cargo test -p orbit-service --lib pairing_completes_once --locked
cargo test -p orbit-server --test workspace_service authenticated_phone_controls --locked
```

公网中继测试单独运行，需要真实网络：

```sh
cargo test -p orbit-service --lib public_relay_pairing_and_device_revocation --locked -- --ignored
cargo test -p orbit-server --test workspace_service public_iroh_desktop --locked -- --ignored
```

默认 APK 为调试签名，正式分发需提供自己的签名参数。CI 同时验证 APK 签名和 16 KiB 对齐。
自动测试覆盖合成二维码、组件事件、配对授权与上传边界；相机硬件、输入法、字体缩放、
前后台和网络切换仍需 Android 实机或模拟器验证，交付说明应列出实际完成的范围。

[项目总览](../README.md)

Android 输入框保留字体上下边界和 fallback 行距，避免中文字体被裁切。网络恢复时重连并重新读取会话历史，绝不自动重发结果未知的写操作。系统结束应用进程后连接无法继续保活，再次打开时恢复；电脑任务不依赖手机进程。

项目置顶和排序规则由 `orbit-client::projects` 与桌面共用，手机按已保存连接保存展示偏好。
抽屉列表独立滚动，搜索时根据键盘高度缩小可用区域；展开和收起使用文件夹图标。
Android 原生输入框将短内容的垂直滚动范围限制为零，长内容达到输入区上限后才滚动。
原生验收可让 `remote_navigation` 测试通过 `ORBIT_MOBILE_NATIVE_STORAGE` 输出合成连接存储，
同时保持合成服务十分钟，供隔离模拟器检查；不使用真实工作区或账号。
