// Hand-written Box Apex runtime (TR-Apex.6) — BoxRepresentations through the
// generated `files.getRepresentationContent` and the real `BoxHttpClient`.
@isTest
private class BoxRepresentationsTest {
    // Answers like Box: GET /files/1 offers the representation in `initial`
    // state; its info URL answers `polled`; the content URL serves the bytes.
    private class RepMock implements HttpCalloutMock {
        private final String initial;
        private final String polled;
        public Integer infoCalls = 0;

        RepMock(String initial, String polled) {
            this.initial = initial;
            this.polled = polled;
        }

        public HttpResponse respond(HttpRequest req) {
            HttpResponse res = new HttpResponse();
            res.setStatusCode(200);
            String endpoint = req.getEndpoint();
            if (endpoint.contains('/files/1?')) {
                System.assert(endpoint.contains('fields=representations'), endpoint);
                System.assertEquals('[extracted_text]', req.getHeader('x-rep-hints'));
                res.setBody(entry(initial, true));
            } else if (endpoint.endsWith('/info')) {
                infoCalls++;
                res.setBody(entry(polled, false));
            } else if (endpoint == 'https://dl.boxcloud.com/rep/content') {
                res.setBodyAsBlob(Blob.valueOf('extracted words'));
            } else {
                res.setStatusCode(404);
                res.setBody('{"type":"error","status":404,"code":"not_found"}');
            }
            return res;
        }

        private String entry(String state, Boolean wrapped) {
            String rep =
                '{"representation":"extracted_text","info":{"url":"https://api.box.com/2.0/rep/info"},' +
                '"status":{"state":"' + state + '","code":"' + state + '_code"},' +
                '"content":{"url_template":"https://dl.boxcloud.com/rep/{+asset_path}"}}';
            return wrapped ? '{"id":"1","type":"file","representations":{"entries":[' + rep + ']}}' : rep;
        }
    }

    private static Box client() {
        return new Box(new BoxHttpClient(new BoxDeveloperTokenProvider('t')));
    }

    @isTest
    static void readyContentIsDownloaded() {
        RepMock mock = new RepMock('success', 'success');
        Test.setMock(HttpCalloutMock.class, mock);
        Test.startTest();
        BoxRepresentations.Result result = client().files.getRepresentationContent('1', '[extracted_text]', 'content');
        Test.stopTest();
        System.assertEquals('success', result.state);
        System.assertEquals('extracted words', result.content.toString());
        System.assertEquals(0, mock.infoCalls, 'a ready representation needs no status check');
    }

    @isTest
    static void pendingIsCheckedOnceThenDownloaded() {
        RepMock mock = new RepMock('pending', 'success');
        Test.setMock(HttpCalloutMock.class, mock);
        Test.startTest();
        BoxRepresentations.Result result = client().files.getRepresentationContent('1', '[extracted_text]', 'content');
        Test.stopTest();
        System.assertEquals(1, mock.infoCalls);
        System.assertEquals('extracted words', result.content.toString());
    }

    @isTest
    static void stillPendingIsReported() {
        Test.setMock(HttpCalloutMock.class, new RepMock('none', 'pending'));
        Test.startTest();
        BoxRepresentations.Result result = client().files.getRepresentationContent('1', '[extracted_text]', 'content');
        Test.stopTest();
        System.assertEquals('pending', result.state);
        System.assertEquals(null, result.content);
    }

    @isTest
    static void anErrorStateThrows() {
        Test.setMock(HttpCalloutMock.class, new RepMock('error', 'error'));
        Test.startTest();
        try {
            client().files.getRepresentationContent('1', '[extracted_text]', '');
            System.assert(false, 'an error state should throw');
        } catch (BoxApiException e) {
            System.assert(e.getMessage().contains('error_code'), e.getMessage());
        }
        Test.stopTest();
    }
}
