//! 豆包桌面客户端版本探测 —— 指纹保鲜的事实源。
//!
//! **为什么必须运行时探测**:豆包活动接口按客户端形状放行,2026-10-24/25 起服务端
//! 收紧,钉死的 `version_code=20800` 被拒(业务码 710010702「系统错误」;复盘见
//! docs/report/doubao-checkin-710010702-2026-11-14.md,自动签到因此随 v0.3.47 移除)。
//! 凡是与客户端版本联动的参数,写死必然腐烂 —— 本机安装的真实客户端就是唯一可信的
//! 当前值;取数(detail)带着同一份指纹,不能等服务端连读路径也收紧时才补。
//!
//! 口径(与 SignDock 2026-10-27 的公开复现一致):
//! - `version_code = major*10_000 + minor*100 + patch`(2.36.15 → 23615;
//!   2.32.6 → 23206 也实测通过过,编码规则是同一条)
//! - UA 家族保持 `DoubaoDesktop/{version} Chrome/{kernel}`,只换版本段;
//!   Chrome 内核读客户端自己的 `User Data/Last Version`(Chromium 惯例文件)
//! - `doubao_pc_version` 的派生来源未知,维持抓包钉值(它不在被校验的证据链里)
//!
//! 探测失败(没装客户端 / PE 版本资源解析不出)回落到 [`FALLBACK_*`] ——
//! 2026-10-19 的真机抓包值,**大概率已被服务端拒绝**;回落只是让请求形状
//! 仍是合法的,不代表能过。走了哪条路写进诊断日志,一次进程只记一行。
//!
//! 布局口径:豆包桌面客户端装在 `%LOCALAPPDATA%\{Doubao,DoubaoWork}\Application\`,
//! User Data 与 Application 同级。原版优先于工作版(签到活动挂在个人账号)。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 兜底版本(2026-10-19 CDP 抓包;仅在找不到本机客户端时使用)。
pub const FALLBACK_VERSION: &str = "2.32.6";
/// 兜底 version_code(与 FALLBACK_VERSION 同源;见模块注释——大概率已被拒)。
pub const FALLBACK_VERSION_CODE: &str = "20800";
/// 兜底 Chrome 内核(与 FALLBACK_VERSION 同源)。
pub const FALLBACK_CHROME: &str = "147.0.7727.149";
/// 兜底 UA 完整串(表格占位值;`ua()` 按 parts 拼出的必须与它逐字节一致,有测试钉住)。
pub const FALLBACK_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) DoubaoDesktop/2.32.6 Chrome/147.0.7727.149 Safari/537.36";

/// 一次探测的结果。字段都是拼好的字符串,调用方直接塞进 URL / 头。
/// `ua` 在探测时拼好 —— 调用方(doubao::app_ua)要 `&'static str`,不能现拼。
#[derive(Debug, Clone, PartialEq)]
pub struct ClientFingerprint {
    /// 客户端版本,如 "2.36.15"(UA 与 pc_version 用)。
    pub version: String,
    /// 客户端构建码,如 "23615"(URL 查询串的 version_code 用)。
    pub version_code: String,
    /// Chrome 内核版本,如 "147.0.7727.149"(UA 用)。
    pub chrome: String,
    /// 拼好的桌面版 webview UA(与抓包原样同构,只换版本段)。
    pub ua: String,
    /// 事实源说明(诊断日志用,不进请求)。
    pub source: String,
}

/// 桌面版 webview 形状的 UA 模板(与抓包原样同构,只换版本段)。
fn build_ua(version: &str, chrome: &str) -> String {
    format!(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
         DoubaoDesktop/{v} Chrome/{c} Safari/537.36",
        v = version,
        c = chrome,
    )
}

impl ClientFingerprint {
    fn fallback() -> Self {
        Self {
            version: FALLBACK_VERSION.into(),
            version_code: FALLBACK_VERSION_CODE.into(),
            chrome: FALLBACK_CHROME.into(),
            ua: FALLBACK_UA.into(),
            source: "兜底钉值(2026-10-19 抓包;未找到本机客户端)".into(),
        }
    }
}

/// 探测到的当前指纹(进程内缓存;首次调用写一行诊断日志)。
pub fn detect() -> &'static ClientFingerprint {
    static DETECTED: OnceLock<ClientFingerprint> = OnceLock::new();
    DETECTED.get_or_init(|| match probe() {
        Some(fp) => {
            crate::diaglog::write_keyed(
                "doubao",
                "client",
                &format!("客户端指纹 = {} ({})", fp.ua, fp.source),
            );
            fp
        }
        None => {
            crate::diaglog::write_keyed(
                "doubao",
                "client",
                &format!(
                    "未找到本机豆包客户端,回落钉值 {} —— 该值已过期,活动接口可能拒绝请求",
                    FALLBACK_VERSION_CODE
                ),
            );
            ClientFingerprint::fallback()
        }
    })
}

