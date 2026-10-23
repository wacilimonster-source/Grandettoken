//! TokenScope 的 Tauri 外壳:窗口、托盘、命令、轮询调度。
//!
//! 取数逻辑全在 tokenscope-core,这里只做胶水。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, State};
use tauri_plugin_notification::NotificationExt;
use tokenscope_core::config::Config;
use tokenscope_core::fetch::{now_ts, ChannelView, Fetcher};
use tokenscope_core::{appauth, checkin, inject, models, providers, secrets, store::Store, view, workbuddy};

pub struct AppState {
    pub store: Arc<Mutex<Store>>,
    pub config: Mutex<Config>,
    pub fetcher: Arc<Fetcher>,
    /// 当前已注册的全局快捷键(空 = 未注册)。换键时先注册新的、成功后再撤旧的。
    pub hotkey: Mutex<Option<String>>,
    /// 启动期的非致命错误(快捷键被占用、配置损坏回落默认…)。之前只 eprintln,
    /// 绿色版从托盘启动时没人看得到 stderr(报告 P2-14)。前端取走即清空,只提示一次。
    pub startup_error: Mutex<Option<String>>,
}

#[tauri::command]
async fn get_channels(state: State<'_, AppState>) -> Result<Vec<ChannelView>, String> {
    // 先克隆 Arc 再 await:不让 State 的借用跨越 await 点
    let store = state.store.clone();
    let fetcher = state.fetcher.clone();
    // fetch_shared:与轮询/托盘刷新共用闸门,手动刷新不再叠加出一份并发请求(报告 O-4)
    Ok(fetcher.fetch_shared(&store).await)
}

/// 首页四种形态的**统一视图**:一行一个 `FormRow`,加一份排好的顺序与几个计数。
///
/// 存在的理由见 `core::view` 的头注:四种形态以前各自解释一次 `valid / limited /
/// updatedAt`,同一个渠道能给出三种结论。这一条命令之后,前端只负责"画"。
///
/// 取数走 `fetch_shared`(与 `get_channels` 同一闸门、同一份 5 秒内的近期结果),
/// 所以前端连着调这两条不会多打一次账务接口。
#[tauri::command]
async fn home_view(state: State<'_, AppState>) -> Result<view::HomeView, String> {
    let store = state.store.clone();
    let fetcher = state.fetcher.clone();
    let views = fetcher.fetch_shared(&store).await;

    let cfg = lock_rw(&state.config).clone();
    let now = now_ts();
    // 签到的"今天签完没"由后端判定(Qoder 的周期日从 10:00 起算,前端算必错),
    // 这里沿用 `checkin_state` 那条链路上的同一个函数。
    // 隐藏的渠道不必在这里排除:`build_home` 只数排进行列表的那些行,而隐藏的
    // 渠道根本不进 `rows`(与前端 `ckActiveIds` 的口径仍然是同一个)。
    let checks: Vec<view::CheckinInput> = checkin::CHECKINS
        .iter()
        .map(|d| {
            let c = cfg.checkin.get(d.id).cloned().unwrap_or_default();
            let activity_ready = c
                .activity_state
                .as_ref()
                .map(|a| a.status == "claimable")
                .unwrap_or(false);
            view::CheckinInput {
                id: d.id.to_string(),
                enabled: c.enabled,
                done_today: c.blocks_today(&cycle_day_of(d.id, now), APP_VERSION) && !activity_ready,
            }
        })
        .collect();

    Ok(view::build_home(&views, &checks, &view::ViewInput::from_cfg(&cfg, now)))
}

#[tauri::command]
fn get_config(state: State<'_, AppState>) -> Config {
    lock_rw(&state.config).clone()
}

/// 按配置重注册全局快捷键(空串 = 只注销)。先注册新键、成功后才撤旧键:
/// 新键被其它应用占用时保持现状,不会出现「旧键没了、新键也没有」。
/// 行为回调挂在插件 Builder 的 with_handler 上(见 run())。
fn apply_hotkey(app: &tauri::AppHandle, state: &AppState, want: &str) -> Result<(), String> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    let want = want.trim().to_string();
    let mut cur = lock_rw(&state.hotkey);
    if cur.as_deref().unwrap_or("") == want.as_str() {
        return Ok(()); // 没变,别动注册器
    }
    let manager = app.global_shortcut();
    if want.is_empty() {
        if let Some(old) = cur.take() {
            let _ = manager.unregister(old.as_str());
        }
        return Ok(());
    }
    manager
        .register(want.as_str())
        .map_err(|e| format!("注册 {want} 失败(可能已被其它程序占用): {e}"))?;
    if let Some(old) = cur.take() {
        let _ = manager.unregister(old.as_str());
    }
    *cur = Some(want);
    Ok(())
}

/// 配置那行 JSON 只是**格式坏了**、内容还可能手工救回来的时候,绝不能让任何一次保存把
/// 默认配置整份 UPSERT 覆盖上去(报告 P1-16:拖一下窗口/点一次置顶就会永久丢失隐藏清单、
/// 快捷键、申请制规则)。启动时置位,所有写配置的命令都先问一句。
static CONFIG_BROKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 配置写入的串行闸:这些命令以前都在主线程上跑,两次保存天然排队;改成 async 后
/// 必须自己排,否则"后改的先落库"会把配置倒回旧值(P3-35 的一小半)。
static CFG_WRITE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn save_config_off_main(
    state: &AppState,
    mutate: impl FnOnce(&mut Config),
) -> Result<(), String> {
    let _ser = CFG_WRITE.lock().await;
    // 内存里改 + 序列化(快),落库(慢)交给阻塞任务。
    let json = {
        let mut cfg = lock_rw(&state.config);
        mutate(&mut cfg);
        serde_json::to_string(&*cfg).map_err(|e| format!("配置序列化失败: {e}"))?
    };
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || {
        let s = lock_rw(&store);
        s.set_setting("config", &json)
    })
    .await
    .map_err(|e| format!("保存设置失败: {e}"))?
}

/// 更新维护态与"客户端配置正在写"两个标志(报告 P1-15)。
/// Windows 上 updater 的 install 这一步会直接结束进程:如果正好落在 inject 已落盘、
/// 还没记账的那一瞬间,客户端配置就停在半写状态,重启后没有任何记录能回滚它。
/// 所以:更新前先等在途的写入;更新期间停轮询、停签到节拍、拒绝新的写入与密钥箱保存。
static UPDATING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static INJECTING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

struct FlagGuard(&'static std::sync::atomic::AtomicBool);
impl Drop for FlagGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

fn refuse_while_updating(what: &str) -> Result<(), String> {
    if UPDATING.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(format!("正在更新挂件,{what}已暂缓 —— 等更新结束后再点"));
    }
    Ok(())
}

/// 正在跑签到的渠道(节拍器与手动点击之间的互斥)。
///
/// 以前没有任何在途标记:"今天已经处理过/退避中"这两个闸门只在**所有网络往返结束之后**
/// 才写配置,而一次签到要串 2~3 个请求(每个最多 20 秒)。用户在节拍器正跑的时候点一次
/// 「领积分」就能同时发出第二个 claim,两条路的 attempts/last_day/next_try_at 还会互相覆盖。
/// 只存 id、不跨 await 持锁,拿不到就明确告诉用户"稍等几秒"。
static CHECKIN_RUNNING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

struct CheckinBusy(String);
impl CheckinBusy {
    fn acquire(id: &str) -> Result<Self, String> {
        let mut g = CHECKIN_RUNNING.lock().unwrap_or_else(|p| p.into_inner());
        if g.iter().any(|x| x == id) {
            return Err(id.to_string());
        }
        g.push(id.to_string());
        Ok(CheckinBusy(id.to_string()))
    }
}
impl Drop for CheckinBusy {
    fn drop(&mut self) {
        let mut g = CHECKIN_RUNNING.lock().unwrap_or_else(|p| p.into_inner());
        g.retain(|x| x != &self.0);
    }
}

/// 把启动期的致命错误写成文件。release 是 GUI 子系统 + `panic = "abort"`:
/// `?` 与 `expect` 的信息只进 stderr,而 stderr 在这种构建下没人看得见 ——
/// 用户只会看到"双击了没反应"(报告 P2-38)。
fn report_fatal(stage: &str, e: &str) -> String {
    let line = format!(
        "启动失败 · {stage}:{e}
建议:确认是否还有 Grandettoken.exe 在任务管理器里运行,结束后再打开
"
    );
    for dir in [
        std::env::var_os("APPDATA").map(|d| std::path::PathBuf::from(d).join("com.wacil.tokenscope")),
        Some(std::env::temp_dir()),
    ]
    .into_iter()
    .flatten()
    {
        let _ = std::fs::create_dir_all(&dir);
        if std::fs::write(dir.join("startup-error.log"), &line).is_ok() {
            return format!("{line}(详情见 {})", dir.join("startup-error.log").display());
        }
    }
    line
}

fn refuse_if_config_broken() -> Result<(), String> {
    if CONFIG_BROKEN.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(
            "配置读取失败,现在按默认设置运行(你隐藏的渠道也在按默认取数)。\
             为保住库里还能修的那份,已暂停保存设置:请先修复或确认重置,再改设置。"
                .to_string(),
        );
    }
    Ok(())
}

#[tauri::command]
async fn set_config(app: tauri::AppHandle, state: State<'_, AppState>, config: Config) -> Result<(), String> {
    refuse_if_config_broken()?;
    // 前端提交的是它启动时那份配置的快照。更新流程(跳过版本/上次检查时间)和
    // 窗口位置是后端单方面往前推的字段 —— 前端拿不到最新值,整份覆盖会把它们
    // 抹回 None(报告 P1-3:跳过某版本后,下次保存设置让该版本又重新弹窗)。
    let mut incoming = config;
    {
        let cur = lock_rw(&state.config);
        if incoming.skipped_version.is_none() {
            incoming.skipped_version = cur.skipped_version.clone();
        }
        if incoming.last_check_at.is_none() {
            incoming.last_check_at = cur.last_check_at;
        }
        // 「详细日志」是**后端单方面推进的字段**(只有 diag_log_verbose 命令能改它)。
        // 前端手里永远是开机那一刻的快照,整份覆盖会把用户的开关抹回默认值 ——
        // 与「跳过版本被覆盖」是同一个坑(报告 P1-3),这里直接由后端说了算。
        incoming.diag_verbose = cur.diag_verbose;
        if incoming.win_x.is_none() {
            incoming.win_x = cur.win_x;
        }
        if incoming.win_y.is_none() {
            incoming.win_y = cur.win_y;
        }
        // 选中项口径是后端从客户端文件里读出来的(前端手里永远是旧的):
        // 整份覆盖会把刚学会的抹掉,所以后端已知的键以库里为准。
        let mut merged = cur.selection_prefix.clone();
        for (k, v) in &incoming.selection_prefix {
            merged.entry(k.clone()).or_insert_with(|| v.clone());
        }
        incoming.selection_prefix = merged;

        // 签到:只有 enabled / auto 是用户意图,其余七个字段(last_day / last_code / attempts /
        // next_try_at / last_try_at / last_credits / last_message)是后端进度。前端提交的永远是
        // 开机那一刻的快照,整份覆盖会把当天「已领 +100」抹回「今天还没签」、退避计数一起清零,
        // 节拍器随后在同一天再签一次(报告 P1-13;与它修掉的「跳过版本被覆盖」是同一个坑)。
        let mut ck = cur.checkin.clone();
        for (id, inc) in &incoming.checkin {
            match ck.get_mut(id) {
                Some(c) => {
                    c.enabled = inc.enabled;
                    c.auto = inc.auto;
                }
                None => {
                    let mut fresh = checkin::CheckinCfg::default();
                    fresh.enabled = inc.enabled;
                    fresh.auto = inc.auto;
                    ck.insert(id.clone(), fresh);
                }
            }
        }
        incoming.checkin = ck;
    }

    // 先应用快捷键:注册失败(如被占用)就让整次保存失败,前端好回滚旧值
    let old_hotkey = lock_rw(&state.hotkey).clone();
    if let Err(e) = apply_hotkey(&app, &state, &incoming.hotkey) {
        return Err(e);
    }
    let next = incoming;
    if let Err(e) = save_config_off_main(&state, move |c| *c = next).await {
        // 落库失败:快捷键严格回到保存前。以前只在"原来有键"时回滚,
        // 第一次设键又落库失败时,那个组合键已被挂件抢走却显示为空(报告 P3-34)。
        let _ = apply_hotkey(&app, &state, old_hotkey.as_deref().unwrap_or(""));
        return Err(e);
    }
    // 配置变了(隐藏渠道 / 间隔 / 阈值…)就作废"5 秒内复用上次结果"的窗口 ——
    // 否则保存后立刻取数会拿到旧视图,界面看起来像"点了没反应"(用户实测:隐藏渠道恢复)。
    state.fetcher.invalidate();
    Ok(())
}

/// 只回显尾 4 位,用于确认每个渠道存的到底是哪把 key(存错 key 却以为是接口坏了,
/// 是最难排查的一类问题)。不回显完整密钥。复用本机应用登录态的渠道没有密钥可回显;
/// 网页登录型(WorkBuddy)存的就是令牌本身,同样值得回显尾 4 位。
#[tauri::command]
async fn key_hint(id: String) -> Result<Option<String>, String> {
    if !matches!(
        providers::find(&id).map(|d| d.auth),
        Some(providers::AuthKind::Keyring | providers::AuthKind::WebSession)
    ) {
        return Ok(None);
    }
    // 这是个 RPC,而且每轮渲染会对每个已配密钥的渠道各发一次 —— 以前是同步命令,
    // 等于把最多 6 次凭据管理器调用串在主线程上(报告 P1-14)。
    // 另外不再用 secrets::masked:它把"凭据库坏了"吞成 None(报告 P2-15)。
    // 走 resolve 而不是 get:引用模式的渠道要显示**箱子那把**的尾号,否则界面说
    // "未配置密钥"而取数其实正常(两份真相的典型症状)。
    tokio::task::spawn_blocking(move || match secrets::resolve(&id) {
        Ok(r) => Ok(r.key().map(secrets::mask_of)),
        Err(e) => Err(e),
    })
    .await
    .map_err(|e| format!("读取凭据失败: {e}"))?
}

/// 这个 id 是不是自定义渠道(custom-1、custom-2…)。
fn is_custom_id(id: &str) -> bool {
    id.starts_with("custom-")
}

/// 这个渠道"当前密钥的指纹":内置渠道读它自己的凭据,自定义渠道的凭据名就是 id。
/// **阻塞**(读凭据管理器),只能在非主线程上调。
fn fp_of_any(id: &str) -> String {
    if let Some(def) = providers::find(id) {
        return tokenscope_core::fetch::current_key_fp(def);
    }
    match secrets::channel_key(id) {
        Ok(Some(k)) if !k.trim().is_empty() => tokenscope_core::fetch::key_fp(&k),
        _ => String::new(),
    }
}

/// 这个渠道能不能填/删密钥:内置渠道(非 App 型)或**存在的**自定义渠道。
/// 自定义渠道的密钥也在同一个凭据管理器里,账户名就是渠道 id。
fn key_allowed(state: &AppState, id: &str) -> Result<(), String> {
    if providers::find(id).is_some() {
        return Ok(());
    }
    let c = lock_rw(&state.config);
    if is_custom_id(id) && c.custom_channels.iter().any(|x| x.id == id) {
        return Ok(());
    }
    Err(format!("未知渠道: {id}"))
}

#[tauri::command]
async fn set_key(app: tauri::AppHandle, id: String, key: String) -> Result<(), String> {
    key_allowed(&app.state::<AppState>(), &id)?;
    if let Some(def) = providers::find(&id) {
        if let providers::AuthKind::App(app) = def.auth {
            return Err(format!(
                "{} 不需要填密钥:凭据直接读本机已登录的 {} 客户端",
                def.name,
                app.label()
            ));
        }
    }
    // 凭据管理器写 = RPC:不能在主线程做(报告 P1-14)
    //
    // 互斥(裁决 7):填了值就把引用清掉。否则读路径**优先看引用**,
    // 用户填的新值会被完全忽略 —— 表现为"保存成功但还在用旧的",最难查的那种。
    //
    // ⚠ **顺序:先写值、后清引用**(2026-10-21 修 N03)。反过来的话,
    // 写值这一步会因为"密钥为空"被拒(`secrets::set` 里 `if key.is_empty()`),
    // 而那时引用**已经被清掉了** —— 用户看到"保存失败",但原来那份能用的引用没了,
    // 渠道变成"既没引用也没值"。改成先写后清:写失败时旧引用还在,渠道继续正常工作;
    // 而"两份都在"的短暂状态完全无害,因为读路径优先看引用。
    let cid = id.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        secrets::set(&cid, &key)?;
        secrets::link_clear(&cid)
    })
    .await
    .map_err(|e| format!("保存密钥失败: {e}"))??;
    // 反查索引同步清掉(它只是索引,权威来源是上面那个指针)
    let store = app.state::<AppState>().store.clone();
    lock_rw(&store).vault_ref_clear(&id)?;
    // 与删除同理:刚换过凭据就别再复用 5 秒内的旧结果,否则"保存了却还显示未配置/旧数值"
    app.state::<AppState>().fetcher.invalidate();
    Ok(())
}

/// 返回值是给界面直接显示的一句话(成功也要说清"删了什么"),不是 Result<(),_>:
/// 清浏览器会话失败时以前是 `let _ =` 一吞,命令照样 Ok、界面照样说"已删除密钥",
/// 而下一轮取数又读到了那个 Cookie —— 用户看到"清掉了却还显示已登录"。
#[tauri::command]
async fn delete_key(app: tauri::AppHandle, id: String) -> Result<String, String> {
    key_allowed(&app.state::<AppState>(), &id)?;
    // 自定义渠道:只有凭据管理器里那一份,没有网页会话可清
    if providers::find(&id).is_none() {
        purge_channel_credentials(&app, &id).await?;
        app.state::<AppState>().fetcher.invalidate();
        return Ok("已删除这个自定义渠道的密钥(渠道定义还在,换一把可继续用)".into());
    }
    let def = providers::find(&id).ok_or_else(|| format!("未知渠道: {id}"))?;
    if let providers::AuthKind::App(app) = def.auth {
        return Err(format!("{} 没有本应用保存的密钥(用的是 {} 的登录态)", def.name, app.label()));
    }
    // 网页登录型的会话住在浏览器 profile 里,不在凭据管理器 —— 一并清掉。
    // 不清的话这个按钮点了等于没点:下一轮取数照样读得到 Cookie(2026-10-08)。
    // 这条命令因此必须是 async:清 Cookie 要同步问 webview,不能占着主线程等。
    let is_web = matches!(def.auth, providers::AuthKind::WebSession);
    let mut warn = String::new();
    if is_web {
        if let Err(e) = wb_clear_session(&app) {
            warn = format!(";浏览器登录会话可能没清干净({e}),请在客户端里退出登录一次");
        }
    }
    purge_channel_credentials(&app, &id).await?;
    // 凭据刚变过:作废"5 秒内复用上次结果",否则紧接着的刷新还会把旧结果(带旧凭据
    // 取到的那份)端上来,用户看到"清掉了却还显示已登录"。
    app.state::<AppState>().fetcher.invalidate();
    Ok(if is_web {
        format!("已清除 {} 的登录会话{warn}", def.name)
    } else {
        format!("已删除 {} 的密钥{warn}", def.name)
    })
}

/// 把一个渠道的**全部**密钥痕迹清干净:直接填的值 + 引用指针 + 反查索引行。
///
/// 2026-10-21 修 N01/N02:以前各处只删 `TokenScope/<id>`,而密钥箱打通之后
/// 一个渠道的密钥有**两个来源**,读取时**优先看引用** —— 于是"删除密钥"点了等于没点:
/// 弹窗说已删除,渠道照样取得到数。这不是文案问题,是凭据真的还在。
///
/// 三样必须一起清,少一样就留一条孤儿:
///   · `secrets::delete`     —— 直接填的值
///   · `secrets::link_clear` —— 引用指针(**读取时优先级最高的那个**)
///   · `vault_ref_clear`     —— SQLite 反查索引(不清的话密钥箱还显示"正被它使用")
async fn purge_channel_credentials(app: &tauri::AppHandle, id: &str) -> Result<(), String> {
    let cid = id.to_string();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        secrets::delete(&cid)?;
        secrets::link_clear(&cid)
    })
    .await
    .map_err(|e| format!("删除密钥任务失败: {e}"))??;
    let store = app.state::<AppState>().store.clone();
    lock_rw(&store).vault_ref_clear(id)?;
    Ok(())
}

// ───────────── 渠道密钥 ↔ 密钥箱(设计稿 product-vault-channel-bridge)─────────────
//
// 裁决 1:主线是**引用**,不是"存两份"。所以渠道的密钥来源只有两种状态 ——
//   · 直接填在渠道卡上(inline)
//   · 引用密钥箱里的某一条(vault)
// 两者互斥:切到引用会清掉手填的值(裁决 7),填了值会清掉引用。

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct KeySourceInfo {
    /// "none" | "inline" | "vault" | "dangling"
    mode: String,
    vault_id: Option<String>,
    /// 尾 4 位掩码。只有真读到值才有 —— 断链时是 None,界面不该编。
    hint: Option<String>,
    /// 引用模式下被引用条目的 平台 / 标签(渠道卡上要显示"正在用 4SAPI / 主号")
    vault_platform: Option<String>,
    vault_label: Option<String>,
}

/// 这个渠道的密钥从哪来。渠道卡渲染与"密钥来源"二选一都靠它。
#[tauri::command]
async fn key_source(app: tauri::AppHandle, id: String) -> Result<KeySourceInfo, String> {
    key_allowed(&app.state::<AppState>(), &id)?;
    let store = app.state::<AppState>().store.clone();
    let cid = id.clone();
    let resolved = tokio::task::spawn_blocking(move || secrets::resolve(&cid))
        .await
        .map_err(|e| format!("读取密钥来源失败: {e}"))??;

    let mut info = KeySourceInfo {
        mode: "none".into(),
        vault_id: None,
        hint: None,
        vault_platform: None,
        vault_label: None,
    };
    match resolved {
        secrets::Resolved::None => {}
        secrets::Resolved::Inline(k) => {
            info.mode = "inline".into();
            info.hint = Some(secrets::mask_of(&k));
        }
        secrets::Resolved::Vault { id: vid, key } => {
            info.mode = "vault".into();
            info.hint = Some(secrets::mask_of(&key));
            // 平台/标签来自索引(不含密文),读失败不该让整条命令失败 —— 少了文案而已
            if let Ok(Some(item)) = lock_rw(&store).vault_get(&vid) {
                info.vault_platform = Some(item.platform);
                info.vault_label = Some(item.label);
            }
            info.vault_id = Some(vid);
        }
        secrets::Resolved::Dangling(vid) => {
            info.mode = "dangling".into();
            info.vault_id = Some(vid);
        }
    }
    Ok(info)
}

