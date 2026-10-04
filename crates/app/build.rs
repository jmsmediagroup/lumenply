//! Windows: embed the app icon and version details in the .exe, so
//! Explorer, the taskbar and the installer show Lumenply's icon. Nothing
//! to do on other systems.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/lumenply.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/lumenply.ico");
        res.set("ProductName", "Lumenply");
        res.set("FileDescription", "Lumenply photo editor");
        res.set("LegalCopyright", "Lumenply contributors, GPL-3.0-or-later");
        res.compile()
            .expect("embed the Windows icon and version resource");
    }
}
