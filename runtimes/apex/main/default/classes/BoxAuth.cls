// Hand-written Box Apex runtime (TR-Apex) — the auth entry point.
//
// Apex is one flat namespace, so an "auth package" is a facade class: `BoxAuth`
// gathers the four Box auth flows behind static factories (D-193) instead of
// five `Box*TokenProvider` class names to discover. Each returns a
// `BoxTokenProvider` — the value the `BoxHttpClient` is constructed with:
//
//   Box client = new Box(new BoxHttpClient(BoxAuth.developerToken('TOKEN')));
public with sharing class BoxAuth {
    private BoxAuth() {
        // Static factory only.
    }

    /** A fixed developer-console token, for a quick start. */
    public static BoxTokenProvider developerToken(String token) {
        return new BoxDeveloperTokenProvider(token);
    }

    /**
     * Client Credentials Grant (server auth, no key). `subjectType` is
     * `'enterprise'` for the service account or `'user'` to act as a managed
     * user; `subjectId` is the matching id.
     */
    public static BoxTokenProvider clientCredentials(
        String clientId,
        String clientSecret,
        String subjectType,
        String subjectId
    ) {
        return new BoxCcgTokenProvider(clientId, clientSecret, subjectType, subjectId);
    }

    /**
     * JWT server auth with a signing key. `certDevName` names the Salesforce
     * certificate holding the RSA key; `publicKeyId` is its Box key id.
     */
    public static BoxTokenProvider jwt(
        String clientId,
        String clientSecret,
        String subjectType,
        String subjectId,
        String publicKeyId,
        String certDevName
    ) {
        return new BoxJwtTokenProvider(
            clientId,
            clientSecret,
            subjectType,
            subjectId,
            publicKeyId,
            certDevName
        );
    }

    /**
     * Revoke the token a Client Credentials or JWT provider holds, with the
     * app's own client credentials, and drop it (the next call mints a fresh
     * one) — for logout and uninstall. A developer token has no client
     * credentials to revoke with, so it throws; revoke one through the
     * generated `authorization` manager instead.
     */
    public static void revokeToken(BoxTokenProvider provider) {
        if (provider instanceof BoxCachingTokenProvider) {
            ((BoxCachingTokenProvider) provider).revoke();
            return;
        }
        throw new BoxApiException(
            'This token provider cannot revoke its token (a developer token has no client credentials).'
        );
    }
}
