//! `#[sql]` Mapper × SQLite 端到端集成测试（oxiron-sql + oxiron-jdbc，零 sqlx、零 tokio）。
//!
//! 使用 Rust 原生测试框架（`#[test]` + `cargo test`）：oxiron-jdbc 是同步 API，
//! Mapper 声明为同步 `fn` 即可，无需任何异步运行时。
//!
//! 覆盖：增删改查、单行/多行/标量/None 各种返回形状、参数按名字映射（与书写顺序无关）。
//! 每个测试使用独立的内存库（`sqlite::memory:`），互不干扰、可并行。

use oxiron::{delete, insert, select, sql, update, FromRow};
use oxiron_jdbc::sqlite::{SqliteConnection, SqliteDataSource};
use oxiron_jdbc::DataSource;

#[derive(Debug, FromRow, PartialEq)]
struct User {
    id: i64,
    name: String,
    age: i32,
}

// ─── Mapper 层：只有签名和 SQL，函数体由宏生成 ────────────────────────

#[sql("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, age INTEGER NOT NULL)")]
fn create_table(conn: &SqliteConnection) -> Result<()>;

#[insert("INSERT INTO users (id, name, age) VALUES (#{id}, #{name}, #{age})")]
fn insert_user(conn: &SqliteConnection, id: i64, name: String, age: i32) -> Result<u64>;

#[select("SELECT id, name, age FROM users WHERE id = #{id}")]
fn find_by_id(conn: &SqliteConnection, id: i64) -> Result<Option<User>>;

#[select("SELECT id, name, age FROM users WHERE age >= #{min_age} ORDER BY age")]
fn find_age_at_least(conn: &SqliteConnection, min_age: i32) -> Result<Vec<User>>;

#[select("SELECT COUNT(*) FROM users WHERE age > #{age}")]
fn count_older_than(conn: &SqliteConnection, age: i32) -> Result<i64>;

/// 注意：SQL 里 #{name} 在 #{id} 之前，形参顺序相反——占位符按名字映射
#[update("UPDATE users SET name = #{name} WHERE id = #{id}")]
fn rename_user(conn: &SqliteConnection, id: i64, name: String) -> Result<u64>;

#[delete("DELETE FROM users WHERE id = #{id}")]
fn delete_user(conn: &SqliteConnection, id: i64) -> Result<u64>;

#[sql("SELECT id, name, age FROM users WHERE name = #{name}")]
fn find_by_name(conn: &SqliteConnection, name: String) -> Result<Option<User>>;

/// 每个测试独立的内存库：建表 + 三个种子用户（Alice30 / Bob17 / Carol25）。
fn setup() -> Result<SqliteConnection, Box<dyn std::error::Error>> {
    let ds = SqliteDataSource::open("sqlite::memory:")?;
    let conn = ds.get_connection()?;
    create_table(&conn)?;
    assert_eq!(insert_user(&conn, 1, "Alice".into(), 30)?, 1);
    assert_eq!(insert_user(&conn, 2, "Bob".into(), 17)?, 1);
    assert_eq!(insert_user(&conn, 3, "Carol".into(), 25)?, 1);
    Ok(conn)
}

#[test]
fn crud_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
    let conn = setup()?;

    // 增：返回受影响行数
    assert_eq!(insert_user(&conn, 4, "Dave".into(), 40)?, 1);

    // 查：单行映射为结构体
    let bob = find_by_id(&conn, 2)?.expect("Bob 应存在");
    assert_eq!(bob, User { id: 2, name: "Bob".into(), age: 17 });

    // 改：返回受影响行数
    assert_eq!(rename_user(&conn, 4, "David".into())?, 1);
    assert_eq!(find_by_id(&conn, 4)?.expect("Dave 应存在").name, "David");

    // 删 + None 语义
    assert_eq!(delete_user(&conn, 4)?, 1);
    assert!(find_by_id(&conn, 4)?.is_none());

    Ok(())
}

#[test]
fn query_shapes() -> Result<(), Box<dyn std::error::Error>> {
    let conn = setup()?;

    // 多行：Vec<T>，按 age 排序
    let adults = find_age_at_least(&conn, 18)?;
    assert_eq!(
        adults,
        vec![
            User { id: 3, name: "Carol".into(), age: 25 },
            User { id: 1, name: "Alice".into(), age: 30 },
        ]
    );

    // 标量：COUNT(*) → i64
    assert_eq!(count_older_than(&conn, 18)?, 2);
    assert_eq!(count_older_than(&conn, 100)?, 0);

    Ok(())
}

#[test]
fn named_parameter_mapping() -> Result<(), Box<dyn std::error::Error>> {
    let conn = setup()?;

    // rename_user 的 SQL 占位符顺序（name, id）与形参顺序（id, name）相反：
    // 若绑定按名字映射正确，改的就是 id=3 这行
    assert_eq!(rename_user(&conn, 3, "Carrie".into())?, 1);
    let carrie = find_by_name(&conn, "Carrie".into())?.expect("Carrie 应存在");
    assert_eq!(carrie, User { id: 3, name: "Carrie".into(), age: 25 });
    // 其他行不受影响
    assert_eq!(find_by_id(&conn, 1)?.expect("Alice 应存在").name, "Alice");

    Ok(())
}

#[test]
fn missing_rows_return_none() -> Result<(), Box<dyn std::error::Error>> {
    let conn = setup()?;

    assert!(find_by_id(&conn, 999)?.is_none());
    assert!(find_by_name(&conn, "nobody".into())?.is_none());

    Ok(())
}
