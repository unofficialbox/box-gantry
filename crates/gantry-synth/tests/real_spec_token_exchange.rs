//! Token-exchange detection over Box's real specs.

use std::path::PathBuf;

use gantry_spec::SpecSet;
use gantry_synth::detect_token_exchanges;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/specs")
        .join(name)
}

/// Box has exactly one token exchange: `POST /oauth2/token`, whose form body
/// offers the token-exchange grant. Its `#refresh` variation takes a refresh
/// grant only, so it is not one. A spec change that breaks the shape drops
/// the exchange, and this fails.
#[test]
fn the_oauth2_token_post_is_the_only_token_exchange() {
    let set = SpecSet::load(&[
        fixture("openapi.json"),
        fixture("openapi-v2025.0.json"),
        fixture("openapi-v2026.0.json"),
    ])
    .unwrap();
    let lowering = gantry_spec::lower(&set).unwrap();
    let analysis = gantry_sema::analyze(&lowering.program).unwrap();
    let exchanges = detect_token_exchanges(&analysis);
    let ops = &lowering.program.operations;
    let found: Vec<String> = exchanges
        .iter()
        .map(|e| {
            let op = &ops[e.operation];
            format!(
                "{}.{}{} -> {}",
                op.manager.as_str(),
                op.name.as_str(),
                op.variation
                    .as_ref()
                    .map(|v| format!("#{}", v.as_str()))
                    .unwrap_or_default(),
                lowering.program.decl(e.body).name.as_str()
            )
        })
        .collect();
    assert_eq!(
        found,
        ["authorization.create_oauth2_token -> PostOAuth2Token"]
    );
}
