//! Antigravity 配额取数（仅官方 `agy` CLI 公开输出，零凭据/桌面库嗅探）。
//!
//! 产品设计见 `docs/product/product-antigravity-quota-2026-11-15.md`：
//! - 通过 `agy -p /usage --output-format json --print-timeout 25s` 获取配额快照；
//! - 主数字优先锁定 `gemini-weekly`（`w.main = true`），其次 `gemini-5h`，否则首条有效窗口；
//! - AI Credits 一期不读取、不展示；
//! - 支持可选自定义 CLI 路径；未找到 / 未登录时如实报错，绝不把未知数值伪装为 `0%`。

use crate::appauth::Credential;
use crate::errcodes;
use crate::providers::{num, round1, FetchResult, Kind, Window};
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 子进程单次硬超时（高于 `--print-timeout 25s`，留给冷启动与退场缓冲）。
const HARD_TIMEOUT: Duration = Duration::from_secs(35);
/// `--version` 探测超时。
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
/// stdout / stderr 单流读取上限（1 MiB）。
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// 自动轮询最小冷却时间（秒）：手点刷新与测试连接可跳过。
pub const AUTO_COOLDOWN_SECS: i64 = 120;

static CUSTOM_CLI_PATH: OnceLock<Mutex<Option<String>>> = OnceLock::new();
static VERSION_CACHE: OnceLock<Mutex<HashMap<(PathBuf, Option<u64>), Option<String>>>> =
    OnceLock::new();
static USAGE_CACHE: OnceLock<Mutex<Option<CachedUsage>>> = OnceLock::new();
static CLI_GATE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
struct CachedUsage {
    path: String,
    ts: i64,
    outcome: AgyOutcome,
}

/// CLI 调用结果（与 `fetch.rs` 解耦，仅传错误分类标签）。
#[derive(Debug, Clone)]
pub struct AgyOutcome {
    pub result: FetchResult,
    /// `"auth"` | `"transport"` | `"server"` | `"business"` | `None`
    pub fail_kind: Option<&'static str>,
    pub err_code: Option<&'static str>,
    pub bytes: usize,
    pub shape: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    CustomMissing(String),
    NotFound,
}

fn cli_path_slot() -> &'static Mutex<Option<String>> {
    CUSTOM_CLI_PATH.get_or_init(|| Mutex::new(None))
}

fn usage_cache_slot() -> &'static Mutex<Option<CachedUsage>> {
    USAGE_CACHE.get_or_init(|| Mutex::new(None))
}

fn cli_gate() -> &'static tokio::sync::Mutex<()> {
    CLI_GATE.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 更新内存中的自定义 `agy` 路径（留空则回到自动探测）；变更时清空冷却缓存。
pub fn set_cli_path(path: Option<String>) {
    let normalized = path
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut slot = cli_path_slot().lock().unwrap_or_else(|p| p.into_inner());
    if *slot != normalized {
        *slot = normalized;
        clear_cooldown();
    }
}

/// 清除 120s 自动轮询冷却（手动刷新、配置保存、遗忘渠道时调用）。
pub fn clear_cooldown() {
    *usage_cache_slot().lock().unwrap_or_else(|p| p.into_inner()) = None;
}

/// 按配置或本机默认安装位置解析 `agy` 可执行文件路径。
pub fn resolve_agy_path() -> Result<PathBuf, ResolveError> {
    let custom = cli_path_slot()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    resolve_agy_path_with(
        custom.as_deref(),
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).as_deref(),
        std::env::var_os("USERPROFILE").map(PathBuf::from).as_deref(),
        std::env::var_os("PATH").as_deref(),
    )
}

