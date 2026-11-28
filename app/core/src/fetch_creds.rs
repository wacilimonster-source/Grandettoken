use super::*;

/// 当前这把密钥的指纹(曲线上只画"现在这本账";换 key 后旧账不进曲线)。
/// 非 Keyring 型的凭据会轮换(网页会话/本机登录态),一律空串 = 不分段。
/// **阻塞**(要读凭据管理器),只能在非主线程上调。
pub fn current_key_fp(def: &providers::ProviderDef) -> String {
    if !matches!(def.auth, AuthKind::Keyring) {
        return String::new();
    }
    // 走 resolve 而不是 get:引用模式的渠道,值在密钥箱里(设计稿 product-vault-channel-bridge)
    match secrets::resolve(def.id).map(|r| r.key().map(|k| k.to_string())) {
        Ok(Some(k)) if !k.trim().is_empty() => key_fp(&k),
        _ => String::new(),
    }
}

/// 密钥指纹:同一把 API Key 恒定,换 key 就换一本账(见 store::record_fp)。
///
/// 为什么用 sha256 前 12 字节的十六进制:
///   ① 它只用来给快照分段,不承担任何认证职责,截断够用;
///   ② 不落明文也不可逆(与"密钥只进凭据管理器"同一条纪律);
///   ③ 稳定 —— 换台机器同一把 key 也是同一段。
pub fn key_fp(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.trim().as_bytes());
    let d = h.finalize();
    d.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

/// Prefer an explicit stable account ID; never infer an account from a display name.
pub(super) fn credential_scope(c: &appauth::Credential) -> String {
    account_scope(&c.token, c.account_id.as_deref())
}

pub(super) fn account_scope(token: &str, account: Option<&str>) -> String {
    match account.filter(|id| !id.is_empty()) {
        Some(id) => key_fp(&format!("account:{id}")),
        None => key_fp(&format!("token:{token}")),
    }
}

/// 余额凭据的 scope 落在 settings 里的键(按渠道)。值只是指纹,不是密钥。
pub const QODER_SCOPE_KEY: &str = "qoder_cred_scope_v1";
/// 本渠道最近一次换余额凭据的时刻(unix 秒)。它与网页会话登录时刻一比,
/// 就知道浏览器里那份 Cookie 是不是上一本账留下的。外壳的登录命令也读它,
/// 所以是 `pub`(键名必须与取数侧一致,错一个字符就判不出换号)。
pub const QODER_CHANGED_KEY: &str = "qoder_account_changed_at";

/// 记下本轮余额凭据的 scope,返回"是否换了凭据(账号 / 令牌)"。
///
/// 上一本账优先读专门的设置键;升级上来的用户还没有那个键,就回退读上一次成功
/// 快照里的 `scope` —— 这样"换令牌之后第一次取数"就能发现,不必等旧网页列表
/// 再错一轮。第一次运行(两者都没有)按"没换过"处理,绝不误杀。
pub(super) fn note_qoder_scope(store: &Store, id: &str, scope: &str) -> bool {
    if scope.is_empty() {
        return false;
    }
    let key = format!("{QODER_SCOPE_KEY}:{id}");
    let prev = store
        .get_setting(&key)
        .ok()
        .flatten()
        .or_else(|| Snapshot::stored_scope(store, id));
    let changed = prev.as_deref().map(|p| p != scope).unwrap_or(false);
    if changed {
        // 写失败不能吞:换号时刻是"网页会话是否属于上一本账"的唯一判据,
        // 没落盘就等于判据回到旧值,下一轮还会按旧账处理。GUI 里没人看 stderr,
        // 所以进诊断日志,出问题时能用 diag.log 复现判定链。
        if let Err(e) = store.set_setting(&format!("{QODER_CHANGED_KEY}:{id}"), &now_ts().to_string()) {
            crate::diaglog::write_keyed(id, "qoder-scope", &format!("换号时刻写入失败: {e}"));
        }
    }
    if let Err(e) = store.set_setting(&key, scope) {
        crate::diaglog::write_keyed(id, "qoder-scope", &format!("凭据指纹写入失败: {e}"));
    }
    changed
}

