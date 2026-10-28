//! `gantry` — the one CLI (FR-8).
//!
//! A thin argument parser over the engine library: no naming rules, no
//! spec shaping, no business logic lives here (FR-8.2 — the `PostFolders`
//! lesson). Subcommands appear as the engine grows them: `check`,
//! `generate`, `verify`, `conform`, `diff`, `names`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Exit codes (FR-8.3). Distinct classes so CI and callers can tell input
/// problems from engine problems. Clap itself exits 2 on usage errors.
mod exit_codes {
    /// The input specs are at fault; fix the spec (or the file list).
    pub const SPEC_ERROR: u8 = 3;
    /// Generated output failed verification (`verify`).
    pub const VERIFICATION_FAILURE: u8 = 4;
    /// An internal invariant broke; file a box-gantry bug.
    pub const ENGINE_BUG: u8 = 5;
}

#[derive(Parser)]
#[command(name = "gantry", version, about = "Box SDK generator")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Ingest and validate spec documents; report what they contain.
    ///
    /// Exits 0 if the whole spec set ingests cleanly, 3 on any spec error.
    Check {
        /// Spec documents: the base spec followed by versioned specs.
        #[arg(required = true, value_name = "SPEC")]
        specs: Vec<PathBuf>,
    },
    /// Generate an SDK from spec documents.
    Generate {
        #[arg(required = true, value_name = "SPEC")]
        specs: Vec<PathBuf>,
        /// Target language(s) (manifest key). `go`, `rust`, `typescript`, and
        /// `java` are feature-complete SDKs; `apex` emits a deployable SFDX
        /// project. Accepts a
        /// comma-separated list or repeated flags to build several at once
        /// (`--target go,rust`); `all` expands to every target. A single target
        /// writes into `--out` directly; two or more each land in their own
        /// `<out>/<target>/` subdirectory.
        #[arg(long, required = true, value_parser = ["go", "apex", "rust", "typescript", "java", "all"], value_delimiter = ',')]
        target: Vec<String>,
        /// Output directory (created if missing). With more than one target,
        /// each SDK lands in a `<out>/<target>/` subdirectory.
        #[arg(long)]
        out: PathBuf,
        /// A JSON file of human-chosen replacements for synthesized names
        /// that are long but not wrong (see `gantry names`). Two optional
        /// top-level maps: `components` (keyed by a top-level schema's own
        /// name — cascades to every field synthesized under it) and
        /// `locations` (keyed by the exact synthesis site `gantry names`
        /// reports — replaces just that one name).
        #[arg(long)]
        overrides: Option<PathBuf>,
    },
    /// Report every synthesized name at or above `--min-length`, grouped by
    /// shared top-level component prefix, with enough detail to write a
    /// `--overrides` file by hand (no invented replacement text — see the
    /// module doc on `gantry_spec::report`). Informational: always exits 0
    /// unless the specs themselves fail to load.
    Names {
        #[arg(required = true, value_name = "SPEC")]
        specs: Vec<PathBuf>,
        #[arg(long, default_value_t = 40)]
        min_length: usize,
    },
    /// Generate, then compile the output with the target's real toolchain
    /// (`go` → VR-1.1; `rust` → VR-1.2). Exits 4 when the generated code fails
    /// verification.
    Verify {
        #[arg(required = true, value_name = "SPEC")]
        specs: Vec<PathBuf>,
        /// Target language (manifest key).
        #[arg(long, value_parser = ["go", "rust", "typescript"])]
        target: String,
    },
    /// Report the R§1 capability conformance checklist for a target
    /// (VR-3): managers, operations, paginated surfaces, auth flows, tests,
    /// and docs — expected (from the spec) vs generated. Exits 4 when any
    /// capability falls short, so CI can gate a partial SDK.
    Conform {
        #[arg(required = true, value_name = "SPEC")]
        specs: Vec<PathBuf>,
        /// Target language (manifest key). `go`, `apex`, `rust`, and
        /// `typescript` are all measured against the same R§1 capability
        /// contract. `rust` and `typescript` are progress reports
        /// (docs/tests/pagination are later slices), not yet green gates.
        #[arg(long, value_parser = ["go", "apex", "rust", "typescript"])]
        target: String,
    },
    /// Diff two spec sets and report breaking vs compatible changes and the
    /// recommended SDK version bump (FR-9). Exits 4 when the diff is
    /// breaking, so CI can gate a major bump.
    Diff {
        /// The baseline spec set.
        #[arg(long = "from", required = true, num_args = 1.., value_name = "SPEC")]
        from: Vec<PathBuf>,
        /// The new spec set.
        #[arg(long = "to", required = true, num_args = 1.., value_name = "SPEC")]
        to: Vec<PathBuf>,
    },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Check { specs } => check(&specs),
        Command::Generate {
            specs,
            target,
            out,
            overrides,
        } => generate(&specs, &target, &out, overrides.as_deref()),
        Command::Verify { specs, target } => verify(&specs, &target),
        Command::Conform { specs, target } => conform(&specs, &target),
        Command::Diff { from, to } => diff(&from, &to),
        Command::Names { specs, min_length } => names(&specs, min_length),
    }
}