/// 扫候选安装位,取第一份解析成功的。
fn probe() -> Option<ClientFingerprint> {
    for (exe, root) in candidates() {
        if let Some(fp) = probe_at(&exe, &root) {
            return Some(fp);
        }
    }
    None
}

/// 从一份 exe + 产品根目录(Application / User Data 的父目录)探测指纹。
/// 路径可注入,单测不用碰真实安装。
fn probe_at(exe: &Path, root: &Path) -> Option<ClientFingerprint> {
    let (major, minor, patch) = file_version_of(exe)?;
    let version = version_triple(major, minor, patch)?;
    let version_code = encode_version_code(&version)?;
    let chrome = chrome_of(root).unwrap_or_else(|| FALLBACK_CHROME.into());
    let ua = build_ua(&version, &chrome);
    Some(ClientFingerprint {
        version,
        version_code,
        chrome,
        ua,
        source: format!("本机客户端 {}", exe.display()),
    })
}

/// 候选安装位:原版豆包优先,其次豆包工作版。全部来自环境变量,不写盘符。
fn candidates() -> Vec<(PathBuf, PathBuf)> {
    let mut out = Vec::new();
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let pf = std::env::var_os("ProgramFiles").map(PathBuf::from);
    let pf86 = std::env::var_os("ProgramFiles(x86)").map(PathBuf::from);
    for base in [local, pf, pf86].into_iter().flatten() {
        // (exe, 产品根目录 —— User Data 所在的那一层)
        for name in ["Doubao", "DoubaoWork"] {
            out.push((
                base.join(name).join("Application").join("app").join(exe_name(name)),
                base.join(name),
            ));
            out.push((
                base.join(name).join("Application").join(exe_name(name)),
                base.join(name),
            ));
        }
    }
    out
}

fn exe_name(product: &str) -> &'static str {
    if product == "Doubao" {
        "Doubao.exe"
    } else {
        "DoubaoWork.exe"
    }
}

/// Chrome 内核版本:客户端 User Data/Last Version(Chromium 惯例,一行纯文本)。
fn chrome_of(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("User Data").join("Last Version")).ok()?;
    let v = text.trim();
    // 形状门:dotted digits,别把空文件/乱码塞进 UA
    let ok = !v.is_empty()
        && v.split('.').all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        && v.split('.').count() >= 3;
    ok.then(|| v.to_string())
}

/// `(major, minor, patch)` → "2.36.15"。数字段必须落得进编码规则,否则放弃。
fn version_triple(major: u32, minor: u32, patch: u32) -> Option<String> {
    (minor <= 99 && patch <= 99 && major > 0).then(|| format!("{major}.{minor}.{patch}"))
}

/// `2.36.15 → "23615"`。minor/patch 超出两位时编码有歧义(2.326.1 与 2.32.61 同码),
/// 与 SignDock 的处置一致:宁可不发,不发一个会被风控盯上的畸形指纹。
fn encode_version_code(version: &str) -> Option<String> {
    let parts: Vec<u32> = version
        .split('.')
        .map(|s| s.parse::<u32>().ok())
        .collect::<Option<_>>()?;
    if parts.len() != 3 {
        return None;
    }
    let (major, minor, patch) = (parts[0], parts[1], parts[2]);
    (minor <= 99 && patch <= 99).then(|| (major * 10_000 + minor * 100 + patch).to_string())
}

// ───────────── Windows PE 版本资源(系统 version.dll,零新依赖)─────────────

/// 读 PE 的 VS_FIXEDFILEINFO,返回 `(major, minor, patch)`。
/// build 段非 0 时版本四段有歧义,返回 None(走回落)。
#[cfg(windows)]
fn file_version_of(path: &Path) -> Option<(u32, u32, u32)> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let size = ffi::GetFileVersionInfoSizeW(wide.as_ptr(), std::ptr::null_mut());
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        if ffi::GetFileVersionInfoW(wide.as_ptr(), 0, size, buf.as_mut_ptr() as *mut _) == 0 {
            return None;
        }
        let mut block: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut len: u32 = 0;
        // VS_FIXEDFILEINFO 的子块名是 "\\"
        let sub: Vec<u16> = [0x5C, 0x5C, 0].into_iter().collect();
        if ffi::VerQueryValueW(
            buf.as_ptr() as *const _,
            sub.as_ptr(),
            &mut block,
            &mut len,
        ) == 0
            || block.is_null()
            || (len as usize) < std::mem::size_of::<ffi::VsFixedFileInfo>()
        {
            return None;
        }
        let info = &*(block as *const ffi::VsFixedFileInfo);
        if info.signature != 0xFEEF_04BD {
            return None;
        }
        let major = info.file_ms >> 16;
        let minor = info.file_ms & 0xFFFF;
        let patch = info.file_ls >> 16;
        let build = info.file_ls & 0xFFFF;
        (build == 0).then(|| (major, minor, patch))
    }
}

