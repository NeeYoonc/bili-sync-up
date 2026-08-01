use reqwest::header::{HeaderMap, HeaderValue};

/// 标准的Chrome 140浏览器User-Agent
pub const CHROME_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

/// 现代浏览器安全头
pub const SEC_CH_UA: &str = "\"Chromium\";v=\"140\", \"Not=A?Brand\";v=\"24\", \"Google Chrome\";v=\"140\"";
pub const SEC_CH_UA_MOBILE: &str = "?0";
pub const SEC_CH_UA_PLATFORM: &str = "\"Windows\"";

/// 为API请求创建标准请求头
pub fn create_api_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();

    headers.insert("User-Agent", HeaderValue::from_static(CHROME_USER_AGENT));
    headers.insert("Accept", HeaderValue::from_static("*/*"));
    headers.insert("Accept-Language", HeaderValue::from_static("zh-CN,zh;q=0.9,en;q=0.8"));
    headers.insert("Referer", HeaderValue::from_static("https://www.bilibili.com/"));
    headers.insert("Origin", HeaderValue::from_static("https://www.bilibili.com"));

    // 现代浏览器安全头
    headers.insert("sec-ch-ua", HeaderValue::from_static(SEC_CH_UA));
    headers.insert("sec-ch-ua-mobile", HeaderValue::from_static(SEC_CH_UA_MOBILE));
    headers.insert("sec-ch-ua-platform", HeaderValue::from_static(SEC_CH_UA_PLATFORM));
    headers.insert("sec-fetch-dest", HeaderValue::from_static("empty"));
    headers.insert("sec-fetch-mode", HeaderValue::from_static("cors"));
    headers.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));

    headers
}

/// 为图片下载创建请求头
pub fn create_image_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();

    headers.insert("User-Agent", HeaderValue::from_static(CHROME_USER_AGENT));
    headers.insert("Referer", HeaderValue::from_static("https://www.bilibili.com/"));

    // 图片请求的安全头
    headers.insert("sec-ch-ua", HeaderValue::from_static(SEC_CH_UA));
    headers.insert("sec-ch-ua-mobile", HeaderValue::from_static(SEC_CH_UA_MOBILE));
    headers.insert("sec-ch-ua-platform", HeaderValue::from_static(SEC_CH_UA_PLATFORM));
    headers.insert("sec-fetch-dest", HeaderValue::from_static("image"));
    headers.insert("sec-fetch-mode", HeaderValue::from_static("no-cors"));
    headers.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));

    headers
}

/// 为页面导航创建请求头
pub fn create_navigation_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();

    headers.insert("User-Agent", HeaderValue::from_static(CHROME_USER_AGENT));

    // 页面导航的安全头
    headers.insert("sec-ch-ua", HeaderValue::from_static(SEC_CH_UA));
    headers.insert("sec-ch-ua-mobile", HeaderValue::from_static(SEC_CH_UA_MOBILE));
    headers.insert("sec-ch-ua-platform", HeaderValue::from_static(SEC_CH_UA_PLATFORM));
    headers.insert("sec-fetch-dest", HeaderValue::from_static("document"));
    headers.insert("sec-fetch-mode", HeaderValue::from_static("navigate"));
    headers.insert("sec-fetch-site", HeaderValue::from_static("none"));

    headers
}

/// 为Aria2下载器创建请求头字符串数组
pub fn create_aria2_headers() -> Vec<String> {
    create_aria2_headers_with_referer("https://www.bilibili.com")
}

/// 为 Aria2 下载器创建带平台 Referer 的请求头字符串数组。
pub fn create_aria2_headers_with_referer(referer: &str) -> Vec<String> {
    vec![
        format!("User-Agent: {}", CHROME_USER_AGENT),
        format!("Referer: {referer}"),
        "Accept: */*".to_string(),
        "Accept-Language: zh-CN,zh;q=0.9,en;q=0.8".to_string(),
        format!("sec-ch-ua: {}", SEC_CH_UA),
        format!("sec-ch-ua-mobile: {}", SEC_CH_UA_MOBILE),
        format!("sec-ch-ua-platform: {}", SEC_CH_UA_PLATFORM),
        "sec-fetch-dest: empty".to_string(),
        "sec-fetch-mode: cors".to_string(),
        "sec-fetch-site: cross-site".to_string(),
        "Cache-Control: no-cache".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aria2_headers_override_platform_referer() {
        let headers = create_aria2_headers_with_referer("https://www.douyin.com/");

        assert!(headers
            .iter()
            .any(|header| header == "Referer: https://www.douyin.com/"));
        assert!(!headers.iter().any(|header| header.contains("bilibili.com")));
    }
}
