// Hand-written Box Apex runtime (TR-Apex.6) — Box's event stream, polled.
//
// The other SDKs follow Box's long-poll protocol: wait on a real-time server
// until Box reports a change. Apex can't: a callout times out after at most
// 120 seconds (Box holds a long poll open for up to ~10 minutes), and a
// transaction can't wait. So the Apex stream is the protocol's other half:
// `poll()` fetches the events after the stream position once, drops repeats
// (Box may deliver an event twice), and advances the position. Call it from a
// `Schedulable` or `Queueable`, and keep `position` between transactions (a
// custom setting, say) to resume where the last poll stopped.
public with sharing class BoxEventStream {
    /** What the stream polls: a generated events manager's typed page call. */
    public interface Source {
        /** The events after a stream position, and the position to poll from next. */
        Page page(String position);
        /** The event's id, so a repeat can be dropped (null when it has none). */
        String id(Object event);
    }

    /** One page of events. */
    public class Page {
        public List<Object> events;
        public String next;

        public Page(List<Object> events, String next) {
            this.events = events;
            this.next = next;
        }
    }

    /** Box may deliver an event twice; this many recent event ids are kept to
     * drop the repeats (the bound Box's own SDKs use). */
    private static final Integer MAX_SEEN_EVENTS = 1000;

    private final Source source;
    private Set<String> seen = new Set<String>();
    /** The stream position the next poll fetches from. Save it to resume later. */
    public String position { get; private set; }

    public BoxEventStream(Source source, String position) {
        this.source = source;
        this.position = String.isBlank(position) ? 'now' : position;
    }

    /** Fetch the events after the position, once each, and advance past them. */
    public List<Object> poll() {
        Page page = source.page(position);
        List<Object> fresh = new List<Object>();
        List<Object> events = page.events == null ? new List<Object>() : page.events;
        for (Object event : events) {
            String id = source.id(event);
            if (id != null) {
                if (seen.contains(id)) {
                    continue;
                }
                seen.add(id);
            }
            fresh.add(event);
        }
        if (seen.size() >= MAX_SEEN_EVENTS) {
            // Repeats arrive close together: keep only this page's ids.
            seen = new Set<String>();
            for (Object event : events) {
                String id = source.id(event);
                if (id != null) {
                    seen.add(id);
                }
            }
        }
        if (String.isNotBlank(page.next)) {
            position = page.next;
        }
        return fresh;
    }

    /** A stream position Box sent as a JSON string or number, as the string to
     * send back. */
    public static String position(Object value) {
        return value == null ? null : String.valueOf(value);
    }
}
