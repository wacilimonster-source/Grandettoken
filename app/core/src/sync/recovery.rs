use super::{publish, FetchedSources};
use crate::store::Store;

const RECOVERY_LIMIT: usize = 24;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryCandidate {
    pub gist_id: String,
    pub key_epoch: u8,
    pub has_local_source: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoverySearch {
    pub candidates: Vec<RecoveryCandidate>,
    pub unverified: usize,
    pub truncated: bool,
}

pub struct VerifiedRecovery {
    pub source: Option<String>,
    pub last_upload_at: Option<i64>,
}

pub fn candidate_ids(ids: Vec<String>, current: Option<&str>) -> (Vec<String>, bool) {
    let mut ids: Vec<_> = ids
        .into_iter()
        .filter(|id| current != Some(id.as_str()))
        .collect();
    let truncated = ids.len() > RECOVERY_LIMIT;
    ids.truncate(RECOVERY_LIMIT);
    (ids, truncated)
}

pub fn collect_candidates(
    checks: impl IntoIterator<Item = Result<RecoveryCandidate, String>>,
    truncated: bool,
) -> RecoverySearch {
    let mut candidates = Vec::new();
    let mut unverified = 0;
    for check in checks {
        match check {
            Ok(candidate) => candidates.push(candidate),
            Err(_) => unverified += 1,
        }
    }
    candidates.sort_by(|a, b| a.gist_id.cmp(&b.gist_id));
    RecoverySearch {
        candidates,
        unverified,
        truncated,
    }
}

pub fn verify(
    store: &Store,
    fetched: FetchedSources,
    owner_id: i64,
) -> Result<VerifiedRecovery, String> {
    let sources = match fetched {
        FetchedSources::Loaded {
            sources,
            owner_id: owner,
            ..
        } => {
            if owner != Some(owner_id) {
                return Err("旧同步组不属于当前 GitHub 账号,请连接原账号".into());
            }
            sources
        }
        FetchedSources::NotModified { .. } => Vec::new(),
    };
    if sources.is_empty() {
        return Err("无法验证旧同步码,请确认旧组仍可访问且有有效数据".into());
    }
    let (source, last_upload_at) = publish::local_recovery_source(store, &sources)
        .map_err(|error| format!("{}: {}", error.code, error.message))?;
    Ok(VerifiedRecovery {
        source,
        last_upload_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_exclude_the_live_group_and_report_unverified_items() {
        let ids = (0..=RECOVERY_LIMIT + 1).map(|id| id.to_string()).collect();
        let (remaining, truncated) = candidate_ids(ids, Some("0"));
        assert!(truncated);
        assert_eq!(remaining.len(), RECOVERY_LIMIT);
        assert_eq!(remaining[0], "1");
        let candidate = |id: &str| RecoveryCandidate {
            gist_id: id.into(),
            key_epoch: 0,
            has_local_source: false,
        };
        let out = collect_candidates(
            [
                Ok(candidate("b")),
                Err("unverified".into()),
                Ok(candidate("a")),
            ],
            truncated,
        );
        assert_eq!(out.unverified, 1);
        assert_eq!(
            out.candidates
                .iter()
                .map(|c| c.gist_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert!(out.truncated);
    }
}
