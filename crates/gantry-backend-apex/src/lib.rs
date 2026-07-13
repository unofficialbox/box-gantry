//! The Apex backend: lowering + printer (FR-6, TR-Apex).
//!
//! Generates the Salesforce Apex SDK from a verified program. Apex is the
//! stress-test target (assessment §8): one **flat namespace** (modules
//! become outer-class grouping + deterministic name mangling, TR-Apex.1),
//! **no user-defined generics** (shared containers lower to per-type code,
//! TR-Apex.2), **exceptions** for errors, **buffered** bodies bounded by
//! the platform heap, and a **75% test-coverage** deploy gate. The backend
//! consumes only the [`gantry_manifest`] capability axes — never the
//! language name (FR-4.2).
//!
//! This first slice lowers the **model layer**: every schema declaration
//! becomes a top-level Apex class. Managers, the client, serialization, and
//! the Apex runtime land in later slices (see PLAN.md, M4). Because no Apex
//! toolchain runs here, the per-commit signal is structural + determinism
//! tests; the scratch-org `sf project deploy validate` loop (VR-1.3) is the
//! CI/merge gate once a Dev Hub is configured.

mod docs;
mod managers;
mod models;
mod runtime;

pub use managers::generate_managers;
pub use models::generate_models;

use gantry_manifest::CapabilityManifest;
use gantry_sema::Analysis;

/// One generated file, path relative to the SDK root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedFile {
    pub path: String,
    pub content: String,
}

/// SFDX source-format class directory. The generated tree is a deployable
/// SFDX project so the scratch-org loop (VR-1.3) can `sf project deploy
/// validate` it directly.
pub(crate) const CLASSES_DIR: &str = "force-app/main/default/classes";

/// The Salesforce API version the generated metadata targets.
pub(crate) const APEX_API_VERSION: &str = "62.0";

/// Generate the complete Apex SDK as a deployable SFDX project: the project
/// scaffolding (`sfdx-project.json`, `config/project-scratch-def.json`,
/// `.forceignore`, `manifest/package.xml`, `README.md`), every model and
/// manager class with a `-meta.xml` sidecar (source format), and per-endpoint
/// Markdown reference docs under `docs/`. Deterministic (FR-6.2).
pub fn generate(analysis: &Analysis<'_>, manifest: &CapabilityManifest) -> Vec<GeneratedFile> {
    let mut files = project_scaffolding();

    let mut classes = generate_models(analysis, manifest);
    classes.extend(generate_managers(analysis, manifest));
    // The hand-written runtime deploys alongside the generated classes (Apex
    // is one flat namespace), behind the generated `BoxClient` contract.
    classes.extend(runtime::runtime_classes());
    let class_meta = class_meta_xml();
    for class in classes {
        files.push(GeneratedFile {
            path: format!("{}-meta.xml", class.path),
            content: class_meta.clone(),
        });
        files.push(class);
    }

    // Per-endpoint reference docs (not under a package directory, so never
    // deployed — the `.forceignore` also excludes them defensively).
    files.extend(docs::generate_docs(analysis, manifest));

    // Deterministic: sort by path so the tree order never depends on
    // insertion order.
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// The standard SFDX project scaffolding — everything a developer expects
/// when they clone the SDK and run `sf project deploy start` (TR-Apex.5).
fn project_scaffolding() -> Vec<GeneratedFile> {
    let file = |path: &str, content: String| GeneratedFile {
        path: path.to_string(),
        content,
    };
    vec![
        file("sfdx-project.json", sfdx_project_json()),
        file("config/project-scratch-def.json", scratch_def_json()),
        file(".forceignore", forceignore()),
        file("manifest/package.xml", package_xml()),
        file("README.md", project_readme()),
    ]
}

fn sfdx_project_json() -> String {
    format!(
        "{{\n  \"packageDirectories\": [{{ \"path\": \"force-app\", \"default\": true }}],\n  \"name\": \"box-gantry-apex\",\n  \"namespace\": \"\",\n  \"sfdcLoginUrl\": \"https://login.salesforce.com\",\n  \"sourceApiVersion\": \"{APEX_API_VERSION}\"\n}}\n"
    )
}

/// A minimal Developer-edition scratch-org definition — the same one the
/// VR-1.3 compile loop deploys against, now shipped with the project so a
/// developer can `sf org create scratch -f config/project-scratch-def.json`.
fn scratch_def_json() -> String {
    "{\n  \"orgName\": \"box-gantry-apex\",\n  \"edition\": \"Developer\"\n}\n".to_string()
}

/// Keep the docs and repository chrome out of source deploys/retrieves.
fn forceignore() -> String {
    "# Not Salesforce metadata — never deploy or retrieve these.\ndocs/**\nREADME.md\n**/*.dup\n**/.DS_Store\n".to_string()
}

/// A wildcard manifest so `sf project deploy start -x manifest/package.xml`
/// deploys every generated Apex class.
fn package_xml() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Package xmlns=\"http://soap.sforce.com/2006/04/metadata\">\n    <types>\n        <members>*</members>\n        <name>ApexClass</name>\n    </types>\n    <version>{APEX_API_VERSION}</version>\n</Package>\n"
    )
}

