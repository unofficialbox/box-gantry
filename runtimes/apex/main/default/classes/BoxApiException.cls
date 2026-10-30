// Hand-written Box Apex runtime (TR-Apex.6) — the error type.
//
// The manifest's error model is exceptions (D-107): a non-2xx response is
// raised, not returned. This carries the HTTP status, Box's error fields when
// the body is a Box error object, and the raw response body. Apex requires
// custom exception names to end in `Exception`.
public class BoxApiException extends Exception {
    /** The HTTP status code that triggered the error (0 if none). */
    public Integer statusCode = 0;
    /** The raw response body, for inspecting the Box error payload. */
    public String responseBody;
    /** Box's error `code` (e.g. `not_found`), when the body is a Box error. */
    public String code;
    /** Box's error `message`, when the body is a Box error. */
    public String boxMessage;
    /** Box's `request_id` — quote it to Box support — when the body is a Box error. */
    public String requestId;

    /** Build an API error from a non-2xx response, reading Box's error fields
     * from the body when it is a Box error object. */
    public static BoxApiException fromResponse(Integer status, String body) {
        Map<String, Object> fields = new Map<String, Object>();
        if (String.isNotBlank(body)) {
            try {
                Object parsed = JSON.deserializeUntyped(body);
                if (parsed instanceof Map<String, Object>) {
                    fields = (Map<String, Object>) parsed;
                }
            } catch (JSONException notABoxError) {
                // A non-JSON body (a proxy's HTML page): keep it raw only.
            }
        }
        // A Box error object, or from the token endpoint an OAuth 2.0 error.
        String code = stringField(fields, 'code');
        if (code == null) {
            code = stringField(fields, 'error');
        }
        String boxMessage = stringField(fields, 'message');
        if (boxMessage == null) {
            boxMessage = stringField(fields, 'error_description');
        }
        String requestId = stringField(fields, 'request_id');

        String message = 'box: status ' + status;
        if (code != null) {
            message += ' ' + code;
        }
        if (boxMessage != null) {
            message += ': ' + boxMessage;
        }
        if (requestId != null) {
            message += ' (request_id ' + requestId + ')';
        }
        BoxApiException error = new BoxApiException(message);
        error.statusCode = status;
        error.responseBody = body;
        error.code = code;
        error.boxMessage = boxMessage;
        error.requestId = requestId;
        return error;
    }

    /** Whether this is Box's 412 Precondition Failed: an `If-Match` write whose
     * etag no longer matches, because the item changed since it was read.
     * Re-read the item and retry with its new etag. */
    public Boolean isPreconditionFailed() {
        return statusCode == 412;
    }

    private static String stringField(Map<String, Object> fields, String key) {
        Object value = fields.get(key);
        return value instanceof String ? (String) value : null;
    }
}
