//! 抖音 Web API `a_bogus` 参数生成器。
//!
//! 算法依据 Johnserf/f2 的 Apache-2.0 `f2/utils/abogus.py` 独立移植，
//! 这里只保留项目调用作品列表所需的 GET 请求路径，避免引入 Python/Node
//! 运行时或增大 Docker 镜像。

use rand::Rng;
use sm3::{Digest, Sm3};

const DEFAULT_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                         (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0";
const ALPHABET: &[u8; 64] = b"Dkdpgh2ZmsQB80/MfvV36XI1R45-WUAlEixNLwoqYTOPuzKFjJnry79HbGcaStCe";
const ALPHABET2: &[u8; 64] = b"ckdp1h4ZKsUB80/Mfvw36XIgR25+WQAlEi7NLboqYTOPuzmFjJnryx9HVGDaStCe";
const SORT_INDEX: [usize; 44] = [
    18, 20, 52, 26, 30, 34, 58, 38, 40, 53, 42, 21, 27, 54, 55, 31, 35, 57, 39, 41, 43, 22, 28, 32, 60, 36, 23, 29, 33,
    37, 44, 45, 59, 46, 47, 48, 49, 50, 24, 25, 65, 66, 70, 71,
];
const SORT_INDEX2: [usize; 44] = [
    18, 20, 26, 30, 34, 38, 40, 42, 21, 27, 31, 35, 39, 41, 43, 22, 28, 32, 36, 23, 29, 33, 37, 44, 45, 46, 47, 48, 49,
    50, 24, 25, 52, 53, 54, 55, 57, 58, 59, 60, 65, 66, 70, 71,
];
const BIG_ARRAY: [u8; 256] = [
    121, 243, 55, 234, 103, 36, 47, 228, 30, 231, 106, 6, 115, 95, 78, 101, 250, 207, 198, 50, 139, 227, 220, 105, 97,
    143, 34, 28, 194, 215, 18, 100, 159, 160, 43, 8, 169, 217, 180, 120, 247, 45, 90, 11, 27, 197, 46, 3, 84, 72, 5,
    68, 62, 56, 221, 75, 144, 79, 73, 161, 178, 81, 64, 187, 134, 117, 186, 118, 16, 241, 130, 71, 89, 147, 122, 129,
    65, 40, 88, 150, 110, 219, 199, 255, 181, 254, 48, 4, 195, 248, 208, 32, 116, 167, 69, 201, 17, 124, 125, 104, 96,
    83, 80, 127, 236, 108, 154, 126, 204, 15, 20, 135, 112, 158, 13, 1, 188, 164, 210, 237, 222, 98, 212, 77, 253, 42,
    170, 202, 26, 22, 29, 182, 251, 10, 173, 152, 58, 138, 54, 141, 185, 33, 157, 31, 252, 132, 233, 235, 102, 196,
    191, 223, 240, 148, 39, 123, 92, 82, 128, 109, 57, 24, 38, 113, 209, 245, 2, 119, 153, 229, 189, 214, 230, 174,
    232, 63, 52, 205, 86, 140, 66, 175, 111, 171, 246, 133, 238, 193, 99, 60, 74, 91, 225, 51, 76, 37, 145, 211, 166,
    151, 213, 206, 0, 200, 244, 176, 218, 44, 184, 172, 49, 216, 93, 168, 53, 21, 183, 41, 67, 85, 224, 155, 226, 242,
    87, 177, 146, 70, 190, 12, 162, 19, 137, 114, 25, 165, 163, 192, 23, 59, 9, 94, 179, 107, 35, 7, 142, 131, 239,
    203, 149, 136, 61, 249, 14, 156,
];

pub fn user_agent() -> &'static str {
    DEFAULT_UA
}

