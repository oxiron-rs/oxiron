/// MyBatis 风格 SQL Mapper 属性宏与 `FromRow` 派生宏（底层走 oxiron-jdbc）。
///
/// 在无函数体的 fn 签名上标注 SQL，宏生成执行查询的函数体：
///
/// ```ignore
/// #[oxiron::select("SELECT id, name, age FROM users WHERE id = #{id}")]
/// async fn find_by_id(conn: &SqliteConnection, id: i64) -> Result<Option<User>>;
/// ```
pub use oxiron_sql::{delete, insert, select, sql, update, FromRow};
