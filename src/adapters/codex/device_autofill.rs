use std::env;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Error as WsError, Message, WebSocket};

use crate::core::ui as core_ui;

const DEVICE_AUTH_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const DEVICE_PROMPT_TIMEOUT: Duration = Duration::from_secs(30);
const CDP_READY_TIMEOUT: Duration = Duration::from_secs(30);
const CDP_POLL_INTERVAL: Duration = Duration::from_millis(250);
const CDP_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const CDP_EVENT_TIMEOUT: Duration = Duration::from_millis(250);
const AUTOFILL_TIMEOUT: Duration = Duration::from_secs(3 * 60);
const AUTOFILL_FALLBACK_INTERVAL: Duration = Duration::from_secs(5);
const AUTOFILL_BINDING_NAME: &str = "__scodexAutofillReport";

type ChromeSocket = WebSocket<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone)]
pub struct AutofillRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexDeviceAuthPrompt {
    pub url: String,
    pub code: Option<String>,
}

pub fn run_device_autofill_login(
    codex_bin: &Path,
    managed_home: &Path,
    request: &AutofillRequest,
) -> Result<()> {
    let ui = core_ui::messages();

    let chrome =
        resolve_chromium_binary().ok_or_else(|| anyhow!("{}", ui.login_autofill_no_chrome()))?;

    let child = spawn_codex_login(codex_bin, managed_home)?;

    let prompt = match read_codex_login_prompt(&child) {
        Ok(prompt) => prompt,
        Err(error) => {
            let _ = child.kill_and_collect();
            return Err(error);
        }
    };

    eprintln!(
        "{}",
        ui.login_autofill_prompt(&prompt.url, prompt.code.as_deref())
    );

    let browser = match run_browser_autofill(&chrome, &prompt, request) {
        Ok(browser) => browser,
        Err(error) => {
            let _ = child.kill_and_collect();
            return Err(error);
        }
    };

    eprintln!("{}", ui.login_autofill_waiting_consent());

    let wait_result = wait_for_codex_login(child, DEVICE_AUTH_TIMEOUT);
    browser.close();
    wait_result
}

fn spawn_codex_login(codex: &Path, managed_home: &Path) -> Result<CapturedCodexLogin> {
    let child = Command::new(codex)
        .arg("login")
        .env("CODEX_HOME", managed_home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to launch {}", codex.display()))?;
    capture_login_process(child)
}

struct CapturedCodexLogin {
    child: Child,
    stdout: StreamCapture,
    stderr: StreamCapture,
    prompt_rx: mpsc::Receiver<CodexDeviceAuthPrompt>,
}

struct StreamCapture {
    output: Arc<Mutex<String>>,
    join: Option<JoinHandle<()>>,
}

struct CollectedOutput {
    stdout: String,
    stderr: String,
}

impl StreamCapture {
    fn snapshot(&self) -> String {
        self.output.lock().expect("stream capture lock").clone()
    }

    fn finish(mut self) -> String {
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        self.output.lock().expect("stream capture lock").clone()
    }
}

impl CapturedCodexLogin {
    fn combined_output_snapshot(&self) -> String {
        format!("{}\n{}", self.stderr.snapshot(), self.stdout.snapshot())
    }

    fn collect_output(self) -> CollectedOutput {
        CollectedOutput {
            stdout: self.stdout.finish(),
            stderr: self.stderr.finish(),
        }
    }

    fn kill_and_collect(mut self) -> CollectedOutput {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.collect_output()
    }
}

fn capture_login_process(mut child: Child) -> Result<CapturedCodexLogin> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to capture codex login stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to capture codex login stderr"))?;
    let (prompt_tx, prompt_rx) = mpsc::channel();

    Ok(CapturedCodexLogin {
        child,
        stdout: spawn_stream_capture(stdout, prompt_tx.clone()),
        stderr: spawn_stream_capture(stderr, prompt_tx),
        prompt_rx,
    })
}

fn spawn_stream_capture<R: Read + Send + 'static>(
    stream: R,
    prompt_tx: mpsc::Sender<CodexDeviceAuthPrompt>,
) -> StreamCapture {
    let output = Arc::new(Mutex::new(String::new()));
    let output_clone = Arc::clone(&output);

    let join = thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut prompt_sent = false;

        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let prompt = {
                        let mut captured = output_clone.lock().expect("stream capture lock");
                        captured.push_str(&line);
                        if prompt_sent {
                            None
                        } else {
                            parse_codex_login_prompt(&captured).ok()
                        }
                    };
                    if let Some(prompt) = prompt {
                        let _ = prompt_tx.send(prompt);
                        prompt_sent = true;
                    }
                }
                Err(_) => break,
            }
        }
    });

    StreamCapture {
        output,
        join: Some(join),
    }
}

