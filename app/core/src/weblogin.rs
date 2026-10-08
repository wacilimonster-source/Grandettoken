//! 网页登录那一段纯逻辑:什么算"用户已经登录完了",以及要把哪几个 Cookie 交给取数层。
//!
//! 背景(2026-10-08 真机取证):WorkBuddy 桌面端 5.6.2 起把本机凭据文件里的
//! `auth.accessToken` 改成 AES-GCM 信封(静态钥编译期内置、本机拿不到),
//! 「复用客户端登录态」那条路断了(见 2026-10-02 诊断)。改走挂件内网页登录之后,
//! 一开始以为要抓的是 `Authorization: Bearer <token>` —— **实测不是**:
//! 控制台自己打 `/billing/meter/*` 时既没有 Authorization 头,sessionStorage 里
//! 也没有 `growth-center-token`,认证全在 httpOnly 的 `session` / `session_2`
//! 两个 Cookie 上;而且网关(openresty)还按 User-Agent 分流 —— 同一批 Cookie,
//! 浏览器 UA 200、非浏览器 UA 一律 401。
//!
//! 所以这里钉两条规则:哪几个 Cookie 算会话、页面落在什么路径上才算登录完成。
//! 为什么放在 core 而不是 Tauri 外壳:src-tauri 不挂测试(见 core/src/lib.rs 顶部
//! 注释),而这两条规则判错的后果都是"看着登录了却永远取不到数",最难排查的一类。

/// 只认国内版域名。国际版(www.workbuddy.ai)是同名不同栈,不在考虑范围内(裁决 6A)。
pub const SITE_HOSTS: [&str; 2] = ["www.workbuddy.cn", "www.codebuddy.cn"];

/// 登录完成后控制台落地的路径前缀。实测路径:`/login/` →(点一次账号)→
/// `/profile/plans-usage`。没认这份会话的访问会被服务端挡回 `/login/`,
/// 所以"落在这些路径上"就是"登录确实完成了"的证据 —— 不能只看 cookie 罐里有没有东西。
pub const POST_LOGIN_PATHS: [&str; 2] = ["/profile", "/console"];

/// 会话 Cookie 的名字。2026-10-08 实测:只带 `session` 或只带 `session_2` 都是 401,
/// 两个一起才 200 —— 缺一个就当没登录,不凑合。
pub const SESSION_COOKIES: [&str; 2] = ["session", "session_2"];

/// 会话型凭据的前缀。带这个前缀的值当 `Cookie:` 头送,不带的当 Bearer 令牌送
/// (手动粘贴密钥那条备选路,裁决 4A)。
pub const COOKIE_PREFIX: &str = "cookie:";

/// 从 webview 的 Cookie 罐里挑出会话,拼成交给取数层的凭据。缺任何一个都返回 None:
/// 那时候用户其实还没登录完,拿半份会话去请求只会换来 401,不如老实报"未登录"。
pub fn cookie_credential<I, S>(jar: I) -> Option<String>
where
    I: IntoIterator<Item = (S, S)>,
    S: AsRef<str>,
{
    let mut seen: Vec<String> = Vec::new();
    let mut out = String::new();
    for (name, value) in jar {
        let (name, value) = (name.as_ref(), value.as_ref());
        if !SESSION_COOKIES.contains(&name) || seen.iter().any(|s| s == name) {
            continue;
        }
        // 值里有控制字符或空格一律跳过:脏数据写进请求头会把整条头拆坏
        if value.is_empty() || value.chars().any(|c| c.is_control() || c == ' ') {
            continue;
        }
        if !out.is_empty() {
            out.push_str("; ");
        }
        out.push_str(name);
        out.push('=');
        out.push_str(value);
        seen.push(name.to_string());
    }
    if SESSION_COOKIES.iter().all(|n| seen.iter().any(|s| s == n)) {
        Some(format!("{COOKIE_PREFIX}{out}"))
    } else {
        None
    }
}

/// 凭据是 Cookie 型的话,给出 `Cookie:` 请求头该带的值。
pub fn cookie_header(credential: &str) -> Option<&str> {    let rest = credential.trim().strip_prefix(COOKIE_PREFIX)?.trim();
    if rest.is_empty() || !rest.contains('=') {
        return None;
    }
    // 换行/回车会拆出第二条请求头,绝不能放行
    if rest.chars().any(|c| c.is_control()) {
        return None;
    }
    Some(rest)
}

/// 这个名字是不是会话 Cookie(清登录时按它挑要删的那几个)。
pub fn is_session_cookie(name: &str) -> bool {
    SESSION_COOKIES.contains(&name)
}