/// Load a `--overrides` file if one was given; `None` behaves exactly like
/// `gantry_spec::NameOverrides::empty()` (generation without it is
/// unaffected).
fn load_overrides(path: Option<&Path>) -> Result<gantry_spec::NameOverrides, ExitCode> {
    match path {
        Some(path) => gantry_spec::NameOverrides::load(path).map_err(|err| {
            eprintln!("error: {err}");
            ExitCode::from(exit_codes::SPEC_ERROR)
        }),
        None => Ok(gantry_spec::NameOverrides::empty()),
    }
}

/// `gantry names`: enumerate synthesized names at or above `--min-length`.
/// Always runs against the *raw*, un-overridden state — the report exists
/// to help write an `--overrides` file, so it always shows what's there
/// before one is applied.
fn names(specs: &[PathBuf], min_length: usize) -> ExitCode {
    let set = match gantry_spec::SpecSet::load(specs) {
        Ok(set) => set,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(exit_codes::SPEC_ERROR);
        }
    };
    let lowering = match gantry_spec::lower(&set) {
        Ok(lowering) => lowering,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(exit_codes::SPEC_ERROR);
        }
    };
    let report = gantry_spec::long_names(&lowering.synthesis_log, min_length);
    print!("{}", report.report());
    ExitCode::SUCCESS
}

/// VR-3: report the capability conformance checklist for a target. Prints
/// the checklist; exits 4 when any capability falls short of the R§1
/// contract, 0 when the SDK is fully conformant.
fn conform(specs: &[PathBuf], target: &str) -> ExitCode {
    let set = match gantry_spec::SpecSet::load(specs) {
        Ok(set) => set,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(exit_codes::SPEC_ERROR);
        }
    };
    let program = match gantry_spec::lower(&set) {
        Ok(lowering) => lowering.program,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(exit_codes::SPEC_ERROR);
        }
    };
    let analysis = match gantry_sema::analyze(&program) {
        Ok(analysis) => analysis,
        Err(errors) => {
            let engine_bug = errors.iter().any(gantry_sema::SemaError::is_engine_bug);
            for error in &errors {
                eprintln!("error: {error}");
            }
            return ExitCode::from(if engine_bug {
                exit_codes::ENGINE_BUG
            } else {
                exit_codes::SPEC_ERROR
            });
        }
    };

    // Generate the target's SDK and measure it through that target's shape,
    // so one contract checks every backend (VR-3).
    let (files, shape): (Vec<(String, String)>, gantry_verify::TargetShape) = match target {
        "go" => {
            let build = gantry_backend_go::BuildInfo::new(set.fingerprint());
            match gantry_backend_go::generate(&analysis, &build) {
                Ok(files) => (
                    files.into_iter().map(|f| (f.path, f.content)).collect(),
                    gantry_verify::go_shape(),
                ),
                Err(err) => {
                    eprintln!("error: {err}");
                    return ExitCode::from(exit_codes::ENGINE_BUG);
                }
            }
        }
        "apex" => {
            let build = gantry_backend_apex::BuildInfo::new(set.fingerprint());
            let files = gantry_backend_apex::generate(&analysis, &gantry_manifest::apex(), &build)
                .into_iter()
                .map(|f| (f.path, f.content))
                .collect();
            (files, gantry_verify::apex_shape())
        }
        "rust" => {
            let build = gantry_backend_rust::BuildInfo::new(set.fingerprint());
            let files = gantry_backend_rust::generate(&analysis, &gantry_manifest::rust(), &build)
                .into_iter()
                .map(|f| (f.path, f.content))
                .collect();
            (files, gantry_verify::rust_shape())
        }
        "typescript" => {
            let build = gantry_backend_typescript::BuildInfo::new(set.fingerprint());
            let files = gantry_backend_typescript::generate(
                &analysis,
                &gantry_manifest::typescript(),
                &build,
            )
            .into_iter()
            .map(|f| (f.path, f.content))
            .collect();
            (files, gantry_verify::typescript_shape())
        }
        other => unreachable!("clap restricts --target to known manifests, got {other:?}"),
    };

    let views: Vec<gantry_verify::GeneratedView> = files
        .iter()
        .map(|(path, content)| gantry_verify::GeneratedView { path, content })
        .collect();
    let report = gantry_verify::conformance(&shape, &analysis, &views);
    print!("{}", report.report());
    if report.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(exit_codes::VERIFICATION_FAILURE)
    }
}

