//! Tests for the hand-written Java runtime (`runtimes/java/gantryruntime`),
//! driven through the real `javac`/`java` toolchain (the runtime is plain JDK
//! Java, so there is no build tool — the swap gate and these tests compile its
//! source directly). Three signals: the runtime compiles warning-clean under the
//! same strict gate as the generated SDK; it *behaves* — an embedded
//! `HttpServer` exercises the fetch retry loop and all four auth flows (CCG,
//! OAuth with refresh-token rotation, JWT assertion signing, developer token);
//! and the VR-7 live smoke compiles (so it can't rot) and runs against a real
//! Box account when credentials are present.
//!
//! Skips cleanly when the JDK is absent (local dev); CI installs one.

use std::path::PathBuf;
use std::process::Command;

use gantry_verify::scratch::ScratchDir;

fn runtime_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
        "../../runtimes/java/gantryruntime/src/main/java/dev/unofficialbox/runtime/Runtime.java",
    )
}

/// True only when both `javac` and the `java` runtime target Java 26+ (D-180):
/// the runtime and generated SDK target Java 26 and these gates *compile* with
/// `javac` and *execute* with `java`, so a missing, unreadable, or pre-26 tool
/// cannot run them and must skip — not fail — per the toolchain contract. `java`
/// and `javac` are separate binaries, so both are probed. CI installs JDK 26,
/// where both report 26 and the gates run for real.
fn jdk_targets_26() -> bool {
    tool_major_version("javac").is_some_and(|major| major >= 26)
        && tool_major_version("java").is_some_and(|major| major >= 26)
}

/// The major version reported by `<tool> -version`, or `None` when the tool is
/// absent, unreadable, or unparseable. Both tools print `-version` to stdout on
/// modern JDKs and stderr on very old ones, in two shapes: `openjdk version "26"
/// …` (`java`, major is the first quoted token) and `javac 26.0.1` (`javac`, no
/// quotes, major is the second whitespace token). Prefer the quoted token; fall
/// back to the bare one.
fn tool_major_version(tool: &str) -> Option<u32> {
    let out = Command::new(tool).arg("-version").output().ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let text = if stdout.trim().is_empty() {
        String::from_utf8_lossy(&out.stderr).into_owned()
    } else {
        stdout.into_owned()
    };
    text.split('"')
        .nth(1)
        .or_else(|| text.split_whitespace().nth(1))
        .and_then(|v| v.split(['.', '-', '_']).next())
        .and_then(|major| major.parse::<u32>().ok())
}

/// The runtime compiles warning-clean under `javac --release 26 -Xlint:all
/// -Werror` — the same bar as the generated SDK (VR-1.6), on its own. It uses
/// HTTP/3 (D-180), a Java 26 API, so 26 is the floor.
#[test]
fn the_runtime_compiles_standalone() {
    if !jdk_targets_26() {
        eprintln!("SKIPPED: JDK 26+ not available; CI installs one and runs this gate");
        return;
    }
    let dir = ScratchDir::new("gantry-java-rt-std");
    let javac = Command::new("javac")
        .arg("--release")
        .arg("26")
        .arg("-Xlint:all")
        .arg("-Werror")
        .arg("-d")
        .arg(&dir)
        .arg(runtime_src())
        .output()
        .unwrap();
    let ok = javac.status.success();
    let log = String::from_utf8_lossy(&javac.stderr).into_owned();
    assert!(ok, "the runtime failed the strict javac gate:\n{log}");
}

/// The runtime driver: an in-process `HttpServer` proves the fetch retry loop
/// (a 429 is retried and then succeeds) and all four auth flows — CCG, OAuth
/// (with refresh-token rotation persisted through a store), JWT (a signed
/// assertion verified against the generated key), and developer token — thread a
/// token as `Authorization: Bearer …` on the next call.
const DRIVER: &str = r#"
import com.sun.net.httpserver.HttpServer;
import com.sun.net.httpserver.HttpExchange;
import dev.unofficialbox.runtime.Runtime;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.security.KeyPair;
import java.security.KeyPairGenerator;
import java.security.PublicKey;
import java.security.Signature;
import java.util.Base64;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;

public final class RuntimeSmoke {
    /** A System.Logger that keeps every DEBUG message, for the debug log test. */
    record ListLogger(java.util.List<String> lines) implements System.Logger {
        public String getName() {
            return "test";
        }

        public boolean isLoggable(System.Logger.Level level) {
            return true;
        }

        public void log(System.Logger.Level level, java.util.ResourceBundle bundle, String msg, Throwable thrown) {
            lines.add(msg);
        }

        public void log(System.Logger.Level level, java.util.ResourceBundle bundle, String format, Object... params) {
            lines.add(format);
        }
    }

    static void check(boolean cond, String msg) {
        if (!cond) {
            System.out.println("FAIL: " + msg);
            System.exit(1);
        }
    }

    static void respond(HttpExchange ex, int status, String body) throws java.io.IOException {
        byte[] bytes = body.getBytes(StandardCharsets.UTF_8);
        ex.sendResponseHeaders(status, bytes.length);
        ex.getResponseBody().write(bytes);
        ex.close();
    }

    static String formValue(String body, String key) {
        for (String pair : body.split("&")) {
            int eq = pair.indexOf('=');
            if (eq > 0 && pair.substring(0, eq).equals(key)) {
                return java.net.URLDecoder.decode(pair.substring(eq + 1), StandardCharsets.UTF_8);
            }
        }
        return "";
    }

