// Hand-written Box Apex runtime (TR-Apex.6) — the etag flow from Box's "Ensure
// consistency" guide, through the generated `BoxFiles` manager and the real
// `BoxHttpClient`.
//
// `If-None-Match` on a GET answers 304 when the item is unchanged, `If-Match`
// on a write answers 412 when it changed since, and a missing item answers 404.
// Each is a non-2xx that must throw `BoxApiException` carrying its status and
// Box code, never come back as a decoded "result". The other four SDKs gate
// the same flow in their compile gates.
@isTest
private class BoxConsistencyTest {
    // Answers like Box: GET /files/1 honors If-None-Match "etag-1" with a 304,
    // PUT /files/1 requires If-Match "etag-1" (else 412), and anything else is
    // a 404.
    private class EtagMock implements HttpCalloutMock {
        public HttpResponse respond(HttpRequest req) {
            HttpResponse res = new HttpResponse();
            Boolean fileOne = req.getEndpoint().endsWith('/files/1');
            if (req.getMethod() == 'GET' && fileOne) {
                if (req.getHeader('if-none-match') == 'etag-1') {
                    res.setStatusCode(304);
                    res.setBody('');
                } else {
                    res.setStatusCode(200);
                    res.setBody('{"id":"1","type":"file","etag":"etag-1"}');
                }
            } else if (req.getMethod() == 'PUT' && fileOne) {
                if (req.getHeader('if-match') == 'etag-1') {
                    res.setStatusCode(200);
                    res.setBody('{"id":"1","type":"file","etag":"etag-2"}');
                } else {
                    boxError(res, 412, 'precondition_failed');
                }
            } else {
                boxError(res, 404, 'not_found');
            }
            return res;
        }
    }

    // A Box error body, as Box sends with a 4xx.
    private static void boxError(HttpResponse res, Integer status, String code) {
        res.setStatusCode(status);
        res.setBody(JSON.serialize(new Map<String, Object>{
            'type' => 'error',
            'status' => status,
            'code' => code,
            'message' => code,
            'request_id' => 'req-' + code
        }));
    }

    @isTest
    static void generatedMethodsSurfaceConsistencyOutcomesAsApiErrors() {
        Test.setMock(HttpCalloutMock.class, new EtagMock());
        Box client = new Box(new BoxHttpClient(new BoxDeveloperTokenProvider('t')));

        Test.startTest();
        // A plain read succeeds and yields the etag to send back.
        FileFull file = client.files.get('1', null, null, null, null);
        System.assertEquals('etag-1', file.etag, 'the read yields the etag');

        // If-None-Match with the current etag: 304, as an exception.
        try {
            client.files.get('1', null, file.etag, null, null);
            System.assert(false, 'a 304 must throw');
        } catch (BoxApiException e) {
            System.assertEquals(304, e.statusCode, 'the 304 is carried');
            System.assertEquals(null, e.code, 'a 304 has no body');
        }

        // If-Match with a stale etag: 412 precondition_failed.
        try {
            client.files.update_r('1', null, 'etag-0', new UpdateFileRequest());
            System.assert(false, 'a stale If-Match must throw');
        } catch (BoxApiException e) {
            System.assertEquals(412, e.statusCode, 'the 412 is carried');
            System.assertEquals('precondition_failed', e.code, 'Box\'s code is parsed');
            System.assertEquals('req-precondition_failed', e.requestId, 'the request id is parsed');
        }

        // If-Match with the current etag: the write goes through.
        FileFull updated = client.files.update_r('1', null, file.etag, new UpdateFileRequest());
        System.assertEquals('etag-2', updated.etag, 'the write returns the new etag');

        // A missing item: 404 not_found, also from a method with no response body.
        try {
            client.files.delete_r('2', null);
            System.assert(false, 'a 404 must throw');
        } catch (BoxApiException e) {
            System.assertEquals(404, e.statusCode, 'the 404 is carried');
            System.assertEquals('not_found', e.code, 'Box\'s code is parsed');
        }
        Test.stopTest();
    }
}
