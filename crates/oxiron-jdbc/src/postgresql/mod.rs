//! PostgreSQL 驱动（对标 `postgresql-jdbc`）：实现 JDBC 核心 trait。
//!
//! 纯 Rust 自研驱动——直接实现 PostgreSQL 前端/后端协议 v3（见 [`protocol`]），
//! 认证支持 trust / cleartext password / md5 / **SCRAM-SHA-256**。
//!
//! 连接串示例：`postgresql://user:pass@localhost:5432/mydb`（也接受 `postgresql://`）。
//! 已知限制：暂不支持 SSL/TLS 连接与 GSS/SSPI 认证。

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::{
    Connection, DataSource, JdbcError, PlaceholderStyle, PreparedStatement, Row, ResultSet, Value,
};

mod protocol;
mod scram;

use protocol::{affected_rows, hex_decode, hex_lower, Backend, Column, ValueBytes};

/// 连接超时（秒）。
const CONNECT_TIMEOUT_SECS: u64 = 10;
/// 单次读写超时（秒）。
const IO_TIMEOUT_SECS: u64 = 30;

/// PostgreSQL 数据源。
pub struct PgDataSource {
    url: String,
}

impl PgDataSource {
    /// 打开数据源。`url` 形如 `postgresql://user:pass@host:port/db`。
    pub fn open(url: &str) -> Result<Self, JdbcError> {
        parse_url(url)?; // 提前校验
        Ok(Self {
            url: url.to_string(),
        })
    }
}

impl DataSource for PgDataSource {
    type Conn = PgConnection;

    fn get_connection(&self) -> Result<PgConnection, JdbcError> {
        PgConnection::connect_url(&self.url)
    }
}

/// PostgreSQL 连接。
pub struct PgConnection {
    stream: TcpStream,
    next_stmt_id: AtomicU64,
}

impl PgConnection {
    /// 用连接串建立连接（Startup + 认证，直到 ReadyForQuery）。
    pub fn connect_url(url: &str) -> Result<Self, JdbcError> {
        let (host, port, user, password, db) = parse_url(url)?;
        Self::connect(&host, port, &user, password.as_deref(), &db)
    }

    /// 建立连接。
    pub fn connect(
        host: &str,
        port: u16,
        user: &str,
        password: Option<&str>,
        database: &str,
    ) -> Result<Self, JdbcError> {
        let addr = format!("{host}:{port}")
            .to_socket_addrs()
            .map_err(|e| JdbcError::Driver(format!("无法解析主机 {host}: {e}")))?
            .next()
            .ok_or_else(|| JdbcError::Driver(format!("无法解析主机 {host}")))?;

        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(CONNECT_TIMEOUT_SECS))
            .map_err(|e| JdbcError::Driver(format!("无法连接 {host}:{port}: {e}")))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(IO_TIMEOUT_SECS)))
            .ok();
        stream
            .set_write_timeout(Some(Duration::from_secs(IO_TIMEOUT_SECS)))
            .ok();
        stream.set_nodelay(true).ok();

        protocol::connect(&stream, user, database, password)?;
        Ok(Self {
            stream,
            next_stmt_id: AtomicU64::new(1),
        })
    }
}

impl Connection for PgConnection {
    fn prepare(&self, sql: &str) -> Result<Box<dyn PreparedStatement + '_>, JdbcError> {
        // 模板占位符 `#{name}` → `$1..$n`（方言展开在驱动层完成）
        let sql = crate::expand_placeholders(sql, PlaceholderStyle::DollarNumber)?;
        let name = format!(
            "oxiron_{}",
            self.next_stmt_id.fetch_add(1, Ordering::Relaxed)
        );

        let mut batch = Vec::new();
        batch.extend_from_slice(&protocol::msg_parse(&name, &sql));
        batch.extend_from_slice(&protocol::msg_describe_stmt(&name));
        batch.extend_from_slice(&protocol::msg_sync());
        protocol::write_raw(&self.stream, &batch)?;

