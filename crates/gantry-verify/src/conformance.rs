//! The conformance checklist (VR-3, TR-Go.6, TR-Apex).
//!
//! Turns the R§1 capability contract into a machine-checkable, per-target
//! checklist: it derives the *expected* surface from the verified program
//! (managers, operations, paginated surfaces) and measures the *actual*
//! surface from the generated files, capability by capability. Reported
//! every CI run and release-blocking — a shortfall (a manager without a
//! method, a paginated operation without a surface) fails the gate instead
//! of shipping a partial SDK.
//!
//! The checklist reads a lightweight [`GeneratedView`] (path + content), not
//! any backend's file type, and takes a [`TargetShape`] that says how *that*
//! target encodes each capability — so the same contract measures Go, Apex,
//! and Rust by swapping recognizers, never by forking the checklist. A
//! target may also declare **documented platform exclusions** (e.g. Apex
//! erases the tri-state, so there is no serialization package): parity is
//! then measured *minus* those, which is exactly the v2 acceptance criterion
//! ("conformance parity with v1 minus manifest-documented platform
//! exclusions, each recorded in `DECISIONS.md`").

use std::fmt::Write as _;

use gantry_sema::Analysis;
use gantry_synth::detect_pagination;

/// One generated file, decoupled from the backend's own type so the
/// checklist depends only on the IR crates.
#[derive(Debug, Clone, Copy)]
pub struct GeneratedView<'a> {
    pub path: &'a str,
    pub content: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// The output provides at least what the contract expects.
    Pass,
    /// The output falls short, and the shortfall is *not* covered by a
    /// documented platform exclusion — the release gate fails.
    Fail,
    /// The output falls short, but the whole shortfall is a documented
    /// platform exclusion (recorded in `DECISIONS.md`); the gate passes.
    Excluded,
}

/// One capability's line in the checklist: what the contract expects, what
/// the output actually provides, and whether it clears the bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// The capability name (`"managers"`, `"operations"`, `"pagination"`…).
    pub capability: &'static str,
    /// The count the R§1 contract requires (derived from the program).
    pub expected: usize,
    /// The count present in the generated output.
    pub actual: usize,
    pub status: CheckStatus,
    /// A human note — what the numbers mean, or what fell short / is excluded.
    pub detail: String,
}

/// A documented platform exclusion: a capability that a target genuinely
/// cannot (or need not) provide, with the count excused and a reason. Parity
/// is measured minus these; each reason is also recorded in `DECISIONS.md`.
#[derive(Debug, Clone, Copy)]
pub struct Exclusion {
    pub capability: &'static str,
    /// How many of the expected units this exclusion excuses.
    pub count: usize,
    pub reason: &'static str,
}

/// How one target's generated output encodes each conformance capability.
/// Every field measures the *actual* surface from the files; the *expected*
/// surface is derived from the program by [`conformance`], so the two never
/// drift. `exclusions` lists the documented platform exclusions for the
/// target (empty for Go).
#[derive(Debug, Clone, Copy)]
pub struct TargetShape {
    pub target: &'static str,
    pub count_managers: fn(&[GeneratedView]) -> usize,
    pub count_manager_docs: fn(&[GeneratedView]) -> usize,
    pub count_operations: fn(&[GeneratedView]) -> usize,
    pub count_pagination: fn(&[GeneratedView]) -> usize,
    pub count_serialization: fn(&[GeneratedView]) -> usize,
    pub count_round_trip_tests: fn(&[GeneratedView]) -> usize,
    pub count_auth: fn(&[GeneratedView]) -> usize,
    pub count_traceability: fn(&[GeneratedView]) -> usize,
    pub count_guides: fn(&[GeneratedView]) -> usize,
    pub exclusions: &'static [Exclusion],
}

/// The full per-target conformance report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceReport {
    pub target: String,
    pub checks: Vec<Check>,
}

