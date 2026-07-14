// SPIKE — throwaway prototype. NOT shipped, NOT deployed, NOT in the runtime.
//
// Explores whether a Queueable-chained uploader makes a REAL Box chunked upload
// feasible on Apex, sidestepping the two limits that block the in-transaction
// helper (`BoxChunkedUpload`, D-136): the 6/12 MB heap and the absence of a
// `Blob` byte-slice.
//
// Idea — one part per transaction. Each `execute()` fetches exactly one part's
// bytes from a range-readable source (an HTTP `Range` GET), PUTs it to Box,
// records the returned part, and enqueues the next execution. So:
//
//   • only one part (≤ the session `part_size`, ~8 MB) is ever in the 12 MB
//     async heap — the whole-file-in-heap limit disappears; and
//   • the *source* supplies exact part bytes, so there is no Apex `Blob` slicing
//     and therefore no 3-byte-alignment restriction — the second D-136 limit
//     disappears too.
//
// Two things still can't happen on-platform, so they are INPUTS, not work:
//
//   1. The whole-file SHA-1 the commit wants. Apex has no streaming/incremental
//      digest, and the file never sits in heap whole, so it must be precomputed
//      off-platform and passed in — OR the commit `Digest` header must be
//      optional (OPEN QUESTION: confirm against the Box API; each part is
//      already sha1-verified on its PUT).
//   2. A range-readable source. An external URL with `Accept-Ranges: bytes`
//      works; a raw Salesforce `ContentVersion` does not — querying `VersionData`
//      loads the entire `Blob` into heap. Pre-chunked per-part records are the
//      alternative.
public with sharing class BoxChunkedUploadJob implements Queueable, Database.AllowsCallouts {
    // Inputs (immutable across the chain).
    private final Box client;
    private final String sourceUrl; // range-readable source of the file bytes
    private final String fileName;
    private final Long fileSize;
    private final String folderId;
    private final String wholeFileSha1Base64; // precomputed off-platform (see note 1)

    // Chain state — mutates across executions, serialized between them.
    private String sessionId;
    private Long partSize;
    private Long nextOffset = 0;
    private List<UploadPart> parts = new List<UploadPart>();

    public BoxChunkedUploadJob(
        Box client,
        String sourceUrl,
        String fileName,
        Long fileSize,
        String folderId,
        String wholeFileSha1Base64
    ) {
        this.client = client;
        this.sourceUrl = sourceUrl;
        this.fileName = fileName;
        this.fileSize = fileSize;
        this.folderId = folderId;
        this.wholeFileSha1Base64 = wholeFileSha1Base64;
    }

    public void execute(QueueableContext context) {
        if (this.sessionId == null) {
            createSession();
        } else if (this.nextOffset < this.fileSize) {
            uploadNextPart();
        } else {
            commit();
            return; // the chain ends here
        }
        // One child per execution — the chain. Each link is a fresh transaction
        // with its own 12 MB heap and 100-callout budget.
        System.enqueueJob(this);
    }

    private void createSession() {
        PostFilesUploadSessionsBody body = new PostFilesUploadSessionsBody();
        body.file_name = this.fileName;
        body.file_size = this.fileSize;
        body.folder_id = this.folderId;
        UploadSession session = this.client.chunkedUploads.createFilesUploadSessions(body);
        this.sessionId = session.id;
        this.partSize = session.part_size;
    }

    private void uploadNextPart() {
        Long len = Math.min(this.partSize, this.fileSize - this.nextOffset);
        Blob partBytes = fetchRange(this.nextOffset, len); // ≤ part_size, fits async heap
        String digest = 'sha=' + EncodingUtil.base64Encode(Crypto.generateDigest('SHA1', partBytes));
        String range = 'bytes ' + this.nextOffset + '-' + (this.nextOffset + len - 1) + '/' + this.fileSize;
        UploadedPart uploaded = this.client.chunkedUploads.updateFilesUploadSessionsById(
            this.sessionId,
            digest,
            range,
            partBytes
        );
        this.parts.add(uploaded.part);
        this.nextOffset += len;
    }

    // Fetch exactly [offset, offset+len) from the source — the source, not Apex,
    // does the slicing. Expect 206 Partial Content; a 200 means the source
    // ignored `Range` and would drop the whole file in heap (the real impl must
    // detect that and abort).
    private Blob fetchRange(Long offset, Long len) {
        HttpRequest req = new HttpRequest();
        req.setEndpoint(this.sourceUrl);
        req.setMethod('GET');
        req.setHeader('Range', 'bytes=' + offset + '-' + (offset + len - 1));
        HttpResponse res = new Http().send(req);
        return res.getBodyAsBlob();
    }

    private void commit() {
        PostFilesUploadSessionsIdCommitBody body = new PostFilesUploadSessionsIdCommitBody();
        body.parts = this.parts;
        // Whole-file digest precomputed off-platform — Apex can't stream a SHA-1.
        this.client.chunkedUploads.commitFilesUploadSessionsById(
            this.sessionId,
            'sha=' + this.wholeFileSha1Base64,
            null,
            null,
            body
        );
    }
}
