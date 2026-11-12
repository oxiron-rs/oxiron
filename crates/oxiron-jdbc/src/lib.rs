//! oxiron-jdbc —— JDBC 风格的数据库抽象层（对标 `java.sql`）。
//!
//! ## 与 JDBC 的对应关系
//!
//! | java.sql | oxiron-jdbc | 说明 |
//! |---|---|---|
//! | `DataSource` | [`DataSource`] | 获取连接的数据源 |
//! | `Connection` | [`Connection`] | 数据库连接 |
//! | `PreparedStatement` | [`PreparedStatement`] | 预编译语句（防注入绑定） |
//! | `ResultSet` | [`ResultSet`] | 结果集游标 |
//! | `ResultSetMetaData`/列值 | [`Row`] + [`Value`] | 一行数据的强类型取值 |
//! | `TypeHandler` | [`FromValue`] / [`ToValue`] | 类型 ↔ 列值转换 |
//! | `RowMapper<T>` | [`FromRow`] | 行 → Bean 映射（`#[derive(FromRow)]`） |
//!
//! ## 设计原则
//!
//! - **接口与驱动分离**：核心只定义 trait 与数据类型；具体驱动（如 [`sqlite`] 模块）
//!   实现这些 trait，未来可插拔 MySQL/PostgreSQL 驱动；
//! - **同步 API**：与 JDBC 一致。异步框架中使用时由上层包装（`spawn_blocking` 等，路线图）；
//! - **预编译绑定**：参数只走 `PreparedStatement::bind`，不提供字符串拼接接口。
//!
//! 映射查询的便捷函数（[`query_one`] / [`query_optional`] / [`query_all`] / [`query_scalar`]）
//! 是 `#[sql]` 宏生成代码的落点——逻辑在核心层，宏保持薄。

use std::fmt;

pub mod postgres;
pub mod sqlite;

// ─────────────────────────────────────────────────────────────────────
// 值与类型转换（对标 TypeHandler）
// ─────────────────────────────────────────────────────────────────────

/// 数据库列值（与具体驱动解耦的中间表示）。
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

/// Rust 值 → [`Value`]（绑定参数方向）。
pub trait ToValue {
    fn to_value(&self) -> Value;
}

/// [`Value`] → Rust 值（读取列值方向）。
pub trait FromValue: Sized {
    fn from_value(v: Value) -> Result<Self, JdbcError>;
}

macro_rules! impl_int_to_value {
    ($($t:ty),*) => {
        $(impl ToValue for $t {
            fn to_value(&self) -> Value { Value::Int(*self as i64) }
        })*
    };
}
impl_int_to_value!(i8, i16, i32, i64, isize, u8, u16, u32);

impl ToValue for u64 {
    fn to_value(&self) -> Value {
        Value::Int(*self as i64) // 注：超过 i64::MAX 的值截断，业务侧应避免
    }
}
impl ToValue for f32 {
    fn to_value(&self) -> Value {
        Value::Real(*self as f64)
    }
}
impl ToValue for f64 {
    fn to_value(&self) -> Value {
        Value::Real(*self)
    }
}
impl ToValue for bool {
    fn to_value(&self) -> Value {
        Value::Bool(*self)
    }
}
impl ToValue for String {
    fn to_value(&self) -> Value {
        Value::Text(self.clone())
    }
}
impl ToValue for &str {
    fn to_value(&self) -> Value {
        Value::Text((*self).to_string())
    }
}
impl ToValue for Vec<u8> {
    fn to_value(&self) -> Value {
        Value::Blob(self.clone())
    }
}
impl ToValue for Value {
    fn to_value(&self) -> Value {
        self.clone()
    }
}
impl<T: ToValue> ToValue for Option<T> {
    fn to_value(&self) -> Value {
        match self {
            None => Value::Null,
            Some(v) => v.to_value(),
        }
    }
}

/// `Int` 精确转换到目标整数类型（范围检查）。
macro_rules! impl_int_from_value {
    ($($t:ty),*) => {
        $(impl FromValue for $t {
            fn from_value(v: Value) -> Result<Self, JdbcError> {
                match v {
                    Value::Int(n) => <$t>::try_from(n).map_err(|_| JdbcError::TypeMismatch {
                        expected: stringify!($t),
                        got: Value::Int(n),
                    }),
                    other => Err(JdbcError::TypeMismatch {
                        expected: stringify!($t),
                        got: other,
                    }),
                }
            }
        })*
    };
}
impl_int_from_value!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

