use super::*;
use super::tests::{read_v, site_at, spec, tmpdir};

/// Trae:每个 agent 组各删一条;选中项指向它时把那个 agent 的选中也摘掉。
#[test]
fn trae_remove_from_every_group_and_clear_selection() {
    let root = tmpdir("trae-rm");
    let db = root.join("state.vscdb");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB)", [])
            .unwrap();
        let map = json!({
            "solo_agent": [
                {"config_source":1,"provider":"","display_name":"Doubao-Seed","selectable":true}
            ],
            "assistant": []
        });
        conn.execute(
            "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
            rusqlite::params![
                "3739861422447896:AI.agent.model.model_list_map",
                serde_json::to_string(&map).unwrap()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
            rusqlite::params![
                "3739861422447896:AI.agent.model.recent_user_selection_by_agent_label",
                r#"{"solo_agent":{"modelId":"solo_agent_1__Doubao-Seed_null","mode":0}}"#
            ],
        )
        .unwrap();
    }
    let opts = Opts {
        trae_auto_select: true,
        ..Default::default()
    };
    trae_write(&db, &spec(), &opts).unwrap();
    let (configured, removal) = trae_removal(&db, &spec());
    assert!(configured);
    let r = removal.unwrap();
    assert_eq!(r.count, 2, "两个 agent 组各一条");
    assert!(r.clears_selection, "选中项正指向它");

    let o = remove_trae_site(&db, &spec()).unwrap();
    assert_eq!(o.removed, 2);
    assert!(o.clears_selection);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT value FROM ItemTable WHERE key LIKE '%model_list_map'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let v: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["solo_agent"].as_array().unwrap().len(), 1, "只剩内置那条");
    assert_eq!(v["assistant"].as_array().unwrap().len(), 0);
    let sel: String = conn
        .query_row(
            "SELECT value FROM ItemTable WHERE key LIKE '%selection%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let sv: Value = serde_json::from_str(&sel).unwrap();
    assert!(sv.as_object().unwrap().is_empty(), "指向它的选中项要摘掉");

    // 幂等
    let o2 = remove_trae_site(&db, &spec()).unwrap();
    assert_eq!(o2.removed, 0);
    assert_eq!((o2.clears_selection, o2.backup.as_os_str().is_empty()), (false, true));
}

/// 台账:新的在前、最多 20 条、能按备份路径找回它属于哪个客户端。
#[test]
fn log_keeps_last_twenty_and_finds_owner() {
    let store = crate::store::Store::open_memory().unwrap();
    assert!(load_log(&store).is_empty());
    for i in 0..25 {
        push_log(
            &store,
            InjectRecord {
                stamp: format!("s{i}"),
                backup_path: format!("C:/x/{i}"),
                client: "zcode".into(),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let log = load_log(&store);
    assert_eq!(log.len(), 20);
    assert_eq!(log[0].stamp, "s24", "最新的在最前");
    assert_eq!(log_owner(&log, "c:/x/24"), Some(Client::Zcode), "路径比对不区分大小写");
    assert_eq!(log_owner(&log, "C:/nope"), None);
}

/// 目标在跑 → 拒写(这是条硬规矩,不能只写在文档里)。
#[test]
fn running_target_is_blocked() {
    let root = tmpdir("running");
    let mut site = site_at(&root, Client::Workbuddy);
    site.running = true;
    assert!(blocked_by_running(&site).unwrap().contains("正在运行"));
    site.running = false;
    assert!(blocked_by_running(&site).is_none());
}

/// P0-1:「取消写入」只删我们自己写的那一条。同 id 但属于别家来源的条目、
/// 以及用户手配的其它条目,都不该被牵连删掉(以前是按 id 全表 retain)。
#[test]
fn workbuddy_remove_only_ours_not_foreign_same_id() {
    let root = tmpdir("wb-rm-scope");
    std::fs::write(
        root.join("models.json"),
        r#"[{"id":"glm-5.3","name":"4S-glm-5.3","url":"https://4sapi.com/v1"},
                {"id":"glm-5.3","name":"我自己配的","url":"https://opencode.ai/zen/go/v1"},
                {"id":"other","name":"mine","url":"https://example.com/v1"}]"#,
    )
    .unwrap();
    std::fs::write(root.join("settings.json"), "{}").unwrap();
    let site = site_at(&root, Client::Workbuddy);
    let s = ModelSpec::new("4sapi", "glm-5.3", "https://4sapi.com/v1", "sk-test-123", None, false);

    let o = remove_workbuddy_site(&site, &s).unwrap();
    assert_eq!(o.removed, 1, "只该删掉属于本来源的那一条");
    let left = read_v(&root.join("models.json"));
    let a = left.as_array().unwrap();
    assert_eq!(a.len(), 2, "别家同 id 条目与不相干的条目都不动");
    assert!(a
        .iter()
        .any(|e| e["url"] == "https://opencode.ai/zen/go/v1"));
    assert!(a.iter().any(|e| e["id"] == "other"));
}

/// P0-4:同时配过 4SAPI 时,取消另一家的最后一个模型必须成功。
/// 旧校验写的是「配置文本里不能再有 4SAPI 字样」→ 这种组合必然误判失败并自动回滚。
#[test]
fn zcode_remove_other_source_last_model_with_4sapi_present() {
    let root = tmpdir("zcode-rm-multi");
    std::fs::write(root.join("config.json"), r#"{"provider":{}}"#).unwrap();
    std::fs::write(
        root.join("provider_config.json"),
        r#"{"schemaVersion":1,"config":{"providerConfigRules":{"providerRules":[]},"modelConfigRules":{"providerModelRules":[],"manualProviderModelRules":[]}}}"#,
    )
    .unwrap();
    let site = site_at(&root, Client::Zcode);
    let go = ModelSpec::new(
        "opencode-go",
        "kimi-k2",
        "https://opencode.ai/zen/go/v1",
        "sk-go",
        None,
        false,
    );
    write_zcode_site(&site, &spec()).unwrap();
    write_zcode_site(&site, &go).unwrap();

    let o = remove_zcode_site(&site, &go)
        .unwrap_or_else(|e| panic!("取消另一家的模型不该被判回滚: {e}"));
    assert_eq!(o.removed, 1);
    let cfg = read_v(&root.join("config.json"));
    assert!(zcode_pid(&cfg, "4SAPI").is_some(), "4SAPI 的 provider 必须原样留着");
    assert!(zcode_pid(&cfg, &go.source_name).is_none(), "这一家的 provider 应已收走");
    let rules = read_v(&root.join("provider_config.json"));
    let names: Vec<&str> = rules["config"]["providerConfigRules"]["providerRules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["providerName"].as_str().unwrap_or(""))
        .collect();
    assert!(
        names.iter().any(|n| *n == "4SAPI"),
        "4SAPI 的规则不该被摘掉: {names:?}"
    );
}

/// P0-2:Trae 走 commit —— 写入前必须有备份落盘,且读回校验要认这次写入。
/// 以前这条路成功不备份(界面「备份」一栏空白、回滚按钮点不动)。
#[test]
fn trae_apply_via_commit_backs_up_before_writing() {
    let root = tmpdir("trae-commit");
    let db = root.join("state.vscdb");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB)", [])
            .unwrap();
        let map = json!({"assistant": [], "solo_agent": []});
        conn.execute(
            "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
            rusqlite::params![
                "3739861422447896:AI.agent.model.model_list_map",
                serde_json::to_string(&map).unwrap()
            ],
        )
        .unwrap();
    }
    let site = Site {
        client: Client::Trae,
        label: "Trae CN".into(),
        running: false,
        process: "Trae CN.exe".into(),
        root: root.clone(),
        files: vec![db.clone()],
    };
    let sp = spec();
    let cell: std::cell::RefCell<Option<SiteOutcome>> = std::cell::RefCell::new(None);
    let dir = commit(
        &site,
        &site.files,
        "20260101-000000",
        || {
            let o = trae_write(&db, &sp, &Opts::default())?;
            *cell.borrow_mut() = Some(o);
            Ok(())
        },
        || trae_verify_written(&db, &sp),
    )
    .expect("写入应成功");
    assert!(cell.borrow().is_some(), "outcome 应带回来");
    assert!(
        dir.join("state.vscdb").is_file(),
        "备份目录里必须有**写入前**的库"
    );
    // 备份那份不该含有我们这次写的条目(证明备份发生在写之前)。
    // 2026-11-12 起快照是 DPAPI 容器,不能再直接拿 SQLite 打开:
    // 先断言「确实是密文」,再解密到临时库检查内容 —— 这条断言同时守住加密不回退。
    let snap = dir.join("state.vscdb");
    assert!(
        crate::backup_crypto::is_sealed(&std::fs::read(&snap).unwrap()),
        "备份快照必须是加密的,不能留一份明文 SQLite"
    );
    let probe = dir.join("state.verify.vscdb");
    std::fs::write(&probe, crate::backup_crypto::read_plain(&snap).unwrap()).unwrap();
    let braw: String = {
        let bconn = rusqlite::Connection::open(&probe).unwrap();
        bconn
            .query_row("SELECT value FROM ItemTable WHERE key LIKE '%model_list_map'", [], |r| {
                r.get(0)
            })
            .unwrap()
    };
    let _ = std::fs::remove_file(&probe);
    assert!(
        !braw.contains(sp.id.as_str()),
        "备份内容必须是写入前的原样,不能是已改坏的库"
    );
    assert!(trae_verify_written(&db, &sp), "读回校验应通过");
}

/// P0-2 / 共同前提④:校验不过要整批回滚,并且如实说明回滚结果。
#[test]
fn trae_commit_rolls_back_when_verify_fails() {
    let root = tmpdir("trae-commit-rb");
    let db = root.join("state.vscdb");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
            rusqlite::params![
                "3739861422447896:AI.agent.model.model_list_map",
                r#"{"assistant":[]}"#
            ],
        )
        .unwrap();
    }
    let site = Site {
        client: Client::Trae,
        label: "Trae CN".into(),
        running: false,
        process: "Trae CN.exe".into(),
        root: root.clone(),
        files: vec![db.clone()],
    };
    let sp = spec();
    let r = commit(
        &site,
        &site.files,
        "20260101-000000",
        || trae_write(&db, &sp, &Opts::default()).map(|_| ()),
        // 故意让校验失败,看它是否真的盖回写入前的库
        || false,
    );
    let msg = r.unwrap_err();
    assert!(msg.contains("读回校验没过"), "文案: {msg}");
    assert!(
        !msg.contains("而且回滚失败"),
        "回滚应当成功: {msg}"
    );
    let conn = rusqlite::Connection::open(&db).unwrap();
    let raw: String = conn
        .query_row("SELECT value FROM ItemTable WHERE key LIKE '%model_list_map'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(
        !raw.contains(sp.id.as_str()),
        "校验不过必须回到写入前的库,不能把半成品留在 Trae 的配置里"
    );
}

