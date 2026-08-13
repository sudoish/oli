use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

struct TestServer {
    addr: SocketAddr,
    responses: Arc<Mutex<VecDeque<ServerResponse>>>,
    requests: Arc<Mutex<Vec<Value>>>,
}

struct ServerResponse {
    status: u16,
    content_type: &'static str,
    body: String,
}

impl TestServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let responses = Arc::new(Mutex::new(VecDeque::<ServerResponse>::new()));
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let thread_responses = responses.clone();
        let thread_requests = requests.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                handle_connection(stream, &thread_responses, &thread_requests);
            }
        });
        Self {
            addr,
            responses,
            requests,
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    fn push_assistant(&self, content: &str) {
        self.push_sse(assistant_sse(content));
    }

    fn push_tool_call(&self, name: &str, args: Value) {
        self.push_sse(tool_call_sse(name, args));
    }

    fn push_json(&self, status: u16, body: Value) {
        self.responses.lock().unwrap().push_back(ServerResponse {
            status,
            content_type: "application/json",
            body: body.to_string(),
        });
    }

    fn push_sse(&self, body: String) {
        self.responses.lock().unwrap().push_back(ServerResponse {
            status: 200,
            content_type: "text/event-stream",
            body,
        });
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

fn handle_connection(
    mut stream: TcpStream,
    responses: &Arc<Mutex<VecDeque<ServerResponse>>>,
    requests: &Arc<Mutex<Vec<Value>>>,
) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut tmp).unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]);
    let content_len = headers
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length:"))
        .or_else(|| {
            headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
        })
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let body_start = header_end + 4;
    while buf.len() < body_start + content_len {
        let n = stream.read(&mut tmp).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = &buf[body_start..body_start + content_len.min(buf.len().saturating_sub(body_start))];
    if let Ok(value) = serde_json::from_slice(body) {
        requests.lock().unwrap().push(value);
    }

    let response = responses
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or_else(|| ServerResponse {
            status: 500,
            content_type: "application/json",
            body: json!({"error": {"message": "test response queue exhausted"}}).to_string(),
        });
    let text = response.body;
    let reason = if response.status == 200 {
        "OK"
    } else {
        "Error"
    };
    let wire = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        response.status,
        reason,
        response.content_type,
        text.len(),
        text
    );
    let _ = stream.write_all(wire.as_bytes());
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn assistant_sse(content: &str) -> String {
    format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices": [{"delta": {"role": "assistant", "content": content}}]}),
        json!({"choices": [], "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}})
    )
}

fn tool_call_sse(name: &str, args: Value) -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "choices": [{
                "delta": {
                    "role": "assistant",
                    "tool_calls": [{
                        "index": 0,
                        "id": "call-1",
                        "type": "function",
                        "function": {"name": name, "arguments": args.to_string()}
                    }]
                }
            }]
        })
    )
}

struct Sandbox {
    _home: TempDir,
    cwd: TempDir,
    xdg: TempDir,
}

impl Sandbox {
    fn new(server: &TestServer) -> Self {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let xdg = tempfile::tempdir().unwrap();
        std::fs::create_dir(cwd.path().join(".oli")).unwrap();
        std::fs::write(
            cwd.path().join(".oli/config.toml"),
            format!(
                r#"default_provider = "test"
default_model = "test-model"

[providers.test]
kind = "openai-compat"
base_url = "{}"
api_key = "test-key"

[agent]
max_turns = 4
"#,
                server.base_url()
            ),
        )
        .unwrap();
        Self {
            _home: home,
            cwd,
            xdg,
        }
    }

    fn sessions_dir(&self) -> std::path::PathBuf {
        self.xdg.path().join("oli/sessions")
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_oli"));
        cmd.current_dir(self.cwd.path())
            .env("HOME", self._home.path())
            .env("XDG_CONFIG_HOME", self.xdg.path())
            .env_remove("OPENROUTER_API_KEY")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }
}

struct CmdOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

