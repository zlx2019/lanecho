// release 构建关闭 Windows 控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    lanecho_desktop_lib::run()
}