/// P1-5:两份文件必须成套。第二个写失败时,已经落盘的第一个要盖回。
#[test]
fn commit_rolls_back_when_second_file_write_fails() {
    let root = tmpdir("commit-wfail");
    let a = root.join("models.json");
    let b = root.join("settings.json");
    std::fs::write(&a, "[old]").unwrap();
    std::fs::write(&b, "{bold}").unwrap();
    let site = Site {
        client: Client::Workbuddy,
        label: "WorkBuddy".into(),
        running: false,
        process: "WorkBuddy.exe".into(),
        root: root.clone(),
        files: vec![a.clone(), b.clone()],
    };
    let r = commit(
        &site,
        &site.files,
        "20260101-000000",
        || {
            std::fs::write(&a, "[new]").map_err(|e| e.to_string())?;
            Err("模拟:第二个文件被占用".to_string())
        },
        || true,
    );
    let msg = r.unwrap_err();
    assert_eq!(
        std::fs::read_to_string(&a).unwrap(),
        "[old]",
        "第一个文件必须被盖回,不能留半成品"
    );
    assert!(
        msg.contains("未产生半成品") || msg.contains("备份仍在"),
        "文案要说清回滚结果: {msg}"
    );
}

/// P0-2 的另一半:Trae 的每条 UPDATE 在一个事务里,中途失败不留半套。
/// (rusqlite 默认每条语句 autocommit,以前写坏一半是真的会发生的。)
#[test]
fn trae_write_uses_one_transaction() {
    let root = tmpdir("trae-tx");
    let db = root.join("state.vscdb");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB)", [])
            .unwrap();
        // 两行模型表:第一行能写,第二行是数组之外的形状 → 只有第一行会 UPDATE 成功
        for (k, v) in [
            ("1:AI.agent.model.model_list_map", r#"{"assistant":[]}"#),
            ("2:AI.agent.model.model_list_map", r#"{"assistant":{"not":"array"}}"#),
        ] {
            conn.execute(
                "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
                rusqlite::params![k, v],
            )
            .unwrap();
        }
    }
    let sp = spec();
    // 读回校验函数对"至少一组有我们这条"成立,说明写入确实只落在数组行上
    assert!(trae_write(&db, &sp, &Opts::default()).is_ok());
    assert!(trae_verify_written(&db, &sp), "数组行写完能被读回");
    let has_ours = trae_has_ours(&db, &sp);
    assert!(has_ours, "移除前的预检应认到条目存在");
    let o = remove_trae_site(&db, &sp).unwrap();
    assert_eq!(o.removed, 1);
    assert!(trae_verify_removed(&db, &sp), "移除后读回应干净");
}

