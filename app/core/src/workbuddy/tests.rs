use super::*;
use base64::Engine as _;

/// 正常路径:子进程立刻退出,输出要能原样读回来 —— 改成 channel 收数据后不能丢。
#[cfg(windows)]
#[test]
fn child_output_is_read_back() {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", "echo hello"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().unwrap();
    let out = run_child_with_timeout(child, Duration::from_secs(10)).unwrap();
    assert_eq!(out, "hello");
}

/// 超时必须**真的返回**。2026-11-10 真机:原来 `thread::scope` 的 `join()` 会一直等
/// 管道 EOF,15 秒的 `KEY_TIMEOUT` 实际要 104~133 秒才回到调用方。
/// 这里用一个会挂 19 秒的子进程,断言 800ms 的超时能在 10 秒内返回。
#[cfg(windows)]
#[test]
fn child_timeout_returns_within_budget() {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", "ping -n 20 127.0.0.1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().unwrap();
    let started = Instant::now();
    let r = run_child_with_timeout(child, Duration::from_millis(800));
    assert!(r.is_err(), "子进程没退出时必须报超时");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "超时后必须尽快返回,实际等了 {:?}",
        started.elapsed()
    );
}

/// keyId 口径:sha256 前 8 字节的十六进制 —— 真机信封里的 `9127dea1b44020a7` 就是这么来的。
#[test]
fn key_id_is_sha256_prefix() {
    let key = Sha256::digest(b"whatever");
    let id = key_id_of(&key);
    assert_eq!(id.len(), 16);
    assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
}

/// AAD 转录必须逐字节稳定:改动一位,GCM 就会认证失败。
#[test]
fn aad_transcript_is_stable() {
    let aad = field_aad("9127dea1b44020a7", 1);
    // "WB-AAD\0" 是 7 字节(W B - A A D \0)—— 这里写错过一次,所以逐段断言
    assert_eq!(AAD_DOMAIN.len(), 7);
    assert!(aad.starts_with(b"WB-AAD\0"));
    assert_eq!(aad[7], 1, "版本字节紧跟域前缀");
    // 格式 id / 方案都是长度前缀
    assert_eq!(&aad[8..12], &5u32.to_be_bytes(), "WBEV1 长度");
    assert_eq!(&aad[12..17], b"WBEV1");
    assert_eq!(&aad[17..21], &6u32.to_be_bytes(), "sym-v1 长度");
    assert_eq!(&aad[21..27], b"sym-v1");
    assert_eq!(&aad[27..31], &1u32.to_be_bytes(), "suite");
    assert_eq!(&aad[31..35], &16u32.to_be_bytes(), "keyId 长度");
    assert_eq!(&aad[35..51], b"9127dea1b44020a7");
    assert_eq!(aad[51], 2, "framing code");
    assert_eq!(aad.len(), 54);
}

/// 明文凭据不需要 key;加密凭据没有 key 要明确报"加密但没密钥"。
#[test]
fn plaintext_and_encrypted_fields() {
    let plain = r#"{"auth":{"accessToken":"tok","domain":"www.codebuddy.cn","expiresAt":1794709711836},
                        "account":{"uin":330100933717}}"#;
    let c = parse_auth_file(plain, "x", None).unwrap();
    assert_eq!(c.access_token, "tok");
    assert_eq!(c.uin, "330100933717");
    assert!(!c.is_global());
    assert!(!c.expired(1_700_000_000_000));

    let enc = r#"{"auth":{"accessToken":{"$wbEncrypted":1,"envelope":"e30="}}}"#;
    let e = parse_auth_file(enc, "x", None).unwrap_err();
    assert!(matches!(e, DesktopErr::BadCredential(_)), "没有 key 时要说清:{e:?}");
}

