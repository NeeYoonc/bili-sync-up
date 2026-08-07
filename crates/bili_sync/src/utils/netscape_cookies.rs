//! Netscape cookies.txt 的解析、序列化与浏览器式 Set-Cookie 续约合并。
//!
//! 抖音和 YouTube 的登录状态都以 Netscape 格式保存。浏览器之所以长时间不掉
//! 登录，是因为服务端每次响应都会通过 `Set-Cookie` 轮换/续期会话 Cookie；
//! 本模块把响应里的 `Set-Cookie` 合并写回 cookies.txt，让工具像浏览器一样
//! 持续续约，而不是等到过期后才提示重新导入。

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use cookie::Cookie;
use reqwest::header::{HeaderMap, HeaderValue, SET_COOKIE};
use tracing::{debug, warn};

/// cookies.txt 中的一行 Cookie。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetscapeCookie {
    /// 可带前导点，如 `.douyin.com`。
    pub domain: String,
    pub include_subdomains: bool,
    pub path: String,
    pub secure: bool,
    /// Unix 秒；0 表示会话 Cookie。
    pub expires: i64,
    pub http_only: bool,
    pub name: String,
    pub value: String,
}

/// 当前 Unix 时间戳（秒）。
pub fn now_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// 解析 Netscape cookies.txt 内容。
pub fn parse_netscape(contents: &str) -> Vec<NetscapeCookie> {
    let mut rows = Vec::new();
    for raw_line in contents.lines() {
        let line = raw_line.strip_prefix("#HttpOnly_").unwrap_or(raw_line);
        if line.trim_start().starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let columns = line.split('\t').collect::<Vec<_>>();
        if columns.len() < 7 {
            continue;
        }
        rows.push(NetscapeCookie {
            domain: columns[0].to_string(),
            include_subdomains: columns[1].eq_ignore_ascii_case("TRUE"),
            path: columns[2].to_string(),
            secure: columns[3].eq_ignore_ascii_case("TRUE"),
            expires: columns[4].parse().unwrap_or(0),
            http_only: raw_line.starts_with("#HttpOnly_"),
            name: columns[5].to_string(),
            value: columns[6].to_string(),
        });
    }
    rows
}

/// 序列化为 Netscape cookies.txt 内容。
pub fn serialize_netscape(rows: &[NetscapeCookie]) -> String {
    let mut out = String::new();
    out.push_str("# Netscape HTTP Cookie File\n");
    out.push_str("# Renewed by Bili Sync cookie renewal\n");
    for row in rows {
        if row.http_only {
            out.push_str("#HttpOnly_");
        }
        out.push_str(&row.domain);
        out.push('\t');
        out.push_str(if row.include_subdomains { "TRUE" } else { "FALSE" });
        out.push('\t');
        out.push_str(&row.path);
        out.push('\t');
        out.push_str(if row.secure { "TRUE" } else { "FALSE" });
        out.push('\t');
        out.push_str(&row.expires.to_string());
        out.push('\t');
        out.push_str(&row.name);
        out.push('\t');
        out.push_str(&row.value);
        out.push('\n');
    }
    out
}

fn normalize_domain_key(domain: &str) -> String {
    domain.trim_start_matches('.').to_ascii_lowercase()
}

/// 把一条 `Set-Cookie` 合并进现有行；返回是否产生了变化。
///
/// 规则与浏览器一致：同名（域 + 路径 + 名称）Cookie 覆盖，过期的删除，
/// 新名称追加。`allowed_domain` 只允许属于该平台的 Cookie 进入，避免把
/// 其它域的会话写进当前平台的 cookies.txt。
pub fn merge_set_cookie(
    rows: &mut Vec<NetscapeCookie>,
    set_cookie_header: &str,
    fallback_domain: &str,
    allowed_domain: impl Fn(&str) -> bool,
) -> bool {
    let Ok(cookie) = Cookie::parse(set_cookie_header) else {
        return false;
    };
    let name = cookie.name().to_string();
    let value = cookie.value().to_string();
    // 浏览器规则：无 Domain 属性时是主机 Cookie，取请求主机；有则取属性值。
    let domain = cookie
        .domain()
        .map(|domain| domain.trim_start_matches('.').to_string())
        .unwrap_or_else(|| fallback_domain.trim_start_matches('.').to_string());
    if !allowed_domain(&domain) {
        return false;
    }
    let path = cookie.path().map(|value| value.to_string()).unwrap_or_else(|| "/".to_string());
    // RFC 6265：Max-Age 优先于 Expires；Expiration 为 Session 时按会话 Cookie 处理。
    let expires = cookie
        .max_age()
        .map(|value| now_timestamp().saturating_add(value.whole_seconds()))
        .or_else(|| match cookie.expires() {
            Some(cookie::Expiration::DateTime(value)) => Some(value.unix_timestamp()),
            _ => None,
        })
        .unwrap_or(0);
    let expired = expires != 0 && expires <= now_timestamp();
    let key = normalize_domain_key(&domain);

    let mut changed = false;
    let mut replaced = false;
    let mut index = 0usize;
    while index < rows.len() {
        let row = &rows[index];
        let same_cookie = normalize_domain_key(&row.domain) == key && row.path == path && row.name == name;
        if !same_cookie {
            index += 1;
            continue;
        }
        if expired {
            rows.remove(index);
        } else {
            rows[index].value = value.clone();
            rows[index].expires = expires;
            rows[index].secure |= cookie.secure().unwrap_or(false);
            replaced = true;
        }
        changed = true;
        break;
    }
    if !expired && !replaced {
        rows.push(NetscapeCookie {
            domain: if cookie.domain().is_some() {
                format!(".{key}")
            } else {
                domain
            },
            include_subdomains: true,
            path,
            secure: cookie.secure().unwrap_or(false),
            expires,
            http_only: cookie.http_only().unwrap_or(false),
            name,
            value,
        });
        changed = true;
    }
    changed
}

