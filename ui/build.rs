// Embeds the application icon and version info into minipro-ui.exe.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../launcher/assets/minipro_winusb.ico");
        res.set("FileDescription", "MiniPro - new front-end");
        res.set("ProductName", "MiniPro UI");
        res.set("OriginalFilename", "minipro-ui.exe");
        res.compile().expect("failed to compile Windows resources");
    }
}
