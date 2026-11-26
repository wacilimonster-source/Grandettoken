use std::collections::HashSet;

/// A single view of executable names for one discovery operation.
#[derive(Debug, Default)]
pub struct ProcessSnapshot(HashSet<String>);

impl ProcessSnapshot {
    pub fn from_names(names: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        Self(
            names
                .into_iter()
                .map(|name| name.as_ref().to_ascii_lowercase())
                .collect(),
        )
    }

    pub fn has(&self, process: &str) -> bool {
        self.0.contains(&process.to_ascii_lowercase())
    }

    #[cfg(windows)]
    pub fn capture() -> Self {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        };

        struct SnapshotHandle(HANDLE);
        impl Drop for SnapshotHandle {
            fn drop(&mut self) {
                // The guard owns a valid ToolHelp handle.
                unsafe { CloseHandle(self.0) };
            }
        }

        // The process snapshot needs no process ID or access to an individual process.
        let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if raw == INVALID_HANDLE_VALUE {
            return Self::default();
        }
        let snapshot = SnapshotHandle(raw);
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut names = HashSet::new();
        // The initialized structure is sized for the Win32 API.
        let mut present = unsafe { Process32FirstW(snapshot.0, &mut entry) };
        while present != 0 {
            let end = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            names.insert(String::from_utf16_lossy(&entry.szExeFile[..end]).to_ascii_lowercase());
            present = unsafe { Process32NextW(snapshot.0, &mut entry) };
        }
        Self(names)
    }

    #[cfg(not(windows))]
    pub fn capture() -> Self {
        Self::default()
    }
}
