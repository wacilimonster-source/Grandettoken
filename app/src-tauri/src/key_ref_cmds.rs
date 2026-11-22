use super::*;

// ───────────── 渠道密钥 ↔ 密钥箱(设计稿 product-vault-channel-bridge)─────────────
//
// 裁决 1:主线是**引用**,不是"存两份"。所以渠道的密钥来源只有两种状态 ——
//   · 直接填在渠道卡上(inline)
//   · 引用密钥箱里的某一条(vault)
// 两者互斥:切到引用会清掉手填的值(裁决 7),填了值会清掉引用。

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KeySourceInfo {
    /// "none" | "inline" | "vault" | "dangling"
    pub(crate) mode: String,
    pub(crate) vault_id: Option<String>,
    /// 尾 4 位掩码。只有真读到值才有 —— 断链时是 None,界面不该编。
    pub(crate) hint: Option<String>,
    /// 引用模式下被引用条目的 平台 / 标签(渠道卡上要显示"正在用 4SAPI / 主号")
    pub(crate) vault_platform: Option<String>,
    pub(crate) vault_label: Option<String>,
}

/// 这个渠道的密钥从哪来。渠道卡渲染与"密钥来源"二选一都靠它。
#[tauri::command]
pub(crate) async fn key_source(app: tauri::AppHandle, id: String) -> Result<KeySourceInfo, String> {
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
pub(crate) async fn set_key_ref(app: tauri::AppHandle, id: String, vault_id: String) -> Result<(), String> {
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
    sync_channel_to_gateway(&app, &id);
    Ok(())
}

/// 取消引用,改回"直接填"。手填的值本来就在切到引用时清掉了,所以这里结果是"未配置"。
#[tauri::command]
pub(crate) async fn clear_key_ref(app: tauri::AppHandle, id: String) -> Result<(), String> {
    key_allowed(&app.state::<AppState>(), &id)?;
    let store = app.state::<AppState>().store.clone();
    let cid = id.clone();
    tokio::task::spawn_blocking(move || secrets::link_clear(&cid))
        .await
        .map_err(|e| format!("清除密钥引用失败: {e}"))??;
    lock_rw(&store).vault_ref_clear(&id)?;
    app.state::<AppState>().fetcher.forget_channel(&lock_rw(&store), &id);
    sync_channel_to_gateway(&app, &id);
    Ok(())
}

/// 全部引用关系(渠道 → 箱子条目)。密钥箱列表一次读出来批量渲染。
#[tauri::command]
pub(crate) fn vault_ref_map(app: tauri::AppHandle) -> Result<Vec<(String, String)>, String> {
    let store = app.state::<AppState>().store.clone();
    let out = lock_rw(&store).vault_ref_pairs().map_err(|e| e.to_string())?;
    Ok(out)
}

