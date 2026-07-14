// Hand-written Box Apex runtime (TR-Apex.6) — tests for the CCG provider.
//
// Uses `HttpCalloutMock` (the only way Apex exercises callout code) to cover
// the mint / cache / invalidate / error paths without a real token endpoint.
@isTest
private class BoxCcgTokenProviderTest {
    // Returns a fixed token, and counts how many callouts were actually made so
    // a test can prove caching skips the network.
    private class TokenMock implements HttpCalloutMock {
        public Integer calls = 0;
        public Integer statusCode = 200;
        public String body = '{"access_token":"tok_abc","expires_in":3600,"token_type":"bearer"}';
        public HttpResponse respond(HttpRequest req) {
            this.calls++;
            HttpResponse res = new HttpResponse();
            res.setStatusCode(this.statusCode);
            res.setBody(this.body);
            return res;
        }
    }

    @isTest
    static void mintsThenCaches() {
        TokenMock mock = new TokenMock();
        Test.setMock(HttpCalloutMock.class, mock);
        BoxCcgTokenProvider provider = new BoxCcgTokenProvider(
            'id',
            'secret',
            'enterprise',
            '12345'
        );
        Test.startTest();
        String first = provider.getAccessToken();
        String second = provider.getAccessToken();
        Test.stopTest();
        System.assertEquals('tok_abc', first);
        System.assertEquals('tok_abc', second);
        System.assertEquals(1, mock.calls, 'the second read must come from cache');
    }

    @isTest
    static void invalidateForcesReMint() {
        TokenMock mock = new TokenMock();
        Test.setMock(HttpCalloutMock.class, mock);
        BoxCcgTokenProvider provider = new BoxCcgTokenProvider('id', 'secret', 'user', '99');
        Test.startTest();
        provider.getAccessToken();
        provider.invalidate();
        provider.getAccessToken();
        Test.stopTest();
        System.assertEquals(2, mock.calls, 'invalidate must force a fresh callout');
    }

    @isTest
    static void nonSuccessThrows() {
        TokenMock mock = new TokenMock();
        mock.statusCode = 400;
        mock.body = '{"error":"invalid_client"}';
        Test.setMock(HttpCalloutMock.class, mock);
        BoxCcgTokenProvider provider = new BoxCcgTokenProvider('id', 'bad', 'enterprise', '1');
        Test.startTest();
        try {
            provider.getAccessToken();
            System.assert(false, 'a non-2xx token response must throw');
        } catch (BoxApiException e) {
            System.assert(true);
        }
        Test.stopTest();
    }

    @isTest
    static void blankArgumentsThrow() {
        try {
            new BoxCcgTokenProvider('', 'secret', 'enterprise', '1');
            System.assert(false, 'a blank client id must throw');
        } catch (BoxApiException e) {
            System.assert(true);
        }
    }
}
