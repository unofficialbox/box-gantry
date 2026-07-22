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
//! the Apex runtime land in later slices. Because no Apex
//! toolchain runs here, the per-commit signal is structural + determinism
//! tests; the scratch-org `sf project deploy validate` loop (VR-1.3) is the
//! CI/merge gate once a Dev Hub is configured.

mod docs;
mod managers;
mod models;
mod runtime;
mod tests;
mod wire;

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

/// Build provenance stamped into the generated SDK (NF-7): the engine
/// version and the fingerprint of the input spec set. The Apex analogue of
/// Go's `BuildInfo` — every release is traceable to its exact inputs.
#[derive(Debug, Clone)]
pub struct BuildInfo {
    /// The box-gantry engine version (workspace version).
    pub engine: String,
    /// The input spec-set fingerprint (`SpecSet::fingerprint`).
    pub spec_fingerprint: String,
}

impl BuildInfo {
    /// Build info for the current engine over a given spec fingerprint.
    pub fn new(spec_fingerprint: impl Into<String>) -> Self {
        Self {
            engine: env!("CARGO_PKG_VERSION").to_string(),
            spec_fingerprint: spec_fingerprint.into(),
        }
    }
}

/// SFDX source-format class directory. The generated tree is a deployable
/// SFDX project so the scratch-org loop (VR-1.3) can `sf project deploy
/// validate` it directly.
pub(crate) const CLASSES_DIR: &str = "force-app/main/default/classes";

/// The Salesforce API version the generated metadata targets.
pub(crate) const APEX_API_VERSION: &str = "62.0";

/// The registered package namespace the shipped SDK deploys under (NF-8,
/// D-142). Set on the generated SFDX project so scratch orgs and the 2GP
/// package build carry it; a plain source deploy to a non-namespace org
/// ignores it. Apex class names are unaffected — the namespace is a separate
/// prefix (`unbox.ClassName`), so the 40-char identifier budget is untouched.
pub(crate) const APEX_NAMESPACE: &str = "unbox";

/// The 2GP **unlocked** package the SDK ships as (NF-8, D-142). Unlocked (not
/// managed) so the spec-regenerated surface can add, remove, and rename
/// members across versions without a managed package's global-member lock.
pub(crate) const APEX_PACKAGE_NAME: &str = "Unbox Salesforce SDK";

/// Generate the complete Apex SDK as a deployable SFDX project: the project
/// scaffolding (`sfdx-project.json`, `config/project-scratch-def.json`,
/// `.forceignore`, `manifest/package.xml`, `README.md`), every model and
/// manager class with a `-meta.xml` sidecar (source format), and per-endpoint
/// Markdown reference docs under `docs/`. Deterministic (FR-6.2).
pub fn generate(
    analysis: &Analysis<'_>,
    manifest: &CapabilityManifest,
    build: &BuildInfo,
) -> Vec<GeneratedFile> {
    let mut files = project_scaffolding(build);

    let mut classes = generate_models(analysis, manifest);
    classes.extend(generate_managers(analysis, manifest));
    // The hand-written runtime deploys alongside the generated classes (Apex
    // is one flat namespace), behind the generated `BoxClient` contract.
    classes.extend(runtime::runtime_classes());
    // Generated `@isTest` classes exercise the code above so a production
    // deploy clears the platform's mandated coverage gate (TR-Apex).
    classes.extend(tests::generate_tests(analysis, manifest));
    // Build provenance as an on-platform constant class (NF-7): a deployed
    // org can report which engine + spec set produced its SDK.
    classes.push(build_info_class(build));
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
fn project_scaffolding(build: &BuildInfo) -> Vec<GeneratedFile> {
    let file = |path: &str, content: String| GeneratedFile {
        path: path.to_string(),
        content,
    };
    let mut files = vec![
        file("sfdx-project.json", sfdx_project_json()),
        file("config/project-scratch-def.json", scratch_def_json()),
        file(".forceignore", forceignore()),
        file("manifest/package.xml", package_xml()),
        file("README.md", project_readme(build)),
        // The shared community-design banner the README renders at its top (NF-8).
        // Repo-root asset, outside `force-app`, so it never deploys as metadata.
        file("assets/banner.svg", gantry_manifest::banner_svg("Apex")),
    ];
    files.extend(remote_site_settings());
    files
}

/// The `BoxBuildInfo` class: on-platform constants recording the engine
/// version and the spec fingerprint this SDK was generated from (NF-7), so a
/// deployed org can report its own provenance (`BoxBuildInfo.SPEC_FINGERPRINT`).
/// A pure-constant class — no executable statements, so it never affects the
/// 75% coverage gate.
fn build_info_class(build: &BuildInfo) -> GeneratedFile {
    let engine = escape_apex_string(&build.engine);
    let fingerprint = escape_apex_string(&build.spec_fingerprint);
    GeneratedFile {
        path: format!("{CLASSES_DIR}/BoxBuildInfo.cls"),
        content: format!(
            "// Code generated by box-gantry {engine}. DO NOT EDIT.\n\
             \n/**\n \
             * Provenance of this generated SDK (NF-7): the box-gantry engine\n \
             * version and the fingerprint of the Box OpenAPI spec set it was\n \
             * generated from. Lets a deployed org report exactly which inputs\n \
             * produced its SDK.\n \
             */\n\
             public class BoxBuildInfo {{\n    \
             /** The box-gantry engine version that produced this SDK. */\n    \
             public static final String ENGINE_VERSION = '{engine}';\n\n    \
             /** Fingerprint of the exact input spec set. */\n    \
             public static final String SPEC_FINGERPRINT = '{fingerprint}';\n\
             }}\n"
        ),
    }
}

/// Escape a value for an Apex single-quoted string literal.
fn escape_apex_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

/// The Box hosts the runtime calls out to (the D-106 base-URL classes in
/// `BoxHttpClient`, collapsed to distinct origins). Apex blocks any callout to a
/// host without a Remote Site Setting, so a deploy without these can't make a
/// single request. The runtime sets its own `Authorization: Bearer` header and
/// builds absolute URLs, so a Remote Site Setting (not a Named Credential) is
/// the right fit.
const BOX_REMOTE_SITES: &[(&str, &str)] = &[
    ("Box_api_box_com", "https://api.box.com"),
    ("Box_upload_box_com", "https://upload.box.com"),
    ("Box_account_box_com", "https://account.box.com"),
    ("Box_dl_boxcloud_com", "https://dl.boxcloud.com"),
];

/// Ship a Remote Site Setting per Box host so the generated SDK can actually
/// make callouts once deployed (otherwise every request throws "Unauthorized
/// endpoint"). Deterministic; deployed by both source deploys and the wildcard
/// `package.xml`. The source-format file suffix is `.remoteSite-meta.xml` — the
/// SDR registry's canonical suffix for the `RemoteSiteSetting` type. (The
/// MDAPI-style `.remoteSiteSetting` suffix is tolerated by `sf project deploy
/// start` but rejected by `sf package version create`'s strict source→MDAPI
/// conversion, D-144.)
fn remote_site_settings() -> Vec<GeneratedFile> {
    BOX_REMOTE_SITES
        .iter()
        .map(|(name, url)| GeneratedFile {
            path: format!("force-app/main/default/remoteSiteSettings/{name}.remoteSite-meta.xml"),
            content: format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <RemoteSiteSetting xmlns=\"http://soap.sforce.com/2006/04/metadata\">\n    \
                 <disableProtocolSecurity>false</disableProtocolSecurity>\n    \
                 <isActive>true</isActive>\n    \
                 <url>{url}</url>\n\
                 </RemoteSiteSetting>\n"
            ),
        })
        .collect()
}

