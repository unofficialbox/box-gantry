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
    let dir = std::env::temp_dir().join(format!("gantry-java-rt-std-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
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
    let _ = std::fs::remove_dir_all(&dir);
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

        // JWT: a signed assertion the server verifies against the key.
        Runtime.Auth jwt = Runtime.jwt(
            Runtime.JwtConfig.enterprise("cid", "csecret", "kid1", pem, null, "ent1")
                .tokenUrl(base + "/jwt"));
        Runtime.Session jwtSession = new Runtime.Session(jwt);
        check(jwtSession.accessToken().equals("jwttok"), "jwt token");

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
    let dir = std::env::temp_dir().join(format!("gantry-java-rt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
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
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        run.status.success() && stdout.contains("RUNTIME_OK"),
        "runtime behavior check failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// The VR-7 live-smoke driver: one authenticated `GET /users/me` per auth flow
/// whose credentials are present in the environment (developer token, CCG,
/// OAuth, JWT from a `box_config.json`). A clean no-op when none are set.
// A JDK 25+ compact source file (JEP 512): no explicit class declaration and an
// instance `void main()` — the script-shaped entry point the live smoke wants.
// The gate compiles it with `javac` (the implicit class is named after the file,
// `LiveSmoke`) and runs it with `java -cp classes LiveSmoke`, unchanged.
const LIVE_SMOKE: &str = r#"
import dev.unofficialbox.runtime.Runtime;

int ran = 0;

void smoke(String name, Runtime.Auth auth) {
    Runtime.Session session = new Runtime.Session(auth);
    Runtime.Response response = session.fetch(session.newRequest("GET", session.baseUrl("api") + "/users/me"));
    long code = Runtime.statusCode(response);
    if (code != 200) {
        System.out.println("FAIL " + name + ": status " + code + " "
            + new String(Runtime.responseBytes(response), java.nio.charset.StandardCharsets.UTF_8));
        System.exit(1);
    }
    System.out.println("OK " + name);
    ran++;
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
    System.out.println(ran == 0 ? "LIVESMOKE_SKIP (no credentials)" : "LIVESMOKE_OK");
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
    let dir = std::env::temp_dir().join(format!("gantry-java-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
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
        let _ = std::fs::remove_dir_all(&dir);
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
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        run.status.success() && stdout.contains("LIVESMOKE_OK"),
        "live smoke failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
}
