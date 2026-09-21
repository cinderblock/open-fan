// Release builds detach from the console: OpenFan lives in the tray, and a stray console
// window behind the editor is noise. Debug builds keep it so logs are visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    open_fan_lib::run()
}
