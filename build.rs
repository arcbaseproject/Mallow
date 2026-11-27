fn main() {
    println!("cargo:rustc-check-cfg=cfg(sodium_soname)");
    if !std::process::Command::new("pkg-config")
        .args(["--exists", "libsodium"])
        .status()
        .is_ok_and(|s| s.success())
    {
        println!("cargo:rustc-cfg=sodium_soname");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        build_web_extension();
    }
    glib_build_tools::compile_resources(&["assets"], "assets/resources.gresource.xml", "mallow.gresource");
}

/// The WebKit web-process extension (src/engine/webext/memory.c) as a shared
/// object in OUT_DIR. The binary embeds it and writes it out at startup.
fn build_web_extension() {
    let src = "src/engine/webext/memory.c";
    println!("cargo:rerun-if-changed={src}");
    let flags = std::process::Command::new("pkg-config")
        .args(["--cflags", "webkitgtk-web-process-extension-6.0"])
        .output()
        .expect("pkg-config not found");
    assert!(
        flags.status.success(),
        "webkitgtk-web-process-extension-6.0 development files not found"
    );
    let flags = String::from_utf8(flags.stdout).unwrap();
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("mallow-webext.so");
    let status = cc::Build::new()
        .get_compiler()
        .to_command()
        .args(["-shared", "-fPIC", "-O2", "-Wall", "-o"])
        .arg(&out)
        .arg(src)
        .args(flags.split_whitespace())
        .status()
        .expect("C compiler not found");
    assert!(status.success(), "building {src} failed");
}