/// Lower a spec set to a Program, mapping errors to their FR-8.3 class.
fn lower_program(specs: &[PathBuf]) -> Result<gantry_ir::Program, ExitCode> {
    let set = gantry_spec::SpecSet::load(specs).map_err(|err| {
        eprintln!("error: {err}");
        ExitCode::from(exit_codes::SPEC_ERROR)
    })?;
    let lowering = gantry_spec::lower(&set).map_err(|err| {
        eprintln!("error: {err}");
        ExitCode::from(exit_codes::SPEC_ERROR)
    })?;
    Ok(lowering.program)
}

/// FR-9: diff two spec sets. Prints the report; exits 4 when the change is
/// breaking (a major SDK bump) so CI can gate on it, 0 otherwise.
fn diff(from: &[PathBuf], to: &[PathBuf]) -> ExitCode {
    let (old, new) = match (lower_program(from), lower_program(to)) {
        (Ok(old), Ok(new)) => (old, new),
        (Err(code), _) | (_, Err(code)) => return code,
    };
    let report = gantry_verify::diff(&old, &new);
    print!("{}", report.report());
    match report.bump() {
        gantry_verify::VersionBump::Major => ExitCode::from(exit_codes::VERIFICATION_FAILURE),
        gantry_verify::VersionBump::Minor | gantry_verify::VersionBump::None => ExitCode::SUCCESS,
    }
}

/// Load → lower → analyze → generate; shared by `generate` and `verify`.
/// Errors are printed and mapped to their FR-8.3 exit class.
fn generate_files(
    specs: &[PathBuf],
    target: &str,
    overrides: &gantry_spec::NameOverrides,
) -> Result<Vec<gantry_backend_go::GeneratedFile>, ExitCode> {
    assert_eq!(target, "go", "clap restricts --target to known manifests");
    let set = gantry_spec::SpecSet::load(specs).map_err(|err| {
        eprintln!("error: {err}");
        ExitCode::from(exit_codes::SPEC_ERROR)
    })?;
    let lowering = gantry_spec::lower_with_overrides(&set, overrides).map_err(|err| {
        eprintln!("error: {err}");
        ExitCode::from(exit_codes::SPEC_ERROR)
    })?;
    let build = gantry_backend_go::BuildInfo::new(set.fingerprint());
    match gantry_sema::analyze(&lowering.program) {
        Ok(analysis) => gantry_backend_go::generate(&analysis, &build).map_err(|err| {
            eprintln!("error: {err}");
            ExitCode::from(exit_codes::ENGINE_BUG)
        }),
        Err(errors) => {
            let engine_bug = errors.iter().any(gantry_sema::SemaError::is_engine_bug);
            for error in &errors {
                eprintln!("error: {error}");
            }
            Err(ExitCode::from(if engine_bug {
                exit_codes::ENGINE_BUG
            } else {
                exit_codes::SPEC_ERROR
            }))
        }
    }
}