fn read_codex_login_prompt(child: &CapturedCodexLogin) -> Result<CodexDeviceAuthPrompt> {
    let started = Instant::now();

    while started.elapsed() < DEVICE_PROMPT_TIMEOUT {
        let remaining = DEVICE_PROMPT_TIMEOUT.saturating_sub(started.elapsed());
        let wait_for = remaining.min(Duration::from_millis(250));
        match child.prompt_rx.recv_timeout(wait_for) {
            Ok(prompt) => return Ok(prompt),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    parse_codex_login_prompt(&child.combined_output_snapshot())
}

fn wait_for_codex_login(mut child: CapturedCodexLogin, timeout: Duration) -> Result<()> {
    let started = Instant::now();
    loop {
        match child.child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let _ = child.collect_output();
                return Ok(());
            }
            Ok(Some(status)) => {
                let output = child.collect_output();
                return Err(format_codex_login_exit_error(status, &output));
            }
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let output = child.kill_and_collect();
                    let detail = summarize_captured_output(&output)
                        .map(|message| format!(": {message}"))
                        .unwrap_or_default();
                    bail!("Codex device login timed out before authorization completed{detail}");
                }
                thread::sleep(Duration::from_millis(500));
            }
            Err(error) => {
                let output = child.kill_and_collect();
                let detail = summarize_captured_output(&output)
                    .map(|message| format!(": {message}"))
                    .unwrap_or_default();
                bail!("failed to wait for codex device login: {error}{detail}");
            }
        }
    }
}

fn format_codex_login_exit_error(
    status: std::process::ExitStatus,
    output: &CollectedOutput,
) -> anyhow::Error {
    let detail = summarize_captured_output(output)
        .map(|message| format!(": {message}"))
        .unwrap_or_default();
    anyhow!(
        "codex device login exited with {}{detail}",
        describe_exit_status(status)
    )
}

fn describe_exit_status(status: std::process::ExitStatus) -> String {
    status
        .code()
        .map(|code| format!("code {code}"))
        .unwrap_or_else(|| status.to_string())
}

fn summarize_captured_output(output: &CollectedOutput) -> Option<String> {
    summarize_output_text(&output.stderr).or_else(|| summarize_output_text(&output.stdout))
}

fn summarize_output_text(output: &str) -> Option<String> {
    let lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return None;
    }

    Some(
        lines
            .into_iter()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" | "),
    )
}

struct BrowserSession {
    child: Child,
    profile_dir: PathBuf,
}

impl BrowserSession {
    fn close(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.profile_dir);
    }
}

fn run_browser_autofill(
    chrome: &Path,
    prompt: &CodexDeviceAuthPrompt,
    request: &AutofillRequest,
) -> Result<BrowserSession> {
    let port = reserve_local_port()?;
    let profile_dir = std::env::temp_dir().join(format!(
        "scodex-codex-device-login-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&profile_dir).with_context(|| {
        format!(
            "failed to create temp Chrome profile {}",
            profile_dir.display()
        )
    })?;

    let mut child = Command::new(chrome)
        .args(chrome_args(&profile_dir, port, &prompt.url))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to launch Chrome at {}", chrome.display()))?;

    if let Err(error) = drive_cdp_autofill(port, request, prompt.code.as_deref()) {
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&profile_dir);
        return Err(error);
    }

    Ok(BrowserSession { child, profile_dir })
}

fn reserve_local_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .context("failed to reserve local browser debugging port")?;
    let port = listener
        .local_addr()
        .context("failed to read local browser debugging port")?
        .port();
    drop(listener);
    Ok(port)
}

fn chrome_args(profile_dir: &Path, port: u16, url: &str) -> Vec<String> {
    vec![
        format!("--remote-debugging-port={port}"),
        format!("--user-data-dir={}", profile_dir.display()),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-extensions".to_string(),
        "--new-window".to_string(),
        url.to_string(),
    ]
}