/// 让渠道改成"引用密钥箱里的某一条"。
///
/// 会**清掉手填的值**(裁决 7):留着就是两份真相,而且箱子条目被删后可能被回退使用,
/// 出现"我删了箱子里的,它还在用旧密钥"。
#[tauri::command]
async fn set_key_ref(app: tauri::AppHandle, id: String, vault_id: String) -> Result<(), String> {
    key_allowed(&app.state::<AppState>(), &id)?;
    if let Some(def) = providers::find(&id) {
        if let providers::AuthKind::App(app) = def.auth {
            return Err(format!(
                "{} 不需要填密钥:凭据直接读本机已登录的 {} 客户端",
                def.name,
                app.label()
            ));
        }
    }
    let store = app.state::<AppState>().store.clone();
    // 引用的目标必须真的存在,否则一建就是断链。
    //
    // ⚠ N17(2026-10-21):这里必须查**凭据管理器里的密文**,不能只查 SQLite 索引 ——
    // 两者会分开(用户在凭据管理器里手动删过、或 vault_save 回滚时删密文失败)。
    // 只查索引的后果很具体:代码会**先把手填的那把删掉**,然后切到一个死引用上,
    // 渠道从"能用"变成"没密钥" —— 是操作把人搞坏的,不是数据本来坏了。
    let vid_check = vault_id.clone();
    let has_cipher = tokio::task::spawn_blocking(move || secrets::vault_get(&vid_check))
        .await
        .map_err(|e| format!("检查密钥箱条目失败: {e}"))??;
    if has_cipher.is_none() {
        return Err(
            "这个密钥箱条目的密文已经不在了(列表里还看得到,但 Windows 凭据管理器里没有)—— 请在密钥箱里重新录一次"
                .into(),
        );
    }
    let (cid, vid) = (id.clone(), vault_id.clone());
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        // 先写指针再清手填值:哪怕清值失败,引用也已经生效(读路径优先看引用)
        secrets::link_set(&cid, &vid)?;
        secrets::delete(&cid)?;
        Ok(())
    })
    .await
    .map_err(|e| format!("保存密钥引用失败: {e}"))??;
    // 反查索引(仅为"这把被谁用了"服务;权威来源仍是凭据管理器里的指针)
    lock_rw(&store).vault_ref_set(&id, &vault_id)?;
    app.state::<AppState>().fetcher.invalidate();
    Ok(())
}

/// 取消引用,改回"直接填"。手填的值本来就在切到引用时清掉了,所以这里结果是"未配置"。
#[tauri::command]
async fn clear_key_ref(app: tauri::AppHandle, id: String) -> Result<(), String> {
    key_allowed(&app.state::<AppState>(), &id)?;
    let store = app.state::<AppState>().store.clone();
    let cid = id.clone();
    tokio::task::spawn_blocking(move || secrets::link_clear(&cid))
        .await
        .map_err(|e| format!("清除密钥引用失败: {e}"))??;
    lock_rw(&store).vault_ref_clear(&id)?;
    app.state::<AppState>().fetcher.invalidate();
    Ok(())
}

/// 有哪些渠道正引用这个箱子条目。密钥箱列表与删除前拦截都要它。
/// 反查"这把被哪些渠道用着"。**扫真实指针,不查索引**(N30:以前注册了却没人调,
/// 而且它读的是索引 —— 注释说"删除前拦截要它",实现却在浏览器里自己过滤)。
#[tauri::command]
async fn vault_ref_channels(app: tauri::AppHandle, vault_id: String) -> Result<Vec<String>, String> {
    ref_channels_by_pointer(&app, &vault_id).await
}

/// 全部引用关系(渠道 → 箱子条目)。密钥箱列表一次读出来批量渲染。
#[tauri::command]
fn vault_ref_map(app: tauri::AppHandle) -> Result<Vec<(String, String)>, String> {
    let store = app.state::<AppState>().store.clone();
    let out = lock_rw(&store).vault_ref_pairs().map_err(|e| e.to_string())?;
    Ok(out)
}

// ───────────── WorkBuddy 网页登录 ─────────────
//
// WorkBuddy 桌面端 5.6.2 起把本机凭据文件里的 accessToken 写成 AES-GCM 信封
// (实测 {"$wbEncrypted":1,"envelope":…},静态钥编译期内置、本机不可解 —— 2026-10-02 取证),
// 以前那套"只读复用客户端登录态"彻底读不到了,改走挂件内网页登录。
//
// **真正要抄的不是令牌,是会话 Cookie**(2026-10-08 真机取证,第一版去抓 Bearer 白做了):
// 控制台自己打 /billing/meter/* 时既没有 Authorization 头、sessionStorage 里也没有
// growth-center-token,认证全在 httpOnly 的 session / session_2 两个 Cookie 上;
// 而网关(openresty)还按 User-Agent 分流 —— 同一批 Cookie,浏览器 UA 200、客户端 UA 401。
// 所以这里的流程是:
//   1) 登录窗停在「个人中心 / 控制台」= 服务端确实认了这份会话(没认会被挡回 /login/);
//   2) 从 webview 的 Cookie 罐里现读(httpOnly 也读得到;登录窗与主窗口共用同一个 profile);
//   3) **先用挂件自己的 HTTP 栈真打一次接口**,通了才关窗报「已登录」(见 Fetcher::probe);
//   4) 什么都不落盘:会话留在浏览器 profile 里,取数时现读,平台轮换 Cookie 也能跟上
//      (Windows 凭据管理器也放不下 —— 实测 blob 上限约 1280 字符,两个 Cookie 近 6 KB)。
// 判定规则(哪些 Cookie 算会话、落在什么路径才算登录完成)都在 core::weblogin,那边有单测。

/// 登录窗 label:同一时刻只允许一个
const WB_LOGIN_LABEL: &str = "wb-login";
/// 只有这条路走网页登录;手动粘贴的密钥仍存这个条目(Windows 凭据管理器)
const WB_CHANNEL_ID: &str = "workbuddy";
/// 登录页。redirect_uri 就是登录完成后回落的用量页 —— 也是判"登录完成"的路径之一。
const WB_LOGIN_URL: &str = "https://www.workbuddy.cn/login?platform=usercenter&state=0&redirect_uri=https%3A%2F%2Fwww.workbuddy.cn%2Fprofile%2Fplans-usage";
/// 读/删会话 Cookie 时用的地址(只要域名对得上,path 随便给一个能命中的)
// 会话 cookie 的取值域名清单以 weblogin::SITE_HOSTS 为唯一来源:以前这里另写一个常量,
// 于是"判定已登录"认两个域、"取会话"只查一个域,登录了却永远读不到(P2-16)。

/// 成功抓到会话后置位,用于区分"用户自己关掉了窗口"和"我们主动关窗" ——
/// 前者要让界面把「登录中」状态收回去,后者不该再报一次取消。
static WB_LOGIN_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 登录窗已经销毁(用户自己关的,或者我们收尾时关的)。观察者靠它收工 ——
/// 有了这个标志,就不必去问那个可能正在销毁的窗口(问它会 panic,见 on_main_with_timeout)。
static WB_LOGIN_CLOSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// 页面是否已经落到"登录完成"的路径上(判定规则在 core::weblogin)。由 on_navigation
/// 在主线程上置位 —— 那里拿到 URL 不需要任何跨线程派发,比事后去问窗口安全得多。
static WB_LANDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 把一段"要碰 webview"的代码投到主线程去跑,不等结果(关闭窗口这类不需要回信)。
fn on_main(app: &tauri::AppHandle, f: impl FnOnce() + Send + 'static) {
    let _ = app.run_on_main_thread(f);
}

/// 把一段"要碰 webview"的代码投到主线程去跑,带**自己的**超时把结果等回来。
///
/// 为什么不能直接在外壳里调 `cookies_for_url`:它内部走的是**阻塞派发** ——
/// 从 worker 线程发消息给主线程,然后在 `rx.recv().unwrap()` 上等回信。目标 webview
/// 正在销毁、或者应用正在退出时,那条回信永远不会来,`unwrap` 直接 panic;而 release
/// 是 `panic = "abort"`,一次派发失败等于整个挂件原地消失(2026-10-08 踩到:刚关掉
/// 登录窗就去读 Cookie,日志里留下 RecvError panic)。
/// 这里改成"投递任务 + 自己带超时地等",拿不到就当作没读到,让这一轮取数退回"未登录",
/// 绝不让挂件崩。投递本身在主线程上是就地执行,不存在回信问题。
fn on_main_with_timeout<T: Send + 'static>(
    app: &tauri::AppHandle,
    f: impl FnOnce() -> T + Send + 'static,
    timeout: std::time::Duration,
) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(f());
    })
    .ok()?;
    rx.recv_timeout(timeout).ok()
}

/// 从 webview 的 Cookie 罐里取会话。
///
/// **只读主窗口**:它从启动活到退出,而登录窗随时可能被用户关掉(读一个正在销毁的
/// 窗口会走到上面那个 panic 上去)。Cookie 是按 profile 存的,主窗口读到的和登录窗
/// 读到的是同一份 —— 2026-10-08 实测(httpOnly 的 session / session_2 都读得到)。
fn wb_session_of(app: &tauri::AppHandle, id: &str) -> Option<String> {
    if id != WB_CHANNEL_ID {
        return None; // 只有 WorkBuddy 走这条路
    }
    let handle = app.clone();
    on_main_with_timeout(
        app,
        move || {
            let w = handle.get_webview_window("main")?;
            // `is_post_login` 认的工作台域名有两个(www.workbuddy.cn / www.codebuddy.cn),
            // 以前这里只查第一个:在第二个域上完成登录 → 判定"已登录"却永远取不到会话,
            // 界面只剩"看着登录了但取不到数"这种最难排查的样子(报告 P2-16)。
            for host in tokenscope_core::weblogin::SITE_HOSTS {
                let Some(url) = format!("https://{host}/").parse::<tauri::Url>().ok() else {
                    continue;
                };
                let Ok(jar) = w.cookies_for_url(url) else { continue };
                if let Some(c) =
                    tokenscope_core::weblogin::cookie_credential(jar.iter().map(|x| (x.name(), x.value())))
                {
                    return Some(c);
                }
            }
            None
        },
        std::time::Duration::from_secs(3),
    )
    .flatten()
}

/// 清 WorkBuddy 的登录会话(「清除登录」用)。**返回结果不能丢**:以前调用方
/// `let _ =` 一吞,超时 / 删失败都变成"命令成功",界面说清掉了、下一轮取数又读到那个 Cookie。
/// 找不到会话不算失败(目标状态"没有会话"本来就成立),超时与删失败才报错。
fn wb_clear_session(app: &tauri::AppHandle) -> Result<(), String> {
    let handle = app.clone();
    let job = on_main_with_timeout(
        app,
        move || {
            let Some(w) = handle.get_webview_window("main") else {
                return (0usize, 0usize);
            };
            let mut matched = 0usize;
            let mut failed = 0usize;
            // 两个工作台域名都要清:只在其中一个上登过的人,点「清除登录」后
            // 另一个域的会话还在,下一轮取数照样读得到(报告 P2-16)
            for host in tokenscope_core::weblogin::SITE_HOSTS {
                let Ok(url) = format!("https://{host}/").parse::<tauri::Url>() else {
                    continue;
                };
                let Ok(jar) = w.cookies_for_url(url) else {
                    continue;
                };
                for c in jar {
                    if tokenscope_core::weblogin::is_session_cookie(c.name()) {
                        matched += 1;
                        if w.delete_cookie(c).is_err() {
                            failed += 1;
                        }
                    }
                }
            }
            (matched, failed)
        },
        std::time::Duration::from_secs(3),
    );
    match job {
        None => Err("主线程 3 秒内没回信(窗口正忙或正在销毁)".to_string()),
        Some((_, failed)) if failed > 0 => Err(format!("有 {failed} 个会话 cookie 没删掉")),
        Some(_) => Ok(()),
    }
}

/// 会话可用吗:用挂件自己的 HTTP 栈真打一次接口(见 Fetcher::probe)。
/// 不验证就报成功的话,一份被平台拒掉的会话会变成"状态显示已登录、却一直取不到数"。
async fn wb_session_works(app: &tauri::AppHandle, session: &str) -> bool {
    let fetcher = app.state::<AppState>().fetcher.clone();
    match providers::find(WB_CHANNEL_ID) {
        Some(def) => fetcher.probe(def, session).await,
        None => false,
    }
}

/// 收尾:关窗 → 通知界面 → 立刻刷一轮。会话本身不用搬走 —— 它就在浏览器 profile 里,
/// 取数时现读(见文件顶部第 4 条)。
async fn wb_finish_login(app: &tauri::AppHandle) {
    // 先置位再关窗:Destroyed 监听靠这个标志区分"登录成功"与"用户自己关了"
    WB_LOGIN_DONE.store(true, std::sync::atomic::Ordering::Relaxed);
    // 记下"这份会话从现在起是新鲜的":平台会话是绝对 TTL(实测 7 天,证据链在
    // core::weblogin::web_session_hint 的注释),取数侧按这个年龄出提示。
    // 键的定义只留 core 一份(WEB_SESSION_AT_KEY);写失败只是少一条提示,
    // 不值得让收工流程失败。就一次毫秒级的 SQLite 写,直接做。
    {
        let st = app.state::<AppState>();
        let store = lock_rw(&st.store);
        let _ = store.set_setting(
            tokenscope_core::weblogin::WEB_SESSION_AT_KEY,
            &now_ts().to_string(),
        );
    }
    // 复登成功 = 当天"凭据失效"的签到该放出补领了:done_for_day 把 auth 算成
    // "今天完结",节拍器从此照章跳过、手动「领积分」按钮也被 can_try 拦住 ——
    // 实锤过(2026-10-18:上午 auth 一次,中午复登成功,当天的分要静默到次日)。
    // 只清 auth 之后的场次,claimed/already/inactive 不动(那些是真签完了)。
    {
        let st = app.state::<AppState>();
        let mut cfg = lock_rw(&st.config).clone();
        let mut changed = false;
        let store2 = app.state::<AppState>().store.clone();
        if let Some(entry) = cfg.checkin.get_mut(WB_CHANNEL_ID) {
            // `auth` = 平台拒绝了那份凭据;`no_cred` = 本机压根没读到凭据。
            // **两者都该被复登成功放行** —— 复登之后凭据就有了,当天的分不必等到次日。
            if entry
                .last_code
                .as_deref()
                .is_some_and(|c| c.starts_with("auth") || c.starts_with("no_cred"))
            {
                let today = cycle_day_of(WB_CHANNEL_ID, now_ts());
                entry.last_day = Some(today);
                entry.last_code = None;
                entry.attempts = 0;
                entry.next_try_at = None;
                changed = true;
            }
        }
        if changed {
            // save(写盘)与回写内存态照 run_checkin 的次序:先落库,再放出 guard。
            let save_store = lock_rw(&store2);
            if let Err(e) = cfg.save(&save_store) {
                eprintln!("复登后重置签到状态失败: {e}");
            }
            drop(save_store);
            *lock_rw(&st.config) = cfg;
        }
    }
    let handle = app.clone();
    on_main(app, move || {
        if let Some(w) = handle.get_webview_window(WB_LOGIN_LABEL) {
            let _ = w.close();
        }
    });
    let _ = app.emit("workbuddy-login", serde_json::json!({"ok": true}));
    app.state::<AppState>().fetcher.invalidate(); // 作废"5 秒内复用上次结果",否则这次刷新拿的还是旧结果
    refresh_now(app).await;
}

/// 登录窗开着的时候盯着它,直到会话可用或者窗口被用户关掉。
///
/// 为什么轮询而不是拦导航:控制台是 SPA,登录落地那一跳可能走 pushState,不触发导航
/// 事件;1.2 秒一次的开销只是读一次 Cookie,而且**只有**会话内容变了、或者页面落到了
/// 「个人中心 / 控制台」,才发真请求去验一次。
///
/// 判"能不能收工"靠**验证**,不靠"cookie 罐里有没有东西":没登录的访客也可能有
/// session Cookie。反过来,已经登录过的人再点一次「登录」,页面会停在「选择账号」上
/// 等人点 —— 那时会话其实已经可用,验证一过就直接收工,不让用户对着一个"没反应"的
/// 窗口发呆(2026-10-08 真机踩到)。
///
/// 两条状态都由主线程侧的钩子置位(窗口事件、导航事件),**不去碰登录窗的任何 API**:
/// 那类调用是阻塞派发,窗口正在销毁时会让挂件 panic-abort(见 on_main_with_timeout)。
fn spawn_login_watcher(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut tried = String::new();
        let mut probes = 0u32;
        let mut ticks = 0u32;
        loop {
            if WB_LOGIN_CLOSED.load(std::sync::atomic::Ordering::Relaxed) {
                return; // 用户自己把窗口关了
            }
            let landed = WB_LANDED.load(std::sync::atomic::Ordering::Relaxed);
            // 读 Cookie 是阻塞调用(要同步问 webview),下沉到 spawn_blocking:
            // 别占着 tokio worker(与取数那边同一个理由,报告 O-5)。
            let probe_app = app.clone();
            let session = tokio::task::spawn_blocking(move || wb_session_of(&probe_app, WB_CHANNEL_ID))
                .await
                .ok()
                .flatten();
            if let Some(session) = session {
                let fresh = session != tried;
                if fresh {
                    tried = session.clone();
                    probes = 0;
                }
                // 新会话立刻验;同一份会话每 ~6 秒补验一次(用户可能刚点完「选择账号」,
                // Cookie 没变但账号上下文变了),最多 10 次,不对着平台一直打。
                if fresh || (probes < 10 && ticks % 5 == 0) {
                    probes += 1;
                    if wb_session_works(&app, &session).await {
                        wb_finish_login(&app).await;
                        return;
                    }
                    // 验不过:确实落到登录完成页、又刚换了会话(用户刚登录完)才说话;
                    // 还停在登录页说明人家正要登录,别抢着报错。
                    if landed && fresh {
                        let _ = app.emit(
                            "workbuddy-login",
                            serde_json::json!({
                                "ok": false,
                                "reason": "登录是成功了,但挂件用这份会话取数被平台拒绝。平台网页会话实测约 7 天过期(绝对 TTL,挂件每分钟打接口也续不上命),还是先在渠道卡上点「重新登录」;访问密钥那条备选路要标准版以上的账号才能建(入口 www.workbuddy.cn/profile/keys)。"
                            }),
                        );
                    }
                }
            }
            ticks += 1;
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        }
    });
}

/// 打开 WorkBuddy 登录窗。已经开着就把它拉到前台(不重复开第二个)。
///
/// **这条命令必须是 async**,别改成同步的:同步命令由 Tauri 在主线程上就地执行,
/// 而它是在 WebView2 的 WebMessageReceived 回调里被调用的 —— 在 COM 回调里再同步
/// 建一个 webview,CreateCoreWebView2Controller 的完成回调就再也送不到主线程上
/// (2026-10-08 实测:窗口壳出来了、页面停在 about:blank、之后连别的 IPC 一起冻死)。
/// async 命令跑在 async runtime 的线程上,窗口创建被当成一条消息投进事件循环,
/// 与启动时建主窗口走的是同一种上下文 —— 那条路是通的。
#[tauri::command]
async fn workbuddy_login(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(WB_LOGIN_LABEL) {
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }
    WB_LOGIN_DONE.store(false, std::sync::atomic::Ordering::Relaxed);
    WB_LANDED.store(false, std::sync::atomic::Ordering::Relaxed);
    WB_LOGIN_CLOSED.store(false, std::sync::atomic::Ordering::Relaxed);
    let url: tauri::Url = WB_LOGIN_URL
        .parse()
        .map_err(|e| format!("登录地址有问题: {e}"))?;
    // 不注入任何脚本、不拦任何导航:登录页是第三方的,挂件只负责看它走到哪儿。
    // on_navigation 只当信号用(记下"已经落到个人中心了"),一律放行。
    tauri::WebviewWindowBuilder::new(&app, WB_LOGIN_LABEL, tauri::WebviewUrl::External(url))
        .title("登录 WorkBuddy 账号（国内版）")
        .inner_size(560.0, 780.0)
        .min_inner_size(420.0, 560.0)
        .resizable(true)
        // 面板是置顶的,登录窗不置顶就会被压在下面,用户看不见
        .always_on_top(true)
        .on_navigation(|nav| {
            if tokenscope_core::weblogin::is_post_login(nav.as_str()) {
                WB_LANDED.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            true
        })
        .build()
        .map_err(|e| format!("登录窗口打不开: {e}"))?;
    spawn_login_watcher(app.clone());
    Ok(())
}

/// 展开区曲线的返回:值 + 有缺口的分桶 + 数据来源。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SeriesView {
    values: Vec<f64>,
    /// 有缺口的分桶下标(柱图上打斜纹:那段时间挂件没在跑,柱子只算了一半)
    gaps: Vec<usize>,
    /// detail = 全部来自 90 天明细;daily = 含 90 天外的日级摘要(界面写清口径)
    source: String,
}

#[tauri::command]
async fn get_series(app: tauri::AppHandle, id: String, hours: i64) -> Result<SeriesView, String> {
    // 参数来自前端:负数或超大值会把扫描窗口翻成"全表",而这个查询以前还跑在主线程上
    // (报告 P3-24 + P1-14)。id 也必须是认得出的渠道,否则白扫一条不存在的序列。
    // 上限抬到 400 天:长期趋势要用(90 天外的部分由日级摘要补,见 trend_series)。
    key_allowed(&app.state::<AppState>(), &id)?;
    let hours = hours.clamp(1, 24 * 400);
    let store = {
        let st = app.state::<AppState>();
        st.store.clone()
    };
    let id2 = id.clone();
    tokio::task::spawn_blocking(move || {
        // 曲线只画"现在这把钥匙"的账(与消耗推算同一口径);读凭据是阻塞 IO,已在下沉线程里
        let fp = fp_of_any(&id2);
        let fpo = if fp.is_empty() { None } else { Some(fp.as_str()) };
        let to = now_ts();
        let from = to - hours * 3600;
        // lock_rw:锁被毒化时也要出图,不能让取数线程的一次 panic 之后所有曲线都空掉
        let s = lock_rw(&store);
        let archived = s
            .daily_summaries(&id2, fpo, from)
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        let (values, gaps) = s
            .trend_series(
                &id2,
                fpo,
                from,
                to,
                24,
                tokenscope_core::fetch::local_tz_offset(),
            )
            .unwrap_or_else(|_| (vec![0.0; 24], Vec::new()));
        Ok(SeriesView {
            values,
            gaps,
            source: if archived { "daily".into() } else { "detail".into() },
        })
    })
    .await
    .map_err(|e| format!("取曲线失败: {e}"))?
}

