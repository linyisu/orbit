# orbit-client

本地、SSH、WSL 连接、账号流程、附件传输与会话观察，供 CLI、TUI 和 GUI 共用。前端保留平台交互和视图状态。

## 工作区服务

SSH 复用系统 OpenSSH 配置与认证；WSL 需要调用环境提供 `wsl.exe`。客户端将同架构静态服务部署到目标缓存，校验后复用，目标需提供工具执行依赖。

`--config` 与 `--auth-dir` 属于本机，`--database` 属于目标端。配置及引用的凭据通过连接传入目标的私有临时目录，远端 OAuth 刷新尚不回写本机。

断开客户端不会取消服务中的任务。重连按会话 ID 和事件游标恢复，未确认的副作用请求不自动重放。
请求错误区分发送前编码失败、服务拒绝和传输失效；前两者保留连接，未确认的请求仍不重放。
缺失附件通过显式协议结果处理，旧服务返回的错误不按文本猜测。
连接配置使用声明与缓存的模型目录；显式刷新在后台获取目录，不阻塞会话取消和状态读取。
账号初始化保留凭据身份，补填项目不会改写其他账号；保存成功与目录刷新诊断分别返回。

## 入口

- [lib.rs](src/lib.rs)、[connect.rs](src/connect.rs)：准备连接与打开会话。
- [transport.rs](src/transport.rs)、[bootstrap.rs](src/bootstrap.rs)：传输、部署与握手。
- [pump.rs](src/pump.rs)、[prompt.rs](src/prompt.rs)：交互请求和单次任务事件流。
- [accounts.rs](src/accounts.rs)：账号授权与连接元数据；浏览器及项目输入由前端呈现。
- [configuration.rs](src/configuration.rs)：本机配置快照与凭据传递。
- [attachments.rs](src/attachments.rs)：图片与普通文件的有界上传、粘贴路径识别；普通文件单个上限 20 MiB。

## 验证

```sh
cargo test -p orbit-client
```

[项目总览](../../README.md)
