use super::*;

/// 桌面端 auth 目录的候选(Windows 优先)。
///
/// **兼容"数据目录不在 C 盘"的各种装法**(2026-10-20 用户实证:WorkBuddy 装在 D 盘):
///   ① `LOCALAPPDATA` / `APPDATA` / `USERPROFILE` —— 正常机器(用户目录被迁移时,
///      这些环境变量本身就已经指向别的盘,跟着走即可);
///   ② `WORKBUDDY_AUTH_DIR` 显式覆盖 —— 用户把数据目录挪走 / 便携版时唯一的可靠来源;
///   ③ 从桌面端 exe 的位置**反推**:不少"绿色/自定义目录"装法会把 userData 放在
///      `<exe 所在目录>\CodeBuddyExtension\...` 或 `<exe 盘>:\CodeBuddyExtension\...`。
///      这条排在系统路径之后,读不到也无害,但能救回一类本来永远找不到的机器。
pub fn auth_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(v) = std::env::var_os("WORKBUDDY_AUTH_DIR") {
        let p = PathBuf::from(v);
        if !p.as_os_str().is_empty() {
            // 允许用户直接给 auth 目录,或给它的上一层(带了 CodeBuddyExtension 就照用)
            out.push(p.clone());
            if !p.ends_with("auth") {
                out.push(p.join("auth"));
            }
        }
    }
    for var in ["LOCALAPPDATA", "APPDATA"] {
        if let Some(v) = std::env::var_os(var) {
            let p = PathBuf::from(v);
            if !p.as_os_str().is_empty() {
                out.push(p.join("CodeBuddyExtension").join("Data").join("Public").join("auth"));
            }
        }
    }
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        let h = PathBuf::from(home);
        out.push(h.join("AppData").join("Local").join("CodeBuddyExtension").join("Data").join("Public").join("auth"));
    }
    // ③ 从 exe 位置反推(只在真的找到 exe 时才有内容;找不到就是空,不产生候选)
    if let Some(exe) = find_app_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join("CodeBuddyExtension").join("Data").join("Public").join("auth"));
            // 盘根那一层:<盘>:\CodeBuddyExtension\...
            if let Some(root) = dir.ancestors().last() {
                out.push(root.join("CodeBuddyExtension").join("Data").join("Public").join("auth"));
            }
        }
    }
    // 正常机器上 LOCALAPPDATA 就等于 USERPROFILE\AppData\Local,同一条路径会被列两遍。
    // 不去重的话,后面会把同一个凭据文件读两遍、解两遍。
    let mut seen: Vec<String> = Vec::new();
    out.retain(|p| {
        let k = p.to_string_lossy().to_lowercase();
        if seen.iter().any(|s| s == &k) {
            false
        } else {
            seen.push(k);
            true
        }
    });
    out
}

/// 扫描 auth 目录里所有 `workbuddy-desktop*.info`。
///
/// 排序规则(踩过一次才定):**live 文件(`workbuddy-desktop.info`)永远第一**,其余按
/// 修改时间新→旧。桌面端会在同一目录留下带时间戳的历史备份,那些备份里的 token 可能
/// 早已被上游作废(2026-10-19 真机:一份 6 月的备份 token 未过期但接口 401),
/// 光按 mtime 排会把用户钉在作废的旧备份上。
pub fn credential_files() -> Vec<PathBuf> {
    let mut live: Vec<PathBuf> = Vec::new();
    let mut others: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for dir in auth_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if !name.starts_with("workbuddy-desktop") || !name.ends_with(".info") {
                continue;
            }
            // live:workbuddy-desktop.info / workbuddy-desktop-ai.info(不带时间戳)
            if name == "workbuddy-desktop.info" || name == "workbuddy-desktop-ai.info" {
                live.push(p);
                continue;
            }
            let mtime = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            others.push((mtime, p));
        }
    }
    others.sort_by(|a, b| b.0.cmp(&a.0));
    live.extend(others.into_iter().map(|(_, p)| p));
    live
}

/// 列出本机存在的盘符(`A`..`Z`)。
///
/// 为什么不用 `GetLogicalDrives`:core 刻意不依赖 Win32 绑定(见 `lib.rs` 顶部),
/// 而 26 次 `is_dir` 是微秒级的事,还天然跳过光驱/未挂载盘。**只有盘根存在才算**,
/// 所以网络盘断线时不会卡在这里(那一次 `is_dir` 会失败并返回 false)。
pub fn drive_letters() -> Vec<char> {
    ('A'..='Z')
        .filter(|c| Path::new(&format!("{c}:\\")).is_dir())
        .collect()
}

