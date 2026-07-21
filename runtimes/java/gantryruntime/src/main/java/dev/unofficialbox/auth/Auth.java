// Hand-written Box Java runtime (TR-Java) — the auth entry point.
//
// The four Box auth flows, split from the transport runtime (D-193) so the
// user-facing factory reads `auth.Auth.developerToken(…)` rather than the
// transport class name. Each returns a `Runtime.Auth` token source — the value
// the client is constructed with — by delegating to the transport runtime's
// factories. The config types stay nested on `Runtime`; reference them as
// `Runtime.CcgConfig` / `Runtime.OAuthConfig` / `Runtime.JwtConfig`.
package dev.unofficialbox.auth;

import dev.unofficialbox.runtime.Runtime;

/** Builders for the four Box auth flows. Pass the result to the client. */
public final class Auth {
    private Auth() {}

    /** A fixed developer-console token, for a quick start. */
    public static Runtime.Auth developerToken(String token) {
        return Runtime.developerToken(token);
    }

    /** Client Credentials Grant: server-to-server auth with no signing key. */
    public static Runtime.Auth clientCredentials(Runtime.CcgConfig config) {
        return Runtime.clientCredentials(config);
    }

    /** OAuth 2.0 authorization-code, resumed from a stored refresh token. */
    public static Runtime.Auth oauth(Runtime.OAuthConfig config, String refreshToken) {
        return Runtime.oauth(config, refreshToken);
    }

    /** OAuth 2.0 with a store that persists the rotating refresh token. */
    public static Runtime.Auth oauthWithStore(
            Runtime.OAuthConfig config, String refreshToken, Runtime.RefreshTokenStore store) {
        return Runtime.oauthWithStore(config, refreshToken, store);
    }

    /** JWT server auth with an RSA signing key (Box's box_config.json). */
    public static Runtime.Auth jwt(Runtime.JwtConfig config) {
        return Runtime.jwt(config);
    }
}
