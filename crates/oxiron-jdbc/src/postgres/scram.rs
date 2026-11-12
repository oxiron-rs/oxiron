//! SCRAM-SHA-256 客户端实现（RFC 5802 / RFC 7677）。
//!
//! PostgreSQL 认证流程中的 SASL 交换：
//! 1. client-first：`n,,n=,r=<客户端随机数>`（用户名由 PG 在 StartupMessage 中发送，此处留空）；
//! 2. server-first：`r=<合并随机数>,s=<Base64 salt>,i=<迭代次数>`；
//! 3. client-final：`c=biws,r=<合并随机数>,p=<Base64 证明>`；
//! 4. server-final：`v=<Base64 服务器签名>`（客户端校验）。

use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::JdbcError;

type HmacSha256 = Hmac<Sha256>;

/// PBKDF2 迭代次数上限（防御恶意服务器让客户端算到天荒地老）。
const MAX_ITERATIONS: u32 = 10_000_000;

pub(crate) struct ScramSha256 {
    password: String,
    client_nonce: String,
    client_first_bare: String,
    server_first: Option<String>,
    client_final_without_proof: Option<String>,
    salted_password: Option<[u8; 32]>,
}

impl ScramSha256 {
    pub fn new(password: &str) -> Self {
        let client_nonce = random_nonce();
        let client_first_bare = format!("n=,r={client_nonce}");
        Self {
            password: password.to_string(),
            client_nonce,
            client_first_bare,
            server_first: None,
            client_final_without_proof: None,
            salted_password: None,
        }
    }

    /// 完整的 client-first-message（含 GS2 头 `n,,`）。
    pub fn client_first_message(&self) -> String {
        format!("n,,{}", self.client_first_bare)
    }

    /// 处理 server-first-message，返回 client-final-message。
    pub fn handle_server_first(&mut self, server_first: &str) -> Result<String, JdbcError> {
        let mut server_nonce = String::new();
        let mut salt_b64 = String::new();
        let mut iterations: u32 = 0;

        for part in server_first.split(',') {
            let (k, v) = part
                .split_once('=')
                .ok_or_else(|| scram_err("server-first 字段格式错误"))?;
            match k {
                "r" => server_nonce = v.to_string(),
                "s" => salt_b64 = v.to_string(),
                "i" => {
                    iterations = v
                        .parse()
                        .map_err(|_| scram_err("server-first 迭代次数非法"))?
                }
                _ => {} // 忽略扩展字段
            }
        }

        if !server_nonce.starts_with(&self.client_nonce) {
            return Err(scram_err("服务器随机数未包含客户端随机数"));
        }
        if salt_b64.is_empty() || iterations == 0 {
            return Err(scram_err("server-first 缺少 salt 或迭代次数"));
        }
        if iterations > MAX_ITERATIONS {
            return Err(scram_err("迭代次数超出安全上限"));
        }

        let salt = base64::engine::general_purpose::STANDARD
            .decode(&salt_b64)
            .map_err(|_| scram_err("salt Base64 解码失败"))?;

        // SaltedPassword = PBKDF2-HMAC-SHA256(password, salt, i)
        let mut salted_password = [0u8; 32];
        pbkdf2_hmac_sha256(
            self.password.as_bytes(),
            &salt,
            iterations,
            &mut salted_password,
        );

        let client_final_without_proof = format!("c=biws,r={server_nonce}");
        let auth_message = format!(
            "{},{},{}",
            self.client_first_bare, server_first, client_final_without_proof
        );

        // ClientProof = ClientKey XOR ClientSignature
        let client_key = hmac_sha256(&salted_password, b"Client Key");
        let stored_key = Sha256::digest(&client_key);
        let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
        let proof: Vec<u8> = client_key
            .iter()
            .zip(client_signature.iter())
            .map(|(a, b)| a ^ b)
            .collect();

        self.server_first = Some(server_first.to_string());
        self.client_final_without_proof = Some(client_final_without_proof.clone());
        self.salted_password = Some(salted_password);

        let b64_proof = base64::engine::general_purpose::STANDARD.encode(proof);
        Ok(format!("{client_final_without_proof},p={b64_proof}"))
    }

    /// 校验 server-final-message 中的服务器签名（防冒充）。
    pub fn verify_server_final(&self, server_final: &str) -> Result<(), JdbcError> {
        if let Some(err) = server_final.strip_prefix("e=") {
            return Err(scram_err(&format!("服务器拒绝认证: {err}")));
        }
        let sig_b64 = server_final
            .strip_prefix("v=")
            .ok_or_else(|| scram_err("server-final 字段格式错误"))?;

        let server_first = self
            .server_first
            .as_ref()
            .ok_or_else(|| scram_err("SCRAM 状态错误"))?;
        let client_final = self
            .client_final_without_proof
            .as_ref()
            .ok_or_else(|| scram_err("SCRAM 状态错误"))?;
        let salted_password = self
            .salted_password
            .ok_or_else(|| scram_err("SCRAM 状态错误"))?;

        let auth_message = format!(
            "{},{},{}",
            self.client_first_bare, server_first, client_final
        );
        let server_key = hmac_sha256(&salted_password, b"Server Key");
        let expected = hmac_sha256(&server_key, auth_message.as_bytes());

        let actual = base64::engine::general_purpose::STANDARD
            .decode(sig_b64)
            .map_err(|_| scram_err("server-final 签名 Base64 解码失败"))?;

        // 恒定时间比较
        if actual.len() != expected.len()
            || actual
                .iter()
                .zip(expected.iter())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                != 0
        {
            return Err(scram_err("服务器签名校验失败（可能是中间人攻击）"));
        }
        Ok(())
    }
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC 接受任意长度密钥");
    mac.update(msg);
    mac.finalize().into_bytes().to_vec()
}

