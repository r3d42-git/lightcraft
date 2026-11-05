//! Bounded, subscription-only Codex app-server image editing. No credentials are read here.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const MAX_IMAGE: usize = 32 * 1024 * 1024;
const MAX_LINE: usize = 48 * 1024 * 1024;
const MAX_OUTPUT: usize = 128 * 1024 * 1024;
const MAX_MESSAGES: usize = 20_000;
const POLL: Duration = Duration::from_millis(40);

#[derive(Debug)]
pub(super) struct GeneratedImage {
    pub png: Vec<u8>,
    /// Names the transport and account route, never an unreported image-model revision.
    pub provenance: String,
}

pub(super) fn find_executable() -> Result<PathBuf, String> {
    let bundled = Path::new("/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex");
    if bundled.is_file() {
        return Ok(bundled.to_path_buf());
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths).filter(|p| p.is_absolute()) {
            let candidate = dir.join("codex");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err("Codex executable not found. Install or update Codex/ChatGPT and sign in with ChatGPT.".into())
}

fn check(cancel: &AtomicBool, deadline: Instant) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        return Err("Cloud editing cancelled.".into());
    }
    if Instant::now() >= deadline {
        return Err("Cloud editing timed out.".into());
    }
    Ok(())
}

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    messages: mpsc::Receiver<Result<Value, String>>,
    stopped: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    count: usize,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.stdin.take();
        // Kill the isolated process group as well, so an inherited stdout pipe cannot
        // leave the reader waiting on a child process. Never signal a caller's group.
        #[cfg(unix)]
        {
            let _ = Command::new("/bin/kill")
                .args(["-KILL", "--", &format!("-{}", self.child.id())])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(writer) = self.writer.take()
            && writer.is_finished()
        {
            let _ = writer.join();
        }
        if let Some(reader) = self.reader.take() {
            // A hostile descendant may have escaped its process group. Do not block
            // the photo editor indefinitely on an inherited pipe in that case.
            let until = Instant::now() + Duration::from_millis(250);
            while !reader.is_finished() && Instant::now() < until {
                thread::sleep(Duration::from_millis(5));
            }
            if reader.is_finished() {
                let _ = reader.join();
            }
        }
    }
}