        let mut param_count = 0usize;
        let mut columns: Vec<Column> = Vec::new();
        for msg in protocol::read_until_ready(&self.stream)? {
            match msg {
                Backend::ParameterDescription(oids) => param_count = oids.len(),
                Backend::RowDescription(cols) => columns = cols,
                Backend::NoData | Backend::ParseComplete => {}
                other => {
                    return Err(JdbcError::Sql(format!(
                        "prepare 收到意外消息: {other:?}"
                    )))
                }
            }
        }

        Ok(Box::new(PgStatement {
            conn: self,
            name,
            param_count,
            columns,
            params: vec![None; param_count],
        }))
    }
}

/// PostgreSQL 预编译语句（命名语句，支持同一连接上的多语句并存）。
pub struct PgStatement<'c> {
    conn: &'c PgConnection,
    name: String,
    param_count: usize,
    columns: Vec<Column>,
    params: Vec<Option<Value>>,
}

impl PgStatement<'_> {
    /// 执行 Bind + Execute + Sync 并收集响应。
    fn run(&self) -> Result<(u64, Vec<Vec<Value>>), JdbcError> {
        let mut batch = Vec::new();
        batch.extend_from_slice(&protocol::msg_bind(
            &self.name,
            &build_bind_params(&self.params),
        ));
        batch.extend_from_slice(&protocol::msg_execute());
        batch.extend_from_slice(&protocol::msg_sync());
        protocol::write_raw(&self.conn.stream, &batch)?;

        let mut rows: Vec<Vec<Value>> = Vec::new();
        let mut affected = 0u64;
        for msg in protocol::read_until_ready(&self.conn.stream)? {
            match msg {
                Backend::DataRow(raws) => {
                    let mut vals = Vec::with_capacity(raws.len());
                    for (i, raw) in raws.into_iter().enumerate() {
                        let oid = self.columns.get(i).map(|c| c.type_oid).unwrap_or(0);
                        vals.push(decode_column(oid, raw));
                    }
                    rows.push(vals);
                }
                Backend::CommandComplete(tag) => affected = affected_rows(&tag),
                Backend::BindComplete
                | Backend::EmptyQuery
                | Backend::PortalSuspended
                | Backend::ReadyForQuery(_) => {}
                Backend::NoData => {}
                other => {
                    return Err(JdbcError::Sql(format!(
                        "execute 收到意外消息: {other:?}"
                    )))
                }
            }
        }
        Ok((affected, rows))
    }
}

impl PreparedStatement for PgStatement<'_> {
    fn bind(&mut self, index: usize, value: Value) -> Result<(), JdbcError> {
        if index == 0 || index > self.param_count {
            return Err(JdbcError::Conversion(format!(
                "绑定下标 {index} 越界（参数个数 {}）",
                self.param_count
            )));
        }
        self.params[index - 1] = Some(value);
        Ok(())
    }

    fn execute(&self) -> Result<u64, JdbcError> {
        Ok(self.run()?.0)
    }

    fn query(&self) -> Result<Box<dyn ResultSet + '_>, JdbcError> {
        let (_, raw_rows) = self.run()?;
        let rows = raw_rows
            .into_iter()
            .map(|vals| {
                let columns: Vec<String> = self.columns.iter().map(|c| c.name.clone()).collect();
                Row::new(columns, vals)
            })
            .collect::<Vec<_>>()
        .into_iter();
        Ok(Box::new(PgResultSet { rows }))
    }
}

impl Drop for PgStatement<'_> {
    fn drop(&mut self) {
        // 释放服务器端命名语句（尽力而为）
        let mut batch = Vec::new();
        batch.extend_from_slice(&protocol::msg_close_stmt(&self.name));
        batch.extend_from_slice(&protocol::msg_sync());
        if protocol::write_raw(&self.conn.stream, &batch).is_ok() {
            let _ = protocol::read_until_ready(&self.conn.stream);
        }
    }
}

/// PostgreSQL 结果集（已缓冲）。
pub struct PgResultSet {
    rows: std::vec::IntoIter<Row>,
}

impl ResultSet for PgResultSet {
    fn next(&mut self) -> Result<Option<Row>, JdbcError> {
        Ok(self.rows.next())
    }
}