    public static void main(String[] args) throws Exception {
        // A key pair for the JWT flow: the runtime signs with the private key
        // (as an unencrypted PKCS#8 PEM), the server verifies with the public one.
        KeyPairGenerator kpg = KeyPairGenerator.getInstance("RSA");
        kpg.initialize(2048);
        KeyPair keyPair = kpg.generateKeyPair();
        PublicKey publicKey = keyPair.getPublic();
        String pem = "-----BEGIN PRIVATE KEY-----\n"
            + Base64.getMimeEncoder(64, "\n".getBytes(StandardCharsets.UTF_8))
                .encodeToString(keyPair.getPrivate().getEncoded())
            + "\n-----END PRIVATE KEY-----\n";

        HttpServer server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        AtomicInteger retryHits = new AtomicInteger();
        AtomicReference<String> savedRefresh = new AtomicReference<>();

        // 429 once, then 200 — the retry path.
        server.createContext("/retry", ex -> {
            if (retryHits.incrementAndGet() == 1) {
                ex.sendResponseHeaders(429, -1);
                ex.close();
            } else {
                respond(ex, 200, "ok");
            }
        });
        // A CCG token endpoint.
        server.createContext("/token", ex ->
            respond(ex, 200, "{\"access_token\":\"tok123\",\"expires_in\":3600}"));
        // Echoes the Authorization header it received.
        server.createContext("/me", ex ->
            respond(ex, 200, "auth=" + ex.getRequestHeaders().getFirst("Authorization")));
        // OAuth: mints access-oauth-<oldrt> and rotates the refresh token.
        server.createContext("/oauth", ex -> {
            String body = new String(ex.getRequestBody().readAllBytes(), StandardCharsets.UTF_8);
            String oldRefresh = formValue(body, "refresh_token");
            respond(ex, 200, "{\"access_token\":\"oauth-" + oldRefresh
                + "\",\"expires_in\":3600,\"refresh_token\":\"rt-next\"}");
        });
        // JWT: verify the RS256 assertion against the public key before issuing.
        server.createContext("/jwt", ex -> {
            String body = new String(ex.getRequestBody().readAllBytes(), StandardCharsets.UTF_8);
            String assertion = formValue(body, "assertion");
            String[] parts = assertion.split("\\.");
            try {
                Signature verifier = Signature.getInstance("SHA256withRSA");
                verifier.initVerify(publicKey);
                verifier.update((parts[0] + "." + parts[1]).getBytes(StandardCharsets.UTF_8));
                boolean valid = parts.length == 3
                    && verifier.verify(Base64.getUrlDecoder().decode(parts[2]));
                String claims = new String(Base64.getUrlDecoder().decode(parts[1]), StandardCharsets.UTF_8);
                if (valid && claims.contains("\"box_sub_type\":\"enterprise\"")) {
                    respond(ex, 200, "{\"access_token\":\"jwttok\",\"expires_in\":3600}");
                } else {
                    respond(ex, 401, "bad assertion");
                }
            } catch (Exception err) {
                respond(ex, 500, err.getMessage());
            }
        });

        // Echoes the raw request body (ISO-8859-1 so every byte round-trips
        // losslessly through String) for the multipart assertions below.
        AtomicReference<String> multipartBody = new AtomicReference<>();
        server.createContext("/multipart", ex -> {
            byte[] raw = ex.getRequestBody().readAllBytes();
            multipartBody.set(new String(raw, StandardCharsets.ISO_8859_1));
            respond(ex, 200, "ok");
        });

        // A Box error body: fetch must throw it, not return it as a result.
        server.createContext("/missing", ex -> respond(ex, 404,
            "{\"type\":\"error\",\"status\":404,\"code\":\"not_found\","
                + "\"message\":\"Not Found\",\"request_id\":\"abc123\"}"));
        // A persistent 401 (the single refresh doesn't help) and a non-JSON 502.
        server.createContext("/unauthorized", ex -> respond(ex, 401, ""));
        server.createContext("/bad-gateway", ex -> respond(ex, 502, "upstream down"));

        // A download: 302 to the content host, as Box answers GET /files/{id}/content.
        server.createContext("/download", ex -> {
            ex.getResponseHeaders().add("Location", "/content-host");
            ex.sendResponseHeaders(302, -1);
            ex.close();
        });
        server.createContext("/content-host", ex -> respond(ex, 200, "file bytes"));
        // Reached through the proxy tests below: records the Host the client
        // asked the proxy (this server) for.
        AtomicReference<String> proxiedHost = new AtomicReference<>();
        server.createContext("/proxied", ex -> {
            proxiedHost.set(ex.getRequestHeaders().getFirst("Host"));
            respond(ex, 200, "proxied");
        });
        // A 429 (Retry-After: 0), then a 401, then 200, for the hook tests;
        // records each request's Authorization header.
        java.util.List<String> flakyAuths = new java.util.concurrent.CopyOnWriteArrayList<>();
        server.createContext("/flaky", ex -> {
            flakyAuths.add(ex.getRequestHeaders().getFirst("Authorization"));
            if (flakyAuths.size() == 1) {
                ex.getResponseHeaders().add("Retry-After", "0");
                respond(ex, 429, "");
            } else if (flakyAuths.size() == 2) {
                respond(ex, 401, "");
            } else {
                respond(ex, 200, "{}");
            }
        });

        // 503 twenty times, then 200, for the retry backoff test.
        AtomicInteger unavailableHits = new AtomicInteger();
        server.createContext("/unavailable", ex ->
            respond(ex, unavailableHits.incrementAndGet() <= 20 ? 503 : 200, "{}"));
        // Revoke: /rv/oauth2/token mints rv-1, rv-2, … (with rotating refresh
        // tokens), /rv/oauth2/revoke records each form; /rvbad/oauth2/revoke
        // refuses. /revoke is the proxied CCG's revoke (its token URL is
        // http://box.invalid/token), recording the Host it was asked for.
        AtomicInteger minted = new AtomicInteger();
        java.util.List<String> revoked = new java.util.concurrent.CopyOnWriteArrayList<>();
        com.sun.net.httpserver.HttpHandler mint = ex -> {
            int n = minted.incrementAndGet();
            respond(ex, 200, "{\"access_token\":\"rv-" + n + "\",\"expires_in\":3600,\"refresh_token\":\"rrt-" + n + "\"}");
        };
        server.createContext("/rv/oauth2/token", mint);
        server.createContext("/rvbad/oauth2/token", mint);
        server.createContext("/rv/oauth2/revoke", ex -> {
            revoked.add(new String(ex.getRequestBody().readAllBytes(), StandardCharsets.UTF_8));
            respond(ex, 200, "");
        });
        server.createContext("/rvbad/oauth2/revoke", ex -> respond(ex, 400, "{\"error\":\"invalid_client\"}"));
        server.createContext("/revoke", ex -> {
            revoked.add(ex.getRequestHeaders().getFirst("Host") + " "
                + new String(ex.getRequestBody().readAllBytes(), StandardCharsets.UTF_8));
            respond(ex, 200, "");
        });

        server.start();
        String base = "http://127.0.0.1:" + server.getAddress().getPort();

        // Retry: a 429 is retried and the call ultimately succeeds.
        Runtime.Session dev = new Runtime.Session(Runtime.developerToken("devtok"));
        Runtime.Response r1 = dev.fetch(dev.newRequest("GET", base + "/retry"));
        check(Runtime.statusCode(r1) == 200, "retry should end 200, got " + Runtime.statusCode(r1));
        check(new String(Runtime.responseBytes(r1), StandardCharsets.UTF_8).equals("ok"), "retry body");
        check(retryHits.get() == 2, "endpoint should have been hit twice, got " + retryHits.get());

        // CCG: acquire a token, then thread it as a bearer credential.
        Runtime.Auth ccg = Runtime.clientCredentials(
            Runtime.CcgConfig.enterprise("cid", "csecret", "ent1").tokenUrl(base + "/token"));
        Runtime.Session ccgSession = new Runtime.Session(ccg);
        check(ccgSession.accessToken().equals("tok123"), "ccg token should be tok123");
        Runtime.Response r2 = ccgSession.fetch(ccgSession.newRequest("GET", base + "/me"));
        String echoed = new String(Runtime.responseBytes(r2), StandardCharsets.UTF_8);
        check(echoed.equals("auth=Bearer tok123"), "ccg bearer header, got: " + echoed);

        // OAuth: the seed refresh token is exchanged and the rotated one persisted.
        Runtime.Auth oauth = Runtime.oauthWithStore(
            new Runtime.OAuthConfig("cid", "csecret").tokenUrl(base + "/oauth"),
            "rt-0", savedRefresh::set);
        Runtime.Session oauthSession = new Runtime.Session(oauth);
        check(oauthSession.accessToken().equals("oauth-rt-0"), "oauth token from rt-0");
        check("rt-next".equals(savedRefresh.get()), "rotated refresh token persisted, got: " + savedRefresh.get());

        // The code exchange saves the refresh token Box issues before returning.
        AtomicReference<String> exchanged = new AtomicReference<>();
        Runtime.Auth fromCode = new Runtime.OAuthConfig("cid", "csecret").tokenUrl(base + "/oauth")
            .exchangeCode("code", "https://app.example/callback", exchanged::set);
        check("rt-next".equals(exchanged.get()), "exchanged refresh token persisted, got: " + exchanged.get());
        check(new Runtime.Session(fromCode).accessToken().equals("oauth-rt-next"),
            "the exchanged auth refreshes with the saved token");

        // JWT: a signed assertion the server verifies against the key.
        Runtime.Auth jwt = Runtime.jwt(
            Runtime.JwtConfig.enterprise("cid", "csecret", "kid1", pem, null, "ent1")
                .tokenUrl(base + "/jwt"));
        Runtime.Session jwtSession = new Runtime.Session(jwt);
        check(jwtSession.accessToken().equals("jwttok"), "jwt token");

        // Multipart: the JSON part is the bare attributes object (not wrapped)
        // and the file part carries the real bytes — the pre-fix bug sent a
        // wrapped attributes part and never sent the file bytes at all (G-7).
        Runtime.Session mpSession = new Runtime.Session(Runtime.developerToken("t"));
        Runtime.Request mpReq = Runtime.withMultipartBody(
            mpSession.newRequest("POST", base + "/multipart"),
            "attributes", "{\"name\":\"f.txt\"}".getBytes(StandardCharsets.UTF_8),
            "file", Runtime.Stream.fromBytes("file bytes".getBytes(StandardCharsets.UTF_8)));
        mpSession.fetch(mpReq);
        String mp = multipartBody.get();
        check(mp.contains("name=\"attributes\""), "multipart: expected attributes part, got: " + mp);
        check(mp.contains("{\"name\":\"f.txt\"}"), "multipart: expected bare attributes JSON, got: " + mp);
        check(mp.contains("name=\"file\"; filename=\"file\""), "multipart: expected file part, got: " + mp);
        check(mp.contains("file bytes"), "multipart: expected file bytes, got: " + mp);

        // The avatar-upload shape has no attributes field at all: no JSON part
        // should be sent, regardless.
        Runtime.Request avatarReq = Runtime.withMultipartBody(
            mpSession.newRequest("POST", base + "/multipart"),
            "", new byte[0],
            "pic", Runtime.Stream.fromBytes("avatar bytes".getBytes(StandardCharsets.UTF_8)));
        mpSession.fetch(avatarReq);
        String avatarBody = multipartBody.get();
        check(!avatarBody.contains("application/json"), "avatar: expected no JSON part, got: " + avatarBody);
        check(avatarBody.contains("name=\"pic\"; filename=\"pic\""), "avatar: expected pic file part, got: " + avatarBody);

        // A non-2xx response throws BoxApiException with Box's error fields.
        Runtime.BoxApiException notFound = null;
        try {
            dev.fetch(dev.newRequest("GET", base + "/missing"));
        } catch (Runtime.BoxApiException err) {
            notFound = err;
        }
        check(notFound != null, "a 404 should throw BoxApiException");
        check(notFound.statusCode() == 404 && "not_found".equals(notFound.code())
                && "Not Found".equals(notFound.boxMessage()) && "abc123".equals(notFound.requestId()),
            "404 error fields, got: " + notFound.getMessage());
        check(notFound.getMessage().equals("box: status 404 not_found: Not Found (request_id abc123)"),
            "404 message, got: " + notFound.getMessage());

        Runtime.BoxApiException unauthorized = null;
        try {
            dev.fetch(dev.newRequest("GET", base + "/unauthorized"));
        } catch (Runtime.BoxApiException err) {
            unauthorized = err;
        }
        check(unauthorized != null && unauthorized.statusCode() == 401,
            "a persistent 401 should throw BoxApiException(401)");

        Runtime.Session noRetry = Runtime.Session.builder(Runtime.developerToken("devtok"))
            .maxRetries(0)
            .build();
        Runtime.BoxApiException badGateway = null;
        try {
            noRetry.fetch(noRetry.newRequest("GET", base + "/bad-gateway"));
        } catch (Runtime.BoxApiException err) {
            badGateway = err;
        }
        check(badGateway != null && badGateway.statusCode() == 502 && badGateway.code() == null
                && new String(badGateway.body(), StandardCharsets.UTF_8).equals("upstream down"),
            "a non-JSON 502 should throw with the raw body");

        // The built-in client follows the download redirect; Java's default
        // (NEVER) would return the 302's empty body as the file.
        Runtime.Response dl = dev.fetch(dev.newRequest("GET", base + "/download"));
        check(Runtime.statusCode(dl) == 200, "download should follow the 302, got " + Runtime.statusCode(dl));
        check(new String(Runtime.responseBytes(dl), StandardCharsets.UTF_8).equals("file bytes"),
            "download body after the redirect");

        // httpVersion picks the built-in client's protocol; this server speaks
        // HTTP/1.1, so a pinned HTTP_1_1 session must still round-trip.
        Runtime.Session h1 = Runtime.Session.builder(Runtime.developerToken("devtok"))
            .httpVersion(java.net.http.HttpClient.Version.HTTP_1_1)
            .build();
        Runtime.Response rh1 = h1.fetch(h1.newRequest("GET", base + "/me"));
        check(Runtime.statusCode(rh1) == 200, "HTTP_1_1 session should succeed, got " + Runtime.statusCode(rh1));
        // An injected client carries its own version: setting both is loud.
        boolean conflicted = false;
        try {
            Runtime.Session.builder(Runtime.developerToken("devtok"))
                .httpClient(java.net.http.HttpClient.newHttpClient())
                .httpVersion(java.net.http.HttpClient.Version.HTTP_2)
                .build();
        } catch (IllegalStateException expected) {
            conflicted = true;
        }
        check(conflicted, "httpClient + httpVersion should be rejected at build()");

        // proxy sends API and token requests through the given proxy — here
        // this server, which answers for a host that doesn't resolve.
        java.net.ProxySelector viaServer = java.net.ProxySelector.of(server.getAddress());
        Runtime.Auth proxiedAuth = Runtime.clientCredentials(
                Runtime.CcgConfig.enterprise("cid", "csecret", "ent1")
                    .tokenUrl("http://box.invalid/token")
                    .proxy(viaServer));
        Runtime.Session proxied = Runtime.Session.builder(proxiedAuth)
            .proxy(viaServer)
            .build();
        check(proxied.accessToken().equals("tok123"), "token request through the proxy");
        Runtime.Response rp = proxied.fetch(proxied.newRequest("GET", "http://box.invalid/proxied"));
        check(Runtime.statusCode(rp) == 200 && "box.invalid".equals(proxiedHost.get()),
            "API request through the proxy, Host: " + proxiedHost.get());
        // A revoke goes through the token client, so through the proxy too.
        proxiedAuth.revokeToken();
        check(revoked.equals(java.util.List.of("box.invalid client_id=cid&client_secret=csecret&token=tok123")),
            "revoke through the proxy, got: " + revoked);
        revoked.clear();
        boolean proxyConflicted = false;
        try {
            Runtime.Session.builder(Runtime.developerToken("devtok"))
                .httpClient(java.net.http.HttpClient.newHttpClient())
                .proxy(viaServer)
                .build();
        } catch (IllegalStateException expected) {
            proxyConflicted = true;
        }
        check(proxyConflicted, "httpClient + proxy should be rejected at build()");

        // Hooks see every attempt — the retried 429 and the resend after the
        // 401 refresh — with the token withheld from them.
        java.util.List<Runtime.RequestInfo> seenRequests = new java.util.ArrayList<>();
        java.util.List<Runtime.ResponseInfo> seenResponses = new java.util.ArrayList<>();
        Runtime.Session hooked = Runtime.Session.builder(Runtime.developerToken("secret-token"))
            .onRequest(seenRequests::add)
            .onResponse(seenResponses::add)
            .build();
        hooked.fetch(Runtime.withQuery(hooked.newRequest("GET", base + "/flaky"), "fields", "id"));
        check(flakyAuths.size() == 3 && "Bearer secret-token".equals(flakyAuths.get(2)),
            "flaky server saw " + flakyAuths);
        check(seenRequests.size() == 3 && seenResponses.size() == 3,
            "hooks saw " + seenRequests.size() + " requests, " + seenResponses.size() + " responses");
        int[] statuses = {429, 401, 200};
        for (int i = 0; i < 3; i++) {
            Runtime.RequestInfo req = seenRequests.get(i);
            Runtime.ResponseInfo resp = seenResponses.get(i);
            check(req.attempt() == i + 1 && resp.request().attempt() == i + 1, "attempt " + (i + 1));
            check(req.method().equals("GET") && req.url().equals(base + "/flaky?fields=id"), "request " + req);
            check(java.util.List.of("REDACTED").equals(req.headers().get("authorization")),
                "hook saw Authorization " + req.headers().get("authorization"));
            check(resp.status() == statuses[i] && resp.error() == null, "response " + resp);
        }
        check(java.util.List.of("0").equals(seenResponses.get(0).headers().get("retry-after")),
            "429 headers " + seenResponses.get(0).headers());

        // A transport failure reaches the response hook as an error.
        java.util.List<Runtime.ResponseInfo> failed = new java.util.ArrayList<>();
        Runtime.Session refused = Runtime.Session.builder(Runtime.developerToken("t"))
            .maxRetries(0)
            .onResponse(failed::add)
            .build();
        boolean threw = false;
        try {
            refused.fetch(refused.newRequest("GET", "http://127.0.0.1:1/"));
        } catch (Runtime.BoxApiException expected) {
            threw = true;
        }
        check(threw && failed.size() == 1 && failed.get(0).status() == 0 && failed.get(0).error() != null,
            "transport error hook: " + failed);

        // The debug log has a line per attempt and never the token, a secret
        // query value, or a body.
        flakyAuths.clear();
        java.util.List<String> lines = new java.util.ArrayList<>();
        Runtime.Session logged = Runtime.Session.builder(Runtime.developerToken("secret-token"))
            .debugLogging(new ListLogger(lines))
            .build();
        Runtime.Request loggedReq = Runtime.withQuery(Runtime.withQuery(Runtime.withQuery(
            logged.newRequest("POST", base + "/flaky"), "fields", "id"),
            "access_token", "query-token"), "client_secret", "query-secret");
        logged.fetch(Runtime.withJsonBody(loggedReq,
            "{\"password\":\"body-secret\"}".getBytes(StandardCharsets.UTF_8)));
        Runtime.Session loggedRefused = Runtime.Session.builder(Runtime.developerToken("t"))
            .maxRetries(0)
            .debugLogging(new ListLogger(lines))
            .build();
        try {
            loggedRefused.fetch(Runtime.withQuery(
                loggedRefused.newRequest("GET", "http://127.0.0.1:1/"), "access_token", "query-token"));
        } catch (Runtime.BoxApiException expected) {
            // Logged below.
        }
        check(lines.size() == 4, "debug log lines: " + lines);
        for (String line : lines) {
            for (String secret : new String[] {"secret-token", "query-token", "query-secret", "body-secret"}) {
                check(!line.contains(secret), "debug log leaked " + secret + ": " + line);
            }
        }
        String[] loggedStatuses = {"429", "401", "200"};
        for (int i = 0; i < 3; i++) {
            String want = "box: POST " + base
                + "/flaky?fields=id&access_token=REDACTED&client_secret=REDACTED -> " + loggedStatuses[i] + " in ";
            check(lines.get(i).startsWith(want) && lines.get(i).endsWith("(attempt " + (i + 1) + ")"),
                "debug log line " + lines.get(i));
        }
        check(lines.get(3).startsWith("box: GET http://127.0.0.1:1/?access_token=REDACTED -> error: "),
            "transport error line " + lines.get(3));
        // Case-insensitive, percent-decoded parameter names; other values kept.
        lines.clear();
        Runtime.Session loggedProxied = Runtime.Session.builder(Runtime.developerToken("t"))
            .proxy(viaServer)
            .debugLogging(new ListLogger(lines))
            .build();
        loggedProxied.fetch(loggedProxied.newRequest("GET",
            "http://box.invalid/proxied?Access_Token=t&code=c&refresh%5Ftoken=r&api_key=k&q=a@b&password"));
        check(lines.size() == 1 && lines.get(0).startsWith("box: GET http://box.invalid/proxied?Access_Token=REDACTED"
                + "&code=REDACTED&refresh%5Ftoken=REDACTED&api_key=REDACTED&q=a@b&password=REDACTED -> 200 in "),
            "redacted line " + lines);

        // retryBackoff sets the waits: twenty of them, each uniform in
        // [0, 100ms], sum to about 1s — over 400ms all but certainly, and
        // under the 2s cap, where the default 500ms-to-30s backoff would take
        // minutes.
        Runtime.Session quick = Runtime.Session.builder(Runtime.developerToken("t"))
            .maxRetries(20)
            .retryBackoff(java.time.Duration.ofMillis(100), java.time.Duration.ofMillis(100))
            .build();
        long backoffStarted = System.nanoTime();
        quick.fetch(quick.newRequest("GET", base + "/unavailable"));
        long backoffMs = (System.nanoTime() - backoffStarted) / 1_000_000;
        check(unavailableHits.get() == 21 && backoffMs >= 400 && backoffMs < 5_000,
            "retry backoff: " + unavailableHits.get() + " hits in " + backoffMs + "ms");
        boolean negative = false;
        try {
            Runtime.Session.builder(Runtime.developerToken("t"))
                .retryBackoff(java.time.Duration.ofMillis(-1), java.time.Duration.ZERO);
        } catch (IllegalArgumentException expected) {
            negative = true;
        }
        check(negative, "a negative retry delay should be rejected");
        // revokeToken: CCG posts the cached token with its own credentials to
        // the revoke endpoint beside the token URL, then re-acquires.
        Runtime.Auth rvCcg = Runtime.clientCredentials(
            Runtime.CcgConfig.enterprise("c", "s", "ent1").tokenUrl(base + "/rv/oauth2/token"));
        check(rvCcg.accessToken().equals("rv-1"), "ccg token before revoke");
        rvCcg.revokeToken();
        check(revoked.equals(java.util.List.of("client_id=c&client_secret=s&token=rv-1")),
            "ccg revoke form, got: " + revoked);
        check(rvCcg.accessToken().equals("rv-2"), "ccg re-acquires after revoke");
        // Nothing cached: nothing to revoke, no request.
        Runtime.clientCredentials(
            Runtime.CcgConfig.enterprise("c", "s", "ent1").tokenUrl(base + "/rv/oauth2/token")).revokeToken();
        check(revoked.size() == 1, "an empty revoke makes no request, got: " + revoked);
        revoked.clear();

        // JWT revokes the same way.
        Runtime.Auth rvJwt = Runtime.jwt(
            Runtime.JwtConfig.enterprise("c", "s", "kid1", pem, null, "ent1")
                .tokenUrl(base + "/rv/oauth2/token"));
        String jwtToken = rvJwt.accessToken();
        rvJwt.revokeToken();
        check(revoked.equals(java.util.List.of("client_id=c&client_secret=s&token=" + jwtToken)),
            "jwt revoke form, got: " + revoked);
        revoked.clear();

        // OAuth revokes the grant through its (rotated) refresh token, then
        // every call throws instead of refreshing a dead grant.
        Runtime.Auth rvOauth = Runtime.oauth(
            new Runtime.OAuthConfig("c", "s").tokenUrl(base + "/rv/oauth2/token"), "rrt-0");
        rvOauth.accessToken();
        String rotated = "rrt-" + minted.get();
        rvOauth.revokeToken();
        check(revoked.equals(java.util.List.of("client_id=c&client_secret=s&token=" + rotated)),
            "oauth revokes the refresh token, got: " + revoked);
        boolean deadGrant = false;
        try {
            rvOauth.accessToken();
        } catch (Runtime.BoxApiException expected) {
            deadGrant = expected.getMessage().contains("revoked");
        }
        check(deadGrant, "a revoked OAuth grant should throw on the next call");
        rvOauth.revokeToken();
        check(revoked.size() == 1, "a second OAuth revoke is a no-op, got: " + revoked);

        // A failed revoke throws and keeps the token; a developer token can't
        // revoke; a token URL without /token has no derivable revoke URL.
        Runtime.Auth rvBad = Runtime.clientCredentials(
            Runtime.CcgConfig.enterprise("c", "bad", "ent1").tokenUrl(base + "/rvbad/oauth2/token"));
        String kept = rvBad.accessToken();
        boolean revokeRefused = false;
        try {
            rvBad.revokeToken();
        } catch (Runtime.BoxApiException expected) {
            revokeRefused = expected.getMessage().contains("400");
        }
        check(revokeRefused && rvBad.accessToken().equals(kept), "a failed revoke throws and keeps the token");
        boolean devRefused = false;
        try {
            Runtime.developerToken("devtok").revokeToken();
        } catch (Runtime.BoxApiException expected) {
            devRefused = true;
        }
        check(devRefused, "a developer token should refuse to revoke");
        Runtime.Auth rvNoSuffix = Runtime.clientCredentials(
            Runtime.CcgConfig.enterprise("c", "s", "ent1").tokenUrl(base + "/rv/oauth2/token?v=1"));
        rvNoSuffix.accessToken();
        boolean underivable = false;
        try {
            rvNoSuffix.revokeToken();
        } catch (Runtime.BoxApiException expected) {
            underivable = expected.getMessage().contains("revoke endpoint");
        }
        check(underivable, "a token URL without /token should refuse to guess a revoke URL");

        server.stop(0);
        System.out.println("RUNTIME_OK");
    }
}
"#;

