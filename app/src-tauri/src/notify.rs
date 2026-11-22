use super::*;

// ───────────────────── 跌破红线的系统通知(裁决 3A) ─────────────────────

/// 已通知过的「渠道 + 本地日」:同一天里同一个渠道只响一次。
///
/// **落库**:以前只放内存,结果每次重启都可能为同一个已经告急了一整天的渠道
/// 再弹一次(用户看到的就是"一下午好几次")。现在启动时从 settings 读回,
/// 同一天同一渠道无论重启多少次、中间数据怎么抖动,都只提醒一次。
pub(crate) static NOTIFIED_CRIT: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
/// 上一轮的"红线集合",用来识别**跌破的那一刻**而不是持续低位期间每轮都响。
pub(crate) static PREV_CRIT: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);
/// 通知去重状态的落库键:JSON 对象,渠道 id → 本地日(YYYY-MM-DD)。
pub(crate) const CRIT_NOTIFIED_KEY: &str = "crit_notified_v1";

/// 读出去重状态(进程内缓存;第一次调用时从库里加载)。
pub(crate) fn notified_crit_map(app: &tauri::AppHandle) -> HashMap<String, String> {
    let mut g = NOTIFIED_CRIT.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(m) = g.as_ref() {
        return m.clone();
    }
    let store = app.state::<AppState>().store.clone();
    let raw = lock_rw(&store)
        .get_setting(CRIT_NOTIFIED_KEY)
        .ok()
        .flatten();
    let m: HashMap<String, String> = raw
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    *g = Some(m.clone());
    m
}

/// 写回去重状态:先更新进程内缓存,再落库。写盘失败不影响本轮通知。
pub(crate) fn save_notified_crit(app: &tauri::AppHandle, m: &HashMap<String, String>) {
    *NOTIFIED_CRIT.lock().unwrap_or_else(|p| p.into_inner()) = Some(m.clone());
    let json = match serde_json::to_string(m) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("红线通知去重状态序列化失败: {e}");
            return;
        }
    };
    let store = app.state::<AppState>().store.clone();
    // 先接住结果再判错:MutexGuard 的临时值要比 `store` 先析构,
    // 否则借用会活过 store(E0597)。
    let saved = lock_rw(&store).set_setting(CRIT_NOTIFIED_KEY, &json);
    if let Err(e) = saved {
        eprintln!("保存红线通知去重状态失败: {e}");
    }
}

/// 渠道算不算"跌破红线"。**与前端 `toneOf()` 逐条同口径** —— 两边判据必须一致,
/// 否则会出现"界面已经飘红、通知却说没事"这种自相矛盾。
///   百分比型:Codex 看最紧窗,其它看主窗;金额型:剩余 / 本轮额度;
///   积分型:不看余额,只看近 7 天将过期的占比(界面上 >= 30% 就是 bad);
///   取不到数 / 已隐藏 一律不算 —— 失联不等于欠费,这是本产品的第一条红线。
pub(crate) fn crit_now(v: &ChannelView, cfg: &Config) -> bool {
    use tokenscope_core::providers::Kind;
    if !v.has_key || !v.valid || v.hidden {
        return false;
    }
    match v.kind {
        Kind::Percent => tokenscope_core::view::notification_window(v)
            .map(|w| (w.remain_percent / 100.0).clamp(0.0, 1.0) < cfg.crit_percent / 100.0)
            .unwrap_or(false),
        Kind::Amount => {
            // 余额真的见底(0.00 或负)是最高一级警报,与有没有额度分母无关
            if v.remaining.map(|r| r <= 0.0).unwrap_or(false) {
                return true;
            }
            match (v.remaining, v.claim.as_ref().map(|c| c.amount)) {
                (Some(r), Some(d)) if d > 0.0 => (r / d).clamp(0.0, 1.0) < cfg.crit_percent / 100.0,
                // 充值型没有分母:界面不染红,通知也不该自作聪明
                _ => false,
            }
        }
        Kind::Points => {
            // 上游没给余额数值(None)不等于"0 分" —— 那是失联,不是欠费。
            // Amount 分支一直是对的(:1000 用 map+unwrap_or(false)),积分分支以前写成
            // unwrap_or(0.0),会把"读不到数值"报成"积分已用尽"(第三轮体检 P2-③)。
            let Some(r) = v.remaining else {
                return false;
            };
            if r <= 0.0 {
                return true; // 积分见底(界面判 ok,但"没分了"值得说一声)——
                            // 与界面不一致的地方只有这里,注释写明:通知更保守
            }
            let within: f64 = v
                .expiring
                .iter()
                .filter(|e| e.at <= now_ts() + 7 * 86_400)
                .map(|e| e.amount)
                .sum();
            within / r >= 0.3
        }
    }
}

