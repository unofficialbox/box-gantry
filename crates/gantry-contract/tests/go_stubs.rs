//! The FR-5.3 gate: the rendered Go stubs must actually compile with the
//! real Go toolchain — the G-1 loop, applied to the runtime boundary.

use std::process::Command;

#[test]
fn go_stubs_render_deterministically() {
    let manifest = gantry_manifest::go();
    let once = gantry_contract::go_stubs(&gantry_contract::V1, &manifest);
    let twice = gantry_contract::go_stubs(&gantry_contract::V1, &manifest);
    assert_eq!(once, twice);
    // Every contract function is present, context-first where declared.
    for function in gantry_contract::V1.functions {
        assert!(
            once.contains(&format!("{} is not wired", function.name)),
            "stub for {:?} missing",
            function.name
        );
    }
    // Fetch is a session method (receiver *Client); a free builder is not.
    assert!(once.contains(
        "func (c *Client) Fetch(ctx context.Context, request *Request) (*Response, error)"
    ));
    assert!(once.contains("func WithQuery(request *Request, name string, value string) *Request"));
    assert!(once.contains("func New(ts TokenSource, opts ...Option) *Client"));
}

#[test]
fn go_stubs_compile_and_are_gofmt_clean() {
    // Skip — loudly, never silently (VR-6) — when no Go toolchain exists.
    if Command::new("go").arg("version").output().is_err() {
        eprintln!("SKIPPED: go toolchain not available; CI runs this gate");
        return;
    }
    let dir = std::env::temp_dir().join(format!("gantry-stubs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = gantry_contract::go_stubs(&gantry_contract::V1, &gantry_manifest::go());
    std::fs::write(dir.join("runtime.go"), &source).unwrap();
    std::fs::write(dir.join("go.mod"), gantry_contract::GO_MOD).unwrap();

    let build = Command::new("go")
        .args(["build", "./..."])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "go build failed (FR-5.3):\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let vet = Command::new("go")
        .args(["vet", "./..."])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        vet.status.success(),
        "go vet failed:\n{}",
        String::from_utf8_lossy(&vet.stderr)
    );

    // gofmt-clean by construction (G-17): gofmt must produce no diff.
    let fmt = Command::new("gofmt").arg("-l").arg(&dir).output().unwrap();
    assert!(
        fmt.status.success() && fmt.stdout.is_empty(),
        "gofmt wants changes in:\n{}",
        String::from_utf8_lossy(&fmt.stdout)
    );
}