fn resolve_agy_path_with(
    custom: Option<&str>,
    local_app_data: Option<&Path>,
    user_profile: Option<&Path>,
    path_var: Option<&std::ffi::OsStr>,
) -> Result<PathBuf, ResolveError> {
    if let Some(raw) = custom.map(str::trim).filter(|s| !s.is_empty()) {
        let p = PathBuf::from(raw);
        if p.is_file() {
            return Ok(p);
        }
        #[cfg(windows)]
        if p.extension().is_none() {
            let exe = p.with_extension("exe");
            if exe.is_file() {
                return Ok(exe);
            }
        }
        return Err(ResolveError::CustomMissing(raw.to_string()));
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(local) = local_app_data {
        candidates.push(local.join("agy").join("bin").join("agy.exe"));
        candidates.push(local.join("agy").join("bin").join("agy"));
    }
    if let Some(home) = user_profile {
        candidates.push(home.join(".local").join("bin").join("agy.exe"));
        candidates.push(home.join(".local").join("bin").join("agy"));
    }
    if let Some(paths) = path_var {
        for dir in std::env::split_paths(paths) {
            #[cfg(windows)]
            {
                candidates.push(dir.join("agy.exe"));
                candidates.push(dir.join("agy.cmd"));
            }
            candidates.push(dir.join("agy"));
        }
    }
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .ok_or(ResolveError::NotFound)
}

fn file_mtime_secs(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

fn configure_command(cmd: &mut Command, workdir: &Path) {
    cmd.current_dir(workdir)
        .env("AGY_CLI_DISABLE_AUTO_UPDATE", "true")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.env("BROWSER", "NUL").creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        cmd.env("BROWSER", "/bin/false");
    }
}

struct TempDirGuard(PathBuf);

impl TempDirGuard {
    fn new(prefix: &str) -> Self {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("{prefix}-{}-{seq}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        Self(dir)
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct StreamCapture {
    bytes: Vec<u8>,
    truncated: bool,
}

fn spawn_capped_reader<R: Read + Send + 'static>(
    mut reader: R,
    cap: usize,
) -> std::thread::JoinHandle<StreamCapture> {
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut buf = [0u8; 8192];
        let mut truncated = false;
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out.len() + n > cap {
                        truncated = true;
                        break;
                    }
                    out.extend_from_slice(&buf[..n]);
                }
            }
        }
        StreamCapture {
            bytes: out,
            truncated,
        }
    })
}

fn run_command_capped(
    path: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<(Option<i32>, StreamCapture, StreamCapture), String> {
    let workdir = TempDirGuard::new("tokenscope-agy");
    let mut cmd = Command::new(path);
    cmd.args(args);
    configure_command(&mut cmd, &workdir.0);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("无法启动 agy CLI: {e}"))?;
    let stdout_h = child
        .stdout
        .take()
        .map(|r| spawn_capped_reader(r, MAX_OUTPUT_BYTES));
    let stderr_h = child
        .stderr
        .take()
        .map(|r| spawn_capped_reader(r, MAX_OUTPUT_BYTES));

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st.code()),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("等待 agy CLI 退出失败: {e}"));
            }
        }
    };

    let out = stdout_h
        .and_then(|h| h.join().ok())
        .unwrap_or(StreamCapture {
            bytes: Vec::new(),
            truncated: false,
        });
    let err = stderr_h
        .and_then(|h| h.join().ok())
        .unwrap_or(StreamCapture {
            bytes: Vec::new(),
            truncated: false,
        });
    match status {
        Some(code) => Ok((code, out, err)),
        None => Err(format!(
            "agy CLI 执行超时（超过 {} 秒），已中止进程",
            timeout.as_secs()
        )),
    }
}

fn probe_cli_version(path: &Path) -> Option<String> {
    let key = (path.to_path_buf(), file_mtime_secs(path));
    let cache = VERSION_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
        return hit.clone();
    }
    let parsed = match run_command_capped(path, &["--version"], VERSION_TIMEOUT) {
        Ok((Some(0), out, _)) if !out.truncated => {
            let line = String::from_utf8_lossy(&out.bytes);
            let clean: String = line
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .chars()
                .filter(|c| !c.is_control())
                .take(32)
                .collect();
            if clean.is_empty() {
                None
            } else {
                Some(clean)
            }
        }
        _ => None,
    };
    cache
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key, parsed.clone());
    parsed
}