/// The fetch retry loop and all four auth flows work end to end against a real
/// (in-process) HTTP server.
#[test]
fn the_runtime_retries_and_authenticates() {
    if !jdk_targets_26() {
        eprintln!("SKIPPED: JDK 26+ not available; CI installs one and runs this gate");
        return;
    }
    let dir = ScratchDir::new("gantry-java-rt");
    let driver = dir.join("RuntimeSmoke.java");
    std::fs::write(&driver, DRIVER).unwrap();
    let classes = dir.join("classes");
    std::fs::create_dir_all(&classes).unwrap();

    // Compile the runtime + the driver (lint-checked, but the driver uses the
    // supported com.sun.net.httpserver API, so no -Werror here).
    let javac = Command::new("javac")
        .arg("--release")
        .arg("26")
        .arg("-d")
        .arg(&classes)
        .arg(runtime_src())
        .arg(&driver)
        .output()
        .unwrap();
    assert!(
        javac.status.success(),
        "runtime smoke failed to compile:\n{}",
        String::from_utf8_lossy(&javac.stderr)
    );

    let run = Command::new("java")
        .arg("-cp")
        .arg(&classes)
        .arg("RuntimeSmoke")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        run.status.success() && stdout.contains("RUNTIME_OK"),
        "runtime behavior check failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// The webhook driver: `Runtime.WebhookVerifier` against the vectors from Box's
/// own SDK test suite (testWebhookValidation) — keys, bodies, and the
/// signatures Box computed, all delivered at 2020-01-01T00:00:00-07:00 — and
/// every way a delivery must be refused.
const WEBHOOK_DRIVER: &str = r#"
import dev.unofficialbox.runtime.Runtime;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.time.Instant;
import java.util.LinkedHashMap;
import java.util.Map;

public final class WebhookSmoke {
    static final String PRIMARY_KEY = "SamplePrimaryKey";
    static final String SECONDARY_KEY = "SampleSecondaryKey";
    static final String BODY = "{\"type\":\"webhook_event\",\"webhook\":{\"id\":\"1234567890\"},\"trigger\":\"FILE.UPLOADED\",\"source\":{\"id\":\"1234567890\",\"type\":\"file\",\"name\":\"Test.txt\"}}";
    static final String PRIMARY = "6TfeAW3A1PASkgboxxA5yqHNKOwFyMWuEXny/FPD5hI=";
    static final String FILE_PREFIX = "{\"webhook\":{\"id\":\"1234567890\"},\"trigger\":\"FILE.UPLOADED\",\"source\":{\"id\":\"1234567890\",\"type\":\"file\",\"name\":\"";
    static final Instant DELIVERED = Instant.parse("2020-01-01T07:00:00Z");

    static void check(boolean cond, String msg) {
        if (!cond) {
            System.out.println("FAIL: " + msg);
            System.exit(1);
        }
    }

    // Mixed case on purpose: lookups must not depend on the sender's casing.
    static Map<String, String> headers(String primary) {
        Map<String, String> h = new LinkedHashMap<>();
        h.put("box-delivery-id", "f96bb54b-ee16-4fc5-aa65-8c2d9e5b546f");
        h.put("BOX-DELIVERY-TIMESTAMP", "2020-01-01T00:00:00-07:00");
        h.put("box-signature-algorithm", "HmacSHA256");
        h.put("box-signature-primary", primary);
        h.put("Box-Signature-Secondary", "v+1CD1Jdo3muIcbpv5lxxgPglOqMfsNHPV899xWYydo=");
        h.put("box-signature-version", "1");
        return h;
    }

    static Map<String, String> with(String name, String value) {
        Map<String, String> h = headers(PRIMARY);
        h.keySet().removeIf(k -> k.equalsIgnoreCase(name));
        if (value != null) {
            h.put(name, value);
        }
        return h;
    }

    static byte[] utf8(String s) {
        return s.getBytes(StandardCharsets.UTF_8);
    }

    public static void main(String[] args) {
        Runtime.WebhookVerifier both = new Runtime.WebhookVerifier(PRIMARY_KEY, SECONDARY_KEY);
        Runtime.WebhookVerifier primaryOnly = new Runtime.WebhookVerifier(PRIMARY_KEY, null);
        byte[] body = utf8(BODY);

        check(both.verify(body, headers(PRIMARY), DELIVERED), "both keys verify");
        check(new Runtime.WebhookVerifier(null, SECONDARY_KEY)
                .verify(body, headers("not-the-primary"), DELIVERED),
            "the secondary key alone verifies, as during a primary rotation");
        // Box signs non-ASCII characters as JSON unicode escapes; a body that
        // arrives as raw UTF-8 verifies through that form. Escapes already in
        // the JSON (backslash-r, backslash-slash, a doubled backslash) are
        // signed as they are.
        String[][] signed = {
            {"スクリーンショット 2020-08-05.txt", "LV2uCu+5NJtIHrCXDYgZ0v/PP5THGRuegw3RtdnEyuE="},
            {"😀 2020-08-05.txt", "xF/SDZosX4le+v4A0Qn59sZhuD1RqY5KRUKzVMSbh0E="},
            {"test \\r", "SVkbKgy3dEEf2PbbzpNu2lDZS7zZ/aboU7HOZgBGrJk="},
            {"\\/", "t41PWT5ZB6OcysnD6SDy9Ud+p9hdXxIdXqcdweyZv/Q="},
            {"\\\\", "ERpMZwUQsGDTfj82ehdX6VvDZfvOhK5ULNfVmwVAGe0="},
        };
        for (String[] c : signed) {
            check(primaryOnly.verify(utf8(FILE_PREFIX + c[0] + "\"}}"), headers(c[1]), DELIVERED),
                c[0] + ": Box's signature verifies");
        }
        check(both.verify(body, headers(PRIMARY), Instant.parse("2020-01-01T07:10:00Z")),
            "exactly the maximum age old is still in time");

        Map<String, String> unsigned = with("box-signature-primary", null);
        unsigned.remove("Box-Signature-Secondary");
        Object[][] refused = {
            {"wrong keys", new Runtime.WebhookVerifier("IncorrectKey", "IncorrectKey"), body, headers(PRIMARY), DELIVERED},
            {"no keys", new Runtime.WebhookVerifier(null, null), body, headers(PRIMARY), DELIVERED},
            {"secondary key for primary", new Runtime.WebhookVerifier(SECONDARY_KEY, null), body, headers(PRIMARY), DELIVERED},
            {"tampered body", both, utf8(BODY.replace("Test.txt", "Test.exe")), headers(PRIMARY), DELIVERED},
            {"stale", both, body, headers(PRIMARY), Instant.parse("2020-01-01T07:10:01Z")},
            {"future", both, body, headers(PRIMARY), Instant.parse("2020-01-01T06:59:59Z")},
            {"short max age", new Runtime.WebhookVerifier(PRIMARY_KEY, SECONDARY_KEY, Duration.ofMinutes(1)), body, headers(PRIMARY), Instant.parse("2020-01-01T07:01:01Z")},
            {"no timestamp", both, body, with("box-delivery-timestamp", null), DELIVERED},
            {"bad timestamp", both, body, with("box-delivery-timestamp", "yesterday"), DELIVERED},
            {"no signatures", both, body, unsigned, DELIVERED},
            {"no version", both, body, with("box-signature-version", null), DELIVERED},
            {"version 2", both, body, with("box-signature-version", "2"), DELIVERED},
            {"HmacSHA1", both, body, with("box-signature-algorithm", "HmacSHA1"), DELIVERED},
            {"not base64", primaryOnly, body, with("box-signature-primary", "%%%"), DELIVERED},
        };
        for (Object[] c : refused) {
            @SuppressWarnings("unchecked")
            Map<String, String> h = (Map<String, String>) c[3];
            check(!((Runtime.WebhookVerifier) c[1]).verify((byte[]) c[2], h, (Instant) c[4]),
                c[0] + ": must not verify");
        }
        System.out.println("WEBHOOKS_OK");
    }
}
"#;

/// `Runtime.WebhookVerifier` accepts Box's own signatures and refuses every
/// altered, stale, future, or unsigned delivery.
#[test]
fn the_runtime_verifies_webhook_signatures() {
    if !jdk_targets_26() {
        eprintln!("SKIPPED: JDK 26+ not available; CI installs one and runs this gate");
        return;
    }
    let dir = ScratchDir::new("gantry-java-webhooks");
    let driver = dir.join("WebhookSmoke.java");
    std::fs::write(&driver, WEBHOOK_DRIVER).unwrap();
    let classes = dir.join("classes");
    std::fs::create_dir_all(&classes).unwrap();
    let javac = Command::new("javac")
        .arg("--release")
        .arg("26")
        .arg("-d")
        .arg(&classes)
        .arg(runtime_src())
        .arg(&driver)
        .output()
        .unwrap();
    assert!(
        javac.status.success(),
        "webhook driver failed to compile:\n{}",
        String::from_utf8_lossy(&javac.stderr)
    );
    let run = Command::new("java")
        .arg("-cp")
        .arg(&classes)
        .arg("WebhookSmoke")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success() && stdout.contains("WEBHOOKS_OK"),
        "webhook verification failed:\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// The VR-7 live-smoke driver: one authenticated `GET /users/me` per auth flow
/// whose credentials are present in the environment (developer token, CCG,
/// OAuth, JWT from a `box_config.json`), then — on the first flow that
/// authenticated — paginate the root folder and upload, download, and delete a
/// file, matching the Go/Rust/TypeScript smokes. A clean no-op when no
/// credentials are set.
// A JDK 25+ compact source file (JEP 512): no explicit class declaration and an
// instance `void main()` — the script-shaped entry point the live smoke wants.
// The gate compiles it with `javac` (the implicit class is named after the file,
// `LiveSmoke`) and runs it with `java -cp classes LiveSmoke`, unchanged.
const LIVE_SMOKE: &str = r#"
import dev.unofficialbox.runtime.Runtime;

int ran = 0;
// Every flow runs before the smoke fails, so one bad credential (an expired
// developer token) doesn't hide the others' results.
java.util.List<String> failed = new java.util.ArrayList<>();
Runtime.Session primary;

void smoke(String name, Runtime.Auth auth) {
    try {
        Runtime.Session session = new Runtime.Session(auth);
        Object me = getJson(session, session.baseUrl("api") + "/users/me");
        System.out.println("OK " + name + ": authenticated as user " + field(me, "id"));
        if (primary == null) {
            primary = session;
        }
        ran++;
    } catch (RuntimeException err) {
        System.out.println("FAIL " + name + ": " + err.getMessage());
        failed.add(name);
    }
}

// Walk the root folder's items, following the marker cursor across pages just
// like the generated paginators do.
void paginate(Runtime.Session session) {
    int seen = 0;
    String marker = "";
    for (int page = 0; page < 100; page++) {
        Runtime.Request request = Runtime.withQuery(
            session.newRequest("GET", session.baseUrl("api") + "/folders/0/items"), "limit", "100");
        if (!marker.isEmpty()) {
            request = Runtime.withQuery(request, "marker", marker);
        }
        Object body = parseJson(fetchOk(session, request));
        if (field(body, "entries") instanceof java.util.List<?> entries) {
            seen += entries.size();
        }
        if (field(body, "next_marker") instanceof String next && !next.isEmpty()) {
            marker = next;
        } else {
            break;
        }
    }
    System.out.println("paginate: walked the root folder, " + seen + " item(s)");
}

// Upload a small file to the root folder, download it back byte-for-byte, then
// delete it — even if the download fails, so a run never leaves an artifact.
void uploadDownloadDelete(Runtime.Session session) {
    byte[] content = "box-gantry live smoke".getBytes(java.nio.charset.StandardCharsets.UTF_8);
    // Unique per run: Box rejects a duplicate name in the same folder (409).
    String name = "box-gantry-smoke-" + java.util.UUID.randomUUID() + ".txt";
    byte[] attributes = ("{\"name\":\"" + name + "\",\"parent\":{\"id\":\"0\"}}")
        .getBytes(java.nio.charset.StandardCharsets.UTF_8);
    Runtime.Request upload = Runtime.withMultipartBody(
        session.newRequest("POST", session.baseUrl("upload") + "/files/content"),
        "attributes", attributes, "file", Runtime.Stream.fromBytes(content));
    Object uploaded = parseJson(fetchOk(session, upload));
    if (!(field(uploaded, "entries") instanceof java.util.List<?> entries) || entries.isEmpty()
            || !(field(entries.get(0), "id") instanceof String fileId)) {
        throw new IllegalStateException("upload: no file id in the response");
    }
    System.out.println("upload: created file " + fileId);
    String fileUrl = session.baseUrl("api") + "/files/" + fileId;
    try {
        byte[] downloaded = fetchOk(session, session.newRequest("GET", fileUrl + "/content"));
        if (!java.util.Arrays.equals(downloaded, content)) {
            throw new IllegalStateException("download: content mismatch (got " + downloaded.length
                + " bytes, want " + content.length + ")");
        }
        System.out.println("download: content round-tripped");
        zip(session, fileId, name);
        etags(session, fileUrl);
        representation(session, fileUrl, "box-gantry live smoke");
        events(session, fileUrl, fileId);
        downscope(session, fileUrl);
    } finally {
        long code = Runtime.statusCode(session.fetch(session.newRequest("DELETE", fileUrl)));
        if (code != 204) {
            throw new IllegalStateException("delete: expected 204, got " + code);
        }
        System.out.println("delete: cleaned up file " + fileId);
    }
}

// Pack the uploaded file into a zip the way the generated downloadZip does:
// create it, then fetch the download_url Box answers with, as returned. The
// archive must be a zip holding the file under its name. Runs before any step
// that renames the file.
void zip(Runtime.Session session, String fileId, String name) {
    byte[] request = ("{\"items\":[{\"type\":\"file\",\"id\":\"" + fileId
        + "\"}],\"download_file_name\":\"box-gantry-smoke\"}")
        .getBytes(java.nio.charset.StandardCharsets.UTF_8);
    Object created = parseJson(fetchOk(session, Runtime.withJsonBody(
        session.newRequest("POST", session.baseUrl("api") + "/zip_downloads"), request)));
    if (!(field(created, "download_url") instanceof String downloadUrl) || downloadUrl.isEmpty()) {
        throw new IllegalStateException("zip: no download_url in the response");
    }
    byte[] archive = fetchOk(session, session.newRequest("GET", downloadUrl));
    boolean zipped = archive.length >= 4 && archive[0] == 'P' && archive[1] == 'K'
        && archive[2] == 3 && archive[3] == 4;
    if (!zipped || !new String(archive, java.nio.charset.StandardCharsets.ISO_8859_1).contains(name)) {
        throw new IllegalStateException("zip: " + archive.length + " bytes, want a zip holding " + name);
    }
    System.out.println("zip: downloaded a " + archive.length + "-byte archive holding " + name);
}

// Drive Box's etag flow on the uploaded file, as the generated `…IfModified`
// variants and the errors guide's re-read-and-retry loop expect it:
// If-None-Match with the current etag is a 304 and a stale one reads the file;
// If-Match with a stale etag is a 412 and the current one writes.
void etags(Runtime.Session session, String fileUrl) {
    if (!(field(getJson(session, fileUrl), "etag") instanceof String etag) || etag.isEmpty()) {
        throw new IllegalStateException("etag: the file read yields no etag");
    }
    // Box's etags are version counters, so this one is well-formed but stale.
    String stale = "9" + etag;

    expectStatus("If-None-Match with the current etag", 304, () -> session.fetch(
        Runtime.withHeader(session.newRequest("GET", fileUrl), "If-None-Match", etag)));
    fetchOk(session, Runtime.withHeader(session.newRequest("GET", fileUrl), "If-None-Match", stale));

    byte[] description = "{\"description\":\"box-gantry etag smoke\"}"
        .getBytes(java.nio.charset.StandardCharsets.UTF_8);
    expectStatus("If-Match with a stale etag", 412, () -> session.fetch(Runtime.withHeader(
        Runtime.withJsonBody(session.newRequest("PUT", fileUrl), description), "If-Match", stale)));
    fetchOk(session, Runtime.withHeader(
        Runtime.withJsonBody(session.newRequest("PUT", fileUrl), description), "If-Match", etag));
    System.out.println("etag: 304 for the current etag, 412 for a stale If-Match, the current one writes");
}

// Fetch the uploaded file's extracted text through representationContent —
// the request the generated getRepresentationContent builds — and find the
// uploaded text in it within two minutes. When Box says it has no such
// representation for the file (none offered, or it could not generate one),
// the check is skipped with a notice rather than failed.
void representation(Runtime.Session session, String fileUrl, String want) {
    Runtime.Request request = Runtime.withHeader(
        Runtime.withQuery(session.newRequest("GET", fileUrl), "fields", "representations"),
        "x-rep-hints", "[extracted_text]");
    String text;
    try {
        text = new String(Runtime.representationContent(session, request, "representations", "",
            java.time.Duration.ofMinutes(2)).toBytes(), java.nio.charset.StandardCharsets.UTF_8);
    } catch (Runtime.BoxApiException err) {
        if (err.getMessage().contains("no representation matches") || err.getMessage().contains("could not generate")) {
            System.out.println("representation: SKIPPED, Box has no extracted text for " + fileUrl
                + " (" + err.getMessage() + ")");
            return;
        }
        throw err;
    }
    if (!text.contains(want)) {
        throw new IllegalStateException("representation: extracted text \"" + text + "\", want it to contain \""
            + want + "\"");
    }
    System.out.println("representation: Box's extracted text holds the uploaded text (" + text.length() + " chars)");
}

// Follow Box's long-poll event stream from "now" while the uploaded file is
// renamed, and see that rename (ITEM_RENAME) within a minute: the long poll
// reports the change, and the page after it carries it.
void events(Runtime.Session session, String fileUrl, String fileId) {
    String api = session.baseUrl("api");
    Runtime.EventStream<Object> stream = new Runtime.EventStream<>(session, "now", new Runtime.EventSource<Object>() {
        @Override
        public Runtime.LongPollServer server() {
            Object listed = parseJson(fetchOk(session, session.newRequest("OPTIONS", api + "/events")));
            if (field(listed, "entries") instanceof java.util.List<?> entries && !entries.isEmpty()
                    && field(entries.get(0), "url") instanceof String url) {
                Object timeout = field(entries.get(0), "retry_timeout");
                return Runtime.LongPollServer.of(url, null, timeout == null ? null : Long.valueOf(timeout.toString()));
            }
            return null;
        }

        @Override
        public Runtime.EventPage<Object> page(String position) {
            Object page = parseJson(fetchOk(session, Runtime.withQuery(
                session.newRequest("GET", api + "/events"), "stream_position", position)));
            java.util.List<Object> events = field(page, "entries") instanceof java.util.List<?> entries
                ? new java.util.ArrayList<>(entries)
                : java.util.List.of();
            Object next = field(page, "next_stream_position");
            return new Runtime.EventPage<>(events, next == null ? null : next.toString());
        }

        @Override
        public String id(Object event) {
            return field(event, "event_id") instanceof String id ? id : null;
        }
    });
    // Rename the file once the stream is waiting, so the long poll reports it;
    // close the stream if a minute passes first.
    Thread renamer = Thread.ofVirtual().start(() -> {
        try {
            Thread.sleep(3000);
        } catch (InterruptedException stopped) {
            return;
        }
        byte[] name = ("{\"name\":\"box-gantry-smoke-renamed-" + java.util.UUID.randomUUID() + ".txt\"}")
            .getBytes(java.nio.charset.StandardCharsets.UTF_8);
        session.fetch(Runtime.withJsonBody(session.newRequest("PUT", fileUrl), name));
    });
    Thread deadline = Thread.ofVirtual().start(() -> {
        try {
            Thread.sleep(60_000);
            stream.close();
        } catch (InterruptedException found) {
            // The rename arrived in time.
        }
    });
    try {
        while (stream.hasNext()) {
            Object event = stream.next();
            if ("ITEM_RENAME".equals(field(event, "event_type")) && fileId.equals(field(field(event, "source"), "id"))) {
                System.out.println("events: the long poll reported ITEM_RENAME for file " + fileId);
                return;
            }
        }
        throw new IllegalStateException("events: no ITEM_RENAME for file " + fileId + " within a minute");
    } finally {
        deadline.interrupt();
        stream.close();
        try {
            renamer.join();
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
        }
    }
}

// Exchange the session's token for one limited to renaming and previewing the
// uploaded file — the form the generated `downscopeToken` posts — then prove
// the narrow token renames the file and Box refuses its delete. The caller's
// cleanup still deletes the file with the original token.
void downscope(Runtime.Session session, String fileUrl) {
    java.util.function.Function<String, String> encode =
        value -> java.net.URLEncoder.encode(value, java.nio.charset.StandardCharsets.UTF_8);
    String form = "grant_type=" + encode.apply("urn:ietf:params:oauth:grant-type:token-exchange")
        + "&subject_token=" + encode.apply(session.accessToken())
        + "&subject_token_type=" + encode.apply("urn:ietf:params:oauth:token-type:access_token")
        + "&scope=" + encode.apply("item_rename item_preview")
        + "&resource=" + encode.apply(fileUrl);
    Runtime.Request exchange = Runtime.withFormBody(
        session.newRequest("POST", session.baseUrl("api_root") + "/oauth2/token"),
        form.getBytes(java.nio.charset.StandardCharsets.UTF_8));
    if (!(field(parseJson(fetchOk(session, exchange)), "access_token") instanceof String token) || token.isEmpty()) {
        throw new IllegalStateException("downscope: the exchange returned no access token");
    }
    Runtime.Session narrow = new Runtime.Session(Runtime.developerToken(token));

    byte[] name = ("{\"name\":\"box-gantry-smoke-downscoped-" + java.util.UUID.randomUUID() + ".txt\"}")
        .getBytes(java.nio.charset.StandardCharsets.UTF_8);
    fetchOk(narrow, Runtime.withJsonBody(narrow.newRequest("PUT", fileUrl), name));
    try {
        narrow.fetch(narrow.newRequest("DELETE", fileUrl));
    } catch (Runtime.BoxApiException refused) {
        if (refused.statusCode() != 401 && refused.statusCode() != 403) {
            throw new IllegalStateException("downscope: deleting with the narrow token: "
                + refused.getMessage() + ", want a 401/403 refusal");
        }
        System.out.println("downscope: the narrow token renamed the file; its delete was refused ("
            + refused.getMessage() + ")");
        return;
    }
    throw new IllegalStateException("downscope: deleting with the narrow token succeeded, want a refusal");
}

// Run a call that must throw the runtime's error for `status`.
void expectStatus(String what, int status, Runnable call) {
    try {
        call.run();
    } catch (Runtime.BoxApiException err) {
        if (err.statusCode() != status || (status == 412 && !err.isPreconditionFailed())) {
            throw new IllegalStateException("etag: " + what + ": " + err.getMessage() + ", want " + status);
        }
        return;
    }
    throw new IllegalStateException("etag: " + what + ": succeeded, want " + status);
}

// Act as a managed user through `withAsUserHeader`: `/users/me` must answer as
// that user. It needs an admin token from an app allowed to "make API calls
// using the as-user header" and a managed user to act as; without them Box
// answers 403 or lists no one, and the check is skipped with a notice rather
// than failed (the account's setup, not the runtime, is missing).
void asUser(Runtime.Session session) {
    Object meId = field(getJson(session, session.baseUrl("api") + "/users/me"), "id");
    Object users;
    try {
        users = getJson(session, session.baseUrl("api") + "/users?user_type=managed&limit=100");
    } catch (Runtime.BoxApiException err) {
        skipAsUser("listing managed users", err);
        return;
    }
    String target = null;
    if (field(users, "entries") instanceof java.util.List<?> entries) {
        for (Object user : entries) {
            if ("active".equals(field(user, "status")) && field(user, "id") instanceof String id
                    && !id.equals(meId)) {
                target = id;
                break;
            }
        }
    }
    if (target == null) {
        System.out.println("as-user: SKIPPED, no other active managed user to act as");
        return;
    }
    Runtime.Session acting = session.withAsUserHeader(target);
    Object acted;
    try {
        acted = getJson(acting, acting.baseUrl("api") + "/users/me");
    } catch (Runtime.BoxApiException err) {
        skipAsUser("acting as user " + target, err);
        return;
    }
    if (!target.equals(field(acted, "id"))) {
        throw new IllegalStateException("as-user: /users/me answered user " + field(acted, "id")
            + ", want " + target);
    }
    System.out.println("as-user: acted as user " + target);
}

// Box's 403 means the app or token may not act as users: log a skip. Any other
// failure fails the smoke.
void skipAsUser(String what, Runtime.BoxApiException err) {
    if (err.statusCode() != 403) {
        throw err;
    }
    System.out.println("as-user: SKIPPED, " + what + " got " + err.getMessage()
        + " (enable \"Make API calls using the as-user header\" and use an admin token)");
}

Object getJson(Runtime.Session session, String url) {
    return parseJson(fetchOk(session, session.newRequest("GET", url)));
}

// Run a request and return the body, failing on a non-2xx status.
byte[] fetchOk(Runtime.Session session, Runtime.Request request) {
    Runtime.Response response = session.fetch(request);
    long code = Runtime.statusCode(response);
    byte[] body = Runtime.responseBytes(response);
    if (code < 200 || code >= 300) {
        throw new IllegalStateException("status " + code + ": "
            + new String(body, java.nio.charset.StandardCharsets.UTF_8).strip());
    }
    return body;
}

Object field(Object object, String key) {
    return object instanceof java.util.Map<?, ?> map ? map.get(key) : null;
}

// A minimal JSON reader: the runtime keeps its own parser private and ships no
// dependencies, so the smoke carries just enough to read Box's responses.
// Objects become Maps, arrays Lists, strings Strings, `null` null; any other
// scalar is kept as its raw token text.
String json;
int at;

Object parseJson(byte[] body) {
    json = new String(body, java.nio.charset.StandardCharsets.UTF_8);
    at = 0;
    return jsonValue();
}

Object jsonValue() {
    skipSpace();
    char c = json.charAt(at);
    if (c == '{') {
        java.util.Map<String, Object> map = new java.util.LinkedHashMap<>();
        at++;
        if (peek() == '}') {
            at++;
            return map;
        }
        do {
            skipSpace();
            String key = jsonString();
            skipSpace();
            at++; // ':'
            map.put(key, jsonValue());
            skipSpace();
        } while (json.charAt(at++) == ',');
        return map;
    }
    if (c == '[') {
        java.util.List<Object> list = new java.util.ArrayList<>();
        at++;
        if (peek() == ']') {
            at++;
            return list;
        }
        do {
            list.add(jsonValue());
            skipSpace();
        } while (json.charAt(at++) == ',');
        return list;
    }
    if (c == '"') {
        return jsonString();
    }
    int start = at;
    while (at < json.length() && ",}] \t\r\n".indexOf(json.charAt(at)) < 0) {
        at++;
    }
    String token = json.substring(start, at);
    return token.equals("null") ? null : token;
}

String jsonString() {
    StringBuilder out = new StringBuilder();
    at++; // opening quote
    for (char c = json.charAt(at++); c != '"'; c = json.charAt(at++)) {
        if (c != '\\') {
            out.append(c);
            continue;
        }
        char escaped = json.charAt(at++);
        switch (escaped) {
            case 'b' -> out.append('\b');
            case 'f' -> out.append('\f');
            case 'n' -> out.append('\n');
            case 'r' -> out.append('\r');
            case 't' -> out.append('\t');
            case 'u' -> {
                out.append((char) Integer.parseInt(json.substring(at, at + 4), 16));
                at += 4;
            }
            default -> out.append(escaped);
        }
    }
    return out.toString();
}

char peek() {
    skipSpace();
    return json.charAt(at);
}

void skipSpace() {
    while (at < json.length() && Character.isWhitespace(json.charAt(at))) {
        at++;
    }
}

String env(String name) {
    String value = System.getenv(name);
    return value == null ? "" : value;
}

void main() throws Exception {
    if (!env("BOX_DEVELOPER_TOKEN").isEmpty()) {
        smoke("developer", Runtime.developerToken(env("BOX_DEVELOPER_TOKEN")));
    }
    String clientId = env("BOX_CLIENT_ID");
    String clientSecret = env("BOX_CLIENT_SECRET");
    if (!clientId.isEmpty() && !clientSecret.isEmpty() && !env("BOX_ENTERPRISE_ID").isEmpty()) {
        smoke("ccg", Runtime.clientCredentials(
            Runtime.CcgConfig.enterprise(clientId, clientSecret, env("BOX_ENTERPRISE_ID"))));
    }
    if (!clientId.isEmpty() && !clientSecret.isEmpty() && !env("BOX_OAUTH_REFRESH_TOKEN").isEmpty()) {
        smoke("oauth", Runtime.oauth(
            new Runtime.OAuthConfig(clientId, clientSecret), env("BOX_OAUTH_REFRESH_TOKEN")));
    }
    if (!env("BOX_JWT_CONFIG").isEmpty()) {
        String json = java.nio.file.Files.readString(java.nio.file.Path.of(env("BOX_JWT_CONFIG")));
        smoke("jwt", Runtime.jwt(Runtime.JwtConfig.fromBoxConfig(json)));
    }
    if (ran == 0 && failed.isEmpty()) {
        System.out.println("LIVESMOKE_SKIP (no credentials)");
        return;
    }
    if (primary == null) {
        System.out.println("LIVESMOKE_FAIL no auth flow yielded a usable token: " + failed);
        System.exit(1);
    }
    paginate(primary);
    uploadDownloadDelete(primary);
    asUser(primary);
    if (!failed.isEmpty()) {
        System.out.println("LIVESMOKE_FAIL " + failed);
        System.exit(1);
    }
    System.out.println("LIVESMOKE_OK");
}
"#;

/// VR-7: the live smoke **compiles** under the standard gate (so it can't rot),
/// and **runs** against a real Box account when credentials are present in the
/// environment — mirroring the Rust runtime's `#[ignore]`d live smoke. In CI (no
/// credentials) it is compiled but not run. The driver is a JDK 25+ **compact
/// source file** (JEP 512) — no class boilerplate, an instance `void main()` —
/// which the gate's `javac` + `java -cp classes LiveSmoke` flow runs unchanged
/// (the implicit class is named after the file). This is why the runtime floor
/// is Java 26 (D-180); the compact form needs ≥ 25.
#[test]
fn the_live_smoke_compiles_and_runs_when_credentialed() {
    if !jdk_targets_26() {
        eprintln!("SKIPPED: JDK 26+ not available; CI installs one and runs this gate");
        return;
    }
    let dir = ScratchDir::new("gantry-java-live");
    let smoke = dir.join("LiveSmoke.java");
    std::fs::write(&smoke, LIVE_SMOKE).unwrap();
    let classes = dir.join("classes");
    std::fs::create_dir_all(&classes).unwrap();

    let javac = Command::new("javac")
        .arg("--release")
        .arg("26")
        .arg("-d")
        .arg(&classes)
        .arg(runtime_src())
        .arg(&smoke)
        .output()
        .unwrap();
    assert!(
        javac.status.success(),
        "the live smoke failed to compile:\n{}",
        String::from_utf8_lossy(&javac.stderr)
    );

    // Run only when at least one flow's credentials are set; otherwise the
    // compile above is the signal (the smoke can't rot), mirroring `#[ignore]`.
    let credentialed = ["BOX_DEVELOPER_TOKEN", "BOX_CLIENT_ID", "BOX_JWT_CONFIG"]
        .iter()
        .any(|var| std::env::var(var).is_ok_and(|v| !v.is_empty()));
    if !credentialed {
        eprintln!("SKIPPED live run: no BOX_* credentials in the environment (compiled clean)");
        return;
    }

    let run = Command::new("java")
        .arg("-cp")
        .arg(&classes)
        .arg("LiveSmoke")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    // Echo the driver's per-step log so a green run shows what it covered.
    eprint!("{stdout}");
    assert!(
        run.status.success() && stdout.contains("LIVESMOKE_OK"),
        "live smoke failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
}