fn project_readme() -> String {
    format!(
        "<!-- Generated by box-gantry {version}. DO NOT EDIT. -->\n\
         # Box SDK for Salesforce Apex\n\n\
         A generated, deploy-ready SFDX project. Every class lives in\n\
         `force-app/main/default/classes/`; per-endpoint reference docs are in\n\
         [`docs/`](docs/README.md).\n\n\
         ## Layout\n\n\
         | Path | What |\n|---|---|\n\
         | `force-app/main/default/classes/` | model, manager, and client classes (`.cls` + `-meta.xml`) |\n\
         | `docs/` | one Markdown page per endpoint, with runnable snippets |\n\
         | `config/project-scratch-def.json` | scratch-org definition |\n\
         | `manifest/package.xml` | wildcard deploy manifest |\n\n\
         ## Deploy\n\n\
         ```bash\n\
         sf org create scratch -f config/project-scratch-def.json -a box-sdk\n\
         sf project deploy start -x manifest/package.xml -o box-sdk\n\
         ```\n\n\
         ## Use\n\n\
         The `Box` class is the single entry point — one field per resource\n\
         manager. Construct it with a `BoxClient` (the hand-written runtime\n\
         that performs auth + HTTP callouts):\n\n\
         ```apex\n\
         Box client = new Box(myBoxClient);\n\
         FileFull f = client.files.getById(fileId, null, null, null, null);\n\
         ```\n\n\
         Apex needs no import statements — every generated class is visible in\n\
         the namespace. See [`docs/`](docs/README.md) for each endpoint.\n",
        version = env!("CARGO_PKG_VERSION")
    )
}

fn class_meta_xml() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ApexClass xmlns=\"http://soap.sforce.com/2006/04/metadata\">\n    <apiVersion>{APEX_API_VERSION}</apiVersion>\n    <status>Active</status>\n</ApexClass>\n"
    )
}

/// FNV-1a over raw bytes — a fast, dependency-free, deterministic hash used
/// only to disambiguate identifiers abbreviated to the platform limit
/// (TR-Apex.1). Not security-sensitive.
pub(crate) fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(OFFSET, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(PRIME)
    })
}

/// Apex reserved words (case-insensitive) that appear as Box field/param
/// names. The IR's wire-name / identifier split (FR-2) means mangling the
/// Apex identifier never touches serialization — the JSON key is unchanged.
pub(crate) const RESERVED: &[&str] = &[
    "abstract",
    "and",
    "as",
    "asc",
    "blob",
    "boolean",
    "break",
    "by",
    "case",
    "catch",
    "class",
    "commit",
    "continue",
    "currency",
    "date",
    "datetime",
    "decimal",
    "default",
    "delete",
    "desc",
    "do",
    "double",
    "else",
    "end",
    "enum",
    "extends",
    "false",
    "final",
    "finally",
    "float",
    "for",
    "from",
    "global",
    "group",
    "if",
    "implements",
    "insert",
    "instanceof",
    "integer",
    "interface",
    "limit",
    "list",
    "long",
    "map",
    "merge",
    "new",
    "not",
    "null",
    "object",
    "on",
    "or",
    "override",
    "private",
    "protected",
    "public",
    "return",
    "select",
    "set",
    "sort",
    "static",
    "string",
    "super",
    "switch",
    "system",
    "testmethod",
    "then",
    "this",
    "time",
    "transient",
    "trigger",
    "true",
    "try",
    "undelete",
    "update",
    "upsert",
    "value",
    "virtual",
    "void",
    "webservice",
    "when",
    "while",
    "with",
    "without",
];

/// Return an Apex-safe identifier for a wire/param/field name. Apex
/// identifiers must begin with a letter and contain only alphanumerics and
/// single, non-trailing underscores — Box wire names break every one of
/// these rules (`Box__Security__Classification__Key` has runs of `__`; some
/// keys start with a digit), and a reserved word like `limit`/`group` is
/// rejected outright. So: fold every non-alphanumeric to `_`, collapse runs,
/// drop leading/trailing `_`, ensure a letter leads, then give reserved
/// words a `_r` suffix (a bare trailing `_` is itself invalid). The wire
/// name is unaffected — it travels via the serializer, not the field name.
pub(crate) fn safe_word(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('_') {
            // fold any other char to `_`, but never emit a run of them
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    let mut ident = if trimmed
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
    {
        trimmed.to_string()
    } else {
        // leading digit (or empty) — Apex identifiers must start with a
        // letter; `x` prefix keeps it deterministic.
        format!("x{trimmed}")
    };
    if RESERVED.contains(&ident.to_ascii_lowercase().as_str()) {
        ident.push_str("_r");
    }
    ident
}
