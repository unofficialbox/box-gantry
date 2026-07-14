// Hand-written Box Apex runtime (TR-Apex.6) — tests for the JWT provider.
//
// `Crypto.signWithCertificate` needs a real key in the org, so the assertion
// signing is a `virtual` seam the test overrides with a fixed signature; that
// lets `HttpCalloutMock` cover the assertion build / exchange / cache /
// invalidate / error paths without any org certificate.
@isTest
private class BoxJwtTokenProviderTest {
    // A JWT provider whose signature is fixed, so no org certificate is needed.
    private class StubbedJwtProvider extends BoxJwtTokenProvider {
        StubbedJwtProvider() {
            super('id', 'secret', 'enterprise', '12345', 'kid_1', 'BoxAppKey');
        }
        protected override String sign(String signingInput) {
            return 'STUB_SIGNATURE';
        }
    }

    private class TokenMock implements HttpCalloutMock {
        public Integer calls = 0;
        public Integer statusCode = 200;
        public String body = '{"access_token":"tok_jwt","expires_in":3600,"token_type":"bearer"}';
        public String lastBody;
        public HttpResponse respond(HttpRequest req) {
            this.calls++;
            this.lastBody = req.getBody();
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
        BoxJwtTokenProvider provider = new StubbedJwtProvider();
        Test.startTest();
        String first = provider.getAccessToken();
        String second = provider.getAccessToken();
        Test.stopTest();
        System.assertEquals('tok_jwt', first);
        System.assertEquals('tok_jwt', second);
        System.assertEquals(1, mock.calls, 'the second read must come from cache');
        // The exchange used the JWT bearer grant and carried an assertion.
        System.assert(
            mock.lastBody.contains('grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer'),
            'the JWT bearer grant must be used'
        );
        System.assert(mock.lastBody.contains('assertion='), 'the assertion must be sent');
    }

    @isTest
    static void invalidateForcesReMint() {
        TokenMock mock = new TokenMock();
        Test.setMock(HttpCalloutMock.class, mock);
        BoxJwtTokenProvider provider = new StubbedJwtProvider();
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
        mock.body = '{"error":"invalid_grant"}';
        Test.setMock(HttpCalloutMock.class, mock);
        BoxJwtTokenProvider provider = new StubbedJwtProvider();
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
            new BoxJwtTokenProvider('id', 'secret', 'enterprise', '1', '', 'BoxAppKey');
            System.assert(false, 'a blank public key id must throw');
        } catch (BoxApiException e) {
            System.assert(true);
        }
    }
}
