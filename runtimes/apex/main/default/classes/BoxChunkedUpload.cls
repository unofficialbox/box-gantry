// Hand-written Box Apex runtime (TR-Apex.6) — the chunked-upload orchestrator.
//
// ⚠️ NOT USABLE FOR A REAL BOX CHUNKED UPLOAD ON APEX TODAY. Two independent
// platform limits make an *actual* end-to-end chunked upload impossible in a
// single Apex transaction, so treat this as a **correct reference
// implementation of the protocol** — verified against mocked callouts — rather
// than a production upload path. The real path needs an out-of-transaction,
// `Queueable`-chained design (one part per transaction) *and* a byte-accurate
// slicing mechanism; both are tracked as follow-ups (D-136).
//
//   • **Heap (the airtight blocker).** The content is an in-memory `Blob`, and
//     Apex heap is 6 MB sync / 12 MB async — but Box only *offers* chunked
//     upload for files ≥ 20 MB. No file is both ≥ 20 MB (so Box accepts it) and
//     ≤ ~12 MB (so it fits heap), so there is no size at which a real upload
//     succeeds — independent of everything below.
//   • **Slicing.** Apex has no `Blob` byte-slice; the base64-substring workaround
//     only lands on a real byte boundary at a 3-byte-aligned `part_size`. Box
//     sets `part_size` from the session (not documented to be any particular
//     value), but the sizes it issues in practice — e.g. 8 MB — are powers of
//     two, which aren't divisible by 3, so the multi-part path rejects those.
//
// What it *does* do, correctly, for in-heap content with a 3-aligned part size
// (the mocked-test scenario): Box's three-step protocol — create an upload
// session, PUT each part with its byte range + per-part SHA-1 digest, then
// commit with the whole-file SHA-1 and the ordered part list — over the
// generated `BoxChunkedUploads` manager, failing loudly (a `BoxApiException`,
// not a raw `LimitException`) at each limit above.
public with sharing class BoxChunkedUpload {
    // The largest content this helper will attempt to hold in heap. Real Box
    // chunked uploads (≥ 20 MB) exceed it by construction — see the class note.
    private static final Integer DEFAULT_MAX_CONTENT_BYTES = 4 * 1024 * 1024;

    private final Box client;
    private Integer maxContentBytes = DEFAULT_MAX_CONTENT_BYTES;

    public BoxChunkedUpload(Box client) {
        if (client == null) {
            throw new BoxApiException('A Box client is required.');
        }
        this.client = client;
    }

    /** Raise/lower the in-heap content ceiling (bytes). */
    public BoxChunkedUpload withMaxContentBytes(Integer maxContentBytes) {
        if (maxContentBytes != null && maxContentBytes > 0) {
            this.maxContentBytes = maxContentBytes;
        }
        return this;
    }

    /** Upload `content` as a new file named `fileName` into `folderId`. */
    public Files upload(Blob content, String fileName, String folderId) {
        guard(content, fileName);
        if (String.isBlank(folderId)) {
            throw new BoxApiException('A destination folder id is required.');
        }
        FileUploadSessionsCreateRequest request = new FileUploadSessionsCreateRequest();
        request.file_name = fileName;
        request.file_size = content.size();
        request.folder_id = folderId;
        return run(this.client.chunkedUploads.createFileUploadSessions(request), content);
    }

    /** Upload `content` as a new version of the existing file `fileId`. */
    public Files uploadVersion(Blob content, String fileName, String fileId) {
        guard(content, fileName);
        if (String.isBlank(fileId)) {
            throw new BoxApiException('A target file id is required.');
        }
        FileIdUploadSessionsCreateRequest request = new FileIdUploadSessionsCreateRequest();
        request.file_name = fileName;
        request.file_size = content.size();
        return run(
            this.client.chunkedUploads.createFileByIdUploadSessions(fileId, request),
            content
        );
    }

    private void guard(Blob content, String fileName) {
        if (content == null || content.size() == 0) {
            throw new BoxApiException('Upload content must be non-empty.');
        }
        if (String.isBlank(fileName)) {
            throw new BoxApiException('A file name is required.');
        }
        if (content.size() > this.maxContentBytes) {
            throw new BoxApiException(
                'Content of ' + content.size() + ' bytes exceeds the ' + this.maxContentBytes +
                '-byte in-heap ceiling; run from an async context and/or raise' +
                ' withMaxContentBytes, but Apex heap (12 MB async) caps single-transaction uploads.'
            );
        }
    }

    // Any failure after the session is created leaves an orphaned session (and
    // its already-uploaded parts) consuming quota until Box expires it. Abort it
    // best-effort on failure, without letting a cleanup error mask the original.
    private Files run(UploadSession session, Blob content) {
        try {
            return attempt(session, content);
        } catch (Exception failure) {
            try {
                this.client.chunkedUploads.deleteFileUploadSession(session.id);
            } catch (Exception ignored) {
                // best-effort cleanup — keep the original failure
            }
            throw failure;
        }
    }

    // Slice → digest → PUT each part, then commit with the whole-file digest.
    private Files attempt(UploadSession session, Blob content) {
        Integer total = content.size();
        Long partSize = session.part_size;
        if (partSize == null || partSize <= 0) {
            throw new BoxApiException('The upload session returned no usable part size.');
        }
        Boolean multiPart = total > partSize;
        // Remainder without Math.mod's Long/Integer overload ambiguity.
        if (multiPart && (partSize - (partSize / 3) * 3) != 0) {
            throw new BoxApiException(
                'The session part size (' + partSize + ') is not a multiple of 3, which Apex' +
                ' cannot slice a Blob on. Chunked upload is limited to 3-byte-aligned part sizes.'
            );
        }
        // Only the multi-part path needs the base64 slicing buffer.
        String encoded = multiPart ? EncodingUtil.base64Encode(content) : null;

        List<UploadPart> parts = new List<UploadPart>();
        Integer offset = 0;
        while (offset < total) {
            // partSize < remaining ⇒ a full part (partSize < total ≤ heap cap,
            // so it fits an Integer); otherwise the final, shorter part.
            Integer remaining = total - offset;
            Integer len = (partSize < remaining) ? (Integer) partSize : remaining;
            Blob partBlob;
            if (encoded == null) {
                partBlob = content;
            } else {
                Integer startChar = (offset / 3) * 4;
                Integer endChar = (offset + len == total)
                    ? encoded.length()
                    : ((offset + len) / 3) * 4;
                partBlob = EncodingUtil.base64Decode(encoded.substring(startChar, endChar));
            }
            String digest = 'sha=' + EncodingUtil.base64Encode(Crypto.generateDigest('SHA1', partBlob));
            String contentRange = 'bytes ' + offset + '-' + (offset + len - 1) + '/' + total;
            UploadedPart uploaded = this.client.chunkedUploads.updateFileUploadSession(
                session.id,
                digest,
                contentRange,
                partBlob
            );
            parts.add(uploaded.part);
            offset += len;
        }

        String wholeDigest = 'sha=' + EncodingUtil.base64Encode(Crypto.generateDigest('SHA1', content));
        // `commit` is an Apex reserved word (the DML statement), so the local
        // that holds the commit body is named `commitBody`.
        FileUploadSessionCommitRequest commitBody = new FileUploadSessionCommitRequest();
        commitBody.parts = parts;
        return this.client.chunkedUploads.commitFileUploadSession(
            session.id,
            wholeDigest,
            null,
            null,
            commitBody
        );
    }
}
