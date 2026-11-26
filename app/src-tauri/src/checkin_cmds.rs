use super::*;

// ─────────────── 每日签到(Trae / WorkBuddy 加油站)───────────────
//
// 设计见 `docs/design/design-auto-checkin-2026-10-13.html`。三条红线:
//   ① 独立开关、默认关:关掉就一个请求都不发(与 hidden_channels 同口径);
//   ② 判因只看 code / HTTP / 域名,**永远不看 message 文案**;
//   ③ 签到的状态不参与余额渲染 —— 签到失败不可能让余额显示成 0。
//
// 请求走**强制直连**(`checkin::direct_client`,不跟随系统代理):Trae 侧 9074 的成因之一
// 是按出口 IP 排队限流,而系统代理/机场 IP 正是那类高发出口(设计稿裁决 4A)。

/// 界面要的每个渠道的签到状态。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckinState {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) enabled: bool,
    pub(crate) auto: bool,
    /// 今天的状态短码(claimed / already / retry:ready / retry:device / auth / inactive / shape / failed)
    pub(crate) outcome: String,
    pub(crate) credits: Option<f64>,
    pub(crate) attempts: i64,
    pub(crate) last_try_at: Option<i64>,
    pub(crate) next_try_at: Option<i64>,
    /// 现在能不能点「领积分」
    pub(crate) can_try: bool,
    /// 要去用户动手(凭据失效 / 结构变了)
    pub(crate) needs_user: bool,
    /// 拿不到凭据的原因(界面直接显示)
    pub(crate) note: Option<String>,
    /// 本月自动领到的合计(只算真领到的)
    pub(crate) month_credits: f64,
    /// 连续签到天数(只有 Qoder 给;没有就是 None,前端不显示)
    pub(crate) streak_days: Option<i64>,
    /// 近 7 天流水
    pub(crate) log: Vec<tokenscope_core::store::CheckinRow>,
    /// Qoder 最近检测到的活动;即使当前活动不能由挂件领取,也展示监测结果。
    pub(crate) latest_activity: Option<checkin::QoderActivity>,
    /// 跨渠道统一的最近活动状态。
    pub(crate) activity_state: Option<checkin::ActivityState>,
    pub(crate) last_activity_check_at: Option<i64>,
    /// Qoder 返回的活动列表，用于把一个账号下的多个活动分别展示。
    pub(crate) activity_history: Vec<checkin::QoderActivity>,
    /// **今天这个渠道签完了吗**(claimed / already / inactive)。
    ///
    /// 为什么由后端给、不让前端算:Qoder 的周期日**从 10:00 起算**
    /// (`CheckinDef::day_roll_hour`),前端拿 `new Date()` 算本地日,10:00 之前
    /// 必然对不上 —— 明明签了也显示没签。凡是"哪一天"的判断,后端算过就别在前端再算一遍
    /// (与 0.1.49 的 `last_auth_version` 同一个思路)。
    ///
    /// 注意这与 `outcome` 不重复:`outcome` 是给"设置页"看的原始短码(含 reason),
    /// 这里是给"首页徽标"用的**已判定结论**,`already` 也算签完。
    pub(crate) done_today: bool,
    /// claim 之后的余额到账核对(**纯证据**,前端只把它写进文案,不改徽标与状态)。
    /// 只透出**今天**的核对单:昨天的 confirmed 跟着今天的 already 显示会变成误导。
    pub(crate) arrival: Option<checkin::ArrivalInfo>,
}