fn drive_cdp_autofill(port: u16, request: &AutofillRequest, code: Option<&str>) -> Result<()> {
    let debug = env::var_os("SCODEX_AUTOFILL_DEBUG").is_some();
    let ws_url = wait_for_cdp_websocket_url(port)?;
    if debug {
        eprintln!("[scodex-autofill] connected CDP: {ws_url}");
    }
    let (mut socket, _) =
        connect(ws_url.as_str()).context("failed to connect to Chrome DevTools Protocol")?;
    set_socket_read_timeout(&mut socket, Some(CDP_EVENT_TIMEOUT))?;

    let mut last_step: Option<String> = None;
    let mut next_id = 1_u64;

    send_cdp_command(
        &mut socket,
        &mut next_id,
        "Page.enable",
        json!({}),
        &mut |event| handle_cdp_event(event, debug, &mut last_step),
    )?;
    send_cdp_command(
        &mut socket,
        &mut next_id,
        "Runtime.enable",
        json!({}),
        &mut |event| handle_cdp_event(event, debug, &mut last_step),
    )?;
    send_cdp_command(
        &mut socket,
        &mut next_id,
        "Runtime.addBinding",
        json!({ "name": AUTOFILL_BINDING_NAME }),
        &mut |event| handle_cdp_event(event, debug, &mut last_step),
    )?;

    let script = build_autofill_bootstrap_script(&request.email, &request.password, code);
    send_cdp_command(
        &mut socket,
        &mut next_id,
        "Page.addScriptToEvaluateOnNewDocument",
        json!({ "source": script }),
        &mut |event| handle_cdp_event(event, debug, &mut last_step),
    )?;

    let response = send_cdp_command(
        &mut socket,
        &mut next_id,
        "Runtime.evaluate",
        json!({
            "expression": script,
            "awaitPromise": false,
            "returnByValue": true
        }),
        &mut |event| handle_cdp_event(event, debug, &mut last_step),
    )?;
    if log_response_state(&response, debug, &mut last_step) {
        return Ok(());
    }

    let started = Instant::now();
    let mut last_fallback = Instant::now();
    while started.elapsed() < AUTOFILL_TIMEOUT {
        if let Some(event) = read_cdp_message(&mut socket)? {
            if handle_cdp_event(&event, debug, &mut last_step)? {
                return Ok(());
            }
            if should_refresh_autofill_state(&event) {
                let response = send_cdp_command(
                    &mut socket,
                    &mut next_id,
                    "Runtime.evaluate",
                    json!({
                        "expression": script,
                        "awaitPromise": false,
                        "returnByValue": true
                    }),
                    &mut |event| handle_cdp_event(event, debug, &mut last_step),
                )?;
                if log_response_state(&response, debug, &mut last_step) {
                    return Ok(());
                }
            }
        }

        if last_fallback.elapsed() >= AUTOFILL_FALLBACK_INTERVAL {
            let response = send_cdp_command(
                &mut socket,
                &mut next_id,
                "Runtime.evaluate",
                json!({
                    "expression": current_autofill_state_expression(),
                    "awaitPromise": false,
                    "returnByValue": true
                }),
                &mut |event| handle_cdp_event(event, debug, &mut last_step),
            )?;
            if log_response_state(&response, debug, &mut last_step) {
                return Ok(());
            }
            last_fallback = Instant::now();
        }
    }

    bail!("OAuth auto-fill timed out before the consent page became ready");
}

fn set_socket_read_timeout(socket: &mut ChromeSocket, timeout: Option<Duration>) -> Result<()> {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream
            .set_read_timeout(timeout)
            .context("failed to configure Chrome DevTools socket timeout")?,
        #[allow(unreachable_patterns)]
        _ => {}
    }
    Ok(())
}

fn send_cdp_command<F>(
    socket: &mut ChromeSocket,
    next_id: &mut u64,
    method: &str,
    params: Value,
    event_handler: &mut F,
) -> Result<Value>
where
    F: FnMut(&Value) -> Result<bool>,
{
    let id = *next_id;
    *next_id += 1;
    let payload = json!({
        "id": id,
        "method": method,
        "params": params
    });
    socket
        .send(Message::Text(payload.to_string()))
        .context("failed to send Chrome DevTools command")?;

    let started = Instant::now();
    while started.elapsed() < CDP_COMMAND_TIMEOUT {
        let Some(message) = read_cdp_message(socket)? else {
            continue;
        };
        if message.get("id").and_then(Value::as_u64) == Some(id) {
            return Ok(message);
        }
        if message.get("method").is_some() {
            let _ = event_handler(&message)?;
        }
    }

    bail!("Chrome DevTools command timed out: {method}");
}