impl ConformanceReport {
    /// True when no capability fails outright (documented exclusions pass —
    /// they are the v2 "parity minus platform exclusions" allowance).
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|c| c.status != CheckStatus::Fail)
    }

    /// Capabilities that fail outright (excludes documented exclusions).
    pub fn failures(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.status == CheckStatus::Fail)
            .count()
    }

    /// Capabilities passing on a documented platform exclusion.
    pub fn excluded(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.status == CheckStatus::Excluded)
            .count()
    }

    /// A deterministic, human-readable checklist.
    pub fn report(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "conformance ({}): {} capabilities, {} excluded, {} failing — {}",
            self.target,
            self.checks.len(),
            self.excluded(),
            self.failures(),
            if self.passed() { "PASS" } else { "FAIL" },
        );
        for check in &self.checks {
            let mark = match check.status {
                CheckStatus::Pass => "ok  ",
                CheckStatus::Fail => "FAIL",
                CheckStatus::Excluded => "n/a ",
            };
            let _ = writeln!(
                out,
                "  {mark} {:<16} expected {:>4}, generated {:>4} — {}",
                check.capability, check.expected, check.actual, check.detail,
            );
        }
        out
    }
}

/// Build the conformance checklist for a target from the verified program
/// and its generated files, measured through the target's [`TargetShape`].
pub fn conformance(
    shape: &TargetShape,
    analysis: &Analysis<'_>,
    files: &[GeneratedView],
) -> ConformanceReport {
    let program = analysis.program;
    let managers = analysis.managers.len();
    let paged = detect_pagination(analysis).len();

    let checks = vec![
        capability(
            shape,
            "managers",
            managers,
            (shape.count_managers)(files),
            "x-box-tag groups → generated manager artifacts",
        ),
        capability(
            shape,
            "manager-docs",
            managers,
            (shape.count_manager_docs)(files),
            "managers → reference doc pages",
        ),
        capability(
            shape,
            "operations",
            program.operations.len(),
            (shape.count_operations)(files),
            "operations → generated methods",
        ),
        capability(
            shape,
            "pagination",
            paged,
            (shape.count_pagination)(files),
            "paginated operations → paged surfaces",
        ),
        capability(
            shape,
            "serialization",
            1,
            (shape.count_serialization)(files),
            "tri-state (absent/null/value) + Date support",
        ),
        capability(
            shape,
            "round-trip-tests",
            1,
            (shape.count_round_trip_tests)(files),
            "generated round-trip / behavioral tests",
        ),
        capability(
            shape,
            "auth-flows",
            4,
            (shape.count_auth)(files),
            "Developer Token / CCG / JWT / OAuth",
        ),
        capability(
            shape,
            "traceability",
            1,
            (shape.count_traceability)(files),
            "engine version + spec fingerprint",
        ),
        capability(
            shape,
            "docs-guides",
            4,
            (shape.count_guides)(files),
            "index + auth/pagination/errors guides",
        ),
    ];

    ConformanceReport {
        target: shape.target.to_string(),
        checks,
    }
}

/// Build one capability's [`Check`], honoring any documented platform
/// exclusion for the target: a shortfall fully covered by an exclusion is
/// [`CheckStatus::Excluded`] (passes), a larger shortfall is
/// [`CheckStatus::Fail`].
fn capability(
    shape: &TargetShape,
    capability: &'static str,
    expected: usize,
    actual: usize,
    detail: &str,
) -> Check {
    let exclusion = shape.exclusions.iter().find(|e| e.capability == capability);
    let excused = exclusion.map_or(0, |e| e.count);

    let status = if actual >= expected {
        CheckStatus::Pass
    } else if actual + excused >= expected {
        CheckStatus::Excluded
    } else {
        CheckStatus::Fail
    };

    let detail = match exclusion {
        Some(e) if status == CheckStatus::Excluded => {
            format!("{detail}; {} excluded — {}", e.count, e.reason)
        }
        _ => detail.to_string(),
    };

    Check {
        capability,
        expected,
        actual,
        status,
        detail,
    }
}

/// Sum a substring's occurrences across the files a predicate selects.
fn count_marker(files: &[GeneratedView], select: fn(&str) -> bool, marker: &str) -> usize {
    files
        .iter()
        .filter(|f| select(f.path))
        .map(|f| f.content.matches(marker).count())
        .sum()
}