// ───────────────────── DeepSeek Harness ─────────────────────

/// 与真机同形的一份 patch:注释、别的条目、目录里已有的 opencode-go route 都在。
/// 原始字符串:行续接 `\` 会吃掉缩进,夹具会被悄悄改写(踩过一次)。
const DSH_PATCH: &str = r#"# dsh profile patch
- id: agent-default-model
  name: "@deepseek-ai/dsh-agent-default-model"
  config:
    provider: opencode-go
    model: deepseek-v4.1-flash
    reasoningEffort: high

- id: llm-pi-ai
  name: "@deepseek-ai/dsh-llm-pi-ai"
  config:
    providers:
      opencode-go:
        models:
          - id: deepseek-v4.1-flash
            name: OG DeepSeek V4.1 Flash
            contextWindow: 1000000
        apiKeyEnv: OPENCODE_GO_API_KEY
"#;

/// 与真机同形的一份凭据文件(值换成假的)。
const DSH_CRED: &str = r#"version: 1
records:
  client-connection/browser-session:
    kind: grant
    payload:
      version: 1
      secret: abc
refs:
  OPENCODE_GO_API_KEY: sk-existing
"#;

fn dsh_home_at(tag: &str) -> PathBuf {
    let home = tmpdir(tag);
    std::fs::create_dir_all(home.join("profiles").join("desktop")).unwrap();
    home
}

fn dsh_site_at(home: &Path) -> Site {
    Site {
        client: Client::Dsh,
        label: format!("{} · desktop", Client::Dsh.label()),
        running: false,
        process: Client::Dsh.process().into(),
        root: home.to_path_buf(),
        files: vec![dsh_patch_of(home, "desktop"), dsh_creds_of(home)],
    }
}

fn dsh_write_fixture(home: &Path, patch: &str, cred: &str) {
    std::fs::write(dsh_patch_of(home, "desktop"), patch).unwrap();
    std::fs::write(dsh_creds_of(home), cred).unwrap();
}

fn dsh_read(home: &Path, name: &str) -> String {
    let p = if name == "patch" {
        dsh_patch_of(home, "desktop")
    } else {
        dsh_creds_of(home)
    };
    std::fs::read_to_string(p).unwrap()
}

/// 排障用:设 `TS_KEEP_ARTIFACTS=<目录>` 时把这次写出来的两份文件另存一份,
/// 留给外部核验(用 DSH 自己的 `yaml` 包 / 凭据规则过一遍)。默认什么都不做。
fn dump_artifacts(tag: &str, home: &Path) {
    let Some(dir) = std::env::var_os("TS_KEEP_ARTIFACTS") else {
        return;
    };
    let dir = PathBuf::from(dir).join(tag);
    std::fs::create_dir_all(&dir).unwrap();
    for (src, dst) in [
        (dsh_patch_of(home, "desktop"), "cordis.patch.yml"),
        (dsh_creds_of(home), ".credentials.yaml"),
    ] {
        if let Ok(t) = std::fs::read_to_string(&src) {
            std::fs::write(dir.join(dst), t).unwrap();
        }
    }
    eprintln!("artifacts = {}", pretty(&dir));
}

/// 真机取证:本机装了 DSH 时,拿**真实**的 patch / 凭据文件过一遍「读 + 预告」——
/// 只读、绝不写盘。默认 ignore(换台机器就没这两个文件):
/// `cargo test --lib -- --ignored dsh_real_machine`。
#[test]
#[ignore]
fn dsh_real_machine_files_parse_and_preview() {
    let home = dsh_home().expect("本机没有 harness home");
    assert!(home.is_dir(), "harness home 不存在:{}", home.display());
    let profile = dsh_profile(&home).expect("没找到可用的 profile");
    let site = dsh_site_at(&home);
    eprintln!("harness home = {}", home.display());
    eprintln!("profile      = {profile}");
    for f in &site.files {
        eprintln!("  {}", pretty(f));
    }
    site_readable(&site).expect("真实文件应当读得懂(认不出来就别写)");
    let sp = spec();
    let out = preview(Client::Dsh, &sp, &Opts::default());
    assert_eq!(out.len(), 1, "应当只有一个 DSH 站点");
    assert!(
        out[0].blocked.is_none(),
        "预告不该被拒:{:?}",
        out[0].blocked
    );
    eprintln!("动作 = {}", out[0].action);
    for n in &out[0].notes {
        eprintln!("  · {n}");
    }
    // 只读校验:预览前后文件逐字节不变
    let before: Vec<String> = site
        .files
        .iter()
        .map(|f| dsh_read_text(f))
        .collect();
    let _ = preview(Client::Dsh, &sp, &Opts::default());
    let after: Vec<String> = site.files.iter().map(|f| dsh_read_text(f)).collect();
    assert_eq!(before, after, "预告是纯读,一个字节都不该动");
}

