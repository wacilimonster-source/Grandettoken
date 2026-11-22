use super::*;

pub(super) fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "ts-inject-{}-{}-{}",
        tag,
        std::process::id(),
        chrono::Local::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub(super) fn spec() -> ModelSpec {
    ModelSpec::new(
        "4sapi",
        "claude-sonnet-5",
        "https://4sapi.com/v1",
        "sk-test-123",
        Some(1_000_000),
        true,
    )
}

pub(super) fn site_at(root: &Path, client: Client) -> Site {
    Site {
        client,
        label: client.label().into(),
        running: false,
        process: client.process().into(),
        root: root.to_path_buf(),
        files: vec![root.join("models.json"), root.join("settings.json")],
    }
}

/// 测试里读回文件一律走 read_json(它会剥 BOM)—— 我们写 WorkBuddy 的文件时
/// 是照原样保留 BOM 的,直接 from_str 读会失败。
pub(super) fn read_v(path: &Path) -> Value {
    read_json(path).unwrap().unwrap()
}

/// tasklist 的输出按 CSV 首列判,不认本地化提示文案。
#[test]
fn tasklist_matching_is_locale_independent() {
    let zh = "信息: 没有运行的任务匹配指定标准。\r\n";
    assert!(!tasklist_has(zh, "WorkBuddy.exe"));
    let yes = "\"ZCode.exe\",\"28512\",\"Console\",\"1\",\"180,640 K\"\r\n";
    assert!(tasklist_has(yes, "ZCode.exe"));
    assert!(tasklist_has(yes, "zcode.exe"), "进程名不区分大小写");
    assert!(!tasklist_has(yes, "WorkBuddy.exe"));
}

/// WorkBuddy:新增条目 — 字段与用户手配的那条对齐(带 BOM、2 空格)。
#[test]
fn workbuddy_adds_entry_with_our_fields() {
    let root = tmpdir("wb-add");
    std::fs::write(root.join("models.json"), "\u{feff}[\n]\n").unwrap();
    std::fs::write(root.join("settings.json"), "{\n  \"sandbox\": true\n}\n").unwrap();
    let site = site_at(&root, Client::Workbuddy);

    let o = write_workbuddy_site(&site, &spec(), &Opts::default()).unwrap();
    assert_eq!((o.added, o.updated), (1, 0));

    let v = read_v(&root.join("models.json"));
    let e = &v.as_array().unwrap()[0];
    assert_eq!(e["id"], "claude-sonnet-5");
    assert_eq!(e["name"], "4S-claude-sonnet-5");
    assert_eq!(e["vendor"], "Custom");
    assert_eq!(e["url"], "https://4sapi.com/v1");
    assert_eq!(e["apiKey"], "sk-test-123");
    assert_eq!(e["maxInputTokens"], 1_000_000);
    assert_eq!(e["supportsImages"], true);
    // BOM 保留(照原文件习惯写回)
    let raw = std::fs::read(root.join("models.json")).unwrap();
    assert!(raw.starts_with(&[0xEF, 0xBB, 0xBF]), "BOM 要保留");

    // 幂等:同样内容再写一次,不新增、不重复、不写盘
    let o2 = write_workbuddy_site(&site, &spec(), &Opts::default()).unwrap();
    assert!((o2.added, o2.updated) == (0, 0) && o2.same, "内容一致应报「无需改动」");
    let v2 = read_v(&root.join("models.json"));
    assert_eq!(v2.as_array().unwrap().len(), 1, "不能写出重复条目");
}

/// 学到前缀后要写选中项并如实回执;没学到就只写条目(不猜)。
#[test]
fn workbuddy_selection_only_when_prefix_learned() {
    let root = tmpdir("wb-sel");
    std::fs::write(root.join("models.json"), "[]").unwrap();
    std::fs::write(root.join("settings.json"), "{\"model\":\"custom-local:qwen:2b\"}").unwrap();
    let site = site_at(&root, Client::Workbuddy);

    let opts = Opts {
        selection_prefix: learn_prefix(Some("custom-local:qwen:2b"), &[]),
        ..Default::default()
    };
    assert_eq!(opts.selection_prefix.as_deref(), Some("custom-local:"));
    let o = write_workbuddy_site(&site, &spec(), &opts).unwrap();
    assert!(o.selected, "学到前缀就该写选中项");
    let s = read_v(&root.join("settings.json"));
    assert_eq!(s["model"], "custom-local:claude-sonnet-5");
    assert_eq!(s["sandbox"], Value::Null, "没动过的键不该冒出来");

    // 没学到前缀:只写条目
    let root2 = tmpdir("wb-nosel");
    std::fs::write(root2.join("models.json"), "[]").unwrap();
    std::fs::write(root2.join("settings.json"), "{\"model\":\"gpt-6-luna\"}").unwrap();
    let site2 = site_at(&root2, Client::Workbuddy);
    let o2 = write_workbuddy_site(&site2, &spec(), &Opts::default()).unwrap();
    assert!(!o2.selected);
    let s2 = read_v(&root2.join("settings.json"));
    assert_eq!(s2["model"], "gpt-6-luna", "学不到口径就不动选中项");
}

