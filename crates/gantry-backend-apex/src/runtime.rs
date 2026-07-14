//! The hand-written Apex runtime, embedded into every generated SDK.
//!
//! Apex has no package imports — every class deploys together in one flat
//! namespace — so the runtime ships *inside* the generated project's
//! `classes/` directory rather than as a referenced dependency (the Go
//! `gantryruntime` model, adapted to Apex's flatness). The source of truth is
//! `runtimes/apex/main/default/classes/*.cls`, embedded here at build time so
//! generation has no filesystem dependency. The generated `BoxClient`
//! interface + `BoxRequest`/`BoxResponse` stubs are the contract; these
//! classes are the behavior (`BoxHttpClient implements BoxClient` + auth).

use crate::{CLASSES_DIR, GeneratedFile};

/// The runtime's hand-written class names — reserved so no generated model,
/// manager, or client class can collide with them in the flat namespace.
pub(crate) const RUNTIME_CLASS_NAMES: &[&str] = &[
    "BoxTokenProvider",
    "BoxDeveloperTokenProvider",
    "BoxCcgTokenProvider",
    "BoxCcgTokenProviderTest",
    "BoxJwtTokenProvider",
    "BoxJwtTokenProviderTest",
    "BoxApiException",
    "BoxHttpClient",
];

/// The embedded runtime classes, as generated files under `classes/`.
pub(crate) fn runtime_classes() -> Vec<GeneratedFile> {
    const SOURCES: &[(&str, &str)] = &[
        (
            "BoxTokenProvider",
            include_str!("../../../runtimes/apex/main/default/classes/BoxTokenProvider.cls"),
        ),
        (
            "BoxDeveloperTokenProvider",
            include_str!(
                "../../../runtimes/apex/main/default/classes/BoxDeveloperTokenProvider.cls"
            ),
        ),
        (
            "BoxCcgTokenProvider",
            include_str!("../../../runtimes/apex/main/default/classes/BoxCcgTokenProvider.cls"),
        ),
        (
            "BoxCcgTokenProviderTest",
            include_str!("../../../runtimes/apex/main/default/classes/BoxCcgTokenProviderTest.cls"),
        ),
        (
            "BoxJwtTokenProvider",
            include_str!("../../../runtimes/apex/main/default/classes/BoxJwtTokenProvider.cls"),
        ),
        (
            "BoxJwtTokenProviderTest",
            include_str!("../../../runtimes/apex/main/default/classes/BoxJwtTokenProviderTest.cls"),
        ),
        (
            "BoxApiException",
            include_str!("../../../runtimes/apex/main/default/classes/BoxApiException.cls"),
        ),
        (
            "BoxHttpClient",
            include_str!("../../../runtimes/apex/main/default/classes/BoxHttpClient.cls"),
        ),
    ];
    SOURCES
        .iter()
        .map(|(name, content)| GeneratedFile {
            path: format!("{CLASSES_DIR}/{name}.cls"),
            content: (*content).to_string(),
        })
        .collect()
}
