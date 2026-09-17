"""Prepare a real-model compaction exercise; never contact or print credentials."""
import json
import os
from pathlib import Path
import re
import sys
import tomllib

trial, source = map(Path, sys.argv[1:])
text = source.read_text()
# Change only root keys. Keep provider credentials outside the user project and
# protect the copied config using the same tool exclusions as a normal config.
root, *tables = re.split(r"(?m)(?=^\[)", text, maxsplit=1)
root = re.sub(r"(?m)^(context_tokens|max_output_tokens|auto_compact)\s*=.*\n?", "", root)
text = "context_tokens = 32000\nmax_output_tokens = 12000\nauto_compact = true\n" + root + "".join(tables)
tomllib.loads(text)
with os.fdopen(os.open(trial / "config.toml", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as config:
    config.write(text)

project = trial / "project"
notes = project / "review-notes"
notes.mkdir()
expected = {}
for index in range(1, 13):
    key = f"review-{index:02d}"
    # Unique facts at the start of each large document test whether early facts
    # survive semantic compaction, rather than just the most recent tool output.
    fact = {"ticket": f"ORB-{701 + index * 37}", "decision": ["preserve_order", "keep_idle_gaps", "reject_cycles"][index % 3]}
    expected[key] = fact
    lines = [f"# {key}", "需要保留的审查结论：" + json.dumps(fact), "以下为归档运行记录，数字仅用于排查，不能代替上面的审查结论。"]
    for row in range(110):
        lines.append(f"trace {index:02d}/{row:03d}: queue=worker-{row % 7} ready_at={row * 13} started_at={row * 13 + index} duration={row % 17 + 1} source=historical-audit; preserve dependency constraints and stable scheduling order.")
    (notes / f"{key}.md").write_text("\n".join(lines) + "\n")
with (project / "TASK.md").open("a") as task:
    task.write("""

## 自动上下文压缩验收

在开始修复之前，按文件名顺序用 read_file 分别完整读取 review-notes/ 下的 12 份资料（每份约 113 行），不要合并成一次命令输出，不要跳过或截取。记录每份开头的 ticket 与 decision。后面大量 trace 是背景资料，不需要逐条复述。
读完全部资料之后，再创建 evidence.json，格式为 {"review-01":{"ticket":"实际编号","decision":"实际结论"}, ...}，包含全部 12 份。不要提前把审查结论写入文件，不要修改或删除资料；尽量从上下文中的记录/摘要恢复结论，不确定时允许重读，但必须在最终报告列明重读了哪些资料。
随后继续完成上面的 scheduler 修复任务，保持测试和公开 API 不变，实际运行测试。在最后报告中列出修复、验证结果，以及你无法确定的事情。这个任务使用真实 API，资料读取期间可能自动压缩上下文；不要为此停止任务。
""")
# Reference answers and the checker stay outside the model's workspace.
(trial / "expected-evidence.json").write_text(json.dumps(expected, indent=2) + "\n")
(trial / "check-evidence.py").write_text('''import json
from pathlib import Path
root = Path(__file__).resolve().parent
expected = json.loads((root / "expected-evidence.json").read_text())
actual = json.loads((root / "project/evidence.json").read_text())
errors = [key for key, value in expected.items() if actual.get(key) != value]
if errors:
    raise SystemExit("FAIL missing or changed evidence: " + ", ".join(errors))
print("PASS: all 12 early and recent review facts preserved")
''')
print(f"验收命令：python3 {trial}/check-evidence.py")