/// 前缀口径的三种证据:带前缀 → 学到前缀;裸 id 命中我们的条目 → 学到无前缀;
/// 内置模型 → 学不到(不猜)。
#[test]
fn prefix_learning_rules() {
    let ours = vec!["claude-sonnet-5".to_string()];
    assert_eq!(learn_prefix(Some("custom-local:x"), &ours).as_deref(), Some("custom-local:"));
    assert_eq!(learn_prefix(Some("claude-sonnet-5"), &ours).as_deref(), Some(""));
    assert_eq!(learn_prefix(Some("gpt-6-luna"), &ours), None);
    assert_eq!(learn_prefix(Some(""), &ours), None);
    assert_eq!(learn_prefix(None, &ours), None);
    assert_eq!(strip_prefix("custom-local:a/b:2b"), "a/b:2b");
    assert_eq!(strip_prefix("plain"), "plain");
}

/// 同名 id 属于别人(另一个中转站)时要报冲突 —— 但要改还是得改,只是界面必须说清。
#[test]
fn workbuddy_flags_foreign_entry_on_same_id() {
    let root = tmpdir("wb-foreign");
    std::fs::write(
        root.join("models.json"),
        r#"[{"id":"deepseek-v4.1-flash","name":"GO-deepseek","url":"https://opencode.ai/zen/go/v1","apiKey":"old"}]"#,
    )
    .unwrap();
    std::fs::write(root.join("settings.json"), "{}").unwrap();
    let site = site_at(&root, Client::Workbuddy);
    let s = ModelSpec::new("4sapi", "deepseek-v4.1-flash", "https://4sapi.com/v1", "sk-new", None, false);
    let o = write_workbuddy_site(&site, &s, &Opts::default()).unwrap();
    assert_eq!(o.updated, 1);
    assert_eq!(
        o.foreign_conflict.as_deref(),
        Some("https://opencode.ai/zen/go/v1")
    );
    // 更新后:url 换成我们的,但用户起的名字保留
    let v = read_v(&root.join("models.json"));
    assert_eq!(v[0]["url"], "https://4sapi.com/v1");
    assert_eq!(v[0]["name"], "GO-deepseek", "更新不改用户起的名字");
}

/// 结构不认识(不是数组)必须拒绝,不能硬写。
#[test]
fn workbuddy_refuses_unknown_shape() {
    let root = tmpdir("wb-shape");
    std::fs::write(root.join("models.json"), "{\"models\":[]}").unwrap();
    std::fs::write(root.join("settings.json"), "{}").unwrap();
    let site = site_at(&root, Client::Workbuddy);
    let e = write_workbuddy_site(&site, &spec(), &Opts::default()).unwrap_err();
    assert!(e.contains("不是数组"), "{e}");
}

/// 备份 + 回滚:回滚后文件与写入前逐字节一致。
#[test]
fn backup_and_rollback_restore_bytes() {
    let root = tmpdir("rollback");
    let f = root.join("models.json");
    std::fs::write(&f, "\u{feff}[{\"id\":\"keep\"}]\n").unwrap();
    let before = std::fs::read(&f).unwrap();

    let dir = backup(&root, &[f.clone()], "20260101-000000").unwrap();
    std::fs::write(&f, "[]").unwrap();
    assert_ne!(std::fs::read(&f).unwrap(), before);

    restore(&dir).unwrap();
    assert_eq!(std::fs::read(&f).unwrap(), before, "回滚要逐字节一致");
}

/// 2026-11-12 加密:新快照是密文、升级前的明文快照被原地加密,且旧快照仍能回滚。
#[test]
fn backup_seals_snapshots_and_migrates_legacy_plaintext() {
    let root = tmpdir("backup-sealed");
    let f = root.join("models.json");
    let plain = br#"{"apiKey":"sk-LEGACY-SECRET","models":[]}"#.to_vec();
    std::fs::write(&f, &plain).unwrap();

    // 手工造一份「升级前」的明文快照:目录名是合法 stamp,文件内容没有 MAGIC。
    let legacy = root.join("backup-inject").join("20260101-000000");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("models.json"), &plain).unwrap();
    assert!(!crate::backup_crypto::is_sealed(
        &std::fs::read(legacy.join("models.json")).unwrap()
    ));

    // 再建一份新备份,触发存量迁移。
    let fresh = backup(&root, &[f.clone()], "20260101-000001").unwrap();
    for dir in [&legacy, &fresh] {
        let raw = std::fs::read(dir.join("models.json")).unwrap();
        assert!(
            crate::backup_crypto::is_sealed(&raw),
            "快照必须是加密容器: {}",
            pretty(dir)
        );
        assert!(
            !raw.windows(plain.len()).any(|w| w == plain.as_slice()),
            "磁盘上不能残留明文密钥: {}",
            pretty(dir)
        );
    }

    // 老快照仍要能回滚(按文件头兼容),且逐字节一致。
    std::fs::write(&f, b"[]").unwrap();
    restore(&legacy).unwrap();
    assert_eq!(std::fs::read(&f).unwrap(), plain, "旧明文快照迁移后仍能回滚");
}

/// 读回校验不过要自动回滚(模拟:写完立刻把文件改坏,校验就该发现)。
#[test]
fn failed_verification_rolls_back() {
    let root = tmpdir("verify");
    let f = root.join("models.json");
    std::fs::write(&f, "[]").unwrap();
    let before = std::fs::read(&f).unwrap();
    let site = site_at(&root, Client::Workbuddy);
    let res = commit(
        &site,
        &[f.clone()],
        "20260101-000001",
        || {
            std::fs::write(&f, "[{\"id\":\"half\"}]").unwrap();
            Ok(())
        },
        || false,
    );
    assert!(res.is_err());
    assert_eq!(std::fs::read(&f).unwrap(), before, "校验失败必须回滚");
    assert!(res.unwrap_err().contains("已回滚"));
}

