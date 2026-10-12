运行命令：
```bash
cargo run -- --check    # 只读不点：打印识别结果让你对照游戏画面
cargo run              # 输入数字自动解题
```
代码块紧接正文，无需手工插入空行。
```rust
fn main() {
    let count = 42;
    println!("结果：{count}"); // 保留中文注释
}
```
配置示例：
```json
{"enabled": true, "count": 42, "name": "Orbit"}
```
没有语言标记的代码：
```
line one
line two
```