/// 把响应头里的 `Set-Cookie` 合并写回 cookies.txt（浏览器式被动续约）。
/// 没有变化时不会触碰文件；读取/写入失败只告警，不阻断请求。
pub async fn renew_cookie_file(
    path: &Path,
    headers: &HeaderMap,
    fallback_domain: &str,
    allowed_domain: impl Fn(&str) -> bool,
) {
    let values = headers.get_all(SET_COOKIE).iter().collect::<Vec<&HeaderValue>>();
    if values.is_empty() {
        return;
    }
    let Ok(contents) = tokio::fs::read_to_string(path).await else {
        return;
    };
    let mut rows = parse_netscape(&contents);
    let mut changed = false;
    for value in values {
        let Ok(text) = value.to_str() else {
            continue;
        };
        changed |= merge_set_cookie(&mut rows, text, fallback_domain, &allowed_domain);
    }
    if !changed {
        return;
    }
    let serialized = serialize_netscape(&rows);
    if serialized == contents {
        return;
    }
    // 先写临时文件再替换，避免写入一半留下损坏的 cookies.txt。
    let temporary = path.with_extension("txt.renewing");
    if let Err(error) = tokio::fs::write(&temporary, serialized.as_bytes()).await {
        warn!(path = %path.display(), error = %error, "写入续约后的 Cookie 文件失败");
        return;
    }
    match replace_atomic(&temporary, path).await {
        Ok(()) => debug!(path = %path.display(), "Cookie 续约完成（合并服务端 Set-Cookie）"),
        Err(error) => {
            let _ = tokio::fs::remove_file(&temporary).await;
            warn!(path = %path.display(), error = %error, "替换续约后的 Cookie 文件失败");
        }
    }
}

async fn replace_atomic(temporary: &Path, target: &Path) -> std::io::Result<()> {
    match tokio::fs::rename(temporary, target).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            // Windows 下目标已存在时 rename 会拒绝；先删除旧文件再替换。
            let _ = tokio::fs::remove_file(target).await;
            tokio::fs::rename(temporary, target).await
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie_line(domain: &str, name: &str, value: &str) -> String {
        format!(
            "{domain}\tTRUE\t/\tTRUE\t1817124794\t{name}\t{value}"
        )
    }

    #[test]
    fn parses_and_serializes_netscape() {
        let contents = format!(
            "# Netscape HTTP Cookie File\n{}\n{}\n",
            cookie_line(".douyin.com", "ttwid", "v1"),
            cookie_line(".douyin.com", "sessionid", "s1")
        );
        let rows = parse_netscape(&contents);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "ttwid");
        assert_eq!(rows[1].expires, 1817124794);
        let serialized = serialize_netscape(&rows);
        assert_eq!(parse_netscape(&serialized), rows);
    }

    #[test]
    fn merges_set_cookie_and_removes_expired() {
        let mut rows = vec![NetscapeCookie {
            domain: ".douyin.com".to_string(),
            include_subdomains: true,
            path: "/".to_string(),
            secure: true,
            expires: 1817124794,
            http_only: false,
            name: "ttwid".to_string(),
            value: "old".to_string(),
        }];
        // 同名续约
        let changed = merge_set_cookie(
            &mut rows,
            "ttwid=new; Domain=.douyin.com; Path=/; Max-Age=3600",
            "www.douyin.com",
            |domain: &str| domain.ends_with("douyin.com"),
        );
        assert!(changed);
        assert_eq!(rows[0].value, "new");
        // 过期删除
        let changed = merge_set_cookie(
            &mut rows,
            "ttwid=gone; Domain=.douyin.com; Path=/; Max-Age=0",
            "www.douyin.com",
            |domain: &str| domain.ends_with("douyin.com"),
        );
        assert!(changed);
        assert!(rows.is_empty());
        // 其它域不写入
        let changed = merge_set_cookie(
            &mut rows,
            "SID=x; Domain=.google.com; Path=/",
            "www.youtube.com",
            |domain: &str| domain.ends_with("douyin.com"),
        );
        assert!(!changed);
    }
}