/// 行详情里的「最近变动」台账:展开时才取,不占轮询、不进视图。
#[tauri::command]
async fn channel_changes(
    app: tauri::AppHandle,
    id: String,
    days: i64,
) -> Result<Vec<tokenscope_core::store::ChangeEntry>, String> {
    key_allowed(&app.state::<AppState>(), &id)?;
    let days = days.clamp(1, 90);
    let store = {
        let st = app.state::<AppState>();
        st.store.clone()
    };
    let id2 = id.clone();
    tokio::task::spawn_blocking(move || {
        let fp = fp_of_any(&id2);
        let fpo = if fp.is_empty() { None } else { Some(fp.as_str()) };
        let s = lock_rw(&store);
        Ok(s.recent_changes(&id2, fpo, now_ts() - days * 86_400, 12)
            .unwrap_or_default())
    })
    .await
    .map_err(|e| format!("取变动台账失败: {e}"))?
}

/// 开机自启:写 HKCU 的 Run 键,不需要管理员权限。
/// 产物是绿色单文件 exe,可能被用户移动位置,所以 setup 里每次启动都按
/// `current_exe()` 重写一遍注册表 —— 路径永远指向当前这份 exe。
#[cfg(windows)]
mod autostart {
    use std::path::{Path, PathBuf};
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const NAME: &str = "Grandettoken";
    /// 改名前的键名:停用/同步时一并清掉,避免残留一个指向已删除 exe 的自启项
    const LEGACY_NAME: &str = "TokenScope";

    /// 是不是从构建目录里跑的开发版(`.../target/release/xxx.exe`)。
    fn in_build_tree(exe: &Path) -> bool {
        let p = exe.display().to_string().to_lowercase();
        p.contains(r"\target\release\") || p.contains(r"\target\debug\")
    }

    /// 注册自启键。**开发版直接报错**:把用户的开机项写到 target 目录里,一次
    /// `cargo clean` 就让自启指向不存在的路径,而 Windows 只会静默失败 —— 用户看
    /// 到的现象是"面板某天开机没出来"(本机踩过:键值指到 target/release/tokenscope.exe)。
    pub fn set(enabled: bool) -> Result<(), String> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(RUN_KEY)
            .map_err(|e| e.to_string())?;
        if enabled {
            // 开着自启就清遗留键:绿色版(TokenScope)时代开过自启、后升级到安装版的
            // 机器,否则注册表残留一个指向已删除 exe 的自启项,每次开机报错(报告 B7)。
            // 放在写自己的键之前:下面开发版那条 return Err 也要保证遗留键被清掉。
            let _ = key.delete_value(LEGACY_NAME);
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            if in_build_tree(&exe) {
                // 只拒写自己这条 `Grandettoken` 键 —— 它可能正指向一份安装版,开发版无权删
                return Err(format!(
                    "当前运行的是构建目录里的开发版,不会写入开机自启(那样路径会被 cargo clean 删掉)。请用已安装的版本开启:{}",
                    exe.display()
                ));
            }
            // 路径可能含空格,必须加引号;--hidden 让开机自启直接进托盘不弹窗
            let cmd = format!("\"{}\" --hidden", exe.display());
            key.set_value(NAME, &cmd).map_err(|e| e.to_string())?;
        } else {
            // 关掉时键可能本就不存在,删除报错属正常,忽略
            let _ = key.delete_value(NAME);
            let _ = key.delete_value(LEGACY_NAME);
        }
        Ok(())
    }

    /// 旧版遗留的 TokenScope 自启项;它指向的文件**确实还在磁盘上**才算数
    /// (只剩一个死键的话清掉就行,不必打扰用户)。
    fn legacy_left() -> Option<String> {
        let hk = RegKey::predef(HKEY_CURRENT_USER).open_subkey(RUN_KEY).ok()?;
        let val: String = hk.get_value(LEGACY_NAME).ok()?;
        // 顺序要紧:旧版写进去的值是 `"<exe 路径>" --hidden`(与本文件 set() 同格式)。
        // 以前先 trim 引号再 split,尾引号没被去掉 → exists() 为假 → 这条提醒永不出现。
        let raw = val.split(" --").next().unwrap_or("").trim().trim_matches('"').trim();
        let path = PathBuf::from(raw);
        if path.as_os_str().is_empty() || !path.exists() {
            return None;
        }
        Some(format!(
            "这台电脑上还有一份旧版({})。它每次开机都会自己把自启项写回来,新版本删不掉,所以开机会同时出现两个程序 —— 请卸载或改名那个旧目录。",
            path.display()
        ))
    }

    /// 开机时与注册表对齐(容忍失败),并把需要告知用户的问题返回出去。
    pub fn sync(enabled: bool) -> Option<String> {
        // 顺序要紧:legacy_left() 要在 set() 删掉那条键之前读
        let notice = legacy_left();
        let failed = match set(enabled) {
            Ok(()) => None,
            // 开发版跳过写入是一种"正当失败",其它注册表错误同样值得让用户看到
            Err(e) => Some(if in_self_build_tree() {
                e
            } else {
                format!("开机自启同步失败:{e}")
            }),
        };
        match (notice, failed) {
            (Some(a), Some(b)) => Some(format!("{a} 另外 {b}")),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    /// 开机自启项在不在、指向哪个 exe(体检只读,不改注册表)。
    /// 返回 Some(路径) = 键存在;None = 没有这条键。
    pub fn entry_present() -> Option<String> {
        let hk = RegKey::predef(HKEY_CURRENT_USER).open_subkey(RUN_KEY).ok()?;
        hk.get_value::<String, _>(NAME).ok()
    }

    fn in_self_build_tree() -> bool {
        std::env::current_exe().map(|p| in_build_tree(&p)).unwrap_or(false)
    }
}

#[cfg(not(windows))]
mod autostart {
    pub fn set(_enabled: bool) -> Result<(), String> {
        Err("开机自启仅支持 Windows".into())
    }
    pub fn sync(_enabled: bool) -> Option<String> {
        None
    }
    /// 非 Windows 没有注册表自启项(体检如实报"不适用")。
    pub fn entry_present() -> Option<String> {
        None
    }
}

#[tauri::command]
async fn set_autostart(enabled: bool) -> Result<(), String> {
    // HKCU 注册表读写:同步命令会占住主线程(报告 P1-14)
    tokio::task::spawn_blocking(move || autostart::set(enabled))
        .await
        .map_err(|e| format!("设置开机自启失败: {e}"))?
}

/// 托盘角标。前端用 canvas 画好 32×32 的 RGBA(图标同款造型 + 状态色角标),
/// 这里换成托盘图标并把文字放进 tooltip —— 托盘图标只有 16px,数字放 tooltip,
/// 和胶囊共用同一套渠道与轮播逻辑。
///
/// 像素走 base64 而不是 `Vec<u8>`:Tauri 把命令参数序列化成 JSON,4096 字节的
/// RGBA 会变成两万多个字符的数组,每几秒轮播一次白耗流量(报告 O-9)。
/// 尺寸不符直接报错:图标画错时宁可保留上一帧,也不要把花屏贴到托盘上。
#[tauri::command]
fn set_tray_icon(
    app: tauri::AppHandle,
    rgba_b64: String,
    size: u32,
    tooltip: String,
) -> Result<(), String> {
    use base64::Engine as _;
    let tray = app.tray_by_id("main").ok_or("托盘不存在")?;
    let rgba = base64::engine::general_purpose::STANDARD
        .decode(rgba_b64.as_bytes())
        .map_err(|e| format!("托盘图标 base64 解码失败: {e}"))?;
    // 尺寸先夹住再乘:极端值下 size*size*4 会在 u32 里回绕,回绕"恰好对上"时
    // 畸形尺寸就被交给 Shell_NotifyIcon(图标一块花,P3-24)
    if !(8..=64).contains(&size) {
        return Err(format!("托盘图标尺寸只能是 8~64 像素,收到 {size}"));
    }
    let want = (size as usize) * (size as usize) * 4;
    if rgba.len() != want {
        return Err(format!("托盘图标尺寸不符: {size}x{size} 需要 {want} 字节,收到 {}", rgba.len()));
    }
    let img = tauri::image::Image::new_owned(rgba, size, size);
    tray.set_icon(Some(img)).map_err(|e| e.to_string())?;
    tray.set_tooltip(Some(&tooltip)).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn window_cmd(app: tauri::AppHandle, action: String) -> Result<(), String> {
    let win = app.get_webview_window("main").ok_or("窗口不存在")?;
    match action.as_str() {
        "hide" => win.hide().map_err(|e| e.to_string())?,
        "minimize" => win.minimize().map_err(|e| e.to_string())?,
        "quit" => {
            // 正在写客户端配置时立刻退出,进程会停在"已落盘、还没记账"的瞬间,
            // 重启后没有任何台账能回滚它(P1-15 那条窗口期)。走同一个等待逻辑。
            quit_when_idle(&app);
        }
        _ => return Err(format!("未知操作: {action}")),
    }
    Ok(())
}

/// 全局锁序约定:**先 store 后 config**。所有同时拿两把锁的命令都必须按这个
/// 顺序,否则与 check_update / skip_update_version(store→config)交错时会 AB-BA 死锁。
/// 锁一律走 `lock_rw` 毒化恢复:一次 panic 不该把后续所有命令永久卡死。
fn lock_rw<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// 往"启动期非致命问题"这条串上再接一条。多条都要让用户看到,
/// 而不是后面那条把前面那条悄悄顶掉。
fn note(prev: Option<String>, msg: String) -> Option<String> {
    Some(match prev {
        Some(p) => format!("{p};另外 {msg}"),
        None => msg,
    })
}

/// 置顶的唯一写入口:窗口状态与配置一起改,不会出现"按钮亮了其实没置顶"。
/// 折叠形态(紧凑条/胶囊/贴边)没有置顶按钮,靠设置页里的同一个开关控制。
#[tauri::command]
async fn set_pin(app: tauri::AppHandle, state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    refuse_if_config_broken()?;
    let win = app.get_webview_window("main").ok_or("窗口不存在")?;
    // 先落库再动窗口:反过来会出现本函数注释声称"不可能"的状态 ——
    // 窗口实际置顶了而按钮不亮、下次启动也不置顶(报告 P3-27)。
    // 走 save_config_off_main:同步命令是在主线程上跑的,而这里要写 SQLite 并等 store 锁,
    // 90 天清理正持着那把锁时会整窗卡住(P1-14 的同一类问题)。
    save_config_off_main(&state, move |c| c.always_on_top = enabled).await?;
    if let Err(e) = win.set_always_on_top(enabled) {
        // 窗口没改成就把刚写的配置撤回,别让界面状态与真实置顶态背离
        let _ = save_config_off_main(&state, move |c| c.always_on_top = !enabled).await;
        return Err(e.to_string());
    }
    Ok(())
}

// ───────────────────── 跌破红线的系统通知(裁决 3A) ─────────────────────

/// 已通知过的「渠道 + 本地日」:同一天里同一个渠道只响一次。
/// 只放内存:重启最多多响一次,不值得为它写盘(小鲸鱼"同一事件只响一次"同款)。
static NOTIFIED_CRIT: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
/// 上一轮的"红线集合",用来识别**跌破的那一刻**而不是持续低位期间每轮都响。
static PREV_CRIT: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);

/// 渠道算不算"跌破红线"。**与前端 `toneOf()` 逐条同口径** —— 两边判据必须一致,
/// 否则会出现"界面已经飘红、通知却说没事"这种自相矛盾。
///   百分比型:看主窗剩余;  金额型:剩余 / 本轮额度(拿不到分母就不算,与界面同);
///   积分型:不看余额,只看近 7 天将过期的占比(界面上 >= 30% 就是 bad);
///   取不到数 / 已隐藏 一律不算 —— 失联不等于欠费,这是本产品的第一条红线。
fn crit_now(v: &ChannelView, cfg: &Config) -> bool {
    use tokenscope_core::providers::Kind;
    if !v.has_key || !v.valid || v.hidden {
        return false;
    }
    match v.kind {
        Kind::Percent => v
            .windows
            .iter()
            .find(|w| w.main)
            .or_else(|| v.windows.first())
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
fn crit_body(v: &ChannelView) -> String {
    use tokenscope_core::providers::Kind;
    match v.kind {
        Kind::Percent => {
            let pct = v
                .windows
                .iter()
                .find(|w| w.main)
                .or_else(|| v.windows.first())
                .map(|w| w.remain_percent)
                .unwrap_or(0.0);
            format!("{} 剩余 {:.0}%,已经很低了 · 打开挂件查看", v.name, pct)
        }
        Kind::Amount => match (v.remaining, v.claim.as_ref().map(|c| c.amount)) {
            (Some(r), Some(d)) if d > 0.0 => format!(
                "{} 剩余 ¥{:.2}({:.0}%),本轮额度 ¥{:.0} · 打开挂件查看",
                v.name,
                r,
                (r / d * 100.0).clamp(0.0, 100.0),
                d
            ),
            (Some(r), _) => format!("{} 余额只剩 ¥{:.2} · 打开挂件查看", v.name, r),
            _ => format!("{} 余额很低 · 打开挂件查看", v.name),
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
                format!("{} 有 {:.0} 分将在 7 天内过期(共 {:.0} 分)· 打开挂件查看", v.name, within, r)
            }
        }
    }
}

/// 每轮取数后检查"刚刚跌破红线"的渠道,发一条系统通知。
/// 三种克制:只在**跌破那一刻**、**一天一次**、**按渠道各自记账**。
fn maybe_notify_crit(app: &tauri::AppHandle, views: &[ChannelView]) {
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
    for v in views {
        let was = prev.get(&v.id).copied().unwrap_or(false);
        let now = cur.get(&v.id).copied().unwrap_or(false);
        if !now || was {
            continue;
        }
        {
            let mut g = NOTIFIED_CRIT.lock().unwrap_or_else(|p| p.into_inner());
            let m = g.get_or_insert_with(HashMap::new);
            if m.get(&v.id).map(|d| *d == today).unwrap_or(false) {
                continue;
            }
            m.insert(v.id.clone(), today.clone());
        }
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
        // 通知本体不回跳(插件不支持动作),闪一下任务栏是最接近的替代:
        // 窗口藏着时"有人注意到"的概率比静默高得多。
        if let Some(w) = app.get_webview_window("main") {
            if !w.is_visible().unwrap_or(false) {
                let _ = w.request_user_attention(Some(tauri::UserAttentionType::Informational));
            }
        }
    }
}

// ───────────────────────── 渠道体检(裁决 2A) ─────────────────────────

/// 按**显示宽度**补空格:中文一个字占两列,`{:<12}` 会把报告排歪。
fn pad_display(s: &str, width: usize) -> String {
    let w: usize = s
        .chars()
        .map(|c| if (c as u32) > 0x2E7F { 2 } else { 1 })
        .sum();
    let mut out = s.to_string();
    for _ in w..width {
        out.push(' ');
    }
    out
}

fn host_of(url: &Option<String>) -> String {
    match url {
        Some(u) => {
            let rest = u.split_once("://").map(|(_, r)| r).unwrap_or(u);
            rest.split(['/', '?']).next().unwrap_or(rest).to_string()
        }
        None => "(未记录)".into(),
    }
}

/// 系统(WinINET)注册表代理的状态:返回"(是否启用, 给人看的一句话)"。
///
/// 为什么要单独读它:挂件取数用的 reqwest 0.12.28 的 default feature 里带着
/// `system-proxy`,hyper-util 会读 `HKCU\Software\Microsoft\Windows\CurrentVersion\
/// Internet Settings` 的 ProxyEnable —— 也就是说**注册表代理是真的影响挂件的**。
/// 体检报告以前只查环境变量,查不到就印"未检测到(全部直连)":在本机
/// (ProxyEnable=1、ProxyServer=127.0.0.1:7897、环境变量为空)是一句错话,
/// 会把"取数为什么连不上/走的哪个出口"的排查方向带歪(2026-10-19 取证结掉第一轮 P2-12)。
#[cfg(windows)]
fn system_proxy_state() -> (bool, String) {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    let Ok(k) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(KEY) else {
        return (false, "注册表里没有 Internet Settings 项".to_string());
    };
    let on = k.get_value::<u32, _>("ProxyEnable").unwrap_or(0) != 0;
    if !on {
        return (false, "未启用".to_string());
    }
    let server = k.get_value::<String, _>("ProxyServer").unwrap_or_default();
    let pac = k.get_value::<String, _>("AutoConfigURL").unwrap_or_default();
    let server = server.trim();
    let pac = pac.trim();
    (
        true,
        if !server.is_empty() {
            format!("已启用 → {server}")
        } else if !pac.is_empty() {
            format!("已启用(PAC)→ {pac}")
        } else {
            "已启用,但注册表里没写地址".to_string()
        },
    )
}

#[cfg(not(windows))]
fn system_proxy_state() -> (bool, String) {
    (false, "非 Windows,注册表代理不适用".to_string())
}

// ───────────────────── 自定义渠道的增删改(裁决 6A) ─────────────────────

/// 下一个自定义渠道 id(从 1 开始找空位;删掉再建不复用旧 id,
/// 免得和"历史快照属于谁"这件事搅在一起)。
fn next_custom_id(cfg: &Config) -> String {
    let mut n = 1u32;
    loop {
        let id = format!("custom-{n}");
        if !cfg.custom_channels.iter().any(|c| c.id == id) {
            return id;
        }
        n += 1;
    }
}

/// 「测试连接」在**保存之前**:先校验定义本身,再真打一次。
/// 测试通过才允许保存 —— 这就是"坏配置进不来"的那道门(照小鲸鱼的做法)。
#[tauri::command]
async fn custom_test(
    app: tauri::AppHandle,
    ch: tokenscope_core::custom::CustomChannel,
    key: String,
) -> Result<tokenscope_core::fetch::ProbeReport, String> {
    ch.validate()?;
    if key.trim().is_empty() {
        return Err("先填密钥再测试".into());
    }
    let http = app.state::<AppState>().fetcher.http.clone();
    let started = std::time::Instant::now();
    let r = tokenscope_core::custom::fetch_custom(&http, &ch, &key).await;
    // 探活模式:HTTP 通了就算过(它本来就不解析余额)
    Ok(tokenscope_core::fetch::ProbeReport {
        id: ch.id.clone(),
        name: ch.name.clone(),
        ok: r.valid,
        auth_label: if ch.probe_only {
            "探活模式".into()
        } else {
            format!("字段 {}(scale {})", ch.path, ch.normalized_scale())
        },
        url_used: r.url_used,
        elapsed_ms: started.elapsed().as_millis() as u64,
        remaining: r.remaining,
        used: None,
        total: None,
        unit: ch.normalized_unit(),
        error: r.error,
        // 还没保存,谈不上本地管道
        snapshot_last_at: None,
        snapshot_count_7d: 0,
        accounts: 1,
    })
}

/// 保存一个新自定义渠道。**先写密钥、再落配置** —— 反过来会留下"配置里有、密钥没有"
/// 的半截渠道(界面显示未配置,用户不知道要不要再填一次)。
#[tauri::command]
async fn custom_add(
    app: tauri::AppHandle,
    ch: tokenscope_core::custom::CustomChannel,
    key: String,
) -> Result<String, String> {
    ch.validate()?;
    if key.trim().is_empty() {
        return Err("密钥不能为空".into());
    }
    let (id, _) = {
        let st = app.state::<AppState>();
        let c = lock_rw(&st.config);
        (next_custom_id(&c), ())
    };
    let mut saved = ch.clone();
    saved.id = id.clone();
    // 小写化颜色/单位在这里做一次,免得每种渲染各写一遍
    saved.unit = ch.normalized_unit();
    let cid = id.clone();
    let k = key.clone();
    // N29(2026-10-21):写值前先清引用指针。next_custom_id 用的是"最小空号",
    // 删掉 custom-1 再新建就会复用它 —— 而上一个渠道若留下孤儿指针(N02 之前删的),
    // 新渠道会**继承别人的引用**,卡片上显示"引用密钥箱",用的却是别人的密钥。
    // 清了指针,id 复用就是安全的(也就不必为它引入单调计数器)。
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        secrets::link_clear(&cid)?;
        secrets::set(&cid, &k)
    })
    .await
    .map_err(|e| format!("写密钥任务失败: {e}"))??;
    let st = app.state::<AppState>();
    let push = saved.clone();
    if let Err(e) = save_config_off_main(&st, move |c| c.custom_channels.push(push)).await {
        // 配置没落成:把刚写的密钥删掉,不留孤儿凭据
        let cid2 = id.clone();
        let _ = tokio::task::spawn_blocking(move || secrets::delete(&cid2)).await;
        return Err(e);
    }
    st.fetcher.invalidate();
    Ok(id)
}

/// 删除一个自定义渠道。**历史快照保留**(与隐藏渠道同一条纪律:数据不删),
/// 只清定义与密钥。
#[tauri::command]
async fn custom_remove(app: tauri::AppHandle, id: String) -> Result<String, String> {
    let st = app.state::<AppState>();
    key_allowed(&st, &id)?;
    let name = {
        let c = lock_rw(&st.config);
        c.custom_channels
            .iter()
            .find(|x| x.id == id)
            .map(|x| x.name.clone())
            .unwrap_or_else(|| id.clone())
    };
    let id2 = id.clone();
    save_config_off_main(&st, move |c| c.custom_channels.retain(|x| x.id != id2)).await?;
    // N02(2026-10-21):删渠道要把**三样**都清干净 —— 直接填的值、引用指针、反查索引行。
    // 以前只 let _ = secrets::delete():① 引用指针留着,渠道"删了但密钥箱还说正被它使用";
    // ② 连删失败都被 let _ 吞了,凭据留在系统里没人知道(凭据纪律问题)。
    let cid = id.clone();
    let purge = purge_channel_credentials(&app, &cid).await;
    st.fetcher.invalidate();
    // 渠道定义已经删了,不该因为清凭据失败就把整个操作报成失败 ——
    // 但也不能静默:如实告诉用户"渠道删了,凭据没清干净"。
    let purge_note = match purge {
        Ok(()) => String::new(),
        Err(e) => format!(" · ⚠ 它的密钥可能没清干净({e}),可在密钥箱里手动检查"),
    };
    Ok(format!(
        "已删除自定义渠道「{name}」{purge_note} · 它的历史快照仍在库里(只是不再显示,也不参与合计)"
    ))
}

/// 一个渠道的「测试连接」:真打一次接口,把过程如实报出来。
/// 与轮询共用同一条取数链路,但**不写快照**(手动测试不该篡改观测缺口)。
#[tauri::command]
async fn probe_channel(
    app: tauri::AppHandle,
    id: String,
) -> Result<tokenscope_core::fetch::ProbeReport, String> {
    // 自定义渠道:用自己的定义 + 凭据(账户名 = id)打一次,报告口径与内置一致
    if providers::find(&id).is_none() {
        let ch = {
            let state = app.state::<AppState>();
            key_allowed(&state, &id)?;
            let c = lock_rw(&state.config);
            c.custom_channels
                .iter()
                .find(|x| x.id == id)
                .cloned()
                .ok_or_else(|| format!("没有这个自定义渠道:{id}"))?
        };
        let cid = id.clone();
        let key = tokio::task::spawn_blocking(move || secrets::channel_key(&cid).ok().flatten())
            .await
            .unwrap_or(None);
        let started = std::time::Instant::now();
        let Some(k) = key else {
            return Ok(tokenscope_core::fetch::ProbeReport {
                id: ch.id.clone(),
                name: ch.name.clone(),
                ok: false,
                auth_label: "未配置密钥".into(),
                url_used: None,
                elapsed_ms: 0,
                remaining: None,
                used: None,
                total: None,
                unit: ch.normalized_unit(),
                error: Some("还没填密钥".into()),
                snapshot_last_at: None,
                snapshot_count_7d: 0,
                accounts: 1,
            });
        };
        let r = {
            let http = app.state::<AppState>().fetcher.http.clone();
            tokenscope_core::custom::fetch_custom(&http, &ch, &k).await
        };
        return Ok(tokenscope_core::fetch::ProbeReport {
            id: ch.id.clone(),
            name: ch.name.clone(),
            ok: r.valid,
            auth_label: "自定义渠道 · 密码管理器".into(),
            url_used: r.url_used,
            elapsed_ms: started.elapsed().as_millis() as u64,
            remaining: r.remaining,
            used: None,
            total: None,
            unit: ch.normalized_unit(),
            error: r.error,
            snapshot_last_at: None,
            snapshot_count_7d: 0,
            accounts: 1,
        });
    }
    let def = providers::find(&id).ok_or_else(|| format!("没有这个渠道:{id}"))?;
    let (fetcher, store) = {
        let state = app.state::<AppState>();
        (state.fetcher.clone(), state.store.clone())
    };
    Ok(fetcher.probe_report(def, &store).await)
}

/// 一键体检:逐渠道真打一次 + 本机四项检查,产出一份**可复制**的纯文本。
/// 报告里只有域名、状态、数值与路径 —— 没有密钥、没有完整响应体。
#[tauri::command]
async fn run_diagnostic(app: tauri::AppHandle) -> Result<String, String> {
    let (fetcher, store) = {
        let state = app.state::<AppState>();
        (state.fetcher.clone(), state.store.clone())
    };
    let cfg = {
        let s = lock_rw(&store);
        Config::try_load(&s).unwrap_or_default()
    };
    let mut lines: Vec<String> = vec![format!(
        "Grandettoken 体检 · {}",
        tokenscope_core::fetch::local_datetime_string()
    )];
    let total = providers::PROVIDERS.len();
    let mut need = 0usize;
    for (i, def) in providers::PROVIDERS.iter().enumerate() {
        let idx = format!("[{}/{}]", i + 1, total);
        if cfg.hidden_channels.iter().any(|h| h == def.id) {
            lines.push(format!(
                "{} {} 已隐藏(不发请求,跳过)",
                idx,
                pad_display(def.name, 12)
            ));
            continue;
        }
        let r = fetcher.probe_report(def, &store).await;
        if r.ok {
            let val = match r.remaining {
                Some(v) if r.unit == "%" => format!("{v:.0}%"),
                Some(v) if r.unit == "CNY" => format!("¥{v:.2}"),
                Some(v) => format!("{v:.0}"),
                None => "—".to_string(),
            };
            lines.push(format!(
                "{} {} ✓ {}ms · {} · {}",
                idx,
                pad_display(def.name, 12),
                r.elapsed_ms,
                host_of(&r.url_used),
                val
            ));
        } else {
            need += 1;
            lines.push(format!(
                "{} {} ✗ {} —— 显示的是本地快照,取数未通",
                idx,
                pad_display(def.name, 12),
                r.error.unwrap_or_else(|| "失败".into())
            ));
        }
    }
    // 本机那一半:网络通了不等于记录在写,两半各证一半
    // 自定义渠道:逐个真打一次(它们不在 PROVIDERS 里,单独一轮)
    {
        let custom = { lock_rw(&app.state::<AppState>().config).custom_channels.clone() };
        for (i, ch) in custom.iter().enumerate() {
            let idx = format!("[自{}/{}]", i + 1, custom.len());
            if cfg.hidden_channels.iter().any(|h| h == &ch.id) {
                lines.push(format!("{} {} 已隐藏(不发请求,跳过)", idx, pad_display(&ch.name, 12)));
                continue;
            }
            let cid = ch.id.clone();
            let key = tokio::task::spawn_blocking(move || secrets::channel_key(&cid).ok().flatten())
                .await
                .unwrap_or(None);
            match key {
                None => {
                    need += 1;
                    lines.push(format!(
                        "{} {} ✗ 还没填密钥 —— 在渠道卡里填一次",
                        idx,
                        pad_display(&ch.name, 12)
                    ));
                }
                Some(k) => {
                    let http = app.state::<AppState>().fetcher.http.clone();
                    let r = tokenscope_core::custom::fetch_custom(&http, ch, &k).await;
                    if r.valid {
                        let val = match r.remaining {
                            Some(v) if ch.normalized_unit() == "%" => format!("{v:.0}%"),
                            Some(v) if ch.normalized_unit() == "CNY" => format!("¥{v:.2}"),
                            Some(v) => format!("{v:.0}"),
                            None => "—".to_string(),
                        };
                        lines.push(format!(
                            "{} {} ✓ {}{}",
                            idx,
                            pad_display(&ch.name, 12),
                            host_of(&r.url_used),
                            if r.probe_only {
                                " · 探活通过(不解析余额)".to_string()
                            } else {
                                format!(" · 读到 {val}")
                            }
                        ));
                    } else {
                        need += 1;
                        lines.push(format!(
                            "{} {} ✗ {}",
                            idx,
                            pad_display(&ch.name, 12),
                            r.error.unwrap_or_else(|| "失败".into())
                        ));
                    }
                }
            }
        }
    }
    let db_err = {
        let s = lock_rw(&store);
        s.set_setting("diag_ping", &now_ts().to_string()).err()
    };
    lines.push(format!(
        "本机:快照库可写 {}",
        if db_err.is_none() { "✓" } else { "✗" }
    ));
    let auto = autostart::entry_present();
    let auto_txt = match (auto, cfg.autostart) {
        (Some(_), true) => "✓ 已注册".to_string(),
        (Some(_), false) => "注册表里有,但设置里是关的(点一次开关即可对齐)".to_string(),
        (None, true) => "✗ 设置里开着,注册表里没有(点一次开关重写)".to_string(),
        (None, false) => "未启用".to_string(),
    };
    lines.push(format!("      开机自启 {auto_txt}"));
    let envp: Vec<String> = ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"]
        .iter()
        .filter_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()).map(|v| format!("{k}={v}")))
        .collect();
    let (sys_on, sys_txt) = system_proxy_state();
    lines.push(format!(
        "      代理 · 环境变量 {}",
        if envp.is_empty() {
            "无".to_string()
        } else {
            envp.join(" · ")
        }
    ));
    lines.push(format!("      代理 · 系统(注册表){sys_txt}"));
    lines.push(format!(
        "      取数是否走代理:{}",
        if !envp.is_empty() || sys_on {
            "是(上面两行任一开着,reqwest 都会走它;每日签到例外,走强制直连)"
        } else {
            "否,直连(注意:代理软件若是 TUN/全局模式,系统层面仍会接管,这里看不出来)"
        }
    ));
    // ── WorkBuddy 专项:这条链最长、失败点最多,而且是"装上就取不到数"的最常见来源。
    //    用户报问题时把这几行贴过来,基本就能定位到"没装 / 没找到 / 拿不到密钥 / 过期 / 被拒"。
    lines.push("      ── WorkBuddy 凭据链 ──".to_string());
    for l in tokenscope_core::workbuddy::diag_summary(now_ts()) {
        lines.push(format!("      {l}"));
    }
    if let Ok(dir) = app.path().app_data_dir() {
        lines.push(format!("      数据目录 {}", dir.display()));
    }
    if let Some(e) = lock_rw(&app.state::<AppState>().startup_error).clone() {
        lines.push(format!("      启动期错误 {e}"));
    }
    lines.push(if need == 0 {
        "结论:全部正常".to_string()
    } else {
        format!("结论:{need} 项需要处理(上面对应行标 ✗)")
    });
    Ok(lines.join("\n"))
}

