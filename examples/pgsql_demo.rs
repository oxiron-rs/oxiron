//! PostgreSQL 驱动演示：与 mybatis_demo 相同的 Mapper 体验，跑在 PG 上。
//!
//! 自研 oxiron-jdbc PG 驱动（wire protocol v3 + SCRAM-SHA-256 认证），零 sqlx 依赖。
//!
//! 运行（需要一个可用的 PostgreSQL 服务器）：
//! ```powershell
//! $env:OXIRON_PG_URL = "postgresql://user:pass@localhost:5432/mydb"
//! cargo run --example pgsql_demo
//! ```
//! 未设置 OXIRON_PG_URL 时默认连接 `postgresql://postgres@localhost:5432/postgres`。

use oxiron::{delete, insert, select, sql, update, FromRow};
use oxiron_jdbc::postgres::{PgConnection, PgDataSource};
use oxiron_jdbc::DataSource;

#[derive(Debug, FromRow, PartialEq)]
struct User {
    id: i64,
    name: String,
    age: i32,
}

// ─── Mapper 层：与 SQLite 版完全相同的写法，只是连接类型不同 ──────────

#[sql("DROP TABLE IF EXISTS oxiron_users")]
async fn drop_table(conn: &PgConnection) -> Result<()>;

#[sql("CREATE TABLE oxiron_users (id BIGINT PRIMARY KEY, name TEXT NOT NULL, age INT NOT NULL)")]
async fn create_table(conn: &PgConnection) -> Result<()>;

#[insert("INSERT INTO oxiron_users (id, name, age) VALUES (#{id}, #{name}, #{age})")]
async fn insert_user(conn: &PgConnection, id: i64, name: String, age: i32) -> Result<u64>;

#[select("SELECT id, name, age FROM oxiron_users WHERE id = #{id}")]
async fn find_by_id(conn: &PgConnection, id: i64) -> Result<Option<User>>;

#[select("SELECT id, name, age FROM oxiron_users WHERE age >= #{min_age} ORDER BY age")]
async fn find_age_at_least(conn: &PgConnection, min_age: i32) -> Result<Vec<User>>;

#[select("SELECT COUNT(*) FROM oxiron_users WHERE age > #{age}")]
async fn count_older_than(conn: &PgConnection, age: i32) -> Result<i64>;

#[update("UPDATE oxiron_users SET name = #{name} WHERE id = #{id}")]
async fn rename_user(conn: &PgConnection, id: i64, name: String) -> Result<u64>;

#[delete("DELETE FROM oxiron_users WHERE id = #{id}")]
async fn delete_user(conn: &PgConnection, id: i64) -> Result<u64>;

#[sql("SELECT id, name, age FROM oxiron_users WHERE name = #{name}")]
async fn find_by_name(conn: &PgConnection, name: String) -> Result<Option<User>>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("OXIRON_PG_URL").unwrap_or_else(|_| {
        "postgresql://postgres@localhost:5432/postgres".to_string()
    });
    println!("连接 PostgreSQL: {url}");

    let ds = PgDataSource::open(&url)?;
    let conn = match ds.get_connection() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("✗ 无法连接 PostgreSQL: {e}");
            eprintln!("  请启动一个 PG 服务器后设置 OXIRON_PG_URL 重试，例如：");
            eprintln!("  $env:OXIRON_PG_URL = \"postgresql://user:pass@localhost:5432/mydb\"");
            return Ok(());
        }
    };

    drop_table(&conn).await?;
    create_table(&conn).await?;

    assert_eq!(insert_user(&conn, 1, "Alice".into(), 30).await?, 1);
    assert_eq!(insert_user(&conn, 2, "Bob".into(), 17).await?, 1);
    assert_eq!(insert_user(&conn, 3, "Carol".into(), 25).await?, 1);
    println!("✓ 插入 3 行");

    let bob = find_by_id(&conn, 2).await?.expect("Bob 应存在");
    assert_eq!(bob, User { id: 2, name: "Bob".into(), age: 17 });
    println!("✓ find_by_id(2) = {bob:?}");

    let adults = find_age_at_least(&conn, 18).await?;
    assert_eq!(adults.len(), 2);
    println!("✓ find_age_at_least(18) = {adults:?}");

    let older = count_older_than(&conn, 18).await?;
    assert_eq!(older, 2);
    println!("✓ count_older_than(18) = {older}");

    assert_eq!(rename_user(&conn, 3, "Carrie".into()).await?, 1);
    let carrie = find_by_name(&conn, "Carrie".into()).await?.expect("Carrie 应存在");
    assert_eq!(carrie.name, "Carrie");
    println!("✓ rename_user(3, \"Carrie\") → find_by_name = {carrie:?}");

    assert_eq!(delete_user(&conn, 2).await?, 1);
    assert!(find_by_id(&conn, 2).await?.is_none());
    println!("✓ delete_user(2) 后 find_by_id(2) = None");

    drop_table(&conn).await?;
    println!("\n所有断言通过 ✓ —— oxiron-jdbc PostgreSQL 驱动端到端工作正常");
    Ok(())
}
