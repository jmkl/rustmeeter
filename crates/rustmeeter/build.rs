fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/app.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app.ico"); // stored as resource ID 1
        res.set("ProductName", "rustmeeter");
        res.set("FileDescription", "RustMeeter");
        res.compile().expect("failed to embed Windows resources");
    }
}