pub fn generate(params: &str) -> String {
    let mut rng = rand::thread_rng();
    let inner_width = rng.gen_range(1024..=1920);
    let inner_height = rng.gen_range(768..=1080);
    let fingerprint = format!(
        "{}|{}|{}|{}|0|{}|0|0|{}|{}|{}|{}|{}|{}|24|24|Win32",
        inner_width,
        inner_height,
        inner_width + rng.gen_range(24..=32),
        inner_height + rng.gen_range(75..=90),
        if rng.gen_bool(0.5) { 0 } else { 30 },
        rng.gen_range(1024..=1920),
        rng.gen_range(768..=1080),
        rng.gen_range(1280..=1920),
        rng.gen_range(800..=1080),
        inner_width,
        inner_height,
    );
    generate_with(params, "", DEFAULT_UA, &fingerprint, current_millis(), &mut rng)
}

fn generate_with<R: Rng + ?Sized>(
    params: &str,
    body: &str,
    user_agent: &str,
    fingerprint: &str,
    timestamp: u64,
    rng: &mut R,
) -> String {
    let random = random_bytes(rng);
    generate_from_parts(params, body, user_agent, fingerprint, timestamp, &random)
}

fn generate_from_parts(
    params: &str,
    body: &str,
    user_agent: &str,
    fingerprint: &str,
    timestamp: u64,
    random: &[u8],
) -> String {
    let array1 = sm3_twice_salted(params.as_bytes());
    let array2 = sm3_twice_salted(body.as_bytes());
    let encrypted_ua = rc4(&[0, 1, 14], user_agent.as_bytes());
    let encoded_ua = custom_base64(&encrypted_ua, ALPHABET2, true);
    let array3 = sm3_bytes(encoded_ua.as_bytes());

    let mut values = [0u32; 72];
    values[8] = 3;
    values[18] = 44;
    values[20] = ((timestamp >> 24) & 255) as u32;
    values[21] = ((timestamp >> 16) & 255) as u32;
    values[22] = ((timestamp >> 8) & 255) as u32;
    values[23] = (timestamp & 255) as u32;
    values[24] = ((timestamp >> 32) & 255) as u32;
    values[25] = ((timestamp >> 40) & 255) as u32;
    values[30] = 0;
    values[31] = 1;
    values[37] = 14;
    values[38] = array1[21] as u32;
    values[39] = array1[22] as u32;
    values[40] = array2[21] as u32;
    values[41] = array2[22] as u32;
    values[42] = array3[23] as u32;
    values[43] = array3[24] as u32;
    values[44] = values[20];
    values[45] = values[21];
    values[46] = values[22];
    values[47] = values[23];
    values[48] = 3;
    values[49] = values[24];
    values[50] = values[25];
    values[56] = 6383;
    values[57] = 6383 & 255;
    values[58] = (6383 >> 8) & 255;
    values[64] = fingerprint.len() as u32;
    values[65] = fingerprint.len() as u32;

    let mut plain = Vec::with_capacity(SORT_INDEX.len() + fingerprint.len() + 1);
    plain.extend(SORT_INDEX.iter().map(|&index| values[index] as u8));
    plain.extend_from_slice(fingerprint.as_bytes());
    let mut checksum = values[SORT_INDEX2[0]] as u8;
    for &index in &SORT_INDEX2[1..] {
        checksum ^= values[index] as u8;
    }
    plain.push(checksum);

    let mut input = random.to_vec();
    input.extend(transform(&plain));
    custom_base64(&input, ALPHABET, true)
}

fn current_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn sm3_bytes(input: &[u8]) -> [u8; 32] {
    let mut digest = Sm3::new();
    digest.update(input);
    digest.finalize().into()
}

fn sm3_twice_salted(input: &[u8]) -> [u8; 32] {
    let mut first_input = Vec::with_capacity(input.len() + 3);
    first_input.extend_from_slice(input);
    first_input.extend_from_slice(b"cus");
    let first = sm3_bytes(&first_input);
    // f2 的第二次 params_to_array 接收的是字节数组，只有第一次
    // 字符串输入会追加 `cus`；这里不能再次加盐。
    sm3_bytes(&first)
}