fn read_cdp_message(socket: &mut ChromeSocket) -> Result<Option<Value>> {
    loop {
        match socket.read() {
            Ok(Message::Text(text)) => {
                let response = serde_json::from_str(&text)
                    .context("failed to parse Chrome DevTools response")?;
                return Ok(Some(response));
            }
            Ok(_) => continue,
            Err(WsError::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error).context("failed to read Chrome DevTools response"),
        }
    }
}

fn handle_cdp_event(event: &Value, debug: bool, last_step: &mut Option<String>) -> Result<bool> {
    let Some(method) = event.get("method").and_then(Value::as_str) else {
        return Ok(false);
    };

    if method != "Runtime.bindingCalled" {
        return Ok(false);
    }

    let params = event
        .get("params")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("Runtime.bindingCalled event missing params"))?;
    if params.get("name").and_then(Value::as_str) != Some(AUTOFILL_BINDING_NAME) {
        return Ok(false);
    }

    let payload = params
        .get("payload")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Runtime.bindingCalled event missing payload"))?;
    let value: Value =
        serde_json::from_str(payload).context("failed to parse autofill progress payload")?;
    log_autofill_state(&value, debug, last_step);
    Ok(autofill_completed(&value))
}

fn should_refresh_autofill_state(event: &Value) -> bool {
    matches!(
        event.get("method").and_then(Value::as_str),
        Some("Page.frameNavigated")
            | Some("Page.loadEventFired")
            | Some("Runtime.executionContextCreated")
    )
}

fn log_response_state(response: &Value, debug: bool, last_step: &mut Option<String>) -> bool {
    let Some(value) = response.pointer("/result/result/value") else {
        return false;
    };
    log_autofill_state(value, debug, last_step);
    autofill_completed(value)
}

fn log_autofill_state(value: &Value, debug: bool, last_step: &mut Option<String>) {
    if !debug {
        return;
    }
    let step = value
        .get("step")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    if last_step.as_deref() == Some(step.as_str()) {
        return;
    }
    eprintln!("[scodex-autofill] step={step} detail={value}");
    *last_step = Some(step);
}

fn autofill_completed(value: &Value) -> bool {
    value
        .get("autofillCompleted")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn current_autofill_state_expression() -> &'static str {
    r#"(() => globalThis.__scodexAutofillState?.latestReport || {
  step: "waiting",
  autofillCompleted: false,
  url: location.href
})()"#
}

fn wait_for_cdp_websocket_url(port: u16) -> Result<String> {
    let client = reqwest::blocking::Client::new();
    let debug = env::var_os("SCODEX_AUTOFILL_DEBUG").is_some();
    let started = Instant::now();
    let endpoints = [
        format!("http://127.0.0.1:{port}/json/list"),
        format!("http://127.0.0.1:{port}/json"),
    ];
    let mut last_error: Option<String> = None;

    while started.elapsed() < CDP_READY_TIMEOUT {
        for endpoint in &endpoints {
            match client.get(endpoint).send() {
                Ok(response) => {
                    let status = response.status();
                    match response.text() {
                        Ok(body) => {
                            let body = body.trim();
                            if !status.is_success() {
                                last_error = Some(format!("{endpoint} returned HTTP {status}"));
                                continue;
                            }
                            if body.is_empty() {
                                last_error = Some(format!("{endpoint} returned an empty body"));
                                continue;
                            }

                            match serde_json::from_str::<Vec<Value>>(body) {
                                Ok(pages) => {
                                    if let Some(url) = select_cdp_page_websocket_url(&pages) {
                                        return Ok(url.to_string());
                                    }
                                    last_error = Some(format!(
                                        "{endpoint} returned {} targets but no OpenAI auth page yet",
                                        pages.len()
                                    ));
                                }
                                Err(error) => {
                                    if debug {
                                        eprintln!(
                                            "[scodex-autofill] invalid CDP page list from {endpoint}: {error}"
                                        );
                                    }
                                    last_error =
                                        Some(format!("{endpoint} returned invalid JSON: {error}"));
                                }
                            }
                        }
                        Err(error) => {
                            last_error = Some(format!("failed to read {endpoint}: {error}"));
                        }
                    }
                }
                Err(error) => {
                    last_error = Some(format!("failed to query {endpoint}: {error}"));
                }
            }
        }
        thread::sleep(CDP_POLL_INTERVAL);
    }

    let detail = last_error
        .map(|message| format!(": {message}"))
        .unwrap_or_default();
    bail!("Chrome DevTools Protocol did not become ready{detail}");
}

