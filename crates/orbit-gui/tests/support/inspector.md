# 工作面板原生验收

`inspector.rs` 通过测试服务的正常协议恢复一个混合状态会话。它不调用模型，也不执行记录中的命令。

在临时工作区创建以下空文件，然后以 `--resume demo` 打开：

- `.fixture-inspector`：恢复 18 个工具记录、3 个文件修改、已批准计划和 3 个子 Agent。
- `.fixture-inspector-hold`：让最后一个 `cargo test` 保持运行中；删除后会返回结果并完成本轮。
- `.fixture-subagents`：启用相同 ID 的子 Agent 列表和对话，包含运行中的子任务。
- `.fixture-input-queue`：启用 35 条分页的待处理输入。
- `.fixture-recovery`：把收到的协议请求记录到 `.fixture-requests`，供验收使用。

工具记录包括成功命令、退出码 2 的失败命令、文件读取和写入、后台委派及运行中的检查。长路径、中文文件名和较长命令用于检查省略与溢出。计划、修改和 Agent 数据通过现有投影和协议读取，不替换 UI 的渲染逻辑。

构建 `orbit-gui` 与 `orbit-gui-fixture` 后，在隔离显示器运行（`qa_root` 应指向新建的临时目录，里面准备独立的配置、偏好、认证目录与工作区）：

```sh
Xvfb :91 -screen 0 1440x1000x24 -nolisten tcp
env -u WAYLAND_DISPLAY DISPLAY=:91 XDG_RUNTIME_DIR="$qa_root/runtime" \
  target/debug/orbit-gui \
  --server "$PWD/target/debug/orbit-gui-fixture" \
  --config "$qa_root/config.toml" --auth-dir "$qa_root/auth" \
  --database "$qa_root/fixture.db" --preferences "$qa_root/gui.json" \
  --workspace "$qa_root/workspace" --model fixture::local --resume demo
```

验收顺序：

1. 展开工作面板，检查执行记录的名称、状态、命令摘要、时长和失败标记；滚动到首尾，正文位置应保持不变。
2. 点击成功和失败记录，确认完整工具结果可读；关闭后焦点返回工作面板。
3. 切换计划、变更、Agent，检查共同边距、表头和空白密度；打开长路径差异和子任务。
4. 保持 `.fixture-inspector-hold`，检查运行中状态；删除标记，检查完成后状态与滚动位置。
5. 在 800×560 与较宽窗口、16 与 20 的基础字体、浅色与深色主题下重复。检查标题、Tab、状态和底部动作没有被截断，局部滚动条紧贴所属区域边缘。

恢复 `demo` 时会重新生成这组记录。删除 `.fixture-inspector` 并重新打开，可检查常规无工具数据的空状态。用过的临时配置、数据库及 Xvfb 进程只属于本次验收；不要复用或停止用户实际工作区中的服务。

以 `--resume tool-details` 打开可检查工具详情。生成的长命令只有一个逻辑行，
末尾带 `COMMAND-END`；在宽、窄窗口及 16、20 字体下打开详情，再调整窗口宽度，
确认命令区随实际换行行数增减高度；未达限高时完整显示，超限后可滚动到末尾，
目录不会遮住最后一行。
短命令应保持紧凑。另以 `.fixture-complete-output` 的 `demo` 会话检查长脚本：
命令区达到高度上限后内部滚动，底部 `COMMAND-END` 与 `PY` 均可读，输出区仍可用。

同一 `tool-details` 会话的 ReadImage 详情应显示图片路径和生成的小图；点击或键盘激活后
打开大图，Esc 关闭大图回到缩略图弹窗，再次 Esc 关闭详情。检查加载失败及断开
连接时的反馈，不显示其他图片参数或原始结果。

用 `--resume navigation` 检查大量未排版历史：向上滚动出现悬浮的圆形返回按钮，
它不占输入框上方整行，也不遮盖整条会话内容。用滚轮或拖动滚动条回到底部，
按钮应自动消失；继续滚动或点击按钮后均可恢复跟随。

以 `--resume tool-history` 打开，滚动至返回按钮覆盖工具行：悬停按钮不能触发
下方整行高亮，点击只回到最新输出；鼠标位于按钮上时仍能用滚轮滚动会话。