// --- Go recognizers -------------------------------------------------------

/// The Go conformance shape: measures the `.go` module layout Go emits.
pub fn go_shape() -> TargetShape {
    TargetShape {
        target: "go",
        count_managers: go_managers,
        count_manager_docs: go_manager_docs,
        count_operations: go_operations,
        count_pagination: go_pagination,
        count_serialization: go_serialization,
        count_round_trip_tests: go_round_trip_tests,
        count_auth: go_auth,
        count_traceability: go_traceability,
        count_guides: go_guides,
        exclusions: &[],
    }
}

fn go_is_manager_file(path: &str) -> bool {
    path.starts_with("managers/") && path.ends_with(".go") && path != "managers/helpers.go"
}

fn go_managers(files: &[GeneratedView]) -> usize {
    files.iter().filter(|f| go_is_manager_file(f.path)).count()
}

fn go_manager_docs(files: &[GeneratedView]) -> usize {
    files
        .iter()
        .filter(|f| f.path.starts_with("docs/managers/") && f.path.ends_with(".md"))
        .count()
}

fn go_operations(files: &[GeneratedView]) -> usize {
    // Operation and pagination methods both take `(ctx context.Context`;
    // subtract the paginators to isolate the plain per-operation methods.
    let ctx = count_marker(files, go_is_manager_file, "(ctx context.Context");
    ctx.saturating_sub(go_pagination(files))
}

fn go_pagination(files: &[GeneratedView]) -> usize {
    count_marker(files, go_is_manager_file, "Paginate(ctx context.Context")
}

fn go_serialization(files: &[GeneratedView]) -> usize {
    usize::from(files.iter().any(|f| {
        f.path == "serialization/serialization.go"
            && f.content.contains("Nullable[")
            && f.content.contains("type Date")
    }))
}

fn go_round_trip_tests(files: &[GeneratedView]) -> usize {
    // The serialization test must be present; then the reported count is the
    // number of per-union round-trip test files (FR-7.8, VR-4).
    if !files
        .iter()
        .any(|f| f.path == "serialization/serialization_test.go")
    {
        return 0;
    }
    files
        .iter()
        .filter(|f| f.path.ends_with("roundtrip_test.go"))
        .count()
}

fn go_auth(files: &[GeneratedView]) -> usize {
    files
        .iter()
        .find(|f| f.path == "docs/auth.md")
        .map(|guide| {
            AUTH_FLOWS
                .iter()
                .filter(|name| guide.content.contains(**name))
                .count()
        })
        .unwrap_or(0)
}

fn go_traceability(files: &[GeneratedView]) -> usize {
    usize::from(files.iter().any(|f| {
        f.path == "buildinfo/buildinfo.go"
            && f.content.contains("EngineVersion")
            && f.content.contains("SpecFingerprint")
    }))
}

fn go_guides(files: &[GeneratedView]) -> usize {
    GUIDES
        .iter()
        .filter(|path| files.iter().any(|f| f.path == **path))
        .count()
}

// --- Apex recognizers -----------------------------------------------------

/// The Apex conformance shape: measures the flat-namespace `.cls` output and
/// the `docs/` tree, and declares the two documented platform exclusions —
/// the erased serialization layer (D-138/D-141) and interactive OAuth
/// (D-141), each recorded in `DECISIONS.md`.
pub fn apex_shape() -> TargetShape {
    TargetShape {
        target: "apex",
        count_managers: apex_managers,
        count_manager_docs: apex_manager_docs,
        count_operations: apex_operations,
        count_pagination: apex_pagination,
        count_serialization: apex_serialization,
        count_round_trip_tests: apex_round_trip_tests,
        count_auth: apex_auth,
        count_traceability: apex_traceability,
        count_guides: apex_guides,
        exclusions: APEX_EXCLUSIONS,
    }
}

