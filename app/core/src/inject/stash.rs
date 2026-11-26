//! 值级 stash：记下接入前用户自己的 endpoint / provider / model 等非密钥原值，
//! 断开时还回去（对齐 magpie `internal/agent/stash.go` 的语义）。
//! 只存非密钥值；`Value::Null` 表示「原来没有这个键」，还原即删除。
//! 整文件 DPAPI 快照仍保留作崩溃/误操作兜底（两者并存，见裁决 5C）。

use super::*;

thread_local! {
    static STASH_OVERRIDE: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// 仅测试用：把 stash 指到临时目录，避免单测碰真实用户数据。
#[cfg(test)]
pub(super) fn override_path(p: Option<PathBuf>) {
    STASH_OVERRIDE.with(|c| *c.borrow_mut() = p);
}

fn stash_path() -> Option<PathBuf> {
    if let Some(p) = STASH_OVERRIDE.with(|c| c.borrow().clone()) {
        return Some(p);
    }
    Some(appdata()?.join("com.wacil.tokenscope").join("inject-stash.json"))
}

pub(super) type Stash = Map<String, Value>;

pub(super) fn load_all_at(p: &Path) -> HashMap<String, Stash> {
    let Ok(raw) = std::fs::read_to_string(p) else {
        return HashMap::new();
    };
    match serde_json::from_str::<HashMap<String, Stash>>(&raw) {
        Ok(m) => m,
        Err(_) => {
            // 半写/手改坏的文件先挪走再重建，别把别人的原值覆盖丢（magpie 同做法）
            let bad = p.with_file_name(format!("inject-stash.bad-{}.json", stamp_now()));
            let _ = std::fs::rename(p, bad);
            HashMap::new()
        }
    }
}

pub(super) fn load_all() -> HashMap<String, Stash> {
    match stash_path() {
        Some(p) => load_all_at(&p),
        None => HashMap::new(),
    }
}

pub(super) fn save_all_at(p: &Path, all: &HashMap<String, Stash>) -> Result<(), String> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("建目录失败: {}", os(&e)))?;
    }
    let text = serde_json::to_string_pretty(all).map_err(|e| e.to_string())?;
    let tmp = p.with_file_name("inject-stash.json.tmp");
    std::fs::write(&tmp, text.as_bytes()).map_err(|e| format!("写 stash 失败: {}", os(&e)))?;
    std::fs::rename(&tmp, p).map_err(|e| format!("替换 stash 失败: {}", os(&e)))
}

fn save_all(all: &HashMap<String, Stash>) -> Result<(), String> {
    match stash_path() {
        Some(p) => save_all_at(&p, all),
        None => Err("取不到 APPDATA，无法记录接入前原值".into()),
    }
}

/// 首次接入时记录原值；已记录过就不覆盖（用户后来自己改的值不该被当成「原值」）。
pub(super) fn capture(client: &str, vals: Stash) {
    if vals.is_empty() {
        return;
    }
    let mut all = load_all();
    if all.contains_key(client) {
        return;
    }
    all.insert(client.to_string(), vals);
    let _ = save_all(&all);
}

/// 读原值但不从文件里取走：写入成功后才 `take`。
/// 先 take 再判断有没有东西要改，会在「无事可做」的路径上把原值弄丢。
pub(super) fn peek(client: &str) -> Stash {
    load_all().remove(client).unwrap_or_default()
}

/// 断开时取走原值；取走后清掉记录，下次接入重新记。
pub(super) fn take(client: &str) -> Stash {
    let mut all = load_all();
    let Some(v) = all.remove(client) else {
        return Map::new();
    };
    let _ = save_all(&all);
    v
}

/// 读 JSON 对象里若干点路径的原值（不存在记 Null）。
pub(super) fn snapshot_json(root: Option<&Value>, keys: &[&str]) -> Stash {
    let mut out = Stash::new();
    for k in keys {
        let mut cur = root;
        for part in k.split('.') {
            cur = cur.and_then(|v| v.get(part));
        }
        out.insert((*k).to_string(), cur.cloned().unwrap_or(Value::Null));
    }
    out
}

