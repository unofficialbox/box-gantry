//! The conformance checklist (VR-3, TR-Go.6).
//!
//! Turns the R§1 capability contract into a machine-checkable, per-target
//! checklist: it derives the *expected* surface from the verified program
//! (managers, operations, paginated surfaces) and measures the *actual*
//! surface from the generated files, capability by capability. Reported
//! every CI run and release-blocking — a shortfall (a manager without a
//! method, a paginated operation without an iterator) fails the gate
//! instead of shipping a partial SDK.
//!
//! The checklist reads a lightweight [`GeneratedView`] (path + content),
//! not any backend's file type, so it stays target-neutral: the same
//! contract will measure the Apex and Rust outputs when they exist.

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
    Pass,
    Fail,
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
    /// A human note — what the numbers mean, or what fell short.
    pub detail: String,
}

/// The full per-target conformance report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceReport {
    pub target: String,
    pub checks: Vec<Check>,
}

impl ConformanceReport {
    /// True when every capability clears its bar (the release gate).
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|c| c.status == CheckStatus::Pass)
    }

    pub fn failures(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.status == CheckStatus::Fail)
            .count()
    }

    /// A deterministic, human-readable checklist.
    pub fn report(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "conformance ({}): {} capabilities, {} failing — {}",
            self.target,
            self.checks.len(),
            self.failures(),
            if self.passed() { "PASS" } else { "FAIL" },
        );
        for check in &self.checks {
            let mark = match check.status {
                CheckStatus::Pass => "ok  ",
                CheckStatus::Fail => "FAIL",
            };
            let _ = writeln!(
                out,
                "  {mark} {:<14} expected {:>4}, generated {:>4} — {}",
                check.capability, check.expected, check.actual, check.detail,
            );
        }
        out
    }
}

/// Build the conformance checklist for a target from the verified program
/// and its generated files.
pub fn conformance(
    target: &str,
    analysis: &Analysis<'_>,
    files: &[GeneratedView],
) -> ConformanceReport {
    let program = analysis.program;
    let mut checks = Vec::new();

    // Managers: every x-box-tag group must produce a manager file.
    let manager_files = files.iter().filter(|f| is_manager_file(f.path)).count();
    checks.push(count_check(
        "managers",
        analysis.managers.len(),
        manager_files,
        "x-box-tag groups → generated manager files",
    ));

    // Manager reference docs: one Markdown page per manager (FR-7.7).
    let manager_docs = files
        .iter()
        .filter(|f| f.path.starts_with("docs/managers/") && f.path.ends_with(".md"))
        .count();
    checks.push(count_check(
        "manager-docs",
        analysis.managers.len(),
        manager_docs,
        "managers → reference doc pages",
    ));

    // Operations: one method per operation. Operation and pagination
    // methods both take `(ctx context.Context`; subtract the paginators to
    // isolate the plain per-operation methods.
    let ctx_sigs = count_marker(files, is_manager_file, "(ctx context.Context");
    let paginate_sigs = count_marker(files, is_manager_file, "Paginate(ctx context.Context");
    let operation_methods = ctx_sigs.saturating_sub(paginate_sigs);
    checks.push(count_check(
        "operations",
        program.operations.len(),
        operation_methods,
        "operations → generated methods",
    ));

    // Pagination: an iterator for every detected paginated surface (FR-7.3).
    let paged = detect_pagination(analysis).len();
    checks.push(count_check(
        "pagination",
        paged,
        paginate_sigs,
        "paginated operations → iterators",
    ));

    // Serialization package: the tri-state wrapper and Date (D-110/D-112).
    let serialization = files.iter().any(|f| {
        f.path == "serialization/serialization.go"
            && f.content.contains("Nullable[")
            && f.content.contains("type Date")
    });
    checks.push(presence_check(
        "serialization",
        serialization,
        "Nullable[T] tri-state + Date package",
    ));

    // Generated round-trip tests: the serialization test plus at least one
    // per-module union test (FR-7.8, VR-4).
    let has_serialization_test = files
        .iter()
        .any(|f| f.path == "serialization/serialization_test.go");
    let union_tests = files
        .iter()
        .filter(|f| f.path.ends_with("roundtrip_test.go"))
        .count();
    // Require the serialization test present *and* at least one union test;
    // when it is, the reported count is the number of union test files.
    let round_trip_actual = if has_serialization_test {
        union_tests
    } else {
        0
    };
    checks.push(count_check(
        "round-trip-tests",
        1,
        round_trip_actual,
        "serialization test + per-union round-trip tests",
    ));

    // Auth flows: all four Box flows surfaced in the generated auth guide
    // (implemented in the hand-written runtime, FR-7.2).
    let auth_flows = files
        .iter()
        .find(|f| f.path == "docs/auth.md")
        .map(|guide| {
            ["Developer Token", "Client Credentials", "JWT", "OAuth"]
                .iter()
                .filter(|name| guide.content.contains(**name))
                .count()
        })
        .unwrap_or(0);
    checks.push(count_check(
        "auth-flows",
        4,
        auth_flows,
        "Developer Token / CCG / JWT / OAuth surfaced",
    ));

    // Cross-cutting guides: the index and the three topic guides (FR-7.7).
    let guides = [
        "docs/README.md",
        "docs/auth.md",
        "docs/pagination.md",
        "docs/errors.md",
    ]
    .iter()
    .filter(|path| files.iter().any(|f| f.path == **path))
    .count();
    checks.push(count_check(
        "docs-guides",
        4,
        guides,
        "index + auth/pagination/errors guides",
    ));

    ConformanceReport {
        target: target.to_string(),
        checks,
    }
}

fn is_manager_file(path: &str) -> bool {
    path.starts_with("managers/") && path.ends_with(".go") && path != "managers/helpers.go"
}

/// Sum a substring's occurrences across the files a predicate selects.
fn count_marker(files: &[GeneratedView], select: fn(&str) -> bool, marker: &str) -> usize {
    files
        .iter()
        .filter(|f| select(f.path))
        .map(|f| f.content.matches(marker).count())
        .sum()
}

/// A capability met iff the output provides at least what the contract
/// expects (never fewer — extra is fine, e.g. helper methods).
fn count_check(capability: &'static str, expected: usize, actual: usize, detail: &str) -> Check {
    let status = if actual >= expected {
        CheckStatus::Pass
    } else {
        CheckStatus::Fail
    };
    Check {
        capability,
        expected,
        actual,
        status,
        detail: detail.to_string(),
    }
}

fn presence_check(capability: &'static str, present: bool, detail: &str) -> Check {
    count_check(capability, 1, usize::from(present), detail)
}

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
        let report = conformance("go", &analysis, &views(&files));

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

        let report = conformance("go", &analysis, &views(&files));
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

        let report = conformance("go", &analysis, &views(&files));
        assert!(!report.passed());
        let flows = report
            .checks
            .iter()
            .find(|c| c.capability == "auth-flows")
            .unwrap();
        assert_eq!(flows.expected, 4);
        assert_eq!(flows.actual, 2);
    }

    #[test]
    fn report_is_deterministic() {
        let program = program();
        let analysis = gantry_sema::analyze(&program).unwrap();
        let files = conformant_files();
        let once = conformance("go", &analysis, &views(&files)).report();
        let twice = conformance("go", &analysis, &views(&files)).report();
        assert_eq!(once, twice);
        assert!(once.contains("conformance (go)"));
        assert!(once.contains("PASS"));
    }
}
