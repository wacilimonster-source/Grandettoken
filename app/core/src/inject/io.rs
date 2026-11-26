use super::*;

// ───────────────────────── 文件读写与备份 ─────────────────────────

/// 读 JSON 文件。返回 None = 文件不存在或内容为空(空文件是合法初态,不是错误)。
pub(super) fn read_json(path: &Path) -> Result<Option<Value>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("读 {} 失败: {e}", pretty(path))),
    };
    // 带 BOM 的 JSON(`\u{feff}`)serde_json 直接解析会报错,先剥掉
    let text = raw.trim_start_matches('\u{feff}').trim();
    if text.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(text)
        .map(Some)
        .map_err(|e| format!("解析 {} 失败(结构可能已变,本次不写): {e}", pretty(path)))
}

/// 写 JSON 文件:先写同目录临时文件再原子替换 —— 半截文件比不写更糟。
/// `bom` / `indent` 按原文件的习惯来(WorkBuddy 的 models.json 带 BOM、2 空格缩进)。
pub(super) fn write_json(path: &Path, v: &Value, bom: bool, indent: usize) -> Result<(), String> {
    let mut text = if indent == 0 {
        serde_json::to_string(v).map_err(|e| e.to_string())?
    } else {
        let pad = " ".repeat(indent);
        let mut buf = Vec::new();
        let fmt = serde_json::ser::PrettyFormatter::with_indent(pad.as_bytes());
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
        serde::Serialize::serialize(v, &mut ser).map_err(|e| e.to_string())?;
        String::from_utf8(buf).map_err(|e| e.to_string())?
    };
    if bom {
        text.insert(0, '\u{feff}');
    }
    // with_extension 是"换扩展名"的语义,对 state.vscdb 这种多段名会生成完全无关的名字;
    // 直接按文件名拼更稳(报告 P3-4)。
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| format!("文件名非法: {}", pretty(path)))?;
    let tmp = path.with_file_name(format!("{name}.tmp-inject"));
    std::fs::write(&tmp, text.as_bytes()).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("写临时文件失败:{} {}", os(&e), pretty(&tmp))
    })?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!(
            "替换 {} 失败:{e} {}(客户端在跑就先退出它)",
            pretty(path),
            os_hint(&e)
        )
    })
}

/// 把 `patch` 的键逐个并入 `obj[key]`:该键本来就是对象就逐键覆盖(其余键原样留着),
/// 不是对象或缺失就整体写入。
pub(super) fn merge_into(obj: &mut Map<String, Value>, key: &str, patch: Value) {
    let was_object = obj.get(key).map(|v| v.is_object()).unwrap_or(false);
    if !was_object {
        obj.insert(key.into(), patch);
        return;
    }
    let Some(p) = patch.as_object() else {
        obj.insert(key.into(), patch);
        return;
    };
    if let Some(Value::Object(have)) = obj.get_mut(key) {
        for (k, v) in p {
            have.insert(k.clone(), v.clone());
        }
    }
}

/// 原文件是不是带 BOM 的(照它的习惯写回去,免得下游 diff 里多出一行变化)。
pub(super) fn has_bom(path: &Path) -> bool {
    // 只需要前 3 个字节;一次写入里同一份文件以前被整读三遍(解析、探 BOM、校验),
    // 100 KB 级的 models.json 上能感到顿(报告 P3-7)
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 3];
    match f.read_exact(&mut head) {
        Ok(()) => head == [0xEF, 0xBB, 0xBF],
        Err(_) => false,
    }
}

/// 把 Windows 的错误码翻成能行动的话(报告 P3-4:以前只有 `os error 5/32`,
/// 用户分不清是权限、被占用还是路径过长)。
pub(super) fn os_hint(e: &std::io::Error) -> &'static str {
    match e.raw_os_error() {
        Some(5) => "(没有写权限:文件可能是只读,或目录不可写)",
        Some(32) | Some(33) => "(文件正被别的程序占用)",
        Some(3) => "(路径不存在)",
        Some(206) | Some(260) => "(路径过长)",
        _ => "",
    }
}