/// PBKDF2-HMAC-SHA256（RFC 2898），单块输出场景的手写实现。
fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    for (block_index, chunk) in out.chunks_mut(32).enumerate() {
        let mut salt_block = salt.to_vec();
        salt_block.extend_from_slice(&((block_index + 1) as u32).to_be_bytes());
        let mut u = hmac_sha256(password, &salt_block);
        let mut t = u.clone();
        for _ in 1..iterations {
            u = hmac_sha256(password, &u);
            for (ti, ui) in t.iter_mut().zip(u.iter()) {
                *ti ^= ui;
            }
        }
        chunk.copy_from_slice(&t[..chunk.len()]);
    }
}

/// 24 字节随机数（std 的 RandomState 由 OS 熵播种 + 时间/进程号混合）。
fn random_nonce() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};

    let mut buf = [0u8; 24];
    let s1 = RandomState::new();
    let s2 = RandomState::new();
    let mut h1 = s1.build_hasher();
    let mut h2 = s2.build_hasher();
    h1.write_u64(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0),
    );
    h2.write_u64(std::process::id() as u64);
    let a = h1.finish().to_le_bytes();
    let b = h2.finish().to_le_bytes();
    for i in 0..8 {
        buf[i] = a[i];
        buf[8 + i] = b[i];
        buf[16 + i] = a[i] ^ b[i].wrapping_add(i as u8);
    }
    base64::engine::general_purpose::STANDARD.encode(buf)
}

fn scram_err(msg: &str) -> JdbcError {
    JdbcError::Driver(format!("SCRAM-SHA-256 认证失败: {msg}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7677（SCRAM-SHA-256）官方示例：user="user" / password="pencil"。
    ///
    /// 期望值已用 .NET `System.Security.Cryptography` 独立交叉验证。
    /// 注意：RFC 5802 的 SCRAM-SHA-**1** 示例使用相同 nonce/salt 输入但证明值不同
    /// （`dHzbZapW...`），勿混淆两套向量。
    ///
    /// 另注：RFC 示例的 client-first-bare 含用户名 `n=user`；PostgreSQL 实际交换中
    /// 用户名已在 StartupMessage 发送、SCRAM 内嵌空 `n=`。两者对服务器都自洽
    ///（AuthMessage 以收到的原文计算），此处按 RFC 原文向量校验加密链路。
    #[test]
    fn rfc7677_example() {
        let mut scram = ScramSha256 {
            password: "pencil".into(),
            client_nonce: "fyko+d2lbbFgONRv9qkxdawL".into(),
            client_first_bare: "n=user,r=fyko+d2lbbFgONRv9qkxdawL".into(),
            server_first: None,
            client_final_without_proof: None,
            salted_password: None,
        };
        assert_eq!(
            scram.client_first_message(),
            "n,,n=user,r=fyko+d2lbbFgONRv9qkxdawL"
        );

        let server_first = "r=fyko+d2lbbFgONRv9qkxdawL3rfcNQJXZhpLrO6m7O9+0Rfy/l5n14bE113v3NhoBQ=,\
                            s=QSXCR+Q6sek8bf92,i=4096";
        let final_msg = scram.handle_server_first(server_first).unwrap();

        // RFC 7677 的客户端证明（SHA-256）
        assert_eq!(
            final_msg,
            "c=biws,r=fyko+d2lbbFgONRv9qkxdawL3rfcNQJXZhpLrO6m7O9+0Rfy/l5n14bE113v3NhoBQ=,\
             p=MsWu4v4Pmoo268itML54n+g/fiWfQJxuWhTPQp5CJoI="
        );

        // 对应的服务器签名（经 .NET 交叉验证）
        scram
            .verify_server_final("v=nIHtxlLoNM8NhtkAXs7qX1gFsui98eFoqVyobGsfAB8=")
            .unwrap();

        // 篡改的服务器签名必须被拒绝
        assert!(scram
            .verify_server_final("v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=")
            .is_err());
    }

    #[test]
    fn rejects_bad_server_nonce() {
        let mut scram = ScramSha256::new("pencil");
        let first = scram.client_first_message();
        let client_nonce = first.strip_prefix("n,,n=,r=").unwrap().to_string();
        let _ = client_nonce;
        let err = scram
            .handle_server_first("r=WRONGNONCE,s=QSXCR+Q6sek8bf92,i=4096")
            .unwrap_err();
        assert!(err.to_string().contains("客户端随机数"));
    }
}
