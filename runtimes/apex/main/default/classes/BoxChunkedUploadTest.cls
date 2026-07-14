// Hand-written Box Apex runtime (TR-Apex.6) — tests for chunked upload.
//
// `HttpCalloutMock` stands in for the three-endpoint protocol (create session,
// PUT part, commit), so the slice/digest/range/commit orchestration is covered
// without a real Box org. A 9-byte payload with a 3-aligned part size of 6
// exercises the multi-part slicing path (parts of 6 and 3 bytes).
@isTest
private class BoxChunkedUploadTest {
    // Captures each part's Content-Range, Digest, and byte size so the test can
    // prove the helper slices/digests/ranges distinctly per part, and records
    // the commit body to check the ordered part list.
    private class UploadMock implements HttpCalloutMock {
        public Integer sessionCalls = 0;
        public Integer partCalls = 0;
        public Integer commitCalls = 0;
        public List<String> partRanges = new List<String>();
        public List<String> partDigests = new List<String>();
        public List<Integer> partSizes = new List<Integer>();
        public String commitBody;

        public HttpResponse respond(HttpRequest req) {
            HttpResponse res = new HttpResponse();
            res.setStatusCode(200);
            String endpoint = req.getEndpoint();
            if (req.getMethod() == 'POST' && endpoint.endsWith('/commit')) {
                this.commitCalls++;
                this.commitBody = req.getBody();
                res.setBody('{"total_count":1,"entries":[{"id":"99","type":"file","name":"f.bin"}]}');
            } else if (req.getMethod() == 'POST' && endpoint.endsWith('/upload_sessions')) {
                this.sessionCalls++;
                res.setBody('{"id":"SESSION_1","type":"upload_session","part_size":6,"total_parts":2}');
            } else if (req.getMethod() == 'PUT') {
                this.partCalls++;
                this.partRanges.add(req.getHeader('content-range'));
                this.partDigests.add(req.getHeader('digest'));
                Blob body = req.getBodyAsBlob();
                this.partSizes.add(body == null ? -1 : body.size());
                res.setBody(
                    '{"part":{"part_id":"P' +
                    this.partCalls +
                    '","offset":0,"size":6,"sha1":"abc"}}'
                );
            } else {
                res.setStatusCode(404);
                res.setBody('{}');
            }
            return res;
        }
    }

    private static Box newClient() {
        return new Box(new BoxHttpClient(new BoxDeveloperTokenProvider('tok')));
    }

    @isTest
    static void uploadsEveryPartThenCommits() {
        UploadMock mock = new UploadMock();
        Test.setMock(HttpCalloutMock.class, mock);
        Test.startTest();
        Files result = new BoxChunkedUpload(newClient())
            .upload(Blob.valueOf('123456789'), 'f.bin', '0');
        Test.stopTest();
        System.assertEquals(1, mock.sessionCalls, 'one session created');
        System.assertEquals(2, mock.partCalls, 'two parts uploaded');
        System.assertEquals(1, mock.commitCalls, 'one commit');
        // Each part carries its own byte range, size, and digest.
        System.assertEquals(new List<Integer>{ 6, 3 }, mock.partSizes, 'distinct part sizes');
        System.assertEquals('bytes 0-5/9', mock.partRanges[0], 'first part range');
        System.assertEquals('bytes 6-8/9', mock.partRanges[1], 'second part range');
        System.assertNotEquals(mock.partDigests[0], mock.partDigests[1], 'per-part digests differ');
        // The commit lists both parts, in order.
        Map<String, Object> commit = (Map<String, Object>) JSON.deserializeUntyped(mock.commitBody);
        System.assertEquals(2, ((List<Object>) commit.get('parts')).size(), 'both parts committed');
        System.assertNotEquals(null, result);
        System.assertEquals(1, result.entries.size(), 'the committed file is returned');
    }

    @isTest
    static void oversizeContentIsRejected() {
        BoxChunkedUpload uploader = new BoxChunkedUpload(newClient()).withMaxContentBytes(4);
        try {
            uploader.upload(Blob.valueOf('12345'), 'f.bin', '0');
            System.assert(false, 'content over the heap ceiling must throw');
        } catch (BoxApiException e) {
            System.assert(true);
        }
    }

    @isTest
    static void unalignedPartSizeIsRejected() {
        Test.setMock(HttpCalloutMock.class, new UnalignedMock());
        Test.startTest();
        try {
            new BoxChunkedUpload(newClient()).upload(Blob.valueOf('123456789'), 'f.bin', '0');
            System.assert(false, 'a part size not divisible by 3 must throw');
        } catch (BoxApiException e) {
            System.assert(true);
        }
        Test.stopTest();
    }

    private class UnalignedMock implements HttpCalloutMock {
        public HttpResponse respond(HttpRequest req) {
            HttpResponse res = new HttpResponse();
            res.setStatusCode(200);
            // part_size 5 (not a multiple of 3) with a 9-byte payload ⇒ multi-part.
            res.setBody('{"id":"S","type":"upload_session","part_size":5,"total_parts":2}');
            return res;
        }
    }

    @isTest
    static void aClientIsRequired() {
        try {
            new BoxChunkedUpload(null);
            System.assert(false, 'a null client must throw');
        } catch (BoxApiException e) {
            System.assert(true);
        }
    }
}