/// 后台轮询。渠道都是账务接口,查询余额不消耗 token,所以间隔可以压得比较短;
/// 唯一约束是对方的限流礼貌性,所以失败时退避。
async fn poll_loop(app: tauri::AppHandle) {    let mut failures = 0u32;
    // 90 天清理原来只在启动时做一次:这台机器常年不重启的话快照表就一直长(报告 O-2)
    let mut last_prune = now_ts();

    loop {
        if UPDATING.load(std::sync::atomic::Ordering::Relaxed) {
            // 更新期间不取数:进程马上可能被 install 结束,拿回来的数也没意义
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            continue;
        }
        let (active_sec, idle_sec, backoff_sec) = {
            let state = app.state::<AppState>();
            let c = lock_rw(&state.config);
            (
                c.active_interval_sec,
                c.idle_interval_sec,
                c.backoff_interval_sec,
            )
        };

        let visible = app
            .get_webview_window("main")
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false);

        let base = if visible { active_sec } else { idle_sec };
        let wait = if failures >= 3 { backoff_sec } else { base };

        // 抖动,避免多渠道同秒并发撞上对方限流
        let jitter = (now_ts() % 5) as u64;
        tokio::time::sleep(std::time::Duration::from_secs(wait + jitter)).await;

        // 先把 Arc 取出来,避免 State 借用跨越 await
        let (store, fetcher) = {
            let state = app.state::<AppState>();
            (state.store.clone(), state.fetcher.clone())
        };

        if now_ts() - last_prune >= 86_400 {
            last_prune = now_ts();
            let s = lock_rw(&store);
            if let Err(e) = s.prune(now_ts() - 90 * 86_400, tokenscope_core::fetch::local_tz_offset()) {
                eprintln!("定期清理快照失败: {e}");
            }
        }

        // 走 fetch_shared:用户点「立即刷新」撞上轮询时排队等结果,
        // 而不是对同一批账务接口发出双份请求(报告 O-4)
        let views = fetcher.fetch_shared(&store).await;

        if views.iter().any(|v| v.valid) {
            failures = 0;
        } else if views.iter().any(|v| v.has_key) {
            failures += 1;
        }
        // 一个渠道都没配凭据时不算失败:那是全新安装,退避到 5 分钟只会让
        // 用户「填好 key 却半天看不到数字」

        LAST_REFRESH.store(now_ts(), std::sync::atomic::Ordering::Relaxed);
        let _ = app.emit("channels-updated", &views);
        // 跌破红线的系统通知(裁决 3A):只在跌破那一刻、一天一次、按渠道记账
        maybe_notify_crit(&app, &views);
    }
}

/// 上次取数的时刻(秒)。托盘/热键把窗口唤回来时用它判断要不要补一轮:
/// 轮询循环只在**进入下一轮时**读一次窗口可见性,然后睡满整个间隔(折叠态最长 30 分钟),
/// 所以"藏了两小时再打开"看到的仍是两小时前的数字(报告 P3-37)。
static LAST_REFRESH: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// 窗口重新显示时调用:数据比 max_age_sec 旧就补一轮。新鲜结果不重复打接口
/// (fetch_shared 自己也会与在途的轮询合流,不会发出双份请求)。
fn refresh_if_stale(app: &tauri::AppHandle, max_age_sec: i64) {
    let last = LAST_REFRESH.load(std::sync::atomic::Ordering::Relaxed);
    if last != 0 && now_ts() - last < max_age_sec {
        return;
    }
    let h = app.clone();
    tauri::async_runtime::spawn(async move {
        refresh_now(&h).await;
    });
}

/// 退出:正在写客户端配置时先等它写完(最多 10 秒)再退。
/// 立即退出会停在"文件已改、台账还没落"的那一瞬间,重启后没有任何记录能回滚它(P1-15)。
/// 交给后台任务等,按钮立刻有反应,不会让人以为点了没动。
fn quit_when_idle(app: &tauri::AppHandle) {
    if INJECTING.load(std::sync::atomic::Ordering::Relaxed) {
        let h = app.clone();
        tauri::async_runtime::spawn(async move {
            for _ in 0..50 {
                if !INJECTING.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            h.exit(0);
        });
    } else {
        app.exit(0);
    }
}

async fn refresh_now(app: &tauri::AppHandle) {
    let (store, fetcher) = {
        let state = app.state::<AppState>();
        (state.store.clone(), state.fetcher.clone())
    };
    let views = fetcher.fetch_shared(&store).await;
    LAST_REFRESH.store(now_ts(), std::sync::atomic::Ordering::Relaxed);
    let _ = app.emit("channels-updated", &views);
}

// ───────────── 检查更新 ─────────────
// 更新逻辑放 Rust 而不是前端:前端是无打包器的静态页,拿不到
// @tauri-apps/plugin-updater 的 npm 包。返回值 + 事件这套路子与
// get_channels / channels-updated 一致 —— 前端只负责渲染,不做判断。
//
// 端点与公钥都在 tauri.conf.json 的 plugins.updater 里,插件自己读。
const UPDATE_CHECK_INTERVAL_SEC: i64 = 24 * 60 * 60;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    /// 当前运行的版本
    pub current: String,
    /// 有新版本时为 true
    pub available: bool,
    pub version: String,
    pub notes: String,
    pub date: Option<String>,
    /// 这次是否真的去问过服务器。节流 / 关掉自动检查时都是 false。
    pub checked_now: bool,
    /// 只有真去问了才有值。失败原因只给手动点击看,自动检查一律静默。
    pub error: Option<String>,
}

impl UpdateState {
    fn idle(current: &str) -> Self {
        Self {
            current: current.into(),
            available: false,
            version: String::new(),
            notes: String::new(),
            date: None,
            checked_now: false,
            error: None,
        }
    }
}

// 注意:async 命令只要带了 State(借用)就必须返回 Result,否则 tauri 的代码生成
// 过不了(AsyncCommandMustReturnResult)。所以这里是 Result<UpdateState, String>,
// Err 只表示命令本身坏了,更新"没有可用版本"是 Ok 里的 available:false。
#[tauri::command]
async fn check_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    force: bool,
) -> Result<UpdateState, String> {
    use tauri_plugin_updater::UpdaterExt;
    let current = env!("CARGO_PKG_VERSION").to_string();

    let (auto, last_check_at, skipped) = {
        let cfg = lock_rw(&state.config);
        (
            cfg.auto_check_update,
            cfg.last_check_at,
            cfg.skipped_version.clone(),
        )
    };
    // 自动检查的闸门:开关关着、或距上次不足 24 小时,就直接闲置,不发请求。
    // 手动点「检查更新」(force=true)一律放行。
    if !force && !auto {
        return Ok(UpdateState::idle(&current));
    }
    if !force {
        if let Some(last) = last_check_at {
            if now_ts() - last < UPDATE_CHECK_INTERVAL_SEC {
                return Ok(UpdateState::idle(&current));
            }
        }
    }

    // 只要真的要发请求就先把时间戳写掉 —— 失败也算检查过,
    // 否则一次网络故障会变成每次启动都重试的请求风暴。
    {
        // 锁必须用 lock_rw():store 被污染时 .lock() 会永久返回 Err,这段被整段跳过,
        // 24 小时节流等于失效(前端每 10 分钟真去打一次 GitHub)。
        let s = lock_rw(&state.store);
        // 锁序样板:这里 store→config,set_pin 等命令必须同序(见 lock_rw 注释)
        let mut cfg = lock_rw(&state.config);
        cfg.last_check_at = Some(now_ts());
        // 配置损坏时**不能**整份写回:内存里那份是 Config::default(),一写就把用户
        // 还能手工救回的原始 JSON 永久覆盖掉 —— 正好是 refuse_if_config_broken 要防的事,
        // 而这里是全文件唯一一个没问它的整份写入者(报告 P1-16 的反例)。
        if refuse_if_config_broken().is_ok() {
            let _ = cfg.save(&s);
        }
    }

    let updater = match app.updater_builder().build() {
        Ok(u) => u,
        Err(e) => {
            return Ok(UpdateState {
                checked_now: true,
                error: Some(e.to_string()),
                ..UpdateState::idle(&current)
            })
        }
    };
    Ok(match updater.check().await {
        Ok(Some(u)) => {
            // 「跳过此版本」要尊重到底:手动点也不该把跳过的版本重新弹出来。
            // 但请求确实发出去了 —— checked_now 必须为真,前端据此才敢刷新
            // 「上次检查时间」;设成 false 会让周期轮询把它当成"没问到"而跳过记账。
            if skipped.as_deref() == Some(u.version.as_str()) {
                UpdateState {
                    checked_now: true,
                    ..UpdateState::idle(&current)
                }
            } else {
                UpdateState {
                    current,
                    available: true,
                    version: u.version.clone(),
                    // 更新说明在远端清单里可能没有,缺失就留空,前端会显示占位
                    notes: u.body.clone().unwrap_or_default(),
                    date: u.date.as_ref().map(|d| d.to_string()),
                    checked_now: true,
                    error: None,
                }
            }
        }
        Ok(None) => UpdateState {
            checked_now: true,
            ..UpdateState::idle(&current)
        },
        // 网络不通、限流、清单格式不对都走这里。自动检查时前端不会显示它
        Err(e) => UpdateState {
            checked_now: true,
            error: Some(e.to_string()),
            ..UpdateState::idle(&current)
        },
    })
}

/// 下载并安装。Windows 上 install 那一步会直接退出进程,
/// 所以这里不会返回 "装好了" —— 前端别在它后面再弹确认框。
#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    // 先在途写入收尾(最多等 20 秒),再进维护态
    for _ in 0..200 {
        if !INJECTING.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    if INJECTING.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("有客户端配置正在写入,请等它结束后再更新(界面上那一步完成后重试)".to_string());
    }
    let _upd_guard = FlagGuard(&UPDATING);
    UPDATING.store(true, std::sync::atomic::Ordering::Relaxed);
    let updater = app.updater_builder().build().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "没有可用的更新".to_string())?;

    let mut received: u64 = 0;
    let mut total: u64 = 0;
    // 进度回调是**每个数据块**一次:几十 MB 会打出上万条事件,每条都带一次 JSON 序列化
    // + IPC + 前端全量重绘,面板与托盘在这期间明显顿(报告 P1-15)。按 200ms 节流。
    let h1 = app.clone();
    let mut last_emit = std::time::Instant::now();
    let h2 = app.clone();
    let _ = h1.emit(
        "update-progress",
        serde_json::json!({ "phase": "started", "received": 0, "total": 0 }),
    );
    update
        .download_and_install(
            move |chunk: usize, content_length: Option<u64>| {
                received += chunk as u64;
                if let Some(cl) = content_length {
                    total = cl;
                }
                if last_emit.elapsed() >= std::time::Duration::from_millis(200) || received == total {
                    last_emit = std::time::Instant::now();
                    let _ = h1.emit(
                        "update-progress",
                        serde_json::json!({ "phase": "downloading", "received": received, "total": total }),
                    );
                }
            },
            move || {
                let _ = h2.emit("update-progress", serde_json::json!({ "phase": "installing" }));
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit("update-progress", serde_json::json!({ "phase": "finished" }));
    Ok(())
}

/// 记住「跳过此版本」。
#[tauri::command]
async fn skip_update_version(state: State<'_, AppState>, version: String) -> Result<(), String> {
    refuse_if_config_broken()?;
    save_config_off_main(&state, move |c| c.skipped_version = Some(version)).await
}

/// 记住窗口位置(报告 O-6)。只在自由态下记 —— 吸附/紧凑条/胶囊的坐标是布局
/// 算出来的,记下来反而会在下次启动覆盖正确的默认位置。
/// 前端在拖动结束后调用一次,不做高频写盘。
#[tauri::command]
async fn save_window_pos(state: State<'_, AppState>, x: i32, y: i32) -> Result<(), String> {
    refuse_if_config_broken()?;
    save_config_off_main(&state, move |c| {
        c.win_x = Some(x);
        c.win_y = Some(y);
    })
    .await
}

/// 把请求的窗口坐标夹进"至少有一条边露在外面"的范围。
/// 多屏环境下用户拔掉副屏后,记下的坐标会落在屏幕外,窗口看起来像消失了。
/// 至少要露出 KEEP_X × KEEP_Y 的可视区,保证还能拖回来。
fn clamp_to_monitors(app: &tauri::AppHandle, x: i32, y: i32, w: i32, h: i32) -> (i32, i32) {
    const KEEP_X: i32 = 80;
    const KEEP_Y: i32 = 40;
    let Ok(monitors) = app.available_monitors() else {
        return (x, y);
    };
    if monitors.is_empty() {
        return (x, y);
    }
    // 完全落在某个显示器内 → 原样返回
    for m in &monitors {
        let (mx, my) = (m.position().x, m.position().y);
        let (mw, mh) = (m.size().width as i32, m.size().height as i32);
        if x >= mx && y >= my && x + w <= mx + mw && y + h <= my + mh {
            return (x, y);
        }
    }
    // 否则挑第一个显示器夹:左上角不越过 (mx+mw-KEEP) 边界
    let m = &monitors[0];
    let (mx, my) = (m.position().x, m.position().y);
    let (mw, mh) = (m.size().width as i32, m.size().height as i32);
    let cx = x.clamp(mx, (mx + mw - KEEP_X).max(mx));
    let cy = y.clamp(my, (my + mh - KEEP_Y).max(my));
    (cx, cy)
}

/// 当前版本号(前端底部显示用)。之前硬编码在 index.html 里,发版必改且会忘(报告 P2-15)。
#[tauri::command]
fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 取走启动期的非致命错误(配置损坏回落默认、快捷键注册失败)。
/// 取走即清空 —— 提示一次就够,别每次刷新都弹。
#[tauri::command]
fn take_startup_error(state: State<'_, AppState>) -> Option<String> {
    lock_rw(&state.startup_error).take()
}

// ─────────────── 本地密钥箱 ───────────────
//
// 设计见 `docs/design/design-vault-and-hidden-restore.html` 第三节(裁决 1A–7A):
//   · **密文在 Windows 凭据管理器**(服务名 `TokenScope:vault`),SQLite 只存索引(平台/标签/尾号/备注/时间)
//     —— 列表渲染**不需要解密**,只有「查看/复制」才读出那一条;
//   · 不导出、不自动使用、不同步上云、无主密码(边界 = 当前 Windows 账户)。

/// 界面提交的新建/编辑请求。`key` 为空 = 只改平台/标签/备注,不动密钥。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaultInput {
    id: Option<String>,
    platform: String,
    label: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    key: String,
}

