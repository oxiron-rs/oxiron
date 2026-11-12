//! SQLite 驱动（对标 `sqlite-jdbc`）：用 `libsqlite3-sys` 实现 JDBC 核心 trait。
//!
//! 这是 oxiron-jdbc 的第一个驱动实现；未来 MySQL/PostgreSQL 驱动
//! 只需同样实现 [`crate::Connection`] / [`crate::PreparedStatement`] / [`crate::ResultSet`]。

use std::ffi::{c_char, c_void, CStr, CString};
use std::marker::PhantomData;

use libsqlite3_sys as ffi;

use crate::{Connection, DataSource, JdbcError, PreparedStatement, Row, ResultSet, Value};

/// SQLite 数据源（对标 JDBC `DataSource`）。连接串支持
/// `"sqlite::memory:"`（内存库）或文件路径。
pub struct SqliteDataSource {
    url: String,
}

impl SqliteDataSource {
    pub fn open(url: &str) -> Result<Self, JdbcError> {
        Ok(Self {
            url: url.to_string(),
        })
    }
}

impl DataSource for SqliteDataSource {
    type Conn = SqliteConnection;

    fn get_connection(&self) -> Result<SqliteConnection, JdbcError> {
        SqliteConnection::open(&self.url)
    }
}

/// SQLite 连接。
pub struct SqliteConnection {
    raw: *mut ffi::sqlite3,
}

impl SqliteConnection {
    /// 打开连接。`url` 可以是 `"sqlite::memory:"`、`":memory:"` 或文件路径。
    pub fn open(url: &str) -> Result<Self, JdbcError> {
        let path = url.strip_prefix("sqlite:").unwrap_or(url);
        let c_path = to_cstring(path)?;
        let mut raw: *mut ffi::sqlite3 = std::ptr::null_mut();
        let rc = unsafe {
            ffi::sqlite3_open_v2(
                c_path.as_ptr(),
                &mut raw,
                ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE | ffi::SQLITE_OPEN_URI,
                std::ptr::null(),
            )
        };
        if rc != ffi::SQLITE_OK {
            let msg = db_errmsg(raw);
            if !raw.is_null() {
                unsafe { ffi::sqlite3_close(raw) };
            }
            return Err(JdbcError::Driver(msg));
        }
        Ok(Self { raw })
    }
}

impl Connection for SqliteConnection {
    fn prepare(&self, sql: &str) -> Result<Box<dyn PreparedStatement + '_>, JdbcError> {
        let c_sql = to_cstring(sql)?;
        let mut stmt: *mut ffi::sqlite3_stmt = std::ptr::null_mut();
        let rc = unsafe {
            ffi::sqlite3_prepare_v2(self.raw, c_sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut())
        };
        if rc != ffi::SQLITE_OK {
            return Err(JdbcError::Sql(db_errmsg(self.raw)));
        }
        Ok(Box::new(SqliteStatement {
            raw: stmt,
            _conn: PhantomData,
        }))
    }
}

impl Drop for SqliteConnection {
    fn drop(&mut self) {
        unsafe { ffi::sqlite3_close(self.raw) };
    }
}

/// SQLite 预编译语句。
pub struct SqliteStatement<'c> {
    raw: *mut ffi::sqlite3_stmt,
    _conn: PhantomData<&'c SqliteConnection>,
}

