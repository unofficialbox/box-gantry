//! Pin the design properties that make the Queueable upload work — so the
//! finding can't silently rot if the sketch is edited.

use apex_queueable_upload_spike::SKETCH;

fn assert_has(needle: &str) {
    assert!(SKETCH.contains(needle), "sketch must contain: {needle}");
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
}

#[test]
fn the_source_slices_via_an_http_range_get() {
    // Slicing win: the source supplies exact bytes — no Apex Blob slice, so any
    // part_size works (no 3-byte-alignment restriction).
    assert_has("req.setHeader('Range', 'bytes=' + offset + '-' + (offset + len - 1));");
}

#[test]
fn the_whole_file_digest_is_an_off_platform_input() {
    // Apex can't stream a SHA-1; the commit digest is precomputed and passed in.
    assert_has("final String wholeFileSha1Base64");
    assert_has("'sha=' + this.wholeFileSha1Base64");
}