/// Every target `all` expands to, in output order.
const ALL_TARGETS: &[&str] = &["go", "apex", "rust", "typescript", "java"];

fn generate(
    specs: &[PathBuf],
    targets: &[String],
    out: &Path,
    overrides: Option<&Path>,
) -> ExitCode {
    let overrides = match load_overrides(overrides) {
        Ok(overrides) => overrides,
        Err(code) => return code,
    };
    let resolved = resolve_targets(targets);
    // A single target writes into `--out` directly (the common case). Two or
    // more each land in their own `<out>/<target>/` subdirectory so the fleet
    // never collides. The spec set is lowered once per target — cheap next to
    // codegen, and it keeps each backend's path independent.
    let multiple = resolved.len() > 1;
    for target in resolved {
        let dir = if multiple {
            out.join(target)
        } else {
            out.to_path_buf()
        };
        match generate_one(specs, target, &dir, &overrides) {
            ExitCode::SUCCESS => {}
            code => return code,
        }
    }
    ExitCode::SUCCESS
}

/// Expand `all`, then dedupe while preserving first-seen order, so
/// `--target go,apex`, `--target all`, and `--target apex,apex` all resolve to
/// a clean target list. `all` anywhere in the list wins the whole set.
fn resolve_targets(targets: &[String]) -> Vec<&'static str> {
    if targets.iter().any(|t| t == "all") {
        return ALL_TARGETS.to_vec();
    }
    let mut resolved: Vec<&'static str> = Vec::new();
    for target in targets {
        // Map to the ALL_TARGETS `'static` spelling so downstream paths and
        // the `generate_one` dispatch share one source of truth.
        if let Some(known) = ALL_TARGETS.iter().find(|t| **t == target.as_str())
            && !resolved.contains(known)
        {
            resolved.push(known);
        }
    }
    resolved
}

/// Generate one target into `out`. Both backends emit `(path, content)`;
/// dispatch on the target and write a common shape.
fn generate_one(
    specs: &[PathBuf],
    target: &str,
    out: &Path,
    overrides: &gantry_spec::NameOverrides,
) -> ExitCode {
    let files: Vec<(String, String)> = match target {
        "go" => match generate_files(specs, "go", overrides) {
            Ok(files) => files.into_iter().map(|f| (f.path, f.content)).collect(),
            Err(code) => return code,
        },
        "apex" => match generate_apex(specs, overrides) {
            Ok(files) => files,
            Err(code) => return code,
        },
        "rust" => match generate_rust(specs, overrides) {
            Ok(files) => files,
            Err(code) => return code,
        },
        "typescript" => match generate_typescript(specs, overrides) {
            Ok(files) => files,
            Err(code) => return code,
        },
        "java" => match generate_java(specs, overrides) {
            Ok(files) => files,
            Err(code) => return code,
        },
        other => unreachable!("clap restricts --target to known manifests, got {other:?}"),
    };
    if let Err(err) = write_pairs(out, &files) {
        eprintln!("error: cannot write output: {err}");
        return ExitCode::from(exit_codes::ENGINE_BUG);
    }
    println!(
        "ok  generated {count} {target} file(s) into {out}",
        count = files.len(),
        out = out.display()
    );
    ExitCode::SUCCESS
}

