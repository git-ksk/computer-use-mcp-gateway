#![cfg(all(test, unix))]

use crate::{
    v2_operator_handoff::{
        ManagedHandoffRuntimeConfig, ManagedOperatorHandoffAuthority, TerminalPtyHandoffBinding,
        TerminalPtyInterventionRef, TerminalPtyTransportEvent,
    },
    v2_terminal_pty::{TerminalPtyProcessState, TerminalPtySpawnConfig},
    v2_terminal_pty_handoff::TerminalPtyDogfoodCoordinator,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path as AxumPath, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use std::{
    env,
    ffi::OsString,
    fs,
    io::Write as _,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq as _;
use tokio::{
    sync::{Mutex, Notify},
    time::{sleep, timeout},
};

const MAX_HUMAN_LINE_BYTES: usize = 2 * 1024;
const MAX_HUMAN_READ_BYTES: usize = 16 * 1024;
const HUMAN_WAIT: Duration = Duration::from_secs(5 * 60);

type AcceptanceCoordinator = TerminalPtyDogfoodCoordinator<ManagedOperatorHandoffAuthority>;

struct LoopbackAcceptanceState {
    coordinator: Arc<AcceptanceCoordinator>,
    fenced: TerminalPtyInterventionRef,
    human: Mutex<Option<TerminalPtyInterventionRef>>,
    verifying: Mutex<Option<TerminalPtyInterventionRef>>,
    client_binding: Mutex<Option<String>>,
    token: String,
    claimed: Notify,
    done: Notify,
}

#[derive(Serialize)]
struct OkBody {
    ok: bool,
}

#[derive(Serialize)]
struct OutputBody {
    ok: bool,
    data_base64: String,
    truncated_before_cursor: bool,
}

#[derive(Deserialize)]
struct InputBody {
    line: String,
}

fn random_hex(bytes: usize) -> String {
    let mut data = vec![0_u8; bytes];
    OsRng.fill_bytes(&mut data);
    let mut output = String::with_capacity(bytes * 2);
    for byte in data {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn same_secret(left: &str, right: &str) -> bool {
    left.as_bytes().ct_eq(right.as_bytes()).into()
}

fn valid_client_binding(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn request_client(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-terminal-client")
        .and_then(|value| value.to_str().ok())
        .filter(|value| valid_client_binding(value))
}

async fn require_client(
    state: &LoopbackAcceptanceState,
    headers: &HeaderMap,
    allow_create: bool,
) -> Result<(), StatusCode> {
    let requested = request_client(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let mut current = state.client_binding.lock().await;
    match current.as_deref() {
        Some(existing) if same_secret(existing, requested) => Ok(()),
        Some(_) => Err(StatusCode::CONFLICT),
        None if allow_create => {
            *current = Some(requested.to_owned());
            Ok(())
        }
        None => Err(StatusCode::UNAUTHORIZED),
    }
}

fn valid_token(state: &LoopbackAcceptanceState, token: &str) -> bool {
    same_secret(&state.token, token)
}

fn private_response(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    response
}

async fn page(
    State(state): State<Arc<LoopbackAcceptanceState>>,
    AxumPath(token): AxumPath<String>,
) -> Response {
    if !valid_token(&state, &token) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let nonce = random_hex(16);
    let csp = format!(
        "default-src 'none'; script-src 'nonce-{nonce}'; style-src 'nonce-{nonce}'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'"
    );
    let html = format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Terminal Human Acceptance</title>
<style nonce="{nonce}">:root{{color-scheme:dark;font-family:system-ui,-apple-system,sans-serif}}body{{margin:0;background:#0d0f12;color:#f5f5f5}}main{{max-width:900px;margin:auto;padding:16px}}pre{{min-height:280px;max-height:62vh;overflow:auto;white-space:pre-wrap;word-break:break-word;background:#050607;border:1px solid #333;border-radius:10px;padding:12px;font:14px ui-monospace,SFMono-Regular,Menlo,monospace}}.row{{display:flex;gap:8px}}input{{flex:1;min-width:0}}input,button{{font:inherit;min-height:44px;border-radius:8px;border:1px solid #555;padding:8px;background:#171a1f;color:#fff}}button{{cursor:pointer}}#done{{margin-top:10px;width:100%}}small{{display:block;margin:10px 0;opacity:.75;line-height:1.4}}</style>
</head><body><main><h2>Terminal Human Acceptance</h2><div id="status">Claiming exclusive Human authority…</div>
<pre id="terminal" aria-live="polite"></pre>
<div class="row"><input id="line" autocomplete="off" autocapitalize="none" autocorrect="off" spellcheck="false" maxlength="1024" placeholder="Type harmless test text"><button id="send">Send line</button></div>
<button id="done">Done</button>
<small>Local loopback acceptance only. Do not enter passwords, tokens, 2FA codes, or other secrets. Output is rendered as plain text; terminal escape sequences are not executed.</small>
<script nonce="{nonce}">(()=>{{const base=location.pathname;const status=document.querySelector('#status');const term=document.querySelector('#terminal');const field=document.querySelector('#line');let stopped=false;const d=new TextDecoder();function client(){{const b=new Uint8Array(16);crypto.getRandomValues(b);return Array.from(b,x=>x.toString(16).padStart(2,'0')).join('')}}const c=client();async function api(name,opt){{const o=opt||{{}};const h=Object.assign({{}},o.headers||{{}},{{'x-terminal-client':c}});const r=await fetch(base+'/api/'+name,Object.assign({{cache:'no-store'}},o,{{headers:h}}));if(!r.ok)throw new Error('rejected');return r}}function decode64(s){{const raw=atob(s);const b=new Uint8Array(raw.length);for(let i=0;i<raw.length;i++)b[i]=raw.charCodeAt(i);return d.decode(b)}}async function poll(){{if(stopped)return;try{{const j=await (await api('output')).json();if(j.data_base64){{term.textContent+=decode64(j.data_base64);if(term.textContent.length>65536)term.textContent=term.textContent.slice(-65536);term.scrollTop=term.scrollHeight}}setTimeout(poll,220)}}catch{{status.textContent='Session unavailable';stopped=true}}}}async function send(){{const line=field.value;if(!line)return;field.value='';try{{await api('input',{{method:'POST',headers:{{'content-type':'application/json'}},body:JSON.stringify({{line}})}})}}catch{{status.textContent='Input rejected';stopped=true}}}}document.querySelector('#send').onclick=()=>void send();field.addEventListener('keydown',e=>{{if(e.key==='Enter'){{e.preventDefault();void send()}}}});document.querySelector('#done').onclick=async()=>{{try{{await api('done',{{method:'POST'}});status.textContent='Done. Return to ChatGPT for Agent verification/resume.'}}catch{{status.textContent='Done rejected or already closed'}}finally{{stopped=true}}}};api('claim',{{method:'POST'}}).then(()=>{{status.textContent='Human authority active';void poll()}}).catch(()=>{{status.textContent='Claim rejected or session already active elsewhere';stopped=true}})}})();</script>
</main></body></html>"#
    );
    let mut response = private_response(Html(html).into_response());
    response.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_str(&csp).expect("bounded CSP"),
    );
    response
}

async fn claim(
    State(state): State<Arc<LoopbackAcceptanceState>>,
    AxumPath(token): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if !valid_token(&state, &token) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Err(status) = require_client(&state, &headers, true).await {
        return status.into_response();
    }
    let mut human = state.human.lock().await;
    if human.is_none() {
        match state.coordinator.claim_human(&state.fenced).await {
            Ok(intervention) => {
                if state
                    .coordinator
                    .human_resize(&intervention, 30, 100)
                    .await
                    .is_err()
                {
                    return StatusCode::CONFLICT.into_response();
                }
                *human = Some(intervention);
                state.claimed.notify_one();
            }
            Err(_) => return StatusCode::CONFLICT.into_response(),
        }
    }
    private_response(Json(OkBody { ok: true }).into_response())
}

async fn output(
    State(state): State<Arc<LoopbackAcceptanceState>>,
    AxumPath(token): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if !valid_token(&state, &token) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Err(status) = require_client(&state, &headers, false).await {
        return status.into_response();
    }
    let intervention = state.human.lock().await.clone();
    let Some(intervention) = intervention else {
        return StatusCode::CONFLICT.into_response();
    };
    match state
        .coordinator
        .human_read(&intervention, MAX_HUMAN_READ_BYTES)
        .await
    {
        Ok(bytes) => private_response(
            Json(OutputBody {
                ok: true,
                data_base64: STANDARD.encode(bytes.as_bytes()),
                truncated_before_cursor: bytes.truncated_before_cursor,
            })
            .into_response(),
        ),
        Err(_) => StatusCode::CONFLICT.into_response(),
    }
}

async fn input(
    State(state): State<Arc<LoopbackAcceptanceState>>,
    AxumPath(token): AxumPath<String>,
    headers: HeaderMap,
    Json(body): Json<InputBody>,
) -> Response {
    if !valid_token(&state, &token) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Err(status) = require_client(&state, &headers, false).await {
        return status.into_response();
    }
    if body.line.is_empty()
        || body.line.len() > MAX_HUMAN_LINE_BYTES
        || body.line.contains(['\r', '\n', '\0'])
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let intervention = state.human.lock().await.clone();
    let Some(intervention) = intervention else {
        return StatusCode::CONFLICT.into_response();
    };
    let mut bytes = body.line.into_bytes();
    bytes.push(b'\n');
    match state.coordinator.human_write(&intervention, &bytes).await {
        Ok(()) => private_response(Json(OkBody { ok: true }).into_response()),
        Err(_) => StatusCode::CONFLICT.into_response(),
    }
}

async fn done(
    State(state): State<Arc<LoopbackAcceptanceState>>,
    AxumPath(token): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if !valid_token(&state, &token) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Err(status) = require_client(&state, &headers, false).await {
        return status.into_response();
    }
    if state.verifying.lock().await.is_some() {
        return private_response(Json(OkBody { ok: true }).into_response());
    }
    let intervention = state.human.lock().await.take();
    let Some(intervention) = intervention else {
        return StatusCode::CONFLICT.into_response();
    };
    match state.coordinator.human_done(&intervention).await {
        Ok(verifying) => {
            *state.verifying.lock().await = Some(verifying);
            state.done.notify_one();
            private_response(Json(OkBody { ok: true }).into_response())
        }
        Err(_) => StatusCode::CONFLICT.into_response(),
    }
}

async fn wait_for_file(path: &Path, bound: Duration) -> bool {
    let deadline = Instant::now() + bound;
    while Instant::now() < deadline {
        if path.is_file() {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "physical local Human PTY acceptance; run explicitly with reviewed Handoff root and Node"]
async fn physical_loopback_terminal_human_acceptance() {
    let handoff_root =
        PathBuf::from(env::var_os("CUMG_V2_HANDOFF_ROOT").expect("CUMG_V2_HANDOFF_ROOT"));
    let node = PathBuf::from(env::var_os("CUMG_V2_NODE").expect("CUMG_V2_NODE"));
    assert!(handoff_root.join("dist/index.js").is_file());
    assert!(
        handoff_root
            .join("dist/terminal-takeover/index.js")
            .is_file()
    );
    assert!(node.is_absolute());

    let temp = env::temp_dir().join(format!(
        "cumg-v2-terminal-human-acceptance-{}-{}",
        std::process::id(),
        random_hex(8),
    ));
    fs::create_dir(&temp).unwrap();
    fs::set_permissions(&temp, fs::Permissions::from_mode(0o700)).unwrap();
    let key = temp.join("checkpoint.key");
    fs::write(&key, [0x61_u8; 32]).unwrap();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    let checkpoint = temp.join("checkpoint.json");
    let env_file = temp.join("managed-runtime.env");
    fs::write(
        &env_file,
        format!(
            "CUMG_V2_HANDOFF_ROOT={}\nCUMG_V2_HANDOFF_CHECKPOINT_FILE={}\nCUMG_V2_HANDOFF_CHECKPOINT_KEY_FILE={}\n",
            handoff_root.display(),
            checkpoint.display(),
            key.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&env_file, fs::Permissions::from_mode(0o600)).unwrap();

    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/v2_handoff_runtime.mjs");
    let runtime_config =
        ManagedHandoffRuntimeConfig::new(node, script, env_file, Duration::from_secs(5)).unwrap();
    let runtime = Arc::new(
        ManagedOperatorHandoffAuthority::spawn(runtime_config)
            .await
            .unwrap(),
    );
    let coordinator =
        Arc::new(TerminalPtyDogfoodCoordinator::new(runtime.clone(), "b".repeat(64)).unwrap());
    let cat = fs::canonicalize("/bin/cat").unwrap();
    coordinator
        .spawn(TerminalPtySpawnConfig {
            program: cat,
            args: Vec::new(),
            cwd: Path::new("/tmp").to_path_buf(),
            env: vec![(OsString::from("TERM"), OsString::from("xterm-256color"))],
            rows: 24,
            cols: 80,
        })
        .await
        .unwrap();
    coordinator.agent_write(b"agent-ready\n").await.unwrap();
    sleep(Duration::from_millis(80)).await;
    let before = coordinator.agent_read(4096).await.unwrap();
    assert!(
        before
            .as_bytes()
            .windows(b"agent-ready".len())
            .any(|window| window == b"agent-ready")
    );

    let fenced = coordinator.begin_human_fence().await.unwrap();
    assert!(coordinator.agent_write(b"must-not-run\n").await.is_err());
    assert!(coordinator.agent_read(4096).await.is_err());
    assert!(coordinator.agent_resize(25, 81).await.is_err());

    let state = Arc::new(LoopbackAcceptanceState {
        coordinator: coordinator.clone(),
        fenced,
        human: Mutex::new(None),
        verifying: Mutex::new(None),
        client_binding: Mutex::new(None),
        token: random_hex(32),
        claimed: Notify::new(),
        done: Notify::new(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/terminal/{token}", get(page))
        .route("/terminal/{token}/api/claim", post(claim))
        .route("/terminal/{token}/api/output", get(output))
        .route("/terminal/{token}/api/input", post(input))
        .route("/terminal/{token}/api/done", post(done))
        .layer(DefaultBodyLimit::max(4 * 1024))
        .with_state(state.clone());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let resume_file = temp.join("explicit-resume.signal");
    let ready_file = temp.join("ready-to-resume.signal");
    let url = format!(
        "http://127.0.0.1:{}/terminal/{}",
        address.port(),
        state.token
    );
    println!("HUMAN_ACCEPTANCE_URL={url}");
    println!("HUMAN_ACCEPTANCE_READY_FILE={}", ready_file.display());
    println!("HUMAN_ACCEPTANCE_RESUME_FILE={}", resume_file.display());
    let _ = std::io::stdout().flush();

    let outcome = async {
        timeout(HUMAN_WAIT, state.claimed.notified())
            .await
            .map_err(|_| "Human claim timed out")?;
        coordinator
            .agent_write(b"human-active-must-not-run\n")
            .await
            .map_err(|_| ())
            .err()
            .ok_or("Agent write unexpectedly allowed while Human active")?;
        if coordinator.agent_read(4096).await.is_ok()
            || coordinator.agent_resize(26, 82).await.is_ok()
        {
            return Err("Agent observation/resize unexpectedly allowed while Human active");
        }

        timeout(HUMAN_WAIT, state.done.notified())
            .await
            .map_err(|_| "Human Done timed out")?;
        if coordinator
            .agent_write(b"verifying-must-not-run\n")
            .await
            .is_ok()
            || coordinator.agent_read(4096).await.is_ok()
            || coordinator.agent_resize(27, 83).await.is_ok()
        {
            return Err("Agent authority unexpectedly restored before verification");
        }
        if coordinator
            .process_state()
            .await
            .map_err(|_| "PTY state unavailable")?
            != TerminalPtyProcessState::Running
        {
            return Err("PTY did not remain running after Human Done");
        }
        let verifying = state
            .verifying
            .lock()
            .await
            .clone()
            .ok_or("verifying intervention missing")?;
        let ready = coordinator
            .report_verification(&verifying, true)
            .await
            .map_err(|_| "content-free verification failed")?;
        if coordinator
            .agent_write(b"ready-must-not-run\n")
            .await
            .is_ok()
            || coordinator.agent_read(4096).await.is_ok()
        {
            return Err("Agent authority unexpectedly restored before explicit resume");
        }
        fs::write(&ready_file, b"ready").map_err(|_| "ready signal write failed")?;
        if !wait_for_file(&resume_file, HUMAN_WAIT).await {
            return Err("explicit Agent resume signal timed out");
        }
        let receipt = coordinator
            .resume(&ready)
            .await
            .map_err(|_| "explicit resume failed")?;
        if !receipt.session_alive || !receipt.agent_state_sync_required {
            return Err("resume receipt did not require live-session state synchronization");
        }
        if coordinator
            .agent_write(b"sync-must-not-run\n")
            .await
            .is_ok()
            || coordinator.agent_read(4096).await.is_ok()
            || coordinator.agent_resize(28, 84).await.is_ok()
        {
            return Err("Agent PTY operation allowed before state resynchronization");
        }
        coordinator
            .acknowledge_state_invalidated()
            .await
            .map_err(|_| "state resynchronization acknowledgement failed")?;
        coordinator
            .agent_write(b"agent-resumed\n")
            .await
            .map_err(|_| "Agent write failed after explicit resume")?;
        sleep(Duration::from_millis(80)).await;
        let after = coordinator
            .agent_read(4096)
            .await
            .map_err(|_| "Agent read failed after explicit resume")?;
        if !after
            .as_bytes()
            .windows(b"agent-resumed".len())
            .any(|window| window == b"agent-resumed")
        {
            return Err("post-resume Agent marker not observed");
        }
        Ok::<(), &'static str>(())
    }
    .await;

    server.abort();
    let _ = server.await;
    let _ = coordinator.close_session().await;
    runtime.shutdown().await;
    assert!(
        !checkpoint.exists(),
        "Terminal Human acceptance must not create a generic Handoff checkpoint"
    );
    let _ = fs::remove_dir_all(&temp);
    if let Err(reason) = outcome {
        panic!("physical Terminal Human acceptance failed: {reason}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "physical iPhone WebRTC Human PTY acceptance; run explicitly with reviewed Handoff root, Node, public origin and loopback bind"]
async fn physical_webrtc_terminal_human_acceptance() {
    let handoff_root =
        PathBuf::from(env::var_os("CUMG_V2_HANDOFF_ROOT").expect("CUMG_V2_HANDOFF_ROOT"));
    let node = PathBuf::from(env::var_os("CUMG_V2_NODE").expect("CUMG_V2_NODE"));
    let web_rtc_bind =
        env::var("CUMG_V2_HANDOFF_WEBRTC_HTTP_BIND").expect("CUMG_V2_HANDOFF_WEBRTC_HTTP_BIND");
    let public_origin = env::var("CUMG_V2_HANDOFF_WEBRTC_PUBLIC_ORIGIN")
        .expect("CUMG_V2_HANDOFF_WEBRTC_PUBLIC_ORIGIN");
    assert!(handoff_root.join("dist/index.js").is_file());
    assert!(
        handoff_root
            .join("dist/terminal-takeover/index.js")
            .is_file()
    );
    assert!(node.is_absolute());
    assert!(web_rtc_bind.starts_with("127.0.0.1:"));
    assert!(public_origin.starts_with("https://"));
    assert!(!web_rtc_bind.contains(['\r', '\n']));
    assert!(!public_origin.contains(['\r', '\n']));

    let temp = env::temp_dir().join(format!(
        "cumg-v2-terminal-webrtc-human-acceptance-{}-{}",
        std::process::id(),
        random_hex(8),
    ));
    fs::create_dir(&temp).unwrap();
    fs::set_permissions(&temp, fs::Permissions::from_mode(0o700)).unwrap();
    let key = temp.join("checkpoint.key");
    fs::write(&key, [0x62_u8; 32]).unwrap();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    let checkpoint = temp.join("checkpoint.json");
    let env_file = temp.join("managed-runtime.env");
    let mut managed_env = format!(
        "CUMG_V2_HANDOFF_ROOT={}\nCUMG_V2_HANDOFF_CHECKPOINT_FILE={}\nCUMG_V2_HANDOFF_CHECKPOINT_KEY_FILE={}\nCUMG_V2_HANDOFF_WEBRTC_HTTP_BIND={}\nCUMG_V2_HANDOFF_WEBRTC_PUBLIC_ORIGIN={}\nCUMG_V2_HANDOFF_TERMINAL_WEBRTC_ONLY=1\n",
        handoff_root.display(),
        checkpoint.display(),
        key.display(),
        web_rtc_bind,
        public_origin,
    );
    for name in [
        "MCP_HANDOFF_CLOUDFLARE_TURN_KEY_ID",
        "MCP_HANDOFF_CLOUDFLARE_TURN_KEY_API_TOKEN",
        "MCP_HANDOFF_COTURN_SHARED_SECRET",
        "MCP_HANDOFF_COTURN_TURN_URLS",
        "MCP_HANDOFF_COTURN_STUN_URLS",
    ] {
        if let Ok(value) = env::var(name) {
            assert!(!value.contains(['\r', '\n']));
            managed_env.push_str(name);
            managed_env.push('=');
            managed_env.push_str(&value);
            managed_env.push('\n');
        }
    }
    fs::write(&env_file, managed_env).unwrap();
    fs::set_permissions(&env_file, fs::Permissions::from_mode(0o600)).unwrap();

    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/v2_handoff_runtime.mjs");
    let runtime_config =
        ManagedHandoffRuntimeConfig::new(node, script, env_file, Duration::from_secs(5)).unwrap();
    let runtime = Arc::new(
        ManagedOperatorHandoffAuthority::spawn(runtime_config)
            .await
            .unwrap(),
    );
    let principal_binding = "c".repeat(64);
    let coordinator = Arc::new(
        TerminalPtyDogfoodCoordinator::new(runtime.clone(), principal_binding.clone()).unwrap(),
    );
    let cat = fs::canonicalize("/bin/cat").unwrap();
    let pty_binding = coordinator
        .spawn(TerminalPtySpawnConfig {
            program: cat,
            args: Vec::new(),
            cwd: Path::new("/tmp").to_path_buf(),
            env: vec![(OsString::from("TERM"), OsString::from("xterm-256color"))],
            rows: 24,
            cols: 80,
        })
        .await
        .unwrap();
    coordinator.agent_write(b"agent-ready\n").await.unwrap();
    sleep(Duration::from_millis(80)).await;
    let before = coordinator.agent_read(4096).await.unwrap();
    assert!(
        before
            .as_bytes()
            .windows(b"agent-ready".len())
            .any(|window| window == b"agent-ready")
    );

    let handoff_binding = TerminalPtyHandoffBinding::new(
        pty_binding.session_id().to_owned(),
        pty_binding.generation(),
        principal_binding,
    )
    .unwrap();
    let fenced = coordinator.begin_human_fence().await.unwrap();
    assert!(coordinator.agent_write(b"must-not-run\n").await.is_err());
    assert!(coordinator.agent_read(4096).await.is_err());
    assert!(coordinator.agent_resize(25, 81).await.is_err());

    let locator = runtime
        .terminal_transport_start(handoff_binding.clone(), &fenced)
        .await
        .unwrap();
    let resume_file = temp.join("explicit-resume.signal");
    let ready_file = temp.join("ready-to-resume.signal");
    println!("HUMAN_ACCEPTANCE_URL={locator}");
    println!("HUMAN_ACCEPTANCE_READY_FILE={}", ready_file.display());
    println!("HUMAN_ACCEPTANCE_RESUME_FILE={}", resume_file.display());
    let _ = std::io::stdout().flush();

    let outcome = async {
        let transport_deadline = Instant::now() + HUMAN_WAIT;
        loop {
            let status = runtime
                .terminal_transport_status(handoff_binding.clone())
                .await
                .map_err(|_| "Terminal WebRTC status unavailable")?;
            if status.faulted {
                return Err("Terminal WebRTC transport faulted before Human claim");
            }
            if status.transport_ready {
                break;
            }
            if Instant::now() >= transport_deadline {
                return Err("Terminal WebRTC Human connection timed out");
            }
            sleep(Duration::from_millis(100)).await;
        }

        let human = coordinator
            .claim_human(&fenced)
            .await
            .map_err(|_| "Human authority claim failed after WebRTC readiness")?;
        if runtime
            .terminal_transport_activate(handoff_binding.clone(), &human)
            .await
            .is_err()
        {
            let _ = coordinator.human_disconnect(&human).await;
            return Err("Terminal WebRTC activation failed after Human authority claim");
        }
        if coordinator
            .agent_write(b"human-active-must-not-run\n")
            .await
            .is_ok()
            || coordinator.agent_read(4096).await.is_ok()
            || coordinator.agent_resize(26, 82).await.is_ok()
        {
            return Err("Agent PTY operation unexpectedly allowed while Human active");
        }

        let mut human_input_seen = false;
        let mut human_output_seen = false;
        let verifying = loop {
            let status = runtime
                .terminal_transport_status(handoff_binding.clone())
                .await
                .map_err(|_| "Terminal WebRTC status unavailable while Human active")?;
            if status.faulted {
                return Err("Terminal WebRTC transport faulted while Human active");
            }
            if status.disconnected && !status.completed {
                coordinator
                    .human_disconnect(&human)
                    .await
                    .map_err(|_| "Human disconnect could not be fenced")?;
                return Err("Human disconnected before Done; Agent remained fenced");
            }

            match runtime
                .terminal_transport_next_event(handoff_binding.clone(), &human)
                .await
                .map_err(|_| "Terminal WebRTC event unavailable")?
            {
                Some(TerminalPtyTransportEvent::Input(bytes)) => {
                    coordinator
                        .human_write(&human, &bytes)
                        .await
                        .map_err(|_| "Human PTY input was rejected")?;
                    human_input_seen = true;
                    sleep(Duration::from_millis(30)).await;
                }
                Some(TerminalPtyTransportEvent::Resize { rows, cols }) => {
                    coordinator
                        .human_resize(&human, rows, cols)
                        .await
                        .map_err(|_| "Human PTY resize was rejected")?;
                }
                Some(TerminalPtyTransportEvent::Done) => {
                    break coordinator
                        .human_done(&human)
                        .await
                        .map_err(|_| "Human Done transition failed")?;
                }
                None => {}
            }

            if human_input_seen {
                let output = coordinator
                    .human_read(&human, 2 * 1024)
                    .await
                    .map_err(|_| "Human PTY output unavailable")?;
                if !output.as_bytes().is_empty() {
                    runtime
                        .terminal_transport_output(
                            handoff_binding.clone(),
                            &human,
                            output.as_bytes(),
                        )
                        .await
                        .map_err(|_| "Human PTY output transport failed")?;
                    human_output_seen = true;
                }
            }
            sleep(Duration::from_millis(25)).await;
        };
        let _ = runtime
            .terminal_transport_revoke(handoff_binding.clone(), &human)
            .await;
        if !human_input_seen {
            return Err("Human Acceptance completed without any PTY input");
        }
        if !human_output_seen {
            return Err("Human Acceptance did not observe PTY output after Human input");
        }

        if coordinator
            .agent_write(b"verifying-must-not-run\n")
            .await
            .is_ok()
            || coordinator.agent_read(4096).await.is_ok()
            || coordinator.agent_resize(27, 83).await.is_ok()
        {
            return Err("Agent authority unexpectedly restored before verification");
        }
        if coordinator
            .process_state()
            .await
            .map_err(|_| "PTY state unavailable")?
            != TerminalPtyProcessState::Running
        {
            return Err("PTY did not remain running after Human Done");
        }
        let ready = coordinator
            .report_verification(&verifying, true)
            .await
            .map_err(|_| "content-free verification failed")?;
        if coordinator
            .agent_write(b"ready-must-not-run\n")
            .await
            .is_ok()
            || coordinator.agent_read(4096).await.is_ok()
        {
            return Err("Agent authority unexpectedly restored before explicit resume");
        }
        fs::write(&ready_file, b"ready").map_err(|_| "ready signal write failed")?;
        if !wait_for_file(&resume_file, HUMAN_WAIT).await {
            return Err("explicit Agent resume signal timed out");
        }
        let receipt = coordinator
            .resume(&ready)
            .await
            .map_err(|_| "explicit resume failed")?;
        if !receipt.session_alive || !receipt.agent_state_sync_required {
            return Err("resume receipt did not require live-session state synchronization");
        }
        if coordinator
            .agent_write(b"sync-must-not-run\n")
            .await
            .is_ok()
            || coordinator.agent_read(4096).await.is_ok()
            || coordinator.agent_resize(28, 84).await.is_ok()
        {
            return Err("Agent PTY operation allowed before state resynchronization");
        }
        coordinator
            .acknowledge_state_invalidated()
            .await
            .map_err(|_| "state resynchronization acknowledgement failed")?;
        coordinator
            .agent_write(b"agent-resumed\n")
            .await
            .map_err(|_| "Agent write failed after explicit resume")?;
        sleep(Duration::from_millis(80)).await;
        let after = coordinator
            .agent_read(4096)
            .await
            .map_err(|_| "Agent read failed after explicit resume")?;
        if !after
            .as_bytes()
            .windows(b"agent-resumed".len())
            .any(|window| window == b"agent-resumed")
        {
            return Err("post-resume Agent marker not observed");
        }
        Ok::<(), &'static str>(())
    }
    .await;

    let _ = runtime
        .terminal_transport_revoke(handoff_binding.clone(), &fenced)
        .await;
    let _ = coordinator.close_session().await;
    runtime.shutdown().await;
    assert!(
        !checkpoint.exists(),
        "Terminal WebRTC Human acceptance must not create a generic Handoff checkpoint"
    );
    let _ = fs::remove_dir_all(&temp);
    if let Err(reason) = outcome {
        panic!("physical Terminal WebRTC Human acceptance failed: {reason}");
    }
}
