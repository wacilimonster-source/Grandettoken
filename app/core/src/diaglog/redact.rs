/// 令牌的**可区分摘要**:长度 + sha256 前 8 位。
///
/// 排查时真正要回答的是"用的是哪一份令牌、换没换、长度对不对"(比如
/// `desktop:` 前缀丢了、Cookie 只剩半截),这些都不需要原文。
pub fn secret_brief(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let t = s.trim();
    if t.is_empty() {
        return "len=0".to_string();
    }
    let d = Sha256::digest(t.as_bytes());
    let fp: String = d.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("len={} fp={fp}", t.chars().count())
}

/// 兜底脱敏:把"长得像令牌/会话值"的串掩掉,只留长度。
///
/// 判据(2026-10-21 重做,原版有两个洞):
///
/// * **JWT 不遮** —— 原判据是"含 <code>.</code> 的串当域名放过",而 JWT 天然带两个点。
///   现在 >=2 个点、每段 >=8 字符、无 <code>/</code> 的长串**一律遮**。
/// * **全小写的长串不遮** —— 原判据要求"有数字或大写",base64url 令牌可能两样都没有。
///   现在只要不是路径/域名形态就遮。
///
/// 但**路径、域名、文件名、UUID、错误码必须留着** —— 它们才是排查要用的信息。
/// 真机日志里 `https://api.trae.cn/...` 曾被当成令牌掩成
/// `https://api.trae.«secret len=48»`,把"用的哪个接口"这条最有用的信息弄丢了。
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.is_empty() {
            return;
        }
        if is_secret_run(run) {
            out.push_str(&format!("«secret len={}»", run.chars().count()));
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for ch in text.chars() {
        // ⚠ 运行字符集**不含 `-`**:UUID(`5f2b9c1e-3a4d-…`)要能被拆成短段而保留,
        // 而键名里的连字符由 `redact_keyed_values` 单独扫原文处理,不依赖这里。
        if ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '=' | '_' | '.') {
            run.push(ch);
        } else {
            flush(&mut run, &mut out);
            out.push(ch);
        }
    }
    flush(&mut run, &mut out);
    // 键值形态的会话值单独再挡一层:`session=<值>` / `{"accessToken":"…"}` 这种
    // 可能不到 32 字符,但同样是凭据。
    redact_keyed_values(&out)
}

/// 一段连续 base64/hex 字符算不算"令牌"。
///
/// 保守优先:**漏掩是事故,误掩只是少一条线索**。所以只有"明确是位置信息"的形态才放过。
fn is_secret_run(s: &str) -> bool {
    let n = s.chars().count();
    if n < 32 {
        return false;
    }
    // ① JWT 特征:>=2 个点、每段 >=8 字符、不是路径 —— 一定是令牌
    if s.matches('.').count() >= 2 && !s.contains('/') && jwt_like(s) {
        return true;
    }
    // ② 含 '/' 的长串是路径 / URL —— 位置信息,要留
    if s.contains('/') {
        return false;
    }
    // ③ 短扩展名结尾的是域名 / 文件名(`api.trae.cn`、`workbuddy-desktop.info`)
    if looks_like_name(s) {
        return false;
    }
    // ④ 其余长串:一律当令牌(含全小写的 base64url)
    true
}

/// 是不是 JWT:`a.b.c`,每段 >= 8 字符,只含 base64url 字符。
fn jwt_like(s: &str) -> bool {
    let segs: Vec<&str> = s.split('.').collect();
    segs.len() >= 3
        && segs.iter().all(|g| {
            g.len() >= 8
                && g.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        })
}

/// 末段是短扩展名 / 顶级域 → 是域名或文件名,不是令牌。
///
/// `api.trae.cn`(cn)、`workbuddy-desktop.info`(info)、`WorkBuddy.exe`(exe) 都命中;
/// JWT 的末段是 40+ 个混合大小写字符,不命中。
fn looks_like_name(s: &str) -> bool {
    match s.rsplit('.').next() {
        Some(last) => {
            let l = last.chars().count();
            l >= 2 && l <= 6 && last.chars().all(|c| c.is_ascii_alphabetic())
        }
        None => false,
    }
}

