//! TikTok X-Gnarly 签名生成器（服务端纯 Rust 实现，VMP v3 完整复现）。
//!
//! 依据 webmssdk 5.3.1 / scm 1.0.0.388（VMP 逆向 + 运行时逐字节验证）独立移植，
//! 端到端验证：`statusCode: 0`（真实 TikTok API 接受）。
//!
//! 签名流程（u[964] = handler 247，经 fetch 拦截器调用，参数 (明文,1,"s3",false)）：
//!   1. 构造二进制明文字段结构 `[count][fid:1][len:2 BE][value]...`（16 字段）；
//!   2. 版本字节固定 0x4B（'K'，`255 & (1<<6|8|3)`，skip_lzw=true 时 e=3）；
//!   3. 生成 48 字节随机密钥 keyBytes，拆成 12 个小端 u32 keyWords；
//!   4. 动态 ChaCha 轮次：r = Σ(keyWords[i] & 0xF) mod 16，rounds = r + 5；
//!      非标准 ChaCha：state = [1196819126,600974999,3863347763,1451689750] + keyWords(12)，
//!      state[12] 兼作 Counter（每 64 字节密钥流 +1），Quarter Round 旋转 [16,12,8,7]，
//!      结束时 Feed-forward 叠加初始状态；
//!   5. 明文直接加密（`skip_lzw=true`，不经过 LZW 压缩）；
//!   6. 校验和确定密钥嵌入位置：pos = (ΣkeyBytes+Σ密文) mod (len+1)，
//!      wire = 0x4B ∥ 密文[0..pos) ∥ keyBytes(48) ∥ 密文[pos..]；
//!   7. 变体 Base64 编码，字母表 "s3" = "u09tbS3UvgDEe6r-ZVMXzLpsAohTn7mdINQlW412GqBjfYiyk8JORCF5/xKHwacP"。
//!
//! 明文 16 字段（字段 0/1/2/8/14/15/16 为浏览器环境指纹值，服务端不严格校验；
//! 字段 3/4/5 为 md5(query)/md5(body)/md5(UA)；字段 6 为秒时间戳；字段 12/13 为调用计数）。

use rand::Rng;
use std::sync::atomic::{AtomicUsize, Ordering};

/// X-Gnarly 输出头字节（VMP v3 实测：0x4B，即 ASCII 'K'）。
pub const X_GNARLY_MAGIC: u8 = 0x4B;

/// 当前线上 SDK（webmssdk 5.3.1）的 ChaCha 状态常量（VMP 逆向确认，u[875]）。
const CHACHA_INIT_WORDS: [u32; 4] = [1196819126, 600974999, 3863347763, 1451689750];

/// 变体 Base64 字母表（"s3"，真实 X-Gnarly/X-Dynosaur 输出字符集）。
const S3_B64: &[u8; 64] = b"u09tbS3UvgDEe6r-ZVMXzLpsAohTn7mdINQlW412GqBjfYiyk8JORCF5/xKHwacP";

/// 参考 SDK 版本号（明文字段 9）。
pub const SDK_VERSION: &str = "5.3.1";
/// 参考 SDK scm 版本号（明文字段 10）。
pub const SCM_VERSION: &str = "1.0.0.388";

/// 环境指纹固定值（Node/无头环境采样，服务端不严格校验）。
const F_FIELD0: u32 = 0x6034_3264; // "`42d"
const F_FIELD1: u16 = 129; // 0x0081
const F_FIELD2: u16 = 14; // 0x000e
const F_FIELD8: u32 = 0x6993_cae1;
const F_FIELD14: u32 = 0x0081_c8f1;
const F_FIELD15: u32 = 0x966c_73e1;
const F_FIELD16: u32 = 0x0ac2_bd10;

const U32_MASK: u32 = 0xFFFF_FFFF;

#[inline]
fn u32v(x: u64) -> u32 {
    (x & u64::from(U32_MASK)) as u32
}

#[inline]
fn rotl(x: u32, n: u32) -> u32 {
    x.rotate_left(n)
}

