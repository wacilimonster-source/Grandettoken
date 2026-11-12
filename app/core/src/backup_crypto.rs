//! 写入客户端配置时的**备份快照加密**。
//!
//! 背景:回滚要靠备份,而被备份的那些客户端配置文件里的 API Key 本来就是明文,
//! 于是 `backup-inject/<时间戳>/` 下每次写入都会留下一份含明文密钥的历史快照。
//! 2026-11-12 用户裁决:这些快照必须加密。
//!
//! 做法:每个备份文件写成一个容器 —— `MAGIC + DPAPI 密文`,密钥由当前 Windows 账户
//! 托管(不落盘、不需要用户记密码、不需要额外凭据条目)。同一账户下才能解开,
//! 这正好与「本机回滚」的场景重合;换账户 / 重装系统后旧快照本来就随用户目录一起失效。
//! 用固定的 `ENTROPY` 做域隔离,别的程序拿同一个账户调 DPAPI 也解不开我们的备份。
//!
//! **向后兼容**:升级前已经落盘的快照是明文(没有 MAGIC)。`read_plain` 按文件头判断,
//! 没有 MAGIC 就原样返回 —— 老用户升级后仍能回滚旧快照。每次新建备份时会顺手把
//! 同目录下的存量明文快照原地加密(见 `encrypt_legacy_backups`),不让明文继续躺着。
//!
//! 非 Windows 上没有 DPAPI:加密一律 **fail closed**(返回错误,写入中止),
//! 解密旧明文仍可用。本项目的写入链路本来就只面向 Windows 客户端。

use std::path::Path;

/// 加密容器的文件头。8 字节,固定不变;读到它就当密文,否则当旧版明文。
/// 选这个前缀是因为被备份的文件是 JSON / YAML / SQLite(文件头 `SQLite format 3\0`),
/// 不会与它撞车;另加最小长度校验,避免极小的巧合文件被误判。
pub const MAGIC: &[u8; 8] = b"GTBKv1\0\0";

/// DPAPI 的额外熵:把「本应用的备份」与其它程序的 DPAPI 数据隔离开。
/// 改这个值 = 旧快照全部解不开,**不许改**。
const ENTROPY: &[u8] = b"grandettoken/inject-backup/v1";

pub fn is_sealed(raw: &[u8]) -> bool {
    raw.len() > MAGIC.len() + 32 && raw.starts_with(MAGIC)
}

/// 加密一份明文,返回可直接落盘的容器字节。
#[cfg(windows)]
pub fn seal(plain: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&dpapi_protect_with(plain, Some(ENTROPY))?);
    Ok(out)
}

#[cfg(not(windows))]
pub fn seal(_plain: &[u8]) -> Result<Vec<u8>, String> {
    Err("备份加密依赖 Windows DPAPI,当前平台不可用 —— 为避免留下明文快照,写入已中止".into())
}

/// 解开容器;不是容器 / 被篡改 / 换了账户,一律如实报错。
#[cfg(windows)]
pub fn unseal(raw: &[u8]) -> Result<Vec<u8>, String> {
    if !is_sealed(raw) {
        return Err("不是加密备份(文件头不对或已损坏)".into());
    }
    dpapi_unprotect_with(&raw[MAGIC.len()..], Some(ENTROPY))
}

#[cfg(not(windows))]
pub fn unseal(_raw: &[u8]) -> Result<Vec<u8>, String> {
    Err("解密备份依赖 Windows DPAPI,当前平台不可用".into())
}

/// 读一份备份文件,返回**明文**。加密的新格式解密,旧的明文格式原样返回(兼容)。
pub fn read_plain(path: &Path) -> Result<Vec<u8>, String> {
    let raw = std::fs::read(path).map_err(|e| format!("读备份文件失败: {e}"))?;
    if is_sealed(&raw) {
        unseal(&raw)
    } else {
        Ok(raw)
    }
}

/// 把明文加密后原子替换到目标路径。失败时目标保持原样,不留半截文件。
pub fn write_sealed(path: &Path, plain: &[u8]) -> Result<(), String> {
    let sealed = seal(plain)?;
    atomic_write(path, &sealed)
}

/// 存量迁移:把一份还是明文的备份文件原地换成加密容器。
/// 返回 `Ok(true)` = 确实改了,`Ok(false)` = 本来就是密文(跳过)。
pub fn encrypt_in_place(path: &Path) -> Result<bool, String> {
    let raw = std::fs::read(path).map_err(|e| format!("读旧备份失败: {e}"))?;
    if is_sealed(&raw) {
        return Ok(false);
    }
    // 先算密文再落盘:seal 失败(例如换了账户)时原文件一个字节都不动。
    let sealed = seal(&raw)?;
    atomic_write(path, &sealed)?;
    Ok(true)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!("{name}.enc-tmp"));
    std::fs::write(&tmp, bytes).map_err(|e| format!("写加密备份临时文件失败: {e}"))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("替换备份文件失败: {e}"));
    }
    Ok(())
}

// ─────────────────────────── DPAPI 薄封装 ───────────────────────────
//
// 这里是全项目唯一直接调 Win32 加解密的地方:qoder.rs 解 Electron safeStorage
// 的 os_crypt key 也走下面的 `unprotect_raw`(它不带熵,与 Electron 的口径一致)。

/// 解不带熵的 DPAPI 数据(给 qoder.rs 的 `os_crypt.encrypted_key` 用)。
#[cfg(windows)]
pub fn unprotect_raw(data: &[u8]) -> Result<Vec<u8>, String> {
    dpapi_unprotect_with(data, None)
}