/// Load → lower → analyze → Apex-generate. The Apex backend consumes the
/// `apex()` manifest; no toolchain runs here (the scratch-org deploy loop
/// is the VR-1.3 gate).
fn generate_apex(
    specs: &[PathBuf],
    overrides: &gantry_spec::NameOverrides,
) -> Result<Vec<(String, String)>, ExitCode> {
    let set = gantry_spec::SpecSet::load(specs).map_err(|err| {
        eprintln!("error: {err}");
        ExitCode::from(exit_codes::SPEC_ERROR)
    })?;
    let build = gantry_backend_apex::BuildInfo::new(set.fingerprint());
    let lowering = gantry_spec::lower_with_overrides(&set, overrides).map_err(|err| {
        eprintln!("error: {err}");
        ExitCode::from(exit_codes::SPEC_ERROR)
    })?;
    match gantry_sema::analyze(&lowering.program) {
        Ok(analysis) => {
            Ok(
                gantry_backend_apex::generate(&analysis, &gantry_manifest::apex(), &build)
                    .into_iter()
                    .map(|f| (f.path, f.content))
                    .collect(),
            )
        }
        Err(errors) => {
            let engine_bug = errors.iter().any(gantry_sema::SemaError::is_engine_bug);
            for error in &errors {
                eprintln!("error: {error}");
            }
            Err(ExitCode::from(if engine_bug {
                exit_codes::ENGINE_BUG
            } else {
                exit_codes::SPEC_ERROR
            }))
        }
    }
}

/// Load → lower → analyze → Rust-generate. The Rust backend consumes the
/// `rust()` manifest; no toolchain runs here — `verify --target rust` runs the
/// VR-1.2 toolchain gate (rustfmt + `cargo check` + clippy + `cargo test`) on
/// this output.
fn generate_rust(
    specs: &[PathBuf],
    overrides: &gantry_spec::NameOverrides,
) -> Result<Vec<(String, String)>, ExitCode> {
    let set = gantry_spec::SpecSet::load(specs).map_err(|err| {
        eprintln!("error: {err}");
        ExitCode::from(exit_codes::SPEC_ERROR)
    })?;
    let build = gantry_backend_rust::BuildInfo::new(set.fingerprint());
    let lowering = gantry_spec::lower_with_overrides(&set, overrides).map_err(|err| {
        eprintln!("error: {err}");
        ExitCode::from(exit_codes::SPEC_ERROR)
    })?;
    match gantry_sema::analyze(&lowering.program) {
        Ok(analysis) => {
            Ok(
                gantry_backend_rust::generate(&analysis, &gantry_manifest::rust(), &build)
                    .into_iter()
                    .map(|f| (f.path, f.content))
                    .collect(),
            )
        }
        Err(errors) => {
            let engine_bug = errors.iter().any(gantry_sema::SemaError::is_engine_bug);
            for error in &errors {
                eprintln!("error: {error}");
            }
            Err(ExitCode::from(if engine_bug {
                exit_codes::ENGINE_BUG
            } else {
                exit_codes::SPEC_ERROR
            }))
        }
    }
}

/// Load → lower → analyze → TypeScript-generate. The TypeScript backend
/// consumes the `typescript()` manifest; no toolchain runs here — `verify
/// --target typescript` runs the VR-1.5 gate (`tsc --noEmit` under `strict`).
fn generate_typescript(
    specs: &[PathBuf],
    overrides: &gantry_spec::NameOverrides,
) -> Result<Vec<(String, String)>, ExitCode> {
    let set = gantry_spec::SpecSet::load(specs).map_err(|err| {
        eprintln!("error: {err}");
        ExitCode::from(exit_codes::SPEC_ERROR)
    })?;
    let build = gantry_backend_typescript::BuildInfo::new(set.fingerprint());
    let lowering = gantry_spec::lower_with_overrides(&set, overrides).map_err(|err| {
        eprintln!("error: {err}");
        ExitCode::from(exit_codes::SPEC_ERROR)
    })?;
    match gantry_sema::analyze(&lowering.program) {
        Ok(analysis) => Ok(gantry_backend_typescript::generate(
            &analysis,
            &gantry_manifest::typescript(),
            &build,
        )
        .into_iter()
        .map(|f| (f.path, f.content))
        .collect()),
        Err(errors) => {
            let engine_bug = errors.iter().any(gantry_sema::SemaError::is_engine_bug);
            for error in &errors {
                eprintln!("error: {error}");
            }
            Err(ExitCode::from(if engine_bug {
                exit_codes::ENGINE_BUG
            } else {
                exit_codes::SPEC_ERROR
            }))
        }
    }
}

