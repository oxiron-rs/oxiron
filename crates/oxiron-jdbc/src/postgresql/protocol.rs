//! PostgreSQL 前端/后端协议 v3 实现（同步、纯 Rust）。
//!
//! 参考：PostgreSQL 官方文档 Chapter 55 "Frontend/Backend Protocol"。
//! 消息帧：`类型字节(u8) + 长度(i32, 含自身不含类型字节) + 负载`。

use std::io::{Read, Write};
use std::net::TcpStream;

use crate::JdbcError;

use super::scram::ScramSha256;

/// 协议版本 3.0。
const PROTOCOL_3_0: i32 = 196608;

/// RowDescription 中的一列。
#[derive(Debug, Clone)]
pub(crate) struct Column {
    pub name: String,
    pub type_oid: u32,
}

/// 已解析的后端消息（无关消息已在读取时跳过）。
#[derive(Debug)]
pub(crate) enum Backend {
    ParseComplete,
    BindComplete,
    NoData,
    ParameterDescription(Vec<u32>),
    RowDescription(Vec<Column>),
    DataRow(Vec<Option<Vec<u8>>>),
    CommandComplete(String),
    EmptyQuery,
    PortalSuspended,
    /// ReadyForQuery；字段为事务状态（'I'/'T'/'E'），暂未使用。
    ReadyForQuery(#[allow(dead_code)] u8),
}

// ─────────────────────────────────────────────────────────────────────
// 连接握手 + 认证
// ─────────────────────────────────────────────────────────────────────

/// 启动连接：StartupMessage + 认证，直到 ReadyForQuery。
pub(crate) fn connect(
    stream: &TcpStream,
    user: &str,
    database: &str,
    password: Option<&str>,
) -> Result<(), JdbcError> {
    send_startup(stream, user, database)?;

    let mut scram: Option<ScramSha256> = None;
    loop {
        let (tag, payload) = read_message(stream)?;
        match tag {
            b'R' => {
                let code = get_i32(&payload, 0)?;
                match code {
                    0 => {} // AuthenticationOk
                    3 => {
                        // CleartextPassword
                        let pw = require_password(password)?;
                        write_message(stream, b'p', &cstring(pw))?;
                    }
                    5 => {
                        // MD5Password
                        let pw = require_password(password)?;
                        let salt: [u8; 4] = payload
                            .get(4..8)
                            .and_then(|s| s.try_into().ok())
                            .ok_or_else(|| JdbcError::Driver("MD5 认证缺少 salt".into()))?;
                        let resp = md5_response(pw, user, &salt);
                        write_message(stream, b'p', &cstring(&resp))?;
                    }
                    10 => {
                        // SASL：选择 SCRAM-SHA-256
                        let mechs = parse_cstring_list(&payload[4..]);
                        if !mechs.iter().any(|m| m == "SCRAM-SHA-256") {
                            return Err(JdbcError::Driver(format!(
                                "服务器不支持 SCRAM-SHA-256（可用: {mechs:?}）"
                            )));
                        }
                        let pw = require_password(password)?;
                        let s = ScramSha256::new(pw);
                        let first = s.client_first_message();
                        let mut body = Vec::new();
                        body.extend_from_slice(b"SCRAM-SHA-256\0");
                        body.extend_from_slice(&(first.len() as i32).to_be_bytes());
                        body.extend_from_slice(first.as_bytes());
                        write_message(stream, b'p', &body)?;
                        scram = Some(s);
                    }
                    11 => {
                        // SASLContinue
                        let s = scram
                            .as_mut()
                            .ok_or_else(|| JdbcError::Driver("SCRAM 状态错误（Continue）".into()))?;
                        let server_first = String::from_utf8_lossy(&payload[4..]).into_owned();
                        let final_msg = s.handle_server_first(&server_first)?;
                        write_message(stream, b'p', final_msg.as_bytes())?;
                    }
                    12 => {
                        // SASLFinal
                        let s = scram
                            .as_ref()
                            .ok_or_else(|| JdbcError::Driver("SCRAM 状态错误（Final）".into()))?;
                        let server_final = String::from_utf8_lossy(&payload[4..]).into_owned();
                        s.verify_server_final(&server_final)?;
                    }
                    other => {
                        return Err(JdbcError::Driver(format!(
                            "暂不支持的认证方式（code={other}），支持 trust/password/md5/SCRAM-SHA-256"
                        )))
                    }
                }
            }
            b'S' | b'K' | b'N' => {} // ParameterStatus / BackendKeyData / Notice
            b'E' => return Err(error_from_payload(&payload)),
            b'Z' => return Ok(()),
            other => {
                return Err(JdbcError::Driver(format!(
                    "连接握手收到未知消息: {}",
                    other as char
                )))
            }
        }
    }
}

fn require_password(password: Option<&str>) -> Result<&str, JdbcError> {
    password.ok_or_else(|| JdbcError::Driver("服务器要求密码，但连接串未提供".into()))
}

fn send_startup(stream: &TcpStream, user: &str, database: &str) -> Result<(), JdbcError> {
    let mut body = Vec::new();
    body.extend_from_slice(&PROTOCOL_3_0.to_be_bytes());
    for (k, v) in [
        ("user", user),
        ("database", database),
        ("client_encoding", "UTF8"),
    ] {
        body.extend_from_slice(k.as_bytes());
        body.push(0);
        body.extend_from_slice(v.as_bytes());
        body.push(0);
    }
    body.push(0);
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    frame.extend_from_slice(&body);
    write_raw(stream, &frame)
}

// ─────────────────────────────────────────────────────────────────────
// 认证辅助
// ─────────────────────────────────────────────────────────────────────

/// PG md5 认证响应："md5" + md5hex(md5hex(password+user) + salt)。
fn md5_response(password: &str, user: &str, salt: &[u8]) -> String {
    use md5::{Digest, Md5};

    let mut h1 = Md5::new();
    h1.update(password.as_bytes());
    h1.update(user.as_bytes());
    let inner = hex_lower(&h1.finalize());

    let mut h2 = Md5::new();
    h2.update(inner.as_bytes());
    h2.update(salt);
    format!("md5{}", hex_lower(&h2.finalize()))
}

pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 十六进制解码（bytea 文本格式 `\x...` 用）。
pub(crate) fn hex_decode(s: &str) -> Result<Vec<u8>, JdbcError> {
    let s = s.trim_start_matches("\\x");
    if s.len() % 2 != 0 {
        return Err(JdbcError::Conversion(format!("非法十六进制: {s}")));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|_| JdbcError::Conversion(format!("非法十六进制: {s}")))
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────
// 消息收发
// ─────────────────────────────────────────────────────────────────────

pub(crate) fn read_message(stream: &TcpStream) -> Result<(u8, Vec<u8>), JdbcError> {
    let mut header = [0u8; 5];
    read_exact(stream, &mut header)?;
    let tag = header[0];
    let len = i32::from_be_bytes(header[1..5].try_into().expect("4 字节"));
    if len < 4 {
        return Err(JdbcError::Driver(format!("非法消息长度: {len}")));
    }
    let mut body = vec![0u8; (len - 4) as usize];
    read_exact(stream, &mut body)?;
    Ok((tag, body))
}

fn read_exact(mut stream: &TcpStream, buf: &mut [u8]) -> Result<(), JdbcError> {
    let mut got = 0;
    while got < buf.len() {
        match stream.read(&mut buf[got..]) {
            Ok(0) => return Err(JdbcError::Driver("连接被服务器关闭".into())),
            Ok(n) => got += n,
            Err(e) => return Err(JdbcError::Driver(format!("读取失败: {e}"))),
        }
    }
    Ok(())
}

pub(crate) fn write_raw(stream: &TcpStream, bytes: &[u8]) -> Result<(), JdbcError> {
    let mut s = stream;
    s.write_all(bytes)
        .map_err(|e| JdbcError::Driver(format!("写入失败: {e}")))?;
    s.flush()
        .map_err(|e| JdbcError::Driver(format!("写入失败: {e}")))
}

pub(crate) fn write_message(stream: &TcpStream, tag: u8, body: &[u8]) -> Result<(), JdbcError> {
    write_raw(stream, &frame(tag, body))
}

/// 读取后端消息直到 ReadyForQuery（错误消息转为 Err）。
pub(crate) fn read_until_ready(stream: &TcpStream) -> Result<Vec<Backend>, JdbcError> {
    let mut out = Vec::new();
    loop {
        let msg = read_backend(stream)?;
        let done = matches!(msg, Backend::ReadyForQuery(_));
        out.push(msg);
        if done {
            return Ok(out);
        }
    }
}

fn read_backend(stream: &TcpStream) -> Result<Backend, JdbcError> {
    loop {
        let (tag, payload) = read_message(stream)?;
        return Ok(match tag {
            b'1' => Backend::ParseComplete,
            b'2' => Backend::BindComplete,
            b'n' => Backend::NoData,
            b's' => Backend::PortalSuspended,
            b'I' => Backend::EmptyQuery,
            b't' => Backend::ParameterDescription(parse_param_desc(&payload)?),
            b'T' => Backend::RowDescription(parse_row_description(&payload)?),
            b'D' => Backend::DataRow(parse_data_row(&payload)?),
            b'C' => Backend::CommandComplete(parse_cstr(&payload, 0)?.0),
            b'Z' => Backend::ReadyForQuery(payload.first().copied().unwrap_or(b'?')),
            b'E' => return Err(error_from_payload(&payload)),
            b'S' | b'K' | b'N' => continue, // ParameterStatus / BackendKeyData / Notice
            other => {
                return Err(JdbcError::Driver(format!(
                    "未知的后端消息类型: {}",
                    other as char
                )))
            }
        });
    }
}

// ─────────────────────────────────────────────────────────────────────
// 前端消息构造（扩展查询协议：Parse / Bind / Describe / Execute / Sync / Close）
// ─────────────────────────────────────────────────────────────────────

pub(crate) fn msg_parse(name: &str, sql: &str) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&cstring(name));
    body.extend_from_slice(&cstring(sql));
    body.extend_from_slice(&0i16.to_be_bytes()); // 不显式声明参数类型
    frame(b'P', &body)
}

