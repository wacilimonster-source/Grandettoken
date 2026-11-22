use super::*;
use chrono::Timelike;

/// 外壳的轮询现在按「下一次重试到期」精确睡眠,这个值就是唯一的重试时机来源。
/// 它必须取**最早到期**的渠道:返回晚了,5/15 秒的短重试就退化成"下一轮主取数才试"。
#[test]
fn next_retry_at_is_the_earliest_due_channel() {
    let f = Fetcher::new();
    assert_eq!(f.next_retry_at(), None, "没有失败渠道时必须允许一次睡到本轮结束");
    {
        let mut states = f.recovery.lock().unwrap();
        states.insert("mid".into(), Recovery { attempts: 1, started: 0, next: 300, kind: None });
        states.insert("slow".into(), Recovery { attempts: 2, started: 0, next: 900, kind: None });
    }
    assert_eq!(f.next_retry_at(), Some(300));
    f.recovery.lock().unwrap().insert(
        "due".into(),
        Recovery { attempts: 1, started: 0, next: 60, kind: None },
    );
    assert_eq!(f.next_retry_at(), Some(60), "必须取最小到期时刻");
}

#[test]
fn restart_cache_is_scoped_and_explicit_forget_removes_history() {
    let store = Arc::new(Mutex::new(Store::open_memory().unwrap()));
    let fetcher = Fetcher::new();
    Snapshot::new("a".into(), 123, FetchResult { valid: true, remaining: Some(42.0), ..Default::default() }).save(&store.lock().unwrap(), "t").unwrap();
    assert_eq!(fetcher.cached(&store, "t", "a").unwrap().1, 123);
    assert!(fetcher.cached(&store, "t", "b").is_none(), "换账户不得复用内存快照");
    assert_eq!(fetcher.cached(&store, "t", "a").unwrap().0.remaining, Some(42.0));
    fetcher.forget_channel(&store.lock().unwrap(), "t");
    assert!(fetcher.cached(&store, "t", "a").is_none());
    assert!(fetcher.cached(&store, "t", "").is_none());
    fetcher.cached(&store, "new", "a");
    fetcher.failed("new", Some(FailKind::Transport));
    assert!(fetcher.cached(&store, "new", "a").is_none());
    assert!(fetcher.recovery.lock().unwrap().contains_key("new"), "首次获取没有快照也必须继续重试");
}

/// 换账号/换令牌后,旧网页会话必须被判定为陈旧;没换过时不能误杀。
#[test]
fn qoder_credential_change_makes_the_old_web_session_stale() {
    let store = Store::open_memory().unwrap();
    // 第一次(没有历史):不算换号
    assert!(!note_qoder_scope(&store, "qoder", "scope-a"));
    assert!(!note_qoder_scope(&store, "qoder", "scope-a"), "同一本账不重复记更换");
    // 换令牌 → 记下更换时刻;此时如果网页会话还是旧的,必须判陈旧
    assert!(note_qoder_scope(&store, "qoder", "scope-b"));
    store.set_setting("qoder_web_session_at", "100").unwrap();
    assert!(qoder_web_session_stale(&store, "qoder", "qoder_web_session_at"));
    // 用新账号重新登录网页(会话时刻晚于更换时刻)→ 不再陈旧
    store.set_setting("qoder_web_session_at", "9999999999").unwrap();
    assert!(!qoder_web_session_stale(&store, "qoder", "qoder_web_session_at"));
    // 换过凭据但一次网页都没登录过 → 没有可用会话,同样要提示重新登录
    store.delete_setting("qoder_web_session_at").unwrap();
    assert!(qoder_web_session_stale(&store, "qoder", "qoder_web_session_at"));

    // 升级上来的用户还没有专门的 scope 键:从上次成功快照里的 scope 认上一本账
    let upgraded = Store::open_memory().unwrap();
    Snapshot::new(
        "old-account".into(),
        1,
        FetchResult { valid: true, remaining: Some(1.0), ..Default::default() },
    )
    .save(&upgraded, "qoder")
    .unwrap();
    assert!(note_qoder_scope(&upgraded, "qoder", "new-account"));
}

#[test]
fn credential_rotation_uses_explicit_account_and_manual_refresh_respects_429() {
    let mut c = appauth::Credential { token: "first".into(), source: "test".into(), account_id: Some("account-a".into()), device_id: None };
    let scope = credential_scope(&c);
    c.token = "second".into();
    assert_eq!(scope, credential_scope(&c));
    c.account_id = Some("account-b".into());
    assert_ne!(scope, credential_scope(&c));
    let f = Fetcher::new();
    f.failed("network", Some(FailKind::Transport));
    f.failed("limited", Some(FailKind::RateLimited(300)));
    f.request_refresh();
    assert!(!f.recovery.lock().unwrap().contains_key("network"));
    assert!(f.recovery.lock().unwrap().contains_key("limited"));
}

#[test]
fn quota_attempt_is_single_request_and_rotates_fallback_with_shared_retry_count() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url: &'static str = Box::leak(format!("http://{}/quota", listener.local_addr().unwrap()).into_boxed_str());
    let fallback: &'static str = Box::leak(format!("http://{}/backup", listener.local_addr().unwrap()).into_boxed_str());
    let fallbacks: &'static [&'static str] = Box::leak(vec![fallback].into_boxed_slice());
    let worker = std::thread::spawn(move || {
        let mut paths = Vec::new();
        for (status, body, headers) in [("503 Service Unavailable", "{}", ""), ("200 OK", "{\"data\":{\"total_available\":21000000}}", ""), ("429 Too Many Requests", "{}", "Retry-After: 120\r\n")] {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
            let mut request = [0u8; 4096];
            let n = stream.read(&mut request).unwrap();
            paths.push(String::from_utf8_lossy(&request[..n]).lines().next().unwrap().to_string());
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
        paths
    });
    let def = providers::ProviderDef { url, fallback_urls: fallbacks, ..*providers::find("4sapi").unwrap() };
    let f = Fetcher::new();
    let c = appauth::Credential { token: "test-quota".into(), source: "test".into(), account_id: None, device_id: None };
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (r, _, k) = f.quota_attempt(&def, &c, None).await;
        assert!(!r.valid);
        assert_eq!(k, Some(FailKind::Server));
        f.failed(def.id, k);
        let (r, used, k) = f.quota_attempt(&def, &c, None).await;
        assert!(r.valid);
        assert_eq!(r.remaining, Some(42.0));
        assert_eq!(used.as_deref(), Some(fallback));
        assert!(k.is_none());
        let (_, _, k) = f.quota_attempt(&def, &c, None).await;
        assert_eq!(k, Some(FailKind::RateLimited(120)));
    });
    let paths = worker.join().unwrap();
    assert_eq!(paths.len(), 3);
    assert!(paths[0].contains("/quota"));
    assert!(paths[1].contains("/backup"));
}