/// 页面落在"登录完成后"的路径上吗?
///
/// 主机名做**整串相等**比较,不是后缀匹配:`www.workbuddy.cn.attacker.example/profile`
/// 这种拼接必须挡掉,否则一个长得像的站点就能让挂件以为登录完成了。
pub fn is_post_login(url: &str) -> bool {
    let Some(rest) = url.split_once("://").map(|(_, r)| r) else {
        return false;
    };
    let (authority, tail) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    // userinfo(https://user@host)不算主机名
    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or(authority)
        .split(':')
        .next()
        .unwrap_or(authority);
    if !SITE_HOSTS.iter().any(|s| host.eq_ignore_ascii_case(s)) {
        return false;
    }
    let path = tail.split(|c| c == '?' || c == '#').next().unwrap_or("");
    POST_LOGIN_PATHS.iter().any(|p| path.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两个会话 Cookie 都在才算拿到会话;顺序按 Cookie 罐里的来。
    #[test]
    fn credential_needs_both_session_cookies() {
        let full = vec![
            ("trafficParams", "x"),
            ("session", "AAA"),
            ("session_2", "BBB"),
        ];
        assert_eq!(
            cookie_credential(full).as_deref(),
            Some("cookie:session=AAA; session_2=BBB")
        );
        // 只有一个 → 没登录完
        assert_eq!(
            cookie_credential(vec![("session", "AAA")].into_iter()),
            None
        );
        assert_eq!(
            cookie_credential(vec![("session_2", "BBB")].into_iter()),
            None
        );
        // 空值不算
        assert_eq!(
            cookie_credential(vec![("session", ""), ("session_2", "BBB")].into_iter()),
            None
        );
        // 脏值(空格/换行)跳过 → 缺一个 → 不算
        assert_eq!(
            cookie_credential(vec![("session", "A A"), ("session_2", "BBB")].into_iter()),
            None
        );
        assert_eq!(
            cookie_credential(vec![("session", "A\nB"), ("session_2", "BBB")].into_iter()),
            None
        );
        assert_eq!(cookie_credential(Vec::<(&str, &str)>::new()), None);
    }

    /// 同名 Cookie 出现两次(不同 path)只取第一次,别拼出重复键。
    #[test]
    fn duplicate_names_collapse() {
        let jar = vec![("session", "AAA"), ("session", "AAA2"), ("session_2", "BBB")];
        assert_eq!(
            cookie_credential(jar).as_deref(),
            Some("cookie:session=AAA; session_2=BBB")
        );
    }

    /// 前缀决定它当 Cookie 送还是当 Bearer 送,认不出来就返回 None(当令牌走)。
    #[test]
    fn cookie_header_only_for_prefixed_credentials() {
        assert_eq!(cookie_header("cookie:session=A; session_2=B"), Some("session=A; session_2=B"));
        assert_eq!(cookie_header("  cookie:session=A; session_2=B  "), Some("session=A; session_2=B"));
        // 没有前缀 = 手动粘贴的令牌,不走 Cookie 头
        assert_eq!(cookie_header("eyJhbGciOiJIUzI1NiJ9.abc"), None);
        // 前缀后面是空的/不像 cookie 的,一律拒绝
        assert_eq!(cookie_header("cookie:"), None);
        assert_eq!(cookie_header("cookie:   "), None);
        assert_eq!(cookie_header("cookie:nonsense"), None);
        // 控制字符(头注入)拒绝
        assert_eq!(cookie_header("cookie:a=b\r\nX-Evil: 1"), None);
    }

    /// "登录完成了"的判定:主机名整串相等 + 路径在前缀表里。
    #[test]
    fn post_login_paths() {
        assert!(is_post_login("https://www.workbuddy.cn/profile/plans-usage"));
        assert!(is_post_login(
            "https://www.workbuddy.cn/profile/plans-usage?from=login#top"
        ));
        assert!(is_post_login("https://www.codebuddy.cn/console/accounts"));
        // 登录页本身不是"登录完成"
        assert!(!is_post_login(
            "https://www.workbuddy.cn/login/?platform=usercenter&state=0"
        ));
        assert!(!is_post_login("https://www.workbuddy.cn/"));
        // 变形域名不算
        assert!(!is_post_login("https://www.workbuddy.cn.attacker.example/profile"));
        assert!(!is_post_login("https://evil.example/profile"));
        // userinfo 伪装:主机名解析出来是 attacker.example
        assert!(!is_post_login("https://www.workbuddy.cn@attacker.example/profile"));
        // 大小写与端口无关(主机名不区分大小写)
        assert!(is_post_login("https://WWW.WorkBuddy.CN:443/profile"));
        assert!(!is_post_login("not a url"));
    }
}
