#!/usr/bin/env bash
# Create an isolated exercise and start Orbit with the user's real model config.
set -euo pipefail
orbit_repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
orbit_prepare=false
orbit_hard=false
for orbit_arg in "$@"; do
    case "$orbit_arg" in
        --prepare-only) orbit_prepare=true ;;
        --hard) orbit_hard=true ;;
        *) echo 'Usage: bash scripts/try-live.sh [--hard] [--prepare-only]' >&2; exit 2 ;;
    esac
done
if [[ ! -f "$orbit_repo/config.toml" ]]; then
    echo "Missing configuration: $orbit_repo/config.toml" >&2
    exit 1
fi
cargo build --manifest-path "$orbit_repo/Cargo.toml" -p orbit-cli --offline
orbit_trial=$(mktemp -d /tmp/orbit-live-XXXXXX)
mkdir "$orbit_trial/project"
if [[ "$orbit_hard" == true ]]; then
    cp -R "$orbit_repo/scripts/fixtures/scheduler/." "$orbit_trial/project/"
else
cat > "$orbit_trial/project/price.c" <<'SOURCE'
#include <stdio.h>
#include <stdlib.h>

/* Return the total in cents. Shipping costs 500 cents below 10000 cents. */
static int total_price(int subtotal) {
    return subtotal > 10000 ? subtotal : subtotal + 500;
}

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    printf("%d\n", total_price(atoi(argv[1])));
    return 0;
}
SOURCE
cat > "$orbit_trial/project/test.sh" <<'TEST'
#!/bin/sh
set -eu
mkdir -p build
cc -Wall -Wextra -Werror price.c -o build/price
for pair in '0 500' '9999 10499' '10000 10000' '10001 10001'; do
    set -- $pair
    actual=$(./build/price "$1")
    if [ "$actual" != "$2" ]; then
        printf 'FAIL subtotal=%s expected=%s actual=%s\n' "$1" "$2" "$actual"
        exit 1
    fi
    printf 'PASS subtotal=%s total=%s\n' "$1" "$actual"
done
printf 'ALL 4 TESTS PASSED\n'
TEST
cat > "$orbit_trial/project/TASK.md" <<'TASK'
这是一个真实工具验收项目，不是模拟回答。

业务规则：金额以分为单位。商品小计达到 10000 分免运费，低于该金额收取 500 分。
price.c 有一个边界条件错误。请完成：

1. 读取 price.c 和 test.sh，理解程序和测试。
2. 使用 run_command 执行 sh test.sh，设置 writable_snapshot=true，先观察失败。
3. 使用 edit_file 做最小局部修改；不要全文重写，不要修改测试或放宽断言。
4. 再使用 run_command 执行 sh test.sh，设置 writable_snapshot=true，确认四项测试全部通过。
5. 简要说明错误、修改和实际测试结果。工具失败时如实报告，不要声称测试成功。

构建副本的产物会丢弃，因此编译和测试已经组合在同一个 test.sh 中。原项目修改必须通过 edit_file。
TASK
fi
cat > "$orbit_trial/start.sh" <<START
#!/usr/bin/env bash
set -euo pipefail
cd -- '$orbit_trial/project'
exec '$orbit_repo/target/debug/orbit' --config '$orbit_repo/config.toml' --database '$orbit_trial/sessions.db' tui
START
chmod 700 "$orbit_trial/start.sh"
printf '\n测试项目：%s/project\n启动入口：bash %s/start.sh\n\n进入后发送：请读取 TASK.md，完成修复并实际运行测试。\n使用真实 API；修改和命令执行按界面提示批准。\n\n' "$orbit_trial" "$orbit_trial"
if [[ "$orbit_prepare" == false ]]; then
    exec bash "$orbit_trial/start.sh"
fi