fn rc4(key: &[u8], input: &[u8]) -> Vec<u8> {
    let mut state = [0u8; 256];
    for (index, value) in state.iter_mut().enumerate() {
        *value = index as u8;
    }
    let mut j = 0usize;
    for i in 0..256 {
        j = (j + state[i] as usize + key[i % key.len()] as usize) % 256;
        state.swap(i, j);
    }
    let (mut i, mut j) = (0usize, 0usize);
    input
        .iter()
        .map(|byte| {
            i = (i + 1) % 256;
            j = (j + state[i] as usize) % 256;
            state.swap(i, j);
            byte ^ state[(state[i] as usize + state[j] as usize) % 256]
        })
        .collect()
}

fn transform(input: &[u8]) -> Vec<u8> {
    let mut table = BIG_ARRAY;
    let mut index_b = table[1] as usize;
    let mut initial = 0usize;
    let mut value_e = 0usize;
    let mut result = Vec::with_capacity(input.len());
    for (index, byte) in input.iter().enumerate() {
        let mut sum = if index == 0 {
            initial = table[index_b] as usize;
            let value = index_b + initial;
            table[1] = initial as u8;
            table[index_b] = index_b as u8;
            value
        } else {
            initial + value_e
        };
        sum %= table.len();
        result.push(byte ^ table[sum]);
        value_e = table[(index + 2) % table.len()] as usize;
        sum = (index_b + value_e) % table.len();
        initial = table[sum] as usize;
        table[sum] = table[(index + 2) % table.len()];
        table[(index + 2) % table.len()] = initial as u8;
        index_b = sum;
    }
    result
}

fn custom_base64(input: &[u8], alphabet: &[u8; 64], padding: bool) -> String {
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let packed = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | chunk.get(2).copied().unwrap_or(0) as u32;
        output.push(alphabet[((packed >> 18) & 63) as usize] as char);
        output.push(alphabet[((packed >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            output.push(alphabet[((packed >> 6) & 63) as usize] as char);
        } else if padding {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(alphabet[(packed & 63) as usize] as char);
        } else if padding {
            output.push('=');
        }
    }
    output
}

fn random_bytes<R: Rng + ?Sized>(rng: &mut R) -> Vec<u8> {
    let mut result = Vec::with_capacity(12);
    for _ in 0..3 {
        let value: u16 = rng.gen_range(0..10_000);
        result.extend([
            ((value as u8 & 170) | 1),
            ((value as u8 & 85) | 2),
            (((value >> 8) as u8 & 170) | 5),
            (((value >> 8) as u8 & 85) | 40),
        ]);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{rngs::StdRng, SeedableRng};

    #[test]
    fn rc4_matches_well_known_vector() {
        assert_eq!(hex::encode(rc4(b"Key", b"Plaintext")), "bbf316e8d940af0ad3");
    }

    #[test]
    fn generated_value_has_expected_shape() {
        let mut rng = StdRng::seed_from_u64(42);
        let value = generate_with(
            "device_platform=webapp&aid=6383&sec_user_id=test",
            "",
            DEFAULT_UA,
            "1920|1080|1948|1160|0|0|0|0|1920|1080|1920|1080|1920|1080|24|24|Win32",
            1_720_000_000_000,
            &mut rng,
        );
        assert!(value.len() > 100);
        assert!(value.bytes().all(|byte| ALPHABET.contains(&byte) || byte == b'='));
    }

    #[test]
    fn matches_f2_reference_vector() {
        let value = generate_from_parts(
            "device_platform=webapp&aid=6383&sec_user_id=test",
            "",
            DEFAULT_UA,
            "1920|1080|1948|1160|0|0|0|0|1920|1080|1920|1080|1920|1080|24|24|Win32",
            1_720_000_000_000,
            &[1, 2, 5, 40, 9, 18, 13, 40, 33, 2, 21, 40],
        );
        assert_eq!(
            value,
            "DfmhQDLVpVEiDi6Y5l/LfY3q6313YDO/0SVkMD2fnx3GJL39HMYD9exobQ4vpY8jNs/DIebjy4hbO3xprQAjM36UHWwoldQ2m66kKl5Q5xSSs1feeLbQrsJx-k4lFeep5JV3EcvhqJKczbEk09Or4hqvPjoja3LkFk6FOoBu"
        );
    }
}