impl FromValue for f64 {
    fn from_value(v: Value) -> Result<Self, JdbcError> {
        match v {
            Value::Real(f) => Ok(f),
            Value::Int(n) => Ok(n as f64),
            other => Err(JdbcError::TypeMismatch {
                expected: "f64",
                got: other,
            }),
        }
    }
}
impl FromValue for f32 {
    fn from_value(v: Value) -> Result<Self, JdbcError> {
        Ok(f64::from_value(v)? as f32)
    }
}
impl FromValue for bool {
    fn from_value(v: Value) -> Result<Self, JdbcError> {
        match v {
            Value::Bool(b) => Ok(b),
            Value::Int(n) => Ok(n != 0),
            other => Err(JdbcError::TypeMismatch {
                expected: "bool",
                got: other,
            }),
        }
    }
}
impl FromValue for String {
    fn from_value(v: Value) -> Result<Self, JdbcError> {
        match v {
            Value::Text(s) => Ok(s),
            other => Err(JdbcError::TypeMismatch {
                expected: "String",
                got: other,
            }),
        }
    }
}
impl FromValue for Vec<u8> {
    fn from_value(v: Value) -> Result<Self, JdbcError> {
        match v {
            Value::Blob(b) => Ok(b),
            other => Err(JdbcError::TypeMismatch {
                expected: "Vec<u8>",
                got: other,
            }),
        }
    }
}
impl FromValue for Value {
    fn from_value(v: Value) -> Result<Self, JdbcError> {
        Ok(v)
    }
}
impl<T: FromValue> FromValue for Option<T> {
    fn from_value(v: Value) -> Result<Self, JdbcError> {
        match v {
            Value::Null => Ok(None),
            other => T::from_value(other).map(Some),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Row / FromRow（对标 ResultSet 行取值 + RowMapper）
// ─────────────────────────────────────────────────────────────────────

/// 结果集中的一行：列名 + 列值。
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    columns: Vec<String>,
    values: Vec<Value>,
}

impl Row {
    pub fn new(columns: Vec<String>, values: Vec<Value>) -> Self {
        debug_assert_eq!(columns.len(), values.len());
        Self { columns, values }
    }

    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// 按列名取值（不区分大小写）。
    pub fn value_by_name(&self, name: &str) -> Result<&Value, JdbcError> {
        self.columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(name))
            .map(|i| &self.values[i])
            .ok_or_else(|| JdbcError::ColumnNotFound(name.to_string()))
    }

    /// 按列名取值并转换为目标类型（`#[derive(FromRow)]` 的落点）。
    pub fn try_get<T: FromValue>(&self, name: &str) -> Result<T, JdbcError> {
        T::from_value(self.value_by_name(name)?.clone())
    }

    /// 第一列的值（`query_scalar` 使用）。
    pub fn first_value(&self) -> Result<&Value, JdbcError> {
        self.values
            .first()
            .ok_or_else(|| JdbcError::ColumnNotFound("<first>".to_string()))
    }
}

/// 行 → 结构体映射（对标 MyBatis ResultMap / Spring RowMapper）。
/// 可用 `#[derive(oxiron::FromRow)]` 自动生成实现。
pub trait FromRow: Sized {
    fn from_row(row: &Row) -> Result<Self, JdbcError>;
}

// ─────────────────────────────────────────────────────────────────────
// JDBC 核心接口
// ─────────────────────────────────────────────────────────────────────

/// 对标 `javax.sql.DataSource`：从数据源获取连接。
pub trait DataSource {
    type Conn: Connection;
    fn get_connection(&self) -> Result<Self::Conn, JdbcError>;
}

/// 对标 `java.sql.Connection`。
pub trait Connection {
    /// 预编译 SQL（对标 `Connection#prepareStatement`）。
    fn prepare(&self, sql: &str) -> Result<Box<dyn PreparedStatement + '_>, JdbcError>;
}

/// 对标 `java.sql.PreparedStatement`。
pub trait PreparedStatement {
    /// 绑定参数，**下标从 1 开始**（与 JDBC 一致）。
    fn bind(&mut self, index: usize, value: Value) -> Result<(), JdbcError>;

    /// 执行 DML，返回受影响行数（对标 `executeUpdate`）。
    fn execute(&self) -> Result<u64, JdbcError>;

