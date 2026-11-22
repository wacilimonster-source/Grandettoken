use super::*;

// ───────────────────── 自定义渠道的增删改(裁决 6A) ─────────────────────

/// 下一个自定义渠道 id(从 1 开始找空位;删掉再建不复用旧 id,
/// 免得和"历史快照属于谁"这件事搅在一起)。
pub(crate) fn next_custom_id(cfg: &Config) -> String {
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
pub(crate) async fn custom_test(
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
pub(crate) async fn custom_add(
    app: tauri::AppHandle,
    ch: tokenscope_core::custom::CustomChannel,
    key: String,
) -> Result<String, String> {
    ch.validate()?;
    if key.trim().is_empty() {
        return Err("密钥不能为空".into());
    }
    let mut saved = ch.clone();
    // 小写化颜色/单位在这里做一次,免得每种渲染各写一遍
    saved.unit = ch.normalized_unit();
    // id 分配与占位落盘在同一个临界区里完成(报告 L12):以前 id 在锁内分配、定义却
    // 等密钥写完才 push,两次并发 custom_add 会分到同一个号,产生重复 id 的渠道定义。
    // 占位先落盘(此刻还没有密钥,渠道只是"未配置"态),密钥写失败就地把占位撤掉。
    let st = app.state::<AppState>();
    let mut assigned = String::new();
    if let Err(e) = save_config_off_main(&st, |c| {
        let id = next_custom_id(c);
        saved.id = id.clone();
        c.custom_channels.push(saved);
        assigned = id;
    })
    .await
    {
        return Err(e);
    }
    let id = assigned;
    let cid = id.clone();
    let k = key.clone();
    // N29(2026-10-21):写值前先清引用指针。next_custom_id 用的是"最小空号",
    // 删掉 custom-1 再新建就会复用它 —— 而上一个渠道若留下孤儿指针(N02 之前删的),
    // 新渠道会**继承别人的引用**,卡片上显示"引用密钥箱",用的却是别人的密钥。
    // 清了指针,id 复用就是安全的(也就不必为它引入单调计数器)。
    if let Err(e) = tokio::task::spawn_blocking(move || -> Result<(), String> {
        secrets::link_clear(&cid)?;
        secrets::set(&cid, &k)
    })
    .await
    .map_err(|e| format!("写密钥任务失败: {e}"))?
    {
        // 密钥没写成:撤掉占位定义 + 删掉半把钥匙,不留无钥渠道也不留孤儿凭据
        let cid2 = id.clone();
        let _ = tokio::task::spawn_blocking(move || secrets::delete(&cid2)).await;
        let id2 = id.clone();
        let _ = save_config_off_main(&st, move |c| c.custom_channels.retain(|x| x.id != id2)).await;
        return Err(e);
    }
    st.fetcher.forget_channel(&lock_rw(&st.store), &id);
    Ok(id)
}

/// 删除一个自定义渠道。**历史快照保留**(与隐藏渠道同一条纪律:数据不删),
/// 只清定义与密钥。
#[tauri::command]
pub(crate) async fn custom_remove(app: tauri::AppHandle, id: String) -> Result<String, String> {
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
    st.fetcher.forget_channel(&lock_rw(&st.store), &id);
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

