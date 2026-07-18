//! Tests for the hand-written Java runtime (`runtimes/java/gantryruntime`),
//! driven through the real `javac`/`java` toolchain (the runtime is plain JDK
//! Java, so there is no build tool — the swap gate and these tests compile its
//! source directly). Two signals: the runtime compiles warning-clean under the
//! same strict gate as the generated SDK, and it *behaves* — an embedded
//! `HttpServer` exercises the fetch retry loop and the CCG auth flow end to end.
//!
//! Skips cleanly when the JDK is absent (local dev); CI installs one.

use std::path::PathBuf;
use std::process::Command;

fn runtime_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../runtimes/java/gantryruntime/src/main/java/com/box/sdk/runtime/Runtime.java")
}

fn jdk_available() -> bool {
    Command::new("javac").arg("-version").output().is_ok()
        && Command::new("java").arg("-version").output().is_ok()
}

/// The runtime compiles warning-clean under `javac --release 21 -Xlint:all
/// -Werror` — the same bar as the generated SDK (VR-1.6), on its own.
#[test]
fn the_runtime_compiles_standalone() {
    if Command::new("javac").arg("-version").output().is_err() {
        eprintln!("SKIPPED: javac not available; CI installs a JDK and runs this gate");
        return;
    }
    let dir = std::env::temp_dir().join(format!("gantry-java-rt-std-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let javac = Command::new("javac")
        .arg("--release")
        .arg("21")
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
/// (a 429 is retried and then succeeds, hitting the endpoint twice) and the CCG
/// flow (a token is fetched from the token endpoint and threaded as
/// `Authorization: Bearer …` on the next call).
const DRIVER: &str = r#"
import com.sun.net.httpserver.HttpServer;
import com.box.sdk.runtime.Runtime;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.atomic.AtomicInteger;

public final class RuntimeSmoke {
    static void check(boolean cond, String msg) {
        if (!cond) {
            System.out.println("FAIL: " + msg);
            System.exit(1);
        }
    }

    static void respond(com.sun.net.httpserver.HttpExchange ex, int status, String body) throws java.io.IOException {
        byte[] bytes = body.getBytes(StandardCharsets.UTF_8);
        ex.sendResponseHeaders(status, bytes.length);
        ex.getResponseBody().write(bytes);
        ex.close();
    }

    public static void main(String[] args) throws Exception {
        HttpServer server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        AtomicInteger retryHits = new AtomicInteger();

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

        server.stop(0);
        System.out.println("RUNTIME_OK");
    }
}
"#;

/// The fetch retry loop and the CCG auth flow work end to end against a real
/// (in-process) HTTP server.
#[test]
fn the_runtime_retries_and_authenticates() {
    if !jdk_available() {
        eprintln!("SKIPPED: JDK not available; CI installs one and runs this gate");
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
        .arg("21")
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
