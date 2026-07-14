// Hand-written Box Apex runtime (TR-Apex.6) — the chunked-upload orchestrator.
//
// Box's chunked upload is a three-step protocol: create an upload session, PUT
// each part with its byte range and a per-part SHA-1 digest, then commit with
// the whole-file SHA-1 and the ordered part list. The generated
// `BoxChunkedUploads` manager exposes each raw call; this helper ties them
// together — slicing, digesting, ranging, and committing — so a caller never
// hand-rolls the fiddly, integrity-checked glue.
//
// ── Apex platform limits (read before using) ────────────────────────────────
// Two hard constraints bound what Apex can do here, so the helper stays honest
// about them instead of failing obscurely:
//
//  1. **Heap.** The content is an in-memory `Blob`; Apex heap is 6 MB
//     synchronous / 12 MB asynchronous. Run chunked uploads from a `Queueable`
//     or `Batchable` for the larger budget. The helper rejects content beyond a
//     configurable ceiling (default ~4 MB, leaving room for the base64 slicing
//     buffer) with a clear `BoxApiException`, rather than a raw `LimitException`.
//     Files large enough to *need* Box's server part sizes therefore can't be
//     uploaded in a single transaction — a platform limit, not a bug.
//
//  2. **Slicing.** Apex has no `Blob` byte-slice. The workaround — substring the
//     base64 encoding at 4-char (3-byte) boundaries — only lands on a real byte
//     boundary when the session's `part_size` is a multiple of 3. A single-part
//     upload needs no slicing; a multi-part upload whose `part_size` isn't
//     3-aligned is rejected with a clear message.
public with sharing class BoxChunkedUpload {
    // Default max in-heap content, well under the 12 MB async heap once the
    // base64 slicing buffer (~1.33x) and a part copy are accounted for.
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
        PostFilesUploadSessionsBody request = new PostFilesUploadSessionsBody();
        request.file_name = fileName;
        request.file_size = content.size();
        request.folder_id = folderId;
        return run(this.client.chunkedUploads.createFilesUploadSessions(request), content);
    }

    /** Upload `content` as a new version of the existing file `fileId`. */
    public Files uploadVersion(Blob content, String fileName, String fileId) {
        guard(content, fileName);
        if (String.isBlank(fileId)) {
            throw new BoxApiException('A target file id is required.');
        }
        PostFilesIdUploadSessionsBody request = new PostFilesIdUploadSessionsBody();
        request.file_name = fileName;
        request.file_size = content.size();
        return run(
            this.client.chunkedUploads.createFilesByIdUploadSessions(fileId, request),
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

    // Slice → digest → PUT each part, then commit with the whole-file digest.
    private Files run(UploadSession session, Blob content) {
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
            UploadedPart uploaded = this.client.chunkedUploads.updateFilesUploadSessionsById(
                session.id,
                digest,
                contentRange,
                partBlob
            );
            parts.add(uploaded.part);
            offset += len;
        }

        String wholeDigest = 'sha=' + EncodingUtil.base64Encode(Crypto.generateDigest('SHA1', content));
        PostFilesUploadSessionsIdCommitBody commit = new PostFilesUploadSessionsIdCommitBody();
        commit.parts = parts;
        return this.client.chunkedUploads.commitFilesUploadSessionsById(
            session.id,
            wholeDigest,
            null,
            null,
            commit
        );
    }
}
