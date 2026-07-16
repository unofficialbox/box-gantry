//! `gantry` — the one CLI (FR-8).
//!
//! A thin argument parser over the engine library: no naming rules, no
//! spec shaping, no business logic lives here (FR-8.2 — the `PostFolders`
//! lesson). Subcommands appear as the engine grows them; only `check`
//! exists today.

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
        /// Target language(s) (manifest key). `go` is complete; `apex` emits a
        /// deployable SFDX project; `rust` emits the models + async managers/
        /// client (M5, in progress). Accepts a comma-separated list or repeated
        /// flags to build several at once (`--target go,rust`); `all` expands to
        /// every target. A single target writes into `--out` directly; two or
        /// more each land in their own `<out>/<target>/` subdirectory.
        #[arg(long, required = true, value_parser = ["go", "apex", "rust", "all"], value_delimiter = ',')]
        target: Vec<String>,
        /// Output directory (created if missing). With more than one target,
        /// each SDK lands in a `<out>/<target>/` subdirectory.
        #[arg(long)]
        out: PathBuf,
    },
    /// Generate, then compile the output with the target's real toolchain
    /// (VR-1.1). Exits 4 when the generated code fails verification.
    Verify {
        #[arg(required = true, value_name = "SPEC")]
        specs: Vec<PathBuf>,
        /// Target language (manifest key).
        #[arg(long, value_parser = ["go"])]
        target: String,
    },
    /// Report the R§1 capability conformance checklist for a target
    /// (VR-3): managers, operations, paginated surfaces, auth flows, tests,
    /// and docs — expected (from the spec) vs generated. Exits 4 when any
    /// capability falls short, so CI can gate a partial SDK.
    Conform {
        #[arg(required = true, value_name = "SPEC")]
        specs: Vec<PathBuf>,
        /// Target language (manifest key). `go` and `apex` are both measured
        /// against the same R§1 capability contract.
        #[arg(long, value_parser = ["go", "apex"])]
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
        Command::Generate { specs, target, out } => generate(&specs, &target, &out),
        Command::Verify { specs, target } => verify(&specs, &target),
        Command::Conform { specs, target } => conform(&specs, &target),
        Command::Diff { from, to } => diff(&from, &to),
    }
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
) -> Result<Vec<gantry_backend_go::GeneratedFile>, ExitCode> {
    assert_eq!(target, "go", "clap restricts --target to known manifests");
    let set = gantry_spec::SpecSet::load(specs).map_err(|err| {
        eprintln!("error: {err}");
        ExitCode::from(exit_codes::SPEC_ERROR)
    })?;
    let lowering = gantry_spec::lower(&set).map_err(|err| {
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

fn write_files(root: &Path, files: &[gantry_backend_go::GeneratedFile]) -> std::io::Result<()> {
    for file in files {
        let path = root.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, &file.content)?;
    }
    Ok(())
}

/// Every target `all` expands to, in output order.
const ALL_TARGETS: &[&str] = &["go", "apex", "rust"];

fn generate(specs: &[PathBuf], targets: &[String], out: &Path) -> ExitCode {
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
        match generate_one(specs, target, &dir) {
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
fn generate_one(specs: &[PathBuf], target: &str, out: &Path) -> ExitCode {
    let files: Vec<(String, String)> = match target {
        "go" => match generate_files(specs, "go") {
            Ok(files) => files.into_iter().map(|f| (f.path, f.content)).collect(),
            Err(code) => return code,
        },
        "apex" => match generate_apex(specs) {
            Ok(files) => files,
            Err(code) => return code,
        },
        "rust" => match generate_rust(specs) {
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
fn generate_apex(specs: &[PathBuf]) -> Result<Vec<(String, String)>, ExitCode> {
    let set = gantry_spec::SpecSet::load(specs).map_err(|err| {
        eprintln!("error: {err}");
        ExitCode::from(exit_codes::SPEC_ERROR)
    })?;
    let build = gantry_backend_apex::BuildInfo::new(set.fingerprint());
    let lowering = gantry_spec::lower(&set).map_err(|err| {
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
/// `rust()` manifest; no toolchain runs here. VR-1.2 is currently exercised by
/// the backend's generated-output toolchain test (`verify --target rust` is a
/// later slice).
fn generate_rust(specs: &[PathBuf]) -> Result<Vec<(String, String)>, ExitCode> {
    let set = gantry_spec::SpecSet::load(specs).map_err(|err| {
        eprintln!("error: {err}");
        ExitCode::from(exit_codes::SPEC_ERROR)
    })?;
    let build = gantry_backend_rust::BuildInfo::new(set.fingerprint());
    let lowering = gantry_spec::lower(&set).map_err(|err| {
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

fn write_pairs(root: &Path, files: &[(String, String)]) -> std::io::Result<()> {
    for (path, content) in files {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
    }
    Ok(())
}

fn verify(specs: &[PathBuf], target: &str) -> ExitCode {
    let files = match generate_files(specs, target) {
        Ok(files) => files,
        Err(code) => return code,
    };
    let dir = std::env::temp_dir().join(format!("gantry-verify-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(err) = write_files(&dir, &files) {
        eprintln!("error: cannot write output: {err}");
        return ExitCode::from(exit_codes::ENGINE_BUG);
    }

    // The VR-1.1 loop: the target's real toolchain is the oracle.
    for (label, program, args) in [
        ("go build", "go", vec!["build", "./..."]),
        ("go vet", "go", vec!["vet", "./..."]),
    ] {
        let output = match std::process::Command::new(program)
            .args(&args)
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
                "error: {label} failed on the generated output:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return ExitCode::from(exit_codes::VERIFICATION_FAILURE);
        }
        println!("ok  {label} clean");
    }
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
    use super::{ALL_TARGETS, resolve_targets};

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
}