/// 网页会话是否属于上一本账(判定本身在 `core::weblogin`,这里只负责读两个时刻)。
pub(super) fn qoder_web_session_stale(store: &Store, id: &str, session_at_key: &str) -> bool {
    let read_ts = |key: &str| {
        store
            .get_setting(key)
            .ok()
            .flatten()
            .and_then(|s| s.trim().parse::<i64>().ok())
    };
    weblogin::qoder_web_session_stale(
        read_ts(&format!("{QODER_CHANGED_KEY}:{id}")),
        read_ts(session_at_key),
    )
}

/// 被服务器拒过的凭据降权多久。**不是永久**:用户重新登录、平台侧恢复、
/// 或者那次 401 只是瞬时抖动,过了这段时间它就回到原位再试一次。
pub(super) const REJECTED_TTL: i64 = 30 * 60;

/// 把"被 401/403 拒过"的凭据排到后面,其余保持原顺序。
///
/// 为什么需要:手动粘贴的令牌排在桌面端凭据之前(用户明确填的东西优先),
/// 而一份**早已作废**的手动令牌会让每轮都先白打一个 401。修好"401 要换下一份"
/// 之后已经能取到数,但那份废令牌仍会在每轮开头拖一个往返 —— 真机日志里
/// 用户就是这么被卡了三天(2026-10-20)。
///
/// **纯函数**(时间与拒因都由参数给),单测能钉住"降权而非丢弃"这条语义。
pub(super) fn demote_rejected(
    creds: Vec<appauth::Credential>,
    rejected: &HashMap<String, i64>,
    now: i64,
    fp_of: impl Fn(&str) -> String,
) -> Vec<appauth::Credential> {
    let fresh = |c: &appauth::Credential| {
        rejected
            .get(&fp_of(&c.token))
            .map(|at| now - *at >= REJECTED_TTL)
            .unwrap_or(true)
    };
    let mut ok: Vec<appauth::Credential> = Vec::new();
    let mut bad: Vec<appauth::Credential> = Vec::new();
    for c in creds {
        if fresh(&c) {
            ok.push(c);
        } else {
            bad.push(c);
        }
    }
    // 被拒的全部沉底:顺序在各自组内保持不变
    ok.extend(bad);
    ok
}

/// `None` 全部(国内版优先,其次国际版,再其次历史备份 —— 签到侧逐份试的旧口径)。
///
/// 为什么给全部:同一台机器可能同时登着国内版与国际版(两套凭据各自有效),
/// 而历史备份里的 token 可能已被上游作废(未过期但 401)—— 上层按 401 换下一份,
/// 比直接让用户重新登录更省事。
///
/// 分区不是优化:国际版 token 打国区域名一定 401(反之亦然),两个渠道混用同一份
/// 列表会让每轮先失败一次,真正的凭据失效被 `cred-switch` 噪音埋掉。
pub(super) fn read_desktop_tokens_region(region: Option<bool>, cycle: Option<&crate::workbuddy_cycle::Credentials>) -> Vec<String> {
    if cfg!(test) {
        // 单测绝不能真去碰本机凭据:会让测试依赖本机装了什么,还会把并发测试拖慢
        // (2026-10-19 就因此让两条无关用例挂掉)。v0.3.36 起测试隔离只靠这个早退,
        // 不再保留可注入的 DESKTOP_READER 钩子(旧注入链无人使用,已删)。
        return Vec::new();
    }
    let now_ms = chrono::Local::now().timestamp_millis();
    // desktop_tokens_region 内部已经处理:上次续期的成果 → 桌面端文件 → 过期时自动续期
    cycle.map_or_else(|| crate::workbuddy::desktop_tokens_region(now_ms, region), |c| c.tokens(now_ms, region))
        .into_iter()
        .map(|t| format!("{}{}", DESKTOP_TOKEN_PREFIX, t))
        .collect()
}