/// 备份最多留 5 份。
#[test]
fn backups_are_pruned_to_five() {
    let root = tmpdir("prune");
    let f = root.join("models.json");
    std::fs::write(&f, "[]").unwrap();
    for i in 0..8 {
        backup(&root, &[f.clone()], &format!("2026010{}-00000{i}", i)).unwrap();
    }
    let n = std::fs::read_dir(root.join("backup-inject")).unwrap().count();
    assert_eq!(n, KEEP_BACKUPS);
}

/// ZCode:新增 provider(config.json + provider_config.json 两处都要有),
/// 重复写入复用同一个 provider,不堆出多个「4SAPI」。
#[test]
fn zcode_creates_provider_once_and_merges_models() {
    let root = tmpdir("zcode");
    std::fs::write(
        root.join("config.json"),
        r#"{"provider":{"existing-uuid":{"name":"OG","kind":"openai-compatible","options":{"apiKey":"k","baseURL":"https://opencode.ai/zen/go/v1"},"source":"custom","models":{}}}}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("provider_config.json"),
        r#"{"schemaVersion":1,"config":{"providerConfigRules":{"providerRules":[]},"modelConfigRules":{"providerModelRules":[],"manualProviderModelRules":[]}}}"#,
    )
    .unwrap();
    let site = site_at(&root, Client::Zcode);

    let o = write_zcode_site(&site, &spec()).unwrap();
    assert_eq!(o.added, 1);
    let cfg: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("config.json")).unwrap()).unwrap();
    let pid = cfg["provider"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, v)| v["name"] == "4SAPI")
        .map(|(k, _)| k.clone())
        .expect("应新建 4SAPI provider");
    assert_ne!(pid, "existing-uuid", "不能动人家的 provider");
    assert_eq!(cfg["provider"][&pid]["options"]["baseURL"], "https://4sapi.com/v1");
    assert_eq!(
        cfg["provider"][&pid]["models"]["claude-sonnet-5"]["limit"]["context"],
        1_000_000
    );
    assert!(
        cfg["provider"][&pid]["models"]["claude-sonnet-5"]
            .get("limit")
            .unwrap()
            .get("output")
            .is_none(),
        "不给 output —— 不编造能力上限"
    );

    let rules: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("provider_config.json")).unwrap(),
    )
    .unwrap();
    let rule = &rules["config"]["providerConfigRules"]["providerRules"][0];
    assert_eq!(rule["providerId"], pid.as_str());
    assert_eq!(rule["config"]["personalModelIds"][0], "claude-sonnet-5");
    assert_eq!(
        rules["config"]["modelConfigRules"]["providerModelRules"][0]["config"]["properties"]
            ["contextWindow"],
        1_000_000
    );

    // 再写第二个模型:同一个 provider,模型合并
    let s2 = ModelSpec::new("4sapi", "glm-5.3", "https://4sapi.com/v1", "sk-test-123", Some(1_000_000), false);
    let o2 = write_zcode_site(&site, &s2).unwrap();
    assert_eq!((o2.added, o2.updated), (0, 1));
    let cfg2: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("config.json")).unwrap()).unwrap();
    let n = cfg2["provider"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, v)| v["name"] == "4SAPI")
        .count();
    assert_eq!(n, 1, "只能有一个 4SAPI provider");
    assert_eq!(cfg2["provider"][&pid]["models"].as_object().unwrap().len(), 2);
    let rules2: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("provider_config.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        rules2["config"]["providerConfigRules"]["providerRules"][0]["config"]["modelOrder"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

/// 被用户删过的模型要解禁(deletedModels),否则写进去也不显示。
#[test]
fn zcode_unblocks_deleted_model() {
    let root = tmpdir("zcode-deleted");
    std::fs::write(
        root.join("config.json"),
        r#"{"provider":{"u1":{"name":"4SAPI","kind":"openai-compatible","options":{"apiKey":"old","baseURL":"https://old/v1"},"source":"custom","models":{},"zcode":{"deletedModels":["claude-sonnet-5"]}}}}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("provider_config.json"),
        r#"{"schemaVersion":1,"config":{"providerConfigRules":{"providerRules":[{"providerId":"u1","providerName":"4SAPI","config":{"group":"standard-personal","access":{"type":"api-key","apiKey":"old"},"api":{"type":"openai-chat-completions","baseUrl":"https://old/v1"},"personalModelIds":[],"modelOrder":[]}}]},"modelConfigRules":{"providerModelRules":[]}}}"#,
    )
    .unwrap();
    let site = site_at(&root, Client::Zcode);
    write_zcode_site(&site, &spec()).unwrap();
    let cfg: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("config.json")).unwrap()).unwrap();
    assert!(cfg["provider"]["u1"]["zcode"]["deletedModels"]
        .as_array()
        .unwrap()
        .is_empty(), "同名模型应被解禁");
    assert_eq!(cfg["provider"]["u1"]["options"]["apiKey"], "sk-test-123");
}