pub(crate) fn msg_describe_stmt(name: &str) -> Vec<u8> {
    let mut body = Vec::new();
    body.push(b'S');
    body.extend_from_slice(&cstring(name));
    frame(b'D', &body)
}

pub(crate) fn msg_bind(stmt_name: &str, params: &[Option<ValueBytes>]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&cstring("")); // 未命名 portal
    body.extend_from_slice(&cstring(stmt_name));
    body.extend_from_slice(&0i16.to_be_bytes()); // 参数格式：全部文本
    body.extend_from_slice(&(params.len() as i16).to_be_bytes());
    for p in params {
        match p {
            None | Some(ValueBytes::Null) => body.extend_from_slice(&(-1i32).to_be_bytes()),
            Some(ValueBytes::Text(bytes)) => {
                body.extend_from_slice(&(bytes.len() as i32).to_be_bytes());
                body.extend_from_slice(bytes);
            }
        }
    }
    body.extend_from_slice(&0i16.to_be_bytes()); // 结果格式：全部文本
    frame(b'B', &body)
}

pub(crate) fn msg_execute() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&cstring("")); // 未命名 portal
    body.extend_from_slice(&0i32.to_be_bytes()); // 不限行数
    frame(b'E', &body)
}

pub(crate) fn msg_sync() -> Vec<u8> {
    frame(b'S', &[])
}