impl PreparedStatement for SqliteStatement<'_> {
    fn bind(&mut self, index: usize, value: Value) -> Result<(), JdbcError> {
        // SQLITE_TRANSIENT 让 SQLite 自行拷贝绑定缓冲区，避免生命周期问题
        let transient: ffi::sqlite3_destructor_type = unsafe { std::mem::transmute(-1isize) };
        let idx = index as std::ffi::c_int;
        let rc = unsafe {
            match &value {
                Value::Null => ffi::sqlite3_bind_null(self.raw, idx),
                Value::Bool(b) => ffi::sqlite3_bind_int64(self.raw, idx, *b as i64),
                Value::Int(n) => ffi::sqlite3_bind_int64(self.raw, idx, *n),
                Value::Real(f) => ffi::sqlite3_bind_double(self.raw, idx, *f),
                Value::Text(s) => ffi::sqlite3_bind_text(
                    self.raw,
                    idx,
                    s.as_ptr() as *const c_char,
                    s.len() as std::ffi::c_int,
                    transient,
                ),
                Value::Blob(b) => ffi::sqlite3_bind_blob(
                    self.raw,
                    idx,
                    b.as_ptr() as *const c_void,
                    b.len() as std::ffi::c_int,
                    transient,
                ),
            }
        };
        if rc != ffi::SQLITE_OK {
            return Err(JdbcError::Sql(db_errmsg(unsafe {
                ffi::sqlite3_db_handle(self.raw)
            })));
        }
        Ok(())
    }

    fn execute(&self) -> Result<u64, JdbcError> {
        let rc = unsafe { ffi::sqlite3_step(self.raw) };
        if rc != ffi::SQLITE_DONE && rc != ffi::SQLITE_ROW {
            let msg = db_errmsg(unsafe { ffi::sqlite3_db_handle(self.raw) });
            unsafe { ffi::sqlite3_reset(self.raw) };
            return Err(JdbcError::Sql(msg));
        }
        let affected = unsafe { ffi::sqlite3_changes(ffi::sqlite3_db_handle(self.raw)) } as u64;
        unsafe { ffi::sqlite3_reset(self.raw) };
        Ok(affected)
    }

    fn query(&self) -> Result<Box<dyn ResultSet + '_>, JdbcError> {
        Ok(Box::new(SqliteResultSet {
            stmt: self.raw,
            done: false,
            _marker: PhantomData,
        }))
    }
}

impl Drop for SqliteStatement<'_> {
    fn drop(&mut self) {
        unsafe { ffi::sqlite3_finalize(self.raw) };
    }
}

/// SQLite 结果集游标。
pub struct SqliteResultSet<'s> {
    stmt: *mut ffi::sqlite3_stmt,
    done: bool,
    _marker: PhantomData<&'s ()>,
}

impl ResultSet for SqliteResultSet<'_> {
    fn next(&mut self) -> Result<Option<Row>, JdbcError> {
        if self.done {
            return Ok(None);
        }
        let rc = unsafe { ffi::sqlite3_step(self.stmt) };
        match rc {
            ffi::SQLITE_ROW => Ok(Some(read_row(self.stmt)?)),
            ffi::SQLITE_DONE => {
                self.done = true;
                Ok(None)
            }
            _ => {
                self.done = true;
                Err(JdbcError::Sql(db_errmsg(unsafe {
                    ffi::sqlite3_db_handle(self.stmt)
                })))
            }
        }
    }
}

/// 读取当前行的全部列（列名 + 值）。
fn read_row(stmt: *mut ffi::sqlite3_stmt) -> Result<Row, JdbcError> {
    let n = unsafe { ffi::sqlite3_column_count(stmt) } as usize;
    let mut columns = Vec::with_capacity(n);
    let mut values = Vec::with_capacity(n);
    for i in 0..n {
        let col = i as std::ffi::c_int;
        let name = unsafe {
            let p = ffi::sqlite3_column_name(stmt, col);
            if p.is_null() {
                format!("col_{i}")
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        let value = unsafe {
            match ffi::sqlite3_column_type(stmt, col) {
                ffi::SQLITE_NULL => Value::Null,
                ffi::SQLITE_INTEGER => Value::Int(ffi::sqlite3_column_int64(stmt, col)),
                ffi::SQLITE_FLOAT => Value::Real(ffi::sqlite3_column_double(stmt, col)),
                ffi::SQLITE_TEXT => {
                    let p = ffi::sqlite3_column_text(stmt, col);
                    let len = ffi::sqlite3_column_bytes(stmt, col) as usize;
                    Value::Text(String::from_utf8_lossy(std::slice::from_raw_parts(p, len)).into_owned())
                }
                ffi::SQLITE_BLOB => {
                    let p = ffi::sqlite3_column_blob(stmt, col);
                    let len = ffi::sqlite3_column_bytes(stmt, col) as usize;
                    Value::Blob(std::slice::from_raw_parts(p as *const u8, len).to_vec())
                }
                _ => Value::Null,
            }
        };
        columns.push(name);
        values.push(value);
    }
    Ok(Row::new(columns, values))
}

fn to_cstring(s: &str) -> Result<CString, JdbcError> {
    CString::new(s).map_err(|e| JdbcError::Conversion(format!("字符串包含非法 NUL 字节: {e}")))
}

fn db_errmsg(db: *mut ffi::sqlite3) -> String {
    unsafe {
        if db.is_null() {
            "未知 SQLite 错误".to_string()
        } else {
            CStr::from_ptr(ffi::sqlite3_errmsg(db))
                .to_string_lossy()
                .into_owned()
        }
    }
}
