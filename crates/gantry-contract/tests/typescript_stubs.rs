//! The FR-5.3 gate for TypeScript: the rendered stubs must type-check with the
//! real compiler (`tsc --noEmit`, strict) — the G-1 loop at the runtime
//! boundary, keyed off the async/exceptions manifest axes.

use std::process::Command;

#[test]
fn typescript_stubs_render_deterministically() {
    let manifest = gantry_manifest::typescript();
    let once = gantry_contract::typescript_stubs(&gantry_contract::V1, &manifest);
    let twice = gantry_contract::typescript_stubs(&gantry_contract::V1, &manifest);
    assert_eq!(once, twice);
    // Every contract function is present.
    for function in gantry_contract::V1.functions {
        assert!(
            once.contains(&format!("{} is not wired", function.name)),
            "stub for {:?} missing",
            function.name
        );
    }
    // The network entry points return `Promise<T>` (the Async axis); builders
    // are sync. The error model is a `BoxApiError` subclass (Exceptions axis).
    assert!(once.contains("async fetch(request: Request): Promise<Response>"));
    assert!(once.contains("async accessToken(): Promise<string>"));
    assert!(once.contains("baseUrl(name: string): string"));
    assert!(once.contains(
        "export function withQuery(request: Request, name: string, value: string): Request"
    ));
    assert!(once.contains("export class BoxApiError extends Error"));
}

#[test]
fn typescript_stubs_type_check() {
    // Skip loudly (VR-6) when tsc is missing; CI runs this gate.
    if Command::new("tsc").arg("--version").output().is_err() {
        eprintln!("SKIPPED: tsc not available; CI runs this gate");
        return;
    }
    let dir = std::env::temp_dir().join(format!("gantry-ts-stubs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let source =
        gantry_contract::typescript_stubs(&gantry_contract::V1, &gantry_manifest::typescript());
    std::fs::write(dir.join("src/runtime.ts"), &source).unwrap();
    std::fs::write(
        dir.join("tsconfig.json"),
        "{ \"compilerOptions\": { \"strict\": true, \"noEmit\": true, \"target\": \"ES2022\", \
         \"module\": \"NodeNext\", \"moduleResolution\": \"NodeNext\", \"skipLibCheck\": true }, \
         \"include\": [\"src/**/*.ts\"] }\n",
    )
    .unwrap();

    let check = Command::new("tsc")
        .args(["--noEmit", "-p", "tsconfig.json"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "tsc --noEmit failed on the stubs (FR-5.3):\n{}\n{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
}