pub(super) fn os(e: &std::io::Error) -> String {
    format!("{e} {}", os_hint(e)).trim().to_string()
}

/// 备份要改的文件到 `<root>/backup-inject/<stamp>/`,**每份都是加密容器**。返回备份目录。
pub(super) fn backup(root: &Path, files: &[PathBuf], stamp: &str) -> Result<PathBuf, String> {
    let dir = fresh_backup_dir(root, stamp)?;
    for f in files {
        if !f.exists() {
            continue;
        }
        let name = f
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("文件名非法: {}", pretty(f)))?;
        let plain = std::fs::read(f).map_err(|e| format!("读 {name} 失败: {e}"))?;
        // **fail closed**:加密不成功就让这次写入中止 —— 宁可不写,也不留一份明文快照。
        crate::backup_crypto::write_sealed(&dir.join(name), &plain)
            .map_err(|e| format!("加密备份 {name} 失败: {e}"))?;
    }
    // 升级前落盘的明文快照顺手加密掉,别让它们继续留在磁盘上(详见函数注释)。
    encrypt_legacy_backups(root);
    prune_backups(root);
    Ok(dir)
}

/// 备份目录里的**落点清单**:备份文件名 → 相对 root 的落点。
/// 只有落点跨目录的客户端(DSH 的 patch 与凭据文件不在同一层)才写这份清单;
/// 其它四家的备份目录与以前逐字节一样(回滚走"按文件名放回 root"的老路)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct BackupEntry {
    /// 备份目录里的文件名(相对路径压平而来)
    pub(super) copy: String,
    /// 相对 `root` 的落点(不含 `..`,不会是绝对路径)
    pub(super) rel: String,
    /// 备份时它本来就在吗?不在 = 是我们新建的,回滚要把它删掉而不是留着
    pub(super) existed: bool,
}

pub(super) const BACKUP_LAYOUT: &str = ".inject-layout.json";

/// 备份目录名:同一秒内连点两次「写入」时不能撞车(撞了第二次会把**已改过**的文件
/// 盖进同一个目录,回滚就回到错误的时间点)。
pub(super) fn fresh_backup_dir(root: &Path, stamp: &str) -> Result<PathBuf, String> {
    let mut dir = root.join("backup-inject").join(stamp);
    let mut n = 2u32;
    while dir.exists() {
        dir = root.join("backup-inject").join(format!("{stamp}-{n}"));
        n += 1;
        if n > 99 {
            return Err(format!("备份目录名重复太多次:{}", pretty(&dir)));
        }
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("建备份目录失败: {e}"))?;
    Ok(dir)
}

/// 带落点清单的备份(DSH 用):文件名按相对路径压平,`restore` 照清单放回去。
pub(super) fn backup_layered(
    root: &Path,
    files: &[PathBuf],
    rels: &[String],
    stamp: &str,
) -> Result<PathBuf, String> {
    if files.len() != rels.len() {
        return Err("备份清单与文件数对不上".into());
    }
    let dir = fresh_backup_dir(root, stamp)?;
    let mut entries: Vec<BackupEntry> = Vec::new();
    for (f, rel) in files.iter().zip(rels.iter()) {
        let copy = rel.replace('/', "__");
        let existed = f.exists();
        if existed {
            let plain = std::fs::read(f).map_err(|e| format!("读 {rel} 失败: {e}"))?;
            crate::backup_crypto::write_sealed(&dir.join(&copy), &plain)
                .map_err(|e| format!("加密备份 {rel} 失败: {e}"))?;
        }
        entries.push(BackupEntry {
            copy,
            rel: rel.clone(),
            existed,
        });
    }
    let json = serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())?;
    // 清单里虽不含密钥(只有落点与 existed),也按同一容器写:格式统一,回滚走同一条 read_plain。
    crate::backup_crypto::write_sealed(&dir.join(BACKUP_LAYOUT), json.as_bytes())
        .map_err(|e| format!("写加密备份清单失败: {e}"))?;
    encrypt_legacy_backups(root);
    prune_backups(root);
    Ok(dir)
}