/// 只吃桌面端凭据、没有网页登录入口的渠道(现在只有 WorkBuddy AI 国际版)。
///
/// 为什么单拎一个判据而不是再加一个 `AuthKind`:它借用 `WebSession` 的**凭据链路**
/// (桌面端令牌 + `desktop:` 前缀 + 桌面端 UA),但**界面**要按 app 型渲染
/// (没有「重新登录」按钮、没有粘贴框)。两件事分开表达,比新增一个枚举分支
/// 去改 43 处 match 更不容易出错。
pub(super) fn wb_desktop_only(id: &str) -> bool {
    id == "workbuddy-ai"
}

/// 把账户号集合压成稳定的分段指纹:**这里**负责排序去重 —— 调用方给什么顺序都行
/// (`demote_rejected` 每轮可能重排候选,指纹不能跟着抖)。空集合 → 空串(不分段的老口径)。
pub(super) fn accounts_fp(uins: &[String]) -> String {
    let mut sorted: Vec<&String> = uins.iter().filter(|u| !u.is_empty()).collect();
    sorted.sort();
    sorted.dedup();
    if sorted.is_empty() {
        String::new()
    } else {
        key_fp(&sorted.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(","))
    }
}

/// 本轮"没凭据就自动隐藏"的候选渠道。
///
/// 两个条件缺一不可:① 渠道在 `providers::auto_hide_when_no_credential` 名单里
/// (现在只有国际版 —— 没装就等于不存在);② 用户**没动过眼睛**(动过就是表过态,
/// 之后一律听用户的)。
///
/// 注意这里只决定"候选":真正藏不藏还要看本轮有没有读到凭据(见 `fetch_all`)。
pub fn auto_hide_candidates(cfg: &Config) -> Vec<String> {
    providers::PROVIDERS
        .iter()
        .map(|p| p.id)
        .filter(|id| providers::auto_hide_when_no_credential(id))
        .filter(|id| !cfg.user_touched_channels.iter().any(|t| t == id))
        .map(str::to_string)
        .collect()
}

