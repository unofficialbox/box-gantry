// Hand-written Box Apex runtime (TR-Apex.6) — tests for the HTTP client.
//
// `BoxHttpClient` is the one runtime class that turns a structural `BoxRequest`
// into a real Salesforce callout, so its behaviour — URL building, the bearer
// token, transient retries, the single 401 refresh, the bounded retry budget,
// and mapping a non-2xx response to `BoxApiException` — is what every generated
// manager depends on. `HttpCalloutMock` is the only way Apex exercises callout
// code; these tests drive each path through it.
//
// Note: Apex's `HttpRequest` exposes no `getHeader()`, so the `Authorization`
// header can't be read back off a captured request. Auth/refresh behaviour is
// asserted instead through the token provider's own call counters — the client
// reads a token per attempt and calls `invalidate()` before its 401 re-attempt.
@isTest
private class BoxHttpClientTest {
    // Replays a fixed sequence of (status, body) responses — one per callout —
    // and records every request it saw, so a test can assert how many callouts
    // happened and what URL/method/body the client built.
    private class SequenceMock implements HttpCalloutMock {
        private List<Integer> statuses;
        private List<String> bodies;
        private Integer next = 0;
        public List<HttpRequest> seen = new List<HttpRequest>();

        public SequenceMock(List<Integer> statuses, List<String> bodies) {
            this.statuses = statuses;
            this.bodies = bodies;
        }

        public HttpResponse respond(HttpRequest req) {
            this.seen.add(req);
            // Past the end of the script, keep returning the last response.
            Integer idx = (this.next < this.statuses.size())
                ? this.next
                : this.statuses.size() - 1;
            this.next++;
            HttpResponse res = new HttpResponse();
            res.setStatusCode(this.statuses[idx]);
            res.setBody(this.bodies[idx]);
            return res;
        }
    }

    // A token provider that counts reads and invalidations, and hands out a
    // fresh token string after each invalidate so a refresh is observable.
    private class CountingTokens implements BoxTokenProvider {
        public Integer reads = 0;
        public Integer invalidations = 0;
        private Integer generation = 1;

        public String getAccessToken() {
            this.reads++;
            return 'tok_' + this.generation;
        }

        public void invalidate() {
            this.invalidations++;
            this.generation++;
        }
    }

    // A typed request body with a nullable field — the shape the default
    // (suppressNulls = true) path always receives. Generated managers only set
    // `request.body` directly (leaving suppression on) for a typed model object;
    // a raw Map only ever reaches the runtime via denormalizeKeys, which sets
    // suppressNulls = false. `JSON.serialize(apexObject, true)` drops null Apex
    // object fields, which is what "an unset field stays absent" means here.
    private class TypedBody {
        public String keep;
        public String drop;
    }

    // Build a bare request; a test fills in query/body/headers as needed.
    private static BoxRequest request(String method, String baseUrl, String path) {
        BoxRequest r = new BoxRequest();
        r.method = method;
        r.baseUrl = baseUrl;
        r.path = path;
        return r;
    }

    @isTest
    static void successReturnsResponseAndBuildsUrl() {
        SequenceMock mock = new SequenceMock(
            new List<Integer>{ 200 },
            new List<String>{ '{"id":"1"}' }
        );
        Test.setMock(HttpCalloutMock.class, mock);
        CountingTokens tokens = new CountingTokens();

        BoxRequest req = request('GET', 'api', '/users/me');
        req.query.put('fields', 'name');

        Test.startTest();
        BoxResponse res = new BoxHttpClient(tokens).send(req);
        Test.stopTest();

        System.assertEquals(200, res.statusCode, 'the 2xx status is returned');
        System.assertEquals('{"id":"1"}', res.body, 'the body is passed through');
        System.assertEquals(1, mock.seen.size(), 'exactly one callout for a 2xx');
        System.assertEquals(
            'https://api.box.com/2.0/users/me?fields=name',
            mock.seen[0].getEndpoint(),
            'base-URL class + path + encoded query'
        );
        System.assertEquals('GET', mock.seen[0].getMethod(), 'the method is set');
        System.assertEquals(1, tokens.reads, 'the token is read once for one attempt');
        System.assertEquals(0, tokens.invalidations, 'no refresh on success');
    }