/// The SFDX project descriptor. Carries the `unbox` namespace and the
/// unlocked-2GP package definition (NF-8, D-142) so `sf package version
/// create` builds the ship artifact. `packageAliases` is emitted empty: this
/// file is regenerated on every build, so it never persists the alias `sf
/// package create` writes — the durable handle is the `0Ho…` package id, which
/// the release build passes to `sf package version create --package 0Ho…` (see
/// the README's Packaging section). The `.NEXT` build segment auto-increments
/// per version; the major.minor is set from the FR-9 spec-diff at release.
fn sfdx_project_json() -> String {
    // `versionNumber` is `major.minor.patch.build`; `.NEXT` auto-increments the
    // build segment per release. Both fields derive from the one SDK version.
    let version = gantry_manifest::SDK_VERSION;
    format!(
        "{{\n  \"packageDirectories\": [\n    {{\n      \"path\": \"force-app\",\n      \"default\": true,\n      \"package\": \"{APEX_PACKAGE_NAME}\",\n      \"versionName\": \"ver {version}\",\n      \"versionNumber\": \"{version}.NEXT\"\n    }}\n  ],\n  \"name\": \"box-gantry-apex\",\n  \"namespace\": \"{APEX_NAMESPACE}\",\n  \"sfdcLoginUrl\": \"https://login.salesforce.com\",\n  \"sourceApiVersion\": \"{APEX_API_VERSION}\",\n  \"packageAliases\": {{}}\n}}\n"
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
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Package xmlns=\"http://soap.sforce.com/2006/04/metadata\">\n    <types>\n        <members>*</members>\n        <name>ApexClass</name>\n    </types>\n    <types>\n        <members>*</members>\n        <name>RemoteSiteSetting</name>\n    </types>\n    <version>{APEX_API_VERSION}</version>\n</Package>\n"
    )
}

