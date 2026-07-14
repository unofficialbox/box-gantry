//! Pin the design properties that make the Queueable upload work — so the
//! finding can't silently rot if the sketch is edited.

use apex_queueable_upload_spike::SKETCH;

fn assert_has(needle: &str) {
    assert!(SKETCH.contains(needle), "sketch must contain: {needle}");
}

/// Assert `first` appears before `second` in the sketch — a light structural
/// check (substring presence alone can't prove ordering).
fn assert_before(first: &str, second: &str) {
    let a = SKETCH
        .find(first)
        .unwrap_or_else(|| panic!("missing: {first}"));
    let b = SKETCH
        .find(second)
        .unwrap_or_else(|| panic!("missing: {second}"));
    assert!(a < b, "expected `{first}` before `{second}`");
}

#[test]
fn it_is_a_callout_capable_queueable() {
    assert_has("implements Queueable, Database.AllowsCallouts");
}

#[test]
fn it_chains_one_child_per_execution() {
    // The chain: each execute() enqueues the next link (a fresh transaction).
    assert_has("System.enqueueJob(this);");
}

#[test]
fn it_handles_exactly_one_part_per_transaction() {
    // Heap win: one part fetched + PUT per execute, bounded by part_size.
    assert_has("fetchRange(this.nextOffset, len)");
    assert_has("this.nextOffset += len;");
    // Order within a part: fetch the range, then PUT it.
    assert_before(
        "Blob partBytes = fetchRange",
        "updateFilesUploadSessionsById",
    );
}

#[test]
fn the_phases_run_in_protocol_order() {
    // create session → upload parts → commit, defined in that order.
    assert_before(
        "private void createSession()",
        "private void uploadNextPart()",
    );
    assert_before("private void uploadNextPart()", "private void commit()");
}

#[test]
fn the_source_slices_via_an_http_range_get() {
    // Slicing win: the source supplies exact bytes — no Apex Blob slice, so any
    // part_size works (no 3-byte-alignment restriction).
    assert_has("req.setHeader('Range', 'bytes=' + offset + '-' + (offset + len - 1));");
}

#[test]
fn it_guards_the_unsafe_cases_the_findings_call_out() {
    // A non-206 range fetch and an unusable part size are rejected up front.
    assert_has("res.getStatusCode() != 206");
    assert_has("this.partSize <= 0 || this.partSize > 8 * 1024 * 1024");
}

#[test]
fn the_whole_file_digest_is_an_off_platform_input() {
    // Apex can't stream a SHA-1; the commit digest is precomputed and passed in.
    assert_has("final String wholeFileSha1Base64");
    assert_has("'sha=' + this.wholeFileSha1Base64");
}
