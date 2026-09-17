"""Prepare a real-model command-size and output-lifecycle exercise."""

from pathlib import Path
import sys


project = Path(sys.argv[1]) / "project"
payload = "0123456789abcdef" * 640
command = (
    "python3 - <<'PY'\n"
    f"payload = '{payload}'\n"
    "assert len(payload) == 10240\n"
    "print('x' * 100000)\n"
    "print('LONG_COMMAND_AND_OUTPUT_OK')\n"
    "raise SystemExit(7)\n"
    "PY\n"
)
(project / "command.txt").write_text(command, encoding="utf-8")
(project / "TASK.md").write_text(
    """# 真实模型验收：长命令与大量输出

请实际调用工具完成测试，不要仅解释预期结果。

1. 用 read_file 读取 command.txt，它是一条超过 10 KiB 的完整 Shell 命令。
2. 将文件正文原样作为 run_command 的 command 参数执行。不要改成 sh command.txt，
   不要缩写长字符串，不要通过读取文件替代参数中的正文；本项验收的就是长参数。
3. 本命令故意打印超过 100 KB 的输出，然后打印 LONG_COMMAND_AND_OUTPUT_OK，最后退出码为 7。
   核对：末尾标记存在、exit_code=7、output_truncated=true、stdout_total_bytes=100028、
   timed_out=false、cancelled=false。退出码 7 是预期，不要修改或重跑来“修复”。
4. 使用 write_file 在当前目录创建 report.md，记录实际工具结果中的上述字段，
   不要把 100 KB 的输出全文写入报告。若参数复制失败或执行失败，如实说明，不得声称通过。

不需要安装依赖或联网；模型请求使用启动器提供的真实 API。
""",
    encoding="utf-8",
)
