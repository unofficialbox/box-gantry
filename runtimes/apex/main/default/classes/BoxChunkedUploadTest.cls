// Hand-written Box Apex runtime (TR-Apex.6) — tests for chunked upload.
//
// `HttpCalloutMock` stands in for the three-endpoint protocol (create session,
// PUT part, commit), so the slice/digest/range/commit orchestration is covered
// without a real Box org. A 9-byte payload with a 3-aligned part size of 6
// exercises the multi-part slicing path (parts of 6 and 3 bytes).
@isTest
private class BoxChunkedUploadTest {
    private class UploadMock implements HttpCalloutMock {
        public Integer sessionCalls = 0;
        public Integer partCalls = 0;
        public Integer commitCalls = 0;

        public HttpResponse respond(HttpRequest req) {
            HttpResponse res = new HttpResponse();
            res.setStatusCode(200);
            String endpoint = req.getEndpoint();
            if (req.getMethod() == 'POST' && endpoint.endsWith('/commit')) {
                this.commitCalls++;
                res.setBody('{"total_count":1,"entries":[{"id":"99","type":"file","name":"f.bin"}]}');
            } else if (req.getMethod() == 'POST' && endpoint.endsWith('/upload_sessions')) {
                this.sessionCalls++;
                res.setBody('{"id":"SESSION_1","type":"upload_session","part_size":6,"total_parts":2}');
            } else if (req.getMethod() == 'PUT') {
                this.partCalls++;
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
        System.assertEquals(2, mock.partCalls, 'two parts uploaded (6 + 3 bytes)');
        System.assertEquals(1, mock.commitCalls, 'one commit');
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
