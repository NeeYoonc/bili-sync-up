//! TikTok X-Gnarly / X-Dynosaur 签名生成器（服务端纯 Rust 实现）。
//!
//! X-Gnarly 依据 n1tr00-10/Tiktok-signature-gen-x_Gnarly 的 `x_gnarly.py`
//! 独立移植（参考版本 SDK 5.1.3-ZTCA / scm 1.0.0.368），只保留生成与自校验
//! 所需的路径，不引入 Python/Node 运行时。
//!
//! 外壳加密（L3）与 X-Dynosaur 同构，分 5 步：
//!   1. 生成 48 字节随机密钥 keyBytes，拆成 12 个 32 位小端整数 keyWords；
//!   2. 动态推导 ChaCha 轮次：r = Σ(keyWords[i] & 0xF) mod 16，rounds = r + 5；
//!   3. 非标准 ChaCha：state = PRNG_INIT_WORDS[0..12] + [0,0,0,0]，
//!      state[12] 兼作 Counter，每 64 字节密钥流 +1，Quarter Round 旋转 [16,12,8,7]，
//!      结束时 Feed-forward 叠加初始状态；
//!   4. 校验和确定密钥嵌入位置：mod = len(cipher)+1，insertPos = (ΣkeyBytes+Σcipher) mod mod；
//!   5. wire = magic ∥ cipher[0..pos) ∥ keyBytes(48) ∥ cipher[pos..]，再做变异 Base64。
//!
//! 注意：当前 TikTok 线上 SDK（webmssdk 5.3.1 / scm 1.0.0.388）的 X-Gnarly/X-Dynosaur
//! 外壳密钥流与公开参考不一致（算法整体被 VMP 虚拟机保护），本模块先落地已公开的
//! 参考实现与通用外壳，待新常量提取后可原地替换。

use rand::Rng;

/// X-Gnarly 输出头字节（当前抓包验证为 0x30）。
pub const X_GNARLY_MAGIC: u8 = 0x30;
/// X-Dynosaur 输出头字节（当前抓包验证为 0x33）。
#[allow(dead_code)]
pub const X_DYNO_MAGIC: u8 = 0x33;

/// 参考 SDK 的 PRNG 初始常量（12 个 32 位小端词）。
const PRNG_INIT_WORDS: [u32; 12] = [
    2517678443, 2718276124, 3212677781, 2633865432, 217618912, 2931180889, 1498001188, 2157053261, 211147047,
    185100057, 2903579748, 3732962506,
];

/// 变异 Base64 字母表：标准表，最终输出把 `+` 替换为 `-`。
const XGNARLY_B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

const CANVAS_DEFAULT: u32 = 1938040196;
/// 参考 SDK 版本号（字段 9）。
pub const SDK_VERSION: &str = "5.1.3-ZTCA";
/// 参考 SDK scm 版本号（字段 10）。
pub const SCM_VERSION: &str = "1.0.0.368";

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

/// 与参考实现一致的 ChaCha 块：`rounds` 计数“列/对角”两种半轮交替，
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

/// 生成密钥流并与明文逐字节 XOR。
/// 注意：参考实现中加密状态固定为 `PRNG_INIT_WORDS[0..12] + [0,0,0,0]`，
/// keyWords 只用于推导轮数与校验和，不直接进入状态。
fn chacha_xor(state_words: &[u32; 12], rounds: u32, cipher: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(cipher.len());
    let mut state = [0u32; 16];
    state[..12].copy_from_slice(state_words);
    let mut i = 0usize;
    while i < cipher.len() {
        let block = chacha_block(&state, rounds);
        for j in 0..64usize {
            if i + j >= cipher.len() {
                break;
            }
            let key_byte = ((block[j / 4] >> ((j % 4) * 8)) & 0xFF) as u8;
            out.push(cipher[i + j] ^ key_byte);
        }
        state[12] = u32v(u64::from(state[12]) + 1);
        i += 64;
    }
    out
}

/// 参考实现的变异 Base64。
fn b64_encode(data: &[u8]) -> String {
    let mut out = String::new();
    let n = data.len();
    let mut i = 0usize;
    while i < n {
        if i + 2 < n {
            let b = (u32::from(data[i]) << 16) | (u32::from(data[i + 1]) << 8) | u32::from(data[i + 2]);
            out.push(XGNARLY_B64[((b >> 18) & 0x3F) as usize] as char);
            out.push(XGNARLY_B64[((b >> 12) & 0x3F) as usize] as char);
            out.push(XGNARLY_B64[((b >> 6) & 0x3F) as usize] as char);
            out.push(XGNARLY_B64[(b & 0x3F) as usize] as char);
            i += 3;
        } else if i + 1 < n {
            let b = (u32::from(data[i]) << 16) | (u32::from(data[i + 1]) << 8);
            out.push(XGNARLY_B64[((b >> 18) & 0x3F) as usize] as char);
            out.push(XGNARLY_B64[((b >> 12) & 0x3F) as usize] as char);
            out.push(XGNARLY_B64[((b >> 6) & 0x3F) as usize] as char);
            out.push('=');
            i += 2;
        } else {
            let b = u32::from(data[i]) << 16;
            out.push(XGNARLY_B64[((b >> 18) & 0x3F) as usize] as char);
            out.push(XGNARLY_B64[((b >> 12) & 0x3F) as usize] as char);
            out.push('=');
            out.push('=');
            i += 1;
        }
    }
    out
}

