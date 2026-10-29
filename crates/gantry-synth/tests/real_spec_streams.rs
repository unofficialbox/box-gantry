//! Event-stream detection over Box's real specs.

use std::path::PathBuf;

use gantry_spec::SpecSet;
use gantry_synth::{PositionShape, detect_event_streams};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/specs")
        .join(name)
}

/// Box has exactly one long-poll event stream: `GET /events` paged by
/// `stream_position`, with `OPTIONS /events` listing its real-time servers.
/// A spec change that breaks either shape drops the stream, and this fails.
#[test]
fn the_events_long_poll_is_the_only_event_stream() {
    let set = SpecSet::load(&[
        fixture("openapi.json"),
        fixture("openapi-v2025.0.json"),
        fixture("openapi-v2026.0.json"),
    ])
    .unwrap();
    let lowering = gantry_spec::lower(&set).unwrap();
    let analysis = gantry_sema::analyze(&lowering.program).unwrap();
    let streams = detect_event_streams(&analysis);
    let ops = &lowering.program.operations;
    let found: Vec<String> = streams
        .iter()
        .map(|s| {
            format!(
                "{}.{} + {}.{}",
                ops[s.list].manager.as_str(),
                ops[s.list].name.as_str(),
                ops[s.options].manager.as_str(),
                ops[s.options].name.as_str()
            )
        })
        .collect();
    assert_eq!(found, ["events.list + events.options"]);
    let stream = &streams[0];
    assert_eq!(stream.position, PositionShape::StringOrInteger);
    assert_eq!(
        lowering.program.decl(stream.event).name.as_str(),
        "Event",
        "the stream yields Box's Event"
    );
}