/// 真机形状的端到端:把本机**真实**的 patch / 凭据文件抄进临时 harness home,
/// 在抄本上跑 写入 → 读回校验 → 取消写入 → 两次回滚。
/// **全程不碰 `~/.dsh`**;默认 ignore:`cargo test --lib -- --ignored dsh_real_machine`。
#[test]
#[ignore]
fn dsh_real_machine_shapes_write_and_rollback() {
    let home = dsh_home().expect("本机没有 harness home");
    let profile = dsh_profile(&home).expect("没找到可用的 profile");
    let patch0 = std::fs::read_to_string(dsh_patch_of(&home, &profile)).expect("读真实 patch");
    let cred0 = std::fs::read_to_string(dsh_creds_of(&home)).unwrap_or_default();
    let had_cred = !cred0.is_empty();

    // 抄进临时 home:形状与真机逐字节一致,但一个字符都不写回 ~/.dsh
    let tmp = tmpdir("dsh-real-shapes");
    std::fs::create_dir_all(tmp.join("profiles").join(&profile)).unwrap();
    std::fs::write(dsh_patch_of(&tmp, &profile), &patch0).unwrap();
    if had_cred {
        std::fs::write(dsh_creds_of(&tmp), &cred0).unwrap();
    }
    let site = Site {
        client: Client::Dsh,
        label: format!("{} · {profile}", Client::Dsh.label()),
        running: true, // 故意标成"在跑":DSH 是唯一不该被它挡住的客户端
        process: Client::Dsh.process().into(),
        root: tmp.clone(),
        files: vec![dsh_patch_of(&tmp, &profile), dsh_creds_of(&tmp)],
    };
    assert!(
        blocked_by_running(&site).is_none(),
        "DSH 在跑也不该阻止写入(热写口径)"
    );
    site_readable(&site).expect("真实文件形状应当读得懂");

    let sp = spec(); // 4sapi / claude-sonnet-5 / https://4sapi.com/v1 / sk-test-123
    let o = write_dsh_site(&site, &sp).unwrap();
    assert_eq!(o.added, 1, "真实形状里没有这条 route,应当是新增");
    let patch1 = std::fs::read_to_string(dsh_patch_of(&tmp, &profile)).unwrap();
    assert!(
        dsh_verify(&dsh_patch_of(&tmp, &profile), &dsh_creds_of(&tmp), &sp),
        "真实形状写完之后,独立读回校验也要过"
    );
    // 我们那条进去了
    let root1 = crate::yamledge::parse(&crate::yamledge::split(&patch1)).unwrap();
    let r = dsh_providers(&root1).unwrap().get("foursapi").unwrap();
    assert_eq!(r.get("baseURL").unwrap().scalar(), Some("https://4sapi.com/v1"));
    assert_eq!(r.get("apiKeyEnv").unwrap().scalar(), Some("FOURSAPI_API_KEY"));
    // 别人的一个都没动:条目数、原有 route、注释
    let before_entries = crate::yamledge::parse(&crate::yamledge::split(&patch0))
        .unwrap()
        .children
        .len();
    assert_eq!(root1.children.len(), before_entries, "顶层条目数不该变");
    assert!(patch1.contains("agent-default-model"), "别的条目必须留着");
    if patch0.contains("opencode-go") {
        assert!(patch1.contains("opencode-go"), "原有的 route 必须留着");
    }
    if patch0.starts_with('#') {
        assert!(patch1.starts_with('#'), "文件开头的注释必须留着");
    }
    let cred1 = std::fs::read_to_string(dsh_creds_of(&tmp)).unwrap();
    if had_cred {
        assert!(cred1.contains("records:"), "records 段必须留着");
    }
    assert!(cred1.contains("FOURSAPI_API_KEY"), "密钥引用要落进凭据文件");
    dump_artifacts("with-foursapi", &tmp);

    // 同一份真实形状上再写「目录里没有的那个模型」:它必须落到 `opencode-go-custom`,
    // 目录 route 一个字节都不许动。
    let custom = ModelSpec::new(
        "opencode-go",
        "minimax-m3.1-flash-free",
        "https://opencode.ai/zen/go/v1",
        "sk-go-123",
        None,
        false,
    );
    let oc = write_dsh_site(&site, &custom).unwrap();
    assert_eq!(oc.added, 1);
    let patch_custom = std::fs::read_to_string(dsh_patch_of(&tmp, &profile)).unwrap();
    assert!(
        dsh_verify(&dsh_patch_of(&tmp, &profile), &dsh_creds_of(&tmp), &custom),
        "目录外的模型写到自建 route 后,读回校验也要过"
    );
    let rootc = crate::yamledge::parse(&crate::yamledge::split(&patch_custom)).unwrap();
    let cr = dsh_providers(&rootc)
        .unwrap()
        .get("opencode-go-custom")
        .expect("目录里没有的模型要落到自建 route");
    assert_eq!(cr.get("api").unwrap().scalar(), Some("openai-completions"));
    let cn = cr.get("models").unwrap().items()[0]
        .get("name")
        .unwrap()
        .scalar()
        .unwrap()
        .to_string();
    assert_eq!(cn, "OG minimax-m3.1-flash-free");
    dump_artifacts("with-custom", &tmp);
    // 取消它,再继续原路回滚
    let occ = remove_dsh_site(&site, &custom).unwrap();
    assert_eq!(occ.removed, 1);
    assert_eq!(
        std::fs::read_to_string(dsh_patch_of(&tmp, &profile)).unwrap(),
        patch1,
        "取消目录外的模型 = 回到只写了 4SAPI 那条的样子"
    );
    assert_eq!(
        std::fs::read_to_string(dsh_creds_of(&tmp)).unwrap(),
        cred1,
        "取消目录外的模型,凭据文件也要回到原样"
    );

    // 取消写入 → 回滚它 → 回滚最初那次写入
    let o2 = remove_dsh_site(&site, &sp).unwrap();
    assert_eq!(o2.removed, 1);
    let patch2 = std::fs::read_to_string(dsh_patch_of(&tmp, &profile)).unwrap();
    assert!(dsh_model_absent(&patch2, "4sapi", "claude-sonnet-5"));
    restore(&o2.backup).unwrap();
    assert_eq!(
        std::fs::read_to_string(dsh_patch_of(&tmp, &profile)).unwrap(),
        patch1,
        "回滚这次删除 = 回到写入之后的样子"
    );
    restore(&o.backup).unwrap();
    assert_eq!(
        std::fs::read_to_string(dsh_patch_of(&tmp, &profile)).unwrap(),
        patch0,
        "回滚那次写入 = 逐字节回到原始文件"
    );
    if had_cred {
        assert_eq!(
            std::fs::read_to_string(dsh_creds_of(&tmp)).unwrap(),
            cred0,
            "凭据文件也要逐字节回到原样"
        );
    } else {
        assert!(!dsh_creds_of(&tmp).exists(), "原本没有的凭据文件要删掉");
    }
}

#[test]
fn dsh_key_ref_names_match_dsh_own_derivation() {
    assert_eq!(dsh_key_ref("opencode-go"), "OPENCODE_GO_API_KEY");
    assert_eq!(dsh_key_ref("foursapi"), "FOURSAPI_API_KEY");
    assert_eq!(dsh_key_ref("hapi"), "HAPI_API_KEY");
    // 改名:来源 `4sapi` 的 route 不能就叫 `4sapi` —— 派生出来的 `4SAPI_API_KEY`
    // 数字开头,DSH 会把整份凭据文件拒掉(连带 pi-ai 所有 route 失效)
    assert_eq!(dsh_route("4sapi"), "foursapi");
    assert_eq!(dsh_key_ref(&dsh_route("4sapi")), "FOURSAPI_API_KEY");
    assert!(
        dsh_ref_legal(&dsh_key_ref(&dsh_route("4sapi"))),
        "派生出来的引用名必须过得了 DSH 的 REF_PATTERN"
    );
    assert!(!dsh_ref_legal("4SAPI_API_KEY"), "旧的非法名字要认得出来");
    assert!(!dsh_ref_legal(""));
    assert!(dsh_ref_legal("_X"));
    // 每个来源派生出来的引用名都得合法
    for d in crate::models::SOURCES {
        assert!(
            dsh_ref_legal(&dsh_key_ref(&dsh_route(d.id))),
            "来源 {} 的 route 派生出非法引用名",
            d.id
        );
    }
    assert_eq!(dsh_route_source("foursapi").as_deref(), Some("4sapi"));
    assert_eq!(dsh_route_source("4sapi").as_deref(), Some("4sapi"), "旧写法也要认");
    assert_eq!(dsh_route_source("opencode-go-custom").as_deref(), Some("opencode-go"));
    assert_eq!(dsh_route_source("hapi").as_deref(), Some("hapi"));
    assert!(dsh_route_source("someone-else").is_none());
}