    /// 执行查询，返回结果集游标（对标 `executeQuery`）。
    fn query(&self) -> Result<Box<dyn ResultSet + '_>, JdbcError>;
}

/// 对标 `java.sql.ResultSet`。
pub trait ResultSet {
    /// 前进到下一行；无更多行时返回 `None`。
    fn next(&mut self) -> Result<Option<Row>, JdbcError>;
}

// ─────────────────────────────────────────────────────────────────────
// 映射查询便捷函数（#[sql] 宏生成代码的落点；泛化不破坏对象安全）
// ─────────────────────────────────────────────────────────────────────

/// 查询多行并映射为 `Vec<T>`（对应 `Vec<T>` 返回类型）。
pub fn query_all<S, T>(stmt: &S) -> Result<Vec<T>, JdbcError>
where
    S: PreparedStatement + ?Sized,
    T: FromRow,
{
    let mut rs = stmt.query()?;
    let mut out = Vec::new();
    while let Some(row) = rs.next()? {
        out.push(T::from_row(&row)?);
    }
    Ok(out)
}

/// 查询单行（多于一行取第一行）；无行返回 `Err(NoRows)`（对应 `T` 返回类型）。
pub fn query_one<S, T>(stmt: &S) -> Result<T, JdbcError>
where
    S: PreparedStatement + ?Sized,
    T: FromRow,
{
    query_optional(stmt)?.ok_or(JdbcError::NoRows)
}

/// 查询至多单行；无行返回 `None`（对应 `Option<T>` 返回类型）。
pub fn query_optional<S, T>(stmt: &S) -> Result<Option<T>, JdbcError>
where
    S: PreparedStatement + ?Sized,
    T: FromRow,
{
    let mut rs = stmt.query()?;
    match rs.next()? {
        Some(row) => T::from_row(&row).map(Some),
        None => Ok(None),
    }
}

/// 查询第一行第一列的标量值（对标 `SELECT COUNT(*)` 等）。
pub fn query_scalar<S, T>(stmt: &S) -> Result<T, JdbcError>
where
    S: PreparedStatement + ?Sized,
    T: FromValue,
{
    let mut rs = stmt.query()?;
    match rs.next()? {
        Some(row) => T::from_value(row.first_value()?.clone()),
        None => Err(JdbcError::NoRows),
    }
}

// ─────────────────────────────────────────────────────────────────────
// SQL 模板占位符方言（`#{name}` → 驱动方言，逻辑在核心层）
// ─────────────────────────────────────────────────────────────────────

/// 预编译语句占位符方言。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlaceholderStyle {
    /// `?`（SQLite / MySQL）
    Question,
    /// `$1, $2, ...`（PostgreSQL）
    DollarNumber,
}

/// 把 SQL 模板中的 `#{name}` 按出现顺序展开为驱动方言占位符。
///
/// 普通 SQL（不含 `#{}`）原样返回。`#{}` 只在模板层面有意义，
/// 字符串字面量内的 `#{}` 也会被展开（使用时避免在 SQL 字符串里写 `#{}`）。
pub fn expand_placeholders(sql: &str, style: PlaceholderStyle) -> Result<String, JdbcError> {
    let mut out = String::with_capacity(sql.len());
    let mut rest = sql;
    let mut n = 0usize;
    while let Some(pos) = rest.find("#{") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| JdbcError::Conversion("SQL 模板中存在未闭合的 `#{`".into()))?;
        let name = &after[..end];
        let valid = !name.is_empty()
            && name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return Err(JdbcError::Conversion(format!(
                "非法的占位符参数名 `#{{{name}}}`"
            )));
        }
        n += 1;
        match style {
            PlaceholderStyle::Question => out.push('?'),
            PlaceholderStyle::DollarNumber => out.push_str(&format!("${n}")),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────
// 错误类型
// ─────────────────────────────────────────────────────────────────────

/// oxiron-jdbc 统一错误。
#[derive(Debug)]
pub enum JdbcError {
    /// 驱动/连接错误。
    Driver(String),
    /// SQL 执行错误。
    Sql(String),
    /// 列不存在。
    ColumnNotFound(String),
    /// 列值类型不匹配。
    TypeMismatch {
        expected: &'static str,
        got: Value,
    },
    /// 期望有行但结果集为空。
    NoRows,
    /// 其他转换错误。
    Conversion(String),
}

impl fmt::Display for JdbcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JdbcError::Driver(m) => write!(f, "数据库驱动错误: {m}"),
            JdbcError::Sql(m) => write!(f, "SQL 执行错误: {m}"),
            JdbcError::ColumnNotFound(c) => write!(f, "列不存在: {c}"),
            JdbcError::TypeMismatch { expected, got } => {
                write!(f, "列值类型不匹配: 期望 {expected}，实际 {got:?}")
            }
            JdbcError::NoRows => write!(f, "期望单行结果，但结果集为空"),
            JdbcError::Conversion(m) => write!(f, "类型转换错误: {m}"),
        }
    }
}

impl std::error::Error for JdbcError {}