/// 备份目录名是不是我们生成的那种:`YYYYMMDD-HHMMSS`,或同秒撞车时由 [`backup`]
/// 追加的 `-N`(N = 2..=99)。**其它名字一律不认** ——
/// ① 清理只动我们自己建的目录,用户在 `backup-inject/` 里自建的东西不碰;
/// ② 回滚只认这个形态,拒绝"把随便一个目录里的文件盖到它上一级"。
/// 以前只认 15 位,于是带序号的备份**永远不会被清理**:历史快照无上限地堆着,
/// 与"每个客户端最多 5 份"的承诺相矛盾(P2)。
pub(super) fn is_stamp_name(name: &str) -> bool {
    let b = name.as_bytes();
    if b.len() < 15 {
        return false;
    }
    if !b[..8].iter().all(u8::is_ascii_digit) || b[8] != b'-' {
        return false;
    }
    if !b[9..15].iter().all(u8::is_ascii_digit) {
        return false;
    }
    if b.len() == 15 {
        return true;
    }
    // `<stamp>-N`:只有一个 '-' 加 1~2 位数字(99 封顶,backup 超过就报错不建了)
    if b[15] != b'-' {
        return false;
    }
    let n = &name[16..];
    !n.is_empty()
        && n.len() <= 2
        && n.bytes().all(|c| c.is_ascii_digit())
        && n.parse::<u32>().map(|v| (2..=99).contains(&v)).unwrap_or(false)
}