fn quarter_round(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = u32v(u64::from(s[a]) + u64::from(s[b]));
    s[d] = rotl(s[d] ^ s[a], 16);
    s[c] = u32v(u64::from(s[c]) + u64::from(s[d]));
    s[b] = rotl(s[b] ^ s[c], 12);
    s[a] = u32v(u64::from(s[a]) + u64::from(s[b]));
    s[d] = rotl(s[d] ^ s[a], 8);
    s[c] = u32v(u64::from(s[c]) + u64::from(s[d]));
    s[b] = rotl(s[b] ^ s[c], 7);
}

/// 与线上 SDK 一致的 ChaCha 块：`rounds` 计数“列/对角”两种半轮交替，
/// 结束后叠加初始状态（feed-forward）。
fn chacha_block(initial: &[u32; 16], rounds: u32) -> [u32; 16] {
    let mut working = *initial;
    let mut r = 0u32;
    while r < rounds {
        quarter_round(&mut working, 0, 4, 8, 12);
        quarter_round(&mut working, 1, 5, 9, 13);
        quarter_round(&mut working, 2, 6, 10, 14);
        quarter_round(&mut working, 3, 7, 11, 15);
        r += 1;
        if r >= rounds {
            break;
        }
        quarter_round(&mut working, 0, 5, 10, 15);
        quarter_round(&mut working, 1, 6, 11, 12);
        quarter_round(&mut working, 2, 7, 12, 13);
        quarter_round(&mut working, 3, 4, 13, 14);
        r += 1;
    }
    let mut out = [0u32; 16];
    for i in 0..16 {
        out[i] = u32v(u64::from(working[i]) + u64::from(initial[i]));
    }
    out
}

/// 生成密钥流并与输入逐字节 XOR（明文加密 / 密文解密同一函数）。
/// 加密状态 = `CHACHA_INIT_WORDS[0..4] + keyWords[0..12]`（共 16 词），
/// keyWords 同时决定轮数并参与密钥流；`state[12]` 兼作计数器。
fn chacha_xor(state: &mut [u32; 16], rounds: u32, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0usize;
    while i < data.len() {
        let block = chacha_block(state, rounds);
        for j in 0..64usize {
            if i + j >= data.len() {
                break;
            }
            let key_byte = ((block[j / 4] >> ((j % 4) * 8)) & 0xFF) as u8;
            out.push(data[i + j] ^ key_byte);
        }
        state[12] = u32v(u64::from(state[12]) + 1);
        i += 64;
    }
    out
}

/// 变体 Base64 编码（"s3" 字母表，标准 3→4 字节展开，尾部 `=` 补齐）。
fn s3_b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    let mut i = 0usize;
    while i < data.len() {
        if i + 2 < data.len() {
            let b = (u32::from(data[i]) << 16) | (u32::from(data[i + 1]) << 8) | u32::from(data[i + 2]);
            out.push(S3_B64[((b >> 18) & 0x3F) as usize] as char);
            out.push(S3_B64[((b >> 12) & 0x3F) as usize] as char);
            out.push(S3_B64[((b >> 6) & 0x3F) as usize] as char);
            out.push(S3_B64[(b & 0x3F) as usize] as char);
            i += 3;
        } else if i + 1 < data.len() {
            let b = (u32::from(data[i]) << 16) | (u32::from(data[i + 1]) << 8);
            out.push(S3_B64[((b >> 18) & 0x3F) as usize] as char);
            out.push(S3_B64[((b >> 12) & 0x3F) as usize] as char);
            out.push(S3_B64[((b >> 6) & 0x3F) as usize] as char);
            out.push('=');
            i += 2;
        } else {
            let b = u32::from(data[i]) << 16;
            out.push(S3_B64[((b >> 18) & 0x3F) as usize] as char);
            out.push(S3_B64[((b >> 12) & 0x3F) as usize] as char);
            out.push('=');
            out.push('=');
            i += 1;
        }
    }
    out
}

/// 追加一个 `[fid:1][len:2 BE][value]` 字段。
fn push_field(out: &mut Vec<u8>, fid: u8, value: &[u8]) {
    out.push(fid);
    out.push(((value.len() >> 8) & 0xFF) as u8);
    out.push((value.len() & 0xFF) as u8);
    out.extend_from_slice(value);
}

fn md5_hex(input: &str) -> String {
    hex::encode(md5::compute(input.as_bytes()).0)
}

