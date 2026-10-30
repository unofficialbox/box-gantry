// Hand-written Box Apex runtime (TR-Apex.6) — tests for acting as another user.
//
// `Box.withAsUserHeader` wraps the client in a `BoxAsUserClient`: every request
// from the derived `Box` carries `As-User`, the original `Box` sends none, and
// the header reaches the real callout through `BoxHttpClient`.
@isTest
private class BoxAsUserClientTest {
    // Records each request and answers with a fixed user, without a callout.
    private class RecordingClient implements BoxClient {
        public List<BoxRequest> seen = new List<BoxRequest>();

        public BoxResponse send(BoxRequest request) {
            this.seen.add(request);
            BoxResponse res = new BoxResponse();
            res.statusCode = 200;
            res.body = '{"id":"1","type":"user"}';
            return res;
        }
    }

    // Answers like Box and records the As-User header of each callout.
    private class HeaderMock implements HttpCalloutMock {
        public List<String> asUser = new List<String>();

        public HttpResponse respond(HttpRequest req) {
            this.asUser.add(req.getHeader('As-User'));
            HttpResponse res = new HttpResponse();
            res.setStatusCode(200);
            res.setBody('{"id":"1","type":"user"}');
            return res;
        }
    }

    @isTest
    static void theDerivedBoxActsAsTheUserAndTheOriginalDoesNot() {
        RecordingClient recorder = new RecordingClient();
        Box client = new Box(recorder);
        Box asUser = client.withAsUserHeader('12345');

        asUser.users.getMe(null);
        client.users.getMe(null);
        // Deriving again replaces the user rather than adding a second one.
        asUser.withAsUserHeader('678').users.getMe(null);

        System.assertEquals(3, recorder.seen.size(), 'one request per call');
        System.assertEquals('12345', recorder.seen[0].headers.get('As-User'), 'the derived Box acts as the user');
        System.assert(!recorder.seen[1].headers.containsKey('As-User'), 'the original Box is unchanged');
        System.assertEquals('678', recorder.seen[2].headers.get('As-User'), 'the latest user wins');
        System.assertEquals(1, recorder.seen[2].headers.size(), 'no duplicate As-User header');
    }

    @isTest
    static void theHeaderReachesTheCallout() {
        HeaderMock mock = new HeaderMock();
        Test.setMock(HttpCalloutMock.class, mock);
        Box client = new Box(new BoxHttpClient(new BoxDeveloperTokenProvider('t')));

        Test.startTest();
        client.withAsUserHeader('12345').users.getMe(null);
        client.users.getMe(null);
        Test.stopTest();

        System.assertEquals('12345', mock.asUser[0], 'As-User is on the callout');
        System.assertEquals(null, mock.asUser[1], 'the original client sends none');
    }

    @isTest
    static void aUserIdIsRequired() {
        try {
            new BoxAsUserClient(new RecordingClient(), ' ');
            System.assert(false, 'a blank user id must throw');
        } catch (BoxApiException e) {
            System.assert(e.getMessage().contains('user id'), e.getMessage());
        }
    }
}
