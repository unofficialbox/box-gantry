// Hand-written Box Apex runtime (TR-Apex) — tests for the auth facade.
//
// Each factory returns the matching BoxTokenProvider; construction validates
// its arguments (the providers throw on blanks), so building one with valid
// arguments proves the facade wires the flow through.
@isTest
private class BoxAuthTest {
    @isTest
    static void developerTokenBuildsAProvider() {
        BoxTokenProvider p = BoxAuth.developerToken('DEV_TOKEN');
        System.assertNotEquals(null, p, 'developer token provider');
    }

    @isTest
    static void clientCredentialsBuildsAProvider() {
        BoxTokenProvider p = BoxAuth.clientCredentials(
            'CLIENT_ID',
            'CLIENT_SECRET',
            'enterprise',
            'ENTERPRISE_ID'
        );
        System.assertNotEquals(null, p, 'CCG provider');
    }

    @isTest
    static void jwtBuildsAProvider() {
        BoxTokenProvider p = BoxAuth.jwt(
            'CLIENT_ID',
            'CLIENT_SECRET',
            'enterprise',
            'ENTERPRISE_ID',
            'PUBLIC_KEY_ID',
            'Box_JWT'
        );
        System.assertNotEquals(null, p, 'JWT provider');
    }

    // Answers the token endpoint like Box: a downscoped token, or an OAuth 400
    // when the form carries no scope. Records the last request.
    private class TokenEndpointMock implements HttpCalloutMock {
        public HttpRequest last;
        public HttpResponse respond(HttpRequest req) {
            this.last = req;
            HttpResponse res = new HttpResponse();
            res.setHeader('Content-Type', 'application/json');
            if (req.getBody().contains('scope=item_preview')) {
                res.setStatusCode(200);
                res.setBody(
                    '{"access_token":"down","expires_in":3600,"restricted_to":' +
                    '[{"scope":"item_preview","object":{"type":"file","id":"1"}}]}'
                );
            } else {
                res.setStatusCode(400);
                res.setBody('{"error":"invalid_scope","error_description":"no scope"}');
            }
            return res;
        }
    }

    // The generated downscopeToken posts Box's token-exchange form (not JSON)
    // through the real BoxHttpClient, and an OAuth error throws with its code.
    @isTest
    static void downscopeTokenExchangesTheSubjectTokenAsAForm() {
        TokenEndpointMock mock = new TokenEndpointMock();
        Test.setMock(HttpCalloutMock.class, mock);
        Box client = new Box(new BoxHttpClient(BoxAuth.developerToken('parent')));
        String resource = 'https://api.box.com/2.0/files/1';

        Test.startTest();
        AccessToken token = client.authorization.downscopeToken(
            'parent',
            new List<String>{ 'item_preview', 'item_rename' },
            resource,
            null
        );
        System.assertEquals('down', token.access_token, 'the downscoped token');
        System.assertEquals(1, token.restricted_to.size(), 'restricted_to');
        System.assertEquals(
            'application/x-www-form-urlencoded',
            mock.last.getHeader('Content-Type'),
            'a form, not JSON'
        );
        String form = mock.last.getBody();
        System.assert(form.contains('grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange'), form);
        System.assert(form.contains('subject_token=parent'), form);
        System.assert(form.contains('subject_token_type=urn%3Aietf%3Aparams%3Aoauth%3Atoken-type%3Aaccess_token'), form);
        System.assert(form.contains('scope=item_preview+item_rename'), form);
        System.assert(form.contains('resource=' + EncodingUtil.urlEncode(resource, 'UTF-8')), form);
        System.assert(!form.contains('box_shared_link'), form);

        try {
            client.authorization.downscopeToken('parent', new List<String>(), null, null);
            System.assert(false, 'an exchange with no scope should throw');
        } catch (BoxApiException e) {
            System.assertEquals(400, e.statusCode, 'the 400 is carried');
            System.assertEquals('invalid_scope', e.code, 'the OAuth error code is parsed');
        }
        Test.stopTest();
    }
}
