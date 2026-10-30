// Hand-written Box Apex runtime (TR-Apex.6) — Box's file representations.
//
// Box renders a file into other formats (a PDF, its extracted text, page
// images) on demand: the item answers `representations.entries`, each with a
// `status.state` and an `info.url` to poll while Box generates it, and a
// `content.url_template` to download from once it's ready. The other SDKs wait
// for that in one call. Apex can't — a transaction can't sleep, and callouts
// are bounded (at most 100 per transaction, 120 seconds each, 12 MB of heap
// for an asynchronous one's response) — so each `fetch` checks the status
// once: it returns the bytes when Box has them, or `pending` when Box is still
// generating them. Call it again from a later transaction (a `Queueable`, say)
// until `content` is set.
public with sharing class BoxRepresentations {
    /** What one `fetch` found. */
    public class Result {
        /** Box's state: `success` or `viewable` (with `content`), or `pending`. */
        public String state;
        /** The representation's bytes, once Box has generated them. */
        public Blob content;
    }

    /**
     * One step of Box's representation protocol for the item `request` fetches,
     * which already asks for `field` and carries the `X-Rep-Hints` header: take
     * the first representation Box offers under `field`; if Box is still
     * generating it, ask once more through its info URL; then download
     * `assetPath` from its content URL template (`''` for a single-file
     * representation such as a PDF or extracted text). A representation Box
     * fails to generate (`error`) throws `BoxApiException`.
     */
    public static Result fetch(BoxClient client, BoxRequest request, String field, String assetPath) {
        Map<String, Object> item = (Map<String, Object>) JSON.deserializeUntyped(client.send(request).body);
        Map<String, Object> reps = (Map<String, Object>) item.get(field);
        List<Object> entries = reps == null ? null : (List<Object>) reps.get('entries');
        if (entries == null || entries.isEmpty()) {
            throw new BoxApiException('box: no representation matches the X-Rep-Hints requested');
        }
        Map<String, Object> rep = (Map<String, Object>) entries[0];
        String name = String.valueOf(rep.get('representation'));
        String state = nested(rep, 'status', 'state');
        if (state == 'pending' || state == 'none') {
            String info = nested(rep, 'info', 'url');
            if (info == null) {
                throw new BoxApiException('box: the ' + name + ' representation has no info URL to poll');
            }
            Map<String, Object> polled = (Map<String, Object>) JSON.deserializeUntyped(client.send(absolute(info)).body);
            state = nested(polled, 'status', 'state');
            if (nested(polled, 'content', 'url_template') != null) {
                rep.put('content', polled.get('content'));
            }
        }
        Result result = new Result();
        if (state == 'pending' || state == 'none') {
            result.state = 'pending';
            return result;
        }
        if (state == 'error') {
            throw new BoxApiException(
                'box: Box could not generate the ' + name + ' representation (' + nested(rep, 'status', 'code') + ')'
            );
        }
        if (state != 'success' && state != 'viewable') {
            throw new BoxApiException('box: unknown representation state ' + state);
        }
        String template = nested(rep, 'content', 'url_template');
        if (template == null) {
            throw new BoxApiException('box: the ' + name + ' representation has no content URL');
        }
        String url = template.replace('{+asset_path}', assetPath == null ? '' : assetPath);
        result.state = state;
        result.content = client.send(absolute(url)).binaryBody;
        return result;
    }

    /** A `GET` of a URL Box returned (`baseUrl` null: `path` is the whole URL). */
    private static BoxRequest absolute(String url) {
        BoxRequest request = new BoxRequest();
        request.method = 'GET';
        request.path = url;
        return request;
    }

    /** `value.outer.inner` as a string, or null when absent. */
    private static String nested(Map<String, Object> value, String outer, String inner) {
        Object level = value.get(outer);
        if (!(level instanceof Map<String, Object>)) {
            return null;
        }
        Object found = ((Map<String, Object>) level).get(inner);
        return found == null ? null : String.valueOf(found);
    }
}