/// 该渠道现在能不能拿到签到凭据;拿不到就把原因说清楚(界面照抄)。
///
/// ## WorkBuddy 的凭据分叉(2026-10-20 修复)
///
/// 以前这里**只**读网页 Cookie 会话,而**取数**那条路(见 `fetch.rs::credentials`)
/// 是「手动密钥 → 桌面端凭据 → 网页会话」。于是出现一个很难看的状态:
/// 桌面端凭据还活着、余额天天出数,签到却因为网页会话失效而天天报 `auth`
/// (本机 10-18/19/20 连续三天就是这个)。
///
/// 现在把顺序对齐成同一套:**先桌面端、后网页会话**。
/// 桌面端凭据活得久(accessToken 约 3 周、refreshToken 约 1 年,且可续),
/// 而网页会话是约 7 天的绝对 TTL —— 后者只能当兜底。
///
/// 桌面端 token 是**裸 JWT**(`desktop_tokens()` 返回的就不带 `desktop:` 前缀 ——
/// 那是取数层的标记),交给 `Cred::Web` 后会走「其余当 Bearer 令牌」那一支,
/// 正是签到接口要的形态。**绝不能把 `desktop:` 前缀带进来**,那会变成
/// `Bearer desktop:xxx`,必然 401。
pub(crate) fn checkin_credentials(app: &tauri::AppHandle, def: &checkin::CheckinDef) -> Result<Vec<checkin::Cred>, String> {
    match def.auth {
        checkin::CheckinAuth::TraeApp => {
            // 双装 Trae(SOLO CN + CN)时可能有两份登录态,其中一份早已作废。
            // 全部返回,由 `run_checkin` 在 401 时逐份往下试 —— 只取 `.next()` 的话,
            // 旧那套装在候选首位,签到就天天落 auth,而取数早就用第二份正常出数
            // (2026-10-23 真机:界面一直喊"打开一次 Trae",打开也没用)。
            let mut out = Vec::new();
            let mut missing_device = false;
            for c in appauth::candidates(appauth::App::Trae) {
                match c.device_id {
                    Some(device_id) => out.push(checkin::Cred::Trae {
                        token: c.token,
                        device_id,
                    }),
                    None => missing_device = true,
                }
            }
            if out.is_empty() {
                return Err(if missing_device {
                    "Trae 的 storage.json 里没读到设备号(签到需要它)".into()
                } else {
                    "没检测到 Trae 登录信息 · 打开一次 Trae 即可".into()
                });
            }
            Ok(out)
        }
        checkin::CheckinAuth::WebSession => {
            // ① 桌面端凭据优先(与取数同口径)。读不到不是错误 —— 没装桌面端很常见。
            //    `desktop_tokens` 内部已处理"上次续期成果 → 桌面端文件 → 过期自动续期"。
            //    可能有多份(区域不同),同样交给 `run_checkin` 逐份试。
            let mut out: Vec<checkin::Cred> = workbuddy::desktop_tokens(now_ts() * 1000)
                .into_iter()
                .map(checkin::Cred::Web)
                .collect();
            // ② 兜底:网页会话(手动粘贴的密钥 / 挂件内登录抓到的 Cookie)
            if let Some(session) = wb_session_of(app, def.id) {
                out.push(checkin::Cred::Web(session));
            }
            if out.is_empty() {
                return Err(format!(
                    "未检测到 {} 登录 · 打开一次桌面端即可(或在本卡片点「重新登录」)",
                    def.name
                ));
            }
            Ok(out)
        }
        checkin::CheckinAuth::QoderApp => {
            let app_kind = if def.id == "qoder-cn" {
                appauth::App::QoderCn
            } else {
                appauth::App::Qoder
            };
            let c = appauth::candidates(app_kind)
                .into_iter()
                .next()
                .ok_or_else(|| {
                    format!("没检测到 {} 登录信息 · 打开一次 Qoder 桌面端即可", def.name)
                })?;
            if def.id == "qoder" {
                let account_id = c.account_id.as_deref().unwrap_or_default();
                if let Some(identity) = tokenscope_core::qoder::machine_identity(account_id) {
                    return Ok(vec![checkin::Cred::QoderMachine {
                        token: c.token,
                        identity,
                    }]);
                }
            }
            Ok(vec![checkin::Cred::Qoder(c.token)])
        }
        checkin::CheckinAuth::MiniMaxApp => {
            // 与取数同一条凭据链(core::minimax::blocking_credential):读 ~/.minimax,
            // 临近过期自动续期(托管模式,可经 MINIMAX_NOWRITEBACK=1 关成只读)。
            // 凭据来源必须与取数一致 —— 两边都走这一个入口。
            match tokenscope_core::minimax::blocking_credential() {
                Some(c) => Ok(vec![checkin::Cred::MiniMax {
                    key: tokenscope_core::minimax::encode_key(&c),
                    uid: c.uid,
                }]),
                None => Err("没检测到 MiniMax Code 登录 · 打开一次 MiniMax Code 即可".into()),
            }
        }
    }
}

/// 只要一份凭据的调用点(活动监测 / 状态探测):取候选表的第一份。
pub(crate) fn checkin_credential(app: &tauri::AppHandle, def: &checkin::CheckinDef) -> Result<checkin::Cred, String> {
    checkin_credentials(app, def)?
        .into_iter()
        .next()
        .ok_or_else(|| format!("{} 没有可用签到凭据", def.name))
}