/// 把升级前落盘的**明文**快照原地加密。
///
/// 2026-11-12 用户裁决「含明文密钥的备份需要加密」:只给新备份加密不够,老快照还躺在
/// 同一个 `backup-inject/` 里。每次新建备份时都跑一遍 —— 已是容器的跳过,其余逐文件
/// 加密后原子替换(读明文 / 回滚都走 `backup_crypto::read_plain`,按文件头自动兼容)。
///
/// 某个目录里有文件加密不了(换了 Windows 账户 / 权限异常)时**整体删掉该目录**并打印
/// 原因:明文快照不能继续留着;此时刚建好的那份加密备份仍是可回滚的最新点,不会让用户
/// 落到「没有回滚点」的处境。只动 `is_stamp_name` 的目录,用户自建的一律不碰。
pub(super) fn encrypt_legacy_backups(root: &Path) {
    let base = root.join("backup-inject");
    let Ok(rd) = std::fs::read_dir(&base) else { return };
    for d in rd.filter_map(|e| e.ok()) {
        let dir = d.path();
        let name = dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if !dir.is_dir() || !is_stamp_name(&name) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&dir) else { continue };
        let mut failed: Option<String> = None;
        for f in files.filter_map(|e| e.ok()) {
            let p = f.path();
            if !p.is_file() || p.to_string_lossy().ends_with(".enc-tmp") {
                continue;
            }
            if let Err(e) = crate::backup_crypto::encrypt_in_place(&p) {
                failed = Some(format!("{}: {e}", pretty(&p)));
                break;
            }
        }
        if let Some(reason) = failed {
            eprintln!("旧明文备份加密失败,已移除该快照({}): {reason}", pretty(&dir));
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// 只留最近 KEEP_BACKUPS 份,免得天天写把用户目录撑大。
///
/// 两条边界(报告 P2-8):
/// ① 只动名字形如 `%Y%m%d-%H%M%S`(含 `-N`)的目录 —— 以前只判 `is_dir()`,用户在
///    `backup-inject/` 里自己建的目录会被当成"最旧的备份"删掉;
/// ② 删除失败不再静默:以前 `let _ =` 一吞,备份就会积到 6 份以上,
///    "别撑大用户目录"的目的落空,而界面上的历史还留着指向已消失目录的记录。
pub(super) fn prune_backups(root: &Path) {
    fn name_of(p: &Path) -> String {
        p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
    }
    let base = root.join("backup-inject");
    let Ok(rd) = std::fs::read_dir(&base) else { return };
    let mut dirs: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir() && is_stamp_name(&name_of(p)))
        .collect();
    if dirs.len() <= KEEP_BACKUPS {
        return;
    }
    dirs.sort(); // 目录名是时间戳,字典序即时间序
    for old in &dirs[..dirs.len() - KEEP_BACKUPS] {
        if let Err(e) = std::fs::remove_dir_all(old) {
            eprintln!("清理旧备份失败({}): {e}", pretty(old));
        }
    }
}

/// 从备份目录回滚:备份目录里的文件按名字放回它的上一级(即配置目录)。
/// 布局 `<root>/backup-inject/<stamp>/<文件>` 是固定的,所以不用额外记账 ——
/// 也正因为固定,这里**必须自己核一遍布局**:调用方的包含性检查只是一次字符串前缀
/// 比较,拼错的/别处的路径不该被当成备份,把它的文件盖到上一级去(P2)。
pub fn restore(backup_dir: &Path) -> Result<String, String> {
    let dir_name = backup_dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let parent_is_backup_inject = backup_dir
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().eq_ignore_ascii_case("backup-inject"))
        .unwrap_or(false);
    if !parent_is_backup_inject {
        return Err(format!(
            "这不是我们的备份目录(上一级不是 backup-inject):{}",
            pretty(backup_dir)
        ));
    }
    if !is_stamp_name(&dir_name) {
        return Err(format!(
            "备份目录名不是时间戳形态,拒绝恢复:{}",
            pretty(backup_dir)
        ));
    }
    let Some(root) = backup_dir.parent().and_then(|p| p.parent()) else {
        return Err("备份目录结构异常".into());
    };
    // 带落点清单的备份(DSH):两份文件不在同一层,按文件名平铺会放错地方 ——
    // 照清单逐份放回去,清单里没有那次不存在过的文件则删掉(那才是它写入前的样子)。
    if backup_dir.join(BACKUP_LAYOUT).is_file() {
        return restore_layered(backup_dir, &root);
    }
    let rd = std::fs::read_dir(backup_dir).map_err(|e| format!("读备份目录失败: {e}"))?;
    let mut n = 0usize;
    let mut bad: Vec<String> = Vec::new();
    for e in rd.filter_map(|e| e.ok()) {
        let name = e.file_name();
        let name_str = name.to_string_lossy().to_string();
        let target = root.join(&name);
        // 逐个文件:先写同目录 temp 再 rename。直接 fs::copy 到已存在目标会**先截断**
        // 再逐块拷,目标被占用时就把客户端自己的配置留成了半截文件(报告 P2-6)。
        let tmp = root.join(format!("{name_str}.restore-tmp"));
        // 备份文件是加密容器(或升级前的明文),统一读成明文后再原子放回。
        // 解密失败(换了 Windows 账户 / 文件损坏)必须进 bad 列表,不能悄悄跳过。
        let outcome: Result<(), String> = crate::backup_crypto::read_plain(&e.path())
            .and_then(|plain| {
                std::fs::write(&tmp, plain).map_err(|e| format!("写临时文件失败: {e}"))
            })
            .and_then(|_| {
                std::fs::rename(&tmp, &target).map_err(|e| format!("替换文件失败: {e}"))
            });
        match outcome {
            Ok(_) => {
                n += 1;
                // SQLite 主库恢复完,旁边若留着**没进这份备份**的 -wal/-shm,SQLite
                // 下次打开会把回滚掉的那次写入重放回刚恢复的库上 —— 回滚等于没做。
                // 备份里没有它,就说明它与这份备份不是一套,删掉。
                if name_str.to_ascii_lowercase().ends_with(".vscdb") {
                    for suffix in ["-wal", "-shm"] {
                        let sib = root.join(format!("{name_str}{suffix}"));
                        if sib.exists() && !backup_dir.join(format!("{name_str}{suffix}")).exists() {
                            if let Err(err) = std::fs::remove_file(&sib) {
                                eprintln!("清理 {} 失败: {err}", pretty(&sib));
                            }
                        }
                    }
                }
            }
            Err(err) => {
                let _ = std::fs::remove_file(&tmp);
                bad.push(format!("{}:{err}", pretty(&target)));
            }
        }
    }
    if n == 0 && bad.is_empty() {
        return Err("备份目录是空的".into());
    }
    if !bad.is_empty() {
        return Err(format!(
            "只恢复了 {n} 个文件,这些没成:{} —— 备份仍在 {},可重试或手动拷回",
            bad.join("; "),
            pretty(backup_dir)
        ));
    }
    Ok(format!("已从备份恢复 {n} 个文件"))
}