/// 数值转字节（参考实现：小于 65025 用 2 字节，否则 4 字节）。
fn int_to_bytes(v: u64) -> Vec<u8> {
    if v < (255 * 255) as u64 {
        vec![((v >> 8) & 0xFF) as u8, (v & 0xFF) as u8]
    } else {
        vec![
            ((v >> 24) & 0xFF) as u8,
            ((v >> 16) & 0xFF) as u8,
            ((v >> 8) & 0xFF) as u8,
            (v & 0xFF) as u8,
        ]
    }
}

/// 取字符串前 4 字节按大端拼成 u32。
fn str_to_be_u32(s: &str) -> u32 {
    let buf = s.as_bytes();
    let mut acc = 0u32;
    for &b in buf.iter().take(4) {
        acc = (acc << 8) | u32::from(b);
    }
    acc
}

#[derive(Debug, Clone)]
enum FieldValue {
    Int(u64),
    Str(String),
}

/// 组装 [Count][Idx][Len_u16_BE][Val] 明文。
fn build_payload(fields: &std::collections::BTreeMap<u32, FieldValue>, order: &[u32]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(order.len() as u8);
    for &k in order {
        let v = &fields[&k];
        let vb: Vec<u8> = match v {
            FieldValue::Int(n) => int_to_bytes(*n),
            FieldValue::Str(s) => s.as_bytes().to_vec(),
        };
        out.push(k as u8);
        out.extend_from_slice(&int_to_bytes(vb.len() as u64));
        out.extend_from_slice(&vb);
    }
    out
}

/// 参考实现的 PRNG（用于生成 48 字节密钥）。
struct GnarlyPrng {
    state: [u32; 16],
    idx: usize,
}

impl GnarlyPrng {
    fn new(ts_ms: u64) -> Self {
        let mut rng = rand::thread_rng();
        let ts = ts_ms & u64::from(U32_MASK);
        let mut state = [0u32; 16];
        state[..12].copy_from_slice(&PRNG_INIT_WORDS);
        state[12] = ts as u32;
        state[13] = rng.gen::<u32>();
        state[14] = rng.gen::<u32>();
        state[15] = rng.gen::<u32>();
        Self { state, idx: 0 }
    }

    fn next_u32(&mut self) -> u32 {
        let block = chacha_block(&self.state, 8);
        let val = block[self.idx];
        if self.idx == 7 {
            self.state[12] = u32v(u64::from(self.state[12]) + 1);
            self.idx = 0;
        } else {
            self.idx += 1;
        }
        val
    }
}