    @isTest
    static void refreshesOnceOn401ThenSucceeds() {
        SequenceMock mock = new SequenceMock(
            new List<Integer>{ 401, 200 },
            new List<String>{ '{"error":"expired"}', '{"ok":true}' }
        );
        Test.setMock(HttpCalloutMock.class, mock);
        CountingTokens tokens = new CountingTokens();

        Test.startTest();
        BoxResponse res = new BoxHttpClient(tokens).send(request('GET', 'api', '/me'));
        Test.stopTest();

        System.assertEquals(200, res.statusCode, 'the retry after refresh succeeds');
        System.assertEquals(2, mock.seen.size(), 'one 401, then one refreshed attempt');
        System.assertEquals(1, tokens.invalidations, 'the 401 discards the stale token');
        System.assertEquals(2, tokens.reads, 'a token is minted for each attempt');
    }

    @isTest
    static void persistent401Throws() {
        SequenceMock mock = new SequenceMock(
            new List<Integer>{ 401, 401 },
            new List<String>{ '{"error":"nope"}', '{"error":"nope"}' }
        );
        Test.setMock(HttpCalloutMock.class, mock);
        CountingTokens tokens = new CountingTokens();

        Test.startTest();
        try {
            new BoxHttpClient(tokens).send(request('GET', 'api', '/me'));
            System.assert(false, 'a persistent 401 must throw');
        } catch (BoxApiException e) {
            System.assertEquals(401, e.statusCode, 'the error carries the 401');
        }
        Test.stopTest();

        System.assertEquals(2, mock.seen.size(), 'one refresh re-attempt, then give up');
        System.assertEquals(1, tokens.invalidations, 'the refresh is attempted exactly once');
    }

    @isTest
    static void retriesTransient500ThenSucceeds() {
        SequenceMock mock = new SequenceMock(
            new List<Integer>{ 500, 200 },
            new List<String>{ 'oops', '{"ok":true}' }
        );
        Test.setMock(HttpCalloutMock.class, mock);
        CountingTokens tokens = new CountingTokens();

        Test.startTest();
        BoxResponse res = new BoxHttpClient(tokens).send(request('GET', 'api', '/me'));
        Test.stopTest();

        System.assertEquals(200, res.statusCode, 'a 5xx is retried and then succeeds');
        System.assertEquals(2, mock.seen.size(), 'the transient failure was retried');
        System.assertEquals(0, tokens.invalidations, 'a 5xx is not an auth failure');
    }

    @isTest
    static void exhaustedTransientRetriesThrows() {
        // maxRetries = 1: one retry, then the second 429 is surfaced.
        SequenceMock mock = new SequenceMock(
            new List<Integer>{ 429, 429, 429 },
            new List<String>{ 'busy', 'busy', 'busy' }
        );
        Test.setMock(HttpCalloutMock.class, mock);
        CountingTokens tokens = new CountingTokens();

        Test.startTest();
        try {
            new BoxHttpClient(tokens, 1, 30000).send(request('GET', 'api', '/me'));
            System.assert(false, 'exhausted retries must throw');
        } catch (BoxApiException e) {
            System.assertEquals(429, e.statusCode, 'the last transient status is surfaced');
        }
        Test.stopTest();

        System.assertEquals(2, mock.seen.size(), 'initial attempt + one retry');
    }

    @isTest
    static void nonRetriable4xxThrowsWithBody() {
        SequenceMock mock = new SequenceMock(
            new List<Integer>{ 404 },
            new List<String>{ '{"code":"not_found"}' }
        );
        Test.setMock(HttpCalloutMock.class, mock);
        CountingTokens tokens = new CountingTokens();

        Test.startTest();
        try {
            new BoxHttpClient(tokens).send(request('GET', 'api', '/files/0'));
            System.assert(false, 'a 404 must throw');
        } catch (BoxApiException e) {
            System.assertEquals(404, e.statusCode, 'the status is carried');
            System.assert(
                e.responseBody != null && e.responseBody.contains('not_found'),
                'the raw error payload is preserved'
            );
        }
        Test.stopTest();

        System.assertEquals(1, mock.seen.size(), 'a 4xx is not retried');
        System.assertEquals(0, tokens.invalidations, 'a 404 is not an auth failure');
    }

    @isTest
    static void serializesJsonBodyAndHonorsOverrideBaseUrl() {
        SequenceMock mock = new SequenceMock(
            new List<Integer>{ 201 },
            new List<String>{ '{"id":"9"}' }
        );
        Test.setMock(HttpCalloutMock.class, mock);
        CountingTokens tokens = new CountingTokens();

        BoxRequest req = request('POST', 'api', '/folders');
        req.body = new Map<String, Object>{ 'name' => 'Docs' };

        Test.startTest();
        BoxResponse res = new BoxHttpClient(tokens)
            .overrideBaseUrl('api', 'https://example.my.salesforce.com/')
            .send(req);
        Test.stopTest();

        System.assertEquals(201, res.statusCode, 'a 2xx create is returned');
        System.assertEquals(
            'https://example.my.salesforce.com/folders',
            mock.seen[0].getEndpoint(),
            'the overridden base URL is used, trailing slash trimmed'
        );
        System.assert(
            mock.seen[0].getBody().contains('"name"'),
            'the structural body is JSON-serialized'
        );
    }