/// schemaVersion 不认识就放弃(ZCode 改版后不能替用户改坏)。
#[test]
fn zcode_refuses_unknown_schema() {
    let root = tmpdir("zcode-schema");
    std::fs::write(root.join("config.json"), r#"{"provider":{}}"#).unwrap();
    std::fs::write(
        root.join("provider_config.json"),
        r#"{"schemaVersion":2,"config":{}}"#,
    )
    .unwrap();
    let site = site_at(&root, Client::Zcode);
    let e = write_zcode_site(&site, &spec()).unwrap_err();
    assert!(e.contains("schemaVersion"), "{e}");
}

/// Trae:写入自定义条目到每个 agent 组;重复写是更新;实验开关打开时写选中项。
#[test]
fn trae_writes_entry_into_every_agent_group() {
    let root = tmpdir("trae");
    let db = root.join("state.vscdb");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB)", [])
            .unwrap();
        let map = json!({
            "solo_agent": [
                {"config_source":1,"provider":"","name":"Doubao-Seed","display_name":"Doubao-Seed","selectable":true}
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

    let o = trae_write(&db, &spec(), &Opts::default()).unwrap();
    assert_eq!((o.added, o.updated), (2, 0), "两个 agent 组各加一条");
    assert!(!o.selected, "实验开关关着时不写选中项");

    let conn = rusqlite::Connection::open(&db).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT value FROM ItemTable WHERE key LIKE '%model_list_map'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let v: Value = serde_json::from_str(&raw).unwrap();
    let e = &v["solo_agent"][1];
    assert_eq!(e["display_name"], "claude-sonnet-5");
    assert_eq!(e["provider"], "custom_openai_compatible");
    assert_eq!(e["base_url"], "https://4sapi.com/v1/chat/completions");
    assert_eq!(e["ak"], "sk-test-123");
    assert_eq!(e["config_source"], 3);
    let cid = e["custom_model_id"].as_str().unwrap().to_string();
    assert!(cid.parse::<u64>().is_ok(), "custom_model_id 要是个数字串");

    // 再写一次:更新而不是重复追加,且保留原 custom_model_id
    let o2 = trae_write(&db, &spec(), &Opts::default()).unwrap();
    assert_eq!((o2.added, o2.updated), (0, 2));
    let raw2: String = conn
        .query_row(
            "SELECT value FROM ItemTable WHERE key LIKE '%model_list_map'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let v2: Value = serde_json::from_str(&raw2).unwrap();
    assert_eq!(v2["solo_agent"].as_array().unwrap().len(), 2, "不能重复追加");
    assert_eq!(v2["solo_agent"][1]["custom_model_id"], cid.as_str());

    // 打开实验开关:选中项按 <agent>_<config_source>_<provider>_<name>_<id> 写
    let opts = Opts {
        trae_auto_select: true,
        ..Opts::default()
    };
    let o3 = trae_write(&db, &spec(), &opts).unwrap();
    assert!(o3.selected);
    let sel: String = conn
        .query_row(
            "SELECT value FROM ItemTable WHERE key LIKE '%selection%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let sv: Value = serde_json::from_str(&sel).unwrap();
    assert_eq!(
        sv["solo_agent"]["modelId"],
        format!("solo_agent_3_custom_openai_compatible_custom_openai_compatible//claude-sonnet-5_{cid}")
    );
}

/// 破译出来的 modelId 口径必须能还原这两个真样本(一个内置、一个自定义)。
#[test]
fn trae_model_id_matches_real_samples() {
    // 真样本①(内置,取自 TRAE SOLO CN):solo_agent_lite_1__DeepSeek-V4-Flash-Official_null
    assert_eq!(
        trae_model_id("solo_agent_lite", "DeepSeek-V4-Flash-Official", "", 1, None),
        "solo_agent_lite_1__DeepSeek-V4-Flash-Official_null"
    );
    // 真样本②(自定义,取自 Trae CN):
    // solo_agent_3_custom_openai_compatible_custom_openai_compatible//claude-sonnet-5_2607458050
    assert_eq!(
        trae_model_id(
            "solo_agent",
            "custom_openai_compatible//claude-sonnet-5",
            "custom_openai_compatible",
            3,
            Some("2607458050")
        ),
        "solo_agent_3_custom_openai_compatible_custom_openai_compatible//claude-sonnet-5_2607458050"
    );
    // 反向:从 modelId 取回模型名
    assert_eq!(
        trae_selected_model(
            "solo_agent_3_custom_openai_compatible_custom_openai_compatible//claude-sonnet-5_2607458050"
        )
        .as_deref(),
        Some("claude-sonnet-5")
    );
    assert_eq!(trae_selected_model("solo_agent_1__Doubao-Seed_null"), None, "内置模型不是我们的");
}

/// Trae 没有模型表 → 拒绝(版本变了就别硬写)。
#[test]
fn trae_refuses_when_table_missing() {
    let root = tmpdir("trae-empty");
    let db = root.join("state.vscdb");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB)", [])
        .unwrap();
    drop(conn);
    let e = trae_write(&db, &spec(), &Opts::default()).unwrap_err();
    assert!(e.contains("没找到 Trae 的模型表"), "{e}");
}

/// 生成的 uuid 形状要对(8-4-4-4-12 且版本位是 4),且两次不同。
#[test]
fn uuid_shape_and_uniqueness() {
    let a = new_uuid();
    let b = new_uuid();
    assert_ne!(a, b);
    let parts: Vec<&str> = a.split('-').collect();
    assert_eq!(
        parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
        vec![8, 4, 4, 4, 12]
    );
    assert!(a.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
    assert_eq!(&a[14..15], "4", "version 4");
}

/// custom_model_id 要避开已有的号。
#[test]
fn custom_model_id_avoids_collisions() {
    let existing = vec![1u64, 2, 3];
    let id: u64 = next_custom_model_id(&existing).parse().unwrap();
    assert!(!existing.contains(&id));
    let big = vec![9_999_999_999u64];
    let id2: u64 = next_custom_model_id(&big).parse().unwrap();
    assert!(!big.contains(&id2));
}

/// F11:认"这条是我们写的"必须**精确比主机名**(或整串等于我们会写的那个 base_url),
/// `4S-` 名字前缀**只在 url 空/缺时**才作数。以前是 `url.contains(host)` +
/// "名字带 4S- 就算":`mirror-4sapi.com`、`not4sapi.com.evil` 都算成我们的,
/// 「取消写入」于是会删掉用户自己的条目、写入时也不再报同名冲突。
///
/// (旧断言 `url=https://x/y, name=4S-foo → ours` 就是那个宽松口径 —— 已按新规矩改掉。)
#[test]
fn ownership_requires_an_exact_host_or_our_own_base_url() {
    let hosts = vec!["4sapi.com".into(), "4sapi.cn".into()];
    let base = "https://4sapi.com/v1";
    assert!(wb_is_ours(&json!({"url":"https://4sapi.cn/v1"}), &hosts, base));
    assert!(
        wb_is_ours(&json!({"url":"https://4SAPI.com/v1"}), &hosts, base),
        "主机名比对不区分大小写"
    );
    assert!(wb_is_ours(
        &json!({"url":"https://user:pw@4sapi.com:8443/v1"}),
        &hosts,
        base
    ), "userinfo / 端口 / 路径都要剥掉");
    // 形近域名一律不是
    assert!(!wb_is_ours(&json!({"url":"https://mirror-4sapi.com/v1"}), &hosts, base));
    assert!(!wb_is_ours(&json!({"url":"https://not4sapi.com.evil/v1"}), &hosts, base));
    assert!(!wb_is_ours(&json!({"url":"https://evil.com/?next=4sapi.com"}), &hosts, base));
    // 用户把 model_base_url 配到自定义域名:整串等于我们会写的地址 → 是我们的
    let custom = "https://my-proxy.example/v1";
    assert!(wb_is_ours(&json!({"url":"https://my-proxy.example/v1"}), &hosts, custom));
    assert!(!wb_is_ours(&json!({"url":"https://my-proxy.example/v1/other"}), &hosts, custom));
    // url 空/缺:只剩名字前缀可判
    assert!(wb_is_ours(&json!({"name":"4S-foo"}), &hosts, base));
    assert!(wb_is_ours(&json!({"url":"","name":"4S-foo"}), &hosts, base));
    // 地址指向别家时,名字带 4S- 也不是我们的(用户自己起的名字照样保留)
    assert!(!wb_is_ours(
        &json!({"url":"https://opencode.ai/zen/go/v1","name":"4S-foo"}),
        &hosts,
        base
    ));
}

/// F11(Trae 侧):与 WorkBuddy 共用同一套判据 —— 精确主机名,或整串等于
/// 我们会写下去的那个地址(Trae 写的是 `…/chat/completions`)。
#[test]
fn trae_ownership_uses_the_same_exact_host_rule() {
    let hosts = vec!["4sapi.com".into()];
    let base = "https://4sapi.com/v1";
    let ours = |url: &str| {
        json!({"provider":"custom_openai_compatible",
               "display_name":"claude-sonnet-5","base_url":url})
    };
    assert!(trae_is_ours(&ours("https://4sapi.com/v1/chat/completions"), "claude-sonnet-5", &hosts, base));
    assert!(trae_is_ours(&ours("https://4sapi.com/v1"), "claude-sonnet-5", &hosts, base));
    assert!(!trae_is_ours(&ours("https://mirror-4sapi.com/v1/chat/completions"), "claude-sonnet-5", &hosts, base));
    assert!(!trae_is_ours(&ours("https://not4sapi.com.evil/v1"), "claude-sonnet-5", &hosts, base));
    // 自定义域名(base_url 就是我们会写的那个)→ 是我们的
    let custom = "https://my-proxy.example/v1";
    assert!(trae_is_ours(&ours("https://my-proxy.example/v1/chat/completions"), "claude-sonnet-5", &hosts, custom));
    // 别的模型名不吃这套
    assert!(!trae_is_ours(&ours("https://4sapi.com/v1"), "别的模型", &hosts, base));
}

/// F11:形近域名上的条目是**用户自己的**,「取消写入」一条都不许删,
/// 预告也不许报"已配置"。
#[test]
fn workbuddy_remove_keeps_an_entry_on_a_lookalike_host() {
    let root = tmpdir("wb-rm-lookalike");
    std::fs::write(
        root.join("models.json"),
        r#"[{"id":"glm-5.3","name":"4S-glm-5.3","url":"https://mirror-4sapi.com/v1"},
                {"id":"glm-5.3","name":"4S-mine","url":"https://not4sapi.com.evil/v1"}]"#,
    )
    .unwrap();
    std::fs::write(root.join("settings.json"), "{}").unwrap();
    let site = site_at(&root, Client::Workbuddy);
    let s = ModelSpec::new("4sapi", "glm-5.3", "https://4sapi.com/v1", "sk-test-123", None, false);
    let o = remove_workbuddy_site(&site, &s).unwrap();
    assert_eq!(o.removed, 0, "形近域名不是我们的,一条都不该删");
    assert_eq!(read_v(&root.join("models.json")).as_array().unwrap().len(), 2);
    assert!(!wb_removal(&site, &s).0, "也不该报「已配置」");
}

/// F14:读回校验必须逐字段比(id / url / apiKey / name),不能只看写前就已成立的
/// id+url —— 截断/串码/密钥没落盘(或被客户端抹掉)都要判失败。
#[test]
fn workbuddy_verify_detects_a_write_whose_key_did_not_land() {
    let root = tmpdir("wb-verify");
    let models = root.join("models.json");
    let settings = root.join("settings.json");
    let expect = json!({"id":"claude-sonnet-5","name":"4S-claude-sonnet-5",
                        "url":"https://4sapi.com/v1","apiKey":"sk-test-123"});
    std::fs::write(&models, json!([expect.clone()]).to_string()).unwrap();
    std::fs::write(&settings, "{}").unwrap();
    assert!(wb_verify(&models, &settings, &expect, None), "写对了就该过");

    let mut broken = expect.clone();
    broken["apiKey"] = json!("sk-别的");
    std::fs::write(&models, json!([broken]).to_string()).unwrap();
    assert!(!wb_verify(&models, &settings, &expect, None), "密钥没落上必须判失败");

    let mut renamed = expect.clone();
    renamed["name"] = json!("别人起的名字");
    std::fs::write(&models, json!([renamed]).to_string()).unwrap();
    assert!(!wb_verify(&models, &settings, &expect, None), "名字不对也要判失败");

    std::fs::write(&models, json!([expect.clone()]).to_string()).unwrap();
    std::fs::write(&settings, "{\"model\":\"custom-local:别的东西\"}").unwrap();
    assert!(
        !wb_verify(&models, &settings, &expect, Some("custom-local:claude-sonnet-5")),
        "写过的选中项没落上也要判失败"
    );
    std::fs::write(&settings, "{\"model\":\"custom-local:claude-sonnet-5\"}").unwrap();
    assert!(wb_verify(&models, &settings, &expect, Some("custom-local:claude-sonnet-5")));
}

/// F14 端到端:一次"apiKey 没落上"的写入必须被读回校验拦住并回滚。
#[test]
fn workbuddy_commit_rolls_back_when_the_key_did_not_land() {
    let root = tmpdir("wb-verify-rb");
    let models = root.join("models.json");
    let settings = root.join("settings.json");
    std::fs::write(&models, "[]").unwrap();
    std::fs::write(&settings, "{}").unwrap();
    let before = std::fs::read(&models).unwrap();
    let site = site_at(&root, Client::Workbuddy);
    let expect = json!({"id":"claude-sonnet-5","name":"4S-claude-sonnet-5",
                        "url":"https://4sapi.com/v1","apiKey":"sk-test-123"});
    let res = commit(
        &site,
        &[models.clone(), settings.clone()],
        "20260101-000000",
        || {
            // 模拟截断/串码:entry 写下去了,apiKey 丢了
            std::fs::write(
                &models,
                r#"[{"id":"claude-sonnet-5","name":"4S-claude-sonnet-5","url":"https://4sapi.com/v1"}]"#,
            )
            .unwrap();
            Ok(())
        },
        || wb_verify(&models, &settings, &expect, None),
    );
    assert!(res.is_err(), "apiKey 没落上必须判校验失败");
    assert_eq!(std::fs::read(&models).unwrap(), before, "校验不过要回滚");
}

/// F12:`-N`(同秒撞车时 backup 自己造的序号)也算备份名,必须一起清理 ——
/// 以前只认 15 位,带序号的快照**永远不会被删**,历史备份无上限地堆积。
#[test]
fn prune_keeps_five_and_removes_numbered_backups() {
    let root = tmpdir("prune-n");
    let base = root.join("backup-inject");
    for n in [
        "20260101-000001",
        "20260101-000001-2",
        "20260101-000001-3",
        "20260102-000001",
        "20260103-000001",
        "20260104-000001",
        "20260105-000001",
        "20260106-000001",
    ] {
        std::fs::create_dir_all(base.join(n)).unwrap();
    }
    prune_backups(&root);
    let left: Vec<String> = std::fs::read_dir(&base)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(left.len(), KEEP_BACKUPS, "只留 5 份: {left:?}");
    assert!(
        !left.iter().any(|n| n.ends_with("-2") || n.ends_with("-3")),
        "带序号的那几份在最旧的一端,该被清掉: {left:?}"
    );
    assert!(is_stamp_name("20260101-000001"));
    assert!(is_stamp_name("20260101-000001-2"));
    assert!(is_stamp_name("20260101-000001-99"));
    assert!(!is_stamp_name("20260101-000001-1"), "-1 不是我们造的");
    assert!(!is_stamp_name("20260101-000001-100"));
    assert!(!is_stamp_name("20260101-000001x"));
    assert!(!is_stamp_name("我的备份"));
}

/// F13:restore 只认 `<root>/backup-inject/<stamp>/`。
#[test]
fn restore_refuses_a_directory_outside_backup_inject() {
    let root = tmpdir("restore-guard");
    let outside = root.join("随便一个目录");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("models.json"), "[]").unwrap();
    let e = restore(&outside).unwrap_err();
    assert!(e.contains("backup-inject"), "{e}");

    // 在 backup-inject 下,但名字不是时间戳形态
    let odd = root.join("backup-inject").join("我的备份");
    std::fs::create_dir_all(&odd).unwrap();
    std::fs::write(odd.join("models.json"), "[]").unwrap();
    let e2 = restore(&odd).unwrap_err();
    assert!(e2.contains("时间戳"), "{e2}");
    assert!(!root.join("models.json").exists(), "被拒时一个文件都不许动");
}

/// F13:恢复 `*.vscdb` 时,这份备份里没有的 `-wal`/`-shm` 必须删掉 ——
/// 否则 SQLite 下次打开会把回滚掉的那次写入重放回刚恢复的库上。
#[test]
fn restore_drops_a_stray_wal_next_to_the_restored_vscdb() {
    let root = tmpdir("restore-wal");
    let db = root.join("state.vscdb");
    std::fs::write(&db, "old-db").unwrap();
    let dir = backup(&root, &[db.clone()], "20260101-000000").unwrap();
    std::fs::write(&db, "written-while-running").unwrap();
    let wal = root.join("state.vscdb-wal");
    std::fs::write(&wal, "replay-me").unwrap();
    let shm = root.join("state.vscdb-shm");
    std::fs::write(&shm, "shm").unwrap();

    restore(&dir).unwrap();
    assert_eq!(std::fs::read_to_string(&db).unwrap(), "old-db");
    assert!(!wal.exists(), "备份里没有的 -wal 要删掉");
    assert!(!shm.exists(), "备份里没有的 -shm 也要删掉");
}

/// F16:卸载器/辅助进程不算主程序名(大小写不敏感),候选里优先挑与安装目录
/// 同名的那支 —— `Site.process` 是写入前进程检查用的名字。
#[test]
fn trae_process_choice_prefers_the_install_exe_and_skips_helpers() {
    for bad in [
        "unins000.EXE",
        "Uninstall.exe",
        "UNINST000.exe",
        "Trae CN Helper.exe",
        "Trae CN Helper (GPU).exe",
        "crashpad_handler.exe",
        "CrashSender.exe",
    ] {
        assert!(is_helper_exe(bad), "{bad} 不该算主程序");
    }
    for good in ["Trae CN.exe", "Trae.exe", "TraeWork.exe"] {
        assert!(!is_helper_exe(good), "{good} 是主程序");
    }
    let procs = vec!["aaa.exe".to_string(), "Trae CN.exe".to_string()];
    assert_eq!(expected_process("Trae CN", &procs), "Trae CN.exe", "优先目录同名的那支");
    assert_eq!(
        expected_process("Trae CN", &["aaa.exe".to_string()]),
        "aaa.exe",
        "没有同名项就退到第一个候选"
    );
    assert_eq!(expected_process("Trae CN", &[]), Client::Trae.process());
}

/// F15:「同名条目被覆盖」那句提示里的来源名必须按**实际地址**认 ——
/// 以前写死"不是 4SAPI",写 Hapi / OpenCode Go 模型时意思正好反了。
#[test]
fn foreign_conflict_note_names_the_real_source() {
    assert_eq!(foreign_source_label("https://opencode.ai/zen/go/v1"), "OpenCode Go");
    assert_eq!(foreign_source_label("https://ai.yuchuantest.com/v1"), "Hapi");
    assert_eq!(foreign_source_label("https://4sapi.com/v1"), "4SAPI");
    assert_eq!(foreign_source_label("https://example.com/v1"), "别家来源");
    let go = ModelSpec::new(
        "opencode-go",
        "deepseek-v4.1-flash",
        "https://opencode.ai/zen/go/v1",
        "sk-go",
        None,
        false,
    );
    let root = tmpdir("note-source");
    std::fs::write(
        root.join("models.json"),
        r#"[{"id":"deepseek-v4.1-flash","name":"4S-x","url":"https://4sapi.com/v1","apiKey":"old"}]"#,
    )
    .unwrap();
    std::fs::write(root.join("settings.json"), "{}").unwrap();
    let site = site_at(&root, Client::Workbuddy);
    let o = write_workbuddy_site(&site, &go, &Opts::default()).unwrap();
    assert_eq!(
        o.foreign_conflict.as_deref(),
        Some("https://4sapi.com/v1"),
        "指向 4SAPI 的条目对 Go 来说就是别家的"
    );
}

/// F17:ZCode 的"可不可写"必须先核 config.json 里的 provider 对象 ——
/// 否则预告敢承诺「新增 provider「4SAPI」+ N 条模型」,真写才报
/// "config.json 里没有 provider 对象"。
#[test]
fn zcode_readable_requires_a_provider_object_in_config_json() {
    let root = tmpdir("zcode-readable");
    std::fs::write(
        root.join("provider_config.json"),
        r#"{"schemaVersion":1,"config":{"providerConfigRules":{"providerRules":[]},"modelConfigRules":{"providerModelRules":[]}}}"#,
    )
    .unwrap();
    let site = site_at(&root, Client::Zcode);
    // config.json 缺 provider → 提前判不可读(真写也正是在这一句上失败)
    std::fs::write(root.join("config.json"), "{}").unwrap();
    let e = site_readable(&site).unwrap_err();
    assert!(e.contains("provider"), "{e}");
    let we = write_zcode_site(&site, &spec()).unwrap_err();
    assert!(we.contains("provider"), "真写的失败点应与之对应: {we}");

    // 补齐后通过
    std::fs::write(root.join("config.json"), r#"{"provider":{}}"#).unwrap();
    site_readable(&site).unwrap();
    // provider 不是对象同样不行
    std::fs::write(root.join("config.json"), r#"{"provider":[]}"#).unwrap();
    assert!(site_readable(&site).unwrap_err().contains("provider"));
    // provider_config.json 缺 config 对象也要提前拦住(真写要往它下面塞规则)
    std::fs::write(root.join("config.json"), r#"{"provider":{}}"#).unwrap();
    std::fs::write(root.join("provider_config.json"), r#"{"schemaVersion":1}"#).unwrap();
    let e2 = site_readable(&site).unwrap_err();
    assert!(e2.contains("config"), "{e2}");
}

/// 取消写入:WorkBuddy 删条目;选中项指向它时一并清掉;再删一次是「本来就没有」(不写盘)。

/// 取消写入:WorkBuddy 删条目;选中项指向它时一并清掉;再删一次是「本来就没有」(不写盘)。
#[test]
fn workbuddy_remove_entry_and_clear_selection() {
    let root = tmpdir("wb-rm");
    std::fs::write(root.join("models.json"), "[]").unwrap();
    std::fs::write(root.join("settings.json"), "{}").unwrap();
    let site = site_at(&root, Client::Workbuddy);
    let opts = Opts {
        selection_prefix: Some(WB_PREFIX.to_string()),
        ..Default::default()
    };
    write_workbuddy_site(&site, &spec(), &opts).unwrap();
    let s = read_v(&root.join("settings.json"));
    assert_eq!(s[WB_SELECTION_KEY], "custom-local:claude-sonnet-5");

    // 预告要说清会删哪一条、会顺带清选中项
    let (configured, removal) = wb_removal(&site, &spec());
    assert!(configured);
    let r = removal.unwrap();
    assert!(r.what.contains("4S-claude-sonnet-5"), "{}", r.what);
    assert!(r.what.contains("https://4sapi.com/v1"));
    assert!(r.clears_selection);
    assert_eq!(r.count, 1);

    let o = remove_workbuddy_site(&site, &spec()).unwrap();
    assert_eq!(o.removed, 1);
    assert!(o.clears_selection);
    assert!(read_v(&root.join("models.json")).as_array().unwrap().is_empty());
    assert!(read_v(&root.join("settings.json")).get(WB_SELECTION_KEY).is_none());

    // 幂等:再删一次什么都不动
    let o2 = remove_workbuddy_site(&site, &spec()).unwrap();
    assert_eq!(o2.removed, 0);
    assert!(o2.backup.as_os_str().is_empty(), "没改动就不该建备份目录");
}

/// 选中项指向别人时不许乱清(只清指向这条模型的)。
#[test]
fn workbuddy_remove_keeps_foreign_selection() {
    let root = tmpdir("wb-rm-sel");
    std::fs::write(root.join("models.json"), "[]").unwrap();
    std::fs::write(root.join("settings.json"), "{\"model\":\"custom-local:other\"}").unwrap();
    let site = site_at(&root, Client::Workbuddy);
    write_workbuddy_site(&site, &spec(), &Opts::default()).unwrap();
    let (_, removal) = wb_removal(&site, &spec());
    assert!(!removal.unwrap().clears_selection);
    let o = remove_workbuddy_site(&site, &spec()).unwrap();
    assert_eq!(o.removed, 1);
    assert!(!o.clears_selection);
    let s = read_v(&root.join("settings.json"));
    assert_eq!(s[WB_SELECTION_KEY], "custom-local:other", "别人的选中项不动");
}

/// ZCode:删掉模型;它是最后一个时连 provider 与规则一起收走。
#[test]
fn zcode_remove_model_then_provider() {
    let root = tmpdir("zcode-rm");
    std::fs::write(root.join("config.json"), r#"{"provider":{}}"#).unwrap();
    std::fs::write(
        root.join("provider_config.json"),
        r#"{"schemaVersion":1,"config":{"providerConfigRules":{"providerRules":[]},"modelConfigRules":{"providerModelRules":[],"manualProviderModelRules":[]}}}"#,
    )
    .unwrap();
    let site = site_at(&root, Client::Zcode);
    write_zcode_site(&site, &spec()).unwrap();
    let s2 = ModelSpec::new("4sapi", "glm-5.3", "https://4sapi.com/v1", "sk-test-123", Some(1_000_000), false);
    write_zcode_site(&site, &s2).unwrap();

    let (configured, removal) = zcode_removal(&site, &spec());
    assert!(configured);
    assert!(removal.unwrap().what.contains("还剩 1 个模型"), "预告要说清 provider 会留下");

    let o = remove_zcode_site(&site, &spec()).unwrap();
    assert_eq!(o.removed, 1);
    let cfg = read_v(&root.join("config.json"));
    let pid = zcode_pid(&cfg, "4SAPI").expect("provider 还在");
    assert!(cfg["provider"][&pid]["models"].get(&spec().id).is_none());
    assert!(cfg["provider"][&pid]["models"].get("glm-5.3").is_some(), "别的模型不动");
    let rules = read_v(&root.join("provider_config.json"));
    let rule = &rules["config"]["providerConfigRules"]["providerRules"][0];
    assert_eq!(rule["config"]["personalModelIds"].as_array().unwrap().len(), 1);
    assert_eq!(rule["config"]["personalModelIds"][0], "glm-5.3");
    assert!(rules["config"]["modelConfigRules"]["providerModelRules"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["modelId"] != "claude-sonnet-5"));

    // 删最后一个:provider 与它的规则一起消失
    let (_, r2) = zcode_removal(&site, &s2);
    assert!(r2.unwrap().what.contains("最后一个模型"));
    let o2 = remove_zcode_site(&site, &s2).unwrap();
    assert_eq!(o2.removed, 1);
    let cfg2 = read_v(&root.join("config.json"));
    assert!(zcode_pid(&cfg2, "4SAPI").is_none(), "空 provider 不该留下");
    let rules2 = read_v(&root.join("provider_config.json"));
    assert!(rules2["config"]["providerConfigRules"]["providerRules"]
        .as_array()
        .unwrap()
        .is_empty());
}
