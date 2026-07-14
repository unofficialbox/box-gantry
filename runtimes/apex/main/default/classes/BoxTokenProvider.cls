// Hand-written Box Apex runtime (TR-Apex.6) — the auth-token contract.
//
// A token provider yields the OAuth2 access token the HTTP client sends as
// `Authorization: Bearer <token>`. Implementations range from a static
// developer token to a Client-Credentials / JWT flow that mints and caches a
// token (and refreshes it when the client reports a 401). The generated SDK
// depends only on this interface, never a concrete flow.
public interface BoxTokenProvider {
    /**
     * The current access token. Called before every request; an
     * implementation may cache and refresh transparently.
     */
    String getAccessToken();

    /**
     * Discard any cached token so the next `getAccessToken()` mints a fresh
     * one. The HTTP client calls this before its single 401 re-attempt, so a
     * token revoked before its expiry still recovers. A static-token provider
     * has nothing to discard and treats this as a no-op.
     */
    void invalidate();
}
