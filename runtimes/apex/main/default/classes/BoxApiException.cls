// Hand-written Box Apex runtime (TR-Apex.6) — the error type.
//
// The manifest's error model is exceptions (D-107): a non-2xx response is
// raised, not returned. This carries the HTTP status and the raw response
// body so a caller can inspect the Box error payload. Apex requires custom
// exception names to end in `Exception`.
public class BoxApiException extends Exception {
    /** The HTTP status code that triggered the error (0 if none). */
    public Integer statusCode = 0;
    /** The raw response body, for inspecting the Box error payload. */
    public String responseBody;

    /** Build an API error from a non-2xx response. */
    public static BoxApiException fromResponse(Integer status, String body) {
        BoxApiException error = new BoxApiException(
            'Box API request failed with status ' + status
        );
        error.statusCode = status;
        error.responseBody = body;
        return error;
    }
}