/// 桌面端可能落地的目录。**每个盘符都看** —— 2026-10-20 用户的 WorkBuddy 装在 D 盘,
/// 而原来的实现只认 `%PROGRAMFILES%`(C 盘),于是整条凭据链断在第一步,
/// 界面还只能说"没装桌面端"(明明装着)。
pub(super) fn app_roots() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    // ① 注册表(最准):装到哪都写着,包括自定义目录与其它盘
    out.extend(registry_app_roots());
    // ② 环境变量所指的默认位置(用户目录被迁移到别的盘时会跟着走)
    for var in ["LOCALAPPDATA", "APPDATA", "PROGRAMFILES", "PROGRAMFILES(X86)"] {
        if let Some(v) = std::env::var_os(var) {
            let p = PathBuf::from(v);
            if p.as_os_str().is_empty() {
                continue;
            }
            out.push(p.join("Programs").join("WorkBuddy"));
            out.push(p.join("WorkBuddy"));
        }
    }
    // ③ 每个盘符下的常见装法。用户口径「装在 D 盘」通常就是这几种之一:
    //    D:\WorkBuddy、D:\Programs\WorkBuddy、D:\Program Files\WorkBuddy、D:\Apps\WorkBuddy
    for drive in drive_letters() {
        for sub in [
            "Programs\\WorkBuddy",
            "WorkBuddy",
            "Program Files\\WorkBuddy",
            "Program Files (x86)\\WorkBuddy",
            "Apps\\WorkBuddy",
            "Applications\\WorkBuddy",
            "Software\\WorkBuddy",
        ] {
            out.push(PathBuf::from(format!("{drive}:\\{sub}")));
        }
    }
    // 去重(注册表与环境变量经常指向同一个目录),保持顺序
    let mut seen: Vec<String> = Vec::new();
    out.retain(|p| {
        let k = p.to_string_lossy().to_lowercase();
        if seen.iter().any(|s| s == &k) {
            false
        } else {
            seen.push(k);
            true
        }
    });
    out
}

/// 从注册表找桌面端装在哪。这是**唯一不靠猜**的来源:官方安装器会把位置写进
/// App Paths 与卸载项,自定义安装目录 / 非 C 盘都会如实记在那儿。
///
/// 三处都看(顺序即优先级):
///   ① `HKCU\...\App Paths\WorkBuddy.exe` —— per-user 安装,值就是 exe 全路径;
///   ② `HKLM\SOFTWARE\...\App Paths\WorkBuddy.exe` —— 机器级安装;
///   ③ `HKLM/HKCU\...\Uninstall\*` 的 `InstallLocation` / `DisplayIcon` —— 兜底。
#[cfg(windows)]
fn registry_app_roots() -> Vec<PathBuf> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::RegKey;

    let mut out: Vec<PathBuf> = Vec::new();
    let names = ["WorkBuddy.exe", "WorkBuddyAI.exe", "CodeBuddy.exe"];
    let app_paths = r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths";

    let push_exe = |p: PathBuf, out: &mut Vec<PathBuf>| {
        if p.is_file() {
            if let Some(dir) = p.parent() {
                out.push(dir.to_path_buf());
            }
        }
    };
    let push_dir = |p: PathBuf, out: &mut Vec<PathBuf>| {
        if p.is_dir() {
            out.push(p);
        }
    };

    for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        // ① / ② App Paths
        for sub in [app_paths.to_string(), format!(r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\App Paths")] {
            let Ok(key) = RegKey::predef(hive).open_subkey_with_flags(&sub, KEY_READ) else {
                continue;
            };
            for name in names {
                let Ok(exe) = key.open_subkey_with_flags(name, KEY_READ) else {
                    continue;
                };
                if let Ok(path) = exe.get_value::<String, _>("") {
                    push_exe(PathBuf::from(path.trim().trim_matches('"')), &mut out);
                }
            }
        }
        // ③ 卸载项(找 InstallLocation / DisplayIcon)
        for sub in [
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
        ] {
            let Ok(un) = RegKey::predef(hive).open_subkey_with_flags(sub, KEY_READ) else {
                continue;
            };
            for item in un.enum_keys().flatten() {
                let Ok(k) = un.open_subkey_with_flags(&item, KEY_READ) else {
                    continue;
                };
                let name = k
                    .get_value::<String, _>("DisplayName")
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if !(name.contains("workbuddy") || name.contains("codebuddy")) {
                    continue;
                }
                if let Ok(loc) = k.get_value::<String, _>("InstallLocation") {
                    push_dir(PathBuf::from(loc.trim().trim_matches('"')), &mut out);
                }
                // DisplayIcon 形如 `D:\Apps\WorkBuddy\WorkBuddy.exe,0`
                if let Ok(icon) = k.get_value::<String, _>("DisplayIcon") {
                    if let Some(p) = parse_display_icon(&icon) {
                        push_exe(p, &mut out);
                    }
                }
            }
        }
    }
    out
}