/// keyId 对不上时必须报 KeyMismatch(而不是"解出来一堆乱码")。
#[test]
fn key_mismatch_is_explicit() {
    let env = serde_json::json!({
        "suite": 1, "keyId": "0000000000000000",
        "nonce": "AAAAAAAAAAAAAAAA", "authTag": "AAAAAAAAAAAAAAAAAAAAAA==",
        "ciphertext": "AAAA"
    });
    let wrapper = serde_json::json!({
        "$wbEncrypted": 1,
        "envelope": base64::engine::general_purpose::STANDARD.encode(env.to_string())
    });
    let key = Sha256::digest(b"some other key");
    let e = open_field(&wrapper, &key).unwrap_err();
    match e {
        DesktopErr::KeyMismatch { want, got } => {
            assert_eq!(want, "0000000000000000");
            assert_eq!(got, key_id_of(&key));
        }
        other => panic!("该报 KeyMismatch,实得 {other:?}"),
    }
}

/// 每档错误都要有一句能指导用户的文案(不能只说"失败")。
#[test]
fn every_error_has_actionable_text() {
    for e in [
        DesktopErr::AppNotFound,
        DesktopErr::KeyUnavailable("boom".into()),
        DesktopErr::KeyMismatch { want: "a".into(), got: "b".into() },
        DesktopErr::BadCredential("bad".into()),
        DesktopErr::Expired,
    ] {
        let m = e.message();
        assert!(m.len() > 10, "文案太短:{m}");
        assert!(
            m.contains("桌面端") || m.contains("重新登录") || m.contains("WORKBUDDY_APP_EXECUTABLE"),
            "文案要指对方向:{m}"
        );
    }
}

/// 请求体字段与上游逐字一致(尤其 ProductCode 与那 36.9 天的窗口)。
#[test]
fn billing_body_matches_upstream() {
    let now = chrono::Local::now();
    let b = billing_body(now);
    assert_eq!(b["PageNumber"], 1);
    assert_eq!(b["PageSize"], 100);
    assert_eq!(b["ProductCode"], "p_tcaca");
    assert_eq!(b["Status"], serde_json::json!([0, 3]));
    assert!(b["PackageEndTimeRangeBegin"].as_str().unwrap().contains('-'));
    assert!(b["PackageEndTimeRangeEnd"].as_str().unwrap().contains('-'));
}

/// 端点必须带 `/v2`:少一段就是 401/404(2026-10-19 实测)。
#[test]
fn endpoint_carries_v2() {
    assert!(BILLING_PATH.starts_with("/v2/"));
    assert!(CN_BILLING_BASE.contains("codebuddy.cn"));
}

/// 鉴权头:桌面端走 Bearer,网页会话走 Cookie,不能串。
#[test]
fn auth_headers_do_not_mix() {
    assert_eq!(FetchAuth::Bearer("t".into()).header_value(), "Bearer t");
    assert_eq!(FetchAuth::Cookie("session=a".into()).header_value(), "Cookie: session=a");
}

/// 续期契约:端点、头部字段名、以及"没有 refreshToken 就不发请求"。
#[test]
fn refresh_contract_matches_upstream() {
    assert_eq!(REFRESH_PATH, "/v2/plugin/auth/token/refresh");
    let cn = DesktopCredential {
        domain: "www.codebuddy.cn".into(),
        ..Default::default()
    };
    assert_eq!(chat_base(&cn), "https://copilot.tencent.com", "国内 chat 基址与计费不同域");
    let gl = DesktopCredential {
        domain: "www.workbuddy.ai".into(),
        ..Default::default()
    };
    assert_eq!(chat_base(&gl), "https://www.workbuddy.ai");
    // 没有 refreshToken → 明确拒绝,不发请求
    let http = reqwest::Client::new();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let e = rt.block_on(refresh(&http, &cn)).unwrap_err();
    assert!(e.contains("没有 refreshToken"), "{e}");
}

/// 续期结果的存取(凭据管理器按区域分开,互不覆盖)。
#[test]
fn refresh_slots_are_per_region() {
    assert_ne!(refresh_slot(true), refresh_slot(false));
    assert!(refresh_slot(false).contains("cn"));
    assert!(refresh_slot(true).contains("global"));
}