pub(super) fn stamp_now() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// 按落点清单回滚:每份文件放回它自己的位置,写入前不存在的删掉。
/// 清单里的 `rel` 只允许是 `root` 以内的相对路径(手改过的清单不能把文件写到别处 ——
/// 这条与"备份目录必须叫 backup-inject"是同一道防线)。
pub(super) fn restore_layered(backup_dir: &Path, root: &Path) -> Result<String, String> {
    // 新清单是加密容器,老清单是明文 JSON —— read_plain 按文件头兼容两者。
    let raw = crate::backup_crypto::read_plain(&backup_dir.join(BACKUP_LAYOUT))
        .map_err(|e| format!("读备份清单失败: {e}"))?;
    let entries: Vec<BackupEntry> =
        serde_json::from_slice(&raw).map_err(|e| format!("备份清单解析失败: {e}"))?;
    let mut n = 0usize;
    let mut bad: Vec<String> = Vec::new();
    for e in entries {
        let rel = Path::new(&e.rel);
        let unsafe_rel = rel.is_absolute()
            || rel.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            });
        if unsafe_rel {
            bad.push(format!("落点不是相对路径,拒绝恢复: {}", e.rel));
            continue;
        }
        let target = root.join(rel);
        if !e.existed {
            // 写入前它不存在 → 回滚就是把它删掉
            match std::fs::remove_file(&target) {
                Ok(_) => n += 1,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => bad.push(format!("{}:{err}", pretty(&target))),
            }
            continue;
        }
        // 来源也要同样验一遍:上面那句注释说"手改过的清单不能把文件写到别处",但只防了
        // 落点 `rel` —— 不防 `copy` 的话,手改的来源照样能把备份目录之外的文件拷进来。
        let cp = Path::new(&e.copy);
        let unsafe_src = cp.is_absolute()
            || cp.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            });
        if unsafe_src {
            bad.push(format!("备份来源不是相对路径,拒绝恢复: {}", e.copy));
            continue;
        }
        let src = backup_dir.join(cp);
        if !src.is_file() {
            bad.push(format!("备份里缺 {}", e.copy));
            continue;
        }
        if let Some(p) = target.parent() {
            if let Err(err) = std::fs::create_dir_all(p) {
                bad.push(format!("建目录 {} 失败:{err}", pretty(p)));
                continue;
            }
        }
        let name = target
            .file_name()
            .map(|x| x.to_string_lossy().to_string())
            .unwrap_or_default();
        let tmp = target.with_file_name(format!("{name}.restore-tmp"));
        let outcome: Result<(), String> = crate::backup_crypto::read_plain(&src)
            .and_then(|plain| {
                std::fs::write(&tmp, plain).map_err(|e| format!("写临时文件失败: {e}"))
            })
            .and_then(|_| {
                std::fs::rename(&tmp, &target).map_err(|e| format!("替换文件失败: {e}"))
            });
        match outcome {
            Ok(_) => n += 1,
            Err(err) => {
                let _ = std::fs::remove_file(&tmp);
                bad.push(format!("{}:{err}", pretty(&target)));
            }
        }
    }
    if n == 0 && bad.is_empty() {
        return Err("备份清单是空的".into());
    }
    if !bad.is_empty() {
        return Err(format!(
            "只恢复了 {n} 个文件,这些没成:{} —— 备份仍在 {},可重试或手动拷回",
            bad.join(";"),
            pretty(backup_dir)
        ));
    }
    Ok(format!("已按落点清单恢复 {n} 个文件"))
}

