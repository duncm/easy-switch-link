fn main() {
    println!("cargo:rerun-if-changed=../../assets/easy-switch-link.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set("ProductName", "Easy-Switch Link");
        resource.set("FileDescription", "Easy-Switch Link");
        resource.set_icon("../../assets/easy-switch-link.ico");
        resource
            .compile()
            .expect("Compile Windows application resources");
    }
}