/// 生成供 `appauth` / `fetch::credentials` 使用的 CLI 候选列表及缺失诊断。
pub fn app_credentials() -> (Vec<Credential>, Option<String>) {
    match resolve_agy_path() {
        Ok(path) => {
            let ver = probe_cli_version(&path);
            let source = match ver {
                Some(v) => format!("agy CLI {v}"),
                None => "agy CLI".to_string(),
            };
            (
                vec![Credential {
                    token: path.to_string_lossy().into_owned(),
                    source,
                    account_id: None,
                    device_id: None,
                }],
                None,
            )
        }
        Err(ResolveError::CustomMissing(raw)) => (
            Vec::new(),
            Some(format!(
                "指定的 agy CLI 路径不存在: {raw} · 请检查路径或清空改用自动探测"
            )),
        ),
        Err(ResolveError::NotFound) => (
            Vec::new(),
            Some(
                "未检测到 agy CLI · 请先安装并在终端运行 agy auth login，或在设置中指定路径"
                    .to_string(),
            ),
        ),
    }
}

/// 单飞 + 120s 冷却获取 Antigravity 配额；`cli_path` 来自 `Credential.token`。
pub async fn fetch_usage(cli_path: &str) -> AgyOutcome {
    let _guard = cli_gate().lock().await;
    let now = now_secs();
    {
        let cache = usage_cache_slot().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(hit) = cache.as_ref() {
            if hit.path == cli_path
                && hit.outcome.result.valid
                && hit.ts <= now
                && now - hit.ts < AUTO_COOLDOWN_SECS
            {
                return hit.outcome.clone();
            }
        }
    }

    let path = PathBuf::from(cli_path);
    let outcome = tokio::task::spawn_blocking(move || exec_agy_usage(&path))
        .await
        .unwrap_or_else(|_| fail_outcome("agy CLI 后台任务异常退出", "transport", errcodes::CONNECT, 0, String::new()));

    if outcome.result.valid {
        *usage_cache_slot().lock().unwrap_or_else(|p| p.into_inner()) = Some(CachedUsage {
            path: cli_path.to_string(),
            ts: now_secs(),
            outcome: outcome.clone(),
        });
    }
    outcome
}

fn exec_agy_usage(path: &Path) -> AgyOutcome {
    if !path.is_file() {
        return fail_outcome(
            &format!("找不到 agy CLI 可执行文件: {}", path.display()),
            "business",
            errcodes::BODY_SHAPE,
            0,
            String::new(),
        );
    }
    match run_command_capped(
        path,
        &["-p", "/usage", "--output-format", "json", "--print-timeout", "25s"],
        HARD_TIMEOUT,
    ) {
        Ok((code, out, err)) => {
            if out.truncated {
                return fail_outcome(
                    "agy CLI 输出超过 1 MiB 上限，已丢弃",
                    "server",
                    errcodes::BODY_SHAPE,
                    out.bytes.len(),
                    String::new(),
                );
            }
            parse_cli_output(code, &out.bytes, &err.bytes)
        }
        Err(msg) => fail_outcome(&msg, "transport", errcodes::TIMEOUT, 0, String::new()),
    }
}

fn fail_outcome(
    msg: &str,
    kind: &'static str,
    code: &'static str,
    bytes: usize,
    shape: String,
) -> AgyOutcome {
    AgyOutcome {
        result: FetchResult {
            valid: false,
            kind: Kind::Percent,
            unit: "%".into(),
            error: Some(msg.to_string()),
            ..Default::default()
        },
        fail_kind: Some(kind),
        err_code: Some(code),
        bytes,
        shape,
    }
}

fn looks_like_auth_error(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "not logged in",
        "select login method",
        "agy auth login",
        "unauthenticated",
        "unauthorized",
        "authentication required",
        "login required",
        "invalid_grant",
        "token expired",
        "re-authenticate",
    ]
    .iter()
    .any(|kw| lower.contains(kw))
}

