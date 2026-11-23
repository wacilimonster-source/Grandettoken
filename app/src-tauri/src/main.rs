// 始终以 Windows GUI 子系统启动：debug 构建也不弹控制台窗口。
// 调试输出走 build/launch-debug.ps1 的重定向文件，或 core::diaglog。
#![windows_subsystem = "windows"]

fn main() {
    // 卸载钩子（NSIS PREUNINSTALL）调它摘掉外部客户端里指向已消失网关的托管配置。
    if std::env::args().any(|a| a == "--cleanup-inject") {
        tokenscope_core::inject::cleanup_leftovers();
        std::process::exit(0);
    }
    tokenscope_lib::run()
}
