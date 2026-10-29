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

/// 网页会话验证通过时刻的 settings 键(挂件外壳在登录成功时写,unix 秒字符串)。
pub const WEB_SESSION_AT_KEY: &str = "wb_web_session_at";

/// 网页会话的年龄提示。
///
/// 平台会话是**绝对 TTL**:2026-10-08 登录 → 2026-10-15 11:56:47 最后一次成功,
/// 恰好 168 小时,期间挂件每 60 秒都在用这份会话真打接口也没能续命 —— 说明不是
/// "闲置才掉",而是到点就死。掉之前帮用户把预期摆出来:第 5 天起详情里多一行
/// "已使用几天"。`at` 缺失(从未记录)或已过期好久一律不拦正常取数,这只是文案。
///
/// 返回 None = 不提示(没有记录 / 不到 5 天 / 系统时钟被拨回去)。
pub fn web_session_hint(at: Option<i64>, now: i64) -> Option<String> {
    let at = at?;
    let days = (now - at).max(0) / 86_400;
    (days >= 5).then(|| {
        format!("已使用 {days} 天 · 平台会话实测约 7 天过期;失效后点「重新登录」即可,历史数据不丢")
    })
}

/// 页面落在"登录完成后"的路径上吗?
///
/// 主机名做**整串相等**比较,不是后缀匹配:`www.workbuddy.cn.attacker.example/profile`
/// 这种拼接必须挡掉,否则一个长得像的站点就能让挂件以为登录完成了。
pub fn is_post_login(url: &str) -> bool {
    site_post_login(url, &SITE_HOSTS, &POST_LOGIN_PATHS)
}

/// `is_post_login` 的一般化:主机名整串相等 + 路径在前缀表里(只认 https)。
fn site_post_login(url: &str, hosts: &[&str], paths: &[&str]) -> bool {
    // 只认 https:会话 cookie 落在 http 页上等于把会话走明文,而且 http 的重定向页
    // 不该被当成"登录完成"(报告 P2-16 的次要点)
    if !url.starts_with("https://") {
        return false;
    }
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
    if !hosts.iter().any(|s| host.eq_ignore_ascii_case(s)) {
        return false;
    }
    let path = tail.split(|c| c == '?' || c == '#').next().unwrap_or("");
    paths.iter().any(|p| path.starts_with(p))
}

// ───────────── Qoder 网页会话(2026-10-30)─────────────
//
// 排查结论(docs/product/product-qoder-credit-expiry-2026-10-30.md):Qoder 的
// 逐包到期明细只在 qoder.com 的网页会话后面 —— 桌面 Bearer 能过 openapi 网关,
// 但那个网关只有汇总;账务明细的 /api/v1 端点与桌面网关是两套后端。
// 这套会话**只用于账务只读查询**,签到仍走桌面 Bearer,两条链路互不牵连。

/// Qoder 网页站(国际版)。国内版(qoder.com.cn)是另一套账号,与桌面端的两区划分一致,
/// P0 只接国际版 —— 用户给的用量页就是它。
pub const QODER_SITE_HOST: &str = "qoder.com";

/// 登录完成后落地的路径。没登录访问 /account/usage 会被 302 到
/// /users/sign-in?oauth_callback=…(2026-10-30 实测),所以"落在 /account/usage"
/// 就是"服务端认了这份会话"的证据 —— 与 WorkBuddy 同一条判据思路。
pub const QODER_POST_LOGIN_PATHS: [&str; 1] = ["/account/usage"];

/// 网页会话验证通过时刻的 settings 键(挂件外壳在登录成功时写,unix 秒字符串)。
pub const QODER_WEB_SESSION_AT_KEY: &str = "qoder_web_session_at";

/// 会话的年龄提示。Qoder 平台的会话 TTL 没有实测数据(等真机用出第一份再说),
/// 先沿用 WorkBuddy 的"第 5 天起提示"窗口,措辞里不给具体天数 —— 不编没有证据的数字。
pub fn qoder_web_session_hint(at: Option<i64>, now: i64) -> Option<String> {
    let at = at?;
    let days = (now - at).max(0) / 86_400;
    (days >= 5).then(|| {
        format!("网页会话已使用 {days} 天 · 平台会话会过期,失效后点「登录网页版」重登即可,历史数据不丢")
    })
}

/// 从 webview 的 Cookie 罐里挑出 Qoder 的会话,拼成 `cookie:` 前缀的凭据。
///
/// 与 [`cookie_credential`] 不同:Qoder 的会话 Cookie 名单是平台私有的
/// (httpOnly,公开 bundle 里看不到名单),**不按名字挑** —— 罐里属于本域的
/// 全部 Cookie 都拼上,浏览器同源请求本来就带全部匹配 Cookie,这里照抄浏览器行为。
/// 拿到的会话能不能真取到数,由调用方探测(真打一次账务接口)负责 —— 没登录的
/// 访客也可能有几枚营销 Cookie,拼出来不算数。
pub fn qoder_cookie_credential<I, S>(jar: I) -> Option<String>
where
    I: IntoIterator<Item = (S, S)>,
    S: AsRef<str>,
{
    let mut out = String::new();
    for (name, value) in jar {
        let (name, value) = (name.as_ref(), value.as_ref());
        // 脏键值一律跳过:控制字符/空格写进请求头会把整条头拆坏,甚至注入新头
        if name.is_empty()
            || value.is_empty()
            || name.chars().any(|c| c.is_control() || c == ' ' || c == '=')
            || value.chars().any(|c| c.is_control() || c == ' ')
        {
            continue;
        }
        if !out.is_empty() {
            out.push_str("; ");
        }
        out.push_str(name);
        out.push('=');
        out.push_str(value);
    }
    if out.is_empty() {
        None
    } else {
        Some(format!("{COOKIE_PREFIX}{out}"))
    }
}