#[cfg(not(windows))]
fn file_version_of(_path: &Path) -> Option<(u32, u32, u32)> {
    None
}

#[cfg(windows)]
mod ffi {
    use std::ffi::c_void;

    /// winver.h 的 VS_FIXEDFILEINFO(只取用到的字段段,布局必须完整)。
    #[repr(C)]
    pub struct VsFixedFileInfo {
        pub signature: u32,
        pub struc_version: u32,
        pub file_ms: u32,
        pub file_ls: u32,
        pub product_ms: u32,
        pub product_ls: u32,
        pub flags_mask: u32,
        pub flags: u32,
        pub os: u32,
        pub file_type: u32,
        pub file_subtype: u32,
        pub date_ms: u32,
        pub date_ls: u32,
    }

    #[link(name = "version")]
    extern "system" {
        pub fn GetFileVersionInfoSizeW(file: *const u16, handle: *mut u32) -> u32;
        pub fn GetFileVersionInfoW(file: *const u16, handle: u32, len: u32, data: *mut c_void) -> i32;
        pub fn VerQueryValueW(
            block: *const c_void,
            sub: *const u16,
            buffer: *mut *mut c_void,
            len: *mut u32,
        ) -> i32;
    }
}

// ───────────── 测试 ─────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 编码规则与 SignDock 的公开复现同一条:2.32.6 → 23206(实机过),2.36.15 → 23615。
    #[test]
    fn version_code_encoding() {
        assert_eq!(encode_version_code("2.32.6").as_deref(), Some("23206"));
        assert_eq!(encode_version_code("2.36.15").as_deref(), Some("23615"));
        assert_eq!(encode_version_code("3.0.0").as_deref(), Some("30000"));
    }

    /// 畸形版本拒收:四段、越界段、非数字 —— 宁走兜底,不发畸形指纹。
    #[test]
    fn version_code_rejects_malformed() {
        assert_eq!(encode_version_code("2.36"), None);
        assert_eq!(encode_version_code("2.36.15.7"), None);
        assert_eq!(encode_version_code("2.326.1"), None);
        assert_eq!(encode_version_code("2.361.5"), None);
        assert_eq!(encode_version_code("x.y.z"), None);
        assert_eq!(encode_version_code(""), None);
        assert_eq!(version_triple(2, 326, 1), None);
        assert_eq!(version_triple(0, 36, 15), None);
    }

    /// 兜底指纹拼出的 UA 与抓包钉值逐字节一致 —— 表格占位与兜底是同一份事实。
    #[test]
    fn fallback_ua_matches_pinned_const() {
        let fp = ClientFingerprint::fallback();
        assert_eq!(fp.ua, FALLBACK_UA);
        assert_eq!(fp.version_code, FALLBACK_VERSION_CODE);
        assert_eq!(fp.version, FALLBACK_VERSION);
        assert_eq!(fp.chrome, FALLBACK_CHROME);
    }

    /// UA 模板:版本段替换,其余逐字节保持抓包形状。
    #[test]
    fn build_ua_swaps_version_segments() {
        assert_eq!(
            build_ua("2.36.15", "158.0.1.2"),
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) DoubaoDesktop/2.36.15 Chrome/158.0.1.2 Safari/537.36"
        );
    }

    /// Chrome 内核的形状门:Last Version 是 dotted digits,别的不要。
    #[test]
    fn chrome_shape_gate() {
        let dir = std::env::temp_dir().join(format!("doubao-client-test-{}", std::process::id()));
        let ud = dir.join("User Data");
        std::fs::create_dir_all(&ud).unwrap();
        let write = |s: &str| std::fs::write(ud.join("Last Version"), s).unwrap();

        write("147.0.7727.149\n");
        assert_eq!(chrome_of(&dir).as_deref(), Some("147.0.7727.149"));
        write("158.0.1.2");
        assert_eq!(chrome_of(&dir).as_deref(), Some("158.0.1.2"));
        write("");
        assert_eq!(chrome_of(&dir), None);
        write("not-a-version");
        assert_eq!(chrome_of(&dir), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// probe_at 对不存在的 exe 返回 None(走回落,不 panic)。
    #[test]
    fn probe_missing_exe_is_none() {
        let ghost = std::env::temp_dir().join("no-such-doubao.exe");
        assert_eq!(probe_at(&ghost, Path::new("no-such-root")), None);
    }
}
