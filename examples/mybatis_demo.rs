//! MyBatis 风格 SQL Mapper 演示（oxiron-sql + oxiron-jdbc，无 sqlx 依赖）。
//!
//! 核心卖点：**函数签名 + SQL 注解 = 可执行的数据库方法**。
//! 宏在编译期生成对 oxiron-jdbc（JDBC 风格抽象层）的调用：
//! 预编译语句 + 位置绑定防注入，`#{name}` 按参数名映射，编译期校验。
//!
//! 运行：`cargo run --example mybatis_demo`

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

/// 建表（DDL 也可以是 mapper）
#[sql("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, age INTEGER NOT NULL)")]
async fn create_table(conn: &SqliteConnection) -> Result<()>;

/// 插入：返回受影响行数
#[insert("INSERT INTO users (id, name, age) VALUES (#{id}, #{name}, #{age})")]
async fn insert_user(conn: &SqliteConnection, id: i64, name: String, age: i32) -> Result<u64>;

/// 单行查询：`Option<T>` → 找不到返回 None
#[select("SELECT id, name, age FROM users WHERE id = #{id}")]
async fn find_by_id(conn: &SqliteConnection, id: i64) -> Result<Option<User>>;

/// 多行查询：`Vec<T>`
#[select("SELECT id, name, age FROM users WHERE age >= #{min_age} ORDER BY age")]
async fn find_age_at_least(conn: &SqliteConnection, min_age: i32) -> Result<Vec<User>>;

/// 标量查询：`#[select]` + 标量返回 → query_scalar
#[select("SELECT COUNT(*) FROM users WHERE age > #{age}")]
async fn count_older_than(conn: &SqliteConnection, age: i32) -> Result<i64>;

/// 更新：注意 SQL 里 #{name} 在 #{id} 之前，但形参顺序相反——
/// 占位符按名字映射，与参数书写顺序无关（MyBatis 同款语义）
#[update("UPDATE users SET name = #{name} WHERE id = #{id}")]
async fn rename_user(conn: &SqliteConnection, id: i64, name: String) -> Result<u64>;

/// 删除：返回受影响行数
#[delete("DELETE FROM users WHERE id = #{id}")]
async fn delete_user(conn: &SqliteConnection, id: i64) -> Result<u64>;

/// `#[sql]` 通用形态：按返回类型自动推断（此处 Option<User> → query_optional）
#[sql("SELECT id, name, age FROM users WHERE name = #{name}")]
async fn find_by_name(conn: &SqliteConnection, name: String) -> Result<Option<User>>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // JDBC 风格：DataSource → Connection
    let ds = SqliteDataSource::open("sqlite::memory:")?;
    let conn = ds.get_connection()?;

    create_table(&conn).await?;

    // 插入
    assert_eq!(insert_user(&conn, 1, "Alice".into(), 30).await?, 1);
    assert_eq!(insert_user(&conn, 2, "Bob".into(), 17).await?, 1);
    assert_eq!(insert_user(&conn, 3, "Carol".into(), 25).await?, 1);
    println!("✓ 插入 3 行");

    // 单行查询
    let bob = find_by_id(&conn, 2).await?.expect("Bob 应存在");
    assert_eq!(bob, User { id: 2, name: "Bob".into(), age: 17 });
    println!("✓ find_by_id(2) = {bob:?}");

    // 多行查询
    let adults = find_age_at_least(&conn, 18).await?;
    assert_eq!(adults.len(), 2); // Alice(30), Carol(25)
    println!("✓ find_age_at_least(18) = {adults:?}");

    // 标量查询
    let older = count_older_than(&conn, 18).await?;
    assert_eq!(older, 2);
    println!("✓ count_older_than(18) = {older}");

    // 更新（参数顺序与占位符顺序不同的映射）
    assert_eq!(rename_user(&conn, 3, "Carrie".into()).await?, 1);
    let carrie = find_by_name(&conn, "Carrie".into()).await?.expect("Carrie 应存在");
    assert_eq!(carrie.name, "Carrie");
    println!("✓ rename_user(3, \"Carrie\") → find_by_name = {carrie:?}");

    // 删除 + None 语义
    assert_eq!(delete_user(&conn, 2).await?, 1);
    assert!(find_by_id(&conn, 2).await?.is_none());
    println!("✓ delete_user(2) 后 find_by_id(2) = None");

    println!("\n所有断言通过 ✓ —— oxiron-sql + oxiron-jdbc 端到端工作正常（零 sqlx 依赖）");
    Ok(())
}