/// 目录分派:目录里**有**这个模型 → 目录 route;目录里没有 → 自建 route。
/// 这是 2026-10-20 那次失败的直接原因(目录没有的模型拿到 `needs an api`)。
#[test]
fn dsh_catalog_route_is_decided_per_model() {
    assert!(
        dsh_catalog_route("opencode-go", "deepseek-v4.1-flash"),
        "目录里有的模型:端点协议交给目录"
    );
    assert!(
        !dsh_catalog_route("opencode-go", "minimax-m3.1-flash-free"),
        "目录里没有的模型不能走目录 route"
    );
    assert_eq!(
        dsh_route_for("opencode-go", "deepseek-v4.1-flash"),
        "opencode-go"
    );
    assert_eq!(
        dsh_route_for("opencode-go", "minimax-m3.1-flash-free"),
        "opencode-go-custom",
        "目录里没有的模型要单独成一条 route"
    );
    assert_eq!(dsh_route_for("4sapi", "whatever"), "foursapi");
    assert!(!dsh_catalog_route("foursapi", "whatever"));
    // 目录表的模型数与协议都抄自 pi-ai v1.1.0 的 opencode-go.json
    let cat = dsh_catalog("opencode-go").expect("目录表里应有 opencode-go");
    assert_eq!(cat.models.len(), 30);
    assert_eq!(
        dsh_catalog_model("opencode-go", "minimax-m3").map(|c| c.1),
        Some("anthropic-messages")
    );
    assert_eq!(
        dsh_catalog_model("opencode-go", "grok-4.6").map(|c| c.1),
        Some("openai-responses")
    );
    assert_eq!(
        dsh_catalog_model("opencode-go", "deepseek-v4.1-flash").map(|c| (c.2, c.3)),
        Some((1_000_000, true))
    );
}

/// 显示名按来源区分:4SAPI / Hapi 用前缀 + id,OpenCode Go 用目录名。
#[test]
fn display_names_differ_per_source() {
    assert_eq!(
        display_name_of("4sapi", "claude-sonnet-5"),
        "4S-claude-sonnet-5"
    );
    assert_eq!(display_name_of("hapi", "gpt-6-luna"), "HP-gpt-6-luna");
    assert_eq!(
        display_name_of("opencode-go", "deepseek-v4.1-flash"),
        "OG DeepSeek V4.1 Flash"
    );
    assert_eq!(
        display_name_of("opencode-go", "minimax-m3.1-flash-free"),
        "OG minimax-m3.1-flash-free",
        "目录里没有的不编名字,退回 id"
    );
    assert_eq!(name_prefix("hapi"), "HP-");
    assert_eq!(name_prefix("opencode-go"), "OG ");
    assert!(name_is_ours("4S-x") && name_is_ours("HP-x") && name_is_ours("OG x"));
    assert!(!name_is_ours("claude-sonnet-5"));
}

/// 新增:非目录来源要写全协议 + 端点 + 引用名,密钥进凭据文件;注释与别的条目一个不动。
#[test]
fn dsh_adds_route_and_credential() {
    let home = dsh_home_at("dsh-add");
    dsh_write_fixture(&home, DSH_PATCH, DSH_CRED);
    let site = dsh_site_at(&home);
    let sp = spec(); // 4sapi / claude-sonnet-5 / https://4sapi.com/v1 / sk-test-123

    let o = write_dsh_site(&site, &sp).unwrap();
    assert_eq!((o.added, o.updated, o.same), (1, 0, false));

    let patch = dsh_read(&home, "patch");
    assert!(patch.contains("# dsh profile patch"), "注释必须留着");
    assert!(patch.contains("- id: agent-default-model"), "别的条目必须留着");
    assert!(patch.contains("OPENCODE_GO_API_KEY"), "目录里那条 route 不该被抹掉");
    let root = crate::yamledge::parse(&crate::yamledge::split(&patch)).unwrap();
    let r = dsh_providers(&root).unwrap().get("foursapi").unwrap();
    assert_eq!(r.get("api").unwrap().scalar(), Some("openai-completions"));
    assert_eq!(
        r.get("baseURL").unwrap().scalar(),
        Some("https://4sapi.com/v1")
    );
    assert_eq!(r.get("displayName").unwrap().scalar(), Some("4SAPI"));
    assert_eq!(r.get("apiKeyEnv").unwrap().scalar(), Some("FOURSAPI_API_KEY"));
    let m = r.get("models").unwrap().items();
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].get("id").unwrap().scalar(), Some("claude-sonnet-5"));
    assert_eq!(m[0].get("name").unwrap().scalar(), Some("4S-claude-sonnet-5"));
    assert_eq!(m[0].get("contextWindow").unwrap().scalar(), Some("1000000"));
    assert_eq!(
        m[0].get("input").unwrap().items().len(),
        2,
        "多模态模型要带 image"
    );

    let cred = dsh_read(&home, "cred");
    let croot = crate::yamledge::parse(&crate::yamledge::split(&cred)).unwrap();
    assert_eq!(
        croot
            .get("refs")
            .unwrap()
            .get("FOURSAPI_API_KEY")
            .unwrap()
            .scalar(),
        Some("sk-test-123")
    );
    assert_eq!(
        croot
            .get("refs")
            .unwrap()
            .get("OPENCODE_GO_API_KEY")
            .unwrap()
            .scalar(),
        Some("sk-existing"),
        "别人的引用一个都不动"
    );
    assert_eq!(croot.get("version").unwrap().scalar(), Some("1"));
    assert!(cred.contains("kind: grant"), "records 段必须原样留着");
}

/// 幂等:同样内容再写一次 —— 不动盘、不建备份、回执说「无需改动」。
#[test]
fn dsh_write_is_idempotent() {
    let home = dsh_home_at("dsh-idem");
    dsh_write_fixture(&home, DSH_PATCH, DSH_CRED);
    let site = dsh_site_at(&home);
    let sp = spec();
    write_dsh_site(&site, &sp).unwrap();
    let after1 = (dsh_read(&home, "patch"), dsh_read(&home, "cred"));
    let o2 = write_dsh_site(&site, &sp).unwrap();
    assert!(o2.same, "第二次应报「内容一致」");
    assert_eq!((o2.added, o2.updated), (0, 0));
    assert!(o2.backup.as_os_str().is_empty(), "没改动就不该建备份目录");
    assert_eq!(
        (dsh_read(&home, "patch"), dsh_read(&home, "cred")),
        after1,
        "文件必须逐字节不变"
    );
}