const APEX_EXCLUSIONS: &[Exclusion] = &[
    Exclusion {
        capability: "serialization",
        count: 1,
        reason: "Apex erases the tri-state at the type level (every reference is \
                 nullable) and uses the native Date, so absent-vs-null is handled \
                 inline by the wire remap (D-138) — there is no Nullable[T]/Date \
                 package to emit (D-141)",
    },
    Exclusion {
        capability: "auth-flows",
        count: 1,
        reason: "interactive OAuth 2.0 (authorization-code) needs a browser \
                 redirect/callback that server-side Apex cannot perform; the three \
                 server-to-server flows (Developer Token, CCG, JWT) ship (D-141)",
    },
];

/// A generated Apex manager class: its ApexDoc header carries the em-dash
/// `resource manager —` line (emitted only by the manager-class printer; the
/// `Box` entry point says "per resource manager" without the em-dash).
fn apex_is_manager_class(f: &GeneratedView) -> bool {
    f.path.ends_with(".cls") && f.content.contains("resource manager \u{2014}")
}

fn apex_managers(files: &[GeneratedView]) -> usize {
    files.iter().filter(|f| apex_is_manager_class(f)).count()
}

/// One per-manager reference index: `docs/<manager>/README.md` (the
/// top-level `docs/README.md` has no inner path segment, so it is excluded).
fn apex_is_manager_doc(path: &str) -> bool {
    path.starts_with("docs/")
        && path.ends_with("/README.md")
        && path[..path.len() - "/README.md".len()].contains('/')
}

fn apex_manager_docs(files: &[GeneratedView]) -> usize {
    files.iter().filter(|f| apex_is_manager_doc(f.path)).count()
}

fn apex_operations(files: &[GeneratedView]) -> usize {
    // Every generated manager method opens its ApexDoc with the HTTP verb in
    // backticks (`` * `GET /files/{id}` ``), one line per operation. All HTTP
    // methods the IR models are counted so no valid operation is missed.
    const VERBS: [&str; 8] = [
        "GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "HEAD", "TRACE",
    ];
    files
        .iter()
        .filter(|f| apex_is_manager_class(f))
        .map(|f| {
            VERBS
                .iter()
                .map(|verb| f.content.matches(&format!("* `{verb} ")).count())
                .sum::<usize>()
        })
        .sum()
}

/// A generated per-endpoint doc page (`docs/<manager>/<op>.md`), excluding
/// the per-manager and top-level `README.md` and the top-level guides.
fn apex_is_endpoint_doc(path: &str) -> bool {
    path.starts_with("docs/")
        && path.ends_with(".md")
        && !path.ends_with("/README.md")
        && path["docs/".len()..].contains('/')
}

fn apex_pagination(files: &[GeneratedView]) -> usize {
    // Paginated endpoints document the cursor loop under a `## Pagination`
    // heading (D-131: no page classes — the envelope is the page).
    files
        .iter()
        .filter(|f| apex_is_endpoint_doc(f.path) && f.content.contains("## Pagination"))
        .count()
}

fn apex_serialization(_files: &[GeneratedView]) -> usize {
    // Erased at the type level — see the documented exclusion above.
    0
}

fn apex_round_trip_tests(files: &[GeneratedView]) -> usize {
    // The generated @isTest suite: one per-manager test that drives every
    // operation through the mock, plus the shared union-dispatch test.
    files
        .iter()
        .filter(|f| {
            f.path.ends_with(".cls")
                && (f.content.contains("exercisesEveryOperation()")
                    || f.content.contains("dispatchesEveryUnion()"))
        })
        .count()
}

fn apex_auth(files: &[GeneratedView]) -> usize {
    // The three server-viable token providers ship as runtime classes; OAuth
    // is the documented exclusion. Count the concrete provider classes.
    [
        "BoxDeveloperTokenProvider",
        "BoxCcgTokenProvider",
        "BoxJwtTokenProvider",
    ]
    .iter()
    .filter(|name| {
        files
            .iter()
            .any(|f| f.content.contains(&format!("class {name}")))
    })
    .count()
}

fn apex_traceability(files: &[GeneratedView]) -> usize {
    usize::from(files.iter().any(|f| {
        f.path.ends_with("BoxBuildInfo.cls")
            && f.content.contains("ENGINE_VERSION")
            && f.content.contains("SPEC_FINGERPRINT")
    }))
}

