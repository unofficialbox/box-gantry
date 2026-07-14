// Hand-written Box Apex runtime (TR-Apex.6) — the simplest auth flow.
//
// A fixed developer token, for quick testing and short-lived scripts. Box
// developer tokens expire after ~60 minutes; production integrations should
// use a Client-Credentials or JWT provider (later runtime slices) instead.
public with sharing class BoxDeveloperTokenProvider implements BoxTokenProvider {
    private final String token;

    /** Wrap a Box developer token. */
    public BoxDeveloperTokenProvider(String token) {
        if (String.isBlank(token)) {
            throw new BoxApiException('A developer token is required.');
        }
        this.token = token;
    }

    public String getAccessToken() {
        return this.token;
    }

    /** A fixed developer token can't be refreshed — nothing to discard. */
    public void invalidate() {
        // no-op
    }
}
