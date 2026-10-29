// Hand-written Box Apex runtime (TR-Apex.6) — the polled event stream, through
// the generated `BoxEvents.stream` and the real `BoxHttpClient`: a poll from
// "now" fetches from "now", the next from the position Box returned, and an
// event Box repeats is yielded once.
@isTest
private class BoxEventStreamTest {
    // Answers GET /events like Box: from "now", nothing; from 1, events a and
    // b; from 2, b again (a repeat) and c. The next position is a number, as
    // Box sends it.
    private class EventsMock implements HttpCalloutMock {
        public List<String> positions = new List<String>();

        public HttpResponse respond(HttpRequest req) {
            String position = req.getEndpoint().substringAfter('stream_position=').substringBefore('&');
            positions.add(position);
            List<Object> entries = new List<Object>();
            Long next = 1;
            if (position == '1') {
                entries = new List<Object>{ event('a'), event('b') };
                next = 2;
            } else if (position == '2') {
                entries = new List<Object>{ event('b'), event('c') };
                next = 3;
            }
            HttpResponse res = new HttpResponse();
            res.setStatusCode(200);
            res.setBody(JSON.serialize(new Map<String, Object>{
                'chunk_size' => entries.size(),
                'next_stream_position' => next,
                'entries' => entries
            }));
            return res;
        }
    }

    private static Map<String, Object> event(String id) {
        return new Map<String, Object>{ 'type' => 'event', 'event_id' => id, 'event_type' => 'ITEM_CREATE' };
    }

    private static List<String> ids(List<Object> events) {
        List<String> out = new List<String>();
        for (Object e : events) {
            out.add(((Event) e).event_id);
        }
        return out;
    }

    @isTest
    static void pollsFromThePositionBoxReturnsAndDropsRepeats() {
        EventsMock mock = new EventsMock();
        Test.setMock(HttpCalloutMock.class, mock);
        Box client = new Box(new BoxHttpClient(new BoxDeveloperTokenProvider('t')));

        Test.startTest();
        BoxEventStream stream = client.events.stream(null, null, null, null, null, null);
        System.assertEquals('now', stream.position, 'a stream starts from now');
        System.assertEquals(new List<String>(), ids(stream.poll()), 'nothing yet');
        System.assertEquals('1', stream.position, 'the position Box returned');
        System.assertEquals(new List<String>{ 'a', 'b' }, ids(stream.poll()), 'the new events');
        System.assertEquals(new List<String>{ 'c' }, ids(stream.poll()), 'the repeated b is dropped');
        System.assertEquals('3', stream.position, 'advanced past the last page');
        Test.stopTest();

        System.assertEquals(new List<String>{ 'now', '1', '2' }, mock.positions, 'each poll fetched from the last position');
    }

    @isTest
    static void aSavedPositionResumes() {
        EventsMock mock = new EventsMock();
        Test.setMock(HttpCalloutMock.class, mock);
        Box client = new Box(new BoxHttpClient(new BoxDeveloperTokenProvider('t')));

        Test.startTest();
        BoxEventStream stream = client.events.stream(null, '2', null, null, null, null);
        System.assertEquals(new List<String>{ 'b', 'c' }, ids(stream.poll()), 'resumed at 2');
        Test.stopTest();
        System.assertEquals('3', stream.position, 'advanced');
        System.assertEquals('42', BoxEventStream.position(42L), 'a numeric position as a string');
    }
}