/// 通知正文(人话,带渠道名与具体数字)。
pub(crate) fn crit_body(v: &ChannelView) -> String {
    use tokenscope_core::providers::Kind;
    match v.kind {
        Kind::Percent => {
            let window = tokenscope_core::view::notification_window(v);
            let pct = window
                .map(|w| w.remain_percent)
                .unwrap_or(0.0);
            if v.id == "codex" {
                if let Some(w) = window {
                    return format!("{} {}额度剩 {:.0}% · 打开挂件查看", v.name, w.label, pct);
                }
            }
            format!("{} 额度剩 {:.0}% · 打开挂件查看", v.name, pct)
        }
        Kind::Amount => match (v.remaining, v.claim.as_ref().map(|c| c.amount)) {
            (Some(r), Some(d)) if d > 0.0 => format!(
                "{} 剩 ¥{:.2} / 本轮 ¥{:.0} · 打开挂件查看",
                v.name,
                r,
                d
            ),
            (Some(r), _) => format!("{} 余额只剩 ¥{:.2} · 打开挂件查看", v.name, r),
            _ => format!("{} 余额告急 · 打开挂件查看", v.name),
        },
        Kind::Points => {
            // crit_now 已经保证"没有数值不报警",这里拿到的一定是数字;
            // 兜底那一行只是万一有别的路径进来时,别说出"已用尽 0 分"这种话。
            let Some(r) = v.remaining else {
                return format!("{} 积分余额读不到 · 打开挂件查看", v.name);
            };
            if r <= 0.0 {
                format!("{} 积分已用尽 · 打开挂件查看", v.name)
            } else {
                let within: f64 = v
                    .expiring
                    .iter()
                    .filter(|e| e.at <= now_ts() + 7 * 86_400)
                    .map(|e| e.amount)
                    .sum();
                format!("{} 有 {:.0} 分将在 7 天内过期(共 {:.0} 分) · 打开挂件查看", v.name, within, r)
            }
        }
    }
}

/// 每轮取数后检查"刚刚跌破红线"的渠道,发一条系统通知。
/// 三种克制:只在**跌破那一刻**、**同一天同一渠道一次**、**按渠道各自记账**。
/// 去重状态落库,重启后不会把今天已经响过的渠道再响一遍。
pub(crate) fn maybe_notify_crit(app: &tauri::AppHandle, views: &[ChannelView]) {
    let cfg = lock_rw(&app.state::<AppState>().config).clone();
    let cur: HashMap<String, bool> = views
        .iter()
        .map(|v| (v.id.clone(), crit_now(v, &cfg)))
        .collect();
    let prev = PREV_CRIT
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .unwrap_or_default();
    *PREV_CRIT.lock().unwrap_or_else(|p| p.into_inner()) = Some(cur.clone());
    // 第一轮只记基线:刚启动就把一堆"早就在红线以下"的旧账全报一遍最烦人
    if !cfg.notify_on_crit || prev.is_empty() {
        return;
    }
    let today = tokenscope_core::fetch::local_day_string();
    let mut notified = notified_crit_map(app);
    let mut dirty = false;
    for v in views {
        let was = prev.get(&v.id).copied().unwrap_or(false);
        let now = cur.get(&v.id).copied().unwrap_or(false);
        if !now || was {
            continue;
        }
        // **同一天同一渠道只响一次**:中间数据抖动、进程重启都不再重复。
        if notified.get(&v.id).map(|d| *d == today).unwrap_or(false) {
            continue;
        }
        notified.insert(v.id.clone(), today.clone());
        dirty = true;
        if let Err(e) = app
            .notification()
            .builder()
            .title("Grandettoken · 余额预警")
            .body(crit_body(v))
            .show()
        {
            // 通知发不出去(平台限制/权限)不该影响取数;如实记一行
            eprintln!("发送系统通知失败: {e}");
        }
        // 面板侧同步一条:前端据此播预警音(如果开了)并弹应用内提示
        let _ = app.emit(
            "crit-alert",
            serde_json::json!({ "id": v.id, "name": v.name, "body": crit_body(v) }),
        );
        tokenscope_core::diaglog::write_keyed(
            "notify",
            &format!("crit:{}", v.id),
            &format!("发出余额预警: {}", crit_body(v)),
        );
        // 通知本体不回跳(插件不支持动作),闪一下任务栏是最接近的替代:
        // 窗口藏着时"有人注意到"的概率比静默高得多。
        if let Some(w) = app.get_webview_window("main") {
            if !w.is_visible().unwrap_or(false) {
                let _ = w.request_user_attention(Some(tauri::UserAttentionType::Informational));
            }
        }
    }
    if dirty {
        // 只保留今天的记录:地图大小永远不超过渠道数,不随天数增长
        notified.retain(|_, day| *day == today);
        save_notified_crit(app, &notified);
    }
}

