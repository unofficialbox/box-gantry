//! The Go backend: lowering + printer (FR-6, TR-Go).
//!
//! Generates the SDK tree: `schemas/` model packages (D-110 tri-state
//! mapping, open enums, union variant structs with the only generated
//! serializers), `managers/` with one method per operation calling only
//! through the runtime contract (FR-5.2), the `client/` entry point
//! (FR-7.1), and the contract's compilable runtime stubs so the whole
//! output compile-verifies without a real runtime (FR-5.3). Output is
//! deterministic (FR-6.2) and gofmt-clean by construction (G-17),
//! verified by the real toolchain (VR-1.1 — the primary CI signal).

mod docs;
mod managers;
mod models;
mod tests;

/// The one place the module path is written. A macro rather than a `const` so
/// the package paths below can be built with `concat!` and stay `&'static str`
/// — the import sets are static-only.
macro_rules! module_path_literal {
    () => {
        "github.com/unofficialbox/box-open-go-sdk"
    };
}

/// The published module path — the `go.mod` `module` line and the prefix of
/// every intra-SDK import.
///
/// Go resolves a module by its path, so this must be the SDK repository's real
/// URL and `go.mod` must sit at that repository's root. Each language ships
/// from its own repository (`box-open-<lang>-sdk`), which is what keeps this a
/// bare root path with plain `v0.1.0` tags rather than a `/go` subdirectory
/// needing `go/v0.1.0`-prefixed ones.
pub(crate) const MODULE: &str = module_path_literal!();
/// The intra-SDK packages imported by name from more than one printer.
pub(crate) const SERIALIZATION_IMPORT: &str = concat!(module_path_literal!(), "/serialization");
pub(crate) const RUNTIME_IMPORT: &str = concat!(module_path_literal!(), "/gantryruntime");

/// Published versions withdrawn via `go.mod` `retract` (D-192).
///
/// A module version is immutable once `proxy.golang.org` has served it, so a
/// bad release can only be *superseded*: a later version carrying a `retract`
/// directive tells the toolchain never to select it. The block therefore has to
/// live in the generator — a hand-edit to `go.mod` would be erased by the next
/// regeneration, silently un-retracting the bad version.
pub(crate) const RETRACTIONS: &str = "\n\
     // v0.1.0 shipped the generated compile-only runtime stub instead of the\n\
     // real runtime (TR-Go.7): it builds, but every call panics. Superseded by\n\
     // v0.1.1, which vendors the real runtime.\n\
     retract v0.1.0\n";

pub use docs::generate_docs;
pub use managers::{BackendError, generate_managers};
pub use models::{GeneratedFile, generate_models};
pub use tests::generate_tests;

/// Provenance stamped into the generated SDK for traceability (NF-7): the
/// engine version that produced it and the fingerprint of the input specs.
/// Every release is then traceable to its exact inputs.
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