#[tauri::command]
async fn vault_list(state: State<'_, AppState>) -> Result<Vec<tokenscope_core::store::VaultItem>, String> {
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || {
        let s = lock_rw(&store);
        s.vault_list()
    })
    .await
    .map_err(|e| format!("读取密钥箱失败: {e}"))?
}

/// 新建或编辑一条。**先写凭据、再写索引**:索引写失败就把刚写的凭据删掉,不留半条。
#[tauri::command]
async fn vault_save(
    state: State<'_, AppState>,
    item: VaultInput,
) -> Result<tokenscope_core::store::VaultItem, String> {
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || {
        use tokenscope_core::vault;
        refuse_while_updating("保存密钥")?;
        let platform = vault::valid_platform(&item.platform)?;
        let label = vault::valid_label(&item.label)?;
        let key = item.key.trim().to_string();
        let now = now_ts();

        // 库锁只圈住"读"这一段。下面要调 Windows 凭据管理器(外部 RPC,域账户/组策略
        // 出问题时可以卡到秒级),以前是持着 store 锁跨 RPC:那段时间里 get_series 等
        // 主线程命令在同一把锁上排队,整个面板冻住(报告 P1-7)。
        let (all, existing) = {
            let s = lock_rw(&store);
            (
                s.vault_list()?,
                match item.id.as_deref() {
                    Some(id) => s.vault_get(id)?,
                    None => None,
                },
            )
        };
        if item.id.is_some() && existing.is_none() {
            return Err("这条已经不在密钥箱里了(可能刚被删过)".into());
        }
        if vault::label_taken(&all, &platform, &label, item.id.as_deref()) {
            return Err(format!("「{platform}」下已经有一个叫「{label}」的了"));
        }

        let id = existing
            .as_ref()
            .map(|e| e.id.clone())
            .unwrap_or_else(vault::new_id);
        let hint = if key.is_empty() {
            existing
                .as_ref()
                .map(|e| e.hint.clone())
                .unwrap_or_else(|| "••••".into())
        } else {
            // 先写凭据:写不进去就别动索引
            secrets::vault_set(&id, &key)?;
            secrets::mask_of(&key)
        };
        if existing.is_none() && key.is_empty() {
            return Err("新建时要填密钥".into());
        }
        let out = tokenscope_core::store::VaultItem {
            id: id.clone(),
            platform,
            label,
            hint,
            note: item.note.trim().to_string(),
            created_at: existing.as_ref().map(|e| e.created_at).unwrap_or(now),
            updated_at: now,
        };
        let up = {
            let s = lock_rw(&store);
            s.vault_upsert(&out)
        };
        if let Err(e) = up {
            // 新建时回滚凭据,不留"索引里没有、凭据里有"的孤儿
            if existing.is_none() {
                let _ = secrets::vault_delete(&id);
            }
            return Err(e);
        }
        Ok(out)
    })
    .await
    .map_err(|e| format!("保存失败: {e}"))?
}

/// 删密钥箱条目的结果。
///
/// 为什么要有结构而不是 `Result<(),_>`(N07):删前拦截现在在**后端**做,
/// 而且扫的是真实指针 —— 有渠道在用时不直接报错,而是把清单交回去让界面确认。
/// 用错误字符串传清单是不可靠的(会被转义、会被吞),这里给结构化字段。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct VaultDeleteResult {
    /// true = 真的删了;false = 有渠道在用,等用户确认(此时 referrers 非空)
    deleted: bool,
    /// 正引用这条的渠道 id 清单(**按真实指针扫出来的**,不是查索引)
    referrers: Vec<String>,
}

/// 按**真实指针**反查:有哪些渠道正引用这个箱子条目。
///
/// 与 `store.vault_ref_channels`(查索引)的根本区别:这里问的是凭据管理器 ——
/// **书架说了算**。索引读坏了、过期了、被删了,都不影响这个判断。
///
/// 渠道清单来自配置(内置渠道 + 自定义渠道)。已删除渠道留下的孤儿指针**扫不到**
/// (keyring 不提供枚举),这一点在设计稿 §6 里写明了。
async fn ref_channels_by_pointer(
    app: &tauri::AppHandle,
    vault_id: &str,
) -> Result<Vec<String>, String> {
    let ids = {
        let st = app.state::<AppState>();
        let c = lock_rw(&st.config);
        let mut v: Vec<String> = providers::PROVIDERS.iter().map(|d| d.id.to_string()).collect();
        v.extend(c.custom_channels.iter().map(|x| x.id.clone()));
        v
    };
    let vid = vault_id.to_string();
    tokio::task::spawn_blocking(move || -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        for id in ids {
            if let Ok(Some(v)) = secrets::link_get(&id) {
                if v == vid {
                    out.push(id);
                }
            }
        }
        Ok(out)
    })
    .await
    .map_err(|e| format!("反查引用失败: {e}"))?
}

/// 按真实指针**整表重抄**索引。设计稿 `product-vault-ref-reconcile-2026-10-21` 的动作 1。
///
/// 一句话:**书架说了算,索引只是副本**。所以重建的方向永远是"读指针 → 写索引",
/// 绝不会反过来(拿索引去改指针 —— 那就成了用副本校正正本)。
///
/// 什么时候跑:① 启动后台跑一次;② 删箱子条目之后立刻跑一次(界面要马上对)。
/// 索引写失败不该让任何用户操作失败 —— 这里只记一行日志,下次启动还会重来。
async fn rebuild_vault_refs(app: &tauri::AppHandle) -> Result<(), String> {
    let ids = {
        let st = app.state::<AppState>();
        let c = lock_rw(&st.config);
        let mut v: Vec<String> = providers::PROVIDERS.iter().map(|d| d.id.to_string()).collect();
        v.extend(c.custom_channels.iter().map(|x| x.id.clone()));
        v
    };
    let store = app.state::<AppState>().store.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let mut pairs: Vec<(String, String)> = Vec::new();
        for id in ids {
            if let Ok(Some(vid)) = secrets::link_get(&id) {
                pairs.push((id, vid));
            }
        }
        let n = pairs.len();
        lock_rw(&store).vault_ref_rebuild(&pairs)?;
        tokenscope_core::diaglog::write_keyed(
            "vault",
            "ref-rebuild",
            &format!("引用索引已按真实指针重抄 · {n} 条"),
        );
        Ok(())
    })
    .await
    .map_err(|e| format!("重抄引用索引失败: {e}"))?
}

/// 启动后跑一次索引对账(不挡界面)。
fn spawn_vault_ref_rebuild(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        // 让界面先画出来:对账要读 N 次凭据管理器,没必要和首屏抢
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        if let Err(e) = rebuild_vault_refs(&app).await {
            tokenscope_core::diaglog::write_keyed("vault", "ref-rebuild-fail", &e);
        }
    });
}

#[tauri::command]
async fn vault_delete(
    app: tauri::AppHandle,
    id: String,
    force: Option<bool>,
) -> Result<VaultDeleteResult, String> {
    // N07:删前拦截搬到后端,并且**扫真实指针**而不是查索引。
    // 以前拦截在浏览器里靠索引:索引读失败就静默放行、索引不准就拦了个寂寞。
    let referrers = ref_channels_by_pointer(&app, &id).await?;
    if !referrers.is_empty() && !force.unwrap_or(false) {
        return Ok(VaultDeleteResult { deleted: false, referrers });
    }
    let state = app.state::<AppState>();
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || {
        // 先删索引(用户视角"这条没了"),再删凭据;凭据删失败也不该把索引变回来
        {
            let s = lock_rw(&store);
            s.vault_delete(&id)?;
        }
        // 索引已经删了,凭据删不掉的话那把密钥的密文会**永远**留在凭据管理器里:
        // 界面再也列不出、也删不掉(vault_reveal 先校验索引)。以前只 eprintln 就当成功
        // (报告 P2-14)。重试一次,仍失败就把话说明白,让用户知道要去手动清。
        if let Err(e) = secrets::vault_delete(&id).or_else(|e| {
            std::thread::sleep(std::time::Duration::from_millis(200));
            secrets::vault_delete(&id).map_err(|_| e)
        }) {
            return Err(format!(
                "条目已从列表移除,但密文没能从 Windows 凭据管理器删掉:{e}。
                 请到「控制面板 → 凭据管理器 → 普通凭据」里删除以 TokenScope:vault 开头、
                 结尾为 {id} 的那一条。"
            ));
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("删除失败: {e}"))??;
    // N16:删完立刻重抄索引,并作废取数缓存 —— 渠道卡要**马上**显示"引用的条目已删除",
    // 而不是等下一轮轮询(最长 60 秒,用户会以为没删掉)。
    let _ = rebuild_vault_refs(&app).await;
    state.fetcher.invalidate();
    Ok(VaultDeleteResult {
        deleted: true,
        referrers,
    })
}

/// 取出明文(界面点「显示」时一次性返回,8 秒后界面自己收回)。
#[tauri::command]
async fn vault_reveal(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || {
        let exists = {
            let s = lock_rw(&store);
            s.vault_get(&id)?.is_some()
        };
        if !exists {
            return Err("这条已经不在密钥箱里了".into());
        }
        match secrets::vault_get(&id)? {
            Some(k) => Ok(k),
            None => Err("索引还在但凭据丢了(可能被系统清理过),请重新录入".into()),
        }
    })
    .await
    .map_err(|e| format!("取密钥失败: {e}"))?
}

/// 复制:明文**不回前端**,后端读出来直接进剪贴板。
#[tauri::command]
async fn vault_copy(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let key = vault_reveal(state, id.clone()).await?;
    tokio::task::spawn_blocking(move || {
        copy_to_clipboard(&key).map(|_| "已复制到剪贴板".to_string())
    })
    .await
    .map_err(|e| format!("复制失败: {e}"))?
}

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
struct CheckinState {
    id: String,
    name: String,
    enabled: bool,
    auto: bool,
    /// 今天的状态短码(claimed / already / retry:ready / retry:device / auth / inactive / shape / failed)
    outcome: String,
    credits: Option<f64>,
    attempts: i64,
    last_try_at: Option<i64>,
    next_try_at: Option<i64>,
    /// 现在能不能点「领积分」
    can_try: bool,
    /// 要去用户动手(凭据失效 / 结构变了)
    needs_user: bool,
    /// 拿不到凭据的原因(界面直接显示)
    note: Option<String>,
    /// 本月自动领到的合计(只算真领到的)
    month_credits: f64,
    /// 连续签到天数(只有 Qoder 给;没有就是 None,前端不显示)
    streak_days: Option<i64>,
    /// 近 7 天流水
    log: Vec<tokenscope_core::store::CheckinRow>,
    /// Qoder 最近检测到的活动;即使当前活动不能由挂件领取,也展示监测结果。
    latest_activity: Option<checkin::QoderActivity>,
    /// 跨渠道统一的最近活动状态。
    activity_state: Option<checkin::ActivityState>,
    last_activity_check_at: Option<i64>,
    /// Qoder 返回的活动列表，用于把一个账号下的多个活动分别展示。
    activity_history: Vec<checkin::QoderActivity>,
    /// **今天这个渠道签完了吗**(claimed / already / inactive)。
    ///
    /// 为什么由后端给、不让前端算:Qoder 的周期日**从 10:00 起算**
    /// (`CheckinDef::day_roll_hour`),前端拿 `new Date()` 算本地日,10:00 之前
    /// 必然对不上 —— 明明签了也显示没签。凡是"哪一天"的判断,后端算过就别在前端再算一遍
    /// (与 0.1.49 的 `last_auth_version` 同一个思路)。
    ///
    /// 注意这与 `outcome` 不重复:`outcome` 是给"设置页"看的原始短码(含 reason),
    /// 这里是给"首页徽标"用的**已判定结论**,`already` 也算签完。
    done_today: bool,
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
fn checkin_credentials(app: &tauri::AppHandle, def: &checkin::CheckinDef) -> Result<Vec<checkin::Cred>, String> {
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
    }
}

/// 只要一份凭据的调用点(活动监测 / 状态探测):取候选表的第一份。
fn checkin_credential(app: &tauri::AppHandle, def: &checkin::CheckinDef) -> Result<checkin::Cred, String> {
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
fn cycle_day_of(id: &str, now: i64) -> String {
    let roll = checkin::find(id).map(|d| d.day_roll_hour).unwrap_or(0);
    checkin::cycle_day(now, roll)
}

/// 当前版本。用来判断一条 `auth` 结论是不是**陈旧**的 —— 见
/// [`checkin::CheckinCfg::blocks_today`] 的说明:v0.1.47 升级后当天仍显示
/// "凭据失效",就是因为旧的 `auth` 把当天挡住了,而凭据链路其实已经修好。
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 「今天还用不用再试」。三处调用点(状态展示 / 手动按钮 / 自动节拍器)必须同口径,
/// 否则会出现"界面说不能试、节拍器却在试"这种自相矛盾。
fn checkin_blocks_today(c: &checkin::CheckinCfg, today: &str) -> bool {
    c.blocks_today(today, APP_VERSION)
}

fn checkin_state_of(id: &str, cfg: &Config, store: &Store, note: Option<String>) -> CheckinState {
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
    let outcome = if c.auth_is_stale(APP_VERSION) {
        "recheck".to_string()
    } else {
        c.last_code.clone().unwrap_or_default()
    };
    let needs_user = outcome.starts_with("auth") || outcome.starts_with("shape");
    let log = store.recent_checkins(id, now - 7 * 86_400).unwrap_or_default();
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
        credits: c.last_credits,
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
        activity_state: c.activity_state,
        last_activity_check_at: c.last_activity_check_at,
        activity_history: c.activity_history,
    }
}

/// 记录 Qoder 活动变化,并把活动与签到结果分开持久化。
/// 返回值表示本次新发现了可由挂件领取的活动。
async fn record_qoder_activities(
    app: &tauri::AppHandle,
    id: &str,
    body: &serde_json::Value,
    now: i64,
) -> bool {
    let Some(mut activities) = checkin::qoder_activities(body) else {
        let period = cycle_day_of(id, now);
        record_channel_activity(
            app,
            id,
            checkin::ActivityState {
                key: format!("{}:{}:account", id, period),
                title: "Qoder 活动状态".into(),
                status: "unknown".into(),
                scope: "account".into(),
                region: if id == "qoder-cn" { "CN" } else { "Global" }.into(),
                period,
                reward: None,
                observed_at: Some(now),
                detail: Some("平台返回了无法识别的活动结构".into()),
            },
            false,
        )
        .await;
        return false;
    };
    let def = checkin::find(id);
    let name = def.map(|d| d.name).unwrap_or(id);
    let mut newly_claimable = false;
    let mut notice: Option<checkin::QoderActivity> = None;
    for activity in &mut activities {
        activity.observed_at = Some(now);
    }

    let state = app.state::<AppState>();
    let save_result = save_config_off_main(&state, |live| {
        let entry = live.checkin.entry(id.to_string()).or_default();
        entry.last_activity_check_at = Some(now);

        for activity in &activities {
            let previous = entry
                .activity_history
                .iter()
                .find(|known| known.id == activity.id);
            let is_new = previous.is_none();
            let is_claimable = checkin::qoder_action_claimable(
                &activity.action_type,
                &activity.claim_status,
            );
            let was_claimable = previous
                .map(|known| {
                    checkin::qoder_action_claimable(&known.action_type, &known.claim_status)
                })
                .unwrap_or(false);
            if is_claimable && !was_claimable {
                newly_claimable = true;
            }
            if is_new || (is_claimable && !was_claimable) {
                let prefer_this = notice
                    .as_ref()
                    .map(|known| {
                        checkin::qoder_action_claimable(&activity.action_type, &activity.claim_status)
                            && !checkin::qoder_action_claimable(
                                &known.action_type,
                                &known.claim_status,
                            )
                    })
                    .unwrap_or(true);
                if prefer_this {
                    notice = Some(activity.clone());
                }
            }

            if let Some(known) = entry
                .activity_history
                .iter_mut()
                .find(|known| known.id == activity.id)
            {
                *known = activity.clone();
            } else {
                entry.activity_history.push(activity.clone());
            }
        }
        entry
            .activity_history
            .sort_by_key(|activity| activity.observed_at.unwrap_or_default());
        if entry.activity_history.len() > 100 {
            let excess = entry.activity_history.len() - 100;
            entry.activity_history.drain(0..excess);
        }
        entry.latest_activity = activities
            .iter()
            .find(|activity| {
                checkin::qoder_action_claimable(&activity.action_type, &activity.claim_status)
            })
            .or_else(|| activities.first())
            .cloned();
        let period = cycle_day_of(id, now);
        entry.activity_state = Some(if let Some(activity) = entry.latest_activity.as_ref() {
            let status = if checkin::qoder_action_claimable(
                &activity.action_type,
                &activity.claim_status,
            ) {
                "claimable"
            } else if activity.claim_status == "CLAIMED" {
                "claimed"
            } else if activity.claim_status == "CLAIMABLE" {
                "manual"
            } else if matches!(activity.claim_status.as_str(), "EXPIRED" | "ENDED") {
                "inactive"
            } else {
                "unknown"
            };
            checkin::ActivityState {
                key: format!("{}:{}", id, activity.id),
                title: activity.title.clone(),
                status: status.to_string(),
                scope: "account".into(),
                region: if id == "qoder-cn" { "CN" } else { "Global" }.into(),
                period,
                reward: None,
                observed_at: Some(now),
                detail: Some(format!("{} · {}", activity.action_type, activity.claim_status)),
            }
        } else {
            checkin::ActivityState {
                key: format!("{}:{}:account", id, period),
                title: "本期暂无可参与活动".into(),
                status: "inactive".into(),
                scope: "account".into(),
                region: if id == "qoder-cn" { "CN" } else { "Global" }.into(),
                period,
                reward: None,
                observed_at: Some(now),
                detail: None,
            }
        });
    })
    .await;
    if let Err(e) = save_result {
        eprintln!("Qoder 活动状态落库失败: {e}");
    }
    let _ = app.emit("checkin-updated", serde_json::json!({ "id": id }));

    if let Some(activity) = notice {
        let can_claim = checkin::qoder_action_claimable(&activity.action_type, &activity.claim_status);
        let auto = {
            let state = app.state::<AppState>();
            let cfg = lock_rw(&state.config);
            cfg.checkin.get(id).map(|c| c.enabled && c.auto).unwrap_or(false)
        };
        // 自动模式下，成功回执会作为通知；不再先报“发现可领”再报“已领取”。
        if !can_claim || !auto {
            let _ = app.emit(
                "checkin-activity",
                serde_json::json!({
                    "id": id,
                    "name": name,
                    "title": activity.title,
                    "actionType": activity.action_type,
                    "claimStatus": activity.claim_status,
                    "canClaim": can_claim,
                }),
            );
        }
    }
    newly_claimable
}

/// 记录一次活动监测尝试,凭据暂不可用时也按一小时节奏重试。
async fn mark_qoder_activity_check(app: &tauri::AppHandle, id: &str, now: i64) {
    let state = app.state::<AppState>();
    if let Err(e) = save_config_off_main(&state, |live| {
        live.checkin
            .entry(id.to_string())
            .or_default()
            .last_activity_check_at = Some(now);
    })
    .await
    {
        eprintln!("Qoder 活动监测时间落库失败: {e}");
    }
}

const QODER_ACTIVITY_INTERVAL_SEC: i64 = 60 * 60;

fn activity_state_from_outcome(
    id: &str,
    outcome: &checkin::Outcome,
    body: Option<&serde_json::Value>,
    now: i64,
) -> checkin::ActivityState {
    let period = cycle_day_of(id, now);
    let scope = if id == "trae" { "device" } else { "account" };
    let trae_device_used = id == "trae"
        && body
            .and_then(|v| v.get("code"))
            .and_then(|v| v.as_i64())
            == Some(9095);
    let status = if trae_device_used {
        "device_used"
    } else {
        match outcome {
            checkin::Outcome::Claimed(_) | checkin::Outcome::Already(_) => "claimed",
            checkin::Outcome::Inactive(_) => "inactive",
            checkin::Outcome::Manual(_) => "manual",
            checkin::Outcome::Retry(reason) if reason == "ready" || reason.starts_with("ready:") => {
                "claimable"
            }
            _ => "unknown",
        }
    };
    let region = match id {
        "trae" | "workbuddy" | "qoder-cn" => "CN",
        "qoder" => "Global",
        _ => "Unknown",
    };
    let detail = match status {
        "unknown" => Some(outcome.verdict().to_string()),
        "device_used" => Some("Trae 每台设备每日共用一次名额".into()),
        _ => None,
    };
    checkin::ActivityState {
        key: format!("{}:{}:{}", id, period, scope),
        title: if id == "trae" {
            "每日设备签到".into()
        } else {
            "每日签到活动".into()
        },
        status: status.into(),
        scope: scope.into(),
        region: region.into(),
        period,
        reward: outcome.credits(),
        observed_at: Some(now),
        detail,
    }
}

