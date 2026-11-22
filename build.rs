fn main() {
    println!("cargo:rustc-check-cfg=cfg(sodium_soname)");
    if !std::process::Command::new("pkg-config")
        .args(["--exists", "libsodium"])
        .status()
        .is_ok_and(|s| s.success())
    {
        println!("cargo:rustc-cfg=sodium_soname");
    }
    glib_build_tools::compile_resources(&["assets"], "assets/resources.gresource.xml", "mallow.gresource");
}
