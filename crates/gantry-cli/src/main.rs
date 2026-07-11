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
    /// Generate an SDK from spec documents (models slice today).
    Generate {
        #[arg(required = true, value_name = "SPEC")]
        specs: Vec<PathBuf>,
        /// Target language (manifest key).
        #[arg(long, value_parser = ["go"])]
        target: String,
        /// Output directory (created if missing).
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
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Check { specs } => check(&specs),
        Command::Generate { specs, target, out } => generate(&specs, &target, &out),
        Command::Verify { specs, target } => verify(&specs, &target),
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
    match gantry_sema::analyze(&lowering.program) {
        Ok(analysis) => Ok(gantry_backend_go::generate_models(&analysis)),
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

fn generate(specs: &[PathBuf], target: &str, out: &Path) -> ExitCode {
    let files = match generate_files(specs, target) {
        Ok(files) => files,
        Err(code) => return code,
    };
    if let Err(err) = write_files(out, &files) {
        eprintln!("error: cannot write output: {err}");
        return ExitCode::from(exit_codes::ENGINE_BUG);
    }
    println!(
        "ok  generated {count} file(s) into {out}",
        count = files.len(),
        out = out.display()
    );
    ExitCode::SUCCESS
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
