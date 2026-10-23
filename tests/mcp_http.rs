// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end: drive `dawkey mcp --headless --http` over MCP's streamable
//! HTTP transport, and check that it refuses what it should.

use dawkey_testkit::Harness;
use serde_json::json;

const DAWKEY: &str = env!("CARGO_BIN_EXE_dawkey");
const TOKEN: &str = "e2e-test-token-0123456789abcdef";

async fn start() -> Harness {
    Harness::headless_http(DAWKEY, std::iter::empty::<&str>(), TOKEN)
        .await
        .expect("dawkey starts")
}

#[tokio::test]
async fn drives_a_project_over_http() {
    let dawkey = start().await;
    assert_eq!(dawkey.server_name().as_deref(), Some("dawkey"));
    let url = dawkey.url().unwrap().to_owned();
    assert!(url.starts_with("http://127.0.0.1:") && url.ends_with("/mcp"));

    let tools = dawkey.tools().await.unwrap();
    assert!(tools.iter().any(|t| t.name == "add_track"));
    let added = dawkey
        .call("add_track", json!({ "name": "Drums" }))
        .await
        .unwrap();
    let drums = added["track"].clone();

    // A second client sees the same project, and its calls are undo steps
    // like any other.
    let other = Harness::connect_http(&url, TOKEN).await.unwrap();
    let state = other.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["project"]["tracks"][0]["name"], "Drums");
    other
        .call("rename_track", json!({ "track": drums, "name": "Kit" }))
        .await
        .unwrap();
    dawkey.call("undo", json!({})).await.unwrap();
    let state = dawkey.call("get_state", json!({})).await.unwrap();
    assert_eq!(state["project"]["tracks"][0]["name"], "Drums");

    // Bad input is still a tool error, and images still come back.
    let message = dawkey
        .call_err("remove_track", json!({ "track": 99 }))
        .await
        .unwrap();
    assert!(message.contains("99"), "{message}");
    let shot = dawkey
        .call_raw("ui_screenshot", json!({ "scale": 0.25 }))
        .await
        .unwrap();
    assert!(shot.content.iter().any(|c| c.as_image().is_some()));

    other.shutdown().await.unwrap();
    dawkey.shutdown().await.unwrap();
}

#[tokio::test]
async fn refuses_requests_without_the_token() {
    let dawkey = start().await;
    let url = dawkey.url().unwrap().to_owned();
    let http = reqwest::Client::new();
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "curl", "version": "1" }
        }
    })
    .to_string();
    let post = |url: &str| {
        http.post(url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(initialize.clone())
    };

    let refused = post(&url).send().await.unwrap();
    assert_eq!(refused.status(), 401);
    assert_eq!(refused.headers()["www-authenticate"], "Bearer");
    let refused = post(&url)
        .bearer_auth("not-the-token")
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 401);
    let refused = post(&url)
        .header("authorization", format!("Basic {TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 401);

    // DNS rebinding: a page on another host that resolves to us.
    let port = url.trim_end_matches("/mcp").rsplit(':').next().unwrap();
    let refused = post(&url)
        .bearer_auth(TOKEN)
        .header("host", format!("attacker.example:{port}"))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 403);
    // A web page elsewhere.
    let refused = post(&url)
        .bearer_auth(TOKEN)
        .header("origin", "https://attacker.example")
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 403);

    let elsewhere = url.replace("/mcp", "/other");
    let missing = post(&elsewhere).bearer_auth(TOKEN).send().await.unwrap();
    assert_eq!(missing.status(), 404);

    // With the token, from this computer: MCP answers.
    let accepted = post(&url)
        .bearer_auth(TOKEN)
        .header("origin", "http://localhost:6274")
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), 200);

    dawkey.shutdown().await.unwrap();
}

#[test]
fn serves_only_loopback_addresses() {
    let output = std::process::Command::new(DAWKEY)
        .args(["mcp", "--headless", "--http", "0.0.0.0:0"])
        .env("DAWKEY_MCP_TOKEN", TOKEN)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("loopback"), "{stderr}");
    assert!(output.stdout.is_empty());
}