impl Server {
    fn spawn(executable: &Path, disabled_mcp: &[String]) -> Result<Self, String> {
        let mut cmd = Command::new(executable);
        cmd.args(["app-server", "--listen", "stdio://"])
            .args(["-c", "model_provider=\"openai\"", "-c", "mcp_servers={}", "-c", "web_search=\"disabled\""])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .current_dir(std::env::temp_dir());
        for name in disabled_mcp {
            // Codex's CLI path parser splits dotted keys; quoted path components
            // are not TOML-unquoted and would create a new invalid server entry.
            if !safe_mcp_name(name) {
                return Err("Unsupported Codex MCP server name.".into());
            }
            // A full disabled transport is necessary for entries supplied by the
            // desktop integration rather than the user TOML. A partial table has
            // no transport and prevents Codex from starting at all.
            cmd.args(["-c", &format!("mcp_servers.{name}={{command=\"/usr/bin/false\",enabled=false}}")]);
        }
        for feature in ["apps", "shell_tool", "computer_use", "browser_use", "memories", "multi_agent", "hooks", "plugins", "remote_plugin"] {
            cmd.args(["-c", &format!("features.{feature}=false")]);
        }
        for key in
            ["OPENAI_API_KEY", "CODEX_API_KEY", "CODEX_ACCESS_TOKEN", "OPENAI_BASE_URL", "OPENAI_ORG_ID", "OPENAI_ORGANIZATION", "OPENAI_PROJECT_ID"]
        {
            cmd.env_remove(key);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let child = cmd.spawn().map_err(|_| "Could not start Codex app-server.".to_string())?;
        let (tx, messages) = mpsc::sync_channel(8);
        let stopped = Arc::new(AtomicBool::new(false));
        let mut server = Self { child, stdin: None, messages, stopped, reader: None, writer: None, count: 0 };
        server.stdin = server.child.stdin.take();
        let stdout = server.child.stdout.take().ok_or("Codex stdout unavailable.")?;
        let stop = Arc::clone(&server.stopped);
        server.reader = Some(
            thread::Builder::new()
                .name("codex-output".into())
                .spawn(move || {
                    let mut input = BufReader::new(stdout);
                    let mut total = 0usize;
                    loop {
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                        let value = read_message(&mut input, &mut total);
                        let terminal = value.is_err();
                        let mut pending = value;
                        loop {
                            if stop.load(Ordering::Acquire) {
                                return;
                            }
                            match tx.try_send(pending) {
                                Ok(()) => break,
                                Err(mpsc::TrySendError::Disconnected(_)) => return,
                                Err(mpsc::TrySendError::Full(value)) => {
                                    pending = value;
                                    thread::sleep(Duration::from_millis(5));
                                }
                            }
                        }
                        if terminal {
                            break;
                        }
                    }
                })
                .map_err(|_| "Could not start Codex output reader.")?,
        );
        Ok(server)
    }
    fn send(&mut self, id: Option<u64>, method: &str, params: Value, cancel: &AtomicBool, deadline: Instant) -> Result<(), String> {
        check(cancel, deadline)?;
        let mut value = json!({"method":method,"params":params});
        if let Some(id) = id {
            value["id"] = json!(id);
        }
        let mut bytes = serde_json::to_vec(&value).map_err(|_| "Could not encode Codex request.")?;
        bytes.push(b'\n');
        let mut stdin = self.stdin.take().ok_or("Codex stdin unavailable.")?;
        let (tx, rx) = mpsc::sync_channel(1);
        self.writer = Some(
            thread::Builder::new()
                .name("codex-input".into())
                .spawn(move || {
                    let result = stdin.write_all(&bytes).map_err(|_| "Codex request failed.".to_string());
                    let _ = tx.send((stdin, result));
                })
                .map_err(|_| "Could not start Codex request writer.")?,
        );
        loop {
            check(cancel, deadline)?;
            match rx.recv_timeout(POLL) {
                Ok((stdin, result)) => {
                    self.stdin = Some(stdin);
                    if let Some(writer) = self.writer.take() {
                        let _ = writer.join();
                    }
                    return result;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err("Codex request writer closed.".into()),
            }
        }
    }
    fn next(&mut self, cancel: &AtomicBool, deadline: Instant) -> Result<Value, String> {
        loop {
            check(cancel, deadline)?;
            match self.messages.recv_timeout(POLL) {
                Ok(value) => {
                    self.count = self.count.saturating_add(1);
                    if self.count > MAX_MESSAGES {
                        return Err("Codex message limit exceeded.".into());
                    }
                    let value = value?;
                    if value.get("method").is_some() && value.get("id").is_some() {
                        return Err("Codex requested an unsupported tool or approval.".into());
                    }
                    if value.get("error").is_some() {
                        return Err("Codex protocol request failed.".into());
                    }
                    return Ok(value);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err("Codex closed before completing the image.".into()),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
    fn request(&mut self, id: u64, method: &str, params: Value, cancel: &AtomicBool, deadline: Instant) -> Result<Value, String> {
        self.send(Some(id), method, params, cancel, deadline)?;
        loop {
            let value = self.next(cancel, deadline)?;
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return value.get("result").cloned().ok_or_else(|| "Codex response has no result.".into());
            }
            if value.get("id").is_some() {
                return Err("Codex response identifier mismatch.".into());
            }
        }
    }
}

fn configured_mcp(executable: &Path, cancel: &AtomicBool, deadline: Instant) -> Result<Vec<String>, String> {
    check(cancel, deadline)?;
    let mut command = Command::new(executable);
    command.args(["mcp", "list", "--json"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    for name in ["OPENAI_API_KEY", "CODEX_API_KEY", "CODEX_ACCESS_TOKEN"] {
        command.env_remove(name);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command.spawn().map_err(|_| "Could not inspect Codex MCP configuration.")?;
    let (tx, messages) = mpsc::sync_channel(1);
    let stopped = Arc::new(AtomicBool::new(false));
    let mut server = Server { child, stdin: None, messages, stopped, reader: None, writer: None, count: 0 };
    let stdout = server.child.stdout.take().ok_or("Codex configuration output unavailable.")?;
    server.reader = Some(
        thread::Builder::new()
            .name("codex-config".into())
            .spawn(move || {
                let mut data = Vec::new();
                let read = stdout.take(1_048_577).read_to_end(&mut data);
                let value = if read.is_err() || data.len() > 1_048_576 {
                    Err("Codex MCP configuration output limit exceeded.".to_string())
                } else {
                    serde_json::from_slice::<Value>(&data).map_err(|_| "Invalid Codex MCP configuration response.".to_string())
                };
                let _ = tx.send(value);
            })
            .map_err(|_| "Could not start Codex configuration reader.")?,
    );
    let value = server.next(cancel, deadline)?;
    loop {
        check(cancel, deadline)?;
        if let Some(status) = server.child.try_wait().map_err(|_| "Could not inspect Codex configuration process.")? {
            if !status.success() {
                return Err("Could not inspect Codex MCP configuration.".into());
            }
            break;
        }
        thread::sleep(POLL);
    }
    let entries = value.as_array().ok_or("Invalid Codex MCP configuration response.")?;
    if entries.len() > 256 {
        return Err("Codex MCP configuration limit exceeded.".into());
    }
    entries
        .iter()
        .map(|entry| {
            entry
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| safe_mcp_name(name))
                .map(str::to_owned)
                .ok_or_else(|| "Invalid Codex MCP server name.".into())
        })
        .collect()
}

fn safe_mcp_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 256 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn read_message(input: &mut impl BufRead, total: &mut usize) -> Result<Value, String> {
    let mut line = Vec::new();
    loop {
        let part = input.fill_buf().map_err(|_| "Could not read Codex output.")?;
        if part.is_empty() {
            return Err("Codex closed before completion.".into());
        }
        let count = part.iter().position(|b| *b == b'\n').map_or(part.len(), |p| p + 1);
        *total = total.saturating_add(count);
        if line.len().saturating_add(count) > MAX_LINE || *total > MAX_OUTPUT {
            return Err("Codex output size limit exceeded.".into());
        }
        let done = part.get(count.saturating_sub(1)) == Some(&b'\n');
        line.extend_from_slice(part.get(..count).ok_or("Invalid Codex output.")?);
        input.consume(count);
        if done {
            break;
        }
    }
    serde_json::from_slice(&line).map_err(|_| "Invalid Codex protocol output.".into())
}

pub(super) fn edit(executable: &Path, images: &[PathBuf], prompt: &str, cancel: &AtomicBool, deadline: Instant) -> Result<GeneratedImage, String> {
    check(cancel, deadline)?;
    if images.is_empty() || images.len() > 3 || prompt.len() > 16_384 {
        return Err("Invalid cloud edit inputs.".into());
    }
    let mut input = vec![json!({"type":"text","text":prompt,"text_elements":[]})];
    let mut expected_inputs = Vec::new();
    for path in images {
        if !path.is_absolute() || !path.is_file() {
            return Err("Cloud edit input image is unavailable.".into());
        }
        let path = fs::canonicalize(path).map_err(|_| "Cloud input image unavailable.")?;
        let mut data = Vec::new();
        File::open(&path)
            .map_err(|_| "Cloud input image unavailable.")?
            .take((MAX_IMAGE + 1) as u64)
            .read_to_end(&mut data)
            .map_err(|_| "Could not read cloud input image.")?;
        if data.is_empty() || data.len() > MAX_IMAGE {
            return Err("Cloud input image size limit exceeded.".into());
        }
        expected_inputs.push((path.clone(), format!("{:x}", Sha256::digest(&data))));
        input.push(json!({"type":"localImage","path":path}));
    }
    // Keep the default Codex image-tool instructions, and make the local edit
    // references explicit. An attachment alone can otherwise be treated as visual
    // inspiration for a fresh generation, which has no observed input identity.
    let reference_paths: Vec<&PathBuf> = expected_inputs.iter().map(|(path, _)| path).collect();
    let references = serde_json::to_string(&reference_paths).map_err(|_| "Could not encode cloud edit references.")?;
    *input.first_mut().ok_or("Cloud edit text input unavailable.")? = json!({"type":"text","text":format!("{prompt}\n\nMANDATORY EDIT INPUT CONTRACT: Invoke the built-in image generation tool exactly once as an image edit, with referenced_image_paths set to this exact JSON array of canonical local paths: {references}. Include ALL these paths. The first image is the original edit target; subsequent images are the supplied edit guides. Do not use num_last_images_to_include. Do not perform a fresh generation without referenced_image_paths. Do not use shell, scripts, or an API fallback. If the image tool cannot use these explicit references, stop without generating an image."),"text_elements":[]});
    let disabled_mcp = configured_mcp(executable, cancel, deadline)?;
    let mut server = Server::spawn(executable, &disabled_mcp)?;
    let init = server.request(
        1,
        "initialize",
        json!({"clientInfo":{"name":"lightcraft","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}),
        cancel,
        deadline,
    )?;
    server.send(None, "initialized", json!({}), cancel, deadline)?;
    let account = server.request(2, "account/read", json!({"refreshToken":false}), cancel, deadline)?;
    if account.pointer("/account/type").and_then(Value::as_str) != Some("chatgpt") {
        return Err("ChatGPT subscription sign-in is required; API-key billing is disabled.".into());
    }
    let capability = server.request(3, "modelProvider/capabilities/read", json!({}), cancel, deadline)?;
    if capability.get("imageGeneration").and_then(Value::as_bool) != Some(true) {
        return Err("This ChatGPT account does not currently offer image generation through Codex.".into());
    }
    let config = server.request(7, "config/read", json!({"includeLayers":false,"cwd":std::env::temp_dir()}), cancel, deadline)?;
    verify_builtin_route(&config)?;
    // Refuse inherited MCP connections even if a future Codex release changes config merging.
    let mcp = server.request(4, "mcpServerStatus/list", json!({"limit":100}), cancel, deadline)?;
    if !mcp.get("data").and_then(Value::as_array).is_some_and(|servers| {
        servers.iter().all(|server| {
            server.get("name").and_then(Value::as_str).is_some_and(|name| disabled_mcp.iter().any(|n| n == name))
                && server.get("runtimeStatus").is_none_or(|status| status.is_null() || status.as_str() == Some("disabled"))
        })
    }) || mcp.get("nextCursor").is_some_and(|v| !v.is_null())
    {
        return Err("Cloud editing requires Codex with external MCP servers disabled.".into());
    }
    let started = server.request(5, "thread/start", json!({
        "model":"gpt-6.1-sol","modelProvider":"openai","cwd":std::env::temp_dir(),
        "ephemeral":true,"sandbox":"read-only","approvalPolicy":"never",
        "developerInstructions":"Edit the original photograph using only the built-in hosted image generation tool. Its referenced_image_paths argument MUST include ALL canonical paths explicitly listed in the user edit contract. The attached images are edit inputs, not inspiration for a new image. Never use num_last_images_to_include or generate without referenced_image_paths. Generate exactly one edited image, preserve the requested canvas, then stop. Never run commands, scripts, read unrelated files, access other apps, delegate, or request approvals. Never use API keys or an API fallback.",
        "config":{"web_search":"disabled"}
    }), cancel, deadline)?;
    let thread_id =
        started.pointer("/thread/id").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or("Codex thread identifier missing.")?.to_owned();
    let turn = server.request(6, "turn/start", json!({"threadId":thread_id,"effort":"medium","input":input}), cancel, deadline)?;
    let turn_id = turn.pointer("/turn/id").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or("Codex turn identifier missing.")?.to_owned();
    let mut result = Completion::default();
    loop {
        let message = server.next(cancel, deadline)?;
        if result.observe(&message, &thread_id, &turn_id)? {
            let item = result.image.ok_or("Codex completed without an image.")?;
            check(cancel, deadline)?;
            verify_inputs(&item, &expected_inputs)?;
            let png = image_bytes(&item)?;
            check(cancel, deadline)?;
            let agent = init
                .get("userAgent")
                .and_then(Value::as_str)
                .filter(|s| s.len() <= 100 && s.chars().all(|c| !c.is_control()))
                .unwrap_or("version unreported");
            return Ok(GeneratedImage {
                png,
                provenance: format!("ChatGPT subscription / Codex app-server ({agent}); image model revision unreported"),
            });
        }
    }
}

fn verify_builtin_route(response: &Value) -> Result<(), String> {
    let config = response.get("config").and_then(Value::as_object).ok_or("Codex did not return its effective configuration.")?;
    if config.get("model_provider").and_then(Value::as_str) != Some("openai") {
        return Err("Cloud editing requires the built-in OpenAI provider.".into());
    }
    if config.get("chatgpt_base_url").is_some_and(|value| {
        !value.is_null() && !matches!(value.as_str(), Some("" | "https://chatgpt.com/backend-api/" | "https://chatgpt.com/backend-api"))
    }) {
        return Err("Cloud editing does not support a custom ChatGPT endpoint.".into());
    }
    if config.get("openai_base_url").is_some_and(|value| !value.is_null() && value.as_str() != Some("")) {
        return Err("Cloud editing does not support a custom OpenAI endpoint.".into());
    }
    if config
        .get("mcp_servers")
        .and_then(Value::as_object)
        .is_some_and(|servers| servers.values().any(|server| server.get("enabled").and_then(Value::as_bool) != Some(false)))
    {
        return Err("Cloud editing requires external MCP servers to be disabled.".into());
    }
    if config
        .get("model_providers")
        .and_then(|value| value.get("openai"))
        .is_some_and(|value| !value.is_null() && !value.as_object().is_some_and(serde_json::Map::is_empty))
    {
        return Err("Cloud editing does not support custom OpenAI provider settings.".into());
    }
    Ok(())
}

#[derive(Default)]
struct Completion {
    image: Option<Value>,
}
impl Completion {
    fn observe(&mut self, message: &Value, thread_id: &str, turn_id: &str) -> Result<bool, String> {
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        if !matches!(method, "item/completed" | "turn/completed" | "item/started" | "error") {
            return Ok(false);
        }
        let params = message.get("params").ok_or("Codex event parameters missing.")?;
        if params.get("threadId").and_then(Value::as_str) != Some(thread_id) {
            return Err("Codex event thread mismatch.".into());
        }
        let event_turn = if method == "turn/completed" { params.pointer("/turn/id") } else { params.get("turnId") };
        if event_turn.and_then(Value::as_str) != Some(turn_id) {
            return Err("Codex event turn mismatch.".into());
        }
        if method == "error" {
            return Err("Codex image generation failed.".into());
        }
        if method == "turn/completed" {
            if params.pointer("/turn/status").and_then(Value::as_str) != Some("completed")
                || params.pointer("/turn/error").is_some_and(|v| !v.is_null())
            {
                return Err("Codex image turn did not succeed.".into());
            }
            if self.image.is_none() {
                return Err("Codex completed without an image.".into());
            }
            return Ok(true);
        }
        let item = params.get("item").ok_or("Codex item missing.")?;
        match item.get("type").and_then(Value::as_str) {
            Some("imageGeneration") if method == "item/completed" => {
                if item.get("status").and_then(Value::as_str) != Some("completed")
                    || item.get("failure").is_some_and(|v| !v.is_null())
                    || item.get("id").and_then(Value::as_str).is_none_or(str::is_empty)
                {
                    return Err("Codex image generation did not succeed.".into());
                }
                if self.image.is_some() {
                    return Err("Codex returned multiple images; edit rejected.".into());
                }
                self.image = Some(item.clone());
            }
            Some("imageGeneration" | "agentMessage" | "reasoning" | "userMessage" | "plan") => {}
            _ => return Err("Codex attempted an unsupported tool; edit rejected.".into()),
        }
        Ok(false)
    }
}

fn verify_inputs(item: &Value, expected: &[(PathBuf, String)]) -> Result<(), String> {
    let inputs = item
        .get("inputImages")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("Codex did not report which images were edited ({}).", image_metadata_summary(item)))?;
    if expected.iter().any(|(path, hash)| {
        !inputs.iter().any(|input| {
            input.get("path").and_then(Value::as_str).is_some_and(|observed| Path::new(observed) == path)
                && input.get("sha256").and_then(Value::as_str) == Some(hash.as_str())
        })
    }) {
        return Err(format!("Codex did not confirm the supplied edit input images ({}).", image_metadata_summary(item)));
    }
    Ok(())
}

// Deliberately disclose only booleans and bounded counts, never prompt text,
// private paths, inline blobs, credentials, or the raw protocol response.
fn image_metadata_summary(item: &Value) -> String {
    format!(
        "input_metadata={}, input_count={}, completed={}, saved={}, inline={}, failure={}",
        item.get("inputImages").is_some(),
        item.get("inputImages").and_then(Value::as_array).map_or(0, Vec::len).min(MAX_MESSAGES),
        item.get("status").and_then(Value::as_str) == Some("completed"),
        item.get("savedPath").and_then(Value::as_str).is_some_and(|s| !s.is_empty()),
        item.get("result").and_then(Value::as_str).is_some_and(|s| !s.is_empty()),
        item.get("failure").is_some_and(|value| !value.is_null())
    )
}

fn image_bytes(item: &Value) -> Result<Vec<u8>, String> {
    let bytes = if let Some(path) = item.get("savedPath").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        read_saved(Path::new(path))?
    } else {
        let encoded = item.get("result").and_then(Value::as_str).ok_or("Codex returned no readable image attachment.")?;
        decode_base64(encoded.strip_prefix("data:image/png;base64,").unwrap_or(encoded))?
    };
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("Codex output is not PNG.".into());
    }
    let ihdr = bytes.get(12..16).ok_or("Invalid Codex PNG header.")?;
    if ihdr != b"IHDR" {
        return Err("Invalid Codex PNG header.".into());
    }
    let dimension = |offset: usize| -> Result<u32, String> {
        let value: [u8; 4] =
            bytes.get(offset..offset + 4).ok_or("Invalid Codex PNG dimensions.")?.try_into().map_err(|_| "Invalid Codex PNG dimensions.")?;
        Ok(u32::from_be_bytes(value))
    };
    let (width, height) = (dimension(16)?, dimension(20)?);
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 16_777_216 {
        return Err("Codex PNG dimensions exceeded limit.".into());
    }
    // The caller performs the bounded full PNG decode and colour/alpha validation.
    Ok(bytes)
}

fn read_saved(path: &Path) -> Result<Vec<u8>, String> {
    // Codex owns this fixed attachment store. Never follow a response path elsewhere,
    // never remove it, and reject symlinks in every component below the store root.
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".codex")))
        .ok_or("Codex attachment store unavailable.")?;
    read_saved_from_root(path, &home.join("generated_images"))
}

fn read_saved_from_root(path: &Path, root: &Path) -> Result<Vec<u8>, String> {
    let relative = path.strip_prefix(root).map_err(|_| "Codex image path is outside its attachment store.")?;
    if !path.is_absolute() || relative.as_os_str().is_empty() {
        return Err("Invalid Codex image path.".into());
    }
    let mut current = root.to_path_buf();
    for part in relative.components() {
        if !matches!(part, std::path::Component::Normal(_)) {
            return Err("Invalid Codex image path.".into());
        }
        current.push(part);
        if fs::symlink_metadata(&current).map_err(|_| "Codex image file unavailable.")?.file_type().is_symlink() {
            return Err("Codex image symlinks are not accepted.".into());
        }
    }
    if fs::symlink_metadata(root).map_err(|_| "Codex attachment store unavailable.")?.file_type().is_symlink() {
        return Err("Codex attachment-store symlink rejected.".into());
    }
    let canonical_root = fs::canonicalize(root).map_err(|_| "Codex attachment store unavailable.")?;
    let canonical = fs::canonicalize(path).map_err(|_| "Codex image file unavailable.")?;
    if !canonical.starts_with(canonical_root) {
        return Err("Codex image escaped its attachment store.".into());
    }
    let before = fs::symlink_metadata(&canonical).map_err(|_| "Could not inspect Codex image.")?;
    if !before.is_file() {
        return Err("Codex image is not a regular file.".into());
    }
    let file = File::open(&canonical).map_err(|_| "Could not open Codex image.")?;
    let metadata = file.metadata().map_err(|_| "Could not inspect Codex image.")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != metadata.dev() || before.ino() != metadata.ino() {
            return Err("Codex image changed while opening.".into());
        }
    }
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_IMAGE as u64 {
        return Err("Codex image size limit exceeded.".into());
    }
    let mut bytes = Vec::new();
    file.take((MAX_IMAGE + 1) as u64).read_to_end(&mut bytes).map_err(|_| "Could not read Codex image.")?;
    if bytes.len() > MAX_IMAGE {
        return Err("Codex image size limit exceeded.".into());
    }
    Ok(bytes)
}

fn decode_base64(text: &str) -> Result<Vec<u8>, String> {
    if text.is_empty() || text.len() > MAX_IMAGE.div_ceil(3) * 4 || !text.len().is_multiple_of(4) {
        return Err("Invalid Codex inline image size.".into());
    }
    let mut output = Vec::with_capacity(text.len() / 4 * 3);
    for (index, chunk) in text.as_bytes().as_chunks::<4>().0.iter().enumerate() {
        let mut bits = 0u32;
        let mut padding = 0usize;
        for &byte in chunk {
            let n = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' => {
                    padding += 1;
                    0
                }
                _ => return Err("Invalid Codex image encoding.".into()),
            };
            if padding > 0 && byte != b'=' {
                return Err("Invalid Codex image padding.".into());
            }
            bits = (bits << 6) | u32::from(n);
        }
        if padding > 2 || (padding > 0 && index + 1 != text.len() / 4) || (padding == 1 && bits & 0xff != 0) || (padding == 2 && bits & 0xffff != 0) {
            return Err("Invalid Codex image padding.".into());
        }
        for shift in [16, 8, 0].into_iter().take(3 - padding) {
            output.push((bits >> shift) as u8);
        }
    }
    if output.len() > MAX_IMAGE {
        return Err("Codex image size limit exceeded.".into());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_rejects_malformed_and_oversized_lines() {
        let mut total = MAX_OUTPUT;
        assert!(read_message(&mut &b"{}\n"[..], &mut total).is_err());
        assert!(read_message(&mut &b"not json\n"[..], &mut 0).is_err());
        let oversized = vec![b'x'; MAX_LINE + 1];
        assert!(read_message(&mut oversized.as_slice(), &mut 0).is_err());
        assert!(read_message(&mut &b""[..], &mut 0).is_err());
    }
    #[test]
    fn completion_requires_matching_successful_turn_and_one_image() {
        let image = json!({"method":"item/completed","params":{"threadId":"t","turnId":"u","item":{"type":"imageGeneration","id":"i","status":"completed","result":"eA=="}}});
        let end = |status| json!({"method":"turn/completed","params":{"threadId":"t","turn":{"id":"u","status":status}}});
        assert!(Completion::default().observe(&end("completed"), "t", "u").is_err());
        let mut state = Completion::default();
        assert!(!state.observe(&image, "t", "u").unwrap());
        assert!(state.observe(&end("failed"), "t", "u").is_err());
        assert!(state.observe(&end("completed"), "t", "u").unwrap());
        assert!(state.observe(&image, "t", "u").is_err());
        assert!(Completion::default().observe(&image, "other", "u").is_err());
        assert!(Completion::default().observe(&image, "t", "other").is_err());
    }
    #[test]
    fn strict_base64_and_png_validation() {
        assert_eq!(decode_base64("eA==").unwrap(), b"x");
        for value in ["eB==", "eA=+", "e===", "eA==AAAA", "", "%%%%"] {
            assert!(decode_base64(value).is_err(), "{value}");
        }
        assert!(image_bytes(&json!({"result":"eA=="})).is_err());
        assert!(read_saved(Path::new("/private/tmp/not-a-codex-output.png")).is_err());
    }
    #[test]
    fn cancellation_and_expired_deadline_fail_before_spawn() {
        let cancel = AtomicBool::new(true);
        let future = Instant::now() + Duration::from_secs(1);
        assert!(edit(Path::new("/does-not-exist"), &[], "", &cancel, future).unwrap_err().contains("cancelled"));
        assert!(check(&AtomicBool::new(false), Instant::now()).unwrap_err().contains("timed out"));
    }
    #[test]
    fn endpoint_overrides_fail_closed() {
        assert!(verify_builtin_route(&json!({"config":{"model_provider":"openai"}})).is_ok());
        assert!(verify_builtin_route(&json!({"config":{"model_provider":"openai","chatgpt_base_url":"https://chatgpt.com/backend-api/","mcp_servers":{"example":{"enabled":false}}}})).is_ok());
        assert!(verify_builtin_route(&json!({"config":{"model_provider":"openai","mcp_servers":{"example":{"enabled":true}}}})).is_err());
        assert!(!safe_mcp_name("a.b"));
        assert!(!safe_mcp_name("\"example\""));
        assert!(safe_mcp_name("codex_app"));
        assert!(verify_builtin_route(&json!({"config":{"model_provider":"openai","chatgpt_base_url":"https://elsewhere.invalid"}})).is_err());
        assert!(
            verify_builtin_route(
                &json!({"config":{"model_provider":"openai","model_providers":{"openai":{"base_url":"https://elsewhere.invalid"}}}})
            )
            .is_err()
        );
        assert!(verify_builtin_route(&json!({"config":{"model_provider":"custom"}})).is_err());
    }
    #[test]
    fn edit_input_identity_is_required() {
        let expected = vec![(PathBuf::from("/x.png"), "abc".to_owned())];
        assert!(verify_inputs(&json!({"inputImages":[{"path":"/x.png","sha256":"abc"}]}), &expected).is_ok());
        assert!(verify_inputs(&json!({"inputImages":[{"path":"/x.png","sha256":"other"}]}), &expected).is_err());
        assert!(verify_inputs(&json!({}), &expected).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn full_protocol_edit_confirms_all_referenced_images() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("lightcraft-codex-positive-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let executable = root.join("codex-fake");
        let original = root.join("original.png");
        let guide = root.join("guide.png");
        fs::write(&original, b"original fixture").unwrap();
        fs::write(&guide, b"guide fixture").unwrap();
        let source = r#"#!/usr/bin/python3
import base64,hashlib,json,struct,sys,zlib
if sys.argv[1]=='mcp':
 print('[]');sys.exit(0)
def send(value):
 print(json.dumps(value),flush=True)
def chunk(kind,data):
 return struct.pack('>I',len(data))+kind+data+struct.pack('>I',zlib.crc32(kind+data)&0xffffffff)
png=b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',1,1,8,2,0,0,0))+chunk(b'IDAT',zlib.compress(b'\0\xff\xff\xff'))+chunk(b'IEND',b'')
for line in sys.stdin:
 m=json.loads(line);method=m['method'];params=m.get('params',{})
 if method=='initialized':continue
 if method=='initialize':result={'userAgent':'codex-fixture/1'}
 elif method=='account/read':result={'account':{'type':'chatgpt'}}
 elif method=='modelProvider/capabilities/read':result={'imageGeneration':True}
 elif method=='config/read':result={'config':{'model_provider':'openai'}}
 elif method=='mcpServerStatus/list':result={'data':[],'nextCursor':None}
 elif method=='thread/start':
  assert 'baseInstructions' not in params
  assert 'referenced_image_paths' in params['developerInstructions']
  result={'thread':{'id':'fixture-thread'}}
 elif method=='turn/start':
  assert params['threadId']=='fixture-thread'
  paths=[i['path'] for i in params['input'] if i['type']=='localImage']
  text=params['input'][0]['text']
  assert len(paths)==2 and all(p in text for p in paths)
  assert 'referenced_image_paths' in text and 'num_last_images_to_include' in text
  send({'id':m['id'],'result':{'turn':{'id':'fixture-turn'}}})
  send({'method':'item/completed','params':{'threadId':'fixture-thread','turnId':'fixture-turn','item':{'id':'fixture-image','type':'imageGeneration','status':'completed','result':base64.b64encode(png).decode(),'inputImages':[{'path':p,'sha256':hashlib.sha256(open(p,'rb').read()).hexdigest()} for p in paths]}}})
  send({'method':'turn/completed','params':{'threadId':'fixture-thread','turn':{'id':'fixture-turn','status':'completed','error':None}}})
  continue
 else:raise RuntimeError('unexpected method')
 send({'id':m['id'],'result':result})
"#;
        fs::write(&executable, source).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let generated = edit(
            &executable,
            &[original, guide],
            "Remove selected object and shadow.",
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
        let decoded = lightcraft_codecs::decode(&generated.png, lightcraft_codecs::DecodeOptions { max_size: None, max_pixels: 1 }).unwrap();
        assert_eq!((decoded.image.width, decoded.image.height), (1, 1));
        assert!(generated.provenance.contains("codex-fixture/1"));
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn fake_server_wrong_auth_and_timeout_reap_without_real_account() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::AtomicU64;
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!("lightcraft-codex-fake-{}-{}", std::process::id(), SEQUENCE.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap();
        let executable = root.join("codex-fake");
        let input = root.join("input.png");
        fs::write(&input, b"fixture").unwrap();
        let source = r#"#!/usr/bin/python3
import json,sys,time
if sys.argv[1] == 'mcp':
 print('[]'); sys.exit(0)
for line in sys.stdin:
 m=json.loads(line)
 if m.get('method')=='initialize':
  print(json.dumps({'id':m['id'],'result':{}}), flush=True)
 elif m.get('method')=='account/read':
  print(json.dumps({'id':m['id'],'result':{'account':{'type':'apiKey'}}}), flush=True)
"#;
        fs::write(&executable, source).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let cancel = AtomicBool::new(false);
        let error = edit(&executable, std::slice::from_ref(&input), "edit", &cancel, Instant::now() + Duration::from_secs(5)).unwrap_err();
        assert!(error.contains("subscription"), "{error}");
        let pid_path = root.join("pid");
        let stalled = format!(
            "#!/usr/bin/python3\nimport os,sys,time\nif sys.argv[1]=='mcp':\n print('[]'); sys.exit(0)\nopen({:?},'w').write(str(os.getpid()))\ntime.sleep(30)\n",
            pid_path.to_str().unwrap()
        );
        fs::write(&executable, stalled).unwrap();
        let started = Instant::now();
        let error = edit(&executable, &[input], "edit", &cancel, started + Duration::from_millis(500)).unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));
        let pid = fs::read_to_string(&pid_path).unwrap();
        assert!(!Command::new("/bin/kill").args(["-0", &pid]).stderr(Stdio::null()).status().unwrap().success());
        fs::remove_file(&pid_path).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&cancel);
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            signal.store(true, Ordering::Release);
        });
        let error = edit(&executable, &[root.join("input.png")], "edit", &cancel, Instant::now() + Duration::from_secs(5)).unwrap_err();
        canceller.join().unwrap();
        assert!(error.contains("cancelled"), "{error}");
        if let Ok(pid) = fs::read_to_string(&pid_path) {
            assert!(!Command::new("/bin/kill").args(["-0", &pid]).stderr(Stdio::null()).status().unwrap().success());
        }
        let store = root.join("generated_images");
        fs::create_dir(&store).unwrap();
        let valid = store.join("output.png");
        fs::write(&valid, b"saved bytes").unwrap();
        assert_eq!(read_saved_from_root(&valid, &store).unwrap(), b"saved bytes");
        let link = store.join("link.png");
        std::os::unix::fs::symlink(root.join("input.png"), &link).unwrap();
        assert!(read_saved_from_root(&link, &store).is_err());
        assert!(read_saved_from_root(&store.join("../input.png"), &store).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