/// 该渠道的「签到周期日」——幂等键。
///
/// 不能一律用自然日:Qoder 的活动窗口是 10:00 → 次日 09:59,
/// 09:00 查到的"已领"其实是昨天那一期领的,拿自然日当键会判成"今天已领"而跳过今天该领的。
/// 见 `checkin::cycle_day` 的注释。
pub(crate) fn cycle_day_of(id: &str, now: i64) -> String {
    let roll = checkin::find(id).map(|d| d.day_roll_hour).unwrap_or(0);
    checkin::cycle_day(now, roll)
}

/// 当前版本。用来判断一条 `auth` 结论是不是**陈旧**的 —— 见
/// [`checkin::CheckinCfg::blocks_today`] 的说明:v0.1.47 升级后当天仍显示
/// "凭据失效",就是因为旧的 `auth` 把当天挡住了,而凭据链路其实已经修好。
pub(crate) const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 「今天还用不用再试」。三处调用点(状态展示 / 手动按钮 / 自动节拍器)必须同口径,
/// 否则会出现"界面说不能试、节拍器却在试"这种自相矛盾。
pub(crate) fn checkin_blocks_today(c: &checkin::CheckinCfg, today: &str) -> bool {
    c.blocks_today(today, APP_VERSION)
}

/// 「今天有可领取的活动」。与 checkin_state 的展示口径同源:活动态按 period
/// (观测时的周期日)门控 —— 昨天监测到的 claimable 在今天不算事实(报告 L11):
/// 不门控的话,昨天遗留的状态能绕过 done 门多跑一次签到,回查把当天已领的条目
/// 改写成 already,界面上的 last_credits 就丢了。最新一轮监测永远带着今天的 period。
pub(crate) fn activity_ready_today(c: &checkin::CheckinCfg, today: &str) -> bool {
    c.activity_state
        .as_ref()
        .filter(|a| a.period == today)
        .map(|a| a.status == "claimable")
        .unwrap_or(false)
}

pub(crate) fn checkin_state_of(id: &str, cfg: &Config, store: &Store, note: Option<String>) -> CheckinState {
    let def = checkin::find(id);
    let c = cfg.checkin.get(id).cloned().unwrap_or_default();
    let now = now_ts();
    let today = cycle_day_of(id, now);
    let done = checkin_blocks_today(&c, &today);
    let waiting = c.next_try_at.map(|t| t > now).unwrap_or(false);
    let activity_ready = c
        .activity_state
        .as_ref()
        .map(|a| a.status == "claimable")
        .unwrap_or(false);
    // 陈旧的 `auth` **不要当事实显示**:它是对旧版本那一套凭据的判决,凭据链路
    // 可能早就变了(2026-10-20 真机:启动后 5 分钟内界面一直挂着过期的"凭据失效",
    // 把用户引去网页登录,而那条路还被网关拒)。在节拍器重新判定回来之前,
    // 如实说"正在重新确认",且**不给「去登录」**。
    //
    // 跨日后的旧短码同样不当事实显示(2026-10-25 真机,修「0 点后界面挂着昨天的已签」):
    // `last_code` 是**周期日作用域**的结论,昨天领过不等于今天领过。以前原样透出,
    // 0 点一过徽标还挂「✓已签/今日已领」,而 doneToday 已是 false —— 同一屏自相矛盾。
    // 现在跨日就报空,界面落回「今天还没签 + 领按钮」,与 doneToday 同口径。
    // Qoder 的 10:00 跨日自动正确:09:59 时周期日还没翻,昨天的结论照常显示。
    let fresh_day = c.last_day.as_deref() == Some(today.as_str());
    let outcome = if !fresh_day {
        String::new()
    } else if c.auth_is_stale(APP_VERSION) {
        "recheck".to_string()
    } else {
        c.last_code.clone().unwrap_or_default()
    };
    let needs_user = outcome.starts_with("auth") || outcome.starts_with("shape");
    let log = store.recent_checkins(id, now - 7 * 86_400, 400).unwrap_or_default();
    let month_credits = store
        // 日期串的兜底返回空串;直接 &today[..7] 会 panic(报告 P2-13)
        .checkin_month_credits(id, today.get(..7).unwrap_or(today.as_str()))
        .unwrap_or(0.0);
    CheckinState {
        id: id.to_string(),
        name: def.map(|d| d.name.to_string()).unwrap_or_else(|| id.to_string()),
        enabled: c.enabled,
        auto: c.auto,
        outcome,
        // 数额与短码同一个作用域:昨天的 +100 不该挂到今天「已领」的文案里
        credits: if fresh_day { c.last_credits } else { None },
        attempts: c.attempts,
        last_try_at: c.last_try_at,
        next_try_at: c.next_try_at,
        can_try: c.enabled && (!done || activity_ready) && !waiting,
        needs_user,
        note,
        month_credits,
        // 首页徽标用:今天没有待办了(已签 / 平台说已签 / 本期无活动)。
        // **用后端的 cycle_day** —— Qoder 的"今天"从 10:00 起算,前端按本地日算必错。
        // 与 can_try / 首页聚合行同一口径:旧的 inactive / manual 换版本后要重判,
        // 不能一边说"已签完"、另一边还给"领取"按钮。
        done_today: done && !activity_ready,
        streak_days: c.last_streak_days,
        log,
        latest_activity: c.latest_activity,
        // 活动状态按 period(观测时的周期日)门控:昨天监测到的「已领取」在今天
        // 不算事实 —— 不门控的话,徽标刚被空短码翻回「待签」,又从旧活动态翻回
        // 「平台已领取」,同一份数据两个口径。最新一轮监测永远带着今天的 period。
        activity_state: c.activity_state.filter(|a| a.period == today),
        last_activity_check_at: c.last_activity_check_at,
        activity_history: c.activity_history,
        arrival: c.arrival.filter(|a| a.day == today && c.last_day.as_deref() == Some(a.day.as_str())),
    }
}