/// 页面落在"Qoder 登录完成后"的路径上吗?
pub fn qoder_is_post_login(url: &str) -> bool {
    site_post_login(url, &[QODER_SITE_HOST], &QODER_POST_LOGIN_PATHS)
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

    /// 会话年龄提示:没记录不说话、前 4 天闭嘴、第 5 天起说话、时钟倒拨不造谣。
    #[test]
    fn web_session_hint_window() {
        let now = 1_792_000_000_i64;
        // 没记录 / 时钟被拨回(未来时刻):不提示
        assert_eq!(web_session_hint(None, now), None);
        assert_eq!(web_session_hint(Some(now + 3_600), now), None);
        // 新鲜(第 0~4 天):不提示
        assert_eq!(web_session_hint(Some(now), now), None);
        assert_eq!(web_session_hint(Some(now - 4 * 86_400), now), None);
        // 第 5 天起:提示,且天数随年龄增长;再过 2 天还是同一把钥匙的另一条文案
        let h5 = web_session_hint(Some(now - 5 * 86_400), now).unwrap();
        assert!(h5.contains("已使用 5"), "{h5}");
        let h9 = web_session_hint(Some(now - 9 * 86_400), now).unwrap();
        assert!(h9.contains("已使用 9"), "{h9}");
        // 过期很久也如实报(取数失败那行会把"重新登录"指出来,这里只说年龄)
        let h30 = web_session_hint(Some(now - 30 * 86_400), now).unwrap();
        assert!(h30.contains("已使用 30"), "{h30}");
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

    /// Qoder:罐里全部 Cookie 都拼上(名单是平台私有的,不按名字挑),
    /// 脏键值跳过,空罐不算会话。
    #[test]
    fn qoder_credential_takes_all_cookies() {
        let jar = vec![
            ("gk", "x"),
            ("t", "AAA"),
            ("Hm_lpvt_x", "1"),
            ("bad name", "v"),   // 键里有空格 → 跳过
            ("bad\nkey", "v"),   // 控制字符 → 跳过
            ("ok", ""),          // 空值 → 跳过
            ("eq=in", "v"),      // 键里有等号会拆坏键值对 → 跳过
        ];
        assert_eq!(
            qoder_cookie_credential(jar).as_deref(),
            Some("cookie:gk=x; t=AAA; Hm_lpvt_x=1")
        );
        assert_eq!(qoder_cookie_credential(Vec::<(&str, &str)>::new()), None);
        assert_eq!(qoder_cookie_credential(vec![("bad name", "v")]), None);
        // 值里的空格也不放行(拆坏头部)
        assert_eq!(qoder_cookie_credential(vec![("t", "A B")]), None);
    }

    /// Qoder 登录完成判定:落在 /account/usage 才算,登录页与别的路径不算,
    /// 变形域名 / userinfo / http 全挡。
    #[test]
    fn qoder_post_login_paths() {
        assert!(qoder_is_post_login("https://qoder.com/account/usage"));
        assert!(qoder_is_post_login("https://qoder.com/account/usage?tab=1"));
        assert!(qoder_is_post_login("https://Qoder.com:443/account/usage"));
        // 302 目标(没登录)与首页都不算
        assert!(!qoder_is_post_login(
            "https://qoder.com/users/sign-in?oauth_callback=https%3A%2F%2Fqoder.com%2Faccount%2Fusage"
        ));
        assert!(!qoder_is_post_login("https://qoder.com/"));
        assert!(!qoder_is_post_login("https://qoder.com/pricing"));
        // 变形域名 / userinfo / http
        assert!(!qoder_is_post_login("https://qoder.com.attacker.example/account/usage"));
        assert!(!qoder_is_post_login("https://qoder.com@evil.example/account/usage"));
        assert!(!qoder_is_post_login("http://qoder.com/account/usage"));
    }

    /// Qoder 会话年龄提示:没记录不说话、前 4 天闭嘴、第 5 天起说话;
    /// 措辞不给具体 TTL(平台没有实测数据,不编)。
    #[test]
    fn qoder_session_hint_window() {
        let now = 1_793_000_000_i64;
        assert_eq!(qoder_web_session_hint(None, now), None);
        assert_eq!(qoder_web_session_hint(Some(now + 60), now), None);
        assert_eq!(qoder_web_session_hint(Some(now - 4 * 86_400), now), None);
        let h = qoder_web_session_hint(Some(now - 6 * 86_400), now).unwrap();
        assert!(h.contains("已使用 6"), "{h}");
        assert!(!h.contains("7 天"), "不给没证据的 TTL:{h}");
    }
}
