//! oxiron 公开 API 的集成测试。
//!
//! `tests/` 目录下的每个 `.rs` 文件都会被编译为一个独立的 crate，
//! 只能通过公开 API（`oxiron::...`）访问库，无法触及私有项。
//! 这些测试模拟的是"外部使用者"的真实调用方式。

use oxiron::add;

#[test]
fn add_basic() {
    assert_eq!(add(2, 2), 4);
}

#[test]
fn add_with_zero() {
    assert_eq!(add(0, 0), 0);
    assert_eq!(add(42, 0), 42);
    assert_eq!(add(0, -42), -42);
}

#[test]
fn add_negative_numbers() {
    assert_eq!(add(-1, -1), -2);
    assert_eq!(add(-5, 3), -2);
    assert_eq!(add(5, -3), 2);
}

#[test]
fn add_commutative() {
    let pairs = [(1, 2), (-7, 7), (100, -50), (i32::MIN, 0), (0, i32::MAX)];
    for (a, b) in pairs {
        assert_eq!(add(a, b), add(b, a), "加法应满足交换律: {a} + {b}");
    }
}

#[test]
fn add_large_values() {
    // 边界值：不触发溢出的最大/最小组合
    assert_eq!(add(1_000_000_000, 1_000_000_000), 2_000_000_000);
    assert_eq!(add(i32::MAX - 1, 1), i32::MAX);
    assert_eq!(add(i32::MIN + 1, -1), i32::MIN);
}

#[test]
fn add_matches_readme_example() {
    // 与 README.md 中的用法示例保持一致
    let sum = oxiron::add(2, 2);
    assert_eq!(sum, 4);
}