#[test]
fn codex_reset_credit_read_is_scoped_and_optional_errors_stay_unknown() {
    use std::io::{Read, Write};
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/reset-credits", server.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for response in [
            "HTTP/1.1 200 OK\r\nContent-Length: 34\r\nConnection: close\r\n\r\n{\"available_count\":0,\"credits\":[]}",
            "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ] {
            let (mut stream, _) = server.accept().unwrap();
            stream.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
            let mut raw = Vec::new();
            let mut buf = [0u8; 1024];
            while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0);
                raw.extend_from_slice(&buf[..n]);
            }
            requests.push(String::from_utf8(raw).unwrap().to_lowercase());
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let empty = rt.block_on(fetch_codex_reset_credits(&client, "test-access", Some("workspace-test"), &url)).unwrap();
    assert_eq!(empty.available_count, Some(0));
    let failure = rt.block_on(fetch_codex_reset_credits(&client, "test-access", Some("workspace-test"), &url)).unwrap_err();
    assert!(failure.contains("403"));
    assert!(crate::codex::unavailable(None, failure).available_count.is_none());
    for req in worker.join().unwrap() {
        assert!(req.starts_with("get /reset-credits"));
        assert!(req.contains("authorization: bearer test-access"));
        assert!(req.contains("chatgpt-account-id: workspace-test"));
        assert!(!req.contains("cookie:"));
    }
}

