//! 网页登录令牌从「哨兵地址」交回来的那一段纯逻辑。
//!
//! 背景:WorkBuddy 桌面端 5.6.2 起把本机凭据文件里的 `auth.accessToken` 改成了
//! `{"$wbEncrypted":1,"envelope":…}` 的 AES-GCM 信封,静态钥编译期内置、本机拿不到
//! (2026-10-02 取证),「复用客户端登录态」那条路断了。替代做法是挂件开一个登录窗,
//! 把网页控制台自己发出的 `Authorization: Bearer <token>` 接住。交接动作:注入脚本
//! 抓到令牌后让页面跳 `https://tokenscope-capture.invalid/c?t=<token>`,这条导航在
//! Rust 侧被 `on_navigation` 拦下并解析 —— `.invalid` 是保留顶级域,DNS 永远解析不到,
//! 即便拦截失手令牌也不会出网。
//!
//! 为什么放在 core 而不是 Tauri 外壳:src-tauri 不挂测试(见 core/src/lib.rs 顶部
//! 注释,测试二进制不该链接 WebView2),而"什么算合法的哨兵回调""什么算能用的令牌"
//! 恰恰是最需要钉死的两条规则 —— 判错方向的话,用户面对的会是每轮 401 却查不出为什么。

use form_urlencoded::parse;

/// 哨兵域名。JS 侧写的是同一个字面量,改这里要一起改。
pub const CAPTURE_HOST: &str = "tokenscope-capture.invalid";

/// 令牌长度上限。桌面端那份实测 1287 字符,网页那套同量级;这里留十倍余量,
/// 但绝不把异常内容(例如整页 HTML 被误当成令牌)写进凭据库。
pub const TOKEN_MAX_LEN: usize = 16 * 1024;

/// 从一次导航里取回令牌。不是哨兵回调(或内容不合格)一律 None。
///
/// 主机名做的是**整串相等**比较,不是 `starts_with`:
/// `tokenscope-capture.invalid.attacker.example` 这种拼接必须挡掉,否则谁都能
/// 通过一次跳转把令牌钓走。
pub fn token_from_url(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let (host, tail) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if !host.eq_ignore_ascii_case(CAPTURE_HOST) {
        return None;
    }
    // 只认 query 形式的回调:`/c?t=…`(路径是什么都无所谓,但没有 ? 就不是脚本发的那次跳转)。
    let query_at = tail.find('?')?;
    let query = &tail[query_at + 1..];
    let query = query.split('#').next().unwrap_or(query);
    let (_, value) = parse(query.as_bytes()).find(|(key, _)| key == "t")?;
    sanitize_token(&value)
}

/// 校验并清洗令牌:去掉首尾空白与可能的 `Bearer ` 前缀,再挑掉不能用的内容。
///
/// 拒绝带空格/控制字符的值:Bearer 令牌是 base64url + 点,不含这两类。放行它们
/// 的后果是"凭据里存了一坨脏东西 → 每轮取数 401 → 用户反复重新登录也修不好",
/// 这是最难排查的一类问题。
pub fn sanitize_token(raw: &str) -> Option<String> {
    let mut t = raw.trim();
    for prefix in ["Bearer ", "bearer "] {
        if let Some(stripped) = t.strip_prefix(prefix) {
            t = stripped.trim();
            break;
        }
    }
    if t.is_empty() || t.len() > TOKEN_MAX_LEN {
        return None;
    }
    if t.chars().any(|c| c.is_control() || c == ' ') {
        return None;
    }
    Some(t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_url_yields_the_token() {
        let url = "https://tokenscope-capture.invalid/c?t=eyJhbGciOi.abc-_123%3D%3D";
        assert_eq!(token_from_url(url).as_deref(), Some("eyJhbGciOi.abc-_123=="));
        // 参数顺序无关,其它参数不干扰
        let other = "https://tokenscope-capture.invalid/c?from=wb&t=tok123&x=1";
        assert_eq!(token_from_url(other).as_deref(), Some("tok123"));
    }

    /// 主机名必须是整串相等。`starts_with` 会被这种拼接钓走令牌。
    #[test]
    fn lookalike_hosts_are_rejected() {
        assert_eq!(
            token_from_url("https://tokenscope-capture.invalid.attacker.example/c?t=x"),
            None
        );
        // userinfo 伪装:主机名解析出来是 attacker.example
        assert_eq!(
            token_from_url("https://tokenscope-capture.invalid@attacker.example/c?t=x"),
            None
        );
        assert_eq!(token_from_url("https://workbuddy.cn/c?t=x"), None);
        // 大小写与协议无关(主机名不区分大小写)
        assert_eq!(
            token_from_url("https://TokenScope-Capture.INVALID/c?t=x").as_deref(),
            Some("x")
        );
    }

    /// 没带 query 的回调、空值、超长、脏字符,都不算合法令牌。
    #[test]
    fn bad_payloads_are_rejected() {
        assert_eq!(token_from_url("https://tokenscope-capture.invalid/c"), None);
        assert_eq!(token_from_url("https://tokenscope-capture.invalid/c?t="), None);
        assert_eq!(token_from_url("https://tokenscope-capture.invalid/c?u=x"), None);
        assert_eq!(token_from_url("not a url at all"), None);
        // 换行/制表:多半是把整段响应体或 header 行误当令牌
        assert_eq!(sanitize_token("tok\nSecond-Line: 1"), None);
        assert_eq!(sanitize_token("tok with spaces"), None);
        assert_eq!(sanitize_token("   "), None);
        let huge = "a".repeat(TOKEN_MAX_LEN + 1);
        assert_eq!(sanitize_token(&huge), None);
    }

    /// 脚本万一没剥掉 `Bearer ` 前缀,这里兜住,别让一次登录白做。
    #[test]
    fn bearer_prefix_is_stripped_once() {
        assert_eq!(sanitize_token("Bearer eyJabc").as_deref(), Some("eyJabc"));
        assert_eq!(sanitize_token("bearer eyJabc").as_deref(), Some("eyJabc"));
        assert_eq!(sanitize_token("  eyJabc  ").as_deref(), Some("eyJabc"));
        // 只剥一层:中间还有空格的仍然判脏
        assert_eq!(sanitize_token("Bearer Bearer x y"), None);
    }
}
