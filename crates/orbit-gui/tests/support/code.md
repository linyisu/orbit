运行命令：
```bash
samplectl inspect     # 输出合成样本的元数据
samplectl summarize   # 汇总示例记录
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