// ─────────────────────────────────────────────────────────────────────
// 值编解码（文本格式，按类型 OID 解释）
// ─────────────────────────────────────────────────────────────────────

fn build_bind_params(params: &[Option<Value>]) -> Vec<Option<ValueBytes>> {
    params
        .iter()
        .map(|p| match p {
            None | Some(Value::Null) => Some(ValueBytes::Null),
            Some(v) => Some(match encode_param(v) {
                Some(bytes) => ValueBytes::Text(bytes),
                None => ValueBytes::Null,
            }),
        })
        .collect()
}

/// 参数 → 文本格式字节。
fn encode_param(v: &Value) -> Option<Vec<u8>> {
    match v {
        Value::Null => None,
        Value::Bool(b) => Some(if *b { b"t".to_vec() } else { b"f".to_vec() }),
        Value::Int(n) => Some(n.to_string().into_bytes()),
        Value::Real(f) => Some(f.to_string().into_bytes()),
        Value::Text(s) => Some(s.as_bytes().to_vec()),
        Value::Blob(x) => Some(format!("\\x{}", hex_lower(x)).into_bytes()),
    }
}

/// 文本格式列值 + 类型 OID → [`Value`]。
fn decode_column(type_oid: u32, raw: Option<Vec<u8>>) -> Value {
    let bytes = match raw {
        None => return Value::Null,
        Some(b) => b,
    };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    match type_oid {
        16 => Value::Bool(text == "t"),                     // BOOL
        17 => hex_decode(&text).map(Value::Blob).unwrap_or(Value::Text(text)), // BYTEA
        20 | 21 | 23 => text.parse::<i64>().map(Value::Int).unwrap_or(Value::Text(text)), // INT8/2/4
        700 | 701 | 1700 => text
            .parse::<f64>()
            .map(Value::Real)
            .unwrap_or(Value::Text(text)), // FLOAT4/8、NUMERIC(有损)
        _ => Value::Text(text), // TEXT/VARCHAR/时间类型/JSON 等一律按文本
    }
}

// ─────────────────────────────────────────────────────────────────────
// 连接串解析
// ─────────────────────────────────────────────────────────────────────

/// 解析 `postgresql://user:pass@host:port/db` → (host, port, user, password, db)。
fn parse_url(url: &str) -> Result<(String, u16, String, Option<String>, String), JdbcError> {
    let rest = url
        .strip_prefix("postgresql://")
        .or_else(|| url.strip_prefix("postgresql://"))
        .ok_or_else(|| {
            JdbcError::Conversion("PG 连接串需以 postgresql:// 或 postgresql:// 开头".into())
        })?;

    let (auth_host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, ""),
    };
    let (userinfo, hostport) = match auth_host.rfind('@') {
        Some(i) => (Some(&auth_host[..i]), &auth_host[i + 1..]),
        None => (None, auth_host),
    };
    let (user, password) = match userinfo {
        Some(ui) => match ui.find(':') {
            Some(i) => (pct_decode(&ui[..i])?, Some(pct_decode(&ui[i + 1..])?)),
            None => (pct_decode(ui)?, None),
        },
        None => ("postgresql".to_string(), None),
    };
    let (host, port) = match hostport.rfind(':') {
        Some(i) => {
            let p: u16 = hostport[i + 1..]
                .parse()
                .map_err(|_| JdbcError::Conversion(format!("非法端口: {}", &hostport[i + 1..])))?;
            (hostport[..i].to_string(), p)
        }
        None => (hostport.to_string(), 5432),
    };
    if host.is_empty() {
        return Err(JdbcError::Conversion("连接串缺少主机名".into()));
    }
    let db = if path.is_empty() {
        user.clone()
    } else {
        pct_decode(path)?
    };
    Ok((host, port, user, password, db))
}

fn pct_decode(s: &str) -> Result<String, JdbcError> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .map_err(|_| JdbcError::Conversion(format!("非法百分号转义: {s}")))?;
            out.push(
                u8::from_str_radix(hex, 16)
                    .map_err(|_| JdbcError::Conversion(format!("非法百分号转义: {s}")))?,
            );
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| JdbcError::Conversion(format!("非法 UTF-8: {s}")))
}
