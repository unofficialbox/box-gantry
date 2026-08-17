//! Per-language capability manifests (FR-4).
//!
//! One declarative value per target language. Feature synthesis keys off
//! these axes and *only* these axes — comparing against a language name
//! anywhere outside this crate is prohibited (FR-4.2; the old engine had
//! 31 `=== 'CSharp'` sites).
//!
//! The axes are drafted here in M0/M1 so the {Go, Apex, Rust} extremes are
//! visible from the start (assessment §4); the full manifests are M2 work.

/// The published version stamped into every generated SDK's package manifest
/// (`Cargo.toml`, `package.json`, `pom.xml`, `sfdx-project.json`).
///
/// One constant for all backends: the release layout had each backend hardcode
/// its own `"0.1.0"`, with nothing keeping them in step — which is how npm
/// shipped `0.1.0` while Go's tag-derived version moved to `0.1.1` (D-192
/// fallout). Go is the deliberate exception: its version is the git tag, so it
/// carries no in-file version to align.
///
/// A single-place bump here re-versions the whole fleet at once.
pub const SDK_VERSION: &str = "0.4.0";

/// The MIT license text, emitted as `LICENSE` by every backend.
///
/// Every generated package manifest already *declares* MIT — `Cargo.toml`'s
/// `license`, `package.json`'s `"license"`, the pom's `<licenses>`. None of them
/// ship the license itself, and a declaration without the file is not a license
/// grant: pkg.go.dev read the Go module as "License: None detected — not
/// redistributable", and crates.io and npm both expect the file in the package.
///
/// Held here rather than in each backend so the five SDKs cannot drift to
/// different terms, and so the year and holder have one place to change.
pub const LICENSE: &str = "\
MIT License

Copyright (c) 2026 Kyle @ Unofficial Box

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the \"Software\"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
";

/// The README header banner (NF-8), emitted as `assets/banner.svg` by every
/// backend and referenced at the top of the generated README.
///
/// One SVG in the Unofficial Box community design language — deep-navy stage
/// with a dotted-grid texture, the `B/` mark, a monospace eyebrow, the coral
/// wordmark period, and an offset-shadow language badge. Only the language
/// label varies, so the five SDKs read as one fleet. System fonts only
/// (Helvetica/Arial, `ui-monospace`, Georgia) — no web-font dependency, matching
/// the dependency-light rule — and self-contained navy, so it renders on both
/// GitHub light and dark. Deterministic: same language in → same bytes out.
///
/// `language` is both the badge text and the alt/aria label (e.g. `"Go"`,
/// `"TypeScript"`).
pub fn banner_svg(language: &str) -> String {
    // The badge is a fixed-width pill with centered text; size it from the label
    // (generous padding) so the text clears the pill even under a fallback font.
    let badge_w = 72 + language.chars().count() as u32 * 24;
    let badge_x = 1144 - badge_w - 8;
    let badge_half = badge_w / 2;
    // No literal `{`/`}` anywhere in SVG, so nothing to escape but the args.
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 1200 300\" role=\"img\" \
         aria-label=\"Box Open SDK for {language}\">\
         <defs><pattern id=\"d\" width=\"26\" height=\"26\" patternUnits=\"userSpaceOnUse\">\
         <circle cx=\"1.5\" cy=\"1.5\" r=\"1.5\" fill=\"#16233a\"/></pattern></defs>\
         <rect width=\"1200\" height=\"300\" fill=\"#0b172a\"/>\
         <rect width=\"1200\" height=\"300\" fill=\"url(#d)\"/>\
         <rect width=\"1200\" height=\"5\" fill=\"#0866d9\"/>\
         <text x=\"56\" y=\"70\" font-family=\"'Helvetica Neue',Helvetica,Arial,sans-serif\" \
         font-weight=\"800\" font-size=\"32\"><tspan fill=\"#fffefa\">B</tspan>\
         <tspan fill=\"#5c9df2\">/</tspan></text>\
         <text x=\"58\" y=\"134\" font-family=\"ui-monospace,SFMono-Regular,Menlo,monospace\" \
         font-weight=\"600\" font-size=\"15\" letter-spacing=\"2\" fill=\"#5c9df2\">\
         COMMUNITY-BUILT · OPEN SOURCE</text>\
         <text x=\"54\" y=\"208\" font-family=\"'Helvetica Neue',Helvetica,Arial,sans-serif\" \
         font-weight=\"800\" font-size=\"66\" letter-spacing=\"-1\">\
         <tspan fill=\"#fffefa\">BOX OPEN SDK</tspan><tspan fill=\"#ff6658\">.</tspan></text>\
         <text x=\"56\" y=\"252\" font-family=\"Georgia,'Times New Roman',serif\" font-size=\"19\" \
         fill=\"#9fb0c6\">Every Box endpoint, typed — generated. dependency-light, punk rock 🤘</text>\
         <g transform=\"translate({badge_x},110)\">\
         <rect x=\"8\" y=\"8\" width=\"{badge_w}\" height=\"72\" rx=\"10\" fill=\"#ff6658\"/>\
         <rect x=\"0\" y=\"0\" width=\"{badge_w}\" height=\"72\" rx=\"10\" fill=\"#e9f3ff\"/>\
         <text x=\"{badge_half}\" y=\"50\" text-anchor=\"middle\" \
         font-family=\"'Helvetica Neue',Helvetica,Arial,sans-serif\" font-weight=\"800\" \
         font-size=\"34\" fill=\"#0b172a\">{language}</text></g></svg>\n"
    )
}

