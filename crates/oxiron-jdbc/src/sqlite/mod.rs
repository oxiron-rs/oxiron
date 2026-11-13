//! SQLite 驱动模块入口（对标 `sqlite-jdbc`）。
//!
//! 实现在 [`sqlite`] 子模块中，此处平铺重导出，
//! 保持公开路径 `oxiron_jdbc::sqlite::{SqliteConnection, SqliteDataSource}` 不变。

mod sqlite;
pub use sqlite::*;
