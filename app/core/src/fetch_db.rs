use super::*;

pub(super) type CustomFetchResult = (custom::CustomChannel, Option<custom::CustomResult>, String);

pub(super) struct DbBatch {
    pub cfg: Arc<Config>,
    pub out: Vec<Option<ChannelView>>,
    pub fetched: Vec<FetchedOne>,
    pub previous: Vec<ChannelView>,
    pub custom_results: Vec<CustomFetchResult>,
    pub fx_cached: Option<FxRate>,
    pub fx_now: Option<FxRate>,
    pub cycle_cached: Vec<Option<CycleMemo>>,
}

pub(super) async fn save_recovery(store: Arc<Mutex<Store>>, id: &'static str, snapshot: Snapshot) {
    let result =
        tokio::task::spawn_blocking(move || snapshot.save(&crate::lockx::lock(&store), id)).await;
    if let Err(error) = result.map_err(|e| e.to_string()).and_then(|res| res) {
        crate::diaglog::write_keyed(id, "snapshot", &error);
    }
}

impl Fetcher {
    fn account_count(&self, db: &Store, id: &str, fp: &str) -> i64 {
        if fp.is_empty() {
            return 1;
        }
        if let Some(count) = self.channel_state().accounts_cache.get(fp).copied() {
            return count;
        }
        let count = db.fingerprint_count(id).unwrap_or(1).max(1);
        self.channel_state()
            .accounts_cache
            .insert(fp.to_owned(), count);
        count
    }

