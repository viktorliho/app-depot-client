use std::{env, process::Command};

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    let source = "../../tools/wine-supervisor";
    println!("cargo:rerun-if-changed={source}");
    let status = Command::new("sh")
        .arg(format!("{source}/build.sh"))
        .arg(env::var_os("OUT_DIR").expect("Cargo output directory"))
        .status()
        .expect("build the Windows prerequisite supervisor for Linux hosts");
    assert!(
        status.success(),
        "Building the supervisor requires clang, llvm-dlltool and lld-link"
    );
}