/// 保存非 Qoder 渠道的统一活动状态，并只在活动首次出现/状态变化时提醒。
async fn record_channel_activity(
    app: &tauri::AppHandle,
    id: &str,
    activity: checkin::ActivityState,
    announce: bool,
) {
    let def = checkin::find(id);
    let mut should_notify = false;
    let notice_key = format!("{}:{}", activity.key, activity.status);
    let state = app.state::<AppState>();
    let save_result = save_config_off_main(&state, |live| {
        let entry = live.checkin.entry(id.to_string()).or_default();
        entry.last_activity_check_at = activity.observed_at;
        if matches!(activity.status.as_str(), "claimable" | "manual")
            && entry.last_activity_notice_key.as_deref() != Some(notice_key.as_str())
        {
            should_notify = true;
            entry.last_activity_notice_key = Some(notice_key.clone());
        }
        entry.activity_state = Some(activity.clone());
    })
    .await;
    if let Err(e) = save_result {
        eprintln!("{} 活动状态落库失败: {e}", def.map(|d| d.name).unwrap_or(id));
    }
    let _ = app.emit("checkin-updated", serde_json::json!({ "id": id }));

    if !announce || !should_notify {
        return;
    }
    let auto = {
        let state = app.state::<AppState>();
        let enabled = lock_rw(&state.config)
            .checkin
            .get(id)
            .map(|c| c.enabled && c.auto)
            .unwrap_or(false);
        enabled
    };
    // 自动领取的结果事件负责通知；避免同一活动先报发现、再报领到。
    if activity.status == "claimable" && auto {
        return;
    }
    let _ = app.emit(
        "checkin-activity",
        serde_json::json!({
            "id": id,
            "name": def.map(|d| d.name).unwrap_or(id),
            "title": activity.title,
            "status": activity.status,
            "scope": activity.scope,
            "region": activity.region,
            "canClaim": activity.status == "claimable",
        }),
    );
}

/// Trae / WorkBuddy 每小时只读查询活动状态；自动领取是独立动作。
async fn monitor_standard_activity(app: &tauri::AppHandle, id: &str) {
    if UPDATING.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let now = now_ts();
    let (enabled, hidden, last_check) = {
        let state = app.state::<AppState>();
        let cfg = lock_rw(&state.config);
        let c = cfg.checkin.get(id).cloned().unwrap_or_default();
        (
            c.enabled,
            cfg.hidden_channels.iter().any(|h| h == id),
            c.last_activity_check_at,
        )
    };
    if !enabled
        || hidden
        || last_check
            .map(|t| now.saturating_sub(t) < QODER_ACTIVITY_INTERVAL_SEC)
            .unwrap_or(false)
    {
        return;
    }
    let Ok(busy) = CheckinBusy::acquire(id) else {
        return;
    };
    mark_qoder_activity_check(app, id, now).await;

    let Some(def) = checkin::find(id) else { return };
    let h = app.clone();
    let cred = match tokio::task::spawn_blocking(move || checkin_credential(&h, def)).await {
        Ok(Ok(cred)) => cred,
        Ok(Err(_)) | Err(_) => {
            let state = activity_state_from_outcome(id, &checkin::Outcome::AuthNeeded, None, now);
            record_channel_activity(app, id, checkin::ActivityState {
                status: "unknown".into(),
                detail: Some("本机暂未读到登录凭据".into()),
                ..state
            }, false).await;
            return;
        }
    };
    let client = checkin::direct_client();
    let (out, body) = checkin::call_with_body(&client, def, &cred, false, None).await;
    let activity = activity_state_from_outcome(id, &out, body.as_ref(), now_ts());
    let can_auto_claim = out == checkin::Outcome::Retry("ready".into());
    record_channel_activity(app, id, activity, true).await;
    drop(busy);

    if can_auto_claim {
        let should_claim = {
            let state = app.state::<AppState>();
            let cfg = lock_rw(&state.config);
            cfg.checkin.get(id).map(|c| c.enabled && c.auto).unwrap_or(false)
                && !cfg.hidden_channels.iter().any(|h| h == id)
        };
        if should_claim {
            if let Err(e) = run_checkin(app, id, false).await {
                eprintln!("{} 新活动自动领取跳过: {e}", def.name);
            }
        }
    }
}

/// 独立于每日领取的活动监测。签到开关开启即可每小时检查一次,自动领取关闭时也照常发现活动。
async fn monitor_qoder_activities(app: &tauri::AppHandle, id: &str) {
    if UPDATING.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let now = now_ts();
    let (enabled, hidden, last_check) = {
        let state = app.state::<AppState>();
        let cfg = lock_rw(&state.config);
        let c = cfg.checkin.get(id).cloned().unwrap_or_default();
        (
            c.enabled,
            cfg.hidden_channels.iter().any(|h| h == id),
            c.last_activity_check_at,
        )
    };
    if !enabled || hidden || last_check.map(|t| now.saturating_sub(t) < QODER_ACTIVITY_INTERVAL_SEC).unwrap_or(false) {
        return;
    }
    let Ok(_busy) = CheckinBusy::acquire(id) else {
        return;
    };
    mark_qoder_activity_check(app, id, now).await;

    let Some(def) = checkin::find(id) else { return };
    let h = app.clone();
    let cred = match tokio::task::spawn_blocking(move || checkin_credential(&h, def)).await {
        Ok(Ok(cred)) => cred,
        _ => {
            let activity = checkin::ActivityState {
                key: format!("{}:{}:account", id, cycle_day_of(id, now)),
                title: "Qoder 活动状态".into(),
                status: "unknown".into(),
                scope: "account".into(),
                region: if id == "qoder-cn" { "CN" } else { "Global" }.into(),
                period: cycle_day_of(id, now),
                reward: None,
                observed_at: Some(now),
                detail: Some("本机暂未读到登录凭据".into()),
            };
            record_channel_activity(app, id, activity, false).await;
            return;
        }
    };
    let client = checkin::direct_client();
    let (out, body) = checkin::call_with_body(&client, def, &cred, false, None).await;
    let Some(body) = body else {
        let activity = checkin::ActivityState {
            key: format!("{}:{}:account", id, cycle_day_of(id, now)),
            title: "Qoder 活动状态".into(),
            status: "unknown".into(),
            scope: "account".into(),
            region: if id == "qoder-cn" { "CN" } else { "Global" }.into(),
            period: cycle_day_of(id, now),
            reward: None,
            observed_at: Some(now),
            detail: Some(out.verdict().into()),
        };
        record_channel_activity(app, id, activity, false).await;
        return;
    };
    let newly_claimable = record_qoder_activities(app, id, &body, now_ts()).await;
    let has_claim_id = checkin::qoder_pick(&body).claim_id.is_some();
    drop(_busy);

    if newly_claimable && has_claim_id {
        // 活动可能在每日首次签到之后才开放,发现新可领项时立刻复用同一领取链路。
        // 再读一次实时开关,避免用户刚关掉自动领取时仍发出 POST。
        let auto_enabled_now = {
            let state = app.state::<AppState>();
            let cfg = lock_rw(&state.config);
            cfg.checkin.get(id).map(|c| c.enabled && c.auto).unwrap_or(false)
                && !cfg.hidden_channels.iter().any(|h| h == id)
        };
        if auto_enabled_now {
            if let Err(e) = run_checkin(app, id, false).await {
                eprintln!("Qoder 新活动自动领取跳过: {e}");
            }
        }
    }
}

/// 跑一次签到:查状态 → 可领就领 → 领完回查一次 → 触发一次取数做到账核对。
/// 手动点与自动节拍器共用这一条路。
async fn run_checkin(app: &tauri::AppHandle, id: &str, manual: bool) -> Result<checkin::Outcome, String> {
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
    if let checkin::Outcome::Retry(m) = &out {
        if m == "ready" || m.starts_with("ready:") {
            let claim_id = checkin::ready_claim_id(m).filter(|s| !s.is_empty());
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

    // 到账核对:立刻取一轮(新包会出现在余额里),失败不影响签到结论
    if matches!(out, checkin::Outcome::Claimed(_)) {
        refresh_now(app).await;
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
                activity.detail = Some("平台已收到领取请求，正在确认状态".into());
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
            checkin::Outcome::Confirming(_) => Some("平台已收到领取请求，正在确认状态".into()),
            _ => None,
        };

        // 需要用户动手(凭据失效 / 接口结构变了)时要留下看得见的提醒:绿色版从托盘
        // 启动时 eprintln 永远没人看到(报告 P2-14 / P3-36)。系统通知要额外引插件,
        // 先走面板事件,由设置里的 checkin_notify 管住。
        if out.needs_user() && notify {
            let _ = app.emit(
                "checkin-alert",
                serde_json::json!({
                    "id": id,
                    "name": def.name,
                    "message": entry.last_message.clone().unwrap_or_default(),
                }),
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
        // 再**只**把 checkin[id] 合并进当前配置。读的是 live 配置而不是开头那份快照,
        // 所以用户在签到期间改的其它设置原样保留 —— 这条是 N04 的核心。
        let merged = {
            let mut live = lock_rw(&state.config);
            live.checkin.insert(id.to_string(), entry.clone());
            live.clone()
        };
        {
            let store = lock_rw(&state.store);
            if let Err(e) = merged.save(&store) {
                eprintln!("签到配置落库失败: {e}");
            }
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
    }
    let _ = app.emit("checkin-updated", serde_json::json!({ "id": id }));
    Ok(out)
}

/// 拿不到凭据时也要**留一条流水**。
///
/// 以前这条路径只 `eprintln` 一句就返回,于是"今天一次都没试"在 `checkin_log` 里
/// 是一片空白 —— 与"还没到点"完全无法区分。2026-10-20 排查 WorkBuddy 时就撞上了:
/// 10-16 / 10-17 两天没有任何记录,只能靠推断才知道那两天是凭据读不到。
///
/// 结果码用 `no_cred` 前缀(不是 `auth`):**"凭据读不到"与"平台说凭据失效"是两件事** ——
/// 前者是本机没装/没登录/解不开,后者是服务端拒绝了这份凭据。混成一个会让用户
/// 照着错误的提示去操作。
///
/// 这条**不算当天完结**(`done_for_day` 不认 `no_cred`),所以白天补装/登录后
/// 节拍器还会再试 —— 与 `retry` 同口径。
fn record_checkin_skip(app: &tauri::AppHandle, id: &str, day: &str, now: i64, why: &str) {
    let state = app.state::<AppState>();
    let mut cfg = lock_rw(&state.config).clone();
    let entry = cfg.checkin.entry(id.to_string()).or_default();
    let same_day = entry.last_day.as_deref() == Some(day);
    entry.attempts = if same_day { entry.attempts + 1 } else { 1 };
    entry.last_day = Some(day.to_string());
    entry.last_code = Some(format!("no_cred:{why}"));
    entry.last_try_at = Some(now);
    entry.last_message = Some(why.to_string());
    // 排一次退避:凭据问题不会在 2 分钟内自愈,别每 5 分钟硬撞一次
    entry.next_try_at = Some(now + checkin::next_delay(entry.attempts));

    let store = lock_rw(&state.store);
    if let Err(e) = store.record_checkin(id, now, day, &format!("no_cred:{why}"), None, entry.attempts, why)
    {
        eprintln!("{e}");
    }
    if let Err(e) = cfg.save(&store) {
        eprintln!("签到配置落库失败: {e}");
    }
    drop(store);
    *lock_rw(&state.config) = cfg;
    let _ = app.emit("checkin-updated", serde_json::json!({ "id": id }));
}

#[tauri::command]
async fn checkin_state(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Vec<CheckinState>, String> {
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
    for (i, def) in checkin::CHECKINS.iter().enumerate() {
        let item = &mut list[i];
        if !item.enabled {
            continue;
        }
        // 隐藏中的渠道连探测都不能做:「隐藏 = 不读本机登录凭据」这条红线对取数、
        // 签到、以及这个"只为填 note"的读取同样成立(报告 P0-6)。
        if cfg.hidden_channels.iter().any(|h| h == def.id) {
            continue;
        }
        let o = item.outcome.as_str();
        if o.starts_with("claimed") || o.starts_with("already") || o.starts_with("inactive") {
            continue;
        }
        let a2 = app.clone();
        let err = tokio::task::spawn_blocking(move || checkin_credential(&a2, def).err())
            .await
            .unwrap_or(None);
        if let Some(msg) = err {
            item.note = Some(msg);
        }
    }
    Ok(list)
}

/// 手动领一次(渠道卡上的「领积分」)。
#[tauri::command]
async fn checkin_now(app: tauri::AppHandle, id: String) -> Result<CheckinState, String> {
    // 「现在到底能不能点」后端早就算好了(CheckinState.can_try),前端以前不读它,
    // 于是退避中 / 今天已签的情况下连点 N 次就发 N×3 个请求(报告 P0-6)。
    {
        let st = app.state::<AppState>();
        let cfg = lock_rw(&st.config).clone();
        // 周期日(不是自然日):Qoder 的"今天"从 10:00 起算
        let today = cycle_day_of(&id, now_ts());
        if let Some(c) = cfg.checkin.get(&id) {
            let activity_ready = c
                .activity_state
                .as_ref()
                .map(|a| a.status == "claimable")
                .unwrap_or(false);
            if checkin_blocks_today(c, &today) && !activity_ready {
                return Err(format!(
                    "今天已经处理过了({})",
                    c.last_code.clone().unwrap_or_else(|| "已领".to_string())
                ));
            }
            if let Some(t) = c.next_try_at {
                let left = (t - now_ts()).max(0);
                if left > 0 {
                    return Err(if left >= 60 {
                        format!("太挤了,约 {} 分钟后再试", left / 60)
                    } else {
                        format!("太挤了,约 {} 秒后再试", left)
                    });
                }
            }
        }
    }
    let out = run_checkin(&app, &id, true).await?;
    let state = app.state::<AppState>();
    let cfg = lock_rw(&state.config).clone();
    let store = lock_rw(&state.store);
    let _ = out;
    Ok(checkin_state_of(&id, &cfg, &store, None))
}

/// 每日节拍器:每 5 分钟看一次"今天该不该签"。
/// **不塞进 poll_loop 的 60 秒里** —— 签到是写操作,节奏要跟取数分开(设计稿第 8 节)。
///
/// **先跑一轮再睡**(2026-10-20 修):以前 `sleep(300)` 在循环最前面,启动后要干等
/// 整整 5 分钟才判第一次。而界面在这段时间挂的是**上次运行留下的结论** ——
/// 真机现场:应用 23:38 启动,23:43 才重试成功;中间那 5 分钟卡片一直显示
/// 早已过期的"凭据失效",用户被「去登录」引到死胡同(网页登录还被网关拒)。
fn spawn_checkin_ticker(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            checkin_tick_once(&app).await;
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
        }
    });
}

/// 走一轮:对每个"今天还没处理完"的渠道尝试签到。
async fn checkin_tick_once(app: &tauri::AppHandle) {
    if UPDATING.load(std::sync::atomic::Ordering::Relaxed) {
        return; // 更新期间不签到(报告 P1-15)
    }
    for def in checkin::CHECKINS {
        if def.auth == checkin::CheckinAuth::QoderApp {
            monitor_qoder_activities(app, def.id).await;
        } else {
            monitor_standard_activity(app, def.id).await;
        }
    }
    let (cfg, now) = {
        let state = app.state::<AppState>();
        let cfg = lock_rw(&state.config).clone();
        (cfg, now_ts())
    };
    for def in checkin::CHECKINS {
        let Some(c) = cfg.checkin.get(def.id) else { continue };
        if !c.enabled {
            continue; // 总开关关闭 = 不领取也不监测
        }
        if cfg.hidden_channels.iter().any(|h| h == def.id) {
            continue; // 隐藏 = 不读本机登录凭据、也不对外发请求(与取数同口径)
        }
        if !c.auto {
            continue; // 自动领取关闭;各渠道的只读活动监测已在上方独立执行
        }
        // **逐渠道算周期日**:Qoder 的"今天"从 10:00 起算,不是自然日。
        // 用自然日会让 09:59 与 10:01 混成一天,当天那一期永远领不到。
        let today = checkin::cycle_day(now, def.day_roll_hour);
        let activity_ready = c
            .activity_state
            .as_ref()
            .map(|a| a.status == "claimable")
            .unwrap_or(false);
        if checkin_blocks_today(c, &today) && !activity_ready {
            continue;
        }
        if c.next_try_at.map(|t| t > now).unwrap_or(false) {
            continue; // 退避中
        }
        // 跨日时刻 + 该渠道抖动之后才允许(当天更晚也算 → 开机补跑)
        if !checkin::due_now(now, &today, def.id, def.day_roll_hour) {
            continue;
        }
        if let Err(e) = run_checkin(app, def.id, false).await {
            // 拿不到凭据:记一句就走,别每 5 分钟刷屏。
            // **但也要落一条流水** —— 以前这里只 eprintln,绿色版从托盘启动时
            // 那行没人看得到,于是"没试"与"试了失败"在数据里无法区分
            // (2026-10-20 排查:workbuddy 10-16/10-17 两天在 checkin_log 里是空白)。
            eprintln!("签到 {} 跳过: {e}", def.name);
            record_checkin_skip(app, def.id, &today, now, &e);
        }
        // 两渠道之间串行 + 间隔(设计稿第 8 节)
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    }
}

// ─────────────── 4SAPI 可用模型 + 一键配置到客户端 ───────────────
//
// 设计与取证见 `docs/design/design-4sapi-models.html`。三条纪律:
//   1. 模型清单是**元数据接口**,不产生 token 计费(与余额取数同一条原则);
//   2. 写进客户端一律「先备份 → 写 → 读回校验」,目标在跑就拒写(规矩在 core::inject 里);
//   3. 密钥只从凭据管理器**现读现写**,不回前端、不落盘、不进日志。

/// 来源芯片要的信息。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceInfo {
    id: String,
    name: String,
    has_price: bool,
    list_public: bool,
}

fn source_infos() -> Vec<SourceInfo> {
    models::SOURCES
        .iter()
        .map(|d| SourceInfo {
            id: d.id.to_string(),
            name: d.name.to_string(),
            has_price: d.pricing_path.is_some(),
            list_public: d.list_public,
        })
        .collect()
}

/// 模型页要的整份视图。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogView {
    /// 来源 id("4sapi" / "hapi" / "opencode-go")
    source: String,
    /// 来源的界面名
    source_name: String,
    /// 全部来源(界面上的芯片;没取到的来源也要列出来,标「—」)
    sources: Vec<SourceInfo>,
    /// 这个来源有没有价格(只有 4SAPI 有)。false → 界面不显示单价、也不按价排序
    has_price: bool,
    /// 列表是否公开(Go 是:没配密钥也能列出模型)
    list_public: bool,
    /// 价目表取数的域名
    host: String,
    /// 要写进客户端的接入地址(挂件实测可用的那个)
    base_url: String,
    version: String,
    fetched_at: i64,
    /// 可用集合的取回时间;0 = 从没取到过(界面据此把「可用」退成「未知」)
    usable_at: i64,
    has_key: bool,
    group: String,
    groups: Vec<models::Group>,
    total: usize,
    chat_count: usize,
    usable_count: usize,
    rows: Vec<models::RowView>,
    /// 刷新失败时的真实原因(界面显示上次快照并用它说明)
    error: Option<String>,
}

/// 客户端状态 + 写入台账。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct InjectOverview {
    clients: Vec<inject::ClientState>,
    log: Vec<inject::InjectRecord>,
}

/// 写进客户端的接入地址:用户指定 > 实测可用的域名 > 主域名。
/// 写进客户端的接入地址。
/// 4SAPI:用户指定 > 实测可用的域名 > 主域名(它有几个域名,得挑一个真通的);
/// 其它来源:用来源定义里的固定地址。
fn source_base(cfg: &Config, cache: Option<&models::ModelCache>, def: &models::SourceDef) -> String {
    if def.id != "4sapi" {
        return def.base_url.to_string();
    }
    if let Some(u) = cfg.model_base_url.as_deref() {
        let t = u.trim().trim_end_matches('/');
        if !t.is_empty() {
            return t.to_string();
        }
    }
    match cache {
        Some(c) => c.base_url(),
        None => def.base_url.to_string(),
    }
}

/// 该来源的密钥。Go 的列表公开,但写进客户端必须带密钥。
fn source_key(id: &str) -> Option<String> {
    secrets::channel_key(id)
        .ok()
        .flatten()
        .filter(|k| !k.trim().is_empty())
}


/// 入口顺序:配置里指定的域名优先,其次上次成功的(可用集合 > 价目表),最后默认顺序。
fn host_order(cfg: &Config, cache: Option<&models::ModelCache>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |h: String| {
        if !h.is_empty() && !out.iter().any(|x| x == &h) {
            out.push(h);
        }
    };
    if let Some(base) = cfg.model_base_url.as_deref() {
        if let Some(h) = providers::url_host(base) {
            push(h);
        }
    }
    if let Some(c) = cache {
        if c.usable_at > 0 {
            push(c.usable_host.clone());
        }
        push(c.host.clone());
    }
    for h in providers::four_s_api_hosts() {
        push(h);
    }
    out
}

/// 一个模型写进客户端时的全部信息。上下文/多模态从价目表缓存里取;
/// 缓存里没有就留空 —— 不编造能力上限。
///
/// **这里是阻塞活**(凭据管理器 / 读缓存),调用方负责把它放进 `spawn_blocking` ——
/// 见 `inject_preview` 上面那段注释。
fn model_spec_with(
    store: &Arc<Mutex<Store>>,
    cfg: &Config,
    source: &str,
    model: &str,
) -> Result<inject::ModelSpec, String> {
    let model = model.trim();
    if model.is_empty() {
        return Err("没有指定模型".into());
    }
    let def = models::find_source(source).ok_or_else(|| format!("未知模型来源: {source}"))?;
    let key = match source_key(def.id) {
        Some(k) => k,
        None => {
            return Err(format!(
                "还没配置 {} 密钥:先在「渠道」页签里填一次",
                def.name
            ))
        }
    };
    let cache = {
        let s = lock_rw(store);
        models::ModelCache::load(&s)
    };
    let row = cache
        .as_ref()
        .and_then(|c| c.catalog().ok())
        .and_then(|cat| cat.find(model).cloned());
    let context = row
        .as_ref()
        .and_then(|r| r.context.as_deref())
        .and_then(models::context_tokens);
    let multimodal = row.as_ref().map(|r| r.multimodal).unwrap_or(false);
    let base = source_base(cfg, cache.as_ref(), def);
    Ok(inject::ModelSpec::new(
        def.id,
        model,
        &base,
        &key,
        context,
        multimodal,
    )
    .with_api_protocol(models::api_protocol(def.id, model)))
}