#[cfg(windows)]
fn dpapi_protect_with(data: &[u8], entropy: Option<&[u8]>) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};

    let mut input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut ent = entropy.map(|e| CRYPT_INTEGER_BLOB {
        cbData: e.len() as u32,
        pbData: e.as_ptr() as *mut u8,
    });
    let ent_ptr = ent
        .as_mut()
        .map(|b| b as *mut CRYPT_INTEGER_BLOB as *const CRYPT_INTEGER_BLOB)
        .unwrap_or(std::ptr::null());
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY:三个 blob 都在本栈上且调用期间存活;成功时 output.pbData 由系统分配,
    // 下面立刻拷成 Vec 并用 LocalFree 归还。不传描述串 / prompt,不会弹 UI。
    let ok = unsafe {
        CryptProtectData(
            &mut input,
            std::ptr::null(),
            ent_ptr,
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut output,
        )
    };
    if ok == 0 {
        return Err("DPAPI 加密失败(当前 Windows 账户不可用)".into());
    }
    let out = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        LocalFree(output.pbData as *mut core::ffi::c_void);
    }
    Ok(out)
}

#[cfg(windows)]
fn dpapi_unprotect_with(data: &[u8], entropy: Option<&[u8]>) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    if data.is_empty() {
        return Err("DPAPI 密文是空的".into());
    }
    let mut input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut ent = entropy.map(|e| CRYPT_INTEGER_BLOB {
        cbData: e.len() as u32,
        pbData: e.as_ptr() as *mut u8,
    });
    let ent_ptr = ent
        .as_mut()
        .map(|b| b as *mut CRYPT_INTEGER_BLOB as *const CRYPT_INTEGER_BLOB)
        .unwrap_or(std::ptr::null());
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY:同上;失败时不碰 output.pbData(系统未分配)。
    let ok = unsafe {
        CryptUnprotectData(
            &mut input,
            std::ptr::null_mut(),
            ent_ptr,
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut output,
        )
    };
    if ok == 0 {
        // 最常见成因是「换了 Windows 账户 / 换了机器」或密文被改过,不猜更细。
        return Err("DPAPI 拒绝解密(备份不属于当前 Windows 账户,或文件已损坏)".into());
    }
    let out = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        LocalFree(output.pbData as *mut core::ffi::c_void);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join("gt-backup-crypto").join(name);
        std::fs::create_dir_all(&d).unwrap();
        d.join("snapshot.json")
    }

    #[test]
    fn plain_text_is_not_mistaken_for_a_container() {
        // SQLite / JSON / YAML 这三种真实备份形态都不能被误判成密文。
        for raw in [
            b"SQLite format 3\0".to_vec(),
            br#"{"apiKey":"sk-real-looking-value"}"#.to_vec(),
            b"providers:\n  apiKey: sk-abc\n".to_vec(),
        ] {
            assert!(!is_sealed(&raw));
        }
        assert!(!is_sealed(MAGIC)); // 只有头、没有密文 = 损坏,不能当密文
    }

    #[cfg(windows)]
    #[test]
    fn sealed_backup_roundtrips_and_contains_no_plaintext() {
        // 故意用短值:别让凭据扫描器把这个夹具当成真密钥(它要求 sk- 后 >= 20 字符)。
        let plain = br#"{"apiKey":"sk-TESTVALUE"}"#.to_vec();
        let sealed = seal(&plain).unwrap();
        assert!(is_sealed(&sealed));
        assert!(!sealed
            .windows(plain.len())
            .any(|w| w == plain.as_slice()), "密文里绝不能出现明文片段");
        assert_eq!(unseal(&sealed).unwrap(), plain);

        let p = tmp("roundtrip");
        write_sealed(&p, &plain).unwrap();
        assert_eq!(read_plain(&p).unwrap(), plain, "落盘后仍能还原");
        assert!(!std::fs::read(&p).unwrap().windows(plain.len()).any(|w| w == plain.as_slice()));
    }

    #[cfg(windows)]
    #[test]
    fn tampered_ciphertext_is_rejected() {
        let sealed = seal(b"hello").unwrap();
        let mut bad = sealed.clone();
        let i = bad.len() - 1;
        bad[i] ^= 0x01;
        assert!(unseal(&bad).is_err(), "被改过的备份必须拒绝恢复,不能悄悄还原成乱码");
    }

    #[test]
    fn legacy_plaintext_backup_still_reads() {
        let p = tmp("legacy");
        let plain = b"old plaintext backup with sk-key";
        std::fs::write(&p, plain).unwrap();
        assert_eq!(read_plain(&p).unwrap(), plain.to_vec(), "升级前的明文快照仍可回滚");
    }

    #[cfg(windows)]
    #[test]
    fn encrypt_in_place_migrates_a_legacy_file() {
        let p = tmp("migrate");
        let plain = br#"{"apiKey":"sk-legacy-key"}"#;
        std::fs::write(&p, plain).unwrap();
        assert!(encrypt_in_place(&p).unwrap(), "明文文件应该被改写");
        assert!(!encrypt_in_place(&p).unwrap(), "已经是密文就该跳过");
        assert_eq!(read_plain(&p).unwrap(), plain.to_vec());
        assert!(!std::fs::read(&p).unwrap().windows(plain.len()).any(|w| w == plain));
    }
}
