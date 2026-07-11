//! `gantry` — the one CLI (FR-8).
//!
//! A thin argument parser over the engine library: no naming rules, no
//! spec shaping, no business logic lives here (FR-8.2 — the `PostFolders`
//! lesson). Subcommands appear as the engine grows them; only `check`
//! exists today.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Exit codes (FR-8.3). Distinct classes so CI and callers can tell input
/// problems from engine problems. Clap itself exits 2 on usage errors.
mod exit_codes {
    /// The input specs are at fault; fix the spec (or the file list).
    pub const SPEC_ERROR: u8 = 3;
    /// Reserved: generated output failed verification (`verify`).
    #[expect(
        dead_code,
        reason = "taken into use when `gantry verify` lands (FR-8.1)"
    )]
    pub const VERIFICATION_FAILURE: u8 = 4;
    /// Reserved: an internal invariant broke; file a box-gantry bug.
    #[expect(
        dead_code,
        reason = "taken into use when the engine has invariants to break"
    )]
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
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Check { specs } => check(&specs),
    }
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
    println!(
        "ok  spec set: {docs} document(s), {total_ops} operations, {total_schemas} schemas",
        docs = set.documents.len(),
    );
    ExitCode::SUCCESS
}
