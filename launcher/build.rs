// Embeds the icon and version info into the launcher executable.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/minipro_winusb.ico");
        res.set("FileDescription", "MiniPro v6.85 launcher (WinUSB)");
        res.set("ProductName", "MiniPro WinUSB launcher");
        res.set("OriginalFilename", "minipro-launch.exe");
        res.compile().expect("failed to compile Windows resources (is the Windows SDK's rc.exe installed?)");
    }
}
