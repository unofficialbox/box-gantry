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
    // The shared community-design banner the README renders at its top (NF-8).
    files.push(GeneratedFile {
        path: "assets/banner.svg".to_string(),
        content: gantry_manifest::banner_svg("Go"),
    });
    // Deterministic output is sorted by path (FR-6.2), as the other backends do.
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

/// The module landing page, rendered at the repository root and on pkg.go.dev.
fn readme() -> String {
    format!(
        r##"<!-- Generated by box-gantry. DO NOT EDIT — regenerate from the specs instead. -->
![Box Open SDK for Go](assets/banner.svg)

# box-open-sdk (Go)

[![Go Reference](https://pkg.go.dev/badge/{MODULE}.svg)](https://pkg.go.dev/{MODULE})

A **community, unofficial** Box API client for Go — typed models for the whole
Box surface, one manager per API area behind a single `Client`, and a
`net/http` runtime with retry, exponential backoff, `Retry-After` handling, and
automatic token refresh. Standard library only; no third-party dependencies.

> **Not affiliated with, authorized, or endorsed by Box, Inc.** "Box" is a
> trademark of Box, Inc. This is an independent, generated client.

## Install

```sh
go get {MODULE}@latest
```

## Quickstart

```go
package main

import (
	"context"
	"fmt"
	"log"

	"{MODULE}/auth"
	"{MODULE}/client"
)

func main() {{
	// Developer token for a quick start; CCG, OAuth, and JWT are also supported.
	c := client.NewClient(auth.DeveloperToken("DEVELOPER_TOKEN"))

	// Every API area is a manager on the client.
	me, err := c.Users.GetMe(context.Background(), nil)
	if err != nil {{
		log.Fatal(err)
	}}
	fmt.Println(me)

	// List endpoints return a range-over-func iterator (Go 1.23+); paging is
	// automatic.
	for user, err := range c.Users.List(context.Background(), nil) {{
		if err != nil {{
			log.Fatal(err)
		}}
		fmt.Println(user.Id)
	}}
}}
```

## Authentication

The runtime implements Box's four auth flows — **developer token**, **client
credentials (CCG)**, **OAuth 2.0** (with a pluggable refresh-token store), and
**JWT** (server auth). See [`docs/auth.md`](./docs/auth.md).

## Documentation

API reference on [pkg.go.dev](https://pkg.go.dev/{MODULE}); the [`docs/`](./docs)
tree carries the per-manager reference and the authentication, pagination, and
errors guides.

## License

MIT. Generated by [box-gantry](https://github.com/unofficialbox/box-gantry).
"##,
    )
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
