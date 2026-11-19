//! 凭据与索引分别由系统服务保存；失败时恢复写入前的凭据状态。

pub trait CredentialStore: Send + Sync {
    fn get(&self, id: &str) -> Result<Option<String>, String>;
    fn set(&self, id: &str, value: &str) -> Result<(), String>;
    fn delete(&self, id: &str) -> Result<(), String>;
}

pub struct NativeVault;

impl CredentialStore for NativeVault {
    fn get(&self, id: &str) -> Result<Option<String>, String> { crate::secrets::vault_get(id) }
    fn set(&self, id: &str, value: &str) -> Result<(), String> { crate::secrets::vault_set(id, value) }
    fn delete(&self, id: &str) -> Result<(), String> { crate::secrets::vault_delete(id) }
}

/// 补偿保证操作结束后的状态，不保证其他请求看不到提交期间的新凭据。
pub fn commit<T>(
    credentials: &dyn CredentialStore,
    id: &str,
    next: Option<&str>,
    save_index: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let old = credentials.get(id)?;
    if old.as_deref() == next { return save_index(); }
    match next {
        Some(value) => credentials.set(id, value)?,
        None => credentials.delete(id)?,
    }
    match save_index() {
        Ok(value) => Ok(value),
        Err(error) => {
            let restored = match old.as_deref() {
                Some(value) => credentials.set(id, value),
                None => credentials.delete(id),
            };
            if restored.is_err() {
                return Err("索引保存失败，凭据回滚也失败；部分变更可能已生效，请核对密钥后重试".into());
            }
            Err(error)
        }
    }
}

#[derive(Default)]
pub struct ImportResult {
    pub changed_ids: Vec<String>,
    pub error: Option<String>,
}

/// 各条目独立提交；某条失败仍把前后成功条目交回调用者同步。
pub fn import_batch<T>(items: impl IntoIterator<Item = T>, mut import: impl FnMut(T) -> Result<Option<String>, String>) -> ImportResult {
    let mut result = ImportResult::default();
    let mut errors = Vec::new();
    for item in items {
        match import(item) {
            Ok(Some(id)) => result.changed_ids.push(id),
            Ok(None) => {},
            Err(error) => errors.push(crate::gateway_proto::error::redact_secrets(&error)),
        }
    }
    result.error = (!errors.is_empty()).then(|| errors.join("；"));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Memory { value: Mutex<Option<String>>, fail_read: bool, fail_restore: bool }
    impl CredentialStore for Memory {
        fn get(&self, _: &str) -> Result<Option<String>, String> {
            if self.fail_read { Err("read failed".into()) } else { Ok(self.value.lock().unwrap().clone()) }
        }
        fn set(&self, _: &str, value: &str) -> Result<(), String> {
            if self.fail_restore && value == "old" { return Err("restore failed".into()); }
            *self.value.lock().unwrap() = Some(value.into()); Ok(())
        }
        fn delete(&self, _: &str) -> Result<(), String> { *self.value.lock().unwrap() = None; Ok(()) }
    }
    #[test]
    fn batch_keeps_successful_ids_when_another_entry_fails() {
        let result = import_batch(["first", "failure", "last"], |id| {
            if id == "failure" { Err("index failed".into()) } else { Ok(Some(id.into())) }
        });
        assert_eq!(result.changed_ids, ["first", "last"]);
        assert!(result.error.unwrap().contains("index failed"));
    }

    #[test]
    fn failed_index_restores_new_replaced_and_deleted_credentials() {
        for (old, next) in [(None, Some("new")), (Some("old"), Some("new")), (Some("old"), None)] {
            let m = Memory { value: Mutex::new(old.map(str::to_string)), fail_read: false, fail_restore: false };
            let result = commit::<()>(&m, "fixture", next, || Err("index failed".into()));
            assert!(result.is_err()); assert_eq!(m.get("fixture").unwrap().as_deref(), old);
        }
    }
    #[test]
    fn read_failure_writes_nothing_and_failed_compensation_is_explicit() {
        let m = Memory { value: Mutex::new(Some("old".into())), fail_read: true, fail_restore: false };
        assert!(commit(&m, "fixture", Some("new"), || Ok(())).is_err());
        assert_eq!(m.value.lock().unwrap().as_deref(), Some("old"));
        let m = Memory { value: Mutex::new(Some("old".into())), fail_read: false, fail_restore: true };
        let error = commit::<()>(&m, "fixture", Some("new"), || Err("index failed".into())).unwrap_err();
        assert!(error.contains("部分变更")); assert!(!error.contains("new"));
    }
}