fn safe_hint(raw: &[u8]) -> String {
    let txt = String::from_utf8_lossy(&raw[..raw.len().min(300)]);
    let clean: String = txt
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(120)
        .collect();
    clean.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_cli_output(exit_code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> AgyOutcome {
    let bytes = stdout.len();
    if let Ok(body) = serde_json::from_slice::<Value>(stdout) {
        let shape = body
            .as_object()
            .map(|o| o.keys().take(12).cloned().collect::<Vec<_>>().join(","))
            .unwrap_or_default();

        if let Some(st) = body.get("status").and_then(|v| v.as_str()) {
            if !st.eq_ignore_ascii_case("SUCCESS") {
                let err_txt = body
                    .get("error")
                    .or_else(|| body.get("message"))
                    .or_else(|| body.get("response"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if looks_like_auth_error(err_txt)
                    || looks_like_auth_error(&String::from_utf8_lossy(stderr))
                {
                    return fail_outcome(
                        "Antigravity 未登录或登录已过期 · 请在终端执行 agy auth login",
                        "auth",
                        errcodes::BODY_SHAPE,
                        bytes,
                        shape,
                    );
                }
                let detail = if err_txt.trim().is_empty() {
                    st.to_string()
                } else {
                    format!("{st}: {}", safe_hint(err_txt.as_bytes()))
                };
                return fail_outcome(
                    &format!("agy CLI 返回失败 ({detail})"),
                    "business",
                    errcodes::BODY_SHAPE,
                    bytes,
                    shape,
                );
            }
        }

        let res = extract_antigravity(&body);
        if res.valid {
            return AgyOutcome {
                result: res,
                fail_kind: None,
                err_code: None,
                bytes,
                shape,
            };
        }
        let resp_str = body.get("response").and_then(|v| v.as_str()).unwrap_or("");
        if looks_like_auth_error(resp_str)
            || looks_like_auth_error(&String::from_utf8_lossy(stderr))
        {
            return fail_outcome(
                "Antigravity 未登录或登录已过期 · 请在终端执行 agy auth login",
                "auth",
                errcodes::BODY_SHAPE,
                bytes,
                shape,
            );
        }
        return AgyOutcome {
            result: res,
            fail_kind: Some("business"),
            err_code: Some(errcodes::BODY_SHAPE),
            bytes,
            shape,
        };
    }

    let combined = format!(
        "{} {}",
        String::from_utf8_lossy(&stderr[..stderr.len().min(2048)]),
        String::from_utf8_lossy(&stdout[..stdout.len().min(2048)])
    );
    if looks_like_auth_error(&combined) {
        return fail_outcome(
            "Antigravity 未登录或授权已过期 · 请在终端执行 agy auth login",
            "auth",
            errcodes::BODY_SHAPE,
            bytes,
            "text".into(),
        );
    }
    let hint = {
        let s = safe_hint(stderr);
        if s.is_empty() { safe_hint(stdout) } else { s }
    };
    if let Some(code) = exit_code.filter(|c| *c != 0) {
        let msg = if hint.is_empty() {
            format!("agy CLI 执行失败（退出码 {code}）")
        } else {
            format!("agy CLI 执行失败（退出码 {code}）: {hint}")
        };
        return fail_outcome(&msg, "business", errcodes::BODY_SHAPE, bytes, "non-json".into());
    }
    fail_outcome(
        "agy CLI 返回非 JSON 输出，无法解析配额",
        "server",
        errcodes::BODY_SHAPE,
        bytes,
        "non-json".into(),
    )
}

fn short_group_name(name: &str) -> String {
    let trimmed = name.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower == "gemini models" || lower == "gemini" {
        "Gemini".to_string()
    } else if lower.contains("claude") && lower.contains("gpt") {
        "Claude/GPT".to_string()
    } else if trimmed.is_empty() {
        "默认".to_string()
    } else {
        trimmed.to_string()
    }
}

fn window_suffix(win: Option<&str>, id: &str, name: &str) -> String {
    let w = win.unwrap_or("").trim().to_ascii_lowercase();
    let id_l = id.to_ascii_lowercase();
    if w == "weekly" || id_l.ends_with("-weekly") {
        "本周".into()
    } else if w == "5h" || id_l.ends_with("-5h") {
        "5 小时".into()
    } else if w == "daily" || id_l.ends_with("-daily") {
        "当日".into()
    } else if w == "monthly" || id_l.ends_with("-monthly") {
        "本月".into()
    } else if !name.trim().is_empty() {
        name.trim().to_string()
    } else {
        "周期".into()
    }
}

fn bucket_disabled(b: &Value) -> bool {
    if b.get("enabled").and_then(|v| v.as_bool()) == Some(false) {
        return true;
    }
    if b.get("disabled").and_then(|v| v.as_bool()) == Some(true) {
        return true;
    }
    matches!(
        b.get("status").and_then(|v| v.as_str()).map(|s| s.to_ascii_lowercase()).as_deref(),
        Some("disabled" | "unavailable")
    )
}

fn clean_group_desc(desc: &str) -> Option<String> {
    let s = desc.trim();
    if s.is_empty() {
        return None;
    }
    let stripped = s
        .strip_prefix("Models within this group:")
        .or_else(|| s.strip_prefix("Models in this group:"))
        .unwrap_or(s)
        .trim();
    if stripped.is_empty() {
        None
    } else {
        Some(stripped.to_string())
    }
}

/// 解析 `agy -p /usage --output-format json` 的 JSON 输出为 [`FetchResult`]。
pub fn extract_antigravity(body: &Value) -> FetchResult {
    if let Some(cmd_name) = body
        .get("command")
        .and_then(|c| c.get("name"))
        .and_then(|v| v.as_str())
    {
        if cmd_name != "usage" {
            return FetchResult {
                valid: false,
                kind: Kind::Percent,
                unit: "%".into(),
                error: Some(format!("agy 返回了非预期命令: {cmd_name}")),
                ..Default::default()
            };
        }
    }

    let groups = body
        .get("command")
        .and_then(|c| c.get("data"))
        .and_then(|d| d.get("groups"))
        .and_then(|g| g.as_array())
        .or_else(|| {
            body.get("data")
                .and_then(|d| d.get("groups"))
                .and_then(|g| g.as_array())
        })
        .or_else(|| body.get("groups").and_then(|g| g.as_array()));

    let Some(groups) = groups else {
        return FetchResult {
            valid: false,
            kind: Kind::Percent,
            unit: "%".into(),
            error: Some("未返回 Antigravity 配额分组（缺少 groups 字段）".into()),
            ..Default::default()
        };
    };

    let mut windows: Vec<Window> = Vec::new();
    let mut extra: Vec<(String, String)> = Vec::new();

    for group in groups {
        let raw_gname = group.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let gshort = short_group_name(raw_gname);
        let Some(buckets) = group.get("buckets").and_then(|v| v.as_array()) else {
            continue;
        };
        let mut group_had_valid = false;
        for b in buckets {
            if bucket_disabled(b) {
                continue;
            }
            let Some(frac) = b.get("remaining_fraction").and_then(num) else {
                continue;
            };
            let remain_pct = round1((frac * 100.0).clamp(0.0, 100.0));
            let used_pct = round1(100.0 - remain_pct);
            let id = b
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("unknown")
                .to_string();
            let bname = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let suffix = window_suffix(b.get("window").and_then(|v| v.as_str()), &id, bname);
            let label = if suffix.to_ascii_lowercase().starts_with(&gshort.to_ascii_lowercase()) {
                suffix
            } else {
                format!("{gshort} {suffix}")
            };
            let resets_at = b
                .get("reset_time")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let status = if remain_pct <= 0.0 {
                "rate-limited".to_string()
            } else {
                "ok".to_string()
            };
            windows.push(Window {
                key: id,
                label,
                percent: used_pct,
                remain_percent: remain_pct,
                status,
                resets_at,
                main: false,
            });
            group_had_valid = true;
        }
        if group_had_valid {
            if let Some(desc) = group
                .get("description")
                .and_then(|v| v.as_str())
                .and_then(clean_group_desc)
            {
                extra.push((format!("模型 · {gshort}"), desc));
            }
        }
    }

    if windows.is_empty() {
        return FetchResult {
            valid: false,
            kind: Kind::Percent,
            unit: "%".into(),
            error: Some("Antigravity 未返回有效配额窗口（缺少 remaining_fraction）".into()),
            ..Default::default()
        };
    }

    // 策略 B（用户已裁决）：主数字固定锁定 Gemini 本周池，其次 Gemini 5 小时池，再次首条。
    let main_idx = windows
        .iter()
        .position(|w| w.key == "gemini-weekly")
        .or_else(|| windows.iter().position(|w| w.key == "gemini-5h"))
        .unwrap_or(0);
    windows[main_idx].main = true;
    let main_win = windows[main_idx].clone();
    let limited = windows.iter().any(|w| w.status == "rate-limited");

    FetchResult {
        valid: true,
        kind: Kind::Percent,
        remaining: Some(main_win.remain_percent),
        used: Some(main_win.percent),
        total: Some(100.0),
        unit: "%".into(),
        windows,
        limited,
        extra,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn m0_fixture() -> Value {
        json!({
            "conversation_id": "",
            "status": "SUCCESS",
            "response": "Your Antigravity usage...",
            "command": {
                "name": "usage",
                "data": {
                    "description": "Your current credit balances and model usage limits.",
                    "groups": [
                        {
                            "name": "Gemini Models",
                            "description": "Models within this group: Gemini 4 Hydrogen, Gemini 3.8 Flash",
                            "buckets": [
                                {
                                    "id": "gemini-weekly",
                                    "name": "Weekly Limit Remaining",
                                    "description": "Models shared within the Weekly Limit pool",
                                    "window": "weekly",
                                    "remaining_fraction": 0.804150402545929,
                                    "reset_time": "2026-11-21T15:48:00Z"
                                },
                                {
                                    "id": "gemini-5h",
                                    "name": "Five Hour Limit Remaining",
                                    "description": "Models shared within the Five Hour Limit pool",
                                    "window": "5h",
                                    "remaining_fraction": 0.3207480013370514,
                                    "reset_time": "2026-11-14T20:48:00Z"
                                }
                            ]
                        },
                        {
                            "name": "Claude and GPT models",
                            "description": "Models within this group: Claude Opus 5.5, Claude Sonnet 5.5, GPT-OSS 120B",
                            "buckets": [
                                {
                                    "id": "3p-weekly",
                                    "name": "Weekly Limit Remaining",
                                    "window": "weekly",
                                    "remaining_fraction": 0.46958479285240173,
                                    "reset_time": "2026-11-21T15:42:56Z"
                                },
                                {
                                    "id": "3p-5h",
                                    "name": "Five Hour Limit Remaining",
                                    "window": "5h",
                                    "remaining_fraction": 0,
                                    "reset_time": "2026-11-14T20:42:56Z"
                                }
                            ]
                        },
                        {
                            "name": "Gemini 4 Argon",
                            "description": "Models within this group: Gemini 4 Argon",
                            "buckets": [
                                {
                                    "id": "gemini-argon-wtu-weekly",
                                    "name": "Gemini 4 Argon Weekly Limit",
                                    "window": "weekly",
                                    "remaining_fraction": 0.30698227882385254,
                                    "reset_time": "2026-11-21T15:56:57Z"
                                }
                            ]
                        }
                    ]
                }
            }
        })
    }

    #[test]
    fn parses_real_m0_three_groups_and_pins_gemini_weekly_main() {
        let r = extract_antigravity(&m0_fixture());
        assert!(r.valid, "{:?}", r.error);
        assert_eq!(r.kind, Kind::Percent);
        assert_eq!(r.unit, "%");
        assert_eq!(r.remaining, Some(80.4));
        assert_eq!(r.used, Some(19.6));
        assert!(r.limited, "3p-5h=0 必须标 limited");
        assert_eq!(r.windows.len(), 5);

        let main = r.windows.iter().find(|w| w.main).unwrap();
        assert_eq!(main.key, "gemini-weekly");
        assert_eq!(main.label, "Gemini 本周");
        assert_eq!(main.remain_percent, 80.4);

        let argon = r.windows.iter().find(|w| w.key == "gemini-argon-wtu-weekly").unwrap();
        assert_eq!(argon.label, "Gemini 4 Argon 本周");
        assert_eq!(argon.remain_percent, 30.7);

        let third_p_5h = r.windows.iter().find(|w| w.key == "3p-5h").unwrap();
        assert_eq!(third_p_5h.label, "Claude/GPT 5 小时");
        assert_eq!(third_p_5h.status, "rate-limited");
        assert_eq!(third_p_5h.remain_percent, 0.0);

        assert!(r.extra.iter().any(|(k, v)| k == "模型 · Gemini" && v.contains("Gemini 4 Hydrogen")));
    }

    #[test]
    fn falls_back_to_gemini_5h_or_first_window_when_gemini_weekly_absent() {
        let r = extract_antigravity(&json!({
            "command": {
                "name": "usage",
                "data": {
                    "groups": [{
                        "name": "Gemini Models",
                        "buckets": [
                            {"id": "gemini-5h", "window": "5h", "remaining_fraction": 0.5}
                        ]
                    }]
                }
            }
        }));
        assert!(r.valid);
        assert_eq!(r.remaining, Some(50.0));
        assert!(r.windows[0].main);

        let starter = extract_antigravity(&json!({
            "command": {
                "name": "usage",
                "data": {
                    "groups": [{
                        "name": "Claude and GPT models",
                        "buckets": [
                            {"id": "3p-weekly", "window": "weekly", "remaining_fraction": 0.72}
                        ]
                    }]
                }
            }
        }));
        assert!(starter.valid);
        assert_eq!(starter.remaining, Some(72.0));
        assert!(starter.windows[0].main);
    }

    #[test]
    fn missing_or_disabled_buckets_never_fabricate_zero() {
        let empty = extract_antigravity(&json!({
            "command": {
                "name": "usage",
                "data": {
                    "groups": [{
                        "name": "Gemini Models",
                        "buckets": [
                            {"id": "gemini-weekly", "window": "weekly"},
                            {"id": "gemini-5h", "window": "5h", "enabled": false, "remaining_fraction": 0}
                        ]
                    }]
                }
            }
        }));
        assert!(!empty.valid);
        assert!(empty.remaining.is_none());
        assert!(empty.error.is_some());
    }

    #[test]
    fn cli_errors_classify_auth_vs_non_json_vs_wrong_command() {
        let auth = parse_cli_output(
            Some(1),
            b"",
            b"Not logged in. Please run `agy auth login` first.",
        );
        assert!(!auth.result.valid);
        assert_eq!(auth.fail_kind, Some("auth"));
        assert!(auth.result.error.as_deref().unwrap_or("").contains("agy auth login"));

        let wrong_cmd = parse_cli_output(
            Some(0),
            br#"{"status":"SUCCESS","command":{"name":"chat","data":{}}}"#,
            b"",
        );
        assert!(!wrong_cmd.result.valid);
        assert_eq!(wrong_cmd.fail_kind, Some("business"));

        let non_json = parse_cli_output(Some(0), b"plain text failure", b"");
        assert!(!non_json.result.valid);
        assert_eq!(non_json.fail_kind, Some("server"));
    }

    #[test]
    fn resolves_custom_without_fallback_and_autodetects_localappdata_first() {
        let tmp = TempDirGuard::new("tokenscope-agy-resolve-test");
        let missing_custom = tmp.0.join("missing-agy.exe");
        let auto_bin = tmp.0.join("local").join("agy").join("bin").join("agy.exe");
        fs::create_dir_all(auto_bin.parent().unwrap()).unwrap();
        fs::write(&auto_bin, b"stub").unwrap();

        let err = resolve_agy_path_with(
            Some(&missing_custom.to_string_lossy()),
            Some(&tmp.0.join("local")),
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ResolveError::CustomMissing(_)));

        let found = resolve_agy_path_with(None, Some(&tmp.0.join("local")), None, None).unwrap();
        assert_eq!(found, auto_bin);
    }
}
