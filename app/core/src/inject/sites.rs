use super::*;

pub(super) fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

pub(super) fn appdata() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

/// Trae 的两个安装(CN 与 SOLO CN 各一套独立 DB)。名字与 `appauth::TRAE_DIRS` 同源。
/// Trae 的安装/数据目录名。**版本改名会让这里落空** —— 2026-10-15 用户侧实测:
/// 更新后只剩「Trae CN」,原来的「TRAE SOLO CN」已消失(用户口径:以前是 traesolo/traework,
/// 现在最新版就叫 Trae)。所以与 `appauth::TRAE_DIRS` 保持同一份名单并多留历史名:
/// 多列一个不花钱,漏一个就是"写了但客户端根本不读那份库"。
const TRAE_DIRS: [&str; 4] = ["Trae CN", "TRAE SOLO CN", "Trae", "TraeWork"];

/// 某套安装的候选根目录(用户级安装优先,再退到机器级)。
fn install_roots(dir: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        out.push(local.join("Programs").join(dir));
        out.push(local.join(dir));
    }
    out.push(PathBuf::from(r"C:\Program Files").join(dir));
    out.push(PathBuf::from(r"C:\Program Files (x86)").join(dir));
    out
}

/// 卸载器 / Electron 辅助进程:它们也是 .exe,但**不是主程序名**。
/// 以前只按小写敏感的 `starts_with("unins")` 排一次,`Unins000.exe`、`uninstall.exe`、
/// `Trae CN Helper.exe` 都会留在候选里,于是 `Site.process` 可能挑到它们 ——
/// 而进程检查是写入前唯一那道门(判"客户端在不在跑")。
pub(super) fn is_helper_exe(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("unins")
        || n.starts_with("uninstall")
        || n.starts_with("uninst")
        || n.contains("helper")
        || n.contains("crashsender")
        || n.contains("crashpad")
}

/// 这套 Trae 安装对应的候选进程名。
///
/// 以前两个安装都共用 `Client::Trae.process()` 那个字面量 `"Trae CN.exe"`:SOLO 在跑时
/// 判不出来 → 照样往它的库里写(退出被内存态覆盖),而没在跑的 CN 反而可能被误判在跑
/// (报告 P1-8)。这里按**安装目录里的 exe 文件名**取,再兜底「目录名.exe」;
/// 本机实测 CN 的文件名是 `Trae CN.exe`,SOLO 那套已卸载(只剩数据目录),只能两个都探。
fn trae_processes(dir: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for base in install_roots(dir) {
        let Ok(rd) = std::fs::read_dir(&base) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let is_exe = p
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x.eq_ignore_ascii_case("exe"))
                .unwrap_or(false);
            if !is_exe {
                continue;
            }
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            // 卸载器/更新器/辅助进程不算主程序名(大小写不敏感)
            if is_helper_exe(&name) {
                continue;
            }
            if !v.contains(&name) {
                v.push(name);
            }
        }
    }
    let by_dir = format!("{dir}.exe");
    if !v.contains(&by_dir) {
        v.push(by_dir);
    }
    v
}

/// 这套安装该拿去判"在不在跑"的进程名:**优先与安装目录同名的主程序**
/// (`Trae CN` → `Trae CN.exe`)。以前 `Site.process` 直接吃 `read_dir` 的顺序,
/// 挑到哪个算哪个 —— 顺序随文件系统变,进程检查时灵时不灵(P2)。
pub(super) fn expected_process(dir: &str, procs: &[String]) -> String {
    let by_dir = format!("{dir}.exe");
    procs
        .iter()
        .find(|p| p.eq_ignore_ascii_case(&by_dir))
        .cloned()
        .or_else(|| procs.first().cloned())
        .unwrap_or_else(|| Client::Trae.process().to_string())
}

