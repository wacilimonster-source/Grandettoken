//! Only this application's agent belongs to the kill-on-close job.
use serde::Serialize;
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Instant,
};
use tauri::State;
use windows_sys::Win32::System::JobObjects::*;

#[derive(Default)]
pub(crate) struct Commander(Mutex<Session>);
#[derive(Default)]
struct Session {
    run: Option<(Child, OwnedHandle)>,
    started: Option<Instant>,
    info: Arc<Mutex<Info>>,
}
#[derive(Default, Serialize, Clone)]
pub(crate) struct Info {
    active: bool,
    elapsed: u64,
    status: String,
    logs: Vec<String>,
    verify: Option<String>,
    code: Option<String>,
}
impl Info {
    fn mark(&mut self, message: &str) {
        self.status = message.into();
        if self.logs.last() != Some(&self.status) {
            if self.logs.len() == 40 {
                self.logs.remove(0);
            }
            self.logs.push(message.into());
        }
    }
    fn clear_pair(&mut self) {
        self.verify = None;
        self.code = None;
    }
}
impl Session {
    fn refresh(&mut self) -> Result<(), String> {
        if let Some((child, _)) = &mut self.run {
            if let Some(exit) = child.try_wait().map_err(|e| e.to_string())? {
                self.run.take();
                let mut info = self.info.lock().map_err(|e| e.to_string())?;
                info.clear_pair();
                info.active = false;
                info.mark(&format!("代理已退出（{exit}）"));
            }
        }
        Ok(())
    }
}
fn make_job(child: &Child) -> std::io::Result<OwnedHandle> {
    // The bootstrap waits on stdin until the entire process tree is contained.
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let owner = OwnedHandle::from_raw_handle(job);
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        ) == 0
            || AssignProcessToJobObject(job, child.as_raw_handle()) == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(owner)
    }
}
fn observe(stream: impl Read + Send + 'static, state: Arc<Mutex<Info>>) {
    std::thread::spawn(move || {
        let mut next_code = false;
        for raw in BufReader::new(stream).split(b'\n').map_while(Result::ok) {
            let raw = String::from_utf8_lossy(&raw);
            let line = raw.trim();
            let Ok(mut info) = state.lock() else { break };
            if !info.active {
                break;
            }
            // Only known upstream status messages enter the UI. Never retain tool payloads.
            if line.contains("Make sure the code matches") {
                next_code = true;
            } else if next_code && !line.is_empty() {
                next_code = false;
                if line.len() <= 16 && line.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                {
                    info.code = Some(line.into());
                }
            } else if line.starts_with("https://mcp.desktopcommander.app/")
                && info.status == "等待浏览器验证"
            {
                info.verify = Some(line.into());
            } else if line.contains("Verify this device in your browser") {
                info.mark("等待浏览器验证");
            } else if line.contains("Device ready (device")
                || line.contains("Desktop Commander Remote is connected")
            {
                info.clear_pair();
                info.mark("设备在线");
            } else if line.contains("Remote Connection") {
                info.mark("本地代理运行中，正在连接");
            } else if line.contains("offline") || line.contains("Retrying in the background") {
                info.mark("连接中断，等待恢复");
            } else if line.contains("DC_NODE_TOO_OLD") {
                info.mark("需要 Node.js 22.12 或更高版本");
            } else if line.contains("DC_NPM_FAILED") || line.contains("MODULE_NOT_FOUND") {
                info.mark("无法加载 npm 或代理依赖，请检查 Node.js 安装");
            } else if line.starts_with("npm error code ") {
                let code = line.trim_start_matches("npm error code ");
                if code.len() <= 32 && code.bytes().all(|b| b.is_ascii_alphanumeric()) {
                    info.mark(&format!("npm 错误：{code}"));
                }
            }
        }
    });
}
#[tauri::command]
pub(crate) fn commander_status(state: State<Commander>) -> Result<Info, String> {
    let mut s = state.0.lock().map_err(|e| e.to_string())?;
    s.refresh()?;
    let mut info = s.info.lock().map_err(|e| e.to_string())?.clone();
    info.elapsed = s.started.map_or(0, |t| t.elapsed().as_secs());
    Ok(info)
}
#[tauri::command]
pub(crate) fn commander_start(state: State<Commander>) -> Result<(), String> {
    let mut s = state.0.lock().map_err(|e| e.to_string())?;
    s.refresh()?;
    if s.run.is_some() {
        return Ok(());
    }
    let path = std::env::var_os("PATH").ok_or("找不到 Node.js 搜索路径")?;
    let node = std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("node.exe"))
        .find(|file| file.is_file())
        .ok_or("需要安装 Node.js 22.12 或更高版本（含 npm）")?;
    let cwd =
        std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or("无法定位本地应用目录")?)
            .join("Grandettoken")
            .join("commander");
    std::fs::create_dir_all(&cwd).map_err(|e| format!("无法准备代理工作目录：{e}"))?;
    let mut child = Command::new(node)
        .args(["-e", include_str!("commander-launch.cjs")])
        .current_dir(cwd)
        .creation_flags(0x08000000)
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动失败，请检查 Node.js 22.12+ 是否已安装：{e}"))?;
    let job = match make_job(&child) {
        Ok(job) => job,
        Err(e) => {
            drop(child.stdin.take());
            let _ = child.wait();
            return Err(format!("无法管理代理进程：{e}"));
        }
    };
    let mut info = Info {
        active: true,
        ..Info::default()
    };
    info.mark("正在准备代理依赖");
    let state = Arc::new(Mutex::new(info));
    if let Some(out) = child.stdout.take() {
        observe(out, state.clone());
    }
    if let Some(err) = child.stderr.take() {
        observe(err, state.clone());
    }
    child
        .stdin
        .take()
        .ok_or("无法唤起代理")?
        .write_all(b"1")
        .map_err(|e| e.to_string())?;
    s.info = state;
    s.run = Some((child, job));
    s.started = Some(Instant::now());
    Ok(())
}
#[tauri::command]
pub(crate) fn commander_stop(state: State<Commander>) -> Result<(), String> {
    let mut s = state.0.lock().map_err(|e| e.to_string())?;
    if let Some((mut child, job)) = s.run.take() {
        drop(job);
        child.wait().map_err(|e| e.to_string())?;
    }
    let mut info = s.info.lock().map_err(|e| e.to_string())?;
    info.active = false;
    info.clear_pair();
    info.mark("已停止");
    Ok(())
}
#[tauri::command]
pub(crate) fn commander_verify(
    app: tauri::AppHandle,
    state: State<Commander>,
) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let url = commander_status(state)?.verify.ok_or("暂无待验证的设备")?;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    #[test]
    fn closing_job_stops_the_launcher_and_its_descendant() -> Result<(), Box<dyn std::error::Error>>
    {
        // Offline fixture: exercise job containment without connecting a real account.
        let mut child = Command::new("node.exe").args(["-e",
            "process.stdin.once('data',()=>{let c=require('node:child_process').spawn(process.execPath,['-e','setInterval(()=>{},5000)'],{windowsHide:true,stdio:'ignore'}); console.log(c.pid)})"
        ]).creation_flags(0x08000000).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
        let job = make_job(&child)?;
        child.stdin.take().ok_or("missing stdin")?.write_all(b"1")?;
        let mut pid = String::new();
        BufReader::new(child.stdout.take().ok_or("missing stdout")?).read_line(&mut pid)?;
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid.trim().parse()?) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        assert_eq!(
            unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) },
            258
        );
        drop(job);
        assert_eq!(
            unsafe { WaitForSingleObject(handle.as_raw_handle(), 5000) },
            0
        );
        child.wait()?;
        Ok(())
    }
}
