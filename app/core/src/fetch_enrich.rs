use super::*;

impl Fetcher {
    pub(super) async fn enrich_qoder_result(
        &self,
        def: &providers::ProviderDef,
        chosen: Option<&appauth::Credential>,
        store: &Arc<Mutex<Store>>,
        qoder_web_src: Option<WebCredentialSource>,
        result: &mut FetchResult,
    ) {
        if !matches!(def.id, "qoder" | "qoder-cn") || !result.valid {
            return;
        }
        let site = crate::weblogin::qoder_site(def.id)
            .expect("上面已限定 qoder | qoder-cn,站点配置必然存在"); // matches! 已穷举 qoder_site 支持的两个 ID
                                                                     // 余额凭据换了(换账号 / 换令牌),浏览器里的网页会话不会跟着换:
                                                                     // 2026-11-09 真机就是"余额已经是新账号的 100,逐包明细还是旧账号的
                                                                     // 18 个每日 100 分包"。先判断这本账换没换,再决定要不要用网页会话。
        let qoder_scope = chosen.map(credential_scope).unwrap_or_default();
        let (scope_changed, session_stale) = {
            let st = store.lock().unwrap_or_else(|p| p.into_inner());
            let changed = note_qoder_scope(&st, def.id, &qoder_scope);
            let stale = qoder_web_session_stale(&st, def.id, site.session_at_key);
            (changed, stale)
        };
        if scope_changed {
            crate::lockx::lock(&self.qoder_web).forget_packs(def.id);
            crate::diaglog::write_keyed(
                def.id,
                "webpacks",
                "余额凭据已更换 → 旧网页到期明细作废,需用当前账号重新登录网页版",
            );
        }
        if session_stale {
            // **不拿旧会话的数据来充数**:宁可如实说"需要重新登录",
            // 也不把上一本账的已到期记录挂到新账号名下。
            result.expiry_complete = false;
            extra_set(
                &mut result.extra,
                "逐笔到期明细",
                "余额凭据已更换 · 网页到期明细需用当前账号重新登录",
            );
        } else {
            let qoder_id = def.id;
            // 会话来源是阻塞派发(要同步问 webview 的 Cookie 罐),不能占着 async worker
            let session = tokio::task::spawn_blocking(move || {
                qoder_web_src.as_deref().and_then(|f| f(qoder_id))
            })
            .await
            .ok()
            .flatten();
            let memo = self.qoder_memo_for(def.id);
            match session {
                Some(sess) => match qoderweb::fetch_packs(&self.http, &sess, &memo, site).await {
                    Ok((packs, _)) => {
                        let n = packs.len();
                        result.expiring = packs.iter().map(|p| p.as_expiring()).collect();
                        result.expiry_complete = true;
                        extra_set(
                            &mut result.extra,
                            "逐笔到期明细",
                            if n > 0 {
                                "按平台返回"
                            } else {
                                "按平台返回 · 当前没有待到期的积分包"
                            },
                        );
                        crate::lockx::lock(&self.qoder_web).remember_packs(
                            def.id,
                            &qoder_scope,
                            now_ts(),
                            result.expiring.clone(),
                        );
                    }
                    Err(e) => {
                        // 只用**同一本账**的缓存:旧账号的列表不能因为
                        // 这一轮网络失败就重新冒出来。
                        let cached =
                            crate::lockx::lock(&self.qoder_web).cached_packs(def.id, &qoder_scope);
                        match cached {
                            Some((ts, packs)) => {
                                qoderweb::use_cached_packs(result, packs);
                                let when = chrono::DateTime::from_timestamp(ts, 0)
                                    .map(|d| {
                                        d.with_timezone(&chrono::Local)
                                            .format("%m-%d %H:%M")
                                            .to_string()
                                    })
                                    .unwrap_or_default();
                                extra_set(
                                    &mut result.extra,
                                    "逐笔到期明细",
                                    &format!("{} · 沿用 {when} 抓到的列表", e.user_message()),
                                );
                            }
                            None => {
                                extra_set(&mut result.extra, "逐笔到期明细", e.user_message());
                            }
                        }
                        // 只记形状不记值:响应里可能有余额与会话;诊断按渠道分开
                        // 落,排查时不会把国内版的失败读成国际版的。
                        crate::diaglog::write_keyed(
                            def.id,
                            "webpacks",
                            &match &e {
                                qoderweb::PacksErr::Unauthorized => {
                                    "所有候选都被平台拒绝(未登录或会话过期)".to_string()
                                }
                                // 这一条是"平台换结构"的现场:结构草图在里面
                                qoderweb::PacksErr::Unrecognized(m) => m.clone(),
                                qoderweb::PacksErr::Other(m) => m.clone(),
                            },
                        );
                    }
                },
                None => {
                    extra_set(
                        &mut result.extra,
                        "逐笔到期明细",
                        "积分包明细需网页登录 · 渠道卡点「登录网页版」",
                    );
                }
            }
        }
    }
}