pub(crate) fn msg_close_stmt(name: &str) -> Vec<u8> {
    let mut body = Vec::new();
    body.push(b'S');
    body.extend_from_slice(&cstring(name));
    frame(b'C', &body)
}

/// 绑定参数的文本格式表示。
pub(crate) enum ValueBytes {
    Null,
    Text(Vec<u8>),
}

// ─────────────────────────────────────────────────────────────────────
// 负载解析
// ─────────────────────────────────────────────────────────────────────

fn parse_param_desc(payload: &[u8]) -> Result<Vec<u32>, JdbcError> {
    let count = get_i16(payload, 0)? as usize;
    let mut oids = Vec::with_capacity(count);
    for i in 0..count {
        oids.push(get_u32(payload, 2 + i * 4)?);
    }
    Ok(oids)
}

fn parse_row_description(payload: &[u8]) -> Result<Vec<Column>, JdbcError> {
    let count = get_i16(payload, 0)? as usize;
    let mut i = 2;
    let mut cols = Vec::with_capacity(count);
    for _ in 0..count {
        let (name, next) = parse_cstr(payload, i)?;
        i = next;
        i += 4 + 2; // table_oid(i32) + attnum(i16)
        let type_oid = get_u32(payload, i)?;
        i += 4 + 2 + 4 + 2; // type_oid 已读 + typlen(i16) + typmod(i32) + fmt(i16)
        cols.push(Column { name, type_oid });
    }
    Ok(cols)
}

