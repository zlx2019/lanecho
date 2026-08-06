// Release builds hide the Windows console window
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    lanecho_desktop_lib::run()
}