/// 区域基址:国内打 codebuddy.cn、国际打 workbuddy.ai —— 用错域名一定 401(真机踩过)。
#[test]
fn bases_follow_credential_region() {
    let cn = DesktopCredential { domain: "www.codebuddy.cn".into(), ..Default::default() };
    assert_eq!(bases_for(&cn)[0], "https://www.codebuddy.cn");
    let gl = DesktopCredential { domain: "www.workbuddy.ai".into(), ..Default::default() };
    assert_eq!(bases_for(&gl)[0], "https://www.workbuddy.ai");
    let cb = DesktopCredential { domain: "www.codebuddy.ai".into(), ..Default::default() };
    assert_eq!(bases_for(&cb)[0], "https://www.workbuddy.ai", "codebuddy.ai 属于国际栈");
}

/// live 文件名判定:带时间戳的备份不算 live。
#[test]
fn live_names_are_the_untimestamped_ones() {
    assert!(is_live_name(Path::new("/a/workbuddy-desktop.info")));
    assert!(is_live_name(Path::new("/a/workbuddy-desktop-ai.info")));
    assert!(!is_live_name(Path::new(
        "/a/workbuddy-desktop.2026-06-09T09-04-37-611Z.info"
    )));
}

/// 候选排序:国内版排在前面(挂件只展示国内版),同区域 live 先于备份。
/// 用两把伪造的 key 验顺序,不依赖本机文件。
#[test]
fn domestic_and_live_come_first() {
    let mk = |src: &str, domain: &str| DesktopCredential {
        source: src.to_string(),
        domain: domain.to_string(),
        ..Default::default()
    };
    let mut v = vec![
        mk("/a/workbuddy-desktop-ai.info", "www.workbuddy.ai"),
        mk("/a/workbuddy-desktop.2026-06-09.info", "www.codebuddy.cn"),
        mk("/a/workbuddy-desktop.info", "www.codebuddy.cn"),
    ];
    v.sort_by_key(|c| {
        let live = is_live_name(Path::new(&c.source));
        (c.is_global(), !live)
    });
    assert_eq!(v[0].source, "/a/workbuddy-desktop.info", "国内 live 第一");
    assert_eq!(v[1].source, "/a/workbuddy-desktop.2026-06-09.info", "国内备份第二");
    assert_eq!(v[2].source, "/a/workbuddy-desktop-ai.info", "国际版最后");
}

// ───────── 第三轮体检(2026-10-19)抓到的续期断链:以下四条把它钉住 ─────────