#[cfg(test)]
mod banner_tests {
    use super::banner_svg;

    #[test]
    fn banner_is_deterministic_and_language_specific() {
        assert_eq!(banner_svg("Go"), banner_svg("Go"));
        assert_ne!(banner_svg("Go"), banner_svg("Rust"));
    }

    #[test]
    fn banner_carries_the_label_and_brand_marks() {
        let svg = banner_svg("TypeScript");
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("aria-label=\"Box Open SDK for TypeScript\""));
        // The language is the badge text.
        assert!(svg.contains(">TypeScript</text>"));
        // Brand cues: navy stage, coral accent, the community eyebrow.
        assert!(svg.contains("#0b172a") && svg.contains("#ff6658"));
        assert!(svg.contains("COMMUNITY-BUILT · OPEN SOURCE"));
    }
}

/// The capability axes of one target language (FR-4.1).
///
/// Every field is total — there is no "unknown" — so adding an axis forces
/// every manifest to answer for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityManifest {
    /// Stable key used in CLI arguments and output paths (e.g. `"go"`).
    pub key: &'static str,
    pub modules: ModuleSystem,
    pub generics: Generics,
    pub error_model: ErrorModel,
    pub async_model: AsyncModel,
    pub streaming: Streaming,
    /// Platform transaction budgets that shape API design (Apex governor
    /// limits — assessment §4). `None` for targets without them.
    pub callout_limits: Option<CalloutLimits>,
    /// Test coverage the platform mandates before deployment, as a
    /// percentage (Apex: 75). Generated tests are ship-blocking when set.
    pub mandated_test_coverage: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleSystem {
    /// Real packages/modules; the IR module tree lowers directly.
    Hierarchical,
    /// One flat namespace; modules lower to outer-class grouping + name
    /// mangling (TR-Apex.1).
    Flat { identifier_limit: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Generics {
    Full,
    /// No user-defined generics: shared containers lower to per-type code
    /// or typed `Object` wrappers (TR-Apex.2).
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorModel {
    /// Go: `(T, error)`.
    ValueAndError,
    /// Apex: exceptions.
    Exceptions,
    /// Rust: `Result<T, E>`.
    Result,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsyncModel {
    /// Blocking calls; concurrency is the caller's business (Go, Apex).
    Sync,
    /// Async-first (Rust: `reqwest` + `tokio`, TR-Rust.3).
    Async,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Streaming {
    /// Bodies can stream (Go readers, Rust `Stream`).
    Supported,
    /// Bodies are buffered; sizes bounded by platform heap (Apex — FR-7.4).
    Buffered { max_body_bytes: u32 },
}

/// Per-transaction callout budgets (Apex governor limits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalloutLimits {
    pub max_callouts_per_transaction: u16,
    pub max_heap_bytes: u32,
}

/// The Go manifest (TR-Go). Draft — reviewed and frozen in M2.
pub fn go() -> CapabilityManifest {
    CapabilityManifest {
        key: "go",
        modules: ModuleSystem::Hierarchical,
        generics: Generics::Full,
        error_model: ErrorModel::ValueAndError,
        async_model: AsyncModel::Sync,
        streaming: Streaming::Supported,
        callout_limits: None,
        mandated_test_coverage: None,
    }
}

/// The Apex manifest (TR-Apex). Draft — the stress-test axes, kept visible
/// from day one so nothing upstream assumes them away (assessment §8).
pub fn apex() -> CapabilityManifest {
    CapabilityManifest {
        key: "apex",
        modules: ModuleSystem::Flat {
            identifier_limit: 40,
        },
        generics: Generics::None,
        error_model: ErrorModel::Exceptions,
        async_model: AsyncModel::Sync,
        streaming: Streaming::Buffered {
            max_body_bytes: 6 * 1024 * 1024,
        },
        callout_limits: Some(CalloutLimits {
            max_callouts_per_transaction: 100,
            max_heap_bytes: 6 * 1024 * 1024,
        }),
        mandated_test_coverage: Some(75),
    }
}

/// The Rust manifest (TR-Rust). Draft — reviewed and frozen in M2.
pub fn rust() -> CapabilityManifest {
    CapabilityManifest {
        key: "rust",
        modules: ModuleSystem::Hierarchical,
        generics: Generics::Full,
        error_model: ErrorModel::Result,
        async_model: AsyncModel::Async,
        streaming: Streaming::Supported,
        callout_limits: None,
        mandated_test_coverage: None,
    }
}

/// The TypeScript manifest (TR-TypeScript, D-143). ESM modules; the type system
/// expresses the IR's shapes almost directly (tri-state → `?:`/`| null`,
/// `oneOf` → discriminated unions), a `Promise`-based async API, and a
/// `BoxApiError`-subclass error model surfaced as thrown/rejected exceptions.
pub fn typescript() -> CapabilityManifest {
    CapabilityManifest {
        key: "typescript",
        modules: ModuleSystem::Hierarchical,
        generics: Generics::Full,
        error_model: ErrorModel::Exceptions,
        async_model: AsyncModel::Async,
        streaming: Streaming::Supported,
        callout_limits: None,
        mandated_test_coverage: None,
    }
}

/// The Java manifest (TR-Java, D-164). Java 26 target: real packages (the IR
/// module tree lowers directly), full generics, an exceptions error model
/// (thrown/`throws`), and a **blocking** `java.net.http.HttpClient` API — the
/// SDK is synchronous, with concurrency the caller's business over virtual
/// threads (like Go/Apex), not async-first like Rust/TS. Bodies stream through
/// the JDK client, so streaming is supported and there are no platform callout
/// budgets or mandated test coverage.
pub fn java() -> CapabilityManifest {
    CapabilityManifest {
        key: "java",
        modules: ModuleSystem::Hierarchical,
        generics: Generics::Full,
        error_model: ErrorModel::Exceptions,
        async_model: AsyncModel::Sync,
        streaming: Streaming::Supported,
        callout_limits: None,
        mandated_test_coverage: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_keys_are_distinct() {
        let keys = [
            go().key,
            apex().key,
            rust().key,
            typescript().key,
            java().key,
        ];
        let mut sorted = keys.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len());
    }

    #[test]
    fn the_three_targets_cover_the_extremes() {
        // The IR is designed against near-opposite extremes (assessment §4);
        // if these ever converge, the design pressure is gone.
        assert_ne!(go().error_model, rust().error_model);
        assert_eq!(apex().generics, Generics::None);
        assert!(matches!(apex().modules, ModuleSystem::Flat { .. }));
        assert!(apex().callout_limits.is_some());
        assert_eq!(apex().mandated_test_coverage, Some(75));
    }
}
