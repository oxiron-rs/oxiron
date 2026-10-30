//! oxiron 使用示例程序。
//!
//! `examples/` 目录下每个 `.rs` 文件都会被编译成一个独立的二进制程序，
//! 自动链接 oxiron 库本身，模拟真实用户在自己项目中的调用方式。
//!
//! 运行方式：
//! ```powershell
//! cargo run --example demo
//! ```

use oxiron::add;

fn main() {
    println!("=== oxiron demo ===");

    // 1. 基本用法
    let sum = add(2, 2);
    println!("add(2, 2) = {sum}");
    assert_eq!(sum, 4);

    // 2. 负数与零
    println!("add(-5, 3) = {}", add(-5, 3));
    println!("add(42, 0) = {}", add(42, 0));

    // 3. 实际场景：对一组数字求和
    let numbers = [1, 2, 3, 4, 5];
    let total = numbers.iter().copied().fold(0, add);
    println!("sum of {numbers:?} = {total}");
    assert_eq!(total, 15);

    println!("所有断言通过 ✓");
}