/// 一次站点写入的结果。写盘/校验/回滚都在这层做完,上层只管组报告。
#[derive(Debug, Default)]
pub(super) struct SiteOutcome {
    pub(super) added: usize,
    pub(super) updated: usize,
    pub(super) removed: usize,
    pub(super) selected: bool,
    /// 取消写入时顺带把客户端的选中项也清了
    pub(super) clears_selection: bool,
    /// 已存在且内容一致,没动盘
    pub(super) same: bool,
    /// 同 id 的条目原本指向别的服务(界面要在预告里点出来)
    pub(super) foreign_conflict: Option<String>,
    pub(super) backup: PathBuf,
    pub(super) notes: Vec<String>,
}

/// 备份 → 写 → 读回校验;校验不过就回滚并报错。
/// `write` 是真正落盘的那一步,`verify` 从盘上读回来判断写对了没有。
pub(super) fn commit<W, V>(
    site: &Site,
    files: &[PathBuf],
    stamp: &str,
    write: W,
    verify: V,
) -> Result<PathBuf, String>
where
    W: FnOnce() -> Result<(), String>,
    V: FnOnce() -> bool,
{
    let dir = backup(&site.root, files, stamp)?;
    // 写失败也要盖回:两家客户端都是"两份文件必须成套"(ZCode 的 provider ↔ 规则、
    // WorkBuddy 的条目 ↔ 选中项),第一个文件落盘、第二个失败就停在半套状态。
    if let Err(e) = write() {
        return Err(match restore(&dir) {
            Ok(s) => format!("写入失败: {e}｜已按备份整批盖回,未产生半成品。{s}"),
            Err(r) => format!(
                "写入失败: {e}｜回滚也失败: {r}｜备份仍在 {},退出客户端后可手动还原",
                pretty(&dir)
            ),
        });
    }
    if !verify() {
        // 回滚成没成必须说实话:以前 let _ = 吞掉,文案却无条件写「已回滚」
        return Err(match restore(&dir) {
            Ok(s) => format!(
                "读回校验没过,已回滚到写入前的备份({})。{s}",
                pretty(&dir)
            ),
            Err(e) => format!(
                "读回校验没过,而且回滚失败: {e}｜备份仍在 {}——请退出客户端后手动还原",
                pretty(&dir)
            ),
        });
    }
    Ok(dir)
}

/// 与 [`commit`] 同一条流水线,只是备份带落点清单(落点跨目录的客户端用)。
pub(super) fn commit_layered<W, V>(
    site: &Site,
    files: &[PathBuf],
    rels: Option<&[String]>,
    stamp: &str,
    write: W,
    verify: V,
) -> Result<PathBuf, String>
where
    W: FnOnce() -> Result<(), String>,
    V: FnOnce() -> bool,
{
    // L1 决策:动的是谁、动哪些文件。**注入是爆炸半径最大的操作** ——
    // 写坏了不是"挂件少个数字",是 Trae / DSH / ZCode / WorkBuddy 罢工。
    // 这一层以前一个字都不记,出事时完全无从下手。
    let mut names: Vec<String> = files
        .iter()
        .map(|f| {
            f.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| f.display().to_string())
        })
        .collect();
    names.truncate(6);
    let files_desc = names.join(",");
    crate::diaglog::write_keyed(
        "inject",
        &format!("{}:plan", site.label),
        &format!(
            "准备写入 {} · 站点 {} · 文件 {}(共 {} 个)",
            site.client.label(),
            site.label,
            files_desc,
            files.len()
        ),
    );
    let dir = match rels {
        Some(rels) => backup_layered(&site.root, files, rels, stamp)?,
        None => backup(&site.root, files, stamp)?,
    };
    crate::diaglog::write_keyed(
        "inject",
        &format!("{}:backup", site.label),
        &format!("已备份到 {}", pretty(&dir)),
    );
    if let Err(e) = write() {
        let msg = match restore(&dir) {
            Ok(s) => format!("写入失败: {e}｜已按备份整批盖回,未产生半成品。{s}"),
            Err(r) => format!(
                "写入失败: {e}｜回滚也失败: {r}｜备份仍在 {},可手动还原",
                pretty(&dir)
            ),
        };
        crate::diaglog::write_keyed(
            "inject",
            &format!("{}:write-fail", site.label),
            &format!("写入失败,已尝试回滚: {e}"),
        );
        return Err(msg);
    }
    if !verify() {
        crate::diaglog::write_keyed(
            "inject",
            &format!("{}:verify-fail", site.label),
            "读回校验没过,正在回滚",
        );
        return Err(match restore(&dir) {
            Ok(s) => format!("读回校验没过,已回滚到写入前的备份({})。{s}", pretty(&dir)),
            Err(e) => format!(
                "读回校验没过,而且回滚失败: {e}｜备份仍在 {}——请手动还原",
                pretty(&dir)
            ),
        });
    }
    crate::diaglog::write_keyed(
        "inject",
        &format!("{}:ok", site.label),
        &format!("写入并校验通过 · {}", pretty(&dir)),
    );
    Ok(dir)
}