/// 构造 X-Gnarly 明文字段结构（VMP v3，16 字段）。
fn build_v3_payload(query_string: &str, user_agent: &str, body: &str, ts_sec: u32) -> Vec<u8> {
    // 调用计数（字段 12/13，服务端不严格校验，从 1 递增即可）
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let counter = (COUNTER.fetch_add(1, Ordering::Relaxed) + 1) as u16;

    let md5_query = md5_hex(query_string);
    let md5_body = md5_hex(body);
    let md5_ua = md5_hex(user_agent);

    let mut out = Vec::with_capacity(193);
    out.push(16); // count
    // 字段顺序与线上 SDK 一致：16,0,14,6,13,2,12,9,11,4,3,1,8,15,10,5
    push_field(&mut out, 16, &F_FIELD16.to_be_bytes());
    push_field(&mut out, 0, &F_FIELD0.to_be_bytes());
    push_field(&mut out, 14, &F_FIELD14.to_be_bytes());
    push_field(&mut out, 6, &ts_sec.to_be_bytes());
    push_field(&mut out, 13, &counter.to_be_bytes());
    push_field(&mut out, 2, &F_FIELD2.to_be_bytes());
    push_field(&mut out, 12, &counter.to_be_bytes());
    push_field(&mut out, 9, SDK_VERSION.as_bytes());
    push_field(&mut out, 11, &1u16.to_be_bytes());
    push_field(&mut out, 4, md5_body.as_bytes());
    push_field(&mut out, 3, md5_query.as_bytes());
    push_field(&mut out, 1, &F_FIELD1.to_be_bytes());
    push_field(&mut out, 8, &F_FIELD8.to_be_bytes());
    push_field(&mut out, 15, &F_FIELD15.to_be_bytes());
    push_field(&mut out, 10, SCM_VERSION.as_bytes());
    push_field(&mut out, 5, md5_ua.as_bytes());
    out
}

fn now_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 生成 X-Gnarly 签名。
///
/// `query_string`：去掉 X-Gnarly/X-Dynosaur/X-Bogus/msToken 后的 URL 查询串；
/// `user_agent`：请求 UA；`body`：GET 传空串；`ts_ms`：毫秒时间戳（None 用当前时间）。
pub fn x_gnarly(query_string: &str, user_agent: &str, body: &str, ts_ms: Option<u64>) -> String {
    let ts_ms = ts_ms.unwrap_or_else(now_millis);
    let ts_sec = (ts_ms / 1000) as u32;

    let payload = build_v3_payload(query_string, user_agent, body, ts_sec);

    // 生成 48 字节随机密钥，拆成 12 个小端 u32 keyWords
    let mut key_bytes = [0u8; 48];
    for b in key_bytes.iter_mut() {
        *b = rand::thread_rng().gen();
    }
    let mut key_words = [0u32; 12];
    let mut round_acc = 0u32;
    for (i, w) in key_words.iter_mut().enumerate() {
        let word = u32::from_le_bytes([
            key_bytes[i * 4],
            key_bytes[i * 4 + 1],
            key_bytes[i * 4 + 2],
            key_bytes[i * 4 + 3],
        ]);
        *w = word;
        round_acc = (round_acc + (word & 0xF)) & 0xF;
    }
    let rounds = round_acc + 5;

    // 加密：state = CHACHA_INIT_WORDS(4) + keyWords(12)
    let mut state = [0u32; 16];
    state[..4].copy_from_slice(&CHACHA_INIT_WORDS);
    state[4..].copy_from_slice(&key_words);
    let encrypted = chacha_xor(&mut state, rounds, &payload);

    // 校验和确定密钥嵌入位置
    let mut pos = 0u64;
    let modu = (encrypted.len() + 1) as u64;
    for &b in &key_bytes {
        pos = (pos + u64::from(b)) % modu;
    }
    for &c in &encrypted {
        pos = (pos + u64::from(c)) % modu;
    }
    let pos = pos as usize;

    // wire = magic(0x4B) ∥ enc[0..pos) ∥ key(48) ∥ enc[pos..]
    let mut wire = Vec::with_capacity(1 + encrypted.len() + 48);
    wire.push(X_GNARLY_MAGIC);
    wire.extend_from_slice(&encrypted[..pos]);
    wire.extend_from_slice(&key_bytes);
    wire.extend_from_slice(&encrypted[pos..]);

    s3_b64_encode(&wire)
}