    pub(super) async fn fetch_custom_with_progress(
        &self,
        reader: &Arc<Mutex<Store>>,
        channels: &[custom::CustomChannel],
        hidden: &[String],
    ) -> Vec<CustomFetchResult> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let fetch = async {
            let observe =
                move |ch: &custom::CustomChannel, r: Option<&custom::CustomResult>, fp: &str| {
                    if let Some(r) = r {
                        // A cancelled fetch also cancels its progress consumer.
                        let _ = tx.send((ch.clone(), r.clone(), fp.to_owned()));
                    }
                };
            fetch_custom_all_observed(
                &self.http,
                channels,
                hidden,
                |id| secrets::channel_key(id).ok().flatten(),
                Some(&observe),
            )
            .await
        };
        let progress = async {
            while let Some((ch, r, fp)) = rx.recv().await {
                let reader = reader.clone();
                let id = ch.id.clone();
                let failure = r.failure_kind;
                match tokio::task::spawn_blocking(move || {
                    let db = crate::lockx::lock(&reader);
                    let mut v = build_custom_view(&ch, &r, &db, &fp, 1);
                    if !v.valid {
                        if let Some(c) = Snapshot::load(&db, &ch.id, &custom_scope(&ch, &fp)) {
                            v.remaining = c.result.remaining;
                            v.updated_at = c.ts;
                            v.stale = true;
                        }
                    }
                    v
                })
                .await
                {
                    Ok(mut view) => {
                        if !view.valid {
                            view.recovery = Some(
                                Recovery::failed(
                                    self.channel_state().recovery.get(&id),
                                    now_ts(),
                                    failure,
                                )
                                .view(now_ts()),
                            );
                        }
                        self.publish(view);
                    }
                    Err(e) => crate::diaglog::write_keyed(&id, "view", &e.to_string()),
                }
            }
        };
        futures::join!(fetch, progress).0
    }

    pub(super) async fn publish_pending(
        &self,
        reader: &Arc<Mutex<Store>>,
        cfg: &Arc<Config>,
        f: &FetchedOne,
    ) {
        let Some((result, stale, ts, cred)) = f.pending.clone() else {
            return;
        };
        let reader = reader.clone();
        let cfg = cfg.clone();
        let fp = f.fp.clone();
        let def = f.def;
        match tokio::task::spawn_blocking(move || {
            build_view(
                def,
                result,
                stale,
                ts,
                &crate::lockx::lock(&reader),
                &cfg,
                cred.as_ref(),
                &fp,
                1,
            )
        })
        .await
        {
            Ok(mut view) => {
                view.recovery = self
                    .channel_state()
                    .recovery
                    .get(def.id)
                    .map(|r| r.view(now_ts()));
                self.publish(view);
            }
            Err(e) => crate::diaglog::write_keyed(def.id, "view", &e.to_string()),
        }
    }

    pub(super) fn finish_db(
        &self,
        writer: &Mutex<Store>,
        reader: &Mutex<Store>,
        batch: DbBatch,
    ) -> Vec<ChannelView> {
        let DbBatch {
            cfg,
            mut out,
            fetched,
            previous,
            custom_results,
            fx_cached,
            fx_now,
            cycle_cached,
        } = batch;
        let mut custom_views = Vec::new();
        let mut custom_recovery = Vec::new();

        // Commit new observations before reading the WAL snapshot for the final view.
        {
            let s = crate::lockx::lock(writer);
            for f in &fetched {
                // 升级上来的库里,历史行还没有指纹:首次拿到稳定指纹时认领一次,
                // 让"还没换过 key"的用户的账目连续(见 store::adopt_orphan_snapshots)。
                if !f.fp.is_empty() {
                    let first = {
                        let mut state = self.channel_state();
                        let done = &mut state.adopted;
                        done.insert(format!("{}:{}", f.def.id, f.fp))
                    };
                    if first {
                        if let Err(e) = s.adopt_orphan_snapshots(f.def.id, &f.fp) {
                            eprintln!("认领无指纹历史快照失败 {}: {e}", f.def.id);
                        }
                    }
                }
                if let Some((res, ts)) = f.success.as_ref() {
                    let kind = match res.kind {
                        Kind::Amount => "amount",
                        Kind::Percent => "percent",
                        Kind::Points => "points",
                    };
                    // 积分到账:和**上一份**快照比正增量。必须排在 record_fp 之前 ——
                    // 写完之后"上一次"就成了自己,永远算出 0。
                    // 取数失败不会走到这里(失败时 success 是 None),所以中间漏掉的那段
                    // 增量不会被吞:下一轮仍拿最后一条成功快照当基线。
                    if providers::tracks_grants(f.def.id) {
                        if let Some(cur) = res.remaining {
                            match s.prev_remaining(f.def.id, &f.fp, *ts) {
                                Ok(Some(prev)) => {
                                    if let Some(amount) = crate::grants::detect(prev, cur) {
                                        let g = crate::grants::Grant {
                                            channel: f.def.id.into(),
                                            day: local_day_of(*ts),
                                            ts: *ts,
                                            amount,
                                            source: "delta".into(),
                                            from_bal: Some(prev),
                                            to_bal: Some(cur),
                                        };
                                        match s.insert_grant(&g) {
                                            Ok(true) => crate::diaglog::write_keyed(
                                                f.def.id,
                                                "grant",
                                                &format!(
                                                    "积分到账 +{amount:.2}({prev:.2} → {cur:.2})"
                                                ),
                                            ),
                                            Ok(false) => {} // 同一秒的重放,已有记录
                                            Err(e) => {
                                                eprintln!("写入积分到账事件失败 {}: {e}", f.def.id)
                                            }
                                        }
                                        // 保留期在这里顺手做:到账是低频事件,不值得单开定时器
                                        if let Err(e) = s.prune_grants(*ts - GRANT_KEEP_SEC) {
                                            eprintln!("清理积分到账事件失败 {}: {e}", f.def.id);
                                        }
                                    }
                                }
                                Ok(None) => {} // 首次取数,没有基线 —— 不是"没到账"
                                Err(e) => eprintln!("读取上次余额失败 {}: {e}", f.def.id),
                            }
                        }
                    }
                    // 注意顺序:先写快照再组视图 —— 申请制额度的跳升检测要用最新快照
                    if let Err(e) = s.record_fp(
                        f.def.id,
                        &f.fp,
                        *ts,
                        res.remaining,
                        res.used,
                        res.total,
                        kind,
                    ) {
                        // 写库失败以前是 let _ = 静默吞掉:消耗会悄悄偏小且无从排查(O-7)
                        eprintln!("写入快照失败 {}: {e}", f.def.id);
                    }
                }
            }
            // 牌价只在"跨月取到了新数"那一轮落库,不是每轮都写
            if let Some(f) = fx_now.as_ref() {
                if fx_cached.as_ref() != Some(f) {
                    if let Err(e) = fx::save(&s, f) {
                        eprintln!("写入汇率缓存失败: {e}");
                    }
                }
            }
            for (ch, result, fp) in &custom_results {
                let Some(r) = result else { continue };
                // 历史行认领(与内置渠道同一套);探活模式不写快照 ——
                // 它没有数值,"记一笔"只会把当天的观测缺口悄悄填上
                if !fp.is_empty() {
                    let first = {
                        let mut state = self.channel_state();
                        let done = &mut state.adopted;
                        done.insert(format!("{}:{}", ch.id, fp))
                    };
                    if first {
                        if let Err(e) = s.adopt_orphan_snapshots(&ch.id, fp) {
                            eprintln!("认领无指纹历史快照失败 {}: {e}", ch.id);
                        }
                    }
                    if r.valid && !r.probe_only {
                        if let Some(v) = r.remaining {
                            if let Err(e) =
                                s.record_fp(&ch.id, fp, now_ts(), Some(v), None, None, "amount")
                            {
                                eprintln!("写入快照失败 {}: {e}", ch.id);
                            }
                        }
                    }
                }
            }
        }
        let all = {
            let s = crate::lockx::lock(reader);
            for f in fetched {
                if let Some(v) = f.view {
                    out[f.i] = Some(v);
                } else if let Some((result, stale, ts, cred)) = f.pending {
                    // "这个渠道用过几把密钥"是整渠道扫描,按指纹缓存,别每轮都跑
                    let accounts = self.account_count(&s, f.def.id, &f.fp);
                    out[f.i] = Some(build_view(
                        f.def,
                        result,
                        stale,
                        ts,
                        &s,
                        &cfg,
                        cred.as_ref(),
                        &f.fp,
                        accounts,
                    ));
                }
            }
            for ch in cfg.custom_channels.iter() {
                if cfg.hidden_channels.iter().any(|h| h == &ch.id) {
                    custom_views.push(custom_placeholder(ch, true, None));
                    continue;
                }
                let Some((_, res, fp)) = custom_results.iter().find(|(c, _, _)| c.id == ch.id)
                else {
                    custom_views.push(
                        previous
                            .iter()
                            .find(|v| v.id == ch.id)
                            .cloned()
                            .unwrap_or_else(|| custom_placeholder(ch, false, None)),
                    );
                    continue;
                };
                let Some(r) = res else {
                    self.channel_state().recovery.remove(&ch.id);
                    custom_views.push(custom_placeholder(ch, false, None));
                    continue;
                };
                let accounts = self.account_count(&s, &ch.id, fp);
                let scope = custom_scope(ch, fp);
                let cached = Snapshot::load(&s, &ch.id, &scope);
                let mut v = build_custom_view(ch, r, &s, fp, accounts);
                if r.valid {
                    self.channel_state().recovery.remove(&ch.id);
                    if !r.probe_only {
                        let result = FetchResult {
                            valid: true,
                            remaining: r.remaining,
                            unit: v.unit.clone(),
                            ..Default::default()
                        };
                        custom_recovery.push((&ch.id, Snapshot::new(scope, v.updated_at, result)));
                    }
                } else {
                    self.failed(&ch.id, r.failure_kind);
                    if let Some(cached) = cached {
                        v.remaining = cached.result.remaining;
                        v.updated_at = cached.ts;
                        v.stale = true;
                    }
                }
                custom_views.push(v);
            }
            drop(s);
            // 测试与旧调用方可以共用读写连接，两把 guard 的生命周期必须分开。
            attach_plan_costs(&mut out, &crate::lockx::lock(writer), &cycle_cached, fx_now.as_ref());
            let s = crate::lockx::lock(reader);
            let mut all = crate::antigravity_accounts::expand_all(
                out.into_iter().flatten().collect(),
                &s,
                &cfg,
            );
            all.extend(custom_views);
            all.push(crate::google_cloud::channel(&s, &cfg, now_ts()));
            all
        };
        if !custom_recovery.is_empty() {
            let writer = crate::lockx::lock(writer);
            for (id, snapshot) in custom_recovery {
                if let Err(e) = snapshot.save(&writer, id) {
                    crate::diaglog::write_keyed(id, "snapshot", &e);
                }
            }
        }
        all
    }
}
