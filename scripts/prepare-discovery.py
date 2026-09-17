"""Prepare a real-model file discovery exercise inside try-live's temporary project."""
import pathlib
import sys

trial = pathlib.Path(sys.argv[1])
project = trial / "project"
source = project / "components" / "结算" / "src"
source.mkdir(parents=True)
(project / "price.c").rename(source / "price.c")
script = project / "test.sh"
script.write_text(script.read_text().replace("price.c -o", "components/结算/src/price.c -o"))
for index in range(12):
    directory = project / "components" / f"module-{index:02}"
    directory.mkdir()
    (directory / "README.md").write_text("这是无关的占位模块，没有结算逻辑。\n")
(project / "TASK.md").write_text("""这是目录发现工具的真实 API 验收项目。

结算程序有一个运费边界错误：商品小计达到 10000 分免运费，低于该金额收取 500 分。
源文件位置没有提供，请自己找到它。

1. 使用 list_files 浏览项目，再用 depth=3、limit=4 浏览 components；使用 next_offset 至少翻到第二页。留意中文目录。
2. 找到结算源文件，使用 read_file 读取源码与根目录 test.sh。
3. 用 run_command 执行 sh test.sh，设置 writable_snapshot=true，观察失败。
4. 使用 edit_file 最小修改源文件；不能改测试、TASK.md 或其他模块。
5. 再运行测试，说明实际结果和发现的源码路径。不要用 shell 的 ls/find 替代 list_files。

如果工具失败，如实报告，不要声称执行成功。
""")
print("目录发现验收已准备：包含中文嵌套目录、分页和真实编译测试。")