/// 端到端:临时目录里放一份"accessToken 已过期但仍带可用 refreshToken"的 live 凭据。
/// 旧姿态(include_expired=false)根本看不见它 → 续期分支恒不可达;新姿态才续得上。
#[test]
fn expired_live_credential_reaches_the_refresh_step() {
    let now = 1_800_000_000_000i64;
    let dir = std::env::temp_dir().join(format!("ts-wb-expired-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    // 明文形态:单测不该去起桌面端进程拿 key
    let live = dir.join("workbuddy-desktop.info");
    std::fs::write(
        &live,
        format!(
            r#"{{"auth":{{"accessToken":"old-tok","refreshToken":"rt","domain":"www.codebuddy.cn","expiresAt":{},"refreshExpiresAt":{}}},"account":{{"uin":123}}}}"#,
            now - 1,
            now + 86_400_000
        ),
    )
    .expect("写伪造凭据");
    let files = vec![live.clone()];

    // ① 修复前的过滤:过期那份被丢掉 → 上层拿不到任何候选 → 不会去续
    let old = collect_from(&files, now, false).expect_err("过期的不该进候选");
    assert!(matches!(old, DesktopErr::Expired));

    // ② 修复后:同一份文件带进续期这一步
    let got = collect_from(&files, now, true).expect("include_expired 该收下过期那份");
    assert_eq!(got.len(), 1);
    assert!(got[0].expired(now));

    let tried = std::cell::Cell::new(0usize);
    let plan = plan_tokens(now, &[], &got, &|_: &DesktopCredential| -> Result<Refreshed, String> {
        tried.set(tried.get() + 1);
        Ok(Refreshed {
            access_token: "fresh-tok".into(),
            refresh_token: Some("rt2".into()),
            expires_in_sec: Some(3600),
            domain: None,
        })
    });
    assert_eq!(tried.get(), 1, "该发起一次续期");
    assert_eq!(plan.tokens[0], "fresh-tok", "续来的排最前");
    assert_eq!(plan.to_store.len(), 1, "续期结果该落库");
    assert_eq!(plan.to_store[0].1.refresh_token.as_deref(), Some("rt2"));
    assert!(plan.refresh_fails.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// 不该续的三种情形:没过期、refreshToken 也超期、refreshToken 是空白。
#[test]
fn refresh_is_attempted_only_when_it_can_work() {
    let now = 1_800_000_000_000i64;
    let tried = std::cell::Cell::new(0usize);
    let fake = &|_: &DesktopCredential| -> Result<Refreshed, String> {
        tried.set(tried.get() + 1);
        Ok(Refreshed {
            access_token: "x".into(),
            ..Default::default()
        })
    };
    let mk = |rt: Option<&str>, exp: i64, rtx: i64| DesktopCredential {
        access_token: "old".into(),
        refresh_token: rt.map(String::from),
        expires_at_ms: exp,
        refresh_expires_at_ms: rtx,
        ..Default::default()
    };
    // 没过期 → 直接用,不起请求
    let p = plan_tokens(now, &[], &[mk(Some("rt"), now + 60_000, now + 86_400_000)], fake);
    assert_eq!(tried.get(), 0, "没过期就不该续");
    assert_eq!(p.tokens, vec!["old".to_string()]);
    // accessToken 与 refreshToken 都超期 → 只能人工登一次
    plan_tokens(now, &[], &[mk(Some("rt"), now - 1, now - 1)], fake);
    assert_eq!(tried.get(), 0, "refreshToken 也过期就不该白打");
    // 没有 refreshToken / 只有空白
    plan_tokens(now, &[], &[mk(None, now - 1, now + 1)], fake);
    plan_tokens(now, &[], &[mk(Some("   "), now - 1, now + 1)], fake);
    assert_eq!(tried.get(), 0, "空白 refreshToken 不算可用的续期凭据");
}

/// 上次续期的成果:必须按**过期时刻**决定用不用。
/// 修复前落库只存令牌不存时刻 → 那份令牌作废后仍永远顶在候选第一位,每轮白吃一个 401。
#[test]
fn stored_refresh_is_dropped_once_expired() {
    let now = 1_800_000_000_000i64;
    let st = |exp: i64| StoredRefresh {
        global: false,
        access: format!("tok-{exp}"),
        refresh: None,
        expires_at_ms: exp,
    };
    let none = &|_: &DesktopCredential| -> Result<Refreshed, String> { Err("不该被调".into()) };
    let p = plan_tokens(now, &[st(0), st(now + 1000)], &[], none);
    assert!(p.tokens.contains(&format!("tok-{}", now + 1000)));
    assert!(p.tokens.contains(&"tok-0".to_string()), "老数据没记过期时刻,不该把人挡死");
    let p2 = plan_tokens(now, &[st(now - 1)], &[], none);
    assert!(p2.tokens.is_empty(), "已过期的上次续期成果不该再当首选");
}

/// 续期失败要如实带出来(界面/日志能说清"为什么还是没数"),但不能把其它候选挤掉。
#[test]
fn refresh_failure_is_reported_and_other_candidates_still_used() {
    let now = 1_800_000_000_000i64;
    let expired = DesktopCredential {
        access_token: "dead".into(),
        refresh_token: Some("rt".into()),
        expires_at_ms: now - 1,
        refresh_expires_at_ms: now + 1,
        source: "/a/workbuddy-desktop.info".into(),
        ..Default::default()
    };
    let live_backup = DesktopCredential {
        access_token: "still-ok".into(),
        expires_at_ms: now + 60_000,
        source: "/a/workbuddy-desktop.2026-06-09.info".into(),
        ..Default::default()
    };
    let plan = plan_tokens(
        now,
        &[],
        &[expired, live_backup],
        &|_: &DesktopCredential| -> Result<Refreshed, String> { Err("9074 排队".into()) },
    );
    assert_eq!(plan.tokens, vec!["still-ok".to_string()], "续期失败不该挤掉可用候选");
    assert_eq!(plan.refresh_fails.len(), 1);
    assert!(plan.refresh_fails[0].contains("9074"), "{:?}", plan.refresh_fails);
    assert!(plan.to_store.is_empty());
}

/// 端到端(真机):桌面端凭据 → 按区域选域名 → /v2 取数 → `extract_workbuddy` 算余额。
/// 这是本次改动的最终判据:挂件真的能算出 WorkBuddy 余额。默认 ignore。
#[test]
#[ignore]
fn wb_real_machine_end_to_end_balance() {
    let now_ms = chrono::Local::now().timestamp_millis();
    let creds = read_desktop_credentials(now_ms).expect("桌面端凭据");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();
    let mut ok: Option<(String, f64)> = None;
    for c in &creds {
        let body = billing_body(chrono::Local::now());
        let r = rt.block_on(fetch_billing(
            &http,
            &FetchAuth::Bearer(c.access_token.clone()),
            &body,
            bases_for(c),
        ));
        match r {
            Ok((url, text)) => {
                let v: serde_json::Value = serde_json::from_str(&text).unwrap();
                let parsed = crate::providers::extract_workbuddy(&v);
                eprintln!(
                    "{} → {url} · valid={} remaining={:?} 包={} err={:?}",
                    c.source,
                    parsed.valid,
                    parsed.remaining,
                    parsed.expiring.len(),
                    parsed.error
                );
                if parsed.valid {
                    if let Some(rem) = parsed.remaining {
                        ok = Some((c.source.clone(), rem));
                        break;
                    }
                }
            }
            Err(e) => eprintln!("{} → 取数失败:{e}", c.source),
        }
    }
    let (src, rem) = ok.expect("至少要有一份凭据能算出余额");
    eprintln!("WorkBuddy 余额 = {rem}(来自 {src})");
    assert!(rem >= 0.0);
}

/// 真机:走完 桌面端凭据 → 解密 → /v2 取数,断言能拿到积分。
/// 默认 ignore(要本机装桌面端并登录):`cargo test --lib -- --ignored wb_real_machine`。
#[test]
#[ignore]
fn wb_real_machine_reads_credits() {
    let now_ms = chrono::Local::now().timestamp_millis();
    let cred = match read_desktop_credential(now_ms) {
        Ok(c) => c,
        Err(e) => panic!("桌面端凭据读不出来:{}", e.message()),
    };
    eprintln!("凭据来源 = {}", cred.source);
    eprintln!("domain   = {}", cred.domain);
    eprintln!("uin      = {}", cred.uin);
    eprintln!(
        "accessToken 到 {}",
        chrono::DateTime::from_timestamp_millis(cred.expires_at_ms)
            .map(|d| d.with_timezone(&chrono::Local).to_rfc3339())
            .unwrap_or_else(|| "?".into()),
    );
    assert!(!cred.access_token.is_empty());

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();
    let body = billing_body(chrono::Local::now());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (url, text) = rt
        .block_on(fetch_billing(
            &http,
            &FetchAuth::Bearer(cred.access_token.clone()),
            &body,
            bases_for(&cred),
        ))
        .expect("取数应当成功");
    eprintln!("端点 = {url}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        v["code"],
        0,
        "业务 code 应为 0:{}",
        &text[..200.min(text.len())]
    );
    let accts = v["data"]["Response"]["Data"]["Accounts"]
        .as_array()
        .expect("要有 Accounts");
    eprintln!("Accounts 条数 = {}", accts.len());
    let total: f64 = accts
        .iter()
        .map(|a| {
            a.get("CycleCapacityRemain")
                .or_else(|| a.get("CapacityRemain"))
                .and_then(|x| x.as_f64())
                .unwrap_or(0.0)
        })
        .sum();
    eprintln!("合计剩余 = {total}");
    // 解析层也要能认(与真实接口对齐)
    let r = crate::providers::extract_workbuddy(&v);
    eprintln!(
        "解析结果 = remaining {:?} / 到期条目 {} 个",
        r.remaining,
        r.expiring.len()
    );
    assert!(r.valid, "解析应当有效:{:?}", r.error);
    assert!(r.remaining.is_some(), "应当能算出余额");
}

// ───────── 装到别的盘(2026-10-20 用户实证:WorkBuddy 装在 D 盘)─────────

/// 盘符枚举只列真实存在的盘,且一定是大写字母。
#[test]
fn drive_letters_are_real_drives() {
    let ds = drive_letters();
    assert!(!ds.is_empty(), "本机至少有一个盘");
    for d in &ds {
        assert!(d.is_ascii_uppercase(), "盘符要大写:{d}");
        assert!(Path::new(&format!("{d}:\\")).is_dir(), "列出来的盘符必须存在");
    }
    assert!(ds.contains(&'C'), "本机一定有 C 盘:{ds:?}");
}

/// **这条是用例的核心**:装到别的盘 / 自定义目录时,exe 也必须被找到。
/// 用临时目录伪造一个 `D:\Programs\WorkBuddy` 式的布局,只验证"候选目录的构造"
/// 与"命中逻辑",不依赖真机有没有装。
#[test]
fn exe_search_covers_non_default_install_dirs() {
    let base = std::env::temp_dir().join(format!("ts-wb-exe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    // 伪造一个"用户自己挑的目录"下的安装
    let install = base.join("Programs").join("WorkBuddy");
    std::fs::create_dir_all(&install).unwrap();
    let exe = install.join("WorkBuddy.exe");
    std::fs::write(&exe, b"fake").unwrap();

    // 把用户盘符那一步替换成这个临时根,验证"逐盘 × 常见目录"确实会扫到它
    let roots = app_roots_for(&[base.clone()]);
    let hit = roots
        .iter()
        .flat_map(|r| EXE_NAMES.iter().map(move |n| r.join(n)))
        .find(|p| p.is_file());
    // 用存在性 + 文件名判,不比较绝对路径字符串:临时目录的大小写/短名在
    // Windows 上可能与候选拼出来的写法不一致,那不是被测语义。
    let hit = hit.unwrap_or_else(|| panic!("非默认目录里的 WorkBuddy.exe 必须被找到;候选根:{roots:?}"));
    assert_eq!(
        hit.file_name().unwrap().to_string_lossy(),
        "WorkBuddy.exe",
        "命中的是主程序名"
    );
    assert_eq!(
        hit.parent().map(|p| p.to_path_buf()),
        Some(install.clone()),
        "必须命中那个自定义安装目录:{hit:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// 候选目录里必须**同时**含"环境变量默认位置"与"任意盘常见位置"两大类,
/// 否则"装在 D 盘"这类机器还是会落空。
#[test]
fn app_roots_include_every_drive_and_standard_programs() {
    let roots = app_roots();
    let text: Vec<String> = roots.iter().map(|p| p.display().to_string().to_lowercase()).collect();
    // 每个存在的盘都要有候选(哪怕那个盘上其实没装,也要去看一眼)
    for d in drive_letters() {
        let prefix = format!("{}:\\", d.to_ascii_lowercase());
        assert!(
            text.iter().any(|p| p.starts_with(&prefix)),
            "{d} 盘没有任何候选:{}",
            text.join(" | ")
        );
    }
    // 常见的几种"用户自定义目录"命名都要覆盖
    for want in ["\\programs\\workbuddy", "\\workbuddy", "\\program files\\workbuddy", "\\apps\\workbuddy"] {
        assert!(
            text.iter().any(|p| p.ends_with(want)),
            "缺少候选后缀 {want}:{}",
            text.join(" | ")
        );
    }
    // 去重:同一个路径不许出现两遍(否则日志里看着像探了两遍)
    let mut uniq = text.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), text.len(), "候选目录里有重复");
}

/// exe 名字要认国内版与国际版两个(它们同栈但文件名不同)。
#[test]
fn exe_names_cover_both_editions() {
    assert!(EXE_NAMES.contains(&"WorkBuddy.exe"), "{EXE_NAMES:?}");
    assert!(EXE_NAMES.contains(&"WorkBuddyAI.exe"), "{EXE_NAMES:?}");
}

/// 注册表 `DisplayIcon` 的解析:真机形态是 `C:\Program Files\WorkBuddy\WorkBuddy.exe,0`
/// (逗号后是图标序号)。**这条是"装到任意盘也能定位"的最后一道保险** ——
/// 本机实测 WorkBuddy 5.8.3 的卸载项里 `InstallLocation` 是空的,
/// 只剩 `DisplayIcon` 能给出路径,所以它必须解析得对。
#[test]
fn display_icon_parses_to_the_exe() {
    // 真机样例(本机 WorkBuddy 5.8.3)
    assert_eq!(
        parse_display_icon(r"C:\Program Files\WorkBuddy\WorkBuddy.exe,0"),
        Some(PathBuf::from(r"C:\Program Files\WorkBuddy\WorkBuddy.exe"))
    );
    // 装到别的盘 + 带引号
    assert_eq!(
        parse_display_icon(r#""D:\Apps\WorkBuddy\WorkBuddy.exe",0"#),
        Some(PathBuf::from(r"D:\Apps\WorkBuddy\WorkBuddy.exe"))
    );
    // 没有图标序号
    assert_eq!(
        parse_display_icon(r"E:\WorkBuddy\WorkBuddy.exe"),
        Some(PathBuf::from(r"E:\WorkBuddy\WorkBuddy.exe"))
    );
    // 图标文件不是主程序
    assert_eq!(parse_display_icon(r"C:\x\icon.ico"), None);
    assert_eq!(parse_display_icon(r"C:\x\icon.ico,0"), None);
    assert_eq!(parse_display_icon(""), None);
    // 路径本身带逗号时不能切错(尾部不是数字就当没有序号)
    assert_eq!(
        parse_display_icon(r"C:\a,b\WorkBuddy.exe,0"),
        Some(PathBuf::from(r"C:\a,b\WorkBuddy.exe"))
    );
}

/// `WORKBUDDY_AUTH_DIR` 是数据目录被挪走时的逃生门:给了就**必须**排在第一个,
/// 而且要同时接受"给 auth 目录"和"给它的上一层"两种写法。
#[test]
fn auth_dir_env_override_comes_first() {
    // 环境变量是进程级的,用完就还原,别污染别的用例
    let prev = std::env::var_os("WORKBUDDY_AUTH_DIR");
    let probe = std::env::temp_dir().join(format!("ts-wb-auth-env-{}", std::process::id()));
    std::env::set_var("WORKBUDDY_AUTH_DIR", &probe);
    let dirs = auth_dirs();
    assert_eq!(dirs.first().map(|p| p.as_path()), Some(probe.as_path()), "{dirs:?}");
    // 给的是"上一层"时,还要自动补一个 auth 子目录(免得用户写错一层就失效)
    assert!(
        dirs.iter().any(|p| p.ends_with("auth") && p.starts_with(&probe)),
        "应当自动补 auth 子目录:{dirs:?}"
    );
    match prev {
        Some(v) => std::env::set_var("WORKBUDDY_AUTH_DIR", v),
        None => std::env::remove_var("WORKBUDDY_AUTH_DIR"),
    }
}