fn apex_guides(files: &[GeneratedView]) -> usize {
    GUIDES
        .iter()
        .filter(|path| files.iter().any(|f| f.path == **path))
        .count()
}

// --- Rust recognizers -----------------------------------------------------

/// The Rust conformance shape: measures the `src/` crate layout the Rust
/// backend emits (managers, operation methods, the tri-state helper, and the
/// buildinfo provenance).
///
/// Generated docs and tests are **not yet emitted** (later M5 slices), so those
/// capabilities read zero until they land. Unlike Apex, there are no platform
/// *exclusions* here: the shortfalls are pending work, not permanent — so
/// `conform --target rust` is a progress report (partial today) rather than a
/// green CI gate, and it joins the release gate once the Rust backend reaches
/// parity.
pub fn rust_shape() -> TargetShape {
    TargetShape {
        target: "rust",
        count_managers: rust_managers,
        count_manager_docs: rust_manager_docs,
        count_operations: rust_operations,
        count_pagination: rust_pagination,
        count_serialization: rust_serialization,
        count_round_trip_tests: rust_round_trip_tests,
        count_auth: rust_auth,
        count_traceability: rust_traceability,
        count_guides: rust_guides,
        exclusions: &[],
    }
}

fn rust_round_trip_tests(files: &[GeneratedView]) -> usize {
    // Both the serialization behavioral test (tri-state + typed date/time) and
    // at least one per-union round-trip test must be present — a missing or
    // empty `roundtrip_tests.rs` fails the gate, mirroring the Go recognizer
    // (FR-7.8, VR-4). The reported count is the number of per-union tests.
    if !files.iter().any(|f| f.path == "src/serialization_tests.rs") {
        return 0;
    }
    files
        .iter()
        .filter(|f| f.path == "src/roundtrip_tests.rs")
        .map(|f| f.content.matches("_round_trip(").count())
        .sum()
}

fn rust_is_manager_file(path: &str) -> bool {
    path.starts_with("src/managers/") && path.ends_with(".rs") && path != "src/managers/mod.rs"
}

fn rust_managers(files: &[GeneratedView]) -> usize {
    // One `<Name>Manager` struct per x-box-tag (a module file may hold several).
    // Match the struct definition line, not its `impl` block or options structs.
    files
        .iter()
        .filter(|f| rust_is_manager_file(f.path))
        .map(|f| {
            f.content
                .lines()
                .filter(|l| l.trim_start().starts_with("pub struct ") && l.contains("Manager {"))
                .count()
        })
        .sum()
}

fn rust_operations(files: &[GeneratedView]) -> usize {
    // One `pub async fn` per operation, plus one `async fn next` per paginator;
    // subtract the paginators to isolate the operation methods.
    let async_fns = count_marker(files, rust_is_manager_file, "pub async fn ");
    async_fns.saturating_sub(rust_pagination(files))
}

fn rust_pagination(files: &[GeneratedView]) -> usize {
    // One `<method>_paginate` constructor per paginated operation.
    count_marker(files, rust_is_manager_file, "_paginate(")
}

fn rust_serialization(files: &[GeneratedView]) -> usize {
    // The tri-state helper (D-110). Typed `Date` is a later slice, so only the
    // absent/null/value tri-state is measured here.
    usize::from(
        files
            .iter()
            .any(|f| f.path == "src/serde_helpers.rs" && f.content.contains("double_option")),
    )
}

fn rust_traceability(files: &[GeneratedView]) -> usize {
    // The `buildinfo` provenance module lives in the crate root (`src/lib.rs`).
    usize::from(files.iter().any(|f| {
        f.path == "src/lib.rs"
            && f.content.contains("ENGINE")
            && f.content.contains("SPEC_FINGERPRINT")
    }))
}

fn rust_manager_docs(files: &[GeneratedView]) -> usize {
    // One reference page per manager under the shared `docs/managers/` tree
    // (FR-7.7) — the same layout the Go backend emits.
    files
        .iter()
        .filter(|f| f.path.starts_with("docs/managers/") && f.path.ends_with(".md"))
        .count()
}