/// 生成 X-Gnarly 签名。
///
/// `query_string`：去掉 X-Gnarly/X-Dynosaur/X-Bogus/msToken 后的 URL 查询串；
/// `user_agent`：请求 UA；`body`：GET 传空串；`ts_ms`：毫秒时间戳（None 用当前时间）。
pub fn x_gnarly(query_string: &str, user_agent: &str, body: &str, ts_ms: Option<u64>) -> String {
    let ts_ms = ts_ms.unwrap_or_else(now_millis);
    let ts_sec = ts_ms / 1000;

    let mut fields = std::collections::BTreeMap::new();
    let mut order: Vec<u32> = Vec::new();
    fn put(fields: &mut std::collections::BTreeMap<u32, FieldValue>, order: &mut Vec<u32>, k: u32, v: FieldValue) {
        fields.insert(k, v);
        if !order.contains(&k) {
            order.push(k);
        }
    }

    put(&mut fields, &mut order, 1, FieldValue::Int(1));
    put(&mut fields, &mut order, 2, FieldValue::Int(14));
    put(&mut fields, &mut order, 3, FieldValue::Str(md5_hex(query_string)));
    put(&mut fields, &mut order, 4, FieldValue::Str(md5_hex(body)));
    put(&mut fields, &mut order, 5, FieldValue::Str(md5_hex(user_agent)));
    put(&mut fields, &mut order, 6, FieldValue::Int(ts_sec));
    put(&mut fields, &mut order, 7, FieldValue::Int(u64::from(CANVAS_DEFAULT)));
    put(&mut fields, &mut order, 8, FieldValue::Int(ts_ms % 2147483648));
    put(&mut fields, &mut order, 9, FieldValue::Str(SDK_VERSION.to_string()));
    put(&mut fields, &mut order, 10, FieldValue::Str(SCM_VERSION.to_string()));
    put(&mut fields, &mut order, 11, FieldValue::Int(1));
    put(&mut fields, &mut order, 13, FieldValue::Str("web".to_string()));
    put(&mut fields, &mut order, 14, FieldValue::Str("chromium".to_string()));

    // 字段 12：1..=11 的异或校验
    let mut inner = 0u64;
    for i in 1..=11u32 {
        let v = &fields[&i];
        inner ^= match v {
            FieldValue::Int(n) => *n,
            FieldValue::Str(s) => u64::from(str_to_be_u32(s)),
        };
    }
    put(
        &mut fields,
        &mut order,
        12,
        FieldValue::Int(inner & u64::from(U32_MASK)),
    );

    // 字段 0：所有整型字段异或
    let mut outer = 0u64;
    for &k in &order {
        if let FieldValue::Int(n) = &fields[&k] {
            outer ^= *n;
        }
    }
    put(&mut fields, &mut order, 0, FieldValue::Int(outer & u64::from(U32_MASK)));

    let payload = build_payload(&fields, &order);

    // 生成密钥
    let mut prng = GnarlyPrng::new(ts_ms);
    let mut key_words = [0u32; 12];
    let mut key_bytes = Vec::with_capacity(48);
    let mut round_acc = 0u32;
    for w in key_words.iter_mut() {
        let word = prng.next_u32();
        *w = word;
        round_acc = (round_acc + (word & 0xF)) & 0xF;
        key_bytes.extend_from_slice(&word.to_le_bytes());
    }
    let rounds = round_acc + 5;

    // 加密：参考实现使用固定 PRNG 常量作状态
    let mut state_words = [0u32; 12];
    state_words.copy_from_slice(&PRNG_INIT_WORDS);
    let encrypted = chacha_xor(&state_words, rounds, &payload);

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

    // wire = magic ∥ enc[0..pos) ∥ key(48) ∥ enc[pos..]
    let mut wire = Vec::with_capacity(1 + encrypted.len() + 48);
    wire.push(X_GNARLY_MAGIC);
    wire.extend_from_slice(&encrypted[..pos]);
    wire.extend_from_slice(&key_bytes);
    wire.extend_from_slice(&encrypted[pos..]);

    let mut result = b64_encode(&wire);
    result = result.replace('+', "-");
    // 参考实现的尾部补齐逻辑
    if !result.ends_with("==") {
        if result.ends_with('=') {
            result.push('=');
        } else {
            result.push_str("==");
        }
    }
    result
}

fn md5_hex(input: &str) -> String {
    hex::encode(md5::compute(input.as_bytes()).0)
}

fn now_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 解码变异 Base64（`-` 还原为 `+`）。
#[allow(dead_code)]
pub fn b64_decode_mut(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let fixed = s.replace('-', "+");
    let mut b64 = fixed;
    while b64.len() % 4 != 0 {
        b64.push('=');
    }
    base64::engine::general_purpose::STANDARD.decode(b64).ok()
}

/// 解密外壳并返回 (明文, 密钥嵌入位置, 轮数)。
/// `magic` 期望的头字节（X-Gnarly=0x30，X-Dynosaur=0x33）。
#[allow(dead_code)]
pub fn decrypt_shell(sig: &str, magic: u8) -> Option<(Vec<u8>, usize, u32)> {
    let buf = b64_decode_mut(sig)?;
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
        let mut state_words = [0u32; 12];
        state_words.copy_from_slice(&PRNG_INIT_WORDS);
        let plain = chacha_xor(&state_words, rounds, &cipher);
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
        // 参考实现输出 332 字符，头字节 0x30
        assert_eq!(sig.len(), 332, "X-Gnarly 输出长度应为 332");
        assert!(!sig.contains('+'), "变异 Base64 不应包含 +");

        let (plain, _pos, rounds) = decrypt_shell(&sig, X_GNARLY_MAGIC).expect("应能自解密");
        assert!(plain.len() > 150 && plain.len() < 260, "明文长度异常: {}", plain.len());
        assert_eq!(plain[0], 15, "明文首字节应为字段数 15");
        assert!(rounds >= 5 && rounds <= 20, "轮数应在 [5,20]: {rounds}");
        // 字段 3 应是 md5(query) 的 hex
        let expect_md5 = md5_hex(query);
        let s = String::from_utf8_lossy(&plain);
        assert!(s.contains(&expect_md5), "明文应包含 md5(query)");
    }

    #[test]
    fn x_gnarly_stable_with_same_seed_ts() {
        // 同一毫秒时间戳下输出结构一致（随机词来自 thread_rng，只校验长度/头字节）
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
        assert!(decrypt_shell(&sig, X_DYNO_MAGIC).is_none());
    }
}
