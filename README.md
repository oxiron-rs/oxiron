# oxiron

[![crates.io](https://img.shields.io/crates/v/oxiron.svg)](https://crates.io/crates/oxiron)
[![docs.rs](https://img.shields.io/docsrs/oxiron)](https://docs.rs/oxiron)
[![license](https://img.shields.io/badge/license-Apache--2.0-blue.svg)]()

A lightweight, zero-dependency utility library for Rust.

> **Note / 说明**：`0.1.0` 为初始版本，目前仅包含占位 API，后续版本将持续完善功能与文档。

## Usage / 使用

Add this to your `Cargo.toml`:

```toml
[dependencies]
oxiron = "0.1"
```

Example:

```rust
fn main() {
    let sum = oxiron::add(2, 2);
    assert_eq!(sum, 4);
}
```

## Requirements / 环境要求

- Rust 1.85+ (Edition 2024)

## License / 许可证

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE) or <http://www.apache.org/licenses/LICENSE-2.0>)