fn rust_auth(files: &[GeneratedView]) -> usize {
    // The auth guide documents every Box auth flow the runtime surfaces.
    files
        .iter()
        .find(|f| f.path == "docs/auth.md")
        .map(|guide| {
            AUTH_FLOWS
                .iter()
                .filter(|name| guide.content.contains(**name))
                .count()
        })
        .unwrap_or(0)
}

fn rust_guides(files: &[GeneratedView]) -> usize {
    GUIDES
        .iter()
        .filter(|path| files.iter().any(|f| f.path == **path))
        .count()
}

/// The four Box auth flows the R§1 contract expects to be surfaced.
const AUTH_FLOWS: [&str; 4] = ["Developer Token", "Client Credentials", "JWT", "OAuth"];

/// The cross-cutting guide set (FR-7.7): the index plus the three topic
/// guides. Both targets emit these paths under `docs/`.
const GUIDES: [&str; 4] = [
    "docs/README.md",
    "docs/auth.md",
    "docs/pagination.md",
    "docs/errors.md",
];

#[cfg(test)]
mod tests {
    use gantry_ir as ir;

    use super::*;

    fn operation(name: &str, manager: &str) -> ir::Operation {
        ir::Operation {
            name: ir::Identifier::new(name).unwrap(),
            variation: None,
            manager: ir::Identifier::new(manager).unwrap(),
            api_version: None,
            method: ir::HttpMethod::Get,
            base_url: ir::BaseUrl::Api,
            path: vec![ir::PathSegment::Literal("files".into())],
            params: vec![],
            request: None,
            response: ir::ResponseShape::None,
            deprecated: false,
        }
    }

    /// A program with one manager and two (non-paginated) operations.
    fn program() -> ir::Program {
        let mut program = ir::Program::default();
        program.operations.push(operation("GetFiles", "files"));
        program.operations.push(operation("GetFilesId", "files"));
        program
    }

    /// A generated file set that fully satisfies the contract for `program`.
    fn conformant_files() -> Vec<(String, String)> {
        vec![
            (
                "managers/files.go".into(),
                "func (c *FilesManager) GetFiles(ctx context.Context) {}\n\
                 func (c *FilesManager) GetFilesId(ctx context.Context) {}\n"
                    .into(),
            ),
            ("managers/helpers.go".into(), "package managers\n".into()),
            (
                "docs/managers/files.md".into(),
                "Access via `client.NewClient().Files`".into(),
            ),
            (
                "serialization/serialization.go".into(),
                "type Nullable[T any] struct{}\ntype Date struct{}\n".into(),
            ),
            (
                "serialization/serialization_test.go".into(),
                "package serialization\n".into(),
            ),
            (
                "schemas/roundtrip_test.go".into(),
                "package schemas\n".into(),
            ),
            (
                "docs/auth.md".into(),
                "Developer Token, Client Credentials, JWT, OAuth".into(),
            ),
            ("docs/README.md".into(), "index".into()),
            ("docs/pagination.md".into(), "pages".into()),
            ("docs/errors.md".into(), "errors".into()),
            (
                "buildinfo/buildinfo.go".into(),
                "const EngineVersion = \"0.1.0\"\nconst SpecFingerprint = \"abc\"\n".into(),
            ),
        ]
    }