fn run_oli(mut cmd: Command) -> CmdOutput {
    let mut child = cmd.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let output = child.wait_with_output().unwrap();
            return CmdOutput {
                status,
                stdout: String::from_utf8(output.stdout).unwrap(),
                stderr: String::from_utf8(output.stderr).unwrap(),
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("oli did not exit within 5 seconds");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn conversation_from_stderr(stderr: &str) -> String {
    stderr
        .strip_prefix("conversation: ")
        .and_then(|s| s.trim().split_whitespace().next())
        .unwrap_or_else(|| panic!("missing conversation marker in stderr: {stderr:?}"))
        .to_string()
}

fn transcript(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn fresh_text_run_prints_only_answer_and_persists_conversation() {
    let server = TestServer::start();
    server.push_assistant("fresh answer");
    let sandbox = Sandbox::new(&server);

    let mut cmd = sandbox.command();
    cmd.args(["run", "--prompt", "hello"]);
    let out = run_oli(cmd);

    assert!(out.status.success(), "stderr: {}", out.stderr);
    assert_eq!(out.stdout, "fresh answer\n");
    let id = conversation_from_stderr(&out.stderr);
    let session = sandbox.sessions_dir().join(format!("{id}.jsonl"));
    let body = transcript(&session);
    assert!(body.contains(r#""content":"hello""#), "{body}");
    assert!(body.contains(r#""content":"fresh answer""#), "{body}");
}

#[test]
fn json_run_and_resume_keep_stdout_machine_clean_and_reuse_id() {
    let server = TestServer::start();
    server.push_assistant("first answer");
    server.push_assistant("second answer");
    let sandbox = Sandbox::new(&server);

    let mut first = sandbox.command();
    first.args(["run", "--prompt", "first", "--output", "json"]);
    let first = run_oli(first);
    assert!(first.status.success(), "stderr: {}", first.stderr);
    assert_eq!(first.stderr, "");
    let first_json: Value = serde_json::from_str(&first.stdout).unwrap();
    assert_eq!(first_json["response"], "first answer");
    assert_eq!(first_json["provider"], "test");
    assert_eq!(first_json["model"], "test-model");
    assert_eq!(first_json["usage"]["total_tokens"], 5);
    // A category this provider never reported stays null, and the run
    // says how many of its calls left it out.
    assert!(first_json["usage"]["cache_read_tokens"].is_null());
    assert_eq!(first_json["usage"]["calls"], 1);
    assert_eq!(
        first_json["usage"]["unreported_calls"]["cache_read_tokens"],
        1
    );
    let id = first_json["conversation_id"].as_str().unwrap().to_string();

    let mut second = sandbox.command();
    second.args([
        "run",
        "--conversation",
        &id,
        "--prompt",
        "second",
        "--output",
        "json",
    ]);
    let second = run_oli(second);
    assert!(second.status.success(), "stderr: {}", second.stderr);
    assert_eq!(second.stderr, "");
    let second_json: Value = serde_json::from_str(&second.stdout).unwrap();
    assert_eq!(second_json["conversation_id"], id);
    assert_eq!(second_json["response"], "second answer");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let resumed_messages = requests[1]["messages"].as_array().unwrap();
    assert!(resumed_messages.iter().any(|m| m["content"] == "first"));
    assert!(
        resumed_messages
            .iter()
            .any(|m| m["content"] == "first answer")
    );
    assert!(resumed_messages.iter().any(|m| m["content"] == "second"));
}

#[test]
fn strict_mode_denies_approval_requests_without_blocking_or_stdout_noise() {
    let server = TestServer::start();
    server.push_tool_call(
        "Bash",
        json!({"command": "touch strict-mode-should-not-exist"}),
    );
    server.push_assistant("saw denial");
    let sandbox = Sandbox::new(&server);

    let mut cmd = sandbox.command();
    cmd.args([
        "run",
        "--strict",
        "--prompt",
        "try a shell",
        "--output",
        "json",
    ]);
    let out = run_oli(cmd);

    assert!(out.status.success(), "stderr: {}", out.stderr);
    assert_eq!(out.stderr, "");
    let value: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(value["response"], "saw denial");
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let tool_result = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(tool_result.contains("user declined Bash"), "{tool_result}");
    assert!(
        !sandbox
            .cwd
            .path()
            .join("strict-mode-should-not-exist")
            .exists()
    );
}

#[test]
fn provider_failure_writes_only_stderr_and_exits_nonzero() {
    let server = TestServer::start();
    server.push_json(500, json!({"error": {"message": "boom from provider"}}));
    let sandbox = Sandbox::new(&server);

    let mut cmd = sandbox.command();
    cmd.args(["run", "--prompt", "fail", "--output", "json"]);
    let out = run_oli(cmd);

    assert!(!out.status.success());
    assert_eq!(out.stdout, "");
    assert!(out.stderr.contains("provider error"), "{}", out.stderr);
    assert!(out.stderr.contains("boom from provider"), "{}", out.stderr);
}

#[test]
fn invalid_conversation_exits_nonzero_without_calling_provider_or_creating_session() {
    let server = TestServer::start();
    let sandbox = Sandbox::new(&server);

    let mut cmd = sandbox.command();
    cmd.args([
        "run",
        "--conversation",
        "missing",
        "--prompt",
        "hello",
        "--output",
        "json",
    ]);
    let out = run_oli(cmd);

    assert!(!out.status.success());
    assert_eq!(out.stdout, "");
    assert!(
        out.stderr.contains("unknown conversation: missing"),
        "{}",
        out.stderr
    );
    assert!(server.requests().is_empty());
    assert!(!sandbox.sessions_dir().exists());
}

#[test]
fn max_turn_exhaustion_is_structured_non_success_and_remains_resumable() {
    let server = TestServer::start();
    server.push_tool_call("Read", json!({"file_path": "Cargo.toml"}));
    server.push_assistant("completed after resume");
    let sandbox = Sandbox::new(&server);

    let mut first = sandbox.command();
    first.args([
        "run",
        "--prompt",
        "inspect first",
        "--max-turns",
        "1",
        "--output",
        "json",
    ]);
    let first = run_oli(first);
    assert!(!first.status.success());
    assert_eq!(first.stderr, "");
    let incomplete: Value = serde_json::from_str(&first.stdout).unwrap();
    assert_eq!(incomplete["status"], "incomplete");
    assert_eq!(incomplete["reason"], "max_turns_exhausted");
    assert_eq!(incomplete["max_turns"], 1);
    assert!(incomplete.get("response").is_none());
    let id = incomplete["conversation_id"].as_str().unwrap().to_string();

    let mut resumed = sandbox.command();
    resumed.args([
        "run",
        "--conversation",
        &id,
        "--prompt",
        "finish now",
        "--output",
        "json",
    ]);
    let resumed = run_oli(resumed);
    assert!(resumed.status.success(), "stderr: {}", resumed.stderr);
    let completed: Value = serde_json::from_str(&resumed.stdout).unwrap();
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["conversation_id"], id);
    assert_eq!(completed["response"], "completed after resume");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let resumed_messages = requests[1]["messages"].as_array().unwrap();
    assert!(
        resumed_messages
            .iter()
            .any(|m| m["content"] == "inspect first")
    );
    assert!(
        resumed_messages
            .iter()
            .any(|m| m["content"] == "finish now")
    );
}

#[test]
fn text_max_turn_exhaustion_uses_stderr_and_nonzero_exit() {
    let server = TestServer::start();
    server.push_tool_call("Read", json!({"file_path": "Cargo.toml"}));
    let sandbox = Sandbox::new(&server);

    let mut cmd = sandbox.command();
    cmd.args(["run", "--prompt", "inspect", "--max-turns", "1"]);
    let out = run_oli(cmd);

    assert!(!out.status.success());
    assert_eq!(out.stdout, "");
    assert!(
        out.stderr.contains("max turns exhausted: 1"),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("conversation: "), "{}", out.stderr);
}
