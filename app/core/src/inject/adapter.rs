use super::{
    blocked_by_running, commit, pretty, site_readable, sites_of, Client, Preview, Site, SiteOutcome,
};

pub(super) struct ClientAdapter {
    pub site: Site,
    pub running_warning: Option<String>,
}

impl ClientAdapter {
    pub fn discover(client: Client) -> Result<Vec<Self>, String> {
        let sites = sites_of(client);
        if sites.is_empty() {
            return Err(format!("没检测到 {} 的配置", client.label()));
        }
        Ok(sites
            .into_iter()
            .map(|site| Self {
                running_warning: blocked_by_running(&site),
                site,
            })
            .collect())
    }

    pub fn preview(&self) -> Preview {
        Preview {
            client: self.site.client.id().into(),
            label: self.site.label.clone(),
            files: self.site.files.iter().map(|path| pretty(path)).collect(),
            writes_secret: true,
            running_warning: self.running_warning.clone(),
            ..Default::default()
        }
    }

    pub fn require_readable(&self) -> Result<(), String> {
        site_readable(&self.site).map_err(|e| format!("{}: {e}", self.site.label))
    }

    pub fn commit(
        &self,
        stamp: &str,
        write: impl FnOnce() -> Result<SiteOutcome, String>,
        verify: impl FnOnce() -> bool,
    ) -> Result<SiteOutcome, String> {
        let mut outcome = SiteOutcome::default();
        let backup = commit(
            &self.site,
            &self.site.files,
            stamp,
            || {
                outcome = write()?;
                Ok(())
            },
            verify,
        )
        .map_err(|e| format!("{}: {e}", self.site.label))?;
        outcome.backup = backup;
        Ok(outcome)
    }
}