fn select_cdp_page_websocket_url(pages: &[Value]) -> Option<&str> {
    pages.iter().find_map(|page| {
        let is_page = page.get("type").and_then(Value::as_str) == Some("page");
        let url = page.get("url").and_then(Value::as_str).unwrap_or_default();
        if !is_page || !is_openai_auth_page_url(url) {
            return None;
        }

        page.get("webSocketDebuggerUrl").and_then(Value::as_str)
    })
}

fn is_openai_auth_page_url(url: &str) -> bool {
    url == "https://auth.openai.com/codex/device" || url.starts_with("https://auth.openai.com/")
}

fn build_autofill_bootstrap_script(email: &str, password: &str, code: Option<&str>) -> String {
    let email = serde_json::to_string(email).expect("email json string");
    let password = serde_json::to_string(password).expect("password json string");
    let code = serde_json::to_string(code.unwrap_or("")).expect("code json string");

    format!(
        r#"
(() => {{
  const email = {email};
  const passwordValue = {password};
  const code = {code};
  const bindingName = "{AUTOFILL_BINDING_NAME}";
  const state = globalThis.__scodexAutofillState || (globalThis.__scodexAutofillState = {{}});
  const report = (detail) => {{
    const normalized = {{
      url: location.href,
      ...detail,
    }};
    state.latestReport = normalized;
    const reportKey = JSON.stringify([
      normalized.step || "",
      normalized.url || "",
      normalized.inputs ?? null,
      normalized.autofillCompleted === true
    ]);
    if (state.lastReportKey !== reportKey) {{
      state.lastReportKey = reportKey;
      if (typeof globalThis[bindingName] === "function") {{
        try {{
          globalThis[bindingName](JSON.stringify(normalized));
        }} catch (_error) {{}}
      }}
    }}
    return normalized;
  }};
  const visible = (el) => {{
    const rect = el.getBoundingClientRect();
    const style = window.getComputedStyle(el);
    return rect.width > 0 && rect.height > 0 && style.visibility !== "hidden" && style.display !== "none";
  }};
  const typeable = (input) => {{
    if (input.disabled || input.readOnly) return false;
    return !["hidden", "submit", "button", "checkbox", "radio", "file"].includes((input.type || "").toLowerCase());
  }};
  const labelFor = (input) => `${{input.name || ""}} ${{input.id || ""}} ${{input.placeholder || ""}} ${{input.autocomplete || ""}} ${{(input.getAttribute("aria-label") || "")}}`.toLowerCase();
  const setValue = (input, value) => {{
    input.focus();
    const prototype = input instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    const setter = Object.getOwnPropertyDescriptor(prototype, "value")?.set;
    if (setter) {{
      setter.call(input, value);
    }} else {{
      input.value = value;
    }}
    input.dispatchEvent(new InputEvent("input", {{ bubbles: true, inputType: "insertText", data: value }}));
    input.dispatchEvent(new Event("change", {{ bubbles: true }}));
  }};
  const recentlyClicked = (step) => {{
    const lastClick = state.lastClick;
    return !!lastClick && lastClick.step === step && lastClick.url === location.href && Date.now() - lastClick.at < 900;
  }};
  const rememberClick = (step) => {{
    state.lastClick = {{ step, url: location.href, at: Date.now() }};
  }};
  const clickSubmit = (step, buttons, findButton, pattern = /continue|next|sign in|log in|submit|继续|下一步|登录|登入|提交/i) => {{
    if (recentlyClicked(step)) return false;
    const button = findButton(pattern);
    if (button) {{
      rememberClick(step);
      button.click();
      return true;
    }}
    const fallback = buttons.find((item) => !item.disabled);
    if (fallback) {{
      rememberClick(step);
      fallback.click();
      return true;
    }}
    return false;
  }};
  const fillIfPossible = () => {{
    const inputs = Array.from(document.querySelectorAll("input")).filter(visible);
    const typeableInputs = inputs.filter(typeable);
    const buttons = Array.from(document.querySelectorAll("button,input[type=submit],input[type=button],a[role=button]")).filter(visible);
    const findButton = (pattern) => buttons.find((item) => !item.disabled && pattern.test(`${{item.innerText || ""}} ${{item.value || ""}} ${{item.getAttribute("aria-label") || ""}}`));
    const codeInput = typeableInputs.find((input) => /code|device|otp/.test(labelFor(input)));
    if (code && codeInput) {{
      if (codeInput.value.trim() !== code) {{
        setValue(codeInput, code);
      }}
      const clicked = clickSubmit("code", buttons, findButton);
      return report({{ step: clicked ? "code-submit" : "code", autofillCompleted: false, inputs: typeableInputs.length }});
    }}

    const emailInput = typeableInputs.find((input) => input.type === "email" || /email|username|identifier/.test(labelFor(input)));
    if (emailInput) {{
      if (emailInput.value.trim().toLowerCase() !== email.toLowerCase()) {{
        setValue(emailInput, email);
      }}
      const clicked = clickSubmit("email", buttons, findButton);
      return report({{ step: clicked ? "email-submit" : "email", autofillCompleted: false, inputs: typeableInputs.length }});
    }}

    const passwordInput = typeableInputs.find((input) => input.type === "password" || /password/.test(labelFor(input)));
    if (passwordInput) {{
      if (passwordInput.value !== passwordValue) {{
        setValue(passwordInput, passwordValue);
      }}
      const clicked = clickSubmit("password", buttons, findButton);
      return report({{ step: clicked ? "password-submit" : "password", autofillCompleted: false, inputs: typeableInputs.length }});
    }}

    if (typeableInputs.length === 0) {{
      const clicked = clickSubmit(
        "consent",
        buttons,
        findButton,
        /authorize|authoriz|allow|grant|approve|confirm|continue|授权|允许|确认|同意|继续/i
      );
      if (clicked) {{
        return report({{ step: "consent-submit", autofillCompleted: true }});
      }}
      const consentBtn = findButton(/authorize|authoriz|allow|grant|approve|confirm|continue|授权|允许|确认|同意|继续/i);
      if (consentBtn) {{
        return report({{ step: "consent", autofillCompleted: true }});
      }}
    }}

    return report({{ step: "waiting", autofillCompleted: false, inputs: typeableInputs.length }});
  }};
  const queueFill = () => {{
    if (state.fillScheduled) return;
    state.fillScheduled = true;
    queueMicrotask(() => {{
      state.fillScheduled = false;
      state.fillIfPossible?.();
    }});
  }};

  state.fillIfPossible = fillIfPossible;
  if (!state.installed) {{
    state.installed = true;
    if (!state.observer) {{
      const target = document.documentElement || document;
      if (target) {{
        state.observer = new MutationObserver(() => queueFill());
        state.observer.observe(target, {{
          subtree: true,
          childList: true,
          attributes: true
        }});
      }}
    }}
    if (!state.listenersInstalled) {{
      document.addEventListener("readystatechange", queueFill, true);
      window.addEventListener("load", queueFill, true);
      window.addEventListener("pageshow", queueFill, true);
      window.addEventListener("popstate", queueFill, true);
      window.addEventListener("hashchange", queueFill, true);
      state.listenersInstalled = true;
    }}
  }}

  const result = fillIfPossible();
  setTimeout(() => state.fillIfPossible?.(), 50);
  setTimeout(() => state.fillIfPossible?.(), 250);
  setTimeout(() => state.fillIfPossible?.(), 750);
  return result;
}})();
"#
    )
}

