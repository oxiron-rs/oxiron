//! `#[sql]` Mapper × PostgreSQL 端到端集成测试（自研 oxiron-jdbc PG 驱动）。
//!
//! 使用 Rust 原生测试框架（`#[test]`），同步 Mapper，无需异步运行时。
//!
//! **环境变量门控**：设置 `OXIRON_PG_URL`（如 `postgresql://user:pass@localhost:5432/mydb`）
//! 后测试才会实际连接执行；未设置时直接跳过（打印提示、测试通过）。
//!
//! ```powershell
//! $env:OXIRON_PG_URL = "postgresql://user:pass@localhost:5432/mydb"
//! cargo test --test pg_mapper
//! ```
//!
//! 覆盖与 `sqlite_mapper` 相同的契约：增删改查、单行/多行/标量/None、参数按名字映射。

use oxiron::{delete, insert, select, sql, update, FromRow};
use oxiron_jdbc::postgres::{PgConnection, PgDataSource};
use oxiron_jdbc::DataSource;

#[derive(Debug, FromRow, PartialEq)]
struct User {
    id: i64,
    name: String,
    age: i32,
}

// ─── Mapper 层：与 SQLite 版同一写法，仅连接类型不同 ──────────────────

#[sql("DROP TABLE IF EXISTS oxiron_users")]
fn drop_table(conn: &PgConnection) -> Result<()>;

#[sql("CREATE TABLE oxiron_users (id BIGINT PRIMARY KEY, name TEXT NOT NULL, age INT NOT NULL)")]
fn create_table(conn: &PgConnection) -> Result<()>;

#[insert("INSERT INTO oxiron_users (id, name, age) VALUES (#{id}, #{name}, #{age})")]
fn insert_user(conn: &PgConnection, id: i64, name: String, age: i32) -> Result<u64>;

#[select("SELECT id, name, age FROM oxiron_users WHERE id = #{id}")]
fn find_by_id(conn: &PgConnection, id: i64) -> Result<Option<User>>;

#[select("SELECT id, name, age FROM oxiron_users WHERE age >= #{min_age} ORDER BY age")]
fn find_age_at_least(conn: &PgConnection, min_age: i32) -> Result<Vec<User>>;

#[select("SELECT COUNT(*) FROM oxiron_users WHERE age > #{age}")]
fn count_older_than(conn: &PgConnection, age: i32) -> Result<i64>;

/// 占位符顺序（name, id）与形参顺序（id, name）相反：验证按名字映射
#[update("UPDATE oxiron_users SET name = #{name} WHERE id = #{id}")]
fn rename_user(conn: &PgConnection, id: i64, name: String) -> Result<u64>;

#[delete("DELETE FROM oxiron_users WHERE id = #{id}")]
fn delete_user(conn: &PgConnection, id: i64) -> Result<u64>;

#[sql("SELECT id, name, age FROM oxiron_users WHERE name = #{name}")]
fn find_by_name(conn: &PgConnection, name: String) -> Result<Option<User>>;

/// 单一测试覆盖完整契约（共享真实 PG 的同一张表，避免测试并行冲突）。
#[test]
fn pg_end_to_end() -> Result<(), Box<dyn std::error::Error>> {
    let Ok(url) = std::env::var("OXIRON_PG_URL") else {
        eprintln!("跳过 PostgreSQL 测试：未设置 OXIRON_PG_URL");
        return Ok(());
    };

    let ds = PgDataSource::open(&url)?;
    let conn = ds.get_connection()?;

    // 可重复运行：先清场
    drop_table(&conn)?;
    create_table(&conn)?;

    // 增
    assert_eq!(insert_user(&conn, 1, "Alice".into(), 30)?, 1);
    assert_eq!(insert_user(&conn, 2, "Bob".into(), 17)?, 1);
    assert_eq!(insert_user(&conn, 3, "Carol".into(), 25)?, 1);

    // 单行
    let bob = find_by_id(&conn, 2)?.expect("Bob 应存在");
    assert_eq!(bob, User { id: 2, name: "Bob".into(), age: 17 });

    // 多行（按 age 排序）+ 标量
    let adults = find_age_at_least(&conn, 18)?;
    assert_eq!(
        adults,
        vec![
            User { id: 3, name: "Carol".into(), age: 25 },
            User { id: 1, name: "Alice".into(), age: 30 },
        ]
    );
    assert_eq!(count_older_than(&conn, 18)?, 2);

    // 按名字映射的参数绑定（形参与占位符顺序相反）
    assert_eq!(rename_user(&conn, 3, "Carrie".into())?, 1);
    let carrie = find_by_name(&conn, "Carrie".into())?.expect("Carrie 应存在");
    assert_eq!(carrie, User { id: 3, name: "Carrie".into(), age: 25 });

    // 删 + None 语义
    assert_eq!(delete_user(&conn, 2)?, 1);
    assert!(find_by_id(&conn, 2)?.is_none());

    // 清场
    drop_table(&conn)?;
    Ok(())
}