fn inject_opts(cfg: &Config, client: inject::Client) -> inject::Opts {
    inject::Opts {
        // 学到的口径才写选中项;没学到就只写条目(core::inject::learn_prefix)
        selection_prefix: cfg.selection_prefix.get(client.id()).cloned(),
        trae_auto_select: cfg.trae_auto_select,
    }
}

/// 取 4SAPI 的可用模型:公开价目表 + 这把 key 能调的集合。
/// 两者都是元数据,不产生 token 计费;缓存一天,失败时退回上次快照并如实报错。
#[tauri::command]
async fn get_model_catalog(
    state: State<'_, AppState>,
    source: Option<String>,
    force: bool,
) -> Result<CatalogView, String> {
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    let client = state.fetcher.http.clone();
    let now = now_ts();
    let want = source.unwrap_or_default();
    let def = models::find_source(if want.is_empty() { "4sapi" } else { &want })
        .ok_or_else(|| format!("未知模型来源: {want}"))?;

    let mut cache = {
        let s = lock_rw(&store);
        models::ModelCache::load(&s)
    };
    let mut error: Option<String> = None;

    // ── 只有 id 列表的来源(Hapi / OpenCode Go):列表给什么就是什么,没有价、没有标签 ──
    if def.pricing_path.is_none() {
        // 凭据管理器读是 RPC,别占着 tokio worker(其余调用点都在 spawn_blocking 里)
        let sid = def.id;
        let key = tokio::task::spawn_blocking(move || source_key(sid))
            .await
            .map_err(|e| format!("读取密钥失败: {e}"))?;
        let need = force
            || cache
                .as_ref()
                .and_then(|c| c.sources.get(def.id))
                .map(|sc| sc.stale(now) || sc.list_path != def.list_path)
                .unwrap_or(true);
        if need {
            match models::fetch_source_ids(&client, def, key.as_deref()).await {
                Ok((ids, host)) => {
                    let c = cache.get_or_insert_with(Default::default);
                    let sc = c.sources.entry(def.id.to_string()).or_default();
                    sc.ids = ids.clone();
                    sc.fetched_at = now;
                    sc.host = host;
                    sc.list_path = def.list_path.to_string();
                    // 对这个来源而言,"能用"就是列表本身(公开列表 = 它提供这些模型)
                    sc.usable = ids;
                    sc.usable_at = now;
                }
                Err(e) => error = Some(e),
            }
        }
        if let Some(c) = cache.as_ref() {
            let s = lock_rw(&store);
            if let Err(e) = c.save(&s) {
                eprintln!("模型缓存写入失败: {e}");
            }
        }
        let sc = cache.as_ref().and_then(|c| c.sources.get(def.id));
        let (ids, usable, usable_at) = match sc {
            Some(sc) => (sc.ids.clone(), sc.usable.clone(), sc.usable_at),
            None => (Vec::new(), Vec::new(), 0),
        };
        let rows = models::rows_from_source_ids(def.id, &ids, &usable, usable_at > 0);
        return Ok(CatalogView {
            source: def.id.to_string(),
            source_name: def.name.to_string(),
            sources: source_infos(),
            has_price: false,
            list_public: def.list_public,
            host: sc.map(|s| s.host.clone()).unwrap_or_default(),
            base_url: def.base_url.to_string(),
            version: String::new(),
            fetched_at: sc.map(|s| s.fetched_at).unwrap_or(0),
            usable_at,
            has_key: key.is_some(),
            group: String::new(),
            groups: Vec::new(),
            total: rows.len(),
            chat_count: rows.len(),
            usable_count: rows.iter().filter(|r| r.usable == Some(true)).count(),
            rows: models::row_views_of(rows, &[], ""),
            error,
        });
    }

    // ── 4SAPI:价目表 + 可用集合(它有四个域名,挑真通的)──
    let hosts = host_order(&cfg, cache.as_ref());

    let needs_catalog = force || cache.as_ref().map(|c| c.stale(now)).unwrap_or(true);
    if needs_catalog {
        match models::fetch_catalog(&client, &hosts).await {
            Ok(f) => {
                let c = cache.get_or_insert_with(Default::default);
                c.version = f.catalog.version.clone();
                c.fetched_at = now;
                c.pricing = f.raw.to_string();
                c.host = f.host;
            }
            // 失败不把价目表清空:显示上次快照 + 真实原因(与余额取数同一契约)
            Err(e) => error = Some(e),
        }
    }

    // 凭据管理器读是 RPC,下沉到阻塞任务(模型页虽然是人点的,但一次要串好几个请求)
    let key = tokio::task::spawn_blocking(|| secrets::channel_key("4sapi"))
        .await
        .map_err(|e| format!("读取 4SAPI 密钥失败: {e}"))?
        .ok()
        .flatten()
        .filter(|k| !k.trim().is_empty());
    let needs_usable = cache
        .as_ref()
        .map(|c| force || c.usable_at <= 0 || now - c.usable_at > models::CACHE_TTL_SEC)
        .unwrap_or(true);
    if let (Some(k), true) = (key.as_ref(), needs_usable) {
        match models::fetch_usable(&client, &hosts, k).await {
            Ok((ids, host)) => {
                if let Some(c) = cache.as_mut() {
                    c.usable = ids;
                    c.usable_at = now;
                    c.usable_host = host;
                }
            }
            Err(e) => {
                if error.is_none() {
                    error = Some(e);
                }
            }
        }
    }

    if let Some(c) = cache.as_ref() {
        let s = lock_rw(&store);
        if let Err(e) = c.save(&s) {
            eprintln!("模型缓存写入失败: {e}");
        }
    }

    let Some(cache) = cache else {
        return Ok(CatalogView {
            source: def.id.to_string(),
            source_name: def.name.to_string(),
            sources: source_infos(),
            has_price: true,
            list_public: def.list_public,
            host: String::new(),
            base_url: source_base(&cfg, None, def),
            version: String::new(),
            fetched_at: 0,
            usable_at: 0,
            has_key: key.is_some(),
            group: String::new(),
            groups: Vec::new(),
            total: 0,
            chat_count: 0,
            usable_count: 0,
            rows: Vec::new(),
            error: error.or_else(|| Some("还没有价目表缓存".into())),
        });
    };

    let mut catalog = cache.catalog()?;
    if cache.usable_known() {
        // 只有**成功取到过**才打「不可用」标记:一次网络失败不该让 160 个模型全变红
        catalog.mark_usable(&cache.usable);
    }
    let group = cfg
        .model_group
        .clone()
        .filter(|g| !g.trim().is_empty())
        .unwrap_or_else(|| catalog.default_group());
    let rows = models::row_views(&catalog, &group);
    Ok(CatalogView {
        source: def.id.to_string(),
        source_name: def.name.to_string(),
        sources: source_infos(),
        has_price: true,
        list_public: def.list_public,
        host: cache.host.clone(),
        base_url: source_base(&cfg, Some(&cache), def),
        version: cache.version.clone(),
        fetched_at: cache.fetched_at,
        usable_at: cache.usable_at,
        has_key: key.is_some(),
        group,
        groups: catalog.groups.clone(),
        total: catalog.rows.len(),
        chat_count: catalog.chat_count(),
        usable_count: catalog.rows.iter().filter(|r| r.usable == Some(true)).count(),
        rows,
        error,
    })
}

/// 写入预告(纯读,绝不写盘)。
///
/// `async` + `spawn_blocking` 不是风格问题:**非 async 的 Tauri 命令跑在主线程上**,
/// 而这条路上要起 `tasklist` 探进程、读客户端配置、读凭据管理器 —— 全是阻塞活。
/// 真机实测过一次卡住:整个挂件不再响应任何命令(WebView 的 IPC 一起排队),
/// 连 WorkBuddy 渠道的会话读取都跟着超时。项目里早有同一条教训(取数路径的
/// 文件 IO / keyring 都下沉 spawn_blocking),这里照同一套办。
/// 界面上「当前来源」必须由前端一起传进来。
/// 以前缺省静默落到 "4sapi":用户在 Hapi / OpenCode Go 的模型上操作,拿到的却是
/// 4SAPI 的接入地址 + 4SAPI 的密钥(报告 P0-5)。缺参数就该失败,不该替用户猜。
fn need_source(source: Option<String>) -> Result<String, String> {
    let s = source.unwrap_or_default().trim().to_string();
    if s.is_empty() {
        return Err("没指定模型来源:请在模型行的来源芯片上操作".to_string());
    }
    Ok(s)
}

#[tauri::command]
async fn inject_preview(
    state: State<'_, AppState>,
    client: String,
    source: Option<String>,
    model: String,
) -> Result<Vec<inject::Preview>, String> {
    let c = inject::Client::parse(&client).ok_or_else(|| format!("未知客户端: {client}"))?;
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    tokio::task::spawn_blocking(move || {
        let src = need_source(source.clone())?;
        let spec = model_spec_with(&store, &cfg, &src, &model)?;
        Ok(inject::preview(c, &spec, &inject_opts(&cfg, c)))
    })
    .await
    .map_err(|e| format!("写入预告失败: {e}"))?
}

/// 执行写入。规矩(备份/校验/拒写运行中的目标)全在 core::inject 里。
#[tauri::command]
async fn inject_apply(
    state: State<'_, AppState>,
    client: String,
    source: Option<String>,
    model: String,
) -> Result<Vec<inject::Report>, String> {
    let c = inject::Client::parse(&client).ok_or_else(|| format!("未知客户端: {client}"))?;
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    tokio::task::spawn_blocking(move || {
        let src = need_source(source.clone())?;
        let spec = model_spec_with(&store, &cfg, &src, &model)?;
        refuse_while_updating("写入客户端配置")?;
        let _busy = FlagGuard(&INJECTING);
        INJECTING.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut reports = inject::apply(c, &spec, &inject_opts(&cfg, c))?;
        // 记账:只记「写到哪、备份在哪、写了几条」——密钥不在其中
        {
            let s = lock_rw(&store);
            for r in &mut reports {
                // 没写成、或本来就没动盘:没有可回滚的东西,别往台账塞一条空备份路径
                // (那正是"历史里能看到、点回滚却报备份目录不存在"的来源)
                if r.error.is_some() || r.backup_path.is_empty() {
                    continue;
                }
                if let Err(e) = inject::push_log(
                    &s,
                    inject::InjectRecord {
                        // 读出来时会按目录是否还在重算,这里只需给个"刚写完当然在"的初值
                        backup_ok: true,
                        stamp: r.stamp.clone(),
                        at: now_ts(),
                        client: r.client.clone(),
                        label: r.label.clone(),
                        model: spec.id.clone(),
                        backup: r.backup.clone(),
                        backup_path: r.backup_path.clone(),
                        added: r.added,
                        updated: r.updated,
                        removed: r.removed,
                        selected: r.selected,
                    },
                    ) {
                        // 台账写不进去 = 这次以后在界面上回滚不了,必须说出来(P2-36)
                        r.notes.push(format!(
                            "台账没写进去:{e} —— 这次写入以后可能无法从界面回滚,备份目录仍在 {b}",
                            b = r.backup
                        ));
                    }
                }
            }
        Ok(reports)
    })
    .await
    .map_err(|e| format!("写入失败: {e}"))?
}

/// 取消写入:把这条模型从客户端里删掉(写入的逆操作)。
/// 规矩完全一样:目标没运行、先备份、只动我们写的条目、写完读回校验,不过就回滚。
#[tauri::command]
async fn inject_remove(
    state: State<'_, AppState>,
    client: String,
    source: Option<String>,
    model: String,
) -> Result<Vec<inject::Report>, String> {
    let c = inject::Client::parse(&client).ok_or_else(|| format!("未知客户端: {client}"))?;
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    tokio::task::spawn_blocking(move || {
        // 取消写入也要一个 spec:按 base_url / 名字认出"这条是不是我们写的"
        let src = need_source(source.clone())?;
        let spec = model_spec_with(&store, &cfg, &src, &model)?;
        refuse_while_updating("取消写入")?;
        let _busy = FlagGuard(&INJECTING);
        INJECTING.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut reports = inject::remove(c, &spec)?;
        {
            let s = lock_rw(&store);
            for r in &mut reports {
                if r.error.is_some() || r.backup_path.is_empty() {
                    continue;
                }
                if let Err(e) = inject::push_log(
                    &s,
                    inject::InjectRecord {
                        // 读出来时会按目录是否还在重算,这里只需给个"刚写完当然在"的初值
                        backup_ok: true,
                        stamp: r.stamp.clone(),
                        at: now_ts(),
                        client: r.client.clone(),
                        label: r.label.clone(),
                        model: spec.id.clone(),
                        backup: r.backup.clone(),
                        backup_path: r.backup_path.clone(),
                        added: 0,
                        updated: 0,
                        removed: r.removed,
                        selected: false,
                    },
                    ) {
                        r.notes.push(format!(
                            "台账没写进去:{e} —— 这次删除以后可能无法从界面回滚,备份目录仍在 {b}",
                            b = r.backup
                        ));
                    }
                }
            }
        Ok(reports)
    })
    .await
    .map_err(|e| format!("取消写入失败: {e}"))?
}

/// 四客户端状态 + 已写入的模型 + 台账。顺手把「选中项前缀口径」学下来并持久化:
/// 那是从客户端自己的文件里读出来的证据,不猜(见 core::inject::learn_prefix)。
#[tauri::command]
async fn inject_state(state: State<'_, AppState>) -> Result<InjectOverview, String> {
    refuse_if_config_broken()?;
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    let scanned = tokio::task::spawn_blocking(move || {
        let list = inject::state(&cfg.selection_prefix);
        let mut learned = cfg.selection_prefix.clone();
        let mut dirty = false;
        for st in &list {
            if let Some(p) = &st.learned_prefix {
                if learned.get(&st.client) != Some(p) {
                    learned.insert(st.client.clone(), p.clone());
                    dirty = true;
                }
            }
        }
        let log = {
            let s = lock_rw(&store);
            inject::load_log(&s)
        };
        (list, log, learned, dirty)
    })
    .await;
    let (clients, log, learned, dirty) = scanned.map_err(|e| {
        // 以前这里返回 Ok(空表):模型页显示成"这台机器没装 Trae / WorkBuddy / ZCode",
        // 用户去重装客户端甚至重装挂件,而真实原因(探测任务坏了)一点出口都没有(P2-35)
        format!("检测客户端状态失败:{e} —— 这是挂件的探测环节出问题,不代表你没装客户端")
    })?;
    if dirty {
        // 学到的口径写回配置。两把锁分开拿、各拿各的(不嵌套,避免与 set_config 互等)。
        let updated = {
            let mut c = lock_rw(&state.config);
            c.selection_prefix = learned;
            c.clone()
        };
        let store2 = state.store.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let s = lock_rw(&store2);
            updated.save(&s)
        })
        .await;
    }
    Ok(InjectOverview { clients, log })
}

/// 回滚一次写入:把备份目录里的文件放回原处。目标在跑就拒回滚(同写入的规矩)。
#[tauri::command]
async fn inject_rollback(state: State<'_, AppState>, backup: String) -> Result<String, String> {
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || {
        let path = std::path::PathBuf::from(backup.trim());
        if !path.is_dir() {
            return Err(format!("备份目录不存在: {}", path.display()));
        }
        let log = {
            let s = lock_rw(&store);
            inject::load_log(&s)
        };
        // 归属由"这条备份落在哪个客户端配置目录的 backup-inject 下"决定,而不是只查台账:
        // 台账只留 20 条、以前 Trae 的记录备份路径干脆是空 —— 查不到时以前**直接跳过**
        // 运行检查(fail-open),等于给前端一个"把任意目录里的文件拷到别处"的原语(P2-36)。
        //
        // 旧实现是"字符串前缀 + 不解析 ..":`…\backup-inject\..\..\..\Temp\evil` 与
        // `…\backup-injectXYZ` 都能通过。现在:拒 `..`、要求父目录真叫 backup-inject、
        // 两侧归一成真实路径再比。
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err("备份路径里不允许出现 ..".to_string());
        }
        let in_backup = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.eq_ignore_ascii_case("backup-inject"))
            .unwrap_or(false);
        if !in_backup {
            return Err(
                "这条备份不在任何客户端的 backup-inject 目录下,来历不明,挂件不执行回滚"
                    .to_string(),
            );
        }
        let canon = std::fs::canonicalize(&path).map_err(|e| format!("备份路径无效: {e}"))?;
        let owner = inject::sites().into_iter().find(|site| {
            std::fs::canonicalize(site.root.join("backup-inject"))
                .map(|b| canon.starts_with(&b))
                .unwrap_or(false)
        });
        let Some(site) = owner else {
            return Err(
                "这条备份不在任何客户端的 backup-inject 目录下,来历不明,挂件不执行回滚"
                    .to_string(),
            );
        };
        if site.running && site.client.blocks_when_running() {
            return Err(format!(
                "{} 正在运行,先退出它再回滚(否则回滚会被它的内存态覆盖)",
                site.label
            ));
        }
        let _ = &log;
        inject::restore(&path)
    })
    .await
    .map_err(|e| format!("回滚失败: {e}"))?
}

/// 「复制三件套」。密钥由后端拼进字符串,**不回前端**。
/// 注意:写入剪贴板对其他程序(含剪贴板历史)可见 —— 界面必须写明这一点。
#[tauri::command]
async fn copy_model_bundle(
    state: State<'_, AppState>,
    source: Option<String>,
    model: String,
) -> Result<String, String> {
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    tokio::task::spawn_blocking(move || {
        let src = need_source(source.clone())?;
        let spec = model_spec_with(&store, &cfg, &src, &model)?;
        let protocol = spec
            .api_protocol
            .ok_or_else(|| format!("官网未公布 {} 的 API 协议，不能生成可靠的接入配置", spec.id))?;
        let endpoint = spec
            .endpoint_url()
            .ok_or_else(|| format!("{} 没有可用的完整端点", spec.id))?;
        let text = format!(
            "接口协议:{}\n接入地址:{}\n完整端点:{}\n模型名称:{}\nAPI Key:{}",
            protocol.label(), spec.base_url, endpoint, spec.id, spec.key
        );
        copy_to_clipboard(&text)?;
        Ok(format!("已复制:{} + 模型名 + 密钥", spec.base_url))
    })
    .await
    .map_err(|e| format!("复制失败: {e}"))?
}

/// 模型基本信息:手工在客户端里新建自定义模型时该填的那几个值。
///
/// 为什么要有这条:自动写入有时客户端不认(真机:Trae 的日志把条目收进了 custom_models,
/// 界面列表却没出现),那时候用户得自己在这个客户端的"自定义模型"里手填一遍。
/// 所以他得能看见这些值 —— 密钥在这里**只给后四位**,明文走 key_reveal 那条专用路。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelInfoView {
    /// 模型标识(OpenAI 兼容接口的 model 参数;Trae 里也是它)
    id: String,
    /// 客户端里的显示名口径(`4S-<id>` / `HP-<id>` / `OG <目录名>`;按来源不同)
    display_name: String,
    source: String,
    source_name: String,
    /// 当前模型实际使用的协议。
    api_type: String,
    /// 该模型应填写的 base_url;OpenCode Go 按模型协议分别设置。
    base_url: String,
    /// 完整请求端点。
    endpoint: String,
    context_tokens: Option<i64>,
    multimodal: bool,
    has_key: bool,
    /// 密钥后四位(取不到就是 None)
    key_hint: Option<String>,
}

#[tauri::command]
async fn model_info(
    state: State<'_, AppState>,
    source: Option<String>,
    model: String,
) -> Result<ModelInfoView, String> {
    let store = state.store.clone();
    let cfg = lock_rw(&state.config).clone();
    tokio::task::spawn_blocking(move || {
        let src = need_source(source)?;
        let def = models::find_source(&src).ok_or_else(|| format!("未知模型来源: {src}"))?;
        let model = model.trim().to_string();
        if model.is_empty() {
            return Err("没有指定模型".into());
        }
        // 与写入用同一份清单:上下文/多模态从价目表来,未知就不编(不要求必须配了密钥 ——
        // 没配密钥的来源也要能把地址和模型名给人看)
        let cache = {
            let s = lock_rw(&store);
            models::ModelCache::load(&s)
        };
        let row = cache
            .as_ref()
            .and_then(|c| c.catalog().ok())
            .and_then(|cat| cat.find(&model).cloned());
        let protocol = models::api_protocol(def.id, &model);
        let base = match protocol {
            Some(p) if def.id == "opencode-go" => p.base_url().to_string(),
            Some(_) => source_base(&cfg, cache.as_ref(), def),
            None => String::new(),
        };
        let key = source_key(def.id);
        Ok(ModelInfoView {
            id: model.clone(),
            display_name: inject::display_name_of(def.id, &model),
            source: def.id.to_string(),
            source_name: def.name.to_string(),
            api_type: protocol
                .map(models::ApiProtocol::label)
                .unwrap_or("官网未标注协议，暂停自动接入")
                .to_string(),
            endpoint: protocol
                .map(|p| p.endpoint(&base, &model))
                .unwrap_or_default(),
            base_url: base,
            context_tokens: row
                .as_ref()
                .and_then(|r| r.context.as_deref())
                .and_then(models::context_tokens),
            multimodal: row.as_ref().map(|r| r.multimodal).unwrap_or(false),
            has_key: key.is_some(),
            key_hint: key.as_deref().map(secrets::mask_of),
        })
    })
    .await
    .map_err(|e| format!("取模型信息失败: {e}"))?
}

/// 复制任意一段文本到剪贴板(模型信息的逐项复制)。**明文密钥不走这里** ——
/// 那一条有 key_copy 专用路(明文不回前端)。
#[tauri::command]
async fn copy_text(text: String) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("没有可复制的内容".into());
    }
    tokio::task::spawn_blocking(move || copy_to_clipboard(&text))
        .await
        .map_err(|e| format!("复制失败: {e}"))?
}

/// 渠道密钥的明文(用户点「显示」时一次性返回)。**这是产品里唯一把密钥回前端的路径**,
/// 所以只有用户显式点击才会走到这里;界面侧 8 秒自动收回,不留驻。密钥型渠道以外没有可取的。
#[tauri::command]
async fn key_reveal(id: String) -> Result<String, String> {
    let def = providers::find(&id).ok_or_else(|| format!("未知渠道: {id}"))?;
    if !matches!(def.auth, providers::AuthKind::Keyring) {
        return Err(format!("{} 没有可复制的密钥(用的是本机登录态或网页会话)", def.name));
    }
    // 凭据管理器读是 RPC:与 key_hint/set_key 同一条纪律,不下沉就会占住一个 tokio worker
    let rid = id.clone();
    match tokio::task::spawn_blocking(move || secrets::channel_key(&rid))
        .await
        .map_err(|e| format!("读取凭据失败: {e}"))?
    {
        Ok(Some(k)) if !k.trim().is_empty() => Ok(k),
        Ok(_) => Err(format!("{} 还没有保存密钥", def.name)),
        Err(e) => Err(e),
    }
}

