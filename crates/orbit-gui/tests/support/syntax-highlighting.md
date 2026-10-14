## HDL 代码高亮

```verilog
module fixture_channel (input wire [11:0] in, output wire [11:0] out);
    genvar i; // 生成变量：中文注释应与关键字区分
    generate
        for (i = 0; i < 12; i = i + 1) begin : gen_reverse
            assign out[i] = in[11 - i];
        end
    endgenerate
    initial $display("位序反转：%b", out);
endmodule
```

```systemverilog
module reverse_bits (input logic [11:0] in, output logic [11:0] out);
    /* 跨行注释：第一行
       第二行仍然属于注释。 */
    assign out = {<<{in}}; // 按 1 bit 反转
    initial begin
        assert ($bits(out) == 12) else $error("宽度错误");
    end
endmodule
```

## 常用语言

```rust
pub fn greet(name: &str) -> String {
    // 中文与 emoji：你好 🌟
    format!("Hello, {name}! {}", 42)
}
```

```python
def greet(name: str) -> str:
    """跨行字符串开始
    第二行仍应高亮为字符串。"""
    return f"你好，{name}!" if len(name) > 0 else "访客"
```

```bash
#!/usr/bin/env bash
# shell 注释与命令、字符串、变量区分
count=3
printf '%s\n' "构建 ${count} 个目标"
for file in src/*.rs; do cargo check --quiet; done
```

```sql
-- 查询活跃用户
SELECT id, name, COUNT(*) AS visits
FROM users WHERE active = TRUE AND name <> '访客'
GROUP BY id, name LIMIT 10;
```

```cpp
#include <string>
// 模板参数与数字、字符串
template <typename T> T twice(T value) { return value * 2; }
const std::string message = "你好";
auto result = twice(21);
```

```tsx
type Props = { name: string; count: number };
export const Greeting = ({ name, count }: Props) => (
  // JSX 标签、属性和插值
  <button disabled={count === 0}>你好，{name}!</button>
);
```

```yaml
# YAML 字段、布尔值、数字和字符串
name: "Orbit 高亮"
enabled: true
retries: 3
languages:
  - verilog
  - systemverilog
```

```json
{
  "name": "Orbit 高亮",
  "enabled": true,
  "retries": 3,
  "optional": null,
  "languages": ["verilog", "systemverilog"]
}
```

```markdown
# Markdown 标题
**粗体**、*斜体*、~~删除线~~、`原样代码`。
- [链接](https://example.com)
> 中文引用与 emoji 🌟。
```

## 未知语言保持原文

```orbit-unknown-language
未知语言不会误判为另一种语法。
module sample("你好 🌟", 42) // 保留文字与标点
  indentation stays intact
```