/// 取一个渠道的凭据候选(按可信度排序)。空的第一个返回值 = 没有可用凭据;
/// 第二个返回值 = 凭据管理器本身坏了的说明文字(与"没配密钥"区分,P2-12)。
///
/// `web_src` = 网页登录型渠道的会话来源(外壳注入的"现从 webview 的 Cookie 罐里取",
/// 见 Fetcher::set_web_credential_source)。顺序上**手动粘贴的密钥排在它前面**:
/// 用户明确填进去的东西优先,填错了下一轮就会退回来用宿主的会话(401 时换候选)。
pub(super) fn credentials(
    def: &providers::ProviderDef,
    web_src: Option<&(dyn Fn(&str) -> Option<String> + Send + Sync)>,
    cycle: Option<&crate::workbuddy_cycle::Credentials>,
) -> (Vec<appauth::Credential>, Option<String>) {
    match def.auth {
        // Keyring 与 WebSession 都躺在 Windows 凭据管理器里;网页登录型还可能有一份
        // "宿主浏览器的会话"(不落盘)。
        AuthKind::Keyring | AuthKind::WebSession => {
            let mut out: Vec<appauth::Credential> = Vec::new();
            let mut err = None;
            match secrets::resolve(def.id) {
                Ok(secrets::Resolved::Inline(k)) | Ok(secrets::Resolved::Vault { key: k, .. }) => {
                    out.push(appauth::Credential {
                        token: k,
                        source: String::new(),
                        account_id: None,
                        device_id: None,
                    })
                }
                Ok(secrets::Resolved::None) => {}
                // 断链:引用还在但箱子那条被删了。**发不了请求是必然的**,但不能一声不吭。
                //
                // N06(2026-10-21):以前这里什么都不说,于是兜底文案变成"还没填 XX 的密钥"——
                // 而"断链"和"没配"该去的地方**正好相反**:前者去密钥箱看,后者去渠道卡填。
                // 用户照着错的话去渠道卡重填,填完仍然不通(引用还在、优先级更高)。
                Ok(secrets::Resolved::Dangling(vid)) => {
                    err = Some(format!(
                        "引用的密钥箱条目已被删除(条目 id={vid})—— 去「密钥箱」检查,或在这里重新选一把"
                    ));
                }
                Err(e) => err = Some(e),
            }
            // WorkBuddy:**桌面端凭据优先**。它比网页会话活得久(accessToken 28 天、
            // refreshToken 30 天,且可续),而网页会话是约 7 天的绝对 TTL ——
            // 2026-10-15 之后连续三天取不到数,就是网页会话到期、而界面仍报"已登录"。
            // 读不到桌面端凭据不是错误(没装桌面端很常见),静默跳过、落到网页会话。
            //
            // 两个区域各取各的(2026-10-23):`workbuddy` 只吃国内版令牌,
            // `workbuddy-ai` 只吃国际版令牌。混着给会让每轮先用错区令牌打一次 401。
            if def.id == "workbuddy" || wb_desktop_only(def.id) {
                let region = Some(wb_desktop_only(def.id));
                let src = if wb_desktop_only(def.id) {
                    "WorkBuddy AI 桌面端"
                } else {
                    "WorkBuddy 桌面端"
                };
                for tok in read_desktop_tokens_region(region, cycle) {
                    out.push(appauth::Credential {
                        token: tok,
                        source: src.into(),
                        account_id: None,
                        device_id: None,
                    });
                }
            }
            // 网页会话只属于国内版:国际版被 weblogin 的裁决 6A 排除在外,
            // 对 wb_desktop_only 的渠道去问会话罐,只会得到一个永远空的候选。
            if matches!(def.auth, AuthKind::WebSession) && !wb_desktop_only(def.id) {
                if let Some(session) = web_src.and_then(|f| f(def.id)) {
                    out.push(appauth::Credential {
                        token: session,
                        source: String::new(),
                        account_id: None,
                        device_id: None,
                    });
                }
            }
            (out, err)
        }
        AuthKind::App(appauth::App::Antigravity) => crate::antigravity::app_credentials(),
        // 本机应用的登录态:可能有几份(多个安装 / 旧备份),按可信度依次试
        AuthKind::App(app) => (appauth::candidates(app), None),
    }
}

/// 服务器明确说"这份凭据不行"(401/403)时的用户指引。密钥型返回 None ——
/// 那是密钥本身错了,由 HTTP 码/业务响应自己说话,不能瞎指"重新登录"。
///
/// 两条指引的方向不一样,别混:本机登录型渠道(Trae/Codex)去开一次客户端就行;
/// 网页登录型(WorkBuddy)**不能**让它去开桌面客户端 —— 桌面端 5.6.2 起把本机
/// 凭据文件里的 accessToken 改成加密信封(2026-10-02 取证),开多少次都读不到,
/// 只有挂件里那次网页登录有用。
pub fn expired_credential_msg(def: &providers::ProviderDef) -> Option<String> {
    match def.auth {
        AuthKind::App(app) => Some(format!("登录状态已失效 · 打开一次 {} 即可", app.label())),
        // 国际版只有桌面端一条路,没有「重新登录」按钮可点 —— 别把它指去网页登录。
        AuthKind::WebSession if wb_desktop_only(def.id) => Some(
            "WorkBuddy AI 凭据失效 · 打开一次国际版桌面端即可(它会自动续期)".into(),
        ),
        // WorkBuddy 现在有两条路:桌面端凭据(可续期)与网页会话(约 7 天绝对 TTL)。
        // 桌面端那条自己会刷新,所以先指向它;网页会话那条按老文案说清"7 天一次"。
        AuthKind::WebSession if def.id == "workbuddy" => Some(
            "WorkBuddy 凭据失效 · 打开一次桌面端(它会自动续期),或在本卡片点「重新登录」".into(),
        ),
        AuthKind::WebSession => Some(
            "网页登录已过期(平台会话约 7 天一次,属正常) · 在渠道卡上点「重新登录」即可".into(),
        ),
        AuthKind::Keyring => None,
    }
}