/// 目录里那个模型的人话名字(`deepseek-v4.1-flash` → `DeepSeek V4.1 Flash`)。
/// 单独列一份:目录 json 里的 `name` 字段不能拿来算,只能照抄(见 `DSH_CATALOG`)。
pub(super) fn catalog_display(id: &str) -> String {
    CATALOG_DISPLAY
        .iter()
        .find(|(k, _)| *k == id)
        .map(|(_, v)| (*v).to_string())
        .unwrap_or_else(|| id.to_string())
}

/// pi-ai 目录里的模型显示名(`name` 字段),照抄。
const CATALOG_DISPLAY: &[(&str, &str)] = &[
    ("claude-haiku-5-5", "Claude Haiku 5.5"),
    ("minimax-m3", "MiniMax-M3"),
    ("qwen3.7-plus", "Qwen3.7 Plus"),
    ("qwen3.8-flash", "Qwen3.8 Flash"),
    ("qwen3.8-max", "Qwen3.8 Max"),
    ("deepseek-v4-flash", "DeepSeek V4 Flash"),
    ("deepseek-v4-flash-vision-exp", "DeepSeek V4 Flash Vision Exp"),
    ("deepseek-v4-pro", "DeepSeek V4 Pro (New)"),
    ("deepseek-v4.1-flash", "DeepSeek V4.1 Flash"),
    ("glm-5.2", "GLM-5.2"),
    ("glm-5.3", "GLM-5.3"),
    ("glm-5.3-flash", "GLM-5.3-Flash"),
    ("hy3", "Hy3"),
    ("hy4-preview", "Hy4 preview"),
    ("kimi-k2.7-code", "Kimi K2.7 Code"),
    ("kimi-k3", "Kimi K3"),
    ("longcat-2.0", "LongCat-2.0"),
    ("longcat-2.5-preview-free", "LongCat 2.5 Preview Free"),
    ("mimo-v2.5", "MiMo V2.5"),
    ("mimo-v2.5-pro", "MiMo V2.5 Pro"),
    ("mimo-v2.6-flash", "MiMo-V2.6-Flash"),
    ("mimo-v2.6-pro", "MiMo-V2.6-Pro"),
    ("minimax-m2.7", "MiniMax-M2.7"),
    ("space-bunny", "Space Bunny"),
    ("gpt-5.6-luna", "GPT-5.6 Luna"),
    ("gpt-6-luna", "GPT-6 Luna"),
    ("grok-4.6", "Grok 4.6"),
    ("grok-4.7", "Grok 4.7"),
    ("muse-spark-1.2-contributor", "Muse Spark 1.2 Contributor"),
    ("muse-spark-1.3-contributor", "Muse Spark 1.3 Contributor"),
];