fn parse_data_row(payload: &[u8]) -> Result<Vec<Option<Vec<u8>>>, JdbcError> {
    let count = get_i16(payload, 0)? as usize;
    let mut i = 2;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        let len = get_i32(payload, i)?;
        i += 4;
        if len < 0 {
            values.push(None);
        } else {
            let end = i + len as usize;
            values.push(Some(payload[i..end].to_vec()));
            i = end;
        }
    }
    Ok(values)
}

/// 错误消息负载：`(字段码 u8, C 字符串)*` + `\0`。
fn error_from_payload(payload: &[u8]) -> JdbcError {
    let mut msg = String::new();
    let mut code = String::new();
    let mut i = 0;
    while i < payload.len() {
        let field = payload[i];
        i += 1;
        if field == 0 {
            break;
        }
        let (val, next) = parse_cstr(payload, i).unwrap_or((String::new(), payload.len()));
        match field {
            b'M' => msg = val,
            b'C' => code = val,
            _ => {}
        }
        i = next;
    }
    if msg.is_empty() {
        msg = "未知 PostgreSQL 错误".into();
    }
    if !code.is_empty() {
        msg = format!("[SQLSTATE {code}] {msg}");
    }
    JdbcError::Sql(msg)
}

/// 从命令完成标签解析受影响行数（"INSERT 0 3"→3、"UPDATE 2"→2、"CREATE TABLE"→0）。
pub(crate) fn affected_rows(tag: &str) -> u64 {
    tag.split_whitespace()
        .last()
        .and_then(|t| t.parse().ok())
        .unwrap_or(0)
}

// ─────────────────────────────────────────────────────────────────────
// 字节工具
// ─────────────────────────────────────────────────────────────────────

pub(crate) fn cstring(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

fn parse_cstr(payload: &[u8], start: usize) -> Result<(String, usize), JdbcError> {
    let end = payload[start..]
        .iter()
        .position(|&b| b == 0)
        .map(|p| start + p)
        .ok_or_else(|| JdbcError::Driver("消息中缺少 NUL 终止的字符串".into()))?;
    let s = String::from_utf8_lossy(&payload[start..end]).into_owned();
    Ok((s, end + 1))
}

fn parse_cstring_list(payload: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < payload.len() {
        if payload[i] == 0 {
            break;
        }
        if let Ok((s, next)) = parse_cstr(payload, i) {
            out.push(s);
            i = next;
        } else {
            break;
        }
    }
    out
}

fn frame(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(5 + body.len());
    v.push(tag);
    v.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    v.extend_from_slice(body);
    v
}

fn get_i16(p: &[u8], i: usize) -> Result<i16, JdbcError> {
    let b = p
        .get(i..i + 2)
        .ok_or_else(|| JdbcError::Driver("消息负载过短".into()))?;
    Ok(i16::from_be_bytes(b.try_into().expect("2 字节")))
}

fn get_i32(p: &[u8], i: usize) -> Result<i32, JdbcError> {
    let b = p
        .get(i..i + 4)
        .ok_or_else(|| JdbcError::Driver("消息负载过短".into()))?;
    Ok(i32::from_be_bytes(b.try_into().expect("4 字节")))
}

fn get_u32(p: &[u8], i: usize) -> Result<u32, JdbcError> {
    Ok(get_i32(p, i)? as u32)
}