/// Generate the complete SDK tree for a verified program, stamping it with
/// the build provenance (NF-7). The output is the NF-8 ship artifact: a
/// self-contained, tagged-ready Go module (`go.mod` + the `buildinfo`
/// package reporting its own version).
pub fn generate(
    analysis: &gantry_sema::Analysis<'_>,
    build: &BuildInfo,
) -> Result<Vec<GeneratedFile>, BackendError> {
    let paged = gantry_synth::detect_pagination(analysis);
    let mut files = generate_models(analysis, build);
    files.extend(generate_managers(analysis, &paged)?);
    files.extend(generate_docs(analysis, &paged));
    files.extend(generate_tests(analysis));
    // The chunked-upload orchestrator (D-183): a fixed hand-written helper in the
    // `client` package that drives the generated `ChunkedUploadsManager` — create
    // a session, upload the parts with bounded concurrency, commit — for a new
    // file or a new version. It names concrete `schemas.*` types and manager
    // methods, so it is emitted **only** when the spec carries the whole surface
    // (VR-6: never emit code that wouldn't compile); the Go compile gate is the
    // ultimate backstop for the exact signatures.
    if emits_chunked_upload(analysis) {
        files.push(GeneratedFile {
            path: "client/chunked_upload.go".to_string(),
            content: chunked_upload_go(),
        });
    }
    // The real runtime is vendored here (D-192); generated managers still call
    // only the declared contract surface (FR-5.2), which is what lets the
    // hand-written implementation drop in unchanged.
    files.extend(runtime_files());
    files.push(GeneratedFile {
        path: "serialization/serialization.go".to_string(),
        content: SERIALIZATION.to_string(),
    });
    // The buildinfo package makes the provenance programmatically
    // accessible (NF-7): the shipped SDK can report its own version.
    files.push(GeneratedFile {
        path: "buildinfo/buildinfo.go".to_string(),
        content: buildinfo_go(build),
    });
    // The repository/pkg.go.dev landing page (NF-8). Without it the module root
    // renders blank on both GitHub and pkg.go.dev.
    files.push(GeneratedFile {
        path: "README.md".to_string(),
        content: readme(),
    });
    // The license itself, not just the manifest's declaration of it: pkg.go.dev
    // reads a module with no LICENSE as "None detected — not redistributable".
    files.push(GeneratedFile {
        path: "LICENSE".to_string(),
        content: gantry_manifest::LICENSE.to_string(),
    });
    // A doc-only package at the module root. Every importable package lives in a
    // subdirectory, which left the module root with no package at all — so
    // pkg.go.dev had nothing to render and showed a bare directory listing under
    // the module name. This gives the module a documentation landing page; it
    // exports nothing, so it adds no surface to keep compatible.
    files.push(GeneratedFile {
        path: "doc.go".to_string(),
        content: doc_go(),
    });
    // The shared community-design banner the README renders at its top (NF-8).
    files.push(GeneratedFile {
        path: "assets/banner.svg".to_string(),
        content: gantry_manifest::banner_svg("Go"),
    });
    // Deterministic output is sorted by path (FR-6.2), as the other backends do.
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

/// Whether the program carries the whole chunked-upload surface the
/// `client/chunked_upload.go` orchestrator references (D-183) — both the concrete
/// `schemas.*` types **and** the `ChunkedUploadsManager` methods it calls, so
/// emitting it against a spec that lacks any of them would not compile. Emit only
/// when all are present (VR-6), which also keeps the synthetic-spec gates
/// (round-trip, unit tests) free of the dependency. The exact method signatures
/// can't be checked here, so the Go `go build` gate is the compile-time backstop —
/// a mismatch fails the build, it never ships broken code.
fn emits_chunked_upload(analysis: &gantry_sema::Analysis<'_>) -> bool {
    let program = analysis.program;
    // The concrete `schemas.*` types the orchestrator names, as `package.Name`.
    let mut fqns: std::collections::HashSet<String> = std::collections::HashSet::new();
    for decl in &program.decls {
        let (_, package) = models::module_dir_and_package(&decl.module);
        fqns.insert(format!("{package}.{}", decl.name.as_str()));
    }
    const REQUIRED_TYPES: [&str; 7] = [
        "schemas.UploadSession",
        "schemas.UploadPart",
        "schemas.UploadedPart",
        "schemas.Files",
        "schemas.FileUploadSessionCreateRequest",
        "schemas.FileVersionUploadSessionCreateRequest",
        "schemas.FileUploadSessionCommitRequest",
    ];
    if !REQUIRED_TYPES.iter().all(|r| fqns.contains(*r)) {
        return false;
    }
    // The four `ChunkedUploadsManager` methods the orchestrator calls, named the
    // way the manager printer names them (`managers::method_name`).
    let methods: std::collections::HashSet<String> = program
        .operations
        .iter()
        .map(managers::method_name)
        .collect();
    const REQUIRED_METHODS: [&str; 4] = [
        "CreateFileUploadSession",
        "CreateFileVersionUploadSession",
        "UpdateFileUploadSession",
        "CommitFileUploadSession",
    ];
    REQUIRED_METHODS.iter().all(|m| methods.contains(*m))
}

/// The chunked-upload orchestrator source (D-183), a fixed hand-written helper
/// over the generated `ChunkedUploadsManager`. Standard library only (`crypto/sha1`
/// for the Box part/whole-file digests, a bounded goroutine pool for concurrency)
/// so it adds no dependency. A sentinel + `replace` keeps the brace-dense Go body
/// free of `format!` escaping; only the module import path is substituted.
fn chunked_upload_go() -> String {
    const TEMPLATE: &str = r##"// Code generated by box-gantry. DO NOT EDIT.
package client

import (
	"bytes"
	"context"
	"crypto/sha1"
	"encoding/base64"
	"fmt"
	"sync"

	"@MODULE@/schemas"
)

// ChunkedUpload orchestrates a Box chunked (multipart) upload over a Client: it
// creates an upload session, uploads the content's parts in parallel, and
// commits — for a new file (Upload) or a new version of an existing file
// (UploadVersion). Box requires chunked upload for files at or above its
// minimum session size; smaller files use the single-shot upload endpoints.
type ChunkedUpload struct {
	client *Client
	// maxConcurrent bounds parts in flight, capping peak buffer memory.
	maxConcurrent int
}

// NewChunkedUpload builds an orchestrator over an existing client.
func (c *Client) NewChunkedUpload() *ChunkedUpload {
	return &ChunkedUpload{client: c, maxConcurrent: 4}
}

// Upload uploads content as a new file named fileName into folderID.
func (u *ChunkedUpload) Upload(ctx context.Context, content []byte, fileName, folderID string) (*schemas.Files, error) {
	session, err := u.client.ChunkedUploads.CreateFileUploadSession(ctx, &schemas.FileUploadSessionCreateRequest{
		FolderId: folderID,
		FileSize: int64(len(content)),
		FileName: fileName,
	})
	if err != nil {
		return nil, err
	}
	return u.finish(ctx, session, content)
}

// UploadVersion uploads content as a new version of the existing file fileID.
func (u *ChunkedUpload) UploadVersion(ctx context.Context, content []byte, fileName, fileID string) (*schemas.Files, error) {
	session, err := u.client.ChunkedUploads.CreateFileVersionUploadSession(ctx, fileID, &schemas.FileVersionUploadSessionCreateRequest{
		FileSize: int64(len(content)),
		FileName: &fileName,
	})
	if err != nil {
		return nil, err
	}
	return u.finish(ctx, session, content)
}

func (u *ChunkedUpload) finish(ctx context.Context, session *schemas.UploadSession, content []byte) (*schemas.Files, error) {
	if session.Id == nil {
		return nil, fmt.Errorf("gantry: upload session returned no id")
	}
	if session.PartSize == nil || *session.PartSize <= 0 {
		return nil, fmt.Errorf("gantry: upload session returned a non-positive part_size")
	}
	id := *session.Id
	partSize := int(*session.PartSize)
	total := len(content)

	var offsets []int
	for off := 0; off < total; off += partSize {
		offsets = append(offsets, off)
	}
	parts := make([]schemas.UploadPart, len(offsets))

	// Cancel the still-queued and in-flight part uploads as soon as one fails,
	// so a failure early in a large upload doesn't push every remaining part.
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()

	sem := make(chan struct{}, u.maxConcurrent)
	var wg sync.WaitGroup
	var mu sync.Mutex
	var firstErr error
	for i, off := range offsets {
		wg.Add(1)
		sem <- struct{}{}
		go func(i, off int) {
			defer wg.Done()
			defer func() { <-sem }()
			end := off + partSize
			if end > total {
				end = total
			}
			part, err := u.uploadPart(ctx, id, content, off, end, total)
			mu.Lock()
			defer mu.Unlock()
			if err != nil {
				if firstErr == nil {
					firstErr = err
					cancel()
				}
				return
			}
			parts[i] = part
		}(i, off)
	}
	wg.Wait()
	if firstErr != nil {
		return nil, firstErr
	}

	digest := "sha=" + base64.StdEncoding.EncodeToString(sha1sum(content))
	return u.client.ChunkedUploads.CommitFileUploadSession(ctx, id, digest, &schemas.FileUploadSessionCommitRequest{Parts: parts}, nil)
}

func (u *ChunkedUpload) uploadPart(ctx context.Context, id string, content []byte, start, end, total int) (schemas.UploadPart, error) {
	slice := content[start:end]
	digest := "sha=" + base64.StdEncoding.EncodeToString(sha1sum(slice))
	contentRange := fmt.Sprintf("bytes %d-%d/%d", start, end-1, total)
	uploaded, err := u.client.ChunkedUploads.UpdateFileUploadSession(ctx, id, digest, contentRange, bytes.NewReader(slice))
	if err != nil {
		return schemas.UploadPart{}, err
	}
	if uploaded == nil || uploaded.Part == nil {
		return schemas.UploadPart{}, fmt.Errorf("gantry: upload part returned no part")
	}
	return *uploaded.Part, nil
}

func sha1sum(b []byte) []byte {
	sum := sha1.Sum(b)
	return sum[:]
}
"##;
    TEMPLATE.replace("@MODULE@", MODULE)
}

/// The doc-only root package, so pkg.go.dev has documentation to render for the
/// module rather than a bare listing of its subdirectories.
///
/// Deliberately exports nothing: it is a landing page, not API surface. The
/// import paths it names are the ones a caller actually reaches for, so the
/// module page answers "where do I start" without a round trip to the README.
fn doc_go() -> String {
    const TEMPLATE: &str = r##"// Code generated by box-gantry. DO NOT EDIT.

// Package boxopensdk is the module root for the Box Open SDK for Go: an open
// source, community-built client for the Box API, generated from the Box
// OpenAPI specification.
//
// This package is documentation only and exports nothing. The SDK lives in its
// subpackages:
//
//   - [@MODULE@/client] — the Client, one manager per API area.
//   - [@MODULE@/auth] — Client Credentials Grant, JWT, OAuth, developer token.
//   - [@MODULE@/schemas] — the typed request and response models.
//   - [@MODULE@/gantryruntime] — the net/http runtime: retry, backoff, token refresh.
//
// A first call:
//
//	c := client.NewClient(auth.ClientCredentials(auth.CCGConfig{
//		ClientID:     "CLIENT_ID",
//		ClientSecret: "CLIENT_SECRET",
//		EnterpriseID: "ENTERPRISE_ID",
//	}))
//	me, err := c.Users.GetMe(context.Background(), nil)
//
// See the README for an end-to-end Quickstart, and docs/ for a reference page
// per manager with an example on every method.
//
// Not affiliated with, authorized, or endorsed by Box, Inc. "Box" is a
// trademark of Box, Inc. This is an independent, generated client.
package boxopensdk
"##;
    TEMPLATE.replace("@MODULE@", MODULE)
}

/// The module landing page, rendered at the repository root and on pkg.go.dev.
///
/// A sentinel + `replace` rather than `format!` so the Go code block — dense
/// with braces — needs no `{{`/`}}` escaping. The Quickstart is an end-to-end
/// flow, extracted and compiled against the real SDK by the runtime gate so it
/// can never drift.
fn readme() -> String {
    const TEMPLATE: &str = r##"<!-- Generated by box-gantry. DO NOT EDIT — regenerate from the specs instead. -->
![Box Open SDK for Go](assets/banner.svg)

# box-open-sdk (Go)

[![release](https://img.shields.io/github/v/release/@REPO@?sort=semver)](https://github.com/@REPO@/releases/latest)
[![Go Reference](https://pkg.go.dev/badge/@MODULE@.svg)](https://pkg.go.dev/@MODULE@)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

An **open source, community-built** Box API client for Go — typed models for the whole
Box surface, one manager per API area behind a single `Client`, and a
`net/http` runtime with retry, exponential backoff, `Retry-After` handling, and
automatic token refresh. Standard library only; no third-party dependencies.

> **Not affiliated with, authorized, or endorsed by Box, Inc.** "Box" is a
> trademark of Box, Inc. This is an independent, generated client.

## Install

```sh
go get @MODULE@@latest
```

## Quickstart

Authenticate, look up the current user, create a folder, upload a file, extract
its fields with Box AI, tag it with metadata, and query for it — end to end:

```go
package main

import (
	"context"
	"fmt"
	"log"
	"strings"

	"@MODULE@/auth"
	"@MODULE@/client"
	"@MODULE@/schemas"
)

func main() {
	ctx := context.Background()

	// Client Credentials Grant (server-to-server); developer token, OAuth, and
	// JWT are also supported — see docs/auth.md.
	c := client.NewClient(auth.ClientCredentials(auth.CCGConfig{
		ClientID:     "CLIENT_ID",
		ClientSecret: "CLIENT_SECRET",
		EnterpriseID: "ENTERPRISE_ID",
	}))

	// The current user.
	me, err := c.Users.GetMe(ctx, nil)
	if err != nil {
		log.Fatal(err)
	}
	fmt.Println("authenticated as", me.Id)

	// Create a folder at the account root ("0").
	folder, err := c.Folders.Create(ctx, &schemas.FolderCreateRequest{
		Name:   "Invoices",
		Parent: schemas.AttributesParent{Id: "0"},
	}, nil)
	if err != nil {
		log.Fatal(err)
	}

	// Upload a file into it.
	uploaded, err := c.Uploads.UploadFile(ctx, &schemas.FileContentCreateRequest{
		Attributes: schemas.PostFileContentAttributes{
			Name:   "invoice.pdf",
			Parent: schemas.AttributesParent{Id: folder.Id},
		},
		File: strings.NewReader("<file bytes>"),
	}, nil)
	if err != nil {
		log.Fatal(err)
	}
	fileID := uploaded.Entries[0].Id

	// Extract fields from the file with Box AI.
	answer, err := c.Ai.Extract(ctx, &schemas.AiExtract{
		Prompt: "Extract the invoice number and total amount.",
		Items:  []schemas.AiItemBase{{Id: fileID, Type: schemas.AiCitationTypeFile}},
	})
	if err != nil {
		log.Fatal(err)
	}
	fmt.Println(answer)

	// Attach that metadata to the file (an enterprise template).
	if _, err := c.FileMetadata.CreateFileMetadata(ctx, fileID,
		schemas.GetFileIdMetadataIdIdScopeEnterprise, "invoiceData",
		map[string]any{"invoiceNumber": "INV-0042", "total": 1250}); err != nil {
		log.Fatal(err)
	}

	// Query for files carrying that metadata.
	results, err := c.Search.QueryByMetadata(ctx, &schemas.MetadataQuery{
		From:             "enterprise_0.invoiceData",
		AncestorFolderId: folder.Id,
	})
	if err != nil {
		log.Fatal(err)
	}
	fmt.Println(results)
}
```

## Authentication

Box's four auth flows all live in the `auth` package — **developer token**,
**client credentials (CCG)**, **OAuth 2.0** (with a pluggable refresh-token
store), and **JWT** (server auth). See [`docs/auth.md`](./docs/auth.md).

## Documentation

API reference on [pkg.go.dev](https://pkg.go.dev/@MODULE@); the [`docs/`](./docs)
tree carries the per-manager reference — a call snippet for every method — and
the authentication, pagination, and errors guides.

## License

MIT. Generated by [box-gantry](https://github.com/unofficialbox/box-gantry).
"##;
    // `@REPO@` is the `owner/name` GitHub slug the release badge needs — the
    // module path without its `github.com/` host (`MODULE` is the source of
    // truth for where this SDK ships).
    let repo = MODULE.strip_prefix("github.com/").unwrap_or(MODULE);
    TEMPLATE.replace("@MODULE@", MODULE).replace("@REPO@", repo)
}

/// The hand-written runtime, vendored into the shipped SDK (TR-Go.7, D-192).
///
/// The generated managers call only the runtime contract; `gantry-contract`
/// renders a *compile-only stub* of that surface for generation-time
/// verification (FR-5.3), and shipping that stub would produce an SDK that
/// builds and then panics on the first call. The real implementation is
/// embedded here at build time — the same `include_str!` approach the Apex
/// backend already uses — so a generated tree is functional as emitted.
///
/// `*_test.go` is deliberately excluded: the runtime's own tests import it
/// through its development module path (`boxgantry.invalid/boxsdk`), which does
/// not resolve inside the shipped module and would break `go test ./...` for
/// every consumer.
///
/// The `auth` package imports the transport package by that same dev module
/// path (for the `TokenSource` type); it is rewritten to the shipped module
/// (`RUNTIME_IMPORT`) so the vendored copy resolves inside the generated SDK.
fn runtime_files() -> Vec<GeneratedFile> {
    // (source subdir = shipped subdir, file name, contents).
    [
        (
            "gantryruntime",
            "runtime.go",
            include_str!("../../../runtimes/go/gantryruntime/runtime.go"),
        ),
        (
            "auth",
            "auth.go",
            include_str!("../../../runtimes/go/auth/auth.go"),
        ),
        (
            "auth",
            "pkcs8.go",
            include_str!("../../../runtimes/go/auth/pkcs8.go"),
        ),
    ]
    .into_iter()
    .map(|(subdir, name, content)| GeneratedFile {
        path: format!("{subdir}/{name}"),
        // Vendored files carry the do-not-edit header too (FR-6.3). They are
        // copies, so an edit made in the SDK repository is lost at the next
        // regeneration — the header names the upstream to change instead. The
        // wording also matches Go's `^// Code generated .* DO NOT EDIT\.$`
        // convention, so tooling treats the runtime as generated. The dev
        // module path in intra-runtime imports is rewritten to the shipped one.
        content: format!(
            "// Code generated by box-gantry \
             (vendored from runtimes/go/{subdir}/{name}). DO NOT EDIT.\n\n{}",
            content.replace("boxgantry.invalid/boxsdk/gantryruntime", RUNTIME_IMPORT)
        ),
    })
    .collect()
}

/// The `buildinfo` package: exported constants naming the engine and the
/// spec fingerprint the SDK was generated from.
fn buildinfo_go(build: &BuildInfo) -> String {
    format!(
        "// Code generated by box-gantry {engine}. DO NOT EDIT.\n\
         \n\
         // Package buildinfo records the provenance of this generated SDK\n\
         // (NF-7): the engine version and the fingerprint of the Box\n\
         // OpenAPI specs it was generated from.\n\
         package buildinfo\n\
         \n\
         const (\n\
         \t// EngineVersion is the box-gantry version that produced this SDK.\n\
         \tEngineVersion = {engine:?}\n\
         \n\
         \t// SpecFingerprint identifies the exact input spec set.\n\
         \tSpecFingerprint = {fingerprint:?}\n\
         )\n",
        engine = build.engine,
        fingerprint = build.spec_fingerprint,
    )
}

/// The hand-authored serialization package the models depend on. Static
/// (not per-model — TR-Go.2), so it is content here rather than
/// synthesized. Carries the D-110 tri-state (`Nullable[T]`, resolving
/// BG-1) and the RFC 3339 full-date type (`Date`).
const SERIALIZATION: &str = r#"// Code generated by box-gantry. DO NOT EDIT.
package serialization

import (
	"bytes"
	"encoding/json"
	"time"
)

// Nullable models the D-110 tri-state at a field that may be absent,
// explicitly null, or a value. Use *Nullable[T] with `,omitempty`: a nil
// pointer is absent; a non-nil pointer is sent, as null or as the value.
type Nullable[T any] struct {
	// Valid is false for an explicit JSON null.
	Valid bool
	Value T
}

// Value builds a present, non-null Nullable.
func Value[T any](v T) *Nullable[T] { return &Nullable[T]{Valid: true, Value: v} }

// Null builds a present, explicitly-null Nullable.
func Null[T any]() *Nullable[T] { return &Nullable[T]{} }

func (n Nullable[T]) MarshalJSON() ([]byte, error) {
	if !n.Valid {
		return []byte("null"), nil
	}
	return json.Marshal(n.Value)
}

func (n *Nullable[T]) UnmarshalJSON(data []byte) error {
	if bytes.Equal(bytes.TrimSpace(data), []byte("null")) {
		var zero T
		n.Valid, n.Value = false, zero
		return nil
	}
	n.Valid = true
	return json.Unmarshal(data, &n.Value)
}

// Date is an RFC 3339 full-date (no time component).
type Date struct{ time.Time }

const dateLayout = "2006-01-02"

func (d Date) MarshalJSON() ([]byte, error) {
	return json.Marshal(d.Time.Format(dateLayout))
}

func (d *Date) UnmarshalJSON(data []byte) error {
	var s string
	if err := json.Unmarshal(data, &s); err != nil {
		return err
	}
	t, err := time.Parse(dateLayout, s)
	if err != nil {
		return err
	}
	d.Time = t
	return nil
}

func (d Date) String() string { return d.Time.Format(dateLayout) }
"#;