    fn views<'a>(files: &'a [(String, String)]) -> Vec<GeneratedView<'a>> {
        files
            .iter()
            .map(|(path, content)| GeneratedView { path, content })
            .collect()
    }

    #[test]
    fn a_complete_sdk_passes_every_capability() {
        let program = program();
        let analysis = gantry_sema::analyze(&program).unwrap();
        let files = conformant_files();
        let report = conformance(&go_shape(), &analysis, &views(&files));

        assert!(report.passed(), "{}", report.report());
        assert_eq!(report.failures(), 0);
        // Expected counts are derived from the program, not hard-coded.
        let ops = report
            .checks
            .iter()
            .find(|c| c.capability == "operations")
            .unwrap();
        assert_eq!(ops.expected, 2);
        assert_eq!(ops.actual, 2);
        let managers = report
            .checks
            .iter()
            .find(|c| c.capability == "managers")
            .unwrap();
        assert_eq!(managers.expected, 1);
        assert_eq!(managers.actual, 1); // helpers.go is excluded
    }

    #[test]
    fn a_missing_operation_method_fails_the_gate() {
        let program = program();
        let analysis = gantry_sema::analyze(&program).unwrap();
        let mut files = conformant_files();
        // Drop one operation method from the manager file.
        files[0].1 = "func (c *FilesManager) GetFiles(ctx context.Context) {}\n".into();

        let report = conformance(&go_shape(), &analysis, &views(&files));
        assert!(!report.passed());
        let ops = report
            .checks
            .iter()
            .find(|c| c.capability == "operations")
            .unwrap();
        assert_eq!(ops.expected, 2);
        assert_eq!(ops.actual, 1);
        assert_eq!(ops.status, CheckStatus::Fail);
    }

    #[test]
    fn a_missing_auth_flow_fails_the_gate() {
        let program = program();
        let analysis = gantry_sema::analyze(&program).unwrap();
        let mut files = conformant_files();
        // Auth guide that omits JWT and OAuth.
        let auth = files.iter_mut().find(|(p, _)| p == "docs/auth.md").unwrap();
        auth.1 = "Developer Token, Client Credentials".into();

        let report = conformance(&go_shape(), &analysis, &views(&files));
        assert!(!report.passed());
        let flows = report
            .checks
            .iter()
            .find(|c| c.capability == "auth-flows")
            .unwrap();
        assert_eq!(flows.expected, 4);
        assert_eq!(flows.actual, 2);
        assert_eq!(flows.status, CheckStatus::Fail);
    }

    #[test]
    fn a_documented_exclusion_passes_as_not_applicable() {
        // A shape that excuses the whole `serialization` shortfall passes it
        // as Excluded, not Fail — the "parity minus platform exclusions" rule.
        let program = program();
        let analysis = gantry_sema::analyze(&program).unwrap();
        let mut files = conformant_files();
        // Remove the serialization package entirely.
        files.retain(|(p, _)| p != "serialization/serialization.go");

        const EXCLUDED: &[Exclusion] = &[Exclusion {
            capability: "serialization",
            count: 1,
            reason: "erased at the type level (test)",
        }];
        let mut shape = go_shape();
        shape.exclusions = EXCLUDED;

        let report = conformance(&shape, &analysis, &views(&files));
        let serialization = report
            .checks
            .iter()
            .find(|c| c.capability == "serialization")
            .unwrap();
        assert_eq!(serialization.status, CheckStatus::Excluded);
        assert!(serialization.detail.contains("erased at the type level"));
        // Excluded is not a failure — the gate still passes.
        assert!(report.passed());
        assert_eq!(report.failures(), 0);
        assert_eq!(report.excluded(), 1);
    }

    #[test]
    fn rust_shape_measures_full_capability_parity() {
        let program = program();
        let analysis = gantry_sema::analyze(&program).unwrap();
        // A minimal but complete Rust SDK: one manager with two operation
        // methods and a paginator (struct + `next` + `_paginate` constructor),
        // the tri-state helper, the buildinfo provenance, the `docs/` tree
        // (per-manager page + the guides), and the generated round-trip tests.
        let files =
            vec![
            (
                "src/managers/files.rs".to_string(),
                "pub struct FilesGetFilesPaginator {}\n\
                 impl FilesGetFilesPaginator {\n\
                 \x20   pub async fn next(&mut self) {}\n\
                 }\n\
                 pub struct FilesManager {}\n\
                 impl FilesManager {\n\
                 \x20   pub(crate) fn new() -> Self { Self {} }\n\
                 \x20   pub async fn get_files(&self) {}\n\
                 \x20   pub async fn get_files_id(&self) {}\n\
                 \x20   pub fn get_files_paginate(&self) {}\n\
                 }\n"
                .to_string(),
            ),
            (
                "src/managers/mod.rs".to_string(),
                "pub mod files;\n".to_string(),
            ),
            (
                "src/serde_helpers.rs".to_string(),
                "pub(crate) fn double_option() {}\n".to_string(),
            ),
            (
                "src/lib.rs".to_string(),
                "pub mod buildinfo {\n\
                 \x20   pub const ENGINE: &str = \"0.1.0\";\n\
                 \x20   pub const SPEC_FINGERPRINT: &str = \"abc\";\n\
                 }\n"
                .to_string(),
            ),
            ("docs/README.md".to_string(), "# Box SDK reference\n".to_string()),
            (
                "docs/managers/files.md".to_string(),
                "# FilesManager\n".to_string(),
            ),
            (
                "docs/auth.md".to_string(),
                "# Authentication\n## Developer Token\n## Client Credentials\n## JWT\n## OAuth\n"
                    .to_string(),
            ),
            ("docs/pagination.md".to_string(), "# Pagination\n".to_string()),
            ("docs/errors.md".to_string(), "# Errors\n".to_string()),
            (
                "src/serialization_tests.rs".to_string(),
                "#[test]\nfn date_round_trip() {}\n".to_string(),
            ),
            (
                "src/roundtrip_tests.rs".to_string(),
                "#[test]\nfn schemas_item_round_trip() {}\n".to_string(),
            ),
        ];
        let report = conformance(&rust_shape(), &analysis, &views(&files));
        assert!(report.report().contains("conformance (rust)"));

        let check = |cap: &str| report.checks.iter().find(|c| c.capability == cap).unwrap();
        // Emitted capabilities are measured and pass.
        assert_eq!(check("managers").actual, 1);
        assert_eq!(check("managers").status, CheckStatus::Pass);
        // Operations subtract the paginator's `next` from the async-fn count.
        assert_eq!(check("operations").actual, 2);
        assert_eq!(check("operations").status, CheckStatus::Pass);
        // The `_paginate` constructor is counted as a paged surface.
        assert_eq!(check("pagination").actual, 1);
        assert_eq!(check("serialization").status, CheckStatus::Pass);
        assert_eq!(check("traceability").status, CheckStatus::Pass);
        // Docs are emitted: the per-manager page, all four guides, and every
        // auth flow documented in the auth guide.
        assert_eq!(check("manager-docs").actual, 1);
        assert_eq!(check("manager-docs").status, CheckStatus::Pass);
        assert_eq!(check("docs-guides").actual, 4);
        assert_eq!(check("docs-guides").status, CheckStatus::Pass);
        assert_eq!(check("auth-flows").actual, 4);
        assert_eq!(check("auth-flows").status, CheckStatus::Pass);
        // Generated round-trip tests: one per-union round-trip test present
        // (gated on the serialization behavioral test) — the capability passes.
        assert_eq!(check("round-trip-tests").actual, 1);
        assert_eq!(check("round-trip-tests").status, CheckStatus::Pass);
        // Every capability is emitted now — the Rust report passes end to end.
        assert!(report.passed());

        // Regression: dropping the per-union round-trip file fails the gate even
        // though the serialization behavioral test remains.
        let without_unions: Vec<(String, String)> = files
            .iter()
            .filter(|(path, _)| path != "src/roundtrip_tests.rs")
            .cloned()
            .collect();
        let degraded = conformance(&rust_shape(), &analysis, &views(&without_unions));
        let rt = degraded
            .checks
            .iter()
            .find(|c| c.capability == "round-trip-tests")
            .unwrap();
        assert_eq!(rt.actual, 0);
        assert_eq!(rt.status, CheckStatus::Fail);
        assert!(!degraded.passed());
    }

    #[test]
    fn report_is_deterministic() {
        let program = program();
        let analysis = gantry_sema::analyze(&program).unwrap();
        let files = conformant_files();
        let once = conformance(&go_shape(), &analysis, &views(&files)).report();
        let twice = conformance(&go_shape(), &analysis, &views(&files)).report();
        assert_eq!(once, twice);
        assert!(once.contains("conformance (go)"));
        assert!(once.contains("PASS"));
    }
}