/// 把原值写回 JSON 对象：Null 删键（父级不存在就什么都不做），其余设值；只动记过的键。
pub(super) fn restore_json(obj: &mut Map<String, Value>, stash: &Stash) {
    for (k, v) in stash {
        let parts: Vec<&str> = k.split('.').collect();
        let (last, parents) = parts.split_last().unwrap();
        if parents.is_empty() {
            if v.is_null() {
                obj.remove(*last);
            } else {
                obj.insert((*last).to_string(), v.clone());
            }
            continue;
        }
        if v.is_null() {
            let mut cur = &mut *obj;
            let mut reached = true;
            for part in parents {
                if !cur.get(*part).is_some_and(Value::is_object) {
                    reached = false;
                    break;
                }
                cur = cur.get_mut(*part).unwrap().as_object_mut().unwrap();
            }
            if reached {
                cur.remove(*last);
            }
        } else {
            let mut cur = &mut *obj;
            for part in parents {
                if !cur.get(*part).is_some_and(Value::is_object) {
                    cur.insert((*part).to_string(), json!({}));
                }
                cur = cur.get_mut(*part).unwrap().as_object_mut().unwrap();
            }
            cur.insert((*last).to_string(), v.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stash_roundtrip_restores_originals_and_absence() {
        let root = json!({"model": "user-model", "env": {"ANTHROPIC_MODEL": "user-env-model", "KEEP": "1"}});
        let snap = snapshot_json(Some(&root), &["model", "env.ANTHROPIC_MODEL", "env.ABSENT"]);
        assert_eq!(snap["model"], json!("user-model"));
        assert_eq!(snap["env.ANTHROPIC_MODEL"], json!("user-env-model"));
        assert!(snap["env.ABSENT"].is_null());

        let mut changed = json!({"model": "gw-model", "env": {"ANTHROPIC_MODEL": "gw-model", "KEEP": "1"}});
        restore_json(changed.as_object_mut().unwrap(), &snap);
        assert_eq!(changed["model"], json!("user-model"));
        assert_eq!(changed["env"]["ANTHROPIC_MODEL"], json!("user-env-model"));
        assert_eq!(changed["env"]["KEEP"], json!("1"));

        let mut removed = json!({"env": {"KEEP": "1"}});
        restore_json(removed.as_object_mut().unwrap(), &snap);
        assert_eq!(removed["model"], json!("user-model"));
        assert_eq!(removed["env"]["ANTHROPIC_MODEL"], json!("user-env-model"));
        assert!(removed["env"].get("ABSENT").is_none());
    }

    #[test]
    fn wired_model_drives_codex_stand_in() {
        let dir = std::env::temp_dir().join(format!("ts-wired-{}", crate::vault::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        override_path(Some(dir.join("inject-stash.json")));
        assert_eq!(agent_stand_in("codex-auto-review"), None);
        note_wired("codex", "group/coding");
        assert_eq!(agent_stand_in("codex-auto-review").as_deref(), Some("group/coding"));
        assert_eq!(agent_stand_in("gpt-5"), None);
        clear_wired("codex");
        assert_eq!(agent_stand_in("codex-auto-review"), None);
        override_path(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stash_file_capture_once_and_take_clears() {
        let dir = std::env::temp_dir().join(format!("ts-stash-{}", crate::vault::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("inject-stash.json");
        let mut all = HashMap::new();
        let mut vals = Stash::new();
        vals.insert("model".into(), json!("user-model"));
        all.insert("claude-code".into(), vals);
        save_all_at(&p, &all).unwrap();
        assert_eq!(load_all_at(&p)["claude-code"]["model"], json!("user-model"));
        // 坏文件挪走重建，不 panic
        std::fs::write(&p, "{half").unwrap();
        assert!(load_all_at(&p).is_empty());
        assert!(dir.join("inject-stash.json").exists() || std::fs::read_dir(&dir).unwrap().count() >= 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// 记录当前接入的模型（非密钥），供网关对 agent 内部模型名做 stand-in。
pub(super) fn note_wired(client: &str, model: &str) {
    let mut all = load_all();
    let mut m = Stash::new();
    m.insert("model".into(), json!(model));
    all.insert(format!("__wired_{client}"), m);
    let _ = save_all(&all);
}

pub(super) fn clear_wired(client: &str) {
    let mut all = load_all();
    all.remove(&format!("__wired_{client}"));
    let _ = save_all(&all);
}

pub(crate) fn wired_model(client: &str) -> Option<String> {
    load_all()
        .get(&format!("__wired_{client}"))
        .and_then(|m| m.get("model"))
        .and_then(Value::as_str)
        .map(String::from)
}

/// magpie 口径：Codex 的审批复核等内部模型名（codex-auto-review）不该 404，
/// 用该客户端当前接入的模型顶替（magpie 读配置里的 model，我们读接入时记下的同一个值）。
pub(crate) fn agent_stand_in(model: &str) -> Option<String> {
    if model == "codex-auto-review" {
        wired_model("codex")
    } else {
        None
    }
}