/// 目录里的 route(`opencode-go`):只往 models 里加一条,不给 api/baseURL,
/// 用户的 apiKeyEnv 与已有模型都留着。
#[test]
fn dsh_catalog_route_merges_without_touching_endpoint() {
    let home = dsh_home_at("dsh-catalog");
    dsh_write_fixture(&home, DSH_PATCH, DSH_CRED);
    let site = dsh_site_at(&home);
    let sp = ModelSpec::new(
        "opencode-go",
        "glm-5.3",
        "https://opencode.ai/zen/go/v1",
        "sk-go-123",
        None,
        false,
    );
    let o = write_dsh_site(&site, &sp).unwrap();
    assert_eq!(o.added, 1);

    let patch = dsh_read(&home, "patch");
    let root = crate::yamledge::parse(&crate::yamledge::split(&patch)).unwrap();
    let r = dsh_providers(&root).unwrap().get("opencode-go").unwrap();
    assert!(r.get("api").is_none(), "目录路由不该被塞 api");
    assert!(r.get("baseURL").is_none(), "目录路由不该被塞 baseURL");
    assert_eq!(
        r.get("apiKeyEnv").unwrap().scalar(),
        Some("OPENCODE_GO_API_KEY")
    );
    let ids: Vec<&str> = r
        .get("models")
        .unwrap()
        .items()
        .iter()
        .map(|x| x.get("id").unwrap().scalar().unwrap())
        .collect();
    assert_eq!(ids, vec!["deepseek-v4.1-flash", "glm-5.3"], "原有模型必须留着");
    // 目录 route 也要有显示名:目录默认叫 `opencode-go`,人看不出是哪个渠道
    assert_eq!(r.get("displayName").unwrap().scalar(), Some("OpenCode Go"));
    let m = r.get("models").unwrap().items();
    let added = m.iter().find(|x| x.get("id").and_then(|n| n.scalar()) == Some("glm-5.3")).unwrap();
    assert_eq!(
        added.get("name").unwrap().scalar(),
        Some("OG GLM-5.3"),
        "OpenCode Go 的模型用 OG 前缀 + 目录名"
    );
    assert_eq!(
        added.get("contextWindow").unwrap().scalar(),
        Some("1000000"),
        "目录里的上下文照抄"
    );
    let cred = dsh_read(&home, "cred");
    let croot = crate::yamledge::parse(&crate::yamledge::split(&cred)).unwrap();
    assert_eq!(
        croot
            .get("refs")
            .unwrap()
            .get("OPENCODE_GO_API_KEY")
            .unwrap()
            .scalar(),
        Some("sk-go-123"),
        "同名引用按这次的内容覆盖"
    );
}

/// 更新已有条目:用户自己起的名字、自己加过的字段(maxTokens / reasoningEfforts)必须留着;
/// 目录里的能力照目录写(以前固定写 `text`,会把目录的 `text+image` 写窄)。
#[test]
fn dsh_update_keeps_user_added_model_fields() {
    let home = dsh_home_at("dsh-keep");
    let patch = DSH_PATCH.replace(
        "          - id: deepseek-v4.1-flash\n            name: OG DeepSeek V4.1 Flash\n            contextWindow: 1000000\n",
        "          - id: deepseek-v4.1-flash\n            name: 我自己起的名字\n            contextWindow: 1000000\n            maxTokens: 384000\n            reasoningEfforts:\n              off:\n              high: high\n",
    );
    assert!(patch.contains("maxTokens: 384000"), "夹具本身要改成功");
    dsh_write_fixture(&home, &patch, DSH_CRED);
    let site = dsh_site_at(&home);
    let sp = ModelSpec::new(
        "opencode-go",
        "deepseek-v4.1-flash",
        "https://opencode.ai/zen/go/v1",
        "sk-go-123",
        Some(1_000_000),
        true,
    );
    let o = write_dsh_site(&site, &sp).unwrap();
    assert_eq!(o.updated, 1);
    let after = dsh_read(&home, "patch");
    assert!(after.contains("maxTokens: 384000"), "未知字段必须保留");
    assert!(after.contains("reasoningEfforts:"), "未知字段必须保留");
    assert!(after.contains("- image"), "目录里这个模型支持图片,能力要写全");
}

/// 用户手改过的模型名不许被我们覆盖(与 WorkBuddy 那条同口径)。
#[test]
fn dsh_update_keeps_user_renamed_model() {
    let home = dsh_home_at("dsh-rename");
    let patch = DSH_PATCH.replace("name: OG DeepSeek V4.1 Flash", "name: 我的主力模型");
    dsh_write_fixture(&home, &patch, DSH_CRED);
    let site = dsh_site_at(&home);
    let sp = ModelSpec::new(
        "opencode-go",
        "deepseek-v4.1-flash",
        "https://opencode.ai/zen/go/v1",
        "sk-go-123",
        Some(1_000_000),
        true,
    );
    write_dsh_site(&site, &sp).unwrap();
    let after = dsh_read(&home, "patch");
    assert!(after.contains("我的主力模型"), "用户起的名字不许改:\n{after}");
    assert!(!after.contains("OG DeepSeek V4.1 Flash"), "不该改回我们的名字");
}

/// 取消写入:最后一个模型 → route 一起收走(不留空 models),没人再用的密钥也撤掉;
/// 默认模型指着它时如实提示(但不替用户改)。
#[test]
fn dsh_remove_last_model_takes_route_and_credential() {
    let home = dsh_home_at("dsh-rm");
    dsh_write_fixture(&home, DSH_PATCH, DSH_CRED);
    let site = dsh_site_at(&home);
    let sp = ModelSpec::new(
        "4sapi",
        "claude-sonnet-5",
        "https://4sapi.com/v1",
        "sk-test-123",
        None,
        false,
    );
    write_dsh_site(&site, &sp).unwrap();
    // 把默认模型指到它上面,验证「只提示、不改」
    let patched = dsh_read(&home, "patch").replace(
        "    provider: opencode-go\n    model: deepseek-v4.1-flash",
        "    provider: foursapi\n    model: claude-sonnet-5",
    );
    std::fs::write(dsh_patch_of(&home, "desktop"), &patched).unwrap();
    assert!(
        dsh_default_points_at(&patched, "4sapi", "claude-sonnet-5"),
        "默认模型改写应被认成指向这条(旧版 route 键 `4sapi` 也要认)"
    );

    let o = remove_dsh_site(&site, &sp).unwrap();
    assert_eq!(o.removed, 1);
    assert!(
        o.notes.iter().any(|n| n.contains("默认模型")),
        "默认模型指着它必须提示:{:?}",
        o.notes
    );
    let patch = dsh_read(&home, "patch");
    let root = crate::yamledge::parse(&crate::yamledge::split(&patch)).unwrap();
    assert!(
        dsh_providers(&root).unwrap().get("foursapi").is_none(),
        "最后一个模型被摘走,route 不该留成空壳"
    );
    assert!(
        patch.contains("provider: foursapi"),
        "默认模型那几条挂件不动(只提示)"
    );
    let cred = dsh_read(&home, "cred");
    assert!(!cred.contains("FOURSAPI_API_KEY"), "没人再用的引用要撤掉");
    assert!(cred.contains("OPENCODE_GO_API_KEY"), "别人的引用不许动");
    assert!(cred.contains("kind: grant"), "records 段不许动");
}