/// 凭据来源的说明文字,直接显示在界面上。
pub(super) fn auth_label(def: &providers::ProviderDef, cred: Option<&appauth::Credential>) -> String {
    match def.auth {
        AuthKind::Keyring => "Windows 凭据管理器".into(),
        // 网页登录:不碰本机客户端的任何文件,也不写自己的凭据库 —— 会话就住在
        // 挂件的浏览器 profile 里(登录窗那个 webview 与主窗口共用一份)。
        // 三种形态分开报:桌面端凭据 / 浏览器会话 / 用户自己粘的令牌。
        AuthKind::WebSession => match cred {
            Some(c) if c.token.trim().starts_with(DESKTOP_TOKEN_PREFIX) => {
                if wb_desktop_only(def.id) {
                    // 界面上「已读取本机登录 · 国际版桌面端凭据(自动续期)」
                    "国际版桌面端凭据(自动续期)".into()
                } else {
                    "WorkBuddy 桌面端凭据(自动续期)".into()
                }
            }
            Some(c) if weblogin::cookie_header(&c.token).is_some() => "浏览器网页会话".into(),
            Some(_) => "手动粘贴的密钥".into(),
            // 国际版没有网页登录可去,措辞跟 app 型渠道一致
            None if wb_desktop_only(def.id) => format!("未检测到 {} 登录信息", def.name),
            None => format!("未登录 {} 账号", def.name),
        },
        AuthKind::App(app) => match cred {
            Some(c) => {
                let label = app.label();
                let raw = c.source.trim_end_matches(".info").trim();
                if raw.is_empty() {
                    format!("本机 {label}")
                } else {
                    // Qoder 的 source 自带区域名 + 账号名(见 appauth::qoder_candidates:
                    // "Qoder · sebird" / "Qoder 国内版 · 里怪兽"),而这里已经先拼了 label,
                    // 直接接上会显示成「Qoder · Qoder · sebird」(2026-11-10 真机截图)。
                    // 展示层剥掉这段重复前缀即可;source 原值留在诊断日志里不动。
                    // 只认「整段相等」或「label + 分隔符」,否则 "Qoder" 会误吞
                    // 以它开头的 "Qoder 国内版 · …"(国际版标签是国内版标签的前缀)。
                    let sep = format!("{label} · ");
                    let src = if raw == label {
                        ""
                    } else if let Some(rest) = raw.strip_prefix(&sep) {
                        rest.trim()
                    } else {
                        raw
                    };
                    // Trae("TRAE SOLO CN")/Codex("CLI 登录态")的 source 是安装目录或凭据形态,
                    // 与 label 不是同一信息,保持「Trae · TRAE SOLO CN」这类两段式。
                    if src.is_empty() {
                        label.to_string()
                    } else {
                        format!("{label} · {src}")
                    }
                }
            }
            None => format!("未检测到 {} 登录信息", app.label()),
        },
    }
}

/// 凭据候选的**可读标签**(诊断日志用)。只描述形态与来源,不含令牌本身。
pub(super) fn cred_desc(def: &providers::ProviderDef, c: &appauth::Credential) -> String {
    let kind = if c.token.trim().starts_with(DESKTOP_TOKEN_PREFIX) {
        "桌面端"
    } else if weblogin::cookie_header(&c.token).is_some() {
        "网页会话"
    } else if matches!(def.auth, AuthKind::Keyring) {
        "密钥"
    } else {
        "手动令牌"
    };
    if c.source.is_empty() {
        kind.to_string()
    } else {
        format!("{kind}/{}", c.source)
    }
}