/// 跑一次签到:查状态 → 可领就领 → 领完回查一次 → 触发一次取数做到账核对。
/// 手动点与自动节拍器共用这一条路。
pub(crate) async fn run_checkin(app: &tauri::AppHandle, id: &str, manual: bool) -> Result<checkin::Outcome, String> {
    let def = checkin::find(id).ok_or_else(|| format!("未知签到渠道: {id}"))?;
    // 两道闸门(报告 P0-6)。取数侧一直守着「隐藏 = 不发请求、不读本机登录凭据」
    // (fetch.rs 里那句注释),签到这条路以前谁也没判:渠道被隐藏后每天照样读
    // storage.json / Cookie 罐并对外 POST;手动点「领积分」连开关状态都不看。
    {
        let st = app.state::<AppState>();
        let cfg = lock_rw(&st.config).clone();
        if cfg.hidden_channels.iter().any(|h| h == id) {
            return Err(format!("{} 已隐藏,签到已暂停(想领先在渠道卡上取消隐藏)", def.name));
        }
        if !cfg.checkin.get(id).map(|c| c.enabled).unwrap_or(false) {
            return Err(format!("{} 的签到开关没开", def.name));
        }
    }
    // 在途互斥:节拍器与手动点击不许同时跑同一条渠道(见 CHECKIN_BUSY 注释)。
    // 注意绑到具名变量:绑给 `_` 会立刻 Drop,闸门等于没上。
    let _busy = CheckinBusy::acquire(id).map_err(|_| format!("{} 正在签到,等它跑完再点", def.name))?;
    // 本机凭据要读文件 / 派发主线程,都是阻塞调用:下沉到 spawn_blocking,
    // 否则一个卡住的 Trae storage.json 会占住一个 tokio worker(P1-14 同族)。
    // **可能有多份**:Trae 双装 / WorkBuddy 多区域,旧那份可能早已作废。
    let creds = {
        let h = app.clone();
        tokio::task::spawn_blocking(move || checkin_credentials(&h, def))
            .await
            .map_err(|e| format!("读取签到凭据失败: {e}"))??
    };
    let client = checkin::direct_client();
    let now = now_ts();
    // **签到周期日**(不是自然日):Qoder 的"今天"从 10:00 起算。
    // 拿自然日当幂等键会把 09:59 与 10:01 混成一天,当天那一期就永远领不到。
    let today = cycle_day_of(id, now);

    // 查状态。**用 call_with_body**:Qoder 的连续天数在顶层,需要原始响应;
    // 其他渠道拿到 body 也不用,不额外付成本(同一次请求捎回来而已)。
    //
    // 多份凭据时逐份试:只有 401/403(平台明确拒了这份登录态)才换下一份,
    // 网络/业务失败不换 —— 与取数层同一个判据。只试第一份的话,Trae 装了
    // SOLO CN + CN 两套时,旧那套会让签到天天落 auth,界面一直喊"打开一次 Trae",
    // 而取数其实正用第二份正常出数(2026-10-23 真机)。
    let (cred, mut out, status_body) = {
        let mut picked = None;
        for (i, c) in creds.iter().enumerate() {
            let (o, b) = checkin::call_with_body(&client, def, c, false, None).await;
            if matches!(o, checkin::Outcome::AuthNeeded) && i + 1 < creds.len() {
                tokenscope_core::diaglog::write_keyed(
                    def.id,
                    "cred-switch",
                    &format!("签到换下一份凭据:第 {} 份被平台拒绝", i + 1),
                );
                continue;
            }
            picked = Some((c.clone(), o, b));
            break;
        }
        picked.expect("checkin_credentials 至少返回一份")
    };
    let mut credits: Option<f64> = out.credits();
    // 连续天数:只有 Qoder 的接口给,拿不到就是 None(不编)
    let streak: Option<i64> = status_body.as_ref().and_then(checkin::qoder_streak);
    if let Some(body) = status_body.as_ref() {
        if matches!(id, "qoder" | "qoder-cn") {
            record_qoder_activities(app, id, body, now).await;
        } else {
            let activity = activity_state_from_outcome(id, &out, Some(body), now);
            record_channel_activity(app, id, activity, false).await;
        }
    }

    // 可领 → 领;领完**回查一次**(服务端可能已被客户端自己点掉,那种情况也算成功)
    //
    // Qoder 的可领短码是 `retry:ready:{活动id}`(领取地址要这个 id);
    // Trae / WorkBuddy 是固定的 `retry:ready`。两种都要认。
    //
    // 到账核对的基线:claim **前**最后一份成功快照(纯读库,零网络请求)。
    // 读不到(该渠道还没成功取过数)就不核对 —— 宁可没有证据,不发一个多余请求。
    let mut arrival_base: Option<(i64, f64)> = None;
    if let checkin::Outcome::Retry(m) = &out {
        if m == "ready" || m.starts_with("ready:") {
            let claim_id = checkin::ready_claim_id(m).filter(|s| !s.is_empty());
            arrival_base = {
                let st = app.state::<AppState>();
                let snap = lock_rw(&st.store).latest_snapshot(def.id).ok().flatten();
                snap
            };
            let claimed = checkin::call(&client, def, &cred, true, claim_id).await;
            credits = match &claimed {
                checkin::Outcome::Claimed(c) | checkin::Outcome::Already(c) => c.or(credits),
                _ => credits,
            };
            let (back, back_body) =
                checkin::call_with_body(&client, def, &cred, false, None).await;
            let checked_at = now_ts();
            if let Some(body) = back_body.as_ref() {
                if matches!(id, "qoder" | "qoder-cn") {
                    record_qoder_activities(app, id, body, checked_at).await;
                } else {
                    let activity = activity_state_from_outcome(id, &back, Some(body), checked_at);
                    record_channel_activity(app, id, activity, false).await;
                }
            } else {
                let activity = activity_state_from_outcome(id, &back, None, checked_at);
                record_channel_activity(app, id, activity, false).await;
            }
            // 回查是**唯一的成功证据**。只有平台明确说已签才报成功；还可领取时说明
            // claim 未生效，网络/结构异常时保持“确认中”，不能把请求回执当到账证明。
            out = match &back {
                // 回查说已签:真领到了,顺手把回查里带的积分数补上
                checkin::Outcome::Already(c) => checkin::Outcome::Claimed(c.or(credits)),
                // 回查仍说可领 = 这次领取没生效:降级成可重试,别把这一天记成已领
                checkin::Outcome::Retry(reason)
                    if reason == "ready" || reason.starts_with("ready:") =>
                {
                    checkin::Outcome::Retry("claim 未见生效".into())
                }
                // 回查暂时给不出明确结论，保留平台 claim 回执并按退避再次查询。
                _ => checkin::Outcome::Confirming(credits),
            };
        }
    }

    // 到账核对:立刻取一轮(新包会出现在余额里),失败不影响签到结论。
    // 增量是**纯证据**(checkin::ArrivalInfo):见不到正增量就落 pending,
    // 交给 settle_arrivals 在后续每轮取数里接着核(发放可能延迟/被消耗抵消)。
    // 这里绝不回改 out —— 回查状态才是唯一成功证据。
    let mut arrival: Option<checkin::ArrivalInfo> = None;
    if matches!(out, checkin::Outcome::Claimed(_)) {
        refresh_now(app).await;
        if let Some((base_at, base_bal)) = arrival_base {
            let mut a = checkin::ArrivalInfo::start(base_bal, base_at, today.clone());
            let snap = {
                let st = app.state::<AppState>();
                let s = lock_rw(&st.store).latest_snapshot(def.id).ok().flatten();
                s
            };
            if let Some((_, bal)) = snap {
                a.observe(bal, now_ts(), &today);
            }
            tokenscope_core::diaglog::write_keyed(
                "checkin",
                id,
                &format!(
                    "到账核对:{} · 基线 {}@{} · {}",
                    a.status,
                    a.baseline,
                    a.baseline_at,
                    a.delta
                        .map(|d| format!("增量 +{d}"))
                        .unwrap_or_else(|| "未见增量".into())
                ),
            );
            arrival = Some(a);
        }
    }

    // 落配置 + 落流水(凭据不进去)
    {
        let state = app.state::<AppState>();
        // ⚠ 只取"我要改的那一项",**不整份克隆配置**(2026-10-21 修 N04)。
        // 以前是 lock_rw(&state.config).clone() 拍一份快照,结束时 cfg.save() 把整份
        // 旧快照写回磁盘、*lock_rw = cfg 还把内存态一起覆盖 —— 用户在签到跑的那 30~70 秒里
        // 改的任何设置(关渠道、改刷新间隔)都被悄悄还原。本轮更严重:节拍器从"启动先睡 5 分钟"
        // 改成"启动立刻跑一轮",撞上这个窗口的概率明显变大。
        let notify = lock_rw(&state.config).checkin_notify;
        let mut entry = lock_rw(&state.config).checkin.get(id).cloned().unwrap_or_default();
        let same_day = entry.last_day.as_deref() == Some(today.as_str());
        entry.attempts = if same_day { entry.attempts + 1 } else { 1 };
        entry.last_day = Some(today.clone());
        entry.last_code = Some(out.code());
        if matches!(&out, checkin::Outcome::Confirming(_)) {
            if let Some(activity) = entry.activity_state.as_mut() {
                activity.status = "confirming".into();
                activity.detail = Some("平台已收到领取请求,正在确认状态".into());
            }
        }
        // 记下"判出这条结论的是哪个版本"。换版本后 `blocks_today` 会认为它陈旧、
        // 给一次重新判定的机会 —— 见 checkin.rs 里 last_auth_version 的说明。
        // 除了 `auth`,**`inactive` / `manual` 也要记**:它们不是平台给的既成事实,
        // 而是**我们自己的分类器**给出的结论(哪些活动算"打卡")。规则一改,旧结论
        // 就该重问一次 —— v0.1.57 把 VIEW_DETAILS + CLAIMABLE 从"没活动"改成"有活动",
        // 不重问的话升级后当天仍然显示"本期无活动"(用户看到的就是"改了也没用")。
        if matches!(
            out,
            checkin::Outcome::AuthNeeded
                | checkin::Outcome::Inactive(_)
                | checkin::Outcome::Manual(_)
        ) {
            entry.last_auth_version = Some(APP_VERSION.to_string());
        }
        entry.last_try_at = Some(now);
        entry.last_credits = credits;
        // 到账核对单:这次真 claim 了才写。今天没 claim(比如 already / retry)时
        // **保留**既有单子 —— 上面那次 claim 的证据仍然有效;跨了周期日才清,
        // 否则昨天的 confirmed 会挂在今天的 already 上变成误导。
        entry.arrival = match arrival {
            Some(a) => Some(a),
            None if same_day => entry.arrival.take().filter(|a| a.day == today),
            None => None,
        };
        // 签到落一行诊断(**不含凭据**)。以前这条路一个字都不记,排查只能靠数据库反推 ——
        // 2026-10-21 查"为什么显示不出领了多少分"时才发现:Trae / WorkBuddy 的接口
        // 根本不返回数额,而日志里连这条事实都看不到。现在至少能一眼确认
        // 「平台没给数额」与「我们解析漏了」是哪种。
        tokenscope_core::diaglog::write_keyed(
            "checkin",
            id,
            &format!(
                "签到 {} · 结果={} · 数额={} · 第 {} 次",
                def.name,
                out.code(),
                credits
                    .map(|c| format!("{c}"))
                    .unwrap_or_else(|| "平台未返回".into()),
                entry.attempts
            ),
        );
        // 连续天数:拿不到就**保留上次的值**(None 覆盖成 None 没问题,但别把已知的清掉)
        if streak.is_some() {
            entry.last_streak_days = streak;
        }
        // 退避:retryable 就一律排下一次,间隔由 next_delay 封顶(120→…→1200 秒)。
        // 以前还多判一个 attempts<5,第 6 次起把 next_try_at 置成 None,而节拍器把 None
        // 读成"不在退避中" → 当天改成每 5 分钟硬撞一次,比封顶值还激进 4 倍(报告 P1-2)。
        entry.next_try_at = if out.retryable() {
            Some(now + checkin::next_delay(entry.attempts))
        } else {
            None
        };
        entry.last_message = match &out {
            checkin::Outcome::Inactive(m) | checkin::Outcome::Manual(m)
            | checkin::Outcome::Retry(m) | checkin::Outcome::BadShape(m)
            | checkin::Outcome::Failed(m) => Some(m.clone()),
            checkin::Outcome::Confirming(_) => Some("平台已收到领取请求,正在确认状态".into()),
            _ => None,
        };

        // 需要用户动手(凭据失效 / 接口结构变了)时要留下看得见的提醒:绿色版从托盘
        // 启动时 eprintln 永远没人看到(报告 P2-14 / P3-36)。系统通知要额外引插件,
        // 先走面板事件,由设置里的 checkin_notify 管住。
        //
        // **同一天同一个问题只提示一次**:以前只要 needs_user 就 emit,节拍器/监测
        // 每次撞上都弹,用户一下午会看到好几次。状态码变了(auth → shape)或到了新的
        // 一天,才允许再提醒 —— 键里带周期日,不需要额外的跨日清理。
        let alert_key = format!("{}:{}", out.code(), today);
        let should_alert = out.needs_user()
            && notify
            && entry.last_alert_key.as_deref() != Some(alert_key.as_str());
        if should_alert {
            entry.last_alert_key = Some(alert_key);
            let _ = app.emit(
                "checkin-alert",
                serde_json::json!({
                    "id": id,
                    "name": def.name,
                    "message": entry.last_message.clone().unwrap_or_default(),
                }),
            );
            tokenscope_core::diaglog::write_keyed(
                "notify",
                &format!("checkin-alert:{id}"),
                &format!(
                    "发出签到告警: {} · {}",
                    def.name,
                    entry.last_message.as_deref().unwrap_or("")
                ),
            );
        }
        // 先落流水(独立作用域:不拿着 store 锁去取 config 锁,避免锁序问题)
        {
            let store = lock_rw(&state.store);
            if let Err(e) = store.record_checkin(
                id,
                now,
                &today,
                &out.code(),
                credits,
                entry.attempts,
                entry.last_message.as_deref().unwrap_or(""),
            ) {
                eprintln!("{e}");
            }
        }
        // 再**只**把 checkin[id] 合并进当前配置,过串行闸落盘(报告 B4)。
        // 合并的是 live 配置而不是开头那份快照,所以用户在签到期间改的其它设置
        // 原样保留 —— 这条是 N04 的核心;串行闸保证与其它配置写者不乱序(P3-35)。
        if let Err(e) = save_config_off_main(&state, |live| {
            live.checkin.insert(id.to_string(), entry.clone());
        })
        .await
        {
            eprintln!("签到配置落库失败: {e}");
        }
    }
    let _ = manual;
    // 真领到了才单独发一条:前端据此播"叮"一声(裁决 8A 的两声之一)。
    // 自动签到跑在后台,没有这条事件用户就无从知道"今天领到了"。
    if let checkin::Outcome::Claimed(cr) = &out {
        let _ = app.emit(
            "checkin-claimed",
            serde_json::json!({ "id": id, "name": def.name, "credits": cr }),
        );
        tokenscope_core::diaglog::write_keyed(
            "notify",
            &format!("checkin-claimed:{id}"),
            &format!("发出签到到账提示: {} · +{}", def.name, cr.unwrap_or(0.0)),
        );
    }
    let _ = app.emit("checkin-updated", serde_json::json!({ "id": id }));
    Ok(out)
}

