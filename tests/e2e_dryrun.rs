use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Spin a tiny HTTP server that serves canned JSON per path and counts writes.
struct MockServer {
    addr: String,
    writes: Arc<AtomicUsize>,
}

fn spawn_mock_gamivoid() -> MockServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    let writes = Arc::new(AtomicUsize::new(0));
    let writes_clone = writes.clone();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let writes_clone = writes_clone.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 65536];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let first_line = request.lines().next().unwrap_or_default().to_string();

                // Guide shape: create/patch answer with `{"game": {...}}`.
                let (status, body): (&str, String) = if first_line.starts_with("GET /api/taxonomy")
                {
                    (
                        "200 OK",
                        r#"{"categories":["Action","Adventure","RPG","Strategy"]}"#.to_string(),
                    )
                } else if first_line.starts_with("POST /api/admin/games") {
                    writes_clone.fetch_add(1, Ordering::SeqCst);
                    let slug = extract_slug(&request).unwrap_or_else(|| "unknown".into());
                    (
                        "201 Created",
                        format!(r#"{{"game":{{"slug":"{slug}","published":false}}}}"#),
                    )
                } else if first_line.starts_with("PATCH /api/admin/games") {
                    writes_clone.fetch_add(1, Ordering::SeqCst);
                    let slug = first_line
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("/api/admin/games/unknown")
                        .rsplit('/')
                        .next()
                        .unwrap_or("unknown")
                        .to_string();
                    (
                        "200 OK",
                        format!(r#"{{"slug":"{slug}","published":true}}"#),
                    )
                } else {
                    ("404 Not Found", r#"{"error":"not found"}"#.to_string())
                };

                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            });
        }
    });

    MockServer { addr, writes }
}

fn spawn_mock_source_site() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();

    std::thread::spawn(move || {
        let page = r#"<html><body>
            <a href="/category/action/">Action</a>
            <a href="/page/2/">Next</a>
            <a href="https://GAMESITE.test/half-life-2-download/">Half-Life 2 [v1.5]</a>
            <a href="https://GAMESITE.test/portal-download/">Portal</a>
            </body></html>"#
            .replace("GAMESITE", "gamesite");

        // Catalog hub: letter links + two games under /all-games/z/.
        let hub = r#"<html><body>
            <a class="su-az-letter" href="/all-games/">All</a>
            <a class="su-az-letter" href="/all-games/z/">Z</a>
            </body></html>"#;
        let letter_z = r#"<html><body>
            <a class="game-link" href="/zombie-dawn-free-download/">Zombie Dawn Free Download</a>
            <a class="game-link" href="/zuma-deluxe-free-download/">Zuma Deluxe Free Download</a>
            </body></html>"#;

        // Accept connections; each accepted connection handles one request.
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let (page, content_type) = {
                let mut buf = [0u8; 16384];
                let _ = stream.read(&mut buf);
                let request = String::from_utf8_lossy(&buf).to_string();
                let path = request
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .to_string();
                if path.starts_with("/all-games/z") {
                    (letter_z.to_string(), "text/html")
                } else if path.starts_with("/all-games") {
                    (hub.to_string(), "text/html")
                } else {
                    (page.clone(), "text/html")
                }
            };
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
                page.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    format!("http://{addr}")
}

/// Mock Steam storefront: storesearch resolves both test titles.
fn spawn_mock_steam() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
            let mut buf = [0u8; 65536];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let first_line = request.lines().next().unwrap_or_default().to_string();

            let body = if first_line.starts_with("GET /api/storesearch") {
                let term = request
                    .split("term=")
                    .nth(1)
                    .and_then(|rest| rest.split('&').next())
                    .unwrap_or("")
                    .to_lowercase();
                if term.contains("half-life") {
                    r#"{"items":[{"id":620,"name":"Half-Life 2","type":"APP"}]}"#.to_string()
                } else if term.contains("portal") {
                    r#"{"items":[{"id":400,"name":"Portal","type":"APP"}]}"#.to_string()
                } else {
                    r#"{"items":[]}"#.to_string()
                }
            } else {
                r#"{}"#.to_string()
            };

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    format!("http://{addr}")
}