/// 老版本用 `4sapi` 这个 route 键写进去的条目,新版本也要能取消掉、并把旧引用一并撤走。
#[test]
fn dsh_remove_cleans_up_legacy_4sapi_route() {
    let home = dsh_home_at("dsh-legacy");
    let patch = DSH_PATCH.replace(
        "      opencode-go:\n",
        "      4sapi:\n        displayName: 4SAPI\n        api: openai-completions\n        baseURL: https://4sapi.com/v1\n        apiKeyEnv: 4SAPI_API_KEY\n        models:\n          - id: claude-sonnet-5\n            name: 4S-claude-sonnet-5\n      opencode-go:\n",
    );
    let cred = DSH_CRED.replace(
        "  OPENCODE_GO_API_KEY: sk-existing",
        "  OPENCODE_GO_API_KEY: sk-existing\n  4SAPI_API_KEY: sk-test-123",
    );
    dsh_write_fixture(&home, &patch, &cred);
    let site = dsh_site_at(&home);
    let sp = ModelSpec::new(
        "4sapi",
        "claude-sonnet-5",
        "https://4sapi.com/v1",
        "sk-test-123",
        None,
        false,
    );
    // 状态里认得出这条是 4sapi 的
    assert!(dsh_read_configured(&dsh_read(&home, "patch"))
        .iter()
        .any(|m| m.id == "claude-sonnet-5" && m.source == "4sapi"));
    let o = remove_dsh_site(&site, &sp).unwrap();
    assert_eq!(o.removed, 1);
    let after = dsh_read(&home, "patch");
    let root = crate::yamledge::parse(&crate::yamledge::split(&after)).unwrap();
    assert!(dsh_providers(&root).unwrap().get("4sapi").is_none(), "旧 route 要收走");
    let after_cred = dsh_read(&home, "cred");
    assert!(!after_cred.contains("4SAPI_API_KEY"), "旧引用也要撤掉");
    assert!(after_cred.contains("OPENCODE_GO_API_KEY"), "别人的引用不许动");
}

/// 还有别的 route 在用同一个引用名 → 密钥留着不动(读回校验也要按这个口径)。
#[test]
fn dsh_remove_keeps_credential_still_used_by_another_route() {
    let home = dsh_home_at("dsh-rm2");
    let patch = DSH_PATCH.replace(
        "      opencode-go:\n",
        "      foursapi-other:\n        api: openai-completions\n        baseURL: https://4sapi.com/v1\n        apiKeyEnv: FOURSAPI_API_KEY\n        models:\n          - id: keep-me\n            name: keep\n      opencode-go:\n",
    );
    let cred = DSH_CRED.replace(
        "  OPENCODE_GO_API_KEY: sk-existing",
        "  OPENCODE_GO_API_KEY: sk-existing\n  FOURSAPI_API_KEY: sk-test-123",
    );
    dsh_write_fixture(&home, &patch, &cred);
    let site = dsh_site_at(&home);
    let sp = ModelSpec::new(
        "4sapi",
        "claude-sonnet-5",
        "https://4sapi.com/v1",
        "sk-test-123",
        None,
        false,
    );
    write_dsh_site(&site, &sp).unwrap();
    let o = remove_dsh_site(&site, &sp).unwrap();
    assert_eq!(o.removed, 1);
    assert!(
        o.notes.iter().any(|n| n.contains("还有别的 route 在用")),
        "要说明密钥为什么留着:{:?}",
        o.notes
    );
    let cred = dsh_read(&home, "cred");
    assert!(cred.contains("FOURSAPI_API_KEY"), "别的 route 还在用,密钥不能删");
}

/// 回滚:两份文件一起盖回原来的样子;写入前不存在的凭据文件,回滚后要消失。
#[test]
fn dsh_rollback_restores_both_files_and_removes_created_one() {
    let home = dsh_home_at("dsh-roll");
    // 凭据文件先不放:写入会新建它
    std::fs::write(dsh_patch_of(&home, "desktop"), DSH_PATCH).unwrap();
    let site = dsh_site_at(&home);
    let sp = spec();
    let o = write_dsh_site(&site, &sp).unwrap();
    assert!(dsh_creds_of(&home).is_file(), "写入应建出凭据文件");
    let backup = o.backup.clone();
    assert!(
        backup.starts_with(home.join("backup-inject")),
        "备份落在 harness home 下"
    );
    assert!(
        backup.join(BACKUP_LAYOUT).is_file(),
        "跨目录的备份要带落点清单"
    );

    let msg = restore(&backup).unwrap();
    assert!(msg.contains("落点清单"), "回滚走的是清单那条路:{msg}");
    assert_eq!(dsh_read(&home, "patch"), DSH_PATCH, "patch 要逐字节回到原样");
    assert!(
        !dsh_creds_of(&home).exists(),
        "写入前不存在的文件,回滚后要删掉(而不是留一份我们建的)"
    );
}

/// 结构认不出来(providers 是流式集合)→ 写入要明确拒绝,不猜、不写坏。
#[test]
fn dsh_refuses_flow_style_providers() {
    let home = dsh_home_at("dsh-flow");
    let patch =
        DSH_PATCH.replace("    providers:\n", "    providers: {opencode-go: {apiKeyEnv: X}}\n");
    dsh_write_fixture(&home, &patch, DSH_CRED);
    let e = dsh_patch_apply(&dsh_read(&home, "patch"), &spec()).unwrap_err();
    assert!(
        e.contains("认不出来") || e.contains("mapping") || e.contains("sequence"),
        "拒绝的理由要说清:{e}"
    );
    let site = dsh_site_at(&home);
    let before = dsh_read(&home, "patch");
    assert!(write_dsh_site(&site, &spec()).is_err(), "认不出来就不许写");
    assert_eq!(dsh_read(&home, "patch"), before, "拒绝时文件必须原样");
}