/// 积分到账记录(近 N 天,默认 30)。渠道卡上「查看记录」用。
///
/// 只读 `credit_grants`,不在这里做判定 —— "算不算活跃"的口径在 `core::grants` 里,
/// 界面拿到金额自己按同一套窗口判断,免得两处口径漂移。
#[tauri::command]
pub(crate) async fn credit_grants(
    state: State<'_, AppState>,
    id: String,
    days: Option<i64>,
) -> Result<Vec<tokenscope_core::grants::Grant>, String> {
    let days = days.unwrap_or(30).clamp(1, 90);
    let since = now_ts() - days * 86_400;
    let store = lock_rw(&state.store);
    store.grants_since(&id, since)
}

#[tauri::command]
pub(crate) async fn checkin_state(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Vec<CheckinState>, String> {
    let cfg = lock_rw(&state.config).clone();
    let mut list = {
        let store = lock_rw(&state.store);
        checkin::CHECKINS
            .iter()
            .map(|d| checkin_state_of(d.id, &cfg, &store, None))
            .collect::<Vec<_>>()
    };
    // 把"拿不到凭据"的具体原因填进 note。`checkin_state_of` 的 note 形参以前两个调用点
    // 都传 None,于是界面上那条「去登录」分支永远是死代码,用户只能反复点领取(报告 P2-37)。
    // 探测是阻塞活(读第三方登录文件 / 问 webview 要 Cookie),所以:只对"开着签到且今天
    // 还没签完"的渠道做,并且放阻塞任务里。
    let probes = checkin::CHECKINS.iter().enumerate().map(|(i, def)| {
        let item = &list[i];
        let app = &app;
        let cfg = &cfg;
        async move {
            if !item.enabled {
                return (i, None);
            }
            // 隐藏渠道不能读取本机凭据，探测也必须服从同一规则。
            if cfg.hidden_channels.iter().any(|h| h == def.id) {
                return (i, None);
            }
            let outcome = item.outcome.as_str();
            if outcome.starts_with("claimed") || outcome.starts_with("already") || outcome.starts_with("inactive") {
                return (i, None);
            }
            let handle = app.clone();
            let err = tokio::task::spawn_blocking(move || checkin_credential(&handle, def).err())
                .await
                .unwrap_or(None);
            (i, err)
        }
    });
    for (i, err) in futures::future::join_all(probes).await {
        if let Some(msg) = err { list[i].note = Some(msg); }
    }
    Ok(list)
}

/// 手动领一次(渠道卡上的「领积分」)。
#[tauri::command]
pub(crate) async fn checkin_now(app: tauri::AppHandle, id: String) -> Result<CheckinState, String> {
    // 「现在到底能不能点」后端早就算好了(CheckinState.can_try),前端以前不读它,
    // 于是退避中 / 今天已签的情况下连点 N 次就发 N×3 个请求(报告 P0-6)。
    {
        let st = app.state::<AppState>();
        let cfg = lock_rw(&st.config).clone();
        // 周期日(不是自然日):Qoder 的"今天"从 10:00 起算
        let today = cycle_day_of(&id, now_ts());
        if let Some(c) = cfg.checkin.get(&id) {
            let activity_ready = activity_ready_today(c, &today);
            if checkin_blocks_today(c, &today) && !activity_ready {
                return Err(format!(
                    "今天已处理过({})",
                    c.last_code.clone().unwrap_or_else(|| "已领".to_string())
                ));
            }
            if let Some(t) = c.next_try_at {
                let left = (t - now_ts()).max(0);
                if left > 0 {
                    return Err(if left >= 60 {
                        format!("平台太挤,约 {} 分钟后再试", left / 60)
                    } else {
                        format!("平台太挤,约 {} 秒后再试", left)
                    });
                }
            }
        }
    }
    // 手动点没跑起来时也要留一条流水 —— 节拍器那条路早就有(record_checkin_skip),
    // 手动这条路以前一个字都不记:「点过但没试成」与「没点过」在数据里无法区分。
    // 只记凭据类失败;互斥/隐藏/开关这类「本来就不该跑」的拒绝不记,免得误算退避。
    let out = match run_checkin(&app, &id, true).await {
        Ok(o) => o,
        Err(e) => {
            let gate_like = ["已隐藏", "开关没开", "正在签到", "未知签到渠道"]
                .iter()
                .any(|k| e.contains(k));
            if !gate_like {
                let now = now_ts();
                record_checkin_skip(&app, &id, &cycle_day_of(&id, now), now, &e).await;
            }
            return Err(e);
        }
    };
    let state = app.state::<AppState>();
    let cfg = lock_rw(&state.config).clone();
    let store = lock_rw(&state.store);
    let _ = out;
    Ok(checkin_state_of(&id, &cfg, &store, None))
}