pub fn parse_codex_login_prompt(raw: &str) -> Result<CodexDeviceAuthPrompt> {
    let plain = strip_ansi(raw);
    let url = plain
        .split_whitespace()
        .find(|part| {
            let value = part.trim();
            value.starts_with("https://auth.openai.com/oauth/authorize?")
                || value == "https://auth.openai.com/codex/device"
        })
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Codex login URL was not found"))?;

    let code = plain
        .split_whitespace()
        .find(|part| is_device_code(part))
        .map(|part| part.trim().to_string());

    Ok(CodexDeviceAuthPrompt { url, code })
}

fn strip_ansi(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            let _ = chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        output.push(ch);
    }

    output
}

fn is_device_code(value: &str) -> bool {
    let trimmed = value.trim();
    let Some((left, right)) = trimmed.split_once('-') else {
        return false;
    };

    left.len() == 4
        && right.len() == 5
        && left.chars().all(|ch| ch.is_ascii_alphanumeric())
        && right.chars().all(|ch| ch.is_ascii_alphanumeric())
}

pub fn resolve_chromium_binary() -> Option<PathBuf> {
    let app_roots = default_chromium_app_roots();
    resolve_chromium_binary_from(&app_roots, env::var_os("PATH"))
}

fn default_chromium_app_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }
    roots
}