/// 显示用路径:`C:\Users\x\...` → `~/.zcode/...`。
pub fn pretty(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    if let Some(h) = home() {
        let h = h.to_string_lossy().replace('\\', "/");
        // 必须是**路径组件**边界上的前缀:以前用字符串 strip_prefix,主目录名互为前缀时
        // (账户 wacil 与 wacilon 并存)会把旁系目录也吃掉,显示成 "~on/.workbuddy/..."
        // 这种既不象 ~ 展开也不是绝对路径的怪串(报告 P3-2)。
        if s.len() > h.len() && s.starts_with(&h) && s[h.len()..].starts_with('/') {
            return format!("~{}", &s[h.len()..]);
        }
    }
    s
}

/// 检测本机装有的写入目标。没装的不返回(界面据「四客户端里缺哪个」显示未安装)。
pub fn sites() -> Vec<Site> {
    sites_with_snapshot(&ProcessSnapshot::capture())
}

fn sites_with_snapshot(processes: &ProcessSnapshot) -> Vec<Site> {
    let mut out = Vec::new();

    if let Some(roaming) = appdata() {
        for dir in TRAE_DIRS {
            let root = roaming.join(dir).join("User").join("globalStorage");
            let db = root.join("state.vscdb");
            if !db.exists() {
                continue;
            }
            let mut files = vec![db.clone()];
            // SQLite 的 WAL/SHM 也要一起备份:只拷主库会在回滚后留下不一致的日志
            for suffix in ["-wal", "-shm"] {
                let f = root.join(format!("state.vscdb{suffix}"));
                if f.exists() {
                    files.push(f);
                }
            }
            let procs = trae_processes(dir);
            let running = procs.iter().find(|p| processes.has(p));
            out.push(Site {
                client: Client::Trae,
                label: dir.to_string(),
                running: running.is_some(),
                process: running.cloned().unwrap_or_else(|| expected_process(dir, &procs)),
                root,
                files,
            });
        }
    }

    if let Some(h) = home() {
        let wb_root = h.join(".workbuddy");
        if wb_root.join("models.json").exists() {
            out.push(Site {
                client: Client::Workbuddy,
                label: Client::Workbuddy.label().to_string(),
                running: processes.has(Client::Workbuddy.process()),
                process: Client::Workbuddy.process().to_string(),
                root: wb_root.clone(),
                files: vec![wb_root.join("models.json"), wb_root.join("settings.json")],
            });
        }
        let wba_root = h.join(".workbuddy-ai");
        if wba_root.join("models.json").exists() {
            out.push(Site {
                client: Client::WorkbuddyAi,
                label: Client::WorkbuddyAi.label().to_string(),
                running: processes.has(Client::WorkbuddyAi.process()),
                process: Client::WorkbuddyAi.process().to_string(),
                root: wba_root.clone(),
                files: vec![wba_root.join("models.json"), wba_root.join("settings.json")],
            });
        }
        let zc_root = h.join(".zcode").join("v2");
        if zc_root.join("config.json").exists() {
            out.push(Site {
                client: Client::Zcode,
                label: Client::Zcode.label().to_string(),
                running: processes.has(Client::Zcode.process()),
                process: Client::Zcode.process().to_string(),
                root: zc_root.clone(),
                files: vec![
                    zc_root.join("config.json"),
                    zc_root.join("provider_config.json"),
                ],
            });
        }
    }

    // DeepSeek Harness:harness home 下挑一个 profile。两份文件不在同一层,
    // 所以 root 是 harness home、回滚靠备份清单(见 backup_layered)。
    if let Some(home) = dsh_home() {
        if let Some(profile) = dsh_profile(&home) {
            let patch = dsh_patch_of(&home, &profile);
            let cred = dsh_creds_of(&home);
            out.push(Site {
                client: Client::Dsh,
                label: format!("{} · {profile}", Client::Dsh.label()),
                running: processes.has(Client::Dsh.process()),
                process: Client::Dsh.process().to_string(),
                root: home.clone(),
                files: vec![patch, cred],
            });
        }
    }

    out.extend(inject_discover::detect_cli_sites(processes));

    out
}

pub fn sites_of(client: Client) -> Vec<Site> {
    sites().into_iter().filter(|s| s.client == client).collect()
}

/// 使用操作系统进程快照查询完整的可执行文件名。
pub fn process_running(process: &str) -> bool {
    ProcessSnapshot::capture().has(process)
}