    @isTest
    static void defaultHeaderIsSentAndOverridableByARequestHeader() {
        SequenceMock mock = new SequenceMock(
            new List<Integer>{ 200, 200 },
            new List<String>{ '{}', '{}' }
        );
        Test.setMock(HttpCalloutMock.class, mock);
        CountingTokens tokens = new CountingTokens();
        BoxHttpClient client = new BoxHttpClient(tokens).withHeader('X-Trace-Id', 'default');

        Test.startTest();
        client.send(request('GET', 'api', '/a'));
        BoxRequest overriding = request('GET', 'api', '/b');
        overriding.headers.put('X-Trace-Id', 'override');
        client.send(overriding);
        Test.stopTest();

        System.assertEquals(
            'default',
            mock.seen[0].getHeader('X-Trace-Id'),
            'the client default is sent when the request sets no override'
        );
        System.assertEquals(
            'override',
            mock.seen[1].getHeader('X-Trace-Id'),
            'a request-level header overrides the same-named default'
        );
    }

    @isTest
    static void suppressNullsOmitsNullKeysByDefault() {
        // The default (suppressNulls = true) path receives a typed model object;
        // a null-valued field is dropped from the serialized body — an unset
        // field stays absent (D-138). Note this is Apex object-field suppression:
        // `JSON.serialize(x, true)` drops null Apex object fields, not null Map
        // entries, and the default path only ever carries a typed object.
        SequenceMock mock = new SequenceMock(
            new List<Integer>{ 200 },
            new List<String>{ '{}' }
        );
        Test.setMock(HttpCalloutMock.class, mock);
        CountingTokens tokens = new CountingTokens();

        BoxRequest req = request('POST', 'api', '/things');
        TypedBody payload = new TypedBody();
        payload.keep = 'x';
        payload.drop = null;
        req.body = payload;

        Test.startTest();
        new BoxHttpClient(tokens).send(req);
        Test.stopTest();

        String body = mock.seen[0].getBody();
        System.assert(body.contains('"keep"'), 'a set key is sent: ' + body);
        System.assert(!body.contains('"drop"'), 'a null field is omitted: ' + body);
    }

    @isTest
    static void explicitNullsAreSentWhenSuppressionIsOff() {
        // A body routed through denormalizeKeys arrives as a reduced map and
        // sets suppressNulls = false, so an intentional null reaches Box.
        SequenceMock mock = new SequenceMock(
            new List<Integer>{ 200 },
            new List<String>{ '{}' }
        );
        Test.setMock(HttpCalloutMock.class, mock);
        CountingTokens tokens = new CountingTokens();

        BoxRequest req = request('POST', 'api', '/things');
        req.body = new Map<String, Object>{ 'keep' => 'x', 'clear' => null };
        req.suppressNulls = false;

        Test.startTest();
        new BoxHttpClient(tokens).send(req);
        Test.stopTest();

        String body = mock.seen[0].getBody();
        System.assert(body.contains('"keep"'), 'a set key is sent: ' + body);
        System.assert(body.contains('"clear":null'), 'an explicit null is sent: ' + body);
    }

    @isTest
    static void unknownBaseUrlThrowsBeforeAnyCallout() {
        SequenceMock mock = new SequenceMock(
            new List<Integer>{ 200 },
            new List<String>{ '{}' }
        );
        Test.setMock(HttpCalloutMock.class, mock);
        CountingTokens tokens = new CountingTokens();

        Test.startTest();
        try {
            new BoxHttpClient(tokens).send(request('GET', 'no_such_class', '/x'));
            System.assert(false, 'an unknown base-URL class must throw');
        } catch (BoxApiException e) {
            System.assert(
                e.getMessage().contains('Unknown base-URL class'),
                'the message names the missing class'
            );
        }
        Test.stopTest();

        System.assertEquals(0, mock.seen.size(), 'the URL is validated before any callout');
    }

    @isTest
    static void nullTokenProviderIsRejected() {
        Test.startTest();
        try {
            new BoxHttpClient(null);
            System.assert(false, 'a null token provider must throw');
        } catch (BoxApiException e) {
            System.assert(true);
        }
        Test.stopTest();
    }
}