fn resolve_chromium_binary_from(
    app_roots: &[PathBuf],
    path_var: Option<OsString>,
) -> Option<PathBuf> {
    const KNOWN_APP_BUNDLES: &[(&str, &str)] = &[
        ("Google Chrome.app", "Google Chrome"),
        ("Google Chrome Canary.app", "Google Chrome Canary"),
        ("Google Chrome Beta.app", "Google Chrome Beta"),
        ("Google Chrome Dev.app", "Google Chrome Dev"),
        ("Chromium.app", "Chromium"),
        ("谷歌浏览器.app", "Google Chrome"),
    ];
    const CHROMIUM_EXECUTABLES: &[&str] = &[
        "Google Chrome",
        "Google Chrome Canary",
        "Google Chrome Beta",
        "Google Chrome Dev",
        "Chromium",
    ];

    for root in app_roots {
        for (bundle, binary) in KNOWN_APP_BUNDLES {
            let candidate = root.join(bundle).join("Contents/MacOS").join(binary);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    for root in app_roots {
        if let Some(found) = scan_root_for_chromium_bundle(root, CHROMIUM_EXECUTABLES) {
            return Some(found);
        }
    }

    let path_var = path_var?;
    for dir in env::split_paths(&path_var) {
        for name in [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
        ] {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    None
}

fn scan_root_for_chromium_bundle(root: &Path, binaries: &[&str]) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("app") {
            continue;
        }
        let macos_dir = path.join("Contents/MacOS");
        if !macos_dir.is_dir() {
            continue;
        }
        for binary in binaries {
            let candidate = macos_dir.join(binary);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_device_auth_prompt_with_ansi_colors() {
        let prompt = "\
Welcome to Codex [v\u{1b}[90m0.121.0\u{1b}[0m]

Follow these steps to sign in with ChatGPT using device code authorization:

1. Open this link in your browser and sign in to your account
   \u{1b}[94mhttps://auth.openai.com/codex/device\u{1b}[0m

2. Enter this one-time code \u{1b}[90m(expires in 15 minutes)\u{1b}[0m
   \u{1b}[94mK88F-TC9HS\u{1b}[0m
";

        let parsed = parse_codex_login_prompt(prompt).expect("prompt");

        assert_eq!(parsed.url, "https://auth.openai.com/codex/device");
        assert_eq!(parsed.code, Some("K88F-TC9HS".to_string()));
    }

    #[test]
    fn rejects_prompt_without_device_url() {
        let error = parse_codex_login_prompt("code ABCD-EFGH")
            .expect_err("missing url")
            .to_string();

        assert_eq!(error, "Codex login URL was not found");
    }

    #[test]
    fn resolves_chrome_from_macos_app_bundle() {
        let root = temp_test_dir("chrome-resolver");
        let chrome = root.join("Google Chrome.app/Contents/MacOS/Google Chrome");
        fs::create_dir_all(chrome.parent().expect("chrome parent")).expect("chrome dir");
        fs::write(&chrome, "").expect("chrome binary");

        let resolved = resolve_chromium_binary_from(&[root.clone()], None);

        assert_eq!(resolved, Some(chrome));
    }

    #[test]
    fn resolves_chrome_from_localized_app_bundle_name() {
        let root = temp_test_dir("chrome-localized");
        let chrome = root.join("谷歌浏览器.app/Contents/MacOS/Google Chrome");
        fs::create_dir_all(chrome.parent().expect("chrome parent")).expect("chrome dir");
        fs::write(&chrome, "").expect("chrome binary");

        let resolved = resolve_chromium_binary_from(&[root.clone()], None);

        assert_eq!(resolved, Some(chrome));
    }

    #[test]
    fn resolves_chrome_from_renamed_bundle_via_scan() {
        let root = temp_test_dir("chrome-renamed");
        // Some user-renamed bundle that isn't in our known list.
        let chrome = root.join("MyBrowser.app/Contents/MacOS/Google Chrome");
        fs::create_dir_all(chrome.parent().expect("chrome parent")).expect("chrome dir");
        fs::write(&chrome, "").expect("chrome binary");

        let resolved = resolve_chromium_binary_from(&[root.clone()], None);

        assert_eq!(resolved, Some(chrome));
    }

    #[test]
    fn resolves_chromium_from_path() {
        let root = temp_test_dir("chromium-path");
        let bin = root.join("bin");
        let chromium = bin.join("chromium");
        fs::create_dir_all(&bin).expect("bin dir");
        fs::write(&chromium, "").expect("chromium binary");

        let resolved = resolve_chromium_binary_from(
            &[],
            Some(OsString::from(bin.to_string_lossy().to_string())),
        );

        assert_eq!(resolved, Some(chromium));
    }

    #[test]
    fn autofill_script_fills_credentials_and_submits_consent() {
        let script = build_autofill_bootstrap_script(
            "user@example.com",
            "secret-password",
            Some("K88F-TC9HS"),
        );

        assert!(script.contains("user@example.com"));
        assert!(script.contains("secret-password"));
        assert!(script.contains("K88F-TC9HS"));
        assert!(script.contains("autofillCompleted"));
        assert!(script.contains("__scodexAutofillState"));
        assert!(script.contains("__scodexAutofillReport"));
        assert!(script.contains("MutationObserver"));
        assert!(script.contains("HTMLInputElement.prototype"));
        assert!(script.contains("InputEvent"));
        assert!(script.contains("email-submit"));
        assert!(script.contains("password-submit"));
        assert!(script.contains("recentlyClicked"));
        assert!(script.contains("queueMicrotask"));
        assert!(script.contains("setTimeout(() => state.fillIfPossible?.(), 750)"));
        assert!(script.contains("step: \"waiting\""));
        assert!(script.contains("clickSubmit("));
        assert!(script.contains(
            "pattern = /continue|next|sign in|log in|submit|继续|下一步|登录|登入|提交/i"
        ));
        assert!(script.contains("const clicked = clickSubmit("));
        assert!(script.contains("const consentBtn = findButton("));
        assert!(script.contains("continue"));
        assert!(script.contains("继续"));
        assert!(script.contains("step: \"consent-submit\""));
        assert!(script.contains("typeableInputs.length === 0"));
    }

    #[test]
    fn command_args_use_temporary_profile_and_debug_port() {
        let profile = std::path::Path::new("/tmp/scodex-profile");
        let args = chrome_args(profile, 9333, "https://auth.openai.com/codex/device");

        assert!(args.contains(&"--remote-debugging-port=9333".to_string()));
        assert!(args.contains(&"--user-data-dir=/tmp/scodex-profile".to_string()));
        assert!(args.contains(&"https://auth.openai.com/codex/device".to_string()));
    }

    #[test]
    fn selects_openai_auth_page_instead_of_background_targets() {
        let targets = serde_json::json!([
            {
                "type": "background_page",
                "url": "chrome-extension://bg/background.html",
                "webSocketDebuggerUrl": "ws://127.0.0.1:9229/devtools/page/background"
            },
            {
                "type": "page",
                "url": "https://auth.openai.com/log-in",
                "webSocketDebuggerUrl": "ws://127.0.0.1:9229/devtools/page/openai"
            },
            {
                "type": "service_worker",
                "url": "chrome-extension://sw/service_worker.js",
                "webSocketDebuggerUrl": "ws://127.0.0.1:9229/devtools/page/worker"
            }
        ]);
        let pages = targets.as_array().expect("targets");

        let selected = select_cdp_page_websocket_url(pages).expect("openai page");

        assert_eq!(selected, "ws://127.0.0.1:9229/devtools/page/openai");
    }

    #[test]
    fn success_detection_matches_autofill_completed_flag() {
        let done = serde_json::json!({
            "result": { "result": { "value": { "step": "done", "autofillCompleted": true } } }
        });
        let progress = serde_json::json!({
            "result": { "result": { "value": { "step": "password", "autofillCompleted": false } } }
        });
        let mut last_step = None;

        assert!(log_response_state(&done, false, &mut last_step));
        assert!(!log_response_state(&progress, false, &mut last_step));
    }

    fn temp_test_dir(prefix: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("scodex-{prefix}-{unique}"));
        fs::create_dir_all(&path).expect("temp dir");
        path
    }
}