/// 解码变体 Base64（"s3" 字母表）。
#[allow(dead_code)]
pub fn s3_b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut val = 0u32;
    let mut bits = 0u32;
    for ch in s.chars() {
        if ch == '=' {
            break;
        }
        let idx = S3_B64.iter().position(|&c| c == ch as u8)?;
        val = (val << 6) | idx as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((val >> bits) & 0xFF) as u8);
        }
    }
    Some(out)
}

/// 解密外壳并返回 (明文, 密钥嵌入位置, 轮数)。
#[allow(dead_code)]
pub fn decrypt_shell(sig: &str, magic: u8) -> Option<(Vec<u8>, usize, u32)> {
    let buf = s3_b64_decode(sig)?;
    if buf.first() != Some(&magic) {
        return None;
    }
    let body = &buf[1..];
    let n = body.len().checked_sub(48)?;
    for pos in 0..=n {
        let key = &body[pos..pos + 48];
        let cipher: Vec<u8> = body[..pos].iter().chain(&body[pos + 48..]).copied().collect();
        let sum_key: u64 = key.iter().map(|&b| u64::from(b)).sum();
        let sum_cipher: u64 = cipher.iter().map(|&b| u64::from(b)).sum();
        if (sum_key + sum_cipher) % (n as u64 + 1) != pos as u64 {
            continue;
        }
        let mut kw = [0u32; 12];
        for (i, w) in kw.iter_mut().enumerate() {
            *w = u32::from_le_bytes([key[i * 4], key[i * 4 + 1], key[i * 4 + 2], key[i * 4 + 3]]);
        }
        let mut round_acc = 0u32;
        for w in kw.iter() {
            round_acc = (round_acc + (w & 0xF)) & 0xF;
        }
        let rounds = round_acc + 5;
        let mut state = [0u32; 16];
        state[..4].copy_from_slice(&CHACHA_INIT_WORDS);
        state[4..].copy_from_slice(&kw);
        let plain = chacha_xor(&mut state, rounds, &cipher);
        return Some((plain, pos, rounds));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x_gnarly_format_and_roundtrip() {
        let query = "aid=1988&app_name=tiktok_web";
        let ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";
        let sig = x_gnarly(query, ua, "", Some(1_700_000_000_123));
        // VMP v3: magic 0x4B, 明文 193 字节 -> wire 242 -> 324 字符
        assert_eq!(sig.len(), 324, "X-Gnarly 输出长度应为 324");
        assert!(!sig.contains('+'), "s3 字母表不应包含 +");

        let (plain, _pos, rounds) = decrypt_shell(&sig, X_GNARLY_MAGIC).expect("应能自解密");
        assert_eq!(plain.len(), 193, "明文长度应为 193");
        assert_eq!(plain[0], 16, "明文首字节应为字段数 16");
        assert!(rounds >= 5 && rounds <= 20, "轮数应在 [5,20]: {rounds}");
        // 字段 3 应是 md5(query) 的 hex
        let expect_md5 = md5_hex(query);
        let s = String::from_utf8_lossy(&plain);
        assert!(s.contains(&expect_md5), "明文应包含 md5(query)");
        // 字段 9/10 版本
        assert!(s.contains(SDK_VERSION), "明文应包含 SDK 版本");
        assert!(s.contains(SCM_VERSION), "明文应包含 scm 版本");
    }

    #[test]
    fn x_gnarly_stable_with_same_seed_ts() {
        let a = x_gnarly("aid=1988", "UA", "", Some(1_700_000_000_123));
        let b = x_gnarly("aid=1988", "UA", "", Some(1_700_000_000_123));
        assert_eq!(a.len(), b.len());
        assert_eq!(
            decrypt_shell(&a, X_GNARLY_MAGIC).map(|p| p.0.len()),
            decrypt_shell(&b, X_GNARLY_MAGIC).map(|p| p.0.len())
        );
    }

    #[test]
    fn decrypt_shell_rejects_bad_magic() {
        let sig = x_gnarly("aid=1988", "UA", "", Some(1_700_000_000_123));
        assert!(decrypt_shell(&sig, 0x30).is_none());
    }

    #[test]
    fn s3_b64_roundtrip() {
        let data = b"hello world\x00\xff\x80";
        let enc = s3_b64_encode(data);
        assert_eq!(s3_b64_decode(&enc).unwrap(), data);
    }
}
