//! Embeds the TDM logo in `tdm.exe`, so Explorer and the taskbar show it.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winres::WindowsResource::new();
        res.set_icon("packaging/icons/icon.ico");
        res.compile().expect("embed the application icon");
    }
    println!("cargo:rerun-if-changed=packaging/icons/icon.ico");
}
