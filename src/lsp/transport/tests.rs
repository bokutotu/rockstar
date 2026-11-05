use super::*;
use futures_lite::future::block_on;
use std::{
    fs,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "rockstar-lsp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, text: &str) {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn messages(&self) -> Vec<Value> {
        fs::read_to_string(self.0.join("lsp-log.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect()
    }
    fn mock(&self) -> Arc<ServerSession> {
        let (events, _) = async_channel::unbounded();
        ServerSession::start(
            ServerKind::Rust,
            self.0.clone(),
            CommandSpec {
                program: "python3".into(),
                args: vec![concat!(env!("CARGO_MANIFEST_DIR"), "/tests/mock_lsp.py").into()],
            },
            events,
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn wait(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < end, "LSP timeout");
        thread::sleep(Duration::from_millis(5));
    }
}
fn ready(session: &ServerSession) {
    wait(|| {
        if let ServerState::Failed(error) = session.state() {
            panic!("{error}");
        }
        matches!(session.state(), ServerState::Ready { .. })
    });
}
fn stop(session: &ServerSession) {
    let done = session.stop();
    wait(|| done.is_closed());
}

#[test]
fn framing_counts_bytes_accepts_extra_headers_and_rejects_malformed_input() {
    let value = json!({"jsonrpc":"2.0","result":"日本🎸"});
    let mut bytes = b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n".to_vec();
    write_message(&mut bytes, &value).unwrap();
    let mut reader = std::io::Cursor::new(bytes);
    assert_eq!(read_message(&mut reader).unwrap(), Some(value));
    assert_eq!(read_message(&mut reader).unwrap(), None);
    for bad in [
        b"Content-Length: 999999999\r\n\r\n".as_slice(),
        b"Bad\r\n\r\n",
        b"Content-Length: 5\r\n\r\n{}",
        b"\r\n",
        b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}",
    ] {
        assert!(read_message(&mut std::io::Cursor::new(bad)).is_err());
    }
}
#[test]
fn stdio_negotiation_reordered_responses_errors_cancellation_and_shutdown() {
    let fixture = Fixture::new();
    let session = fixture.mock();
    // Requests may be queued while initialization is still in progress.
    let slow = session.request("test/reverse", json!({"value":"slow","delay":0.15}));
    let fast = session.request("test/reverse", json!({"value":"fast"}));
    assert_eq!(block_on(fast.recv()).unwrap(), json!("fast"));
    assert_eq!(block_on(slow.recv()).unwrap(), json!("slow"));
    assert_eq!(session.encoding(), Encoding::Utf16);
    assert!(
        block_on(session.request("test/error", json!({})).recv())
            .unwrap_err()
            .to_string()
            .contains("intentional error")
    );
    let cancelled = session.request("test/reverse", json!({"value":"cancel","delay":2}));
    wait(|| {
        fixture
            .messages()
            .iter()
            .any(|v| v["params"]["value"] == "cancel")
    });
    drop(cancelled);
    wait(|| {
        fixture
            .messages()
            .iter()
            .any(|v| v["method"] == "$/cancelRequest")
    });
    stop(&session);
    let methods = fixture
        .messages()
        .iter()
        .filter_map(|v| v["method"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert_eq!(methods.first().unwrap(), "initialize");
    assert_eq!(methods.last().unwrap(), "exit");
    assert!(methods.contains(&"shutdown".into()));
}
#[test]
fn a_crashed_server_fails_pending_requests_and_reports_failure() {
    let fixture = Fixture::new();
    let session = fixture.mock();
    ready(&session);
    let pending = session.request("test/reverse", json!({"value":1,"delay":2}));
    let crash = session.request("test/crash", json!({}));
    assert!(block_on(crash.recv()).is_err());
    assert!(block_on(pending.recv()).is_err());
    wait(|| matches!(session.state(), ServerState::Failed(_)));
    assert!(block_on(session.request("test/reverse", json!({})).recv()).is_err());
}
#[test]
fn shutdown_is_bounded_even_when_the_server_stops_reading_stdin() {
    let fixture = Fixture::new();
    fixture.write("mock-options.json", r#"{"stall_after_initialize":true}"#);
    let session = fixture.mock();
    ready(&session);
    session
        .notify("test/large", json!({"text":"x".repeat(2*1024*1024)}))
        .unwrap();
    let start = Instant::now();
    stop(&session);
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[test]
#[ignore = "requires language servers from the current devShell; set ROCKSTAR_LSP_LANGUAGE to select one"]
fn real_language_servers() {
    let selection = std::env::var("ROCKSTAR_LSP_LANGUAGE").ok();
    let fixtures = [
        (
            "rust",
            ServerKind::Rust,
            "src/lib.rs",
            "rust",
            "fn target() -> i32 { 1 }\npub fn caller() -> i32 { target() }\n",
        ),
        (
            "haskell",
            ServerKind::Haskell,
            "Main.hs",
            "haskell",
            "module Main where\ntarget :: Int\ntarget = 1\nmain = print target\n",
        ),
        (
            "typescript",
            ServerKind::TypeScript,
            "main.ts",
            "typescript",
            "function target(value: number) { return value; }\nconst caller = target(1);\n",
        ),
        (
            "terraform",
            ServerKind::Terraform,
            "main.tf",
            "terraform",
            "variable \"target\" { default = 1 }\noutput \"caller\" { value = var.target }\n",
        ),
        (
            "c",
            ServerKind::Ccls,
            "main.c",
            "c",
            "int target(void) { return 1; }\nint caller(void) { return target(); }\n",
        ),
        (
            "cpp",
            ServerKind::Ccls,
            "main.cpp",
            "cpp",
            "int target() { return 1; }\nint caller() { return target(); }\n",
        ),
        (
            "cuda",
            ServerKind::Ccls,
            "main.cu",
            "cuda",
            "int target() { return 1; }\nint caller() { return target(); }\n",
        ),
        (
            "nix",
            ServerKind::Nix,
            "main.nix",
            "nix",
            "let target = 1; in target\n",
        ),
    ];
    for (name, kind, file, language_id, text) in fixtures {
        if selection
            .as_deref()
            .is_some_and(|selected| selected != name)
        {
            continue;
        }
        let fixture = Fixture::new();
        fixture.write(file, text);
        if name == "rust" {
            fixture.write(
                "Cargo.toml",
                "[package]\nname = \"lsp_probe\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            );
        }
        if name == "haskell" {
            fixture.write("hie.yaml", "cradle:\n  direct:\n    arguments: [Main.hs]\n");
        }
        if name == "typescript" {
            fixture.write(
                "tsconfig.json",
                "{\"compilerOptions\":{\"strict\":true},\"include\":[\"main.ts\"]}",
            );
        }
        if kind == ServerKind::Ccls {
            let mut args = vec![
                "clang".to_owned(),
                "-c".to_owned(),
                fixture.0.join(file).display().to_string(),
            ];
            if name == "cuda" {
                args.extend([
                    "--cuda-host-only".into(),
                    "-nocudainc".into(),
                    "-nocudalib".into(),
                ]);
            }
            fixture.write(
                "compile_commands.json",
                &json!([{"directory":fixture.0,"file":fixture.0.join(file),"arguments":args}])
                    .to_string(),
            );
        }
        let (events, rx) = async_channel::unbounded();
        let session = ServerSession::start(kind, fixture.0.clone(), kind.into(), events);
        ready(&session);
        let uri = language::uri(&fixture.0.join(file)).unwrap();
        session.notify("textDocument/didOpen",json!({"textDocument":{"uri":uri,"languageId":language_id,"version":1,"text":text}})).unwrap();
        let position = session
            .encoding()
            .position(text, text.rfind("target").unwrap())
            .unwrap();
        // Wait for project loading. Some servers initially report content-modified/not-ready.
        let mut hover = Value::Null;
        let mut hover_ok = false;
        for _ in 0..30 {
            thread::sleep(Duration::from_millis(200));
            if let Ok(result) = block_on(
                session
                    .request(
                        "textDocument/hover",
                        json!({"textDocument":{"uri":uri},"position":position}),
                    )
                    .recv(),
            ) {
                hover = result;
                hover_ok = true;
                if !hover.is_null() || name == "nix" {
                    break;
                }
            }
        }
        assert!(
            hover_ok && (name == "nix" || !hover.is_null()),
            "{name}: no hover; events: {:?}",
            std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>()
        );
        if session.supports("definitionProvider") {
            let result = block_on(
                session
                    .request(
                        "textDocument/definition",
                        json!({"textDocument":{"uri":uri},"position":position}),
                    )
                    .recv(),
            )
            .unwrap();
            assert!(
                !result.is_null() && result.as_array().is_none_or(|a| !a.is_empty()),
                "{name}: no definition"
            );
        }
        let caps = session.capabilities();
        if session.supports("completionProvider") {
            let result = block_on(session.request("textDocument/completion", json!({"textDocument":{"uri":uri},"position":position,"context":{"triggerKind":1}})).recv()).unwrap();
            if !result.is_null() {
                serde_json::from_value::<lsp_types::CompletionResponse>(result).unwrap();
            }
        }
        if session.supports("documentHighlightProvider") {
            let result = block_on(
                session
                    .request(
                        "textDocument/documentHighlight",
                        json!({"textDocument":{"uri":uri},"position":position}),
                    )
                    .recv(),
            )
            .unwrap();
            serde_json::from_value::<Option<Vec<lsp_types::DocumentHighlight>>>(result).unwrap();
        }
        if session.supports("callHierarchyProvider") {
            let result = block_on(
                session
                    .request(
                        "textDocument/prepareCallHierarchy",
                        json!({"textDocument":{"uri":uri},"position":position}),
                    )
                    .recv(),
            )
            .unwrap();
            let items: Option<Vec<lsp_types::CallHierarchyItem>> =
                serde_json::from_value(result).unwrap();
            for item in items.unwrap_or_default().into_iter().take(1) {
                for method in ["callHierarchy/incomingCalls", "callHierarchy/outgoingCalls"] {
                    let result =
                        block_on(session.request(method, json!({"item":item})).recv()).unwrap();
                    assert!(result.is_array() || result.is_null());
                }
            }
        }
        if session.supports("inlayHintProvider") {
            let end = session.encoding().position(text, text.len()).unwrap();
            let result = block_on(session.request("textDocument/inlayHint", json!({"textDocument":{"uri":uri},"range":{"start":{"line":0,"character":0},"end":end}})).recv()).unwrap();
            serde_json::from_value::<Option<Vec<lsp_types::InlayHint>>>(result).unwrap();
        }
        if session.supports("semanticTokensProvider") {
            let result = block_on(
                session
                    .request(
                        "textDocument/semanticTokens/full",
                        json!({"textDocument":{"uri":uri}}),
                    )
                    .recv(),
            )
            .unwrap();
            serde_json::from_value::<Option<lsp_types::SemanticTokens>>(result).unwrap();
        }
        if session.supports("codeLensProvider") {
            let result = block_on(
                session
                    .request("textDocument/codeLens", json!({"textDocument":{"uri":uri}}))
                    .recv(),
            )
            .unwrap();
            let lenses: Option<Vec<lsp_types::CodeLens>> = serde_json::from_value(result).unwrap();
            for mut lens in lenses.unwrap_or_default().into_iter().take(3) {
                if lens.command.is_none() && caps["codeLensProvider"]["resolveProvider"] == true {
                    lens = serde_json::from_value(
                        block_on(
                            session
                                .request("codeLens/resolve", serde_json::to_value(lens).unwrap())
                                .recv(),
                        )
                        .unwrap(),
                    )
                    .unwrap();
                }
                if let Some(command) = lens.command {
                    eprintln!("{name}: lens {} ({})", command.title, command.command);
                    if command.command == "ccls.xref" {
                        let result = block_on(session.request("workspace/executeCommand", json!({"command":command.command,"arguments":command.arguments})).recv()).unwrap();
                        eprintln!("{name}: ccls.xref result {result}");
                    }
                }
            }
        }
        eprintln!("{name}: initialization and all advertised Fileview requests succeeded");
        session
            .notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}))
            .unwrap();
        stop(&session);
    }
}