fn project_readme(build: &BuildInfo) -> String {
    // A sentinel + `replace` rather than `format!` so the Apex Quickstart's
    // `Map`/`List` literals need no `{{`/`}}` escaping. The example is derived
    // from the generated surface (Apex has no local compile gate — its
    // verification is the scratch-org deploy, VR-1.3).
    const TEMPLATE: &str = r##"<!-- Generated by box-gantry @VERSION@. DO NOT EDIT. -->
![Box Open SDK for Apex](assets/banner.svg)

# Box SDK for Salesforce Apex

A **community, unofficial** Box API client for Salesforce Apex, as a deploy-ready
SFDX project. Every class lives in `force-app/main/default/classes/`; per-endpoint
reference docs are in [`docs/`](docs/README.md). Provenance (engine version + spec
fingerprint) is recorded in `BoxBuildInfo` and below.

> **Not affiliated with, authorized, or endorsed by Box, Inc.** "Box" is a
> trademark of Box, Inc. This is an independent, generated client.

> Generated by box-gantry `@VERSION@` from spec fingerprint `@FINGERPRINT@`.

## Layout

| Path | What |
|---|---|
| `force-app/main/default/classes/` | model, manager, and client classes (`.cls` + `-meta.xml`) |
| `force-app/main/default/remoteSiteSettings/` | Remote Site Settings for the Box hosts, so callouts are allowed once deployed |
| `docs/` | one Markdown page per endpoint, with a runnable snippet |
| `config/project-scratch-def.json` | scratch-org definition |
| `manifest/package.xml` | wildcard deploy manifest |
| `sfdx-project.json` | project + unlocked-package definition (namespace `@NAMESPACE@`) |

## Quickstart

Apex needs no imports — every generated class is visible in the namespace.
Authenticate, look up the current user, create a folder, upload a file, extract
its fields with Box AI, tag it with metadata, and query for it — end to end:

```apex
// Client Credentials Grant (server-to-server); developer token and JWT are also
// available via BoxAuth.
Box client = new Box(new BoxHttpClient(
    BoxAuth.clientCredentials('CLIENT_ID', 'CLIENT_SECRET', 'enterprise', 'ENTERPRISE_ID')));

// The current user.
UserFull me = client.users.getMe(null);
System.debug('authenticated as ' + me.id);

// Create a folder at the account root ('0').
FolderCreateRequest folderReq = new FolderCreateRequest();
folderReq.name = 'Invoices';
folderReq.parent = new AttributesParent();
folderReq.parent.id = '0';
FolderFull folder = client.folders.create(null, folderReq);

// Upload a file into it.
PostFileContentAttributes attrs = new PostFileContentAttributes();
attrs.name = 'invoice.pdf';
attrs.parent = new AttributesParent();
attrs.parent.id = folder.id;
FileContentCreateRequest uploadReq = new FileContentCreateRequest();
uploadReq.attributes = attrs;
uploadReq.file = Blob.valueOf('<file bytes>');
Files uploaded = client.uploads.uploadFile(null, null, uploadReq);
String fileId = uploaded.entries[0].id;

// Extract fields from the file with Box AI.
AiItemBase item = new AiItemBase();
item.id = fileId;
item.type = AiCitationType.File;
AiExtract extractReq = new AiExtract();
extractReq.prompt = 'Extract the invoice number and total amount.';
extractReq.items = new List<AiItemBase>{ item };
AiResponse answer = client.ai.extract(extractReq);
System.debug(answer);

// Attach that metadata to the file (an enterprise template).
Map<String, Object> metadata = new Map<String, Object>{
    'invoiceNumber' => 'INV-0042',
    'total' => 1250
};
client.fileMetadata.createFileMetadata(fileId, 'enterprise', 'invoiceData', metadata);

// Query for files carrying that metadata.
MetadataQuery queryReq = new MetadataQuery();
queryReq.from_r = 'enterprise_0.invoiceData';
queryReq.ancestor_folder_id = folder.id;
MetadataQueryResults results = client.search.queryByMetadata(queryReq);
System.debug(results);
```

See [`docs/`](docs/README.md) for a reference page — with its own snippet — for
every endpoint.

## Deploy

```bash
sf org create scratch -f config/project-scratch-def.json -a box-sdk
sf project deploy start -x manifest/package.xml -o box-sdk
```

## Packaging (unlocked 2GP)

The SDK ships as the unlocked package **`@PACKAGE@`** under the `@NAMESPACE@`
namespace. Unlocked (not managed) so a regenerated version can add, remove, or
rename members freely. One-time, against your Dev Hub (the namespace must be
linked to it):

```bash
sf package create --name "@PACKAGE@" --package-type Unlocked \
    --path force-app --target-dev-hub myHub
```

This prints the package id (`0Ho…`). **Save it** — this SFDX project is
regenerated on every build, which rewrites `sfdx-project.json` and clears
`packageAliases`, so the `0Ho…` id (not the by-name alias) is the durable handle.
Then build a version by id — this compiles + runs every test in the namespace and
yields the installable ship artifact (a `04t` version id):

```bash
sf package version create --package 0Ho... --installation-key-bypass \
    --code-coverage --wait 60 --target-dev-hub myHub
```

`versionNumber` in `sfdx-project.json` auto-increments its `.NEXT` build segment;
set the major.minor from the engine's spec-diff (a breaking change is a major
bump). Install with `sf package install --package 04t... --target-org yourOrg`.
"##;
    TEMPLATE
        .replace("@VERSION@", &build.engine)
        .replace("@FINGERPRINT@", &build.spec_fingerprint)
        .replace("@NAMESPACE@", APEX_NAMESPACE)
        .replace("@PACKAGE@", APEX_PACKAGE_NAME)
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