/// 从卸载项的 `DisplayIcon` 里取出 exe 路径。
/// 真机形态:`C:\Program Files\WorkBuddy\WorkBuddy.exe,0`(逗号后是图标序号);
/// 也有只给 `.ico` 的(那种不是主程序,返回 None)。**纯函数**,便于钉住格式。
pub(super) fn parse_display_icon(raw: &str) -> Option<PathBuf> {
    // 先按逗号切图标序号。注意路径里本身可能带逗号,所以只在"末尾像序号"时才切。
    let s = raw.trim().trim_matches('"').trim();
    let s = match s.rsplit_once(',') {
        Some((head, tail)) if tail.trim().parse::<i32>().is_ok() => head.trim().trim_matches('"'),
        _ => s,
    };
    // 只认 .exe:`.ico` / `.png` 是图标文件,不是桌面端本体
    let p = PathBuf::from(s);
    let is_exe = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("exe"))
        .unwrap_or(false);
    (is_exe && !s.is_empty()).then_some(p)
}

/// 非 Windows 没有注册表这一说。
#[cfg(not(windows))]
fn registry_app_roots() -> Vec<PathBuf> {
    Vec::new()
}

/// 找桌面端可执行文件。探测链与结果会**原样写进诊断日志** —— 用户报"取不到数"时,
/// 第一件事就是看这条链走到哪儿断了。
///
/// 顺序:
///   ① `WORKBUDDY_APP_EXECUTABLE`(显式指定,最高优先;用户自己改过目录时的逃生门)
///   ② 注册表(App Paths / 卸载项,任意盘任意目录)
///   ③ 环境变量所指的默认目录
///   ④ 每个盘符 × 常见目录名
pub fn find_app_exe() -> Option<PathBuf> {
    // ① 显式覆盖
    if let Some(v) = std::env::var_os("WORKBUDDY_APP_EXECUTABLE") {
        let p = PathBuf::from(v);
        // 允许给目录:给了目录就在里面找主程序名
        let hit = if p.is_file() {
            Some(p.clone())
        } else if p.is_dir() {
            EXE_NAMES.iter().map(|n| p.join(n)).find(|c| c.is_file())
        } else {
            None
        };
        if let Some(hit) = hit {
            crate::diaglog::write_keyed(
                "wb",
                "exe-env",
                &format!("桌面端 exe 来自 WORKBUDDY_APP_EXECUTABLE:{}", hit.display()),
            );
            return Some(hit);
        }
        crate::diaglog::write_keyed(
            "wb",
            "exe-env",
            &format!(
                "WORKBUDDY_APP_EXECUTABLE 已设置但不可用({})—— 继续按默认链探测",
                p.display()
            ),
        );
    }

    let roots = app_roots();
    for r in &roots {
        for name in EXE_NAMES {
            let p = r.join(name);
            if p.is_file() {
                crate::diaglog::write_keyed(
                    "wb",
                    "exe-hit",
                    &format!(
                        "找到桌面端:{} · 命中目录 {}",
                        p.display(),
                        r.display()
                    ),
                );
                return Some(p);
            }
        }
    }
    // 一个都没找到:**把整条链记下来**,否则只能远程让用户猜
    let shown: Vec<String> = roots.iter().take(40).map(|p| p.display().to_string()).collect();
    crate::diaglog::write_keyed(
        "wb",
        "exe-miss",
        &format!(
            "没找到桌面端 exe(主程序名 {:?} · 盘符 {:?} · 探了 {} 个目录){}",
            EXE_NAMES,
            drive_letters(),
            roots.len(),
            if shown.is_empty() {
                String::new()
            } else {
                format!(":{}", shown.join(" | "))
            }
        ),
    );
    None
}
