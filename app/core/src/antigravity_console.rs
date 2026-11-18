//! 官方 statusLine 回调提供同一会话的身份与额度，终端输出仅用于协议应答。
use std::path::Path;

#[cfg(not(windows))]
pub(crate) fn capture(_cli: &Path) -> Result<serde_json::Value, String> {
    Err("Antigravity 账号识别需要 Windows 官方 CLI 状态栏回调".into())
}

#[cfg(windows)]
pub(crate) fn capture(cli: &Path) -> Result<serde_json::Value, String> {
    windows::capture(cli)
}

#[cfg(windows)]
mod windows {
    use super::*;
    use base64::Engine;
    use std::ffi::{c_void, OsStr};
    use std::fs;
    use std::io::{Read, Write};
    use std::os::windows::{ffi::OsStrExt, io::FromRawHandle};
    use std::path::PathBuf;
    use std::ptr::{null, null_mut};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Console::*;
    use windows_sys::Win32::System::JobObjects::*;
    use windows_sys::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
    use windows_sys::Win32::System::Threading::*;

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    struct Folder(PathBuf);
    impl Folder {
        fn new() -> Result<Self, String> {
            let mut nonce = [0u8; 16];
            getrandom::getrandom(&mut nonce).map_err(|_| "无法创建安全采集目录")?;
            let name: String = nonce.iter().map(|v| format!("{v:02x}")).collect();
            let path = std::env::temp_dir().join(format!("grandettoken-agy-{name}"));
            fs::create_dir(&path).map_err(|_| "无法创建 Antigravity 采集目录")?;
            Ok(Self(path))
        }
    }
    impl Drop for Folder {
        fn drop(&mut self) {
            // 仅删除本次创建的已知文件，目录中出现其它内容时保留供用户排查。
            for name in ["account.json", "account.tmp", "settings.json"] {
                let _ = fs::remove_file(self.0.join(name));
            }
            let _ = fs::remove_dir(&self.0);
        }
    }

    fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
        s.as_ref().encode_wide().chain(Some(0)).collect()
    }

    fn pipe() -> Result<(Handle, Handle), String> {
        let (mut read, mut write) = (null_mut(), null_mut());
        if unsafe { CreatePipe(&mut read, &mut write, null(), 0) } == 0 {
            return Err("无法创建 Antigravity 采集管道".into());
        }
        Ok((Handle(read), Handle(write)))
    }

    fn environment(powershell: &Path, cli: &Path, settings: &Path) -> Vec<u16> {
        let mut vars: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| {
                !key.eq_ignore_ascii_case("BROWSER")
                    && !key.eq_ignore_ascii_case("AGY_CLI_DISABLE_AUTO_UPDATE")
                    && !key.eq_ignore_ascii_case("PATH")
                    && !key.eq_ignore_ascii_case("GRANDETTOKEN_AGY_CLI")
                    && !key.eq_ignore_ascii_case("GRANDETTOKEN_AGY_SETTINGS")
            })
            .collect();
        let mut paths = vec![powershell.parent().unwrap().to_path_buf()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        vars.push((
            "PATH".into(),
            std::env::join_paths(paths).unwrap_or_default(),
        ));
        vars.push(("BROWSER".into(), "NUL".into()));
        vars.push(("AGY_CLI_DISABLE_AUTO_UPDATE".into(), "true".into()));
        vars.push(("GRANDETTOKEN_AGY_CLI".into(), cli.as_os_str().to_owned()));
        vars.push((
            "GRANDETTOKEN_AGY_SETTINGS".into(),
            settings.as_os_str().to_owned(),
        ));
        vars.sort_by_key(|(key, _)| key.to_ascii_uppercase());
        let mut block = Vec::new();
        for (key, value) in vars {
            block.extend(key.encode_wide());
            block.push('=' as u16);
            block.extend(value.encode_wide());
            block.push(0);
        }
        block.push(0);
        block
    }

    /// Windows CommandLineToArgvW quoting, including embedded JSON quotes and trailing slashes.
    fn quote(arg: &str) -> String {
        let mut out = String::from("\"");
        let mut slashes = 0;
        for ch in arg.chars() {
            if ch == '\\' {
                slashes += 1;
                continue;
            }
            out.extend(std::iter::repeat('\\').take(slashes * if ch == '"' { 2 } else { 1 }));
            if ch == '"' {
                out.push('\\');
            }
            out.push(ch);
            slashes = 0;
        }
        out.extend(std::iter::repeat('\\').take(slashes * 2));
        out.push('"');
        out
    }

    struct Console(isize);
    impl Drop for Console {
        fn drop(&mut self) {
            unsafe {
                ClosePseudoConsole(self.0);
            }
        }
    }

    struct Attributes {
        // usize alignment is required for PROC_THREAD_ATTRIBUTE_LIST.
        storage: Vec<usize>,
    }
    impl Attributes {
        fn new(console: isize) -> Result<Self, String> {
            let mut bytes = 0;
            unsafe {
                InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut bytes);
            }
            let mut storage = vec![0usize; bytes.div_ceil(std::mem::size_of::<usize>())];
            let ptr = storage.as_mut_ptr().cast();
            if unsafe { InitializeProcThreadAttributeList(ptr, 1, 0, &mut bytes) } == 0 {
                return Err("无法初始化 Antigravity 终端属性".into());
            }
            let attrs = Self { storage };
            if unsafe {
                UpdateProcThreadAttribute(
                    ptr,
                    0,
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                    console as *const c_void,
                    std::mem::size_of::<isize>(),
                    null_mut(),
                    null(),
                )
            } == 0
            {
                return Err("无法绑定 Antigravity 终端".into());
            }
            Ok(attrs)
        }
    }
    impl Drop for Attributes {
        fn drop(&mut self) {
            unsafe {
                DeleteProcThreadAttributeList(self.storage.as_mut_ptr().cast());
            }
        }
    }

    struct Reader {
        stop: Arc<AtomicBool>,
        overflow: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Reader {
        fn start(output: Handle, input: Handle) -> Self {
            let stop = Arc::new(AtomicBool::new(false));
            let overflow = Arc::new(AtomicBool::new(false));
            let stopped = stop.clone();
            let exceeded = overflow.clone();
            let output_raw = output.0 as usize;
            let input_raw = input.0 as usize;
            std::mem::forget(output);
            std::mem::forget(input);
            let thread = std::thread::spawn(move || {
                let mut output = unsafe { fs::File::from_raw_handle(output_raw as _) };
                let mut input = unsafe { fs::File::from_raw_handle(input_raw as _) };
                let mut total = 0;
                let mut tail = Vec::new();
                while !stopped.load(Ordering::Relaxed) {
                    let mut available = 0;
                    if unsafe {
                        PeekNamedPipe(
                            output_raw as _,
                            null_mut(),
                            0,
                            null_mut(),
                            &mut available,
                            null_mut(),
                        )
                    } == 0
                    {
                        break;
                    }
                    if available == 0 {
                        std::thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    let mut buf = [0; 4096];
                    let Ok(n) = output.read(&mut buf) else { break };
                    if n == 0 {
                        break;
                    }
                    total += n;
                    if total > 1024 * 1024 {
                        exceeded.store(true, Ordering::Relaxed);
                    }
                    tail.extend_from_slice(&buf[..n]);
                    for (request, response) in [
                        (&b"\x1b[6n"[..], &b"\x1b[1;1R"[..]),
                        (&b"\x1b]11;?"[..], &b"\x1b]11;rgb:0000/0000/0000\x1b\\"[..]),
                        (&b"\x1b]10;?"[..], &b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"[..]),
                    ] {
                        if tail.windows(request.len()).any(|w| w == request) {
                            if input.write_all(response).is_err() {
                                return;
                            }
                        }
                    }
                    // 保留未完成的转义序列，不保存终端文本或邮箱。
                    let start = tail.iter().rposition(|b| *b == 0x1b).unwrap_or(tail.len());
                    let pending = tail[start..].to_vec();
                    tail = if pending.len() < 16
                        && !pending.ends_with(b"n")
                        && !pending.ends_with(b"\x07")
                    {
                        pending
                    } else {
                        Vec::new()
                    };
                }
            });
            Self {
                stop,
                overflow,
                thread: Some(thread),
            }
        }
    }
    impl Drop for Reader {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    struct Terminal {
        console: Option<Console>,
        reader: Reader,
    }
    impl Drop for Terminal {
        fn drop(&mut self) {
            // ConPTY 关闭期间保持输出排空，任何提前返回也使用同一清理顺序。
            drop(self.console.take());
        }
    }

    fn capture_script() -> &'static str {
        include_str!("antigravity_capture.ps1")
    }

    pub(super) fn capture(cli: &Path) -> Result<serde_json::Value, String> {
        let absolute_cli = if cli.is_absolute() {
            cli.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|_| "无法解析 CLI 相对路径")?
                .join(cli)
        };
        let normalized_cli: PathBuf = absolute_cli.components().collect();
        let cli = normalized_cli.as_path();
        let folder = Folder::new()?;
        let ps = PathBuf::from(std::env::var_os("SystemRoot").ok_or("无法定位系统 PowerShell")?)
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        // 编码参数避免 cmd /c 对多层引号的解释，也支持含空格/单引号的用户目录。
        let source = format!(
            "& {{ {} }} -CaptureDirectory '{}'",
            capture_script(),
            folder.0.to_string_lossy().replace('\'', "''")
        );
        let bytes: Vec<u8> = source.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let settings = serde_json::json!({"statusLine": {"type":"command", "command":
            format!("powershell.exe -NoProfile -NonInteractive -EncodedCommand {encoded}")}})
        .to_string();
        let settings_file = folder.0.join("settings.json");
        fs::write(&settings_file, settings).map_err(|_| "无法准备单次 CLI 采集配置")?;
        let (input_read, input_write) = pipe()?;
        let (output_read, output_write) = pipe()?;
        let mut console = 0;
        if unsafe {
            CreatePseudoConsole(
                COORD { X: 120, Y: 40 },
                input_read.0,
                output_write.0,
                0,
                &mut console,
            )
        } < 0
        {
            return Err("无法创建 Antigravity 临时终端".into());
        }
        let console = Console(console);
        drop(input_read);
        drop(output_write);
        // 排空线程必须覆盖 ClosePseudoConsole，防止 ConPTY 退场写缓冲时阻塞。
        let reader = Reader::start(output_read, input_write);
        let terminal = Terminal {
            console: Some(console),
            reader,
        };
        let mut attrs = Attributes::new(terminal.console.as_ref().unwrap().0)?;
        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.lpAttributeList = attrs.storage.as_mut_ptr().cast();
        let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let batch = cli
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"));
        let (executable, arguments) = if batch {
            let shell =
                PathBuf::from(std::env::var_os("SystemRoot").ok_or("无法定位系统命令处理器")?)
                    .join("System32")
                    .join("cmd.exe");
            // 通过环境变量单次展开路径，关闭 AutoRun 与延迟展开以保留路径中的特殊字符。
            let arguments = format!("{} /d /v:off /s /c \"\"%GRANDETTOKEN_AGY_CLI%\" --settings \"%GRANDETTOKEN_AGY_SETTINGS%\"\"", quote(&shell.to_string_lossy()));
            (shell, arguments)
        } else {
            (
                cli.to_path_buf(),
                format!(
                    "{} --settings {}",
                    quote(&cli.to_string_lossy()),
                    quote(&settings_file.to_string_lossy())
                ),
            )
        };
        let exe = wide(executable);
        let mut command = wide(arguments);
        let directory = wide(&folder.0);
        let env = environment(&ps, cli, &settings_file);
        let job = Handle(unsafe { CreateJobObjectW(null(), null()) });
        if job.0.is_null() {
            return Err("无法创建 Antigravity 进程组".into());
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as _,
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err("无法设置 Antigravity 进程组清理".into());
        }
        if unsafe {
            CreateProcessW(
                exe.as_ptr(),
                command.as_mut_ptr(),
                null(),
                null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
                env.as_ptr().cast(),
                directory.as_ptr(),
                &startup.StartupInfo,
                &mut info,
            )
        } == 0
        {
            return Err("无法启动官方 agy 账号采集".into());
        }
        let process = Handle(info.hProcess);
        let thread = Handle(info.hThread);
        if unsafe { AssignProcessToJobObject(job.0, process.0) } == 0 {
            unsafe {
                TerminateProcess(process.0, 1);
            }
            return Err("无法将 agy 加入自动清理进程组".into());
        }
        if unsafe { ResumeThread(thread.0) } == u32::MAX {
            return Err("无法启动 agy 临时会话".into());
        }
        let deadline = Instant::now() + Duration::from_secs(35);
        let output = folder.0.join("account.json");
        let result = loop {
            if terminal.reader.overflow.load(Ordering::Relaxed) {
                break Err("agy 终端输出超过安全上限".into());
            }
            if let Ok(meta) = fs::metadata(&output) {
                if meta.len() > 128 * 1024 {
                    break Err("agy 状态栏数据超过安全上限".into());
                }
                if let Ok(bytes) = fs::read(&output) {
                    if let Ok(body) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                        break Ok(body);
                    }
                }
            }
            if unsafe { WaitForSingleObject(process.0, 0) } == 0 {
                break Err("agy 会话结束，账号与额度尚未就绪，请在官方客户端确认登录".into());
            }
            if Instant::now() >= deadline {
                break Err("账号与额度读取超时，请在官方客户端确认登录后重试".into());
            }
            std::thread::sleep(Duration::from_millis(40));
        };
        drop(job);
        unsafe {
            WaitForSingleObject(process.0, 5000);
        }
        drop(terminal);
        result
    }
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn cmd_launcher_accepts_settings_file_and_special_path_characters() {
        let mut random = [0u8; 8];
        getrandom::getrandom(&mut random).unwrap();
        let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let launcher =
            std::env::temp_dir().join(format!("grandettoken agy & fixture!{suffix}.cmd"));
        let script = "@echo off\r\nif not \"%~1\"==\"--settings\" exit /b 1\r\n>\"%~dp2account.json\" echo {\"account\":\"aaaaaaaaaaaaaaaaaaaaaaaa\",\"quota\":{\"gemini-weekly\":{\"remaining_fraction\":0.5}}}\r\n";
        std::fs::write(&launcher, script).unwrap();
        let result = super::capture(&launcher);
        std::fs::remove_file(launcher).unwrap();
        let body = result.unwrap();
        assert_eq!(body["account"], "aaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(body["quota"]["gemini-weekly"]["remaining_fraction"], 0.5);
    }

    #[test]
    #[ignore = "需要本机官方 agy 登录，只验证回调白名单"]
    fn live_official_callback() {
        let cli = crate::antigravity::resolve_agy_path().unwrap();
        let body = super::capture(&cli).unwrap();
        assert_eq!(body.as_object().unwrap().len(), 2);
        assert_eq!(body["account"].as_str().unwrap().len(), 24);
        assert!(!body["quota"].as_object().unwrap().is_empty());
        let text = body.to_string();
        assert!(!text.contains('@'));
        assert!(!text.contains("transcript"));
        assert!(!text.contains("token"));
    }
}