/// 键名归一:小写 + 去掉分隔符。`accessToken` / `access_token` / `X-Api-Key` 归一到同一个。
fn norm_key(k: &str) -> String {
    k.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// 会话值 / 密钥的键名表。归一后比对,大小写和分隔符都无所谓。
const SECRET_KEYS: &[&str] = &[
    "accesstoken",
    "refreshtoken",
    "idtoken",
    "authorization",
    "apikey",
    "xapikey",
    "apisecret",
    "clientsecret",
    "token",
    "secret",
    "password",
    "passwd",
    "pwd",
    "cookie",
    "cookies",
    "session",
    "session2",
    "sessionid",
    "sid",
    "csrf",
    "xsrf",
    "credential",
    "credentials",
    "atrestsecretkey",
    "privatekey",
];

/// 前缀形态:`Bearer xxx` / `Basic xxx`。不是 key=value,是"关键字 + 空格 + 值"。
const SECRET_PREFIXES: &[&str] = &["bearer ", "basic ", "token ", "apikey "];

/// `key=value` / `"key": "value"` / `?key=value` 的值掩掉。
///
/// 原实现只认六种 `key=` 形态,JSON 的 `"accessToken":"…"` 根本匹配不上,
/// 而且值扫到 `"` 就停 —— 等于没遮。现在按归一后的键名表匹配,值按引号感知地扫。
fn redact_keyed_values(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < bytes.len() {
        // 先看前缀形态(Bearer / Basic)
        if let Some(p) = SECRET_PREFIXES.iter().find(|p| {
            bytes[i..].get(..p.len()).is_some_and(|head| head.eq_ignore_ascii_case(p.as_bytes()))
        }) {
            let p_len = p.len();
            out.push_str(&text[i..i + p_len]);
            let vstart = i + p_len;
            let vend = scan_value_end(text, vstart, false);
            let val = &text[vstart..vend];
            if val.is_empty() {
                // 只有前缀没有值,原样保留
            } else {
                out.push_str(&format!("«secret len={}»", val.chars().count()));
            }
            i = vend;
            continue;
        }
        // 再看 key=value / "key": "value" / ?key=value
        if let Some((_klen, quote, vstart)) = match_keyed(text, i) {
            out.push_str(&text[i..vstart]);
            let vend = scan_value_end(text, vstart, quote);
            let val = &text[vstart..vend];
            if val.is_empty() {
                // 只有键没有值,原样保留(比如 `cookie:` 后面跟的是说明文字)
            } else {
                out.push_str(&format!("«secret len={}»", val.chars().count()));
            }
            i = vend;
            continue;
        }
        let ch = text[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 从 `i` 开始,能不能读出一个"敏感键名 + 分隔符 + 值开头"。
///
/// 返回 `(键名长度, 值是否被引号包着, 值起点)`。
fn match_keyed(text: &str, i: usize) -> Option<(usize, bool, usize)> {
    let b = text.as_bytes();
    // 允许键名带引号:`"accessToken": "…"`
    let (kstart, quoted) = if b.get(i) == Some(&b'"') {
        (i + 1, true)
    } else {
        (i, false)
    };
    // 键名:字母数字下划线横线
    if !quoted && !b.get(kstart)?.is_ascii_alphanumeric() { return None; }
    static MAX_KEY: std::sync::LazyLock<usize> =
        std::sync::LazyLock::new(|| SECRET_KEYS.iter().map(|key| key.len()).max().unwrap_or(0));
    let mut letters = 0;
    let mut kend = kstart;
    while kend < b.len()
        && (b[kend].is_ascii_alphanumeric() || b[kend] == b'_' || b[kend] == b'-')
    {
        if b[kend].is_ascii_alphanumeric() {
            letters += 1;
            if letters > *MAX_KEY {
                return None;
            }
        }
        kend += 1;
    }
    if kend == kstart {
        return None;
    }
    let name = norm_key(&text[kstart..kend]);
    if !SECRET_KEYS.contains(&name.as_str()) {
        return None;
    }
    // 键名后面:可选的闭引号 + 空白 + `=` 或 `:`
    let mut j = kend;
    if quoted {
        if b.get(j) != Some(&b'"') {
            return None;
        }
        j += 1;
    }
    while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
        j += 1;
    }
    if !matches!(b.get(j), Some(b'=') | Some(b':')) {
        return None;
    }
    j += 1;
    while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
        j += 1;
    }
    // ⚠ **必须跳过认证方案前缀**(`Bearer ` / `Basic ` …)。
    // 不跳的话,`Authorization: Bearer abc123` 会被当成"键 authorization 的值是
    // Bearer" → 掩掉的是 "Bearer" 六个字母,**真正的令牌原样留下**。
    // 2026-10-21 真的漏过这一条(旧的 KEYS 表里有 "bearer ",改写时把这个口径丢了)。
    for p in ["bearer ", "basic ", "token ", "digest ", "negotiate "] {
        if text
            .get(j..j + p.len())
            .map_or(false, |s| s.eq_ignore_ascii_case(p))
        {
            j += p.len();
            break;
        }
    }
    // 值是否被引号包着
    let vquote = b.get(j) == Some(&b'"') || b.get(j) == Some(&b'\'');
    let vstart = if vquote { j + 1 } else { j };
    let klen = vstart - i;
    Some((klen, vquote, vstart))
}

/// 从 `start` 开始扫到值结束。
///
/// 引号包裹时,扫到**配对的**闭引号为止(不把闭引号吃进值里)。
/// 没引号时,扫到分隔符为止(空格 / `;` / `,` / `&` / 换行 / 闭引号)。
fn scan_value_end(text: &str, start: usize, quoted: bool) -> usize {
    let b = text.as_bytes();
    if quoted {
        let q = b[start - 1];
        let mut j = start;
        while j < b.len() && b[j] != q {
            j += 1;
        }
        return j;
    }
    let mut j = start;
    while j < b.len() && !matches!(b[j], b';' | b' ' | b'\t' | b'\r' | b'\n' | b'"' | b',' | b'&' | b'\'') {
        j += 1;
    }
    j
}
