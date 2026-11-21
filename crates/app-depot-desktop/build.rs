use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=APP_DEPOT_OFFICIAL_RELEASE");
    println!("cargo:rerun-if-env-changed=APP_DEPOT_SOURCE_REVISION");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    let revision = std::env::var("APP_DEPOT_SOURCE_REVISION")
        .ok()
        .or_else(|| {
            let output = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .output()
                .ok()?;
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .map(|s| s.trim().to_owned())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_owned());
    assert!(
        revision == "unknown"
            || (revision.len() == 40 && revision.bytes().all(|c| c.is_ascii_hexdigit()))
    );
    let official = std::env::var("APP_DEPOT_OFFICIAL_RELEASE").as_deref() == Ok("1");
    if official {
        assert_ne!(
            revision, "unknown",
            "official builds require a source revision"
        );
    }
    println!("cargo:rustc-env=APP_DEPOT_SOURCE_REVISION={revision}");
    println!(
        "cargo:rustc-env=APP_DEPOT_BUILD_KIND={}",
        if official { "official" } else { "development" }
    );
}