/// 2026-10-21:端到端验一次"取数失败 → 日志里有一行可用的现场"。
///
/// 这条用例守的是整套日志的**存在意义**:以前 9 个内置渠道里只有 WorkBuddy
/// 那一路有日志,其余的失败时日志里只有"取数失败"四个字。
/// 现在 `fetch_once` 埋一行 L2 —— URL、状态码、耗时、错误码全在里面。
#[test]
fn a_failed_request_leaves_a_usable_line_in_the_log() {
    let _lock = crate::diaglog::test_lock();
    let dir = std::env::temp_dir().join(format!("ts-fetch-log-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let _arm = crate::diaglog::arm_test_dir(dir.clone());

    // 127.0.0.1:1 —— 连都不连得上,必然失败,而且不依赖外网
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap();
    let def = providers::find("4sapi").unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (_res, _kind) = rt.block_on(fetch_once(
        &client,
        def,
        "http://127.0.0.1:1/v1/usage",
        "sk-test-key",
    ));

    let text = crate::diaglog::read_log();
    assert!(!text.is_empty(), "失败必须留下日志");
    assert!(text.contains("E_"), "要有错误码:{text}");
    assert!(text.contains("127.0.0.1:1"), "要有 URL:{text}");
    assert!(text.contains("ms="), "要有耗时字段:{text}");
    assert!(text.contains("host=127.0.0.1"), "要有主机名:{text}");
    // 连接都没连上 → **没有 HTTP 状态码**。这时刻意不写 status=
    // (写一个空状态码会让人以为"服务器回了个空状态")。
    //
    // 例外:某些环境(沙箱 / 透明代理)会先替 127.0.0.1:1 应答一个 5xx ——
    // 那种情况其实是"连上了、只是被中间设备拒了",日志里有 status 是对的。
    // 只在错误码是连接层(E_CONNECT / E_TIMEOUT)时才断言没有 status;
    // 否则反过来断言它必须是 HTTP 层的错误码,不许把代理拦截写成"网络断了"。
    if text.contains("code=E_CONNECT") || text.contains("code=E_TIMEOUT") {
        assert!(!text.contains("status="), "连不上时不该有 status=:{text}");
    } else {
        assert!(
            text.contains("code=E_HTTP"),
            "被代理/中间设备应答时,错误码必须是 HTTP 层:{text}"
        );
    }
    // 密钥绝不能出现
    assert!(!text.contains("sk-test-key"), "密钥不该出现在日志里:{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 2026-10-21:状态码 → 类别 的**两套映射不能打架**。
///
/// 取数层把 401/403/407 判成 `Auth`(该换下一份凭据),
/// 错误码层必须给同一类结论(要去登录)—— 否则日志里的码会把人引向错误的动作。
/// 两套映射以前各写一份,407 那边就对不上(取数层当"凭据被拒",
/// 码层说"请求被拒绝,等一等")。
#[test]
fn status_code_mappings_agree() {
    for code in 400..=599u16 {
        let kind = kind_for_status(code);
        let ec = crate::errcodes::for_status(code);
        assert!(ec.starts_with("E_"), "{code} 没有对应错误码:{ec}");
        if kind == FailKind::Auth {
            assert_eq!(
                crate::errcodes::action(ec),
                crate::errcodes::Action::Login,
                "{code} 在取数层算凭据被拒,码层也要说'去登录'"
            );
        }
        // 5xx / 429 在取数层是可重试的,码层也不该说"要用户动手"
        if kind == FailKind::Server {
            assert_eq!(
                crate::errcodes::action(ec),
                crate::errcodes::Action::Wait,
                "{code} 是服务端/限流问题,不该要用户动手"
            );
        }
    }
}

/// 2026-10-21:失败落盘要带上文 —— 单看"HTTP 401"没用,
/// 看到"之前换过域名、换过凭据"才知道是谁的问题。
#[test]
fn a_failure_carries_the_events_that_came_before_it() {
    let _lock = crate::diaglog::test_lock();
    let dir = std::env::temp_dir().join(format!("ts-fetch-ctx-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let _arm = crate::diaglog::arm_test_dir(dir.clone());

    // 先来几条"之前发生过的事"(成功的事件默认不落盘,但进内存环形缓冲)
    crate::diaglog::result(
        "fetch",
        Some("4sapi"),
        "cred",
        &[("cred_source", "手动粘贴")],
    );
    crate::diaglog::result(
        "fetch",
        Some("4sapi"),
        "attempt",
        &[("host", "api.4sapi.com")],
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap();
    let def = providers::find("4sapi").unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (_res, _kind) = rt.block_on(fetch_once(
        &client,
        def,
        "http://127.0.0.1:1/v1/usage",
        "sk-test-key",
    ));

    let text = crate::diaglog::read_log();
    assert!(text.contains("↳ 上文"), "失败要带上文:{text}");
    assert!(text.contains("cred_source=手动粘贴"), "上文里有之前的事件:{text}");
    assert!(text.contains("api.4sapi.com"), "上文里有之前的决策:{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 2026-10-21:URL 里的查询参数如果带密钥,也不能落进日志。
#[test]
fn a_secret_in_the_query_string_never_reaches_the_log() {
    let _lock = crate::diaglog::test_lock();
    let dir = std::env::temp_dir().join(format!("ts-fetch-log2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let _arm = crate::diaglog::arm_test_dir(dir.clone());

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap();
    let def = providers::find("4sapi").unwrap();
    let secret = "SUPERSECRETVALUE1234567890ABCDEFGH";
    let url = format!("http://127.0.0.1:1/v1/usage?api_key={secret}");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (_res, _kind) = rt.block_on(fetch_once(&client, def, &url, "sk-test-key"));

    let text = crate::diaglog::read_log();
    assert!(!text.contains(secret), "查询参数里的密钥不该落盘:{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

fn test_rate() -> FxRate {
    FxRate {
        rate: 7.0,
        month: "2026-10".into(),
        src: "test".into(),
    }
}

/// `attach_plan_costs` 按 PROVIDERS 下标读写,所以测试也得造**同长同序**的向量:
/// 以前测试里递的是长度 1 的 `[memo]`,恰好替生产代码把 P1 那个下标错位遮住了。
fn pidx(id: &str) -> usize {
    providers::PROVIDERS
        .iter()
        .position(|d| d.id == id)
        .expect("渠道在表里")
}

fn aligned_views(v: ChannelView) -> Vec<Option<ChannelView>> {
    let i = pidx(&v.id);
    let mut out: Vec<Option<ChannelView>> =
        (0..providers::PROVIDERS.len()).map(|_| None).collect();
    out[i] = Some(v);
    out
}

fn aligned_cycles(pairs: &[(&str, Option<CycleMemo>)]) -> Vec<Option<CycleMemo>> {
    providers::PROVIDERS
        .iter()
        .map(|d| {
            pairs
                .iter()
                .find(|(id, _)| *id == d.id)
                .and_then(|(_, m)| *m)
        })
        .collect()
}

fn plan_at(views: &[Option<ChannelView>], id: &str) -> Option<PlanCost> {
    views
        .iter()
        .flatten()
        .find(|v| v.id == id)
        .and_then(|v| v.plan.clone())
}

/// $10 / 10 天周期 / 汇率 7 → 每天 7 元。它只跟"过了多久"有关,与用了多少 token 无关。
#[test]
fn plan_cost_accrues_with_elapsed_time_only() {
    let d = 86_400.0;
    let today0 = 1_000_000;
    let c = plan_cost(
        10.0,
        10.0,
        &test_rate(),
        false,
        today0 + (0.5 * d) as i64, // 今天过了半天
        today0,
        today0 - 3 * d as i64, // 本周已过 3.5 天
        today0 - 5 * d as i64, // 本月已过 5.5 天
        today0 - 35 * d as i64,
    );
    assert!((c.per_day - 7.0).abs() < 1e-9);
    assert!((c.day - 3.5).abs() < 1e-6, "半天 → 3.5 元");
    assert!((c.week - 24.5).abs() < 1e-3);
    assert!((c.month - 38.5).abs() < 1e-3);
    // 上期"同时段"长度与本期一样,所以订阅这一块的环比天然接近 ±0%
    assert!((c.day_prev - c.day).abs() < 1e-6);
    assert!((c.week_prev - c.week).abs() < 1e-3);
    assert!(!c.learned);
    assert_eq!(c.rate_month, "2026-10");
}

/// 上月只有 28 天、本月已过 29 天:上月同期必须钳在 28 天之内,
/// 否则多出来的一天会把基线抬高,"本月环比"凭空少一截(报告 P2-4 同款坑)。
#[test]
fn plan_cost_prev_month_never_leaks_into_this_month() {
    let d = 86_400.0;
    let month0 = 40 * d as i64;
    let prev0 = month0 - 28 * d as i64;
    let now = month0 + 29 * d as i64;
    let c = plan_cost(
        10.0,
        10.0,
        &test_rate(),
        true,
        now,
        now - 3_600,
        month0 - 7 * d as i64,
        month0,
        prev0,
    );
    assert!(
        c.month_prev < c.per_day * 28.0 + 1e-6,
        "上月同期不该超过上月本身的长度: {:?}",
        c.month_prev
    );
    assert!(c.month_prev > c.per_day * 27.0);
}

#[test]
fn cycle_length_is_learned_only_from_a_real_reset() {
    let d = 86_400i64;
    let a = learn_cycle(None, 100);
    assert_eq!(a.days, None, "只看到一次重置,还学不到长度");
    assert_eq!(learn_cycle(Some(a), 100), a, "同一周期内重复观察不该改动任何东西");
    assert_eq!(
        learn_cycle(Some(a), 100 + 2 * d),
        a,
        "服务端调表 2 天不足以断定跨了一期,否则会学出个「2 天周期」"
    );
    let b = learn_cycle(Some(a), 100 + 30 * d);
    assert_eq!(b.days, Some(30.0));
    assert_eq!(
        learn_cycle(Some(b), 100),
        b,
        "时钟被往前校正时重置时刻会倒退,这时必须原样返回"
    );
    let c = learn_cycle(Some(b), 100 + 130 * d);
    assert_eq!(c.days, Some(30.0), "100 天的间隔像改了套餐,保留旧周期只挪锚点");
    assert_eq!(c.last_reset, 100 + 130 * d);
}

/// 没跨过真实重置之前,用"重置日往前推一个整月"当本期长度 —— 第一天就准。
#[test]
fn cycle_length_is_inferred_from_the_reset_date() {
    assert!(
        (inferred_cycle_days(1_793_664_000).unwrap() - 31.0).abs() < 1e-9,
        "11-03 重置 → 10-03 起算,31 天"
    );
    assert!(
        (inferred_cycle_days(1_804_032_000).unwrap() - 28.0).abs() < 1e-9,
        "3-03 重置 → 2-03 起算,平年 28 天"
    );
    assert!(
        (inferred_cycle_days(1_709_596_800).unwrap() - 29.0).abs() < 1e-9,
        "2024 闰年 2 月是 29 天"
    );
    assert!(inferred_cycle_days(i64::MIN).is_none(), "荒谬时间戳不该 panic 也不该出数");
}

/// 周期长度三级口径:学到的 > 从重置日推的 > 30 天兜底。
#[test]
fn attach_prefers_observed_cycle_over_inference() {
    let s = Store::open_memory().unwrap();
    let def = providers::find("opencode-go").unwrap();
    let mut v = placeholder(def);
    v.windows = vec![providers::Window {
        key: "monthly".into(),
        label: "周期".into(),
        percent: 10.0,
        remain_percent: 90.0,
        status: "ok".into(),
        resets_at: Some("2026-11-03T00:00:00+00:00".into()),
        main: true,
    }];

    let mut observed = aligned_views(v.clone());
    attach_plan_costs(
        &mut observed,
        &s,
        &aligned_cycles(&[(
            "opencode-go",
            Some(CycleMemo {
                last_reset: 1,
                days: Some(10.0),
            }),
        )]),
        Some(&test_rate()),
    );
    let p = plan_at(&observed, "opencode-go").unwrap();
    assert!((p.days - 10.0).abs() < 1e-9, "已经学到的优先用");
    assert!(p.learned);

    let mut inferred = aligned_views(v);
    attach_plan_costs(&mut inferred, &s, &aligned_cycles(&[]), Some(&test_rate()));
    let p = plan_at(&inferred, "opencode-go").unwrap();
    assert!(
        (p.days - 31.0).abs() < 1e-9,
        "没学到时按重置日往前推一个整月,而不是 30 天"
    );
    assert!(!p.learned, "推出来的必须标明不是实测");
    assert!(inferred[pidx("opencode-go")]
        .as_ref()
        .unwrap()
        .extra
        .iter()
        .any(|(k, _)| k == "套餐摊销(按上一期长度)"));
}

/// F1:周期记忆是**按 PROVIDERS 下标**读的。opencode-go 在 PROVIDERS[1],以前
/// `cycles` 只装了长度 1 的"有套餐的那几条" → `cycles.get(1)` 恒为 None,
/// 学到的周期永远读不到、`save_cycle` 每轮白写一次库(P1)。
/// 这条测试走的是生产的构造路径(load_cycles)与真实下标,不是手搓向量。
#[test]
fn plan_cost_reads_the_memo_at_the_providers_index() {
    let s = Store::open_memory().unwrap();
    let def = providers::find("opencode-go").expect("opencode-go 渠道存在");
    assert_eq!(pidx("opencode-go"), 1, "这条测试的前提:它不在下标 0 上");
    save_cycle(
        &s,
        "opencode-go",
        &CycleMemo {
            last_reset: 1,
            days: Some(30.0),
        },
    )
    .unwrap();

    let mut v = placeholder(def);
    v.windows = vec![providers::Window {
        key: "monthly".into(),
        label: "周期".into(),
        percent: 10.0,
        remain_percent: 90.0,
        status: "ok".into(),
        resets_at: Some("2026-12-03T00:00:00+00:00".into()),
        main: true,
    }];
    let mut out = aligned_views(v);
    let cycles = load_cycles(&s);
    assert_eq!(
        cycles.len(),
        providers::PROVIDERS.len(),
        "周期记忆必须与 views 同长"
    );
    attach_plan_costs(&mut out, &s, &cycles, Some(&test_rate()));
    let p = plan_at(&out, "opencode-go").expect("有汇率就该挂上摊销");
    assert!(p.learned, "库里那份记忆要在它的真实下标上被读到");
    assert_eq!(p.days, 30.0);
}

#[test]
fn plan_cost_needs_a_rate_and_skips_hidden_and_usage_based_channels() {
    let s = Store::open_memory().unwrap();
    let def = providers::find("opencode-go").expect("opencode-go 渠道存在");

    let mut none = aligned_views(placeholder(def));
    attach_plan_costs(&mut none, &s, &aligned_cycles(&[]), None);
    assert!(
        plan_at(&none, "opencode-go").is_none(),
        "一次汇率都没取到时不许拿猜的数往合计里加钱"
    );

    let mut got = aligned_views(placeholder(def));
    attach_plan_costs(&mut got, &s, &aligned_cycles(&[]), Some(&test_rate()));
    let p = plan_at(&got, "opencode-go").expect("有汇率就该挂上");
    assert!((p.usd - 10.0).abs() < 1e-9);
    assert!((p.days - ASSUMED_CYCLE_DAYS).abs() < 1e-9);
    assert!(!p.learned, "没跨过真实重置前必须标明周期是按 30 天估的");
    assert!(p.day > 0.0);

    let mut hidden_view = placeholder(def);
    hidden_view.hidden = true;
    let mut hid = aligned_views(hidden_view);
    attach_plan_costs(&mut hid, &s, &aligned_cycles(&[]), Some(&test_rate()));
    assert!(
        plan_at(&hid, "opencode-go").is_none(),
        "隐藏的渠道不进合计,挂上等于偷偷把用户关掉的东西算回钱"
    );

    let mut amount = aligned_views(placeholder(providers::find("4sapi").unwrap()));
    attach_plan_costs(&mut amount, &s, &aligned_cycles(&[]), Some(&test_rate()));
    assert!(plan_at(&amount, "4sapi").is_none());
}

/// 观察到一次重置先只记锚点;下一次跨期才学出周期,并且落库(重启后还记得)。
#[test]
fn learned_cycle_survives_across_rounds() {
    let s = Store::open_memory().unwrap();
    let def = providers::find("opencode-go").unwrap();
    let mk = |reset: &str| {
        let mut v = placeholder(def);
        v.windows = vec![providers::Window {
            key: "monthly".into(),
            label: "周期".into(),
            percent: 50.0,
            remain_percent: 50.0,
            status: "ok".into(),
            resets_at: Some(reset.into()),
            main: true,
        }];
        v
    };
    let mut first = aligned_views(mk("2026-11-03T00:00:00+00:00"));
    attach_plan_costs(&mut first, &s, &aligned_cycles(&[]), Some(&test_rate()));
    let m1 = load_cycle(&s, "opencode-go").expect("第一次观察就该落锚点");
    assert_eq!(m1.days, None);

    let mut second = aligned_views(mk("2026-12-03T00:00:00+00:00"));
    attach_plan_costs(
        &mut second,
        &s,
        &aligned_cycles(&[("opencode-go", Some(m1))]),
        Some(&test_rate()),
    );
    let m2 = load_cycle(&s, "opencode-go").unwrap();
    let days = m2.days.expect("跨了一次真实重置,该学到周期长度");
    assert!((days - 30.0).abs() < 0.2, "11-03 → 12-03 是 30 天,实测学到 {days}");
    let p = plan_at(&second, "opencode-go").unwrap();
    assert!(p.learned);
    assert!((p.days - days).abs() < 1e-9);
}

/// WorkBuddy 改走网页登录(裁决 1A/3B)之后的界面口径:没登录时说"未登录 WorkBuddy
/// 账号",auth_source 用 "web" 让前端出登录按钮。凭据的两种形态分开报 ——
/// 会话是从宿主浏览器 profile 里现取的,cookie 前缀是它的标志(2026-10-08)。
#[test]
fn web_session_channel_reports_login_state_not_local_files() {
    let def = providers::find("workbuddy").expect("workbuddy 渠道存在");
    assert_eq!(def.auth, AuthKind::WebSession);
    assert_eq!(placeholder(def).auth_source, "web");
    assert_eq!(auth_label(def, None), "未登录 WorkBuddy 账号");
    let session = appauth::Credential {
        token: "cookie:session=A; session_2=B".into(),
        source: String::new(),
        account_id: None,
        device_id: None,
    };
    assert_eq!(auth_label(def, Some(&session)), "浏览器网页会话");
    let pasted = appauth::Credential {
        token: "eyJhbGciOi.abc".into(),
        source: String::new(),
        account_id: None,
        device_id: None,
    };
    assert_eq!(auth_label(def, Some(&pasted)), "手动粘贴的密钥");
    // 过期指引必须指对方向:WorkBuddy 现在首选桌面端(它会自动续期),
    // 网页会话那条仍说清"约 7 天属正常",免得每次过期都被当成平台又改了接口
    let msg = expired_credential_msg(def).expect("WorkBuddy 要有过期指引");
    assert!(msg.contains("桌面端"), "要先指向会自续期的桌面端:{msg}");
    assert!(msg.contains("重新登录"), "网页会话那条也要留着:{msg}");
}

/// 另外两类渠道不能被这次改造带偏:本机登录型仍说"打开一次 X",密钥型不瞎指路。
#[test]
fn other_auth_kinds_keep_their_own_wording() {
    let trae = providers::find("trae").expect("trae 渠道存在");
    assert_eq!(placeholder(trae).auth_source, "app");
    assert_eq!(auth_label(trae, None), "未检测到 Trae 登录信息");
    assert_eq!(
        expired_credential_msg(trae).as_deref(),
        Some("登录状态已失效 · 打开一次 Trae 即可")
    );

    let codex = providers::find("codex").expect("codex 渠道存在");
    assert_eq!(placeholder(codex).auth_source, "app");

    let ds = providers::find("deepseek").expect("deepseek 渠道存在");
    assert_eq!(placeholder(ds).auth_source, "keyring");
    assert_eq!(auth_label(ds, None), "Windows 凭据管理器");
    assert_eq!(expired_credential_msg(ds), None);
}

/// Qoder 两版的 `source` 自带区域名,展示层不能把 app.label() 拼两遍。
/// 2026-11-10 真机截图:「已读取本机登录 · Qoder · Qoder · sebird」。
#[test]
fn qoder_auth_label_does_not_repeat_channel_name() {
    let cred = |source: &str| appauth::Credential {
        token: "dummy".into(),
        source: source.into(),
        account_id: None,
        device_id: None,
    };
    let qoder = providers::find("qoder").expect("qoder 渠道存在");
    let cn = providers::find("qoder-cn").expect("qoder-cn 渠道存在");
    assert_eq!(auth_label(qoder, Some(&cred("Qoder · sebird"))), "Qoder · sebird");
    assert_eq!(
        auth_label(cn, Some(&cred("Qoder 国内版 · 里怪兽"))),
        "Qoder 国内版 · 里怪兽"
    );
    // 账号名读不到时 source 只有区域名,同样不许拼成「Qoder · Qoder」
    assert_eq!(auth_label(qoder, Some(&cred("Qoder"))), "Qoder");
    assert_eq!(auth_label(cn, Some(&cred("Qoder 国内版"))), "Qoder 国内版");
    // 国际版标签是国内版标签的前缀:不能把后者误当成「重复前缀」剥掉
    assert_eq!(
        auth_label(qoder, Some(&cred("Qoder 国内版 · 里怪兽"))),
        "Qoder · Qoder 国内版 · 里怪兽"
    );
    // 其它 app 型渠道的两段式(安装目录 / 凭据形态)保持原样
    let trae = providers::find("trae").expect("trae 渠道存在");
    assert_eq!(auth_label(trae, Some(&cred("TRAE SOLO CN"))), "Trae · TRAE SOLO CN");
    let codex = providers::find("codex").expect("codex 渠道存在");
    assert_eq!(auth_label(codex, Some(&cred("CLI 登录态"))), "Codex · CLI 登录态");
    assert_eq!(auth_label(codex, Some(&cred(""))), "本机 Codex");
}

#[test]
fn month_start_is_first_day() {
    let ts = local_month_start();
    let d = Local.timestamp_opt(ts, 0).unwrap();
    assert_eq!(d.day(), 1);
    assert_eq!(d.hour(), 0);
    assert_eq!(d.minute(), 0);
}

/// 上月起点:1 号零点,且早于本月起点;跨年时(1 月)要落到去年 12 月。
#[test]
fn prev_month_start_is_first_day_of_previous_month() {
    let prev = local_prev_month_start();
    let cur = local_month_start();
    let p = Local.timestamp_opt(prev, 0).unwrap();
    let c = Local.timestamp_opt(cur, 0).unwrap();
    assert_eq!(p.day(), 1);
    assert!(prev < cur);
    let (py, pm) = (p.year(), p.month());
    let (cy, cm) = (c.year(), c.month());
    assert!((pm == cm - 1 && py == cy) || (cm == 1 && pm == 12 && py == cy - 1));
}

#[test]
fn week_start_is_monday() {
    let ts = local_week_start();
    let d = Local.timestamp_opt(ts, 0).unwrap();
    assert_eq!(d.weekday(), Weekday::Mon);
    assert_eq!(d.hour(), 0);
}

#[test]
fn today_start_is_before_now_and_same_day() {
    let ts = local_midnight_today();
    let now = Local::now();
    assert!(ts <= now.timestamp());
    let d = Local.timestamp_opt(ts, 0).unwrap();
    assert_eq!(d.day(), now.day());
    assert_eq!(d.hour(), 0);
}

/// 申请制:按"上次申请 + 最短间隔"倒计时,到点变成可申请。
#[test]
fn claim_counts_down_until_eligible() {
    let cfg = ClaimConfig {
        enabled: true,
        amount: 200.0,
        min_interval_days: 14,
        ..Default::default()
    };
    let now = 1_700_000_000;
    let eight_days_ago = now - 8 * 86_400;

    let s = claim_state(&cfg, Some(eight_days_ago), now, Some(19.0), None).unwrap();
    assert_eq!(s.days_until_eligible, Some(6));
    assert!(!s.eligible);
    assert_eq!(s.source, "auto");
    assert_eq!(s.amount, 200.0);

    // 满 14 天 → 可申请
    let s = claim_state(&cfg, Some(now - 14 * 86_400), now, Some(19.0), None).unwrap();
    assert_eq!(s.days_until_eligible, Some(0));
    assert!(s.eligible);
}

/// 手动修正优先;但如果快照检测到比"手动写入时刻"更晚的跳升(又申请了一次),自动值接管。
#[test]
fn manual_fix_wins_until_a_newer_jump_appears() {
    let now = 1_700_000_000;
    let manual_day = now - 3 * 86_400;
    let cfg = ClaimConfig {
        enabled: true,
        amount: 200.0,
        min_interval_days: 14,
        manual_last_at: Some(manual_day),
        manual_set_at: Some(now - 2 * 86_400), // 两天前手动改的
    };

    // 自动检测到的是"手动之前"的跳升 → 用手动值
    let older_jump = now - 5 * 86_400;
    let s = claim_state(&cfg, Some(older_jump), now, Some(50.0), None).unwrap();
    assert_eq!(s.last_claim_at, Some(manual_day));
    assert_eq!(s.source, "manual");

    // 手动之后又跳升了一次 → 用自动值
    let newer_jump = now - 86_400;
    let s = claim_state(&cfg, Some(newer_jump), now, Some(190.0), None).unwrap();
    assert_eq!(s.last_claim_at, Some(newer_jump));
    assert_eq!(s.source, "auto");
    assert_eq!(s.days_until_eligible, Some(13));
}

/// 撑不到下次可申请:按当前速度余额只够 2 天,但还要等 6 天 → 预警。
#[test]
fn shortage_risk_when_balance_runs_out_before_eligible() {
    let cfg = ClaimConfig {
        enabled: true,
        amount: 200.0,
        min_interval_days: 14,
        ..Default::default()
    };
    let now = 1_700_000_000;
    let last = now - 8 * 86_400;

    let s = claim_state(&cfg, Some(last), now, Some(20.0), Some(10.0)).unwrap();
    assert_eq!(s.days_of_balance, Some(2.0));
    assert!(s.shortage_risk);

    // 消耗慢就够用
    let s = claim_state(&cfg, Some(last), now, Some(200.0), Some(10.0)).unwrap();
    assert!(!s.shortage_risk);
}

/// 端到端:快照里出现"余额跳升" → 自动识别为一次申请,并算出倒计时。
#[test]
fn claim_detects_an_application_from_snapshot_jumps() {
    let s = Store::open_memory().unwrap();
    let now = now_ts();
    let applied_at = now - 12 * 86_400;
    // 申请前余额 19,申请后补到 200,又花到 18.99
    s.record("4sapi", applied_at, Some(19.0), None, None, "amount").unwrap();
    s.record("4sapi", applied_at + 300, Some(200.0), None, None, "amount").unwrap();
    s.record("4sapi", now - 60, Some(18.99), None, None, "amount").unwrap();

    let cfg = Config::default();
    let def = providers::PROVIDERS
        .iter()
        .find(|d| d.id == "4sapi")
        .expect("4sapi 应在渠道表里");

    let st = claim_for(def, &cfg, &s, Some(18.99), "", now).expect("4sapi 默认开启申请制");
    assert_eq!(st.source, "auto");
    assert_eq!(st.last_claim_at, Some(applied_at + 300));
    // 按整天向下取整:差 5 分钟不满 12 天 → 算 11 天,还剩 3 天(宁可保守,
    // 也不能提前说"可申请"—— 用户去申请会被拒)
    assert_eq!(st.days_until_eligible, Some(3));
    assert!(!st.eligible);

    // 小额抖动不算申请
    s.record("hapi", now - 100, Some(50.0), None, None, "amount").unwrap();
    s.record("hapi", now - 90, Some(55.0), None, None, "amount").unwrap();
    let mut cfg2 = Config::default();
    cfg2.claim_channels.insert("hapi".into(), ClaimConfig { enabled: true, ..Default::default() });
    let def_h = providers::PROVIDERS.iter().find(|d| d.id == "hapi").unwrap();
    let st = claim_for(def_h, &cfg2, &s, Some(55.0), "", now).unwrap();
    assert_eq!(st.source, "none");
}

/// 未启用 / 未记录到申请时间:不硬编倒计时。
#[test]
fn claim_without_config_or_history_is_reported_as_such() {
    let off = ClaimConfig { enabled: false, ..Default::default() };
    assert!(claim_state(&off, Some(1), 2, Some(1.0), None).is_none());

    let on = ClaimConfig { enabled: true, ..Default::default() };
    let s = claim_state(&on, None, 1_700_000_000, Some(18.99), None).unwrap();
    assert_eq!(s.last_claim_at, None);
    assert_eq!(s.days_until_eligible, None);
    assert!(!s.eligible);
    assert_eq!(s.source, "none");
}

/// 换域名重试的策略:网络/5xx/429 才值得换;凭据被拒、其它 4xx 与业务失败不换。
#[test]
fn only_transport_and_server_failures_retry() {
    assert!(FailKind::Transport.retryable());
    assert!(FailKind::Server.retryable());
    assert!(!FailKind::Auth.retryable());     // 401/403:换域名也是同一答案,该换凭据
    assert!(!FailKind::Client.retryable());   // 404/400:路径问题,换域名同一答案
    assert!(!FailKind::Business.retryable()); // code:false:服务器已明确答复
}

/// 尝试顺序:上次成功的优先,其次主地址,再依次补备用,且不重复。
#[test]
fn attempt_order_puts_last_good_first_and_dedups() {
    let fbs = ["https://b/x", "https://c/x"];
    assert_eq!(attempt_order("https://a/x", &fbs, None), vec!["https://a/x", "https://b/x", "https://c/x"]);
    assert_eq!(attempt_order("https://a/x", &fbs, Some("https://c/x")), vec!["https://c/x", "https://a/x", "https://b/x"]);
    // last_good 就是主地址时不产生重复项
    assert_eq!(attempt_order("https://a/x", &fbs, Some("https://a/x")), vec!["https://a/x", "https://b/x", "https://c/x"]);
}

#[test]
fn week_start_not_after_today_start() {
    assert!(local_week_start() <= local_midnight_today());
}

/// 近 30 天到期的合计:只算未来 30 天内的,已过期的不算,更远的不算。
#[test]
fn expiring_soon_sums_next_30_days() {
    let now = 1_700_000_000;
    let day = 86_400;
    let list = vec![
        Expiring { at: now - day, amount: 5.0, label: "已过期".into() },
        Expiring { at: now + 3 * day, amount: 10.0, label: "a".into() },
        Expiring { at: now + 20 * day, amount: 2.5, label: "b".into() },
        Expiring { at: now + 40 * day, amount: 100.0, label: "c".into() },
    ];
    let s = expiring_soon(&list, now).expect("30 天内有到期");
    assert_eq!(s.amount, 12.5);
    assert_eq!(s.at, now + 3 * day, "取最近的那天");

    // 只有 30 天外的 → 没有"近期过期"
    assert!(expiring_soon(&list[3..], now).is_none());
    // 空列表 / 已过期 → 也是 None(不能报 0,界面会渲染成"有 0 分要过期")
    assert!(expiring_soon(&[], now).is_none());
    assert!(expiring_soon(&list[..1], now).is_none());
}

/// 换凭据的判定:只有服务器明确拒绝这份凭据(401/403/407)才换下一份登录态。
#[test]
fn only_auth_failures_try_another_credential() {
    assert!(another_credential_may_help(Some(FailKind::Auth)));
    assert!(!another_credential_may_help(Some(FailKind::Client)));
    assert!(!another_credential_may_help(Some(FailKind::Transport)));
    assert!(!another_credential_may_help(Some(FailKind::Server)));
    assert!(!another_credential_may_help(Some(FailKind::Business)));
    assert!(!another_credential_may_help(None));
}

/// 401 必须永远是 `Auth`,**不管响应体是不是 JSON**。
///
/// 2026-10-20 真机(user diag.log):WorkBuddy 的 401 由网关回了一页 HTML,
/// 旧代码在"响应不是合法 JSON"那支一律给 `Server` → 只换域名、不换凭据 →
/// 明明已经解出桌面端令牌,循环却停在一份作废的手动令牌上,三天取不到数。
#[test]
fn auth_status_wins_over_body_shape() {
    // 状态码口径本身
    assert_eq!(kind_for_status(401), FailKind::Auth);
    assert_eq!(kind_for_status(403), FailKind::Auth);
    assert_eq!(kind_for_status(407), FailKind::Auth);
    assert_eq!(kind_for_status(429), FailKind::Server);
    assert_eq!(kind_for_status(500), FailKind::Server);
    assert_eq!(kind_for_status(404), FailKind::Client);
    assert_eq!(kind_for_status(400), FailKind::Client);

    // 关键那条:Auth 才允许换凭据
    assert!(
        another_credential_may_help(Some(kind_for_status(401))),
        "401 必须触发换下一份凭据(否则桌面端令牌永远轮不到)"
    );
    // 而 5xx 仍然不该换凭据(换域名才有意义)
    assert!(!another_credential_may_help(Some(kind_for_status(503))));
}

/// 被 401 拒过的凭据要沉到末尾,**但不能被丢掉**:用户重新登录后它还会被再试一次。
/// 2026-10-20 真机:一份早已作废的手动令牌排在桌面端凭据前面,每轮先白打一个 401。
#[test]
fn rejected_credentials_are_demoted_not_dropped() {
    let mk = |t: &str, src: &str| appauth::Credential {
        token: t.to_string(),
        source: src.to_string(),
        account_id: None,
        device_id: None,
    };
    let manual = mk("manual-dead-token", "");
    let desktop = mk("desktop:live-token", "WorkBuddy 桌面端");
    let now = 1_800_000_000i64;

    // 被拒的是手动那份 → 它沉底,桌面端排到第一
    let mut rej: HashMap<String, i64> = HashMap::new();
    rej.insert(key_fp("manual-dead-token"), now - 60);
    let got = demote_rejected(
        vec![manual.clone(), desktop.clone()],
        &rej,
        now,
        |t| key_fp(t),
    );
    assert_eq!(got[0].token, "desktop:live-token", "可用那份要排第一");
    assert_eq!(got[1].token, "manual-dead-token", "被拒的沉到末尾");
    assert_eq!(got.len(), 2, "**不许丢** —— 丢了用户就再也看不到那份凭据的状态");

    // 超过 TTL → 回到原位再试一次(可能是瞬时 401,也可能用户已重新登录)
    let got = demote_rejected(
        vec![manual.clone(), desktop.clone()],
        &rej,
        now + REJECTED_TTL,
        |t| key_fp(t),
    );
    assert_eq!(got[0].token, "manual-dead-token", "TTL 过后要恢复原顺序");

    // 没有任何拒绝记录 → 原样返回
    let got = demote_rejected(
        vec![manual.clone(), desktop.clone()],
        &HashMap::new(),
        now,
        |t| key_fp(t),
    );
    assert_eq!(got[0].token, "manual-dead-token");
    assert_eq!(got[1].token, "desktop:live-token");
}

/// F2:失败且真的发过请求 → 清掉"上次成功地址";成功不清、没发过请求也不清。
/// 以前清备忘这一支永远进不去(失败时拿不到 attempted 地址),渠道会被钉在
/// 一个挂掉的备用镜像上,每轮先白等满超时。
#[test]
fn failed_attempt_clears_the_url_memo_but_success_keeps_it() {
    assert!(should_clear_memo(false, true), "失败 + 发过请求 → 清备忘");
    assert!(!should_clear_memo(true, true), "成功时备忘正要写进去,不能清");
    assert!(!should_clear_memo(false, false), "没发过请求就没什么可清的");
    assert!(!should_clear_memo(true, false));
}

/// F3:没有 `main: true` 的窗口时宁可不学 —— 不能拿 5 小时窗/周窗的间隔当包月周期。
#[test]
fn main_window_reset_needs_a_main_window() {
    let def = providers::find("opencode-go").unwrap();
    let mut v = placeholder(def);
    v.windows = vec![
        providers::Window {
            key: "5h".into(),
            label: "5 小时".into(),
            percent: 10.0,
            remain_percent: 90.0,
            status: "ok".into(),
            resets_at: Some("2026-11-03T00:00:00+00:00".into()),
            main: false,
        },
        providers::Window {
            key: "weekly".into(),
            label: "周".into(),
            percent: 20.0,
            remain_percent: 80.0,
            status: "ok".into(),
            resets_at: Some("2026-11-10T00:00:00+00:00".into()),
            main: false,
        },
    ];
    assert!(main_window_reset(&v).is_none(), "没有主窗就该返回 None");

    // 有主窗时照旧取它(而不是顺手拿第一个)
    let mut w = placeholder(def);
    w.windows = vec![
        providers::Window {
            key: "5h".into(),
            label: "5 小时".into(),
            percent: 10.0,
            remain_percent: 90.0,
            status: "ok".into(),
            resets_at: Some("2026-11-03T00:00:00+00:00".into()),
            main: false,
        },
        providers::Window {
            key: "monthly".into(),
            label: "周期".into(),
            percent: 20.0,
            remain_percent: 80.0,
            status: "ok".into(),
            resets_at: Some("2026-12-03T00:00:00+00:00".into()),
            main: true,
        },
    ];
    assert_eq!(
        main_window_reset(&w),
        Some(
            chrono::DateTime::parse_from_rfc3339("2026-12-03T00:00:00+00:00")
                .unwrap()
                .timestamp()
        )
    );
}

/// F4:本地没有 0 点(DST 造出的不存在的时刻)时,退路必须是**请求的那一天**的
/// UTC 零点,不能拿"今天"顶替 —— 否则周一/月初/上月初全算错。
#[test]
fn midnight_fallback_uses_the_requested_day_not_today() {
    let want = |y, m, d| {
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0)
            .unwrap()
            .timestamp()
    };
    assert_eq!(
        resolve_midnight(chrono::LocalResult::None, 2026, 3, 3),
        want(2026, 3, 3)
    );
    assert_eq!(
        resolve_midnight(chrono::LocalResult::None, 2020, 1, 2),
        want(2020, 1, 2),
        "请求的是哪一天就用哪一天"
    );
    // 正常/歧义(DST 回拨)时仍按 earliest 取第一段,不动行为
    let t = Local.with_ymd_and_hms(2026, 3, 3, 0, 0, 0).unwrap();
    assert_eq!(
        resolve_midnight(chrono::LocalResult::Single(t), 2026, 3, 3),
        t.timestamp()
    );
    let later = t + Duration::hours(1);
    assert_eq!(
        resolve_midnight(chrono::LocalResult::Ambiguous(t, later), 2026, 3, 3),
        t.timestamp(),
        "0 点被重复时取第一段"
    );
}

/// F5:响应体的上限在**边收边判**时就生效,不是等整包读进内存之后再判。
#[test]
fn body_cap_is_enforced_while_collecting_chunks() {
    let mut out: Vec<u8> = Vec::new();
    assert!(push_chunk(&mut out, b"12345", 10).is_ok());
    assert_eq!(out, b"12345");
    assert!(push_chunk(&mut out, b"12345", 10).is_ok());
    assert_eq!(out.len(), 10);
    let e = push_chunk(&mut out, b"x", 10).unwrap_err();
    assert!(e.contains("响应过大"), "{e}");
    assert_eq!(out.len(), 10, "超限的那块不许拼进去");
    // 单块就超限也要立刻报错(分块传输时第一块就可能已经超了)
    let mut first: Vec<u8> = Vec::new();
    assert!(push_chunk(&mut first, &vec![0u8; 11], 10).is_err());
    assert!(first.is_empty());
}
