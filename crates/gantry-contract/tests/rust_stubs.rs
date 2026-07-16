//! The FR-5.3 gate for Rust: the rendered stubs must compile with the real
//! toolchain (`cargo check` + rustfmt-clean) — the G-1 loop at the runtime
//! boundary, keyed off the async/Result manifest axes.

use std::process::Command;

#[test]
fn rust_stubs_render_deterministically() {
    let manifest = gantry_manifest::rust();
    let once = gantry_contract::rust_stubs(&gantry_contract::V1, &manifest);
    let twice = gantry_contract::rust_stubs(&gantry_contract::V1, &manifest);
    assert_eq!(once, twice);
    // Every contract function is present.
    for function in gantry_contract::V1.functions {
        assert!(
            once.contains(&format!("{} is not wired", function.name)),
            "stub for {:?} missing",
            function.name
        );
    }
    // The network entry points are `async fn` (the Async axis); builders are
    // sync. Fallible functions return `Result<_, Error>`.
    assert!(
        once.contains("pub async fn fetch(&self, request: Request) -> Result<Response, Error>")
    );
    assert!(once.contains("pub async fn access_token(&self) -> Result<String, Error>"));
    assert!(once.contains("pub fn base_url(&self, name: &str) -> String"));
    assert!(
        once.contains("pub fn with_query(request: Request, name: &str, value: &str) -> Request")
    );
    assert!(once.contains("impl From<serde_json::Error> for Error"));
}

#[test]
fn rust_stubs_compile_and_are_rustfmt_clean() {
    // Skip loudly (VR-6) when no cargo toolchain exists; CI runs this gate.
    if Command::new("cargo").arg("--version").output().is_err() {
        eprintln!("SKIPPED: cargo toolchain not available; CI runs this gate");
        return;
    }
    let dir = std::env::temp_dir().join(format!("gantry-rust-stubs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let source = gantry_contract::rust_stubs(&gantry_contract::V1, &gantry_manifest::rust());
    std::fs::write(dir.join("src/runtime.rs"), &source).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"gantryruntime-stub\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
         \n[dependencies]\nserde_json = \"1\"\n\n[lib]\npath = \"src/runtime.rs\"\n",
    )
    .unwrap();

    let check = Command::new("cargo")
        .arg("check")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "cargo check failed (FR-5.3):\n{}",
        String::from_utf8_lossy(&check.stderr)
    );

    let fmt = Command::new("rustfmt")
        .args(["--check", "--edition", "2021"])
        .arg(dir.join("src/runtime.rs"))
        .output()
        .unwrap();
    assert!(
        fmt.status.success(),
        "rustfmt wants changes (G-17):\n{}",
        String::from_utf8_lossy(&fmt.stdout)
    );
}
