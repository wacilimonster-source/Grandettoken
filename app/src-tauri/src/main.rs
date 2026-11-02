// 始终以 Windows GUI 子系统启动：debug 构建也不弹控制台窗口。
// 调试输出走 build/launch-debug.ps1 的重定向文件，或 core::diaglog。
#![windows_subsystem = "windows"]

fn main() {
    tokenscope_lib::run()
}