/// 复制渠道密钥:明文**不回前端**,后端读出来直接进剪贴板(与「复制三件套」同一条路)。
#[tauri::command]
async fn key_copy(id: String) -> Result<String, String> {
    let key = key_reveal(id.clone()).await?;
    let def = providers::find(&id).expect("上面校验过");
    tokio::task::spawn_blocking(move || copy_to_clipboard(&key).map(|_| format!("已复制 {} 的密钥", def.name)))
        .await
        .map_err(|e| format!("复制失败: {e}"))?
}

/// 用系统 `clip.exe` 写剪贴板(经 stdin,密钥不进命令行)。
/// 输入按 UTF-16LE:clip 按控制台代码页解释字节流,中文标签走 UTF-8 会被切坏。
fn copy_to_clipboard(text: &str) -> Result<(), String> {
    use std::io::Write;
    let mut cmd = std::process::Command::new("clip");
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // GUI 程序默认会给子进程配一个新的控制台窗口 —— clip.exe 也会闪一下黑框
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("调用 clip.exe 失败: {e}"))?;
    {
        let mut si = child.stdin.take().ok_or("拿不到 clip 的标准输入")?;
        let utf16: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        si.write_all(&utf16)
            .map_err(|e| format!("写入剪贴板失败: {e}"))?;
    }
    let st = child
        .wait()
        .map_err(|e| format!("clip.exe 未正常结束: {e}"))?;
    if !st.success() {
        return Err("clip.exe 返回失败".into());
    }
    Ok(())
}

/// 在资源管理器里打开一个目录(备份目录 / 客户端配置目录)。
/// 界面上的路径是 `~/…` 缩写(见 `inject::pretty`),这里先展开成真实路径再打开 ——
/// 否则资源管理器会去找一个叫 `~` 的目录。
#[tauri::command]
async fn open_path(app: tauri::AppHandle, path: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        use tauri_plugin_opener::OpenerExt;
        let raw = path.trim();
        let expanded = match raw.strip_prefix('~') {
            Some(rest) => {
                let home = std::env::var("USERPROFILE")
                    .or_else(|_| std::env::var("HOME"))
                    .map_err(|_| "拿不到用户目录,无法展开 ~".to_string())?;
                format!("{}{}", home, rest.replace('/', "\\"))
            }
            None => raw.to_string(),
        };
        let target = std::path::PathBuf::from(&expanded);
        if !target.exists() {
            return Err(format!("路径不存在: {raw}"));
        }
        // 词法上的 `..` 一律拒绝:starts_with 是按组件比、**不解析父目录**的,
        // 于是 `<site.root>\..\..\..\..\Windows\System32\calc.exe` 也能过白名单 ——
        // 这条命令就退化成"把任意 exe 交给资源管理器"的原语(P3-24 要挡的正是这个)。
        if target
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err("路径里不允许出现 ..".to_string());
        }
        // 只允许打开挂件认识的客户端配置目录(含各自的 backup-inject/):
        // 两侧都归一成真实路径再比,短名 / 大小写 / 符号链接都绕不过去。
        let canon = std::fs::canonicalize(&target).map_err(|e| format!("路径无效: {e}"))?;
        let allowed = inject::sites().iter().any(|site| {
            std::fs::canonicalize(&site.root)
                .map(|r| canon.starts_with(&r))
                .unwrap_or(false)
        });
        if !allowed {
            return Err("只能打开客户端自己的配置目录与备份目录".to_string());
        }
        app.opener()
            .open_path(expanded, None::<&str>)
            .map_err(|e| format!("打开失败: {e}"))
    })
    .await
    .map_err(|e| format!("打开失败: {e}"))?
}

/// 排查日志的落点与大小(设置页要显示"日志在哪儿、多大")。
/// 只报路径,不读内容 —— 打开/复制是另外两条命令,便于前端按需触发。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagLogInfo {
    pub path: String,
    pub dir: String,
    pub size: u64,
    /// 静默关闭了吗(`TOKENSCOPE_NODIAG=1`)。关着时界面上要如实说明,
    /// 否则用户以为"有日志",报问题时却什么都没有。
    pub enabled: bool,
    /// 「详细日志」现在开着吗(成功的请求也记)。界面据此回填开关。
    pub verbose: bool,
    /// 本次运行的会话 id(8 位十六进制)。用户报问题时说出它,就能定位那一次运行。
    pub session: String,
}

#[tauri::command]
async fn diag_log_info() -> Result<DiagLogInfo, String> {
    let dir = tokenscope_core::diaglog::log_dir().ok_or("拿不到日志目录")?;
    let path = tokenscope_core::diaglog::log_path().unwrap_or_else(|| dir.join(tokenscope_core::diaglog::LOG_FILE));
    Ok(DiagLogInfo {
        path: path.display().to_string(),
        dir: dir.display().to_string(),
        size: tokenscope_core::diaglog::size_bytes(),
        enabled: !tokenscope_core::diaglog::is_disabled(),
        verbose: tokenscope_core::diaglog::is_verbose(),
        session: tokenscope_core::diaglog::session_id(),
    })
}

/// 读日志尾巴(设置页预览 + 用户点「复制」时用)。只读文本,不含任何密钥
/// (写入侧已经做过 [`tokenscope_core::diaglog::redact`])。
#[tauri::command]
async fn diag_log_tail(lines: Option<usize>) -> Result<String, String> {
    let n = lines.unwrap_or(400).clamp(1, 5000);
    let got = tokenscope_core::diaglog::tail_lines(n);
    if got.is_empty() {
        return Ok("(日志还是空的 —— 等一轮取数之后再看,或者点一次「立即刷新」)".to_string());
    }
    Ok(got.join("\n"))
}

/// 在资源管理器里打开日志目录。**与 `open_path` 分开**:那条只允许打开客户端配置
/// 目录(白名单很窄),而这里只有"我们自己的日志目录"一个落点,不需要也不该复用。
#[tauri::command]
async fn open_log_dir(app: tauri::AppHandle) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        use tauri_plugin_opener::OpenerExt;
        let dir = tokenscope_core::diaglog::log_dir().ok_or("拿不到日志目录".to_string())?;
        if !dir.exists() {
            std::fs::create_dir_all(&dir).map_err(|e| format!("建日志目录失败: {e}"))?;
        }
        app.opener()
            .open_path(dir.display().to_string(), None::<&str>)
            .map_err(|e| format!("打开失败: {e}"))
    })
    .await
    .map_err(|e| format!("打开失败: {e}"))?
}

/// 清空日志(用户报完问题、想重新观察一段时用)。
#[tauri::command]
async fn diag_log_clear() -> Result<String, String> {
    tokenscope_core::diaglog::clear()?;
    // 清完立刻记一行:让"新的一段"从明确的起点开始,不至于看起来像日志坏了
    tokenscope_core::diaglog::write("日志", "已清空(用户手动)");
    Ok("日志已清空".to_string())
}

/// 开关「详细日志」。
///
/// 默认关。打开后**成功的**请求也落盘 —— 排查"余额数字不对"这类问题必须看
/// 成功响应,那是唯一能看到"接口到底返了什么"的地方。绝大多数问题只需要失败
/// 现场(默认档就够),所以这个开关默认关,免得 99% 的人被日志量干扰。
///
/// 状态存进设置(重启后还在),但**读设置之前先用内存里的值** ——
/// 开关是"临时的调试手段",不该在配置还没加载出来时被打回默认。
#[tauri::command]
async fn diag_log_verbose(app: tauri::AppHandle, on: bool) -> Result<bool, String> {
    tokenscope_core::diaglog::set_verbose(on);
    tokenscope_core::diaglog::write(
        "日志",
        &format!("详细日志已{}", if on { "打开" } else { "关闭" }),
    );
    // 存进设置:下次启动还是这个状态
    let st = app.state::<AppState>();
    if let Err(e) = save_config_off_main(&st, move |c| c.diag_verbose = on).await {
        eprintln!("详细日志开关落库失败: {e}");
    }
    Ok(on)
}

/// 导出诊断包:环境 + 体检 + 事件日志 + 失败现场,合成**一份**可复制的文本。
///
/// 为什么合成一份:以前用户要贴两样东西(体检报告 + 日志),support 还要人工对。
/// 合成一份之后,用户只贴一样,support 也不用追问第二轮。
///
/// 纯文本而不是 zip:目标是"贴进聊天窗口",一段文本比一个压缩包好使。
#[tauri::command]
async fn diag_export(app: tauri::AppHandle) -> Result<String, String> {
    // 体检那一半要真打请求(最多十几秒)。`run_diagnostic` 本身是 async 的,
    // 里面的取数也是 await 的,直接等它就行 —— 真正占 CPU 的是 HTTP,不是这里。
    //
    // ⚠ 体检失败**不能带崩整个导出**:网络坏了正是用户最需要日志的时候,
    // 那时体检必然失败(要真打请求)—— 要是因此什么都拿不到,这个功能就废了。
    let report = match run_diagnostic(app.clone()).await {
        Ok(r) => r,
        Err(e) => format!("(体检没能跑完:{e})—— 下面是事件日志,先看这段"),
    };
    // 体检报告里含渠道名、地址、错误文案(可能带 URL),**也要过一遍脱敏** ——
    // 页脚写着"已脱敏",就得真的脱敏,不能只在日志那一段做。
    let report = tokenscope_core::diaglog::redact(&report);

    let session = tokenscope_core::diaglog::session_id();
    let version = env!("CARGO_PKG_VERSION");
    let now = tokenscope_core::fetch::local_datetime_string();
    let mut s = String::new();
    let bar = "=".repeat(60);
    s.push_str(&bar);
    s.push_str(&format!(
        "\n Grandettoken 诊断包 · {now} · v{version}\n 会话 ID: {session}\n"
    ));
    s.push_str(&bar);
    s.push_str("\n\n【1】环境\n");
    s.push_str(&diag_env_section());
    s.push_str("\n\n【2】体检(逐渠道真打一次)\n");
    s.push_str(&report);
    s.push_str("\n\n【3】事件日志(最近的记录)\n");
    let events = tokenscope_core::diaglog::tail_lines(400);
    if events.is_empty() {
        s.push_str("(日志还是空的 —— 等一轮取数之后再看)\n");
    } else {
        s.push_str(&events.join("\n"));
        s.push('\n');
    }
    s.push_str("\n【4】说明\n");
    s.push_str("本包已做脱敏:密钥、Cookie、Token 一律不落原文。\n");
    s.push_str("但路径、域名、渠道名仍是敏感信息 —— 请当作敏感信息处理。\n");
    Ok(s)
}

/// 诊断包里的环境段。
fn diag_env_section() -> String {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<取不到>".into());
    let dir = tokenscope_core::diaglog::log_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_else(|| "<取不到>".into());
    let drives = tokenscope_core::workbuddy::drive_letters()
        .iter()
        .map(|d| format!("{d}:"))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "  Grandettoken {} · exe={exe}\n  日志目录={dir}\n  本机盘符: {drives}",
        env!("CARGO_PKG_VERSION")
    )
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // 必须是第一个插件:第二个实例不再往下走,直接把已开实例的窗口唤到前台。
        // 之前双开会出现两个托盘图标、两个轮询一起打账务接口,而且两套 SQLite
        // 连接写同一个库(报告 O-3)。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let was = w.is_visible().unwrap_or(false);
                let _ = w.show();
                let _ = w.set_focus();
                if !was {
                    let _ = app.emit("window-shown", ());
                }
                // 第二实例把窗口唤回来时也补一轮,别让人看到半天前的数字(P3-37)
                refresh_if_stale(app, 60);
            }
        }))
        .plugin(tauri_plugin_opener::init())
        // 系统通知(裁决 3A)。前端没有打包器拿不到插件的 JS 包,所以通知**全在
        // Rust 侧发**(poll_loop → maybe_notify_crit),这里只负责把能力挂上。
        .plugin(tauri_plugin_notification::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                // 全插件共用这一个回调:按下的那一刻切换主窗口显隐(与托盘左键同语义)
                .with_handler(|app, _shortcut, ev| {
                    use tauri_plugin_global_shortcut::ShortcutState;
                    if ev.state != ShortcutState::Pressed {
                        return;
                    }
                    if let Some(w) = app.get_webview_window("main") {
                        if w.is_visible().unwrap_or(false) {
                            let _ = w.hide();
                        } else {
                            let _ = w.show();
                            let _ = w.set_focus();
                            // 真正「隐藏 → 显示」的那一刻告知前端(重放入场动画)。
                            // 前端不能靠 focus 事件区分 alt-tab 与唤回(报告 B8)
                            let _ = app.emit("window-shown", ());
                            refresh_if_stale(app, 60);
                        }
                    }
                })
                .build(),
        )
        // 公钥与端点都在 tauri.conf.json 的 plugins.updater 里,插件自己读配置
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let dir = app.path().app_data_dir().map_err(|e| {
                let m = format!("无法定位数据目录: {e}");
                report_fatal("定位数据目录", &m)
            })?;
            let store = Store::open(&dir.join("tokenscope.db")).map_err(|e| {
                let m = format!(
                    "打开数据文件失败: {e}(路径 {});若还有 Grandettoken.exe 在运行,先结束它",
                    dir.join("tokenscope.db").display()
                );
                report_fatal("打开数据库", &m)
            })?;
            // 配置坏了不能静默用默认值:那会把用户的隐藏渠道、快捷键、轮询间隔
            // 全部悄悄还原(报告 P2-14)。取默认值继续启动,但把原因留给前端提示。
            let (config, mut startup_error) = match Config::try_load(&store) {
                Ok(c) => (c, None),
                Err(e) => {
                    // 用默认值继续启动是既定取舍(P2-14),但必须把后果说清、并且别再让
                    // 一次随手保存把还能手工救回的那行 JSON 覆盖掉(报告 P1-16)。
                    CONFIG_BROKEN.store(true, std::sync::atomic::Ordering::Relaxed);
                    let msg = format!(
                        "{e} —— 现已按默认设置运行:你之前隐藏的渠道恢复了取数。设置保存已暂停,以免覆盖还能修复的原始配置"
                    );
                    (Config::default(), Some(msg))
                }
            };

            // 注册表与配置对齐:开机自启以配置为准,并刷新为当前 exe 路径,
            // 用户挪动 exe 后无需重新设置自启。构建目录里的开发版不写键,
            // 旧版残留的自启项要让用户看得见(光靠新版删会被旧版开机时写回来)。
            if let Some(msg) = autostart::sync(config.autostart) {
                startup_error = note(startup_error, msg);
            }

            // 置顶 + 上次位置是持久化设置:启动时按配置应用,折叠形态没有置顶按钮也生效
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_always_on_top(config.always_on_top);
                // 恢复窗口位置(报告 O-6)。多屏环境下副屏拔掉后旧坐标落在屏幕外,
                // 窗口"消失"又无从找回 —— 所以按当前可用显示器裁一遍。
                if let (Some(x), Some(y)) = (config.win_x, config.win_y) {
                    if let Ok(size) = w.outer_size() {
                        let (cx, cy) =
                            clamp_to_monitors(app.handle(), x, y, size.width as i32, size.height as i32);
                        if cx != x || cy != y {
                            eprintln!("窗口位置 ({x},{y}) 不在任何显示器内,已修正为 ({cx},{cy})");
                        }
                        let _ = w.set_position(tauri::PhysicalPosition::new(cx, cy));
                    }
                }
            }

            let _ = store.prune(now_ts() - 90 * 86400, tokenscope_core::fetch::local_tz_offset());

            // 排查用日志:目录与 SQLite 同一个文件夹(用户找得着、也跟着数据一起备份/清理)。
            // **必须在 manage 与起轮询之前**:第一轮取数的失败链要能落进文件,否则
            // "装上就取不到数"那种场景又只剩一句界面文案(2026-10-20 用户报的那类)。
            tokenscope_core::diaglog::set_dir(dir.clone());
            // 「详细日志」要在横幅**之前**生效:否则用户上次开着详细日志,
            // 这一轮的启动事件就只记了"关着",横幅与实际对不上。
            tokenscope_core::diaglog::set_verbose(config.diag_verbose);
            tokenscope_core::diaglog::startup_banner(env!("CARGO_PKG_VERSION"));

            app.manage(AppState {
                store: Arc::new(Mutex::new(store)),
                config: Mutex::new(config.clone()),
                fetcher: Arc::new(Fetcher::new()),
                hotkey: Mutex::new(None),
                startup_error: Mutex::new(None),
            });

            // 网页登录型渠道的会话来源:每轮取数时现从 webview 的 Cookie 罐里读。
            // 回调会在取数路径的 spawn_blocking 里被调用(见 set_web_credential_source
            // 的注释),里面那次 webview 派发是阻塞的 —— 只能在非主线程上调,符合。
            let session_app = app.handle().clone();
            app.state::<AppState>()
                .fetcher
                .set_web_credential_source(Box::new(move |id| wb_session_of(&session_app, id)));

            // 启动时注册已保存的全局快捷键;被占用不拦启动,但要把原因留给界面
            // (之前只 eprintln,绿色版从托盘启动时没人看得到 stderr —— 报告 P2-14)
            if !config.hotkey.is_empty() {
                let st = app.state::<AppState>();
                if let Err(e) = apply_hotkey(app.handle(), &st, &config.hotkey) {
                    eprintln!("全局快捷键注册失败: {e}");
                    startup_error = note(startup_error, e);
                }
            }
            if let Some(msg) = startup_error {
                *lock_rw(&app.state::<AppState>().startup_error) = Some(msg);
            }

            let refresh = MenuItem::with_id(app, "refresh", "立即刷新", true, None::<&str>)?;
            let show = MenuItem::with_id(app, "show", "显示面板", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &refresh, &sep, &quit])?;

            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Grandettoken")
                .menu(&menu)
                // 左键只切换显示/隐藏(下面的事件处理),菜单留给右键。
                // 不设这个开关时 Tauri 在 Windows 上左键也会弹菜单。
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id().as_ref() {
                    // 正在写客户端配置时立刻退出会留下"改了没记账"的中间态,先等它写完(P1-15)
                    "quit" => quit_when_idle(app),
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let was = w.is_visible().unwrap_or(false);
                            let _ = w.show();
                            let _ = w.set_focus();
                            if !was {
                                let _ = app.emit("window-shown", ());
                            }
                            refresh_if_stale(app, 60);
                        }
                    }
                    "refresh" => {
                        let handle = app.clone();
                        tauri::async_runtime::spawn(async move { refresh_now(&handle).await });
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = ev
                    {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            if w.is_visible().unwrap_or(false) {
                                let _ = w.hide();
                            } else {
                                let _ = w.show();
                                let _ = w.set_focus();
                                let _ = app.emit("window-shown", ());
                                refresh_if_stale(app, 60);
                            }
                        }
                    }
                })
                .build(app)?;

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move { poll_loop(handle).await });
            // 签到节拍器:独立于取数轮询(签到是写操作,节奏要分开)。
            // **必须在原有 setup 里起** —— Tauri 的 Builder::setup 只有一个槽位,
            // 再加一个会把 manage(AppState) 那套顶掉,应用会在第一条命令就 panic。
            spawn_checkin_ticker(app.handle().clone());

    // 引用索引对账(设计稿 product-vault-ref-reconcile-2026-10-21):
    // 启动后台按真实指针重抄一遍索引 —— 无论上次是怎么走偏的,下次启动自己就好了。
    spawn_vault_ref_rebuild(app.handle().clone());

            // 窗口配置为 visible:false,这里统一控制显隐:
            // 正常启动立即显示;开机自启带 --hidden,直接进托盘等用户点开
            if !std::env::args().any(|a| a == "--hidden") {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }

            Ok(())
        })
        // WorkBuddy 登录窗被用户自己关掉(没抓到会话)时,要让界面把「登录中」收回去,
        // 否则会一直卡在等令牌的样子。抓到会话那一次是我们主动关窗的,由 WB_LOGIN_DONE
        // 标位区分,不该再报一次"已取消"。观察者也靠 WB_LOGIN_CLOSED 收工 ——
        // 它因此不用去问一个正在销毁的窗口(那种调用会 panic,见 on_main_with_timeout)。
        .on_window_event(|window, event| {
            if window.label() != WB_LOGIN_LABEL || !matches!(event, tauri::WindowEvent::Destroyed) {
                return;
            }
            WB_LOGIN_CLOSED.store(true, std::sync::atomic::Ordering::Relaxed);
            if WB_LOGIN_DONE.swap(false, std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let _ = window.emit(
                "workbuddy-login",
                serde_json::json!({ "ok": false, "cancelled": true }),
            );
        })
        .invoke_handler(tauri::generate_handler![
            get_channels,
            home_view,
            get_config,
            set_config,
            set_key,
            delete_key,
            workbuddy_login,
            get_series,
            channel_changes,
            set_autostart,
            set_pin,
            set_tray_icon,
            key_hint,
            window_cmd,
            check_update,
            install_update,
            skip_update_version,
            save_window_pos,
            app_version,
            take_startup_error,
            get_model_catalog,
            inject_preview,
            inject_apply,
            inject_remove,
            inject_state,
            inject_rollback,
            copy_model_bundle,
            model_info,
            copy_text,
            open_path,
            diag_log_info,
            diag_log_tail,
            open_log_dir,
            diag_log_clear,
            diag_log_verbose,
            diag_export,
            checkin_state,
            checkin_now,
            key_reveal,
            key_copy,
            vault_list,
            vault_save,
            vault_delete,
            vault_reveal,
            vault_copy,
            // 渠道密钥 ↔ 密钥箱(设计稿 product-vault-channel-bridge)
            key_source,
            set_key_ref,
            clear_key_ref,
            vault_ref_channels,
            vault_ref_map,
            probe_channel,
            run_diagnostic,
            custom_test,
            custom_add,
            custom_remove,
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            // 以前是 expect:abort 之后连一行字都看不到(GUI 子系统 + panic=abort)
            let msg = report_fatal("运行期", &e.to_string());
            eprintln!("{msg}");
            std::process::exit(1);
        });
}