/// Load → lower → analyze → Java-generate. The Java backend consumes the
/// `java()` manifest; no toolchain runs here — the VR-1.6 gate (`javac
/// -Xlint:all -Werror`) runs in the backend's `compile_output` test.
fn generate_java(
    specs: &[PathBuf],
    overrides: &gantry_spec::NameOverrides,
) -> Result<Vec<(String, String)>, ExitCode> {
    let set = gantry_spec::SpecSet::load(specs).map_err(|err| {
        eprintln!("error: {err}");
        ExitCode::from(exit_codes::SPEC_ERROR)
    })?;
    let build = gantry_backend_java::BuildInfo::new(set.fingerprint());
    let lowering = gantry_spec::lower_with_overrides(&set, overrides).map_err(|err| {
        eprintln!("error: {err}");
        ExitCode::from(exit_codes::SPEC_ERROR)
    })?;
    match gantry_sema::analyze(&lowering.program) {
        Ok(analysis) => {
            Ok(
                gantry_backend_java::generate(&analysis, &gantry_manifest::java(), &build)
                    .into_iter()
                    .map(|f| (f.path, f.content))
                    .collect(),
            )
        }
        Err(errors) => {
            let engine_bug = errors.iter().any(gantry_sema::SemaError::is_engine_bug);
            for error in &errors {
                eprintln!("error: {error}");
            }
            Err(ExitCode::from(if engine_bug {
                exit_codes::ENGINE_BUG
            } else {
                exit_codes::SPEC_ERROR
            }))
        }
    }
}

/// Names the files the last generation wrote, so the next one can prune them.
const MANIFEST: &str = ".gantry-manifest";

fn write_pairs(root: &Path, files: &[(String, String)]) -> std::io::Result<()> {
    // Prune before writing. Regenerating into a populated directory otherwise
    // leaves files the new run no longer emits, and a stale artifact is not
    // inert: renaming Rust's `src/runtime.rs` to `src/runtime/mod.rs` left both
    // behind, which is an ambiguous-module error (E0761) — the tree stopped
    // compiling with no indication why.
    //
    // Only paths this tool recorded writing are removed, so a `.git` directory,
    // a hand-added `.gitignore`, or anything else in the output directory is
    // never touched.
    let emitted: std::collections::BTreeSet<&str> =
        files.iter().map(|(path, _)| path.as_str()).collect();

    // Only a *missing* manifest is benign — that is the first generation into
    // this directory. Any other read failure, and any deletion failure, is
    // reported rather than swallowed: the manifest is rewritten below, so a
    // silently-skipped prune would drop the stale path from the record and
    // leave the file orphaned forever, with nothing left pointing at it.
    let previous = match std::fs::read_to_string(root.join(MANIFEST)) {
        Ok(previous) => previous,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };
    for stale in previous
        .lines()
        .filter(|path| !path.is_empty() && !emitted.contains(path))
    {
        match std::fs::remove_file(root.join(stale)) {
            // Already gone (deleted by hand) — the desired end state.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            other => other?,
        }
    }

    for (path, content) in files {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
    }

    let manifest: Vec<&str> = emitted.into_iter().collect();
    std::fs::write(root.join(MANIFEST), manifest.join("\n") + "\n")?;
    Ok(())
}

/// Generate one target's SDK as `(path, content)` pairs — the common currency
/// `verify` and `conform` measure, so the backend file types stay internal.
fn generate_pairs(specs: &[PathBuf], target: &str) -> Result<Vec<(String, String)>, ExitCode> {
    // `verify` checks a target's real-toolchain compile against the plain,
    // un-overridden output — overriding a name doesn't change whether the
    // generated code compiles, so there is nothing an `--overrides` flag
    // here would verify differently.
    let overrides = gantry_spec::NameOverrides::empty();
    match target {
        "go" => generate_files(specs, "go", &overrides)
            .map(|files| files.into_iter().map(|f| (f.path, f.content)).collect()),
        "apex" => generate_apex(specs, &overrides),
        "rust" => generate_rust(specs, &overrides),
        "typescript" => generate_typescript(specs, &overrides),
        other => unreachable!("clap restricts --target to known manifests, got {other:?}"),
    }
}