/// 状态读取:认得出我们写的模型(带来源)、读得到默认模型;别人的 route 不算数。
#[test]
fn dsh_state_reads_models_and_selection() {
    let home = dsh_home_at("dsh-state");
    let patch = DSH_PATCH.replace(
        "      opencode-go:\n",
        "      someone-else:\n        apiKeyEnv: OTHER_KEY\n        models:\n          - id: not-ours\n            name: nope\n      opencode-go:\n",
    );
    dsh_write_fixture(&home, &patch, DSH_CRED);
    let site = dsh_site_at(&home);
    write_dsh_site(&site, &spec()).unwrap();
    let text = dsh_read(&home, "patch");
    let rows = dsh_read_configured(&text);
    let ids: Vec<(&str, &str)> = rows
        .iter()
        .map(|m| (m.id.as_str(), m.source.as_str()))
        .collect();
    assert!(ids.contains(&("claude-sonnet-5", "4sapi")), "{ids:?}");
    assert!(
        ids.contains(&("deepseek-v4.1-flash", "opencode-go")),
        "目录里那条也是我们认识的来源:{ids:?}"
    );
    assert!(
        !ids.iter().any(|(id, _)| *id == "not-ours"),
        "别人的 route 不算我们写的:{ids:?}"
    );
    assert_eq!(
        dsh_read_selected(&text).as_deref(),
        Some("deepseek-v4.1-flash")
    );
    // 默认模型不在我们认识的来源上 → 不报「选中」
    let other = DSH_PATCH.replace("provider: opencode-go", "provider: deepseek-official");
    assert_eq!(dsh_read_selected(&other), None);
}

/// 目录里没有的 opencode-go 模型:写到 `opencode-go-custom`,自己带 api + 端点 +
/// 自己的引用名;目录 route 一个字节都不动。这正是 2026-10-20 那次「添加失败」的正解。
#[test]
fn dsh_catalog_missing_model_goes_to_custom_route() {
    let home = dsh_home_at("dsh-custom");
    dsh_write_fixture(&home, DSH_PATCH, DSH_CRED);
    let site = dsh_site_at(&home);
    let sp = ModelSpec::new(
        "opencode-go",
        "minimax-m3.1-flash-free",
        "https://opencode.ai/zen/go/v1",
        "sk-go-123",
        None,
        false,
    );
    assert_eq!(sp.display_name, "OG minimax-m3.1-flash-free");
    let o = write_dsh_site(&site, &sp).unwrap();
    assert_eq!(o.added, 1);
    assert!(
        dsh_verify(&dsh_patch_of(&home, "desktop"), &dsh_creds_of(&home), &sp),
        "自建 route 也要过读回校验"
    );

    let patch = dsh_read(&home, "patch");
    let root = crate::yamledge::parse(&crate::yamledge::split(&patch)).unwrap();
    let providers = dsh_providers(&root).unwrap();
    let r = providers.get("opencode-go-custom").expect("应新增自建 route");
    assert_eq!(r.get("api").unwrap().scalar(), Some("openai-completions"));
    assert_eq!(
        r.get("baseURL").unwrap().scalar(),
        Some("https://opencode.ai/zen/go/v1")
    );
    assert_eq!(
        r.get("apiKeyEnv").unwrap().scalar(),
        Some("OPENCODE_GO_CUSTOM_API_KEY"),
        "引用名跟着 route 走,且一定是合法名字"
    );
    assert!(dsh_ref_legal("OPENCODE_GO_CUSTOM_API_KEY"));
    assert_eq!(r.get("displayName").unwrap().scalar(), Some("OpenCode Go"));
    let m = r.get("models").unwrap().items();
    assert_eq!(m.len(), 1);
    assert_eq!(
        m[0].get("id").unwrap().scalar(),
        Some("minimax-m3.1-flash-free")
    );
    assert!(
        m[0].get("input").is_none(),
        "目录里没有的模型不编能力(默认 text 由 pi-ai 给)"
    );
    // 目录 route 没被动过
    let og = providers.get("opencode-go").unwrap();
    assert_eq!(og.get("models").unwrap().items().len(), 1, "目录 route 不该长模型");

    // 取消写入:自建 route 收走,引用也撤掉
    let o2 = remove_dsh_site(&site, &sp).unwrap();
    assert_eq!(o2.removed, 1);
    let after = dsh_read(&home, "patch");
    let root2 = crate::yamledge::parse(&crate::yamledge::split(&after)).unwrap();
    assert!(
        dsh_providers(&root2).unwrap().get("opencode-go-custom").is_none(),
        "最后一个模型走人,自建 route 一起收"
    );
    let cred = dsh_read(&home, "cred");
    assert!(!cred.contains("OPENCODE_GO_CUSTOM_API_KEY"));
    assert!(cred.contains("OPENCODE_GO_API_KEY"), "目录 route 的引用不许动");
}

/// 目录里的模型与新写法并存的两种节点:同名模型在两条 route 上,状态按来源合并去重。
#[test]
fn dsh_state_merges_route_and_custom_route() {
    let home = dsh_home_at("dsh-state2");
    dsh_write_fixture(&home, DSH_PATCH, DSH_CRED);
    let site = dsh_site_at(&home);
    let a = ModelSpec::new(
        "opencode-go",
        "glm-5.3",
        "https://opencode.ai/zen/go/v1",
        "sk-go-123",
        None,
        false,
    );
    let b = ModelSpec::new(
        "opencode-go",
        "step-5-preview",
        "https://opencode.ai/zen/go/v1",
        "sk-go-123",
        None,
        false,
    );
    write_dsh_site(&site, &a).unwrap();
    write_dsh_site(&site, &b).unwrap();
    let rows = dsh_read_configured(&dsh_read(&home, "patch"));
    for id in ["glm-5.3", "step-5-preview", "deepseek-v4.1-flash"] {
        assert_eq!(
            rows.iter().filter(|m| m.id == id && m.source == "opencode-go").count(),
            1,
            "{id} 应恰好出现一次:{rows:?}"
        );
    }
}

/// 站点发现:harness home 有 profile 才算装了;优先 desktop。
#[test]
fn dsh_site_discovery_prefers_desktop() {
    let home = dsh_home_at("dsh-site");
    let work = home.join("profiles").join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("cordis.yml"), "[]").unwrap();
    std::fs::write(dsh_patch_of(&home, "desktop"), DSH_PATCH).unwrap();
    assert_eq!(dsh_profile(&home).as_deref(), Some("desktop"));
    // 只有 work 一个 → 挑它;啥都没有 → 没装
    let home2 = dsh_home_at("dsh-site2");
    let work2 = home2.join("profiles").join("work");
    std::fs::create_dir_all(&work2).unwrap();
    std::fs::write(work2.join("cordis.yml"), "[]").unwrap();
    assert_eq!(dsh_profile(&home2).as_deref(), Some("work"));
    let empty = tmpdir("dsh-site3");
    assert_eq!(dsh_profile(&empty), None);
}