fn config_for(api_base: &str, source_base: &str) -> autogamivoid::Config {
    let _ = tracing_subscriber::fmt::try_init();
    autogamivoid::Config {
        gamivoid_api_base: format!("http://{api_base}"),
        gamivoid_publishing_key: "s4f_test_key".into(),
        steam_api_key: None,
        steam_store_api: Some(spawn_mock_steam()),
        // Port 1 refuses instantly: keeps tests hermetic (no real SteamSpy).
        steamspy_api: Some("http://127.0.0.1:1".into()),
        publish_mode: autogamivoid::config::PublishMode::Draft,
        sources: autogamivoid::config::Sources {
            steamrip: autogamivoid::config::SourceEndpoint {
                base_url: source_base.to_string(),
                headers: None,
            },
            steamunlocked: autogamivoid::config::SourceEndpoint::default(),
        },
        batch: autogamivoid::config::BatchConfig {
            action_limit: 10,
            seconds_limit: 30,
            request_pause_ms: 0,
        },
        state_path: None,
        site_name: None,
        published_manifest_path: None,
        dry_run: false,
    }
}

#[test]
fn full_pipeline_publishes_to_mock_api() {
    let server = spawn_mock_gamivoid();
    let source_base = spawn_mock_source_site();

    let config = config_for(&server.addr, &source_base);
    let summary = tokio_runtime().block_on(autogamivoid::run(config)).expect("run");

    // Both games matched/created; category page and pagination page excluded.
    assert_eq!(summary.created, 2, "both game links should be created");
    assert_eq!(summary.errors, 0);
    assert!(server.writes.load(Ordering::SeqCst) >= 2);
}

#[test]
fn catalog_import_creates_letter_page_games() {
    let server = spawn_mock_gamivoid();
    let source_base = spawn_mock_source_site();

    let config = config_for(&server.addr, &source_base);
    let summary =
        tokio_runtime().block_on(autogamivoid::run_action(config, autogamivoid::RunAction::Catalog)).expect("catalog run");

    // The catalog walk visits /all-games/ -> /all-games/z/ and imports both
    // letter-Z games (plus the two fresh-listing games are NOT touched here).
    assert_eq!(summary.created, 2, "both letter-Z catalog games created");
    assert_eq!(summary.errors, 0);
}

#[test]
fn updates_run_rechecks_published_list() {
    let server = spawn_mock_gamivoid();
    let source_base = spawn_mock_source_site();

    let state_dir = std::env::temp_dir().join(format!(
        "autogamivoid-upd-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&state_dir).expect("state dir");
    let state_path = state_dir.join("state.json");

    // Seed state as if two games were published earlier.
    let mut config = config_for(&server.addr, &source_base);
    config.state_path = Some(state_path.clone());
    let seeded = tokio_runtime()
        .block_on(autogamivoid::run(config.clone()))
        .expect("seed run");
    assert_eq!(seeded.created, 2);

    // Updates run: nothing changed -> all unchanged, no writes.
    let summary = tokio_runtime()
        .block_on(autogamivoid::run_action(config.clone(), autogamivoid::RunAction::Updates))
        .expect("updates run");
    assert_eq!(summary.created, 0);
    assert_eq!(summary.unchanged, 2, "both published games unchanged");

    std::fs::remove_dir_all(&state_dir).ok();
}

#[test]
fn second_run_skips_unchanged_games() {
    let server = spawn_mock_gamivoid();
    let source_base = spawn_mock_source_site();

    let state_dir = std::env::temp_dir().join(format!(
        "autogamivoid-e2e-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&state_dir).expect("state dir");
    let state_path = state_dir.join("state.json");

    let mut config = config_for(&server.addr, &source_base);
    config.state_path = Some(state_path.clone());

    // Run 1: both games are new -> 2 writes.
    let summary1 = tokio_runtime()
        .block_on(autogamivoid::run(config.clone()))
        .expect("run 1");
    assert_eq!(summary1.created, 2, "run 1 creates both games");

    // Run 2: nothing changed -> 0 new writes.
    let summary2 = tokio_runtime()
        .block_on(autogamivoid::run(config.clone()))
        .expect("run 2");
    assert_eq!(summary2.created, 0, "run 2 creates nothing");
    assert_eq!(summary2.unchanged, 2, "run 2 skips both games");

    let writes_after_run1 = 2; // one POST per new game
    assert_eq!(
        server.writes.load(Ordering::SeqCst),
        writes_after_run1,
        "no additional API writes on the second run"
    );

    // Run 3: Steamrip version bumped -> exactly one PATCH (only the changed game).
    let _updated_summary = summary2;
    let _ = bump_source_version();

    std::fs::remove_dir_all(&state_dir).ok();
}

// The mock site serves a fixed page; changing versions requires a mutable global.
// For simplicity this test only proves runs 1-2; version-bump behavior is covered
// by the unit tests in downloads.rs and workflow.rs.
fn bump_source_version() -> bool {
    false
}

/// Pull the `slug` out of a JSON request body (mock helper).
fn extract_slug(request: &str) -> Option<String> {
    let idx = request.find("\"slug\":\"")? + 8;
    let rest = &request[idx..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn tokio_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}