fn verify(specs: &[PathBuf], target: &str) -> ExitCode {
    let files = match generate_pairs(specs, target) {
        Ok(files) => files,
        Err(code) => return code,
    };
    let dir = std::env::temp_dir().join(format!("gantry-verify-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(err) = write_pairs(&dir, &files) {
        eprintln!("error: cannot write output: {err}");
        return ExitCode::from(exit_codes::ENGINE_BUG);
    }

    // The compile loop: the target's real toolchain is the oracle (Go → VR-1.1;
    // Rust → VR-1.2 = rustfmt-clean + cargo check + clippy-clean + the generated
    // round-trip tests passing under `cargo test`).
    let steps: &[(&str, &str, &[&str])] = match target {
        "go" => &[
            ("go build", "go", &["build", "./..."]),
            ("go vet", "go", &["vet", "./..."]),
        ],
        "rust" => &[
            ("cargo fmt --check", "cargo", &["fmt", "--check"]),
            ("cargo check", "cargo", &["check"]),
            (
                "clippy -D warnings",
                "cargo",
                &["clippy", "--all-targets", "--", "-D", "warnings"],
            ),
            // Compile *and run* the generated round-trip / behavioral tests
            // (FR-7.8, VR-4): serialization tri-state + typed date/time, and
            // per-union known/unknown discriminator dispatch.
            ("cargo test", "cargo", &["test"]),
        ],
        // TypeScript → VR-1.5: the TypeScript 7 native compiler type-checks the
        // generated package under `strict`, the TS analogue of `go build`.
        "typescript" => &[("tsc --noEmit", "tsc", &["--noEmit", "-p", "tsconfig.json"])],
        other => unreachable!("clap restricts --target to known manifests, got {other:?}"),
    };
    for (label, program, args) in steps {
        let output = match std::process::Command::new(program)
            .args(*args)
            .current_dir(&dir)
            .output()
        {
            Ok(output) => output,
            Err(err) => {
                eprintln!("error: cannot run {label}: {err}");
                return ExitCode::from(exit_codes::VERIFICATION_FAILURE);
            }
        };
        if !output.status.success() {
            eprintln!(
                "error: {label} failed on the generated output:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return ExitCode::from(exit_codes::VERIFICATION_FAILURE);
        }
        println!("ok  {label} clean");
    }
    // Go's `gofmt -l` reports unformatted files by name (exit 0 + non-empty
    // stdout), unlike Rust's `cargo fmt --check` (exit code), so check it here.
    if target == "go" {
        match std::process::Command::new("gofmt")
            .arg("-l")
            .arg(&dir)
            .output()
        {
            Ok(output) if output.status.success() && output.stdout.is_empty() => {
                println!("ok  gofmt clean");
            }
            Ok(output) => {
                eprintln!(
                    "error: gofmt wants changes (G-17) in:\n{}",
                    String::from_utf8_lossy(&output.stdout)
                );
                return ExitCode::from(exit_codes::VERIFICATION_FAILURE);
            }
            Err(err) => {
                eprintln!("error: cannot run gofmt: {err}");
                return ExitCode::from(exit_codes::VERIFICATION_FAILURE);
            }
        }
    }
    println!(
        "ok  verified: {count} generated file(s) compile clean",
        count = files.len()
    );
    ExitCode::SUCCESS
}

fn check(specs: &[PathBuf]) -> ExitCode {
    let set = match gantry_spec::SpecSet::load(specs) {
        Ok(set) => set,
        Err(err) => {
            eprintln!("error: {err}");
            let mut source = std::error::Error::source(&err);
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
            return ExitCode::from(exit_codes::SPEC_ERROR);
        }
    };

    let mut total_ops = 0;
    let mut total_schemas = 0;
    for doc in &set.documents {
        total_ops += doc.operations.len();
        total_schemas += doc.schemas.len();
        let deprecated = doc.operations.iter().filter(|op| op.deprecated).count();
        let unstable = doc
            .operations
            .iter()
            .filter(|op| op.stability_level.is_some())
            .count();
        println!(
            "ok  {file}  API {version}: {ops} operations ({deprecated} deprecated, {unstable} pre-stable), \
             {managers} managers, {schemas} schemas",
            file = doc.file.display(),
            version = doc.api_version,
            ops = doc.operations.len(),
            managers = doc.managers().len(),
            schemas = doc.schemas.len(),
        );
    }

    let lowering = match gantry_spec::lower(&set) {
        Ok(lowering) => lowering,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(exit_codes::SPEC_ERROR);
        }
    };
    let stats = &lowering.stats;
    println!(
        "ok  IR: {decls} declarations — {structs} structs, {unions} unions ({disc} discriminated), \
         {enums} enums, {aliases} aliases; {synth} synthesized, {holes} free-form JSON sites",
        decls = lowering.program.decls.len(),
        structs = stats.structs,
        unions = stats.unions,
        disc = stats.discriminated_unions,
        enums = stats.enums,
        aliases = stats.aliases,
        synth = stats.synthesized,
        holes = stats.json_value_sites,
    );
    println!(
        "ok  IR: {ops} operations — {json} JSON, {empty} body-less, {binary} binary, \
         {redirect} redirect, {text} text",
        ops = stats.operations,
        json = stats.operations
            - stats.empty_responses
            - stats.binary_responses
            - stats.redirect_responses
            - stats.text_responses,
        empty = stats.empty_responses,
        binary = stats.binary_responses,
        redirect = stats.redirect_responses,
        text = stats.text_responses,
    );

    // The semantic pass (FR-3): backends only ever see verified programs.
    let analysis = match gantry_sema::analyze(&lowering.program) {
        Ok(analysis) => analysis,
        Err(errors) => {
            let engine_bug = errors.iter().any(gantry_sema::SemaError::is_engine_bug);
            for error in &errors {
                eprintln!("error: {error}");
            }
            eprintln!("error: semantic analysis found {} problem(s)", errors.len());
            return ExitCode::from(if engine_bug {
                exit_codes::ENGINE_BUG
            } else {
                exit_codes::SPEC_ERROR
            });
        }
    };
    println!(
        "ok  sema: verified — {managers} managers, every reference bound, every type well-formed",
        managers = analysis.managers.len(),
    );
    println!(
        "ok  spec set: {docs} document(s), {total_ops} operations, {total_schemas} schemas",
        docs = set.documents.len(),
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use std::process::ExitCode;

    use super::{ALL_TARGETS, exit_codes, load_overrides, resolve_targets};

    fn resolve(args: &[&str]) -> Vec<&'static str> {
        resolve_targets(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn a_single_target_resolves_to_itself() {
        assert_eq!(resolve(&["apex"]), vec!["apex"]);
    }

    #[test]
    fn a_subset_keeps_first_seen_order() {
        assert_eq!(resolve(&["apex", "go"]), vec!["apex", "go"]);
    }

    #[test]
    fn all_expands_to_every_target() {
        assert_eq!(resolve(&["all"]), ALL_TARGETS.to_vec());
        // `all` anywhere wins the whole set, in canonical order.
        assert_eq!(resolve(&["apex", "all"]), ALL_TARGETS.to_vec());
    }

    #[test]
    fn duplicates_are_removed() {
        assert_eq!(resolve(&["apex", "apex"]), vec!["apex"]);
        assert_eq!(resolve(&["go", "apex", "go"]), vec!["go", "apex"]);
    }

    #[test]
    fn no_overrides_flag_behaves_like_empty_overrides() {
        assert_eq!(
            load_overrides(None).unwrap(),
            gantry_spec::NameOverrides::empty()
        );
    }

    #[test]
    fn a_missing_overrides_file_is_a_spec_error() {
        let missing = std::path::Path::new("/no/such/gantry-overrides-file.json");
        let err = load_overrides(Some(missing)).unwrap_err();
        assert_eq!(err, ExitCode::from(exit_codes::SPEC_ERROR));
    }
}
