//! External, revisioned AI assets. This store never changes photo settings.
use crate::{EngineError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

fn err(e: impl ToString) -> EngineError {
    EngineError::Other(e.to_string())
}
/// Retouch model families supported by the trusted worker. Package strings remain
/// unchanged on disk; parsing them does not enable arbitrary model code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelFamily {
    Lama,
    Rorem,
    ObjectClear,
}
impl ModelFamily {
    pub fn from_backend(backend: &str) -> Option<Self> {
        match backend {
            "lama" => Some(Self::Lama),
            "rorem" => Some(Self::Rorem),
            "objectclear" => Some(Self::ObjectClear),
            _ => None,
        }
    }
    pub fn backend(self) -> &'static str {
        match self {
            Self::Lama => "lama",
            Self::Rorem => "rorem",
            Self::ObjectClear => "objectclear",
        }
    }
    pub fn adapter(self) -> AdapterDescriptor {
        let (display_name, required_files, weight_components): (&'static str, &'static [&'static str], &'static [&'static str]) = match self {
            Self::Lama => ("Carve/LaMa-ONNX", &["lama_fp32.onnx"], &[]),
            Self::Rorem => (
                "LetsThink/RORem",
                &[
                    "model_index.json",
                    "unet/config.json",
                    "vae/config.json",
                    "scheduler/scheduler_config.json",
                    "text_encoder/config.json",
                    "tokenizer/tokenizer_config.json",
                ],
                &["unet/", "vae/", "text_encoder/"],
            ),
            Self::ObjectClear => (
                "jixin0101/ObjectClear",
                &[
                    "model_index.json",
                    "unet/config.json",
                    "vae/config.json",
                    "scheduler/scheduler_config.json",
                    "text_encoder/config.json",
                    "text_encoder_2/config.json",
                    "tokenizer/tokenizer_config.json",
                    "tokenizer_2/tokenizer_config.json",
                    "image_prompt_encoder/config.json",
                    "postfuse_module/config.json",
                ],
                &["unet/", "vae/", "text_encoder/", "text_encoder_2/", "image_prompt_encoder/", "postfuse_module/"],
            ),
        };
        AdapterDescriptor {
            family: self,
            display_name,
            adapter_version: 1,
            execution: ExecutionKind::PythonProtocolV1,
            required_files,
            weight_components,
        }
    }
}
/// Explicit execution selected from a validated external runtime package.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionKind {
    PythonProtocolV1,
    NativeProtocolV1,
}
impl ExecutionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PythonProtocolV1 => "python-protocol-v1",
            Self::NativeProtocolV1 => "native-protocol-v1",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdapterDescriptor {
    pub family: ModelFamily,
    pub display_name: &'static str,
    pub adapter_version: u32,
    pub execution: ExecutionKind,
    required_files: &'static [&'static str],
    weight_components: &'static [&'static str],
}
impl AdapterDescriptor {
    fn from_manifest(m: &Value) -> Result<Self> {
        let family = ModelFamily::from_backend(m["backend"].as_str().unwrap_or_default()).ok_or_else(|| err("Unsupported AI model family."))?;
        let adapter = family.adapter();
        if m["adapter_version"] != adapter.adapter_version {
            return Err(err("Unsupported AI package or worker adapter."));
        }
        if m["name"] != adapter.display_name {
            return Err(err("Unsupported AI model family."));
        }
        Ok(adapter)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub backend: String,
    pub name: String,
    pub model_version: String,
    pub path: String,
    pub runtime_id: String,
    pub bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Runtime {
    pub id: String,
    pub path: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Defaults {
    pub selected_model_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub runtime_overrides: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Registry {
    pub version: u32,
    pub models: Vec<Model>,
    pub runtimes: Vec<Runtime>,
    pub defaults: Defaults,
}
impl Default for Registry {
    fn default() -> Self {
        Self { version: 1, models: vec![], runtimes: vec![], defaults: Defaults::default() }
    }
}
#[derive(Clone)]
pub struct Resolved {
    pub runtime_id: String,
    pub execution: ExecutionKind,
    pub id: String,
    pub runtime: PathBuf,
    pub model: PathBuf,
    pub worker: PathBuf,
    pub backend: String,
    pub manifest: Value,
}
impl Resolved {
    /// Recheck executable identity immediately before launch, without rehashing weights.
    pub fn verify_execution(&self) -> Result<()> {
        if self.execution == ExecutionKind::PythonProtocolV1 {
            return Ok(());
        }
        self.verify_execution_with_trust(&native_trust()?)
    }
    fn verify_execution_with_trust(&self, trust: &NativeTrust) -> Result<()> {
        let runtime = manifest_with_trust(&self.runtime, "runtime", false, trust)?;
        let (kind, entry) = runtime_entry(&self.runtime, &runtime, trust)?;
        if runtime["id"] != self.runtime_id || kind != self.execution || entry != self.worker {
            return Err(err("Native AI runtime entrypoint changed."));
        }
        native_binding(trust, &runtime, &self.runtime, &self.id, &self.model)
    }
    pub fn adapter(&self) -> Result<AdapterDescriptor> {
        let mut adapter = AdapterDescriptor::from_manifest(&self.manifest)?;
        adapter.execution = self.execution;
        if adapter.family.backend() != self.backend {
            return Err(err("AI model/runtime registry mismatch."));
        }
        Ok(adapter)
    }
}
#[derive(Default)]
pub struct Store {
    pub snapshot: Value,
    pub message: String,
    job: Option<std::sync::mpsc::Receiver<std::result::Result<Value, String>>>,
}
impl Store {
    pub fn poll(&mut self) {
        if let Some(result) = self.job.as_ref().and_then(|r| match r.try_recv() {
            Ok(v) => Some(v),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err("AI model task stopped.".into())),
            Err(_) => None,
        }) {
            self.job = None;
            match result {
                Ok(v) => {
                    self.snapshot = v;
                    self.message = "Done.".into()
                }
                Err(e) => self.message = e,
            }
        }
    }
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }
    fn start(&mut self, op: String, params: Value) -> Result<Value> {
        self.poll();
        if self.busy() {
            return Err(err("AI model management is busy."));
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("ai-model-store".into())
            .spawn(move || {
                let result = crate::guard::catch("AI model management", || operate(&op, &params).map_err(|e| e.to_string())).unwrap_or_else(Err);
                let _ = tx.send(result);
            })
            .map_err(err)?;
        self.job = Some(rx);
        self.message = "Working…".into();
        Ok(json!({"running":true}))
    }
}
pub fn root() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("LIGHTCRAFT_AI_HOME") {
        if p.is_empty() {
            return Err(err("AI model folder is disabled."));
        }
        return Ok(p.into());
    }
    if let Some(c) = crate::config::config_dir() {
        let p = c.join("ai-models.json");
        if p.is_file() {
            let v = read_json(&p)?;
            return v
                .get("root")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .ok_or_else(|| err("Invalid saved AI model folder."));
        }
    }
    Ok(PathBuf::from("/Volumes/Media/LightCraft/AI"))
}
fn read_json(p: &Path) -> Result<Value> {
    if std::fs::metadata(p).map_err(err)?.len() > 16 * 1024 * 1024 {
        return Err(err("AI manifest is too large."));
    }
    serde_json::from_slice(&std::fs::read(p).map_err(err)?).map_err(err)
}
fn registry(root: &Path) -> Result<Registry> {
    if !root.is_dir() {
        return Err(err(format!("AI model folder is unavailable: {}. Choose a mounted folder in Manage models.", root.display())));
    }
    let r: Registry = serde_json::from_value(read_json(&root.join("registry.json"))?).map_err(err)?;
    if r.version != 1 {
        return Err(err("Unsupported AI registry version."));
    }
    Ok(r)
}
fn relative(p: &str) -> Result<()> {
    if p.is_empty() || p.contains('\\') || Path::new(p).components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(err("Unsafe AI package path."));
    }
    Ok(())
}
fn inside(base: &Path, p: &str) -> Result<PathBuf> {
    relative(p)?;
    let base = base.canonicalize().map_err(err)?;
    let path = base.join(p).canonicalize().map_err(err)?;
    if !path.starts_with(&base) {
        return Err(err("AI package symlink escapes its folder."));
    }
    Ok(path)
}
fn id(p: &str) -> Result<()> {
    if p.is_empty() || !p.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)) || p == "." || p == ".." {
        Err(err("Invalid AI package ID."))
    } else {
        Ok(())
    }
}
fn objectclear_pins() -> Result<Value> {
    serde_json::from_str(r#"{"LICENSE":"73c84d2c2ebcdde03cb7fc853aaec0d4b3b280ee8b47f98a3d6c1931430d5aae","objectclear/__init__.py":"1993604ebfb1097873491ead796e5ad3fd6711387452145f04a79aae7012e39b","objectclear/models/__init__.py":"c0a02b976d31b4aba72ee728fa9445d915ce59374f5740c595ac41c3df3b1b95","objectclear/models/clip_image_encoder.py":"8deb2e9634168a8f96723a8f7487df5eaea229d36a4bdaaceed8878b7b1c596f","objectclear/models/postfuse_module.py":"3384daa88be0d463b3117d5f2065387f02e6b7589f3036ecfb746ff1a39cb768","objectclear/pipelines/__init__.py":"5666324f2e22aebd7954a33d4ff14f3b6d9004ccbec18756045f9e92d441d02d","objectclear/pipelines/pipeline_objectclear.py":"f0919c0dc8e2b5e5d9768b907e9d78e6182722a20b2626716311c43bbe0474bc","objectclear/utils/__init__.py":"0bd93dc8b73246d7516feabfd3c9ea22721991732a9a6ead89e4db54192348dd","objectclear/utils/attention_guided_fusion.py":"66f92ed57791e7e453b93f07205f8d4dcadf1b8d776898d3d824456233cbfdb1","objectclear/utils/image_utils.py":"a374fa4b0c7b7a1fb663b67fc0f2563900b7c38cf0f59b568bbb298a20881e71"}"#).map_err(err)
}
fn model_adapter(dir: &Path, m: &Value, files: &std::collections::HashSet<&str>) -> Result<()> {
    let adapter = AdapterDescriptor::from_manifest(m)?;
    for path in adapter.required_files {
        if !files.contains(path) {
            return Err(err(format!("Model adapter asset missing: {path}")));
        }
    }
    for component in adapter.weight_components {
        if !files.iter().any(|p| p.starts_with(*component) && p.ends_with(".safetensors")) {
            return Err(err(format!("Model safe tensor weights missing: {component}")));
        }
    }
    let pins = objectclear_pins()?;
    if adapter.family == ModelFamily::ObjectClear {
        if m["code_pins"] != pins {
            return Err(err("ObjectClear code pins do not match the trusted worker."));
        }
        for (name, hash) in pins.as_object().into_iter().flatten() {
            let path = format!("code/{name}");
            if !files.contains(path.as_str()) || !m["files"].as_array().into_iter().flatten().any(|f| f["path"] == path && f["sha256"] == *hash) {
                return Err(err("ObjectClear source is missing or untrusted."));
            }
        }
    }
    // Every on-disk file must be listed; symlink directories are rejected to bound traversal.
    let mut queue = vec![dir.to_path_buf()];
    let mut count = 0usize;
    while let Some(folder) = queue.pop() {
        for entry in std::fs::read_dir(folder).map_err(err)? {
            let entry = entry.map_err(err)?;
            count = count.saturating_add(1);
            if count > 100_000 {
                return Err(err("AI package contains too many entries."));
            }
            let path = entry.path();
            let rel = path.strip_prefix(dir).map_err(err)?.to_str().ok_or_else(|| err("Non-UTF8 model path."))?.replace('\\', "/");
            let meta = std::fs::symlink_metadata(&path).map_err(err)?;
            if meta.file_type().is_symlink() {
                return Err(err("Model packages must contain ordinary files, without symlinks."));
            }
            if meta.is_dir() {
                queue.push(path);
                continue;
            }
            if !meta.is_file() {
                return Err(err("Unsupported model package file."));
            }
            if rel == "package.json" {
                continue;
            }
            if !files.contains(rel.as_str()) {
                return Err(err("Model package contains unlisted files."));
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
            let executable = matches!(ext.as_str(), "py" | "pyc" | "so" | "dylib" | "dll" | "pyd" | "exe" | "sh" | "bash" | "zsh");
            let pinned = adapter.family == ModelFamily::ObjectClear && rel.strip_prefix("code/").is_some_and(|n| pins.get(n).is_some());
            if executable && !pinned {
                return Err(err("Model package contains untrusted executable code."));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o111 != 0 && !pinned {
                    return Err(err("Model package contains an executable file."));
                }
            }
        }
    }
    Ok(())
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeTrust {
    version: u32,
    runtimes: Vec<NativePin>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativePin {
    id: String,
    binary_sha256: String,
    model_id: String,
    model_manifest_sha256: String,
    /// Native shared libraries are executable code and require application-owned pins.
    #[serde(default)]
    dependency_sha256: BTreeMap<String, String>,
}
fn native_trust() -> Result<NativeTrust> {
    let trust: NativeTrust = serde_json::from_str(include_str!("ai_models_native_trust.json")).map_err(err)?;
    if trust.version != 1 {
        return Err(err("Unsupported native runtime trust version."));
    }
    Ok(trust)
}
fn sha256(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).map_err(err)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(err)?;
        if n == 0 {
            break;
        }
        hash.update(buffer.get(..n).ok_or_else(|| err("Invalid hash read size."))?);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn execution(manifest: &Value) -> Result<ExecutionKind> {
    match manifest.get("execution") {
        None => Ok(ExecutionKind::PythonProtocolV1),
        Some(Value::String(value)) if value == "python-protocol-v1" => Ok(ExecutionKind::PythonProtocolV1),
        Some(Value::String(value)) if value == "native-protocol-v1" => Ok(ExecutionKind::NativeProtocolV1),
        _ => Err(err("Unsupported AI runtime execution kind.")),
    }
}
fn runtime_entry(dir: &Path, manifest: &Value, trust: &NativeTrust) -> Result<(ExecutionKind, PathBuf)> {
    let kind = execution(manifest)?;
    let entry = inside(
        dir,
        match kind {
            ExecutionKind::PythonProtocolV1 => "bin/python3",
            ExecutionKind::NativeProtocolV1 => "bin/lightcraft-ai-native",
        },
    )?;
    if !entry.is_file() {
        return Err(err("AI runtime entrypoint is not a file."));
    }
    if kind == ExecutionKind::NativeProtocolV1 {
        let runtime_id = manifest["id"].as_str().ok_or_else(|| err("Missing runtime ID."))?;
        let binary_hash = sha256(&entry)?;
        let pin = trust
            .runtimes
            .iter()
            .find(|pin| pin.id == runtime_id && pin.binary_sha256 == binary_hash)
            .filter(|_| trust.version == 1)
            .ok_or_else(|| err("Native AI runtime binary is not approved by this application."))?;
        for (relative, expected) in &pin.dependency_sha256 {
            let dependency = inside(dir, relative)?;
            if !std::fs::symlink_metadata(dir.join(relative)).map_err(err)?.is_file() || sha256(&dependency)? != *expected {
                return Err(err("Native AI runtime dependency is not approved by this application."));
            }
        }
        if !manifest["files"]
            .as_array()
            .is_some_and(|files| files.iter().any(|file| file["path"] == "bin/lightcraft-ai-native" && file["sha256"] == binary_hash))
        {
            return Err(err("Native entrypoint is missing from its package manifest."));
        }
        let meta = std::fs::symlink_metadata(dir.join("bin/lightcraft-ai-native")).map_err(err)?;
        if !meta.is_file() {
            return Err(err("Native runtime entrypoint must be an ordinary file."));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o111 == 0 {
                return Err(err("Native runtime entrypoint is not executable."));
            }
        }
    }
    Ok((kind, entry))
}
fn native_binding(trust: &NativeTrust, runtime: &Value, runtime_dir: &Path, model_id: &str, model_dir: &Path) -> Result<()> {
    let binary = sha256(&inside(runtime_dir, "bin/lightcraft-ai-native")?)?;
    let model_manifest = sha256(&inside(model_dir, "package.json")?)?;
    if !trust
        .runtimes
        .iter()
        .any(|pin| runtime["id"] == pin.id && pin.binary_sha256 == binary && pin.model_id == model_id && pin.model_manifest_sha256 == model_manifest)
    {
        return Err(err("Native AI runtime is not approved for this exact model manifest."));
    }
    Ok(())
}
fn manifest(dir: &Path, kind: &str, hashes: bool) -> Result<Value> {
    manifest_with_trust(dir, kind, hashes, &native_trust()?)
}
fn manifest_with_trust(dir: &Path, kind: &str, hashes: bool, trust: &NativeTrust) -> Result<Value> {
    let m = read_json(&inside(dir, "package.json")?)?;
    if m["version"] != 1 || m["kind"] != kind || (kind != "model" && m["adapter_version"] != 1) {
        return Err(err("Unsupported AI package or worker adapter."));
    }
    if kind == "model" {
        AdapterDescriptor::from_manifest(&m)?;
    }
    id(m["id"].as_str().ok_or_else(|| err("Missing AI package ID."))?)?;
    if kind == "runtime" && m["architecture"] != format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS) {
        return Err(err("AI runtime architecture is incompatible with this computer."));
    }
    for field in ["name", "model_version", "source"] {
        if m[field].as_str().is_none_or(|v| v.is_empty()) {
            return Err(err("Missing AI package metadata."));
        }
    }
    let files = m["files"].as_array().filter(|v| !v.is_empty()).ok_or_else(|| err("AI package has no files."))?;
    let mut seen = std::collections::HashSet::new();
    for f in files {
        let name = f["path"].as_str().ok_or_else(|| err("Missing package path."))?;
        if name == "package.json" || !seen.insert(name) {
            return Err(err("Duplicate or reserved AI package file path."));
        }
        let p = inside(dir, f["path"].as_str().ok_or_else(|| err("Missing package file path."))?)?;
        if !p.is_file() || Some(std::fs::metadata(&p).map_err(err)?.len()) != f["bytes"].as_u64() {
            return Err(err("AI package file size mismatch."));
        }
        if hashes {
            let mut file = std::fs::File::open(p).map_err(err)?;
            let mut h = Sha256::new();
            let mut b = [0; 65536];
            loop {
                let n = file.read(&mut b).map_err(err)?;
                if n == 0 {
                    break;
                }
                h.update(b.get(..n).ok_or_else(|| err("Invalid file read size."))?)
            }
            if Some(format!("{:x}", h.finalize()).as_str()) != f["sha256"].as_str() {
                return Err(err("AI package checksum mismatch."));
            }
        }
    }
    if kind == "model" {
        model_adapter(dir, &m, &seen)?;
    }
    if kind == "runtime" {
        runtime_entry(dir, &m, trust)?;
    }
    if kind == "runtime" && (hashes || execution(&m)? == ExecutionKind::NativeProtocolV1) {
        let mut queue = vec![dir.to_path_buf()];
        let mut count = 0usize;
        while let Some(folder) = queue.pop() {
            for entry in std::fs::read_dir(folder).map_err(err)? {
                let entry = entry.map_err(err)?;
                count = count.saturating_add(1);
                if count > 100_000 {
                    return Err(err("AI runtime contains too many entries."));
                }
                let path = entry.path();
                let meta = std::fs::symlink_metadata(&path).map_err(err)?;
                if meta.is_dir() {
                    queue.push(path);
                    continue;
                }
                if !path.is_file() {
                    return Err(err("Unsupported runtime directory symlink or special file."));
                }
                let rel = path.strip_prefix(dir).map_err(err)?.to_str().ok_or_else(|| err("Non-UTF8 runtime path."))?.replace('\\', "/");
                if rel != "package.json" && !seen.contains(rel.as_str()) {
                    return Err(err("AI runtime contains unlisted files."));
                }
            }
        }
    }
    Ok(m)
}
pub fn selected(root: &Path, model_id: Option<&str>, backend: Option<&str>) -> Result<Model> {
    let r = registry(root)?;

    let selected = model_id.or(r.defaults.selected_model_id.as_deref());
    let model = if let Some(requested) = model_id {
        r.models.iter().find(|m| m.id == requested && backend.is_none_or(|b| m.backend == b))
    } else if let Some(b) = backend {
        r.models.iter().find(|m| Some(m.id.as_str()) == selected && m.backend == b).or_else(|| r.models.iter().find(|m| m.backend == b))
    } else {
        r.models.iter().find(|m| Some(m.id.as_str()) == selected)
    }
    .ok_or_else(|| err("No installed AI model selected. Open Manage models."))?;
    Ok(model.clone())
}
pub fn resolve(root: &Path, model_id: Option<&str>, backend: Option<&str>, hashes: bool) -> Result<Resolved> {
    resolve_with_trust(root, model_id, backend, hashes, &native_trust()?)
}
fn resolve_with_trust(root: &Path, model_id: Option<&str>, backend: Option<&str>, hashes: bool, trust: &NativeTrust) -> Result<Resolved> {
    let registry = registry(root)?;
    let model = selected(root, model_id, backend)?;
    let runtime_id = registry.defaults.runtime_overrides.get(&model.id).unwrap_or(&model.runtime_id);
    resolve_runtime(root, &registry, &model, runtime_id, hashes, trust)
}
fn resolve_runtime(root: &Path, registry: &Registry, model: &Model, runtime_id: &str, hashes: bool, trust: &NativeTrust) -> Result<Resolved> {
    let runtime = registry
        .runtimes
        .iter()
        .find(|rt| rt.id == runtime_id)
        .ok_or_else(|| err("Selected AI runtime is missing. Clear or replace its runtime override."))?;
    let model_dir = inside(root, &model.path)?;
    let runtime_dir = inside(root, &runtime.path)?;
    let m = manifest_with_trust(&model_dir, "model", hashes, trust)?;
    let rt = manifest_with_trust(&runtime_dir, "runtime", hashes, trust)?;
    if m["id"] != model.id || m["backend"] != model.backend || m["model_version"] != model.model_version || rt["id"] != runtime.id {
        return Err(err("AI model/runtime registry mismatch."));
    }
    let (execution, entry) = runtime_entry(&runtime_dir, &rt, trust)?;
    match execution {
        ExecutionKind::PythonProtocolV1 => {
            if !m["runtime_compatibility"].as_array().is_some_and(|ids| ids.iter().any(|id| id == runtime_id)) {
                return Err(err("AI model/runtime registry mismatch."));
            }
        }
        ExecutionKind::NativeProtocolV1 => native_binding(trust, &rt, &runtime_dir, &model.id, &model_dir)?,
    }
    let worker = if execution == ExecutionKind::NativeProtocolV1 {
        entry
    } else {
        std::env::current_exe()
            .map_err(err)?
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| err("Cannot locate the AI worker."))?
            .join("Resources/ai-retouch-worker.py")
    };
    Ok(Resolved {
        runtime_id: runtime.id.clone(),
        execution,
        id: model.id.clone(),
        runtime: runtime_dir,
        model: model_dir,
        worker,
        backend: model.backend.clone(),
        manifest: m,
    })
}
fn write_registry(root: &Path, r: &Registry) -> Result<()> {
    let path = root.join("registry.json.pending");
    std::fs::write(&path, serde_json::to_vec_pretty(r).map_err(err)?).map_err(err)?;
    std::fs::rename(path, root.join("registry.json")).map_err(err)
}
/// Display-only provenance from a structurally validated model manifest. File
/// names identify included notices; they do not establish usage permissions.
fn display_metadata(manifest: &Value) -> Value {
    let mut metadata = json!({"source":manifest["source"].as_str().unwrap_or_default()});
    if let Some(revision) = manifest["source_revision"].as_str().filter(|s| !s.is_empty()) {
        metadata["source_revision"] = json!(revision);
    }
    let notices: Vec<&str> = manifest["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| f["path"].as_str())
        .filter(|path| {
            relative(path).is_ok()
                && Path::new(path).file_name().and_then(|name| name.to_str()).is_some_and(|name| {
                    let name = name.to_ascii_lowercase();
                    ["license", "licence", "copying", "notice"].iter().any(|part| name.contains(part))
                })
        })
        .collect();
    metadata["licence_files"] = json!(notices);
    metadata
}
#[cfg(test)]
fn snapshot(root: &Path) -> Result<Value> {
    snapshot_with_trust(root, &native_trust()?)
}
fn snapshot_with_trust(root: &Path, trust: &NativeTrust) -> Result<Value> {
    let r = registry(root)?;
    let mut v = serde_json::to_value(&r).map_err(err)?;
    v["root"] = json!(root);
    for m in v["models"].as_array_mut().into_iter().flatten() {
        let model = r.models.iter().find(|model| Some(model.id.as_str()) == m["id"].as_str()).ok_or_else(|| err("Unknown snapshot model."))?;
        let runtime_id = r.defaults.runtime_overrides.get(&model.id).unwrap_or(&model.runtime_id);
        m["selected_runtime_id"] = json!(runtime_id);
        let mut choices = Vec::new();
        for runtime in &r.runtimes {
            if let Ok(resolved) = resolve_runtime(root, &r, model, &runtime.id, false, trust) {
                let metadata = read_json(&resolved.runtime.join("package.json"))?;
                let display = display_metadata(&metadata);
                choices.push(json!({"id":runtime.id,"name":metadata["name"],"execution":resolved.execution.as_str(),"source":display["source"],"licence_files":display["licence_files"]}));
            }
        }
        m["runtime_choices"] = json!(choices);
        m["status"] = json!(match resolve_with_trust(root, m["id"].as_str(), None, false, trust) {
            Ok(resolved) => {
                m["execution"] = json!(resolved.execution.as_str());
                for (key, value) in display_metadata(&resolved.manifest).as_object().into_iter().flatten() {
                    m[key.as_str()] = value.clone();
                }
                "Installed · structural checks passed".to_string()
            }
            Err(e) => e.to_string(),
        });
    }
    Ok(v)
}
fn operate(op: &str, p: &Value) -> Result<Value> {
    operate_at(&root()?, op, p)
}
pub(crate) struct StoreLock(std::fs::File);
impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
pub(crate) fn store_lock(root: &Path) -> Result<StoreLock> {
    let lock = std::fs::OpenOptions::new().create(true).append(true).open(root.join(".store.lock")).map_err(err)?;
    lock.lock().map_err(err)?;
    Ok(StoreLock(lock))
}
fn operate_at(root: &Path, op: &str, p: &Value) -> Result<Value> {
    operate_with_trust(root, op, p, &native_trust()?)
}
fn operate_with_trust(root: &Path, op: &str, p: &Value, trust: &NativeTrust) -> Result<Value> {
    let _lock = if op != "setRoot" { Some(store_lock(root)?) } else { None };
    match op {
        "setRoot" => {
            if std::env::var_os("LIGHTCRAFT_AI_HOME").is_some() {
                return Err(err("The AI folder is overridden by LIGHTCRAFT_AI_HOME. Change or unset that override to select another store."));
            }
            let new = PathBuf::from(p["path"].as_str().ok_or_else(|| err("Choose an AI model folder."))?);
            if !new.is_absolute() || !new.is_dir() {
                return Err(err("Choose an existing absolute AI model folder."));
            }
            let _new_lock = store_lock(&new)?;
            let selected = registry(&new)?;
            if selected.runtimes.is_empty() {
                return Err(err("Choose an existing AI store with an installed runtime. The folder is selected in place; assets are not moved."));
            }
            for runtime in &selected.runtimes {
                let path = inside(&new, &runtime.path)?;
                let metadata = manifest_with_trust(&path, "runtime", false, trust)?;
                runtime_entry(&path, &metadata, trust)?;
            }
            let c = crate::config::config_dir().ok_or_else(|| err("Settings folder unavailable."))?;
            std::fs::create_dir_all(&c).map_err(err)?;
            std::fs::write(c.join("ai-models.json"), serde_json::to_vec(&json!({"root":new})).map_err(err)?).map_err(err)?;
            return snapshot_with_trust(&new, trust);
        }
        "setRuntime" => {
            let mut registry = registry(root)?;
            let model_id = p["modelId"].as_str().ok_or_else(|| err("Choose a model."))?;
            let model = registry.models.iter().find(|model| model.id == model_id).ok_or_else(|| err("Unknown model."))?;
            match p.get("runtimeId") {
                Some(Value::Null) => {
                    registry.defaults.runtime_overrides.remove(model_id);
                }
                Some(Value::String(runtime_id)) => {
                    resolve_runtime(root, &registry, model, runtime_id, false, trust)?;
                    registry.defaults.runtime_overrides.insert(model_id.to_owned(), runtime_id.clone());
                }
                _ => return Err(err("Choose a runtime ID or null to clear its override.")),
            }
            write_registry(root, &registry)?;
        }
        "removeRuntime" => {
            remove_runtime(root, p)?;
        }
        "setDefault" => {
            let mut r = registry(root)?;
            let chosen = p["id"].as_str().ok_or_else(|| err("Choose a model."))?;
            resolve_with_trust(root, Some(chosen), None, false, trust)?;
            r.defaults.selected_model_id = Some(chosen.into());
            write_registry(root, &r)?
        }
        "verify" => {
            resolve_with_trust(root, p["id"].as_str(), None, true, trust)?;
        }
        "remove" => {
            let mut r = registry(root)?;
            let chosen = p["id"].as_str().ok_or_else(|| err("Choose a model."))?;
            let model = r.models.iter().find(|m| m.id == chosen).cloned().ok_or_else(|| err("Unknown model."))?;
            let path = inside(root, &model.path)?;
            let models_dir = root.canonicalize().map_err(err)?.join("models");
            if path == models_dir || !path.starts_with(&models_dir) || manifest(&path, "model", false)?["id"] != chosen {
                return Err(err("Model is outside its managed package folder or has a mismatched ID."));
            }
            if r.models
                .iter()
                .any(|other| other.id != chosen && inside(root, &other.path).is_ok_and(|p| p == path || p.starts_with(&path) || path.starts_with(&p)))
            {
                return Err(err("Model package path is shared by another registry entry."));
            }
            if r.runtimes.iter().any(|other| inside(root, &other.path).is_ok_and(|p| p == path || p.starts_with(&path) || path.starts_with(&p))) {
                return Err(err("Model package path overlaps a runtime."));
            }
            r.models.retain(|m| m.id != chosen);
            r.defaults.runtime_overrides.remove(chosen);
            if r.defaults.selected_model_id.as_deref() == Some(chosen) {
                r.defaults.selected_model_id = r.models.first().map(|m| m.id.clone())
            }
            let trash = models_dir.join(format!(".removing-{}-{}", chosen, std::process::id()));
            if trash.exists() {
                return Err(err("An earlier model removal needs cleanup."));
            }
            std::fs::rename(&path, &trash).map_err(err)?;
            if let Err(e) = write_registry(root, &r) {
                let _ = std::fs::rename(&trash, &path);
                return Err(e);
            }
            std::fs::remove_dir_all(trash).map_err(err)?;
        }
        "import" => {
            let source = PathBuf::from(p["path"].as_str().ok_or_else(|| err("Choose an unpacked model package."))?);
            let raw = read_json(&source.join("package.json"))?;
            if raw["kind"] == "runtime" {
                import_native_runtime(root, &source, trust)?;
                return snapshot_with_trust(root, trust);
            }
            if raw["kind"] != "model" {
                return Err(err("Only model packages can be imported here. Install or repair the runtime with the reviewed LightCraft installer."));
            }
            let m = manifest(&source, "model", true)?;
            let package_id = m["id"].as_str().ok_or_else(|| err("Missing package ID."))?;
            let mut r = registry(root)?;
            if r.models.iter().any(|v| v.id == package_id) {
                return Err(err("This immutable model revision is already registered."));
            }
            let runtime_id = import_runtime(root, &r, package_id, &source, &m, trust)?;
            let bytes = m["files"]
                .as_array()
                .into_iter()
                .flatten()
                .try_fold(0u64, |n, f| n.checked_add(f["bytes"].as_u64().unwrap_or(0)))
                .ok_or_else(|| err("Package size overflow."))?;
            let parent = root.join("models");
            std::fs::create_dir_all(&parent).map_err(err)?;
            if !parent.canonicalize().map_err(err)?.starts_with(root.canonicalize().map_err(err)?) {
                return Err(err("Model destination escapes the AI folder."));
            }
            let relative = format!("models/{package_id}");
            let target = root.join(&relative);
            if target.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(err("Model package directory cannot be a symlink."));
            }
            if target.exists() {
                if manifest(&target, "model", true)? != m {
                    return Err(err("Conflicting immutable model revision."));
                }
            } else {
                static NEXT_IMPORT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let pending = parent.join(format!(
                    ".incoming-{package_id}-{}-{}",
                    std::process::id(),
                    NEXT_IMPORT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                ));
                if pending.exists() {
                    return Err(err("An interrupted import remains. Remove its .incoming folder before retrying."));
                }
                std::fs::create_dir(&pending).map_err(err)?;
                let copied = (|| -> Result<()> {
                    for f in m["files"].as_array().into_iter().flatten() {
                        let path = f["path"].as_str().ok_or_else(|| err("Missing file path."))?;
                        let from = inside(&source, path)?;
                        let to = pending.join(path);
                        if let Some(parent) = to.parent() {
                            std::fs::create_dir_all(parent).map_err(err)?;
                        }
                        std::fs::copy(from, to).map_err(err)?;
                    }
                    std::fs::copy(source.join("package.json"), pending.join("package.json")).map_err(err)?;
                    manifest(&pending, "model", true)?;
                    std::fs::rename(&pending, &target).map_err(err)?;
                    Ok(())
                })();
                if let Err(e) = copied {
                    let _ = std::fs::remove_dir_all(pending);
                    return Err(e);
                }
            }
            r.models.push(Model {
                id: package_id.into(),
                backend: m["backend"].as_str().unwrap_or_default().into(),
                name: m["name"].as_str().unwrap_or(package_id).into(),
                model_version: m["model_version"].as_str().unwrap_or_default().into(),
                path: relative,
                runtime_id,
                bytes,
            });
            if r.defaults.selected_model_id.is_none() {
                r.defaults.selected_model_id = Some(package_id.into())
            }
            write_registry(root, &r)?;
        }
        "status" => {}
        _ => return Err(err("Unknown AI model operation.")),
    }
    snapshot_with_trust(root, trust)
}
fn remove_runtime(root: &Path, params: &Value) -> Result<()> {
    let mut registry = registry(root)?;
    let chosen = params["id"].as_str().ok_or_else(|| err("Choose a runtime."))?;
    if registry.models.iter().any(|model| model.runtime_id == chosen) || registry.defaults.runtime_overrides.values().any(|value| value == chosen) {
        return Err(err("AI runtime is referenced by a model or runtime override."));
    }
    let runtime = registry.runtimes.iter().find(|runtime| runtime.id == chosen).ok_or_else(|| err("Unknown runtime."))?;
    let path = inside(root, &runtime.path)?;
    let parent = root.canonicalize().map_err(err)?.join("runtimes");
    if path == parent || !path.starts_with(&parent) || read_json(&inside(&path, "package.json")?)?["id"] != chosen {
        return Err(err("Runtime is outside its managed package folder or has a mismatched ID."));
    }
    if registry
        .models
        .iter()
        .map(|m| &m.path)
        .chain(registry.runtimes.iter().filter(|rt| rt.id != chosen).map(|rt| &rt.path))
        .any(|other| inside(root, other).is_ok_and(|other| other == path || other.starts_with(&path) || path.starts_with(&other)))
    {
        return Err(err("Runtime package path overlaps another registry entry."));
    }
    registry.runtimes.retain(|runtime| runtime.id != chosen);
    let trash = parent.join(format!(".removing-{chosen}-{}", std::process::id()));
    if trash.exists() {
        return Err(err("An earlier runtime removal needs cleanup."));
    }
    std::fs::rename(&path, &trash).map_err(err)?;
    if let Err(error) = write_registry(root, &registry) {
        let _ = std::fs::rename(&trash, &path);
        return Err(error);
    }
    std::fs::remove_dir_all(trash).map_err(err)
}
/// Prefer an installed compatible Python runtime; native-only installations bind
/// the source model's exact immutable manifest before any model data is copied.
fn import_runtime(root: &Path, registry: &Registry, model_id: &str, source: &Path, model: &Value, trust: &NativeTrust) -> Result<String> {
    for compatible in model["runtime_compatibility"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        let Some(runtime) = registry.runtimes.iter().find(|runtime| runtime.id == compatible) else {
            continue;
        };
        let Ok(dir) = inside(root, &runtime.path) else {
            continue;
        };
        let Ok(metadata) = manifest_with_trust(&dir, "runtime", false, trust) else {
            continue;
        };
        if metadata["id"] == runtime.id && execution(&metadata)? == ExecutionKind::PythonProtocolV1 {
            return Ok(runtime.id.clone());
        }
    }
    for runtime in &registry.runtimes {
        let Ok(dir) = inside(root, &runtime.path) else {
            continue;
        };
        let Ok(metadata) = manifest_with_trust(&dir, "runtime", false, trust) else {
            continue;
        };
        if metadata["id"] == runtime.id
            && execution(&metadata)? == ExecutionKind::NativeProtocolV1
            && native_binding(trust, &metadata, &dir, model_id, source).is_ok()
        {
            return Ok(runtime.id.clone());
        }
    }
    Err(err("Install a compatible Python runtime or an approved native runtime for this exact model first."))
}
fn import_native_runtime(root: &Path, source: &Path, trust: &NativeTrust) -> Result<()> {
    let metadata = manifest_with_trust(source, "runtime", true, trust)?;
    if execution(&metadata)? != ExecutionKind::NativeProtocolV1 {
        return Err(err("Python runtimes must be installed by the reviewed LightCraft installer."));
    }
    let package_id = metadata["id"].as_str().ok_or_else(|| err("Missing runtime ID."))?;
    let mut registry = registry(root)?;
    if registry.runtimes.iter().any(|runtime| runtime.id == package_id) {
        return Err(err("This immutable runtime revision is already registered."));
    }
    let parent = root.join("runtimes");
    std::fs::create_dir_all(&parent).map_err(err)?;
    if !parent.canonicalize().map_err(err)?.starts_with(root.canonicalize().map_err(err)?) {
        return Err(err("Runtime destination escapes the AI folder."));
    }
    let relative = format!("runtimes/{package_id}");
    let target = root.join(&relative);
    if target.symlink_metadata().is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(err("Runtime package directory cannot be a symlink."));
    }
    if target.exists() {
        if manifest_with_trust(&target, "runtime", true, trust)? != metadata {
            return Err(err("Conflicting immutable runtime revision."));
        }
    } else {
        let pending = parent.join(format!(".incoming-{package_id}-{}", std::process::id()));
        if pending.exists() {
            return Err(err("An interrupted runtime import needs cleanup."));
        }
        std::fs::create_dir(&pending).map_err(err)?;
        let copied = (|| -> Result<()> {
            for file in metadata["files"].as_array().into_iter().flatten() {
                let name = file["path"].as_str().ok_or_else(|| err("Missing runtime file path."))?;
                let from = inside(source, name)?;
                let to = pending.join(name);
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent).map_err(err)?;
                }
                std::fs::copy(from, to).map_err(err)?;
            }
            std::fs::copy(source.join("package.json"), pending.join("package.json")).map_err(err)?;
            manifest_with_trust(&pending, "runtime", true, trust)?;
            std::fs::rename(&pending, &target).map_err(err)?;
            Ok(())
        })();
        if let Err(error) = copied {
            let _ = std::fs::remove_dir_all(&pending);
            return Err(error);
        }
    }
    registry.runtimes.push(Runtime { id: package_id.into(), path: relative });
    write_registry(root, &registry)
}
pub(crate) fn specs() -> Vec<crate::cmd::CommandSpec> {
    use crate::cmd::{always, cmd};
    vec![
        cmd!(query "aiModels.status","AI Model Status",[],None,"{}",always,|s,_|{s.ai_models.poll();if s.ai_models.snapshot.is_null()&&s.ai_models.message.is_empty()&&!s.ai_models.busy(){s.ai_models.start("status".into(),json!({}))?;}Ok(json!({"running":s.ai_models.busy(),"message":s.ai_models.message,"store":s.ai_models.snapshot}))}),
        cmd!(query "aiModels.refresh","Refresh AI Models",[],None,"{}",always,|s,p|s.ai_models.start("status".into(),p.clone())),
        cmd!(query "aiModels.setRoot","Choose AI Model Folder",[],None,"{path}",always,|s,p|{if s.ai_remove.busy(){return Err(err("Wait for AI Remove to finish."))}s.ai_models.start("setRoot".into(),p.clone())}),
        cmd!(query "aiModels.setRuntime","Select AI Runtime",[],None,"{modelId,runtimeId}",always,|s,p|{if s.ai_remove.busy(){return Err(err("Wait for AI Remove to finish."))}s.ai_models.start("setRuntime".into(),p.clone())}),
        cmd!(query "aiModels.removeRuntime","Remove AI Runtime",[],None,"{id}",always,|s,p|{if s.ai_remove.busy(){return Err(err("Wait for AI Remove to finish."))}s.ai_models.start("removeRuntime".into(),p.clone())}),
        cmd!(query "aiModels.setDefault","Select Default AI Model",[],None,"{id}",always,|s,p|s.ai_models.start("setDefault".into(),p.clone())),
        cmd!(query "aiModels.import","Import AI Package",[],None,"{path}",always,|s,p|{if s.ai_remove.busy(){return Err(err("Wait for AI Remove to finish."))}s.ai_models.start("import".into(),p.clone())}),
        cmd!(query "aiModels.verify","Verify AI Model",[],None,"{id?}",always,|s,p|s.ai_models.start("verify".into(),p.clone())),
        cmd!(query "aiModels.remove","Remove AI Model",[],None,"{id}",always,|s,p|{if s.ai_remove.busy(){return Err(err("Wait for AI Remove to finish; its model is in use."))}s.ai_models.start("remove".into(),p.clone())}),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    fn native_fixture() -> (Fixture, PathBuf, PathBuf, NativeTrust) {
        let fixture = Fixture::new();
        let root = fixture.root();
        let legacy = fixture.package("runtime", "runtime-1", "");
        std::fs::create_dir(root.join("runtimes")).unwrap();
        std::fs::rename(&legacy, root.join("runtimes/runtime-1")).unwrap();
        let mut registry = registry(&root).unwrap();
        registry.runtimes.push(Runtime { id: "runtime-1".into(), path: "runtimes/runtime-1".into() });
        write_registry(&root, &registry).unwrap();
        let model = fixture.package("model", "lama-revision-1", "lama");
        operate_at(&root, "import", &json!({"path":model})).unwrap();
        let native = fixture.package("runtime", "native-1", "");
        std::fs::rename(native.join("bin/python3"), native.join("bin/lightcraft-ai-native")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(native.join("bin/lightcraft-ai-native"), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut metadata = read_json(&native.join("package.json")).unwrap();
        metadata["execution"] = json!("native-protocol-v1");
        metadata["files"][0]["path"] = json!("bin/lightcraft-ai-native");
        std::fs::write(native.join("package.json"), serde_json::to_vec(&metadata).unwrap()).unwrap();
        let trust = NativeTrust {
            version: 1,
            runtimes: vec![NativePin {
                id: "native-1".into(),
                binary_sha256: sha256(&native.join("bin/lightcraft-ai-native")).unwrap(),
                dependency_sha256: BTreeMap::new(),
                model_id: "lama-revision-1".into(),
                model_manifest_sha256: sha256(&root.join("models/lama-revision-1/package.json")).unwrap(),
            }],
        };
        (fixture, root, native, trust)
    }
    #[test]
    fn native_dependency_is_rechecked_even_when_package_manifest_is_rewritten() {
        let (_fixture, _root, native, mut trust) = native_fixture();
        std::fs::create_dir(native.join("lib")).unwrap();
        let dependency = native.join("lib/inference.dylib");
        std::fs::write(&dependency, b"approved native library").unwrap();
        trust.runtimes[0].dependency_sha256.insert("lib/inference.dylib".into(), sha256(&dependency).unwrap());
        let mut metadata = read_json(&native.join("package.json")).unwrap();
        let entry = json!({"path":"lib/inference.dylib", "bytes":23, "sha256":sha256(&dependency).unwrap()});
        metadata["files"].as_array_mut().unwrap().push(entry);
        runtime_entry(&native, &metadata, &trust).unwrap();
        std::fs::write(&dependency, b"altered native library").unwrap();
        metadata["files"][1]["sha256"] = json!(sha256(&dependency).unwrap());
        assert!(runtime_entry(&native, &metadata, &trust).is_err());
        std::fs::remove_file(&dependency).unwrap();
        assert!(runtime_entry(&native, &metadata, &trust).is_err());
    }
    #[test]
    fn fresh_native_only_store_bootstraps_without_python_and_rejects_other_models() {
        let (fixture, _existing, native, trust) = native_fixture();
        let root = fixture.0.join("native-only");
        std::fs::create_dir(&root).unwrap();
        write_registry(&root, &Registry::default()).unwrap();
        let model = fixture.0.join("lama-revision-1");
        assert!(operate_with_trust(&root, "import", &json!({"path":model}), &trust).is_err());
        operate_with_trust(&root, "import", &json!({"path":native}), &trust).unwrap();
        assert!(registry(&root).unwrap().models.is_empty());
        assert!(!root.join("runtimes/runtime-1").exists());
        let wrong = fixture.package("model", "wrong-model", "lama");
        assert!(operate_with_trust(&root, "import", &json!({"path":wrong}), &trust).is_err());
        assert!(!root.join("models/wrong-model").exists());
        operate_with_trust(&root, "import", &json!({"path":model}), &trust).unwrap();
        let resolved = resolve_with_trust(&root, None, None, true, &trust).unwrap();
        assert_eq!(resolved.execution, ExecutionKind::NativeProtocolV1);
        assert_eq!(resolved.runtime_id, "native-1");
        assert!(resolved.worker.ends_with("bin/lightcraft-ai-native"));
        resolved.verify_execution_with_trust(&trust).unwrap();
        let registry = registry(&root).unwrap();
        assert_eq!(registry.models[0].runtime_id, "native-1");
        assert!(registry.defaults.runtime_overrides.is_empty());
        assert_eq!(registry.runtimes.len(), 1);
    }
    #[test]
    fn model_import_keeps_legacy_preference_when_both_runtime_kinds_are_installed() {
        let (fixture, root, native, trust) = native_fixture();
        operate_with_trust(&root, "import", &json!({"path":native}), &trust).unwrap();
        operate_with_trust(&root, "remove", &json!({"id":"lama-revision-1"}), &trust).unwrap();
        operate_with_trust(&root, "import", &json!({"path":fixture.0.join("lama-revision-1")}), &trust).unwrap();
        assert_eq!(registry(&root).unwrap().models[0].runtime_id, "runtime-1");
        assert_eq!(resolve_with_trust(&root, None, None, false, &trust).unwrap().execution, ExecutionKind::PythonProtocolV1);
    }
    #[test]
    fn legacy_registry_roundtrip_keeps_empty_runtime_overrides_absent() {
        let old = json!({"version":1,"models":[],"runtimes":[],"defaults":{"selected_model_id":null}});
        let registry: Registry = serde_json::from_value(old.clone()).unwrap();
        assert!(registry.defaults.runtime_overrides.is_empty());
        assert_eq!(serde_json::to_value(registry).unwrap(), old);
        assert_eq!(native_trust().unwrap().version, 1);
    }
    #[test]
    fn reviewed_native_import_selection_clear_and_prelaunch_identity_are_explicit() {
        let (_fixture, root, native, trust) = native_fixture();
        let original = registry(&root).unwrap();
        let model_hash = sha256(&root.join("models/lama-revision-1/package.json")).unwrap();
        assert!(operate_at(&root, "import", &json!({"path":native})).is_err());
        operate_with_trust(&root, "import", &json!({"path":native}), &trust).unwrap();
        assert_eq!(resolve_with_trust(&root, None, None, false, &trust).unwrap().execution, ExecutionKind::PythonProtocolV1);
        operate_with_trust(&root, "setRuntime", &json!({"modelId":"lama-revision-1","runtimeId":"native-1"}), &trust).unwrap();
        let selected = resolve_with_trust(&root, None, None, false, &trust).unwrap();
        assert_eq!(selected.runtime_id, "native-1");
        assert_eq!(selected.execution, ExecutionKind::NativeProtocolV1);
        assert_eq!(selected.adapter().unwrap().execution, ExecutionKind::NativeProtocolV1);
        assert!(selected.worker.ends_with("bin/lightcraft-ai-native"));
        selected.verify_execution_with_trust(&trust).unwrap();
        assert!(selected.verify_execution().is_err()); // Synthetic pins are never added to the production trust list.
        let snapshot = snapshot_with_trust(&root, &trust).unwrap();
        assert_eq!(snapshot["models"][0]["selected_runtime_id"], "native-1");
        assert_eq!(snapshot["models"][0]["execution"], "native-protocol-v1");
        assert_eq!(snapshot["models"][0]["runtime_choices"].as_array().unwrap().len(), 2);
        let saved = registry(&root).unwrap();
        assert_eq!(saved.models[0].runtime_id, original.models[0].runtime_id);
        assert_eq!(saved.defaults.selected_model_id, original.defaults.selected_model_id);
        assert_eq!(sha256(&root.join("models/lama-revision-1/package.json")).unwrap(), model_hash);
        operate_with_trust(&root, "setRuntime", &json!({"modelId":"lama-revision-1","runtimeId":null}), &trust).unwrap();
        assert!(registry(&root).unwrap().defaults.runtime_overrides.is_empty());
        assert_eq!(resolve_with_trust(&root, None, None, false, &trust).unwrap().execution, ExecutionKind::PythonProtocolV1);
        let model_path = selected.model.join("package.json");
        let original_model = std::fs::read(&model_path).unwrap();
        let mut changed = read_json(&model_path).unwrap();
        changed["source"] = json!("changed");
        std::fs::write(&model_path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(selected.verify_execution_with_trust(&trust).is_err());
        std::fs::write(&model_path, original_model).unwrap();
        std::fs::write(&selected.worker, b"evil").unwrap();
        assert!(selected.verify_execution_with_trust(&trust).is_err());
    }
    #[test]
    fn native_rejects_unknown_pin_wrong_model_binary_kind_and_architecture() {
        let (_fixture, root, native, trust) = native_fixture();
        assert!(manifest(&native, "runtime", false).is_err());
        let mut bad = trust.clone();
        bad.runtimes[0].model_id = "other-model".into();
        assert!(
            native_binding(&bad, &read_json(&native.join("package.json")).unwrap(), &native, "lama-revision-1", &root.join("models/lama-revision-1"))
                .is_err()
        );
        bad = trust.clone();
        bad.runtimes[0].model_manifest_sha256 = "0".repeat(64);
        assert!(
            native_binding(&bad, &read_json(&native.join("package.json")).unwrap(), &native, "lama-revision-1", &root.join("models/lama-revision-1"))
                .is_err()
        );
        bad = trust.clone();
        bad.runtimes[0].binary_sha256 = "0".repeat(64);
        assert!(manifest_with_trust(&native, "runtime", false, &bad).is_err());
        let path = native.join("package.json");
        let original = read_json(&path).unwrap();
        for (field, value) in
            [("architecture", json!("wrong-os")), ("execution", json!("unknown-v1")), ("execution", Value::Null), ("adapter_version", json!(2))]
        {
            let mut invalid = original.clone();
            invalid[field] = value;
            std::fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
            assert!(manifest_with_trust(&native, "runtime", false, &trust).is_err());
        }
        std::fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
        std::fs::write(native.join("bin/lightcraft-ai-native"), b"evil").unwrap();
        assert!(manifest_with_trust(&native, "runtime", false, &trust).is_err());
    }
    #[test]
    fn stale_override_is_visible_and_failed_selection_does_not_change_defaults() {
        let (_fixture, root, native, trust) = native_fixture();
        operate_with_trust(&root, "import", &json!({"path":native}), &trust).unwrap();
        let before = std::fs::read(root.join("registry.json")).unwrap();
        assert!(operate_with_trust(&root, "setRuntime", &json!({"modelId":"lama-revision-1","runtimeId":"missing"}), &trust).is_err());
        assert_eq!(before, std::fs::read(root.join("registry.json")).unwrap());
        let mut registry = registry(&root).unwrap();
        registry.defaults.runtime_overrides.insert("lama-revision-1".into(), "missing".into());
        write_registry(&root, &registry).unwrap();
        assert!(resolve_with_trust(&root, None, None, false, &trust).is_err());
        let snapshot = snapshot_with_trust(&root, &trust).unwrap();
        assert_eq!(snapshot["models"][0]["selected_runtime_id"], "missing");
        assert!(snapshot["models"][0]["status"].as_str().unwrap().contains("runtime is missing"));
        assert!(snapshot["models"][0].get("execution").is_none());
        operate_with_trust(&root, "setRuntime", &json!({"modelId":"lama-revision-1","runtimeId":null}), &trust).unwrap();
        assert_eq!(resolve_with_trust(&root, None, None, false, &trust).unwrap().execution, ExecutionKind::PythonProtocolV1);
    }
    #[test]
    fn incompatible_legacy_override_is_rejected_and_model_removal_clears_override() {
        let (fixture, root, native, trust) = native_fixture();
        operate_with_trust(&root, "import", &json!({"path":native}), &trust).unwrap();
        let other = fixture.package("runtime", "runtime-2", "");
        std::fs::rename(other, root.join("runtimes/runtime-2")).unwrap();
        let mut saved = registry(&root).unwrap();
        saved.runtimes.push(Runtime { id: "runtime-2".into(), path: "runtimes/runtime-2".into() });
        write_registry(&root, &saved).unwrap();
        assert!(operate_with_trust(&root, "setRuntime", &json!({"modelId":"lama-revision-1","runtimeId":"runtime-2"}), &trust).is_err());
        assert!(registry(&root).unwrap().defaults.runtime_overrides.is_empty());
        operate_with_trust(&root, "setRuntime", &json!({"modelId":"lama-revision-1","runtimeId":"native-1"}), &trust).unwrap();
        operate_with_trust(&root, "remove", &json!({"id":"lama-revision-1"}), &trust).unwrap();
        assert!(registry(&root).unwrap().defaults.runtime_overrides.is_empty());
        operate_with_trust(&root, "removeRuntime", &json!({"id":"native-1"}), &trust).unwrap();
    }
    #[test]
    fn runtime_removal_protects_immutable_model_references_and_overrides() {
        let (_fixture, root, native, trust) = native_fixture();
        operate_with_trust(&root, "import", &json!({"path":native}), &trust).unwrap();
        assert!(operate_with_trust(&root, "removeRuntime", &json!({"id":"runtime-1"}), &trust).is_err());
        operate_with_trust(&root, "setRuntime", &json!({"modelId":"lama-revision-1","runtimeId":"native-1"}), &trust).unwrap();
        assert!(operate_with_trust(&root, "removeRuntime", &json!({"id":"native-1"}), &trust).is_err());
        operate_with_trust(&root, "setRuntime", &json!({"modelId":"lama-revision-1","runtimeId":null}), &trust).unwrap();
        operate_with_trust(&root, "removeRuntime", &json!({"id":"native-1"}), &trust).unwrap();
        assert!(!root.join("runtimes/native-1").exists());
        assert!(root.join("runtimes/runtime-1").is_dir());
    }
    #[test]
    fn display_metadata_only_reports_source_and_listed_notice_filenames() {
        let manifest = json!({
            "source":"Example/Model@revision-1",
            "source_revision":"code-revision-2",
            "licence":"Do not infer permissions from this field",
            "runtime_id":"implementation-detail",
            "files":[
                {"path":"code/LICENSE"},
                {"path":"Model-LicenCe.txt"},
                {"path":"COPYING.md"},
                {"path":"docs/Notice.txt"},
                {"path":"licenses/weights.json"},
                {"path":"README.md"},
                {"path":"../LICENSE"},
                {"path":"/LICENSE"}
            ]
        });
        assert_eq!(
            display_metadata(&manifest),
            json!({"source":"Example/Model@revision-1","source_revision":"code-revision-2","licence_files":["code/LICENSE","Model-LicenCe.txt","COPYING.md","docs/Notice.txt"]})
        );
        let mut no_revision = manifest;
        no_revision["source_revision"] = Value::Null;
        let metadata = display_metadata(&no_revision);
        assert!(metadata.get("source_revision").is_none());
        assert!(metadata.get("licence").is_none());
        assert!(metadata.get("runtime_id").is_none());
        assert_eq!(display_metadata(&json!({"source":"source","files":[]})), json!({"source":"source","licence_files":[]}));
    }
    #[test]
    fn supported_descriptors_match_package_v1_identities() {
        for (backend, name, family, required_count, weight_count) in [
            ("lama", "Carve/LaMa-ONNX", ModelFamily::Lama, 1, 0),
            ("rorem", "LetsThink/RORem", ModelFamily::Rorem, 6, 3),
            ("objectclear", "jixin0101/ObjectClear", ModelFamily::ObjectClear, 10, 6),
        ] {
            let manifest = json!({"backend":backend,"name":name,"adapter_version":1});
            let adapter = AdapterDescriptor::from_manifest(&manifest).unwrap();
            assert_eq!(adapter.family, family);
            assert_eq!(adapter.family.backend(), backend);
            assert_eq!(adapter.display_name, name);
            assert_eq!(adapter.adapter_version, 1);
            assert_eq!(adapter.execution, ExecutionKind::PythonProtocolV1);
            assert_eq!(adapter.required_files.len(), required_count);
            assert_eq!(adapter.weight_components.len(), weight_count);
        }
    }
    #[test]
    fn unsupported_descriptor_family_version_and_identity_are_rejected() {
        for family in [ModelFamily::Lama, ModelFamily::Rorem, ModelFamily::ObjectClear] {
            let descriptor = family.adapter();
            let valid = json!({"backend":family.backend(),"name":descriptor.display_name,"adapter_version":1});
            for version in [Value::Null, json!(0), json!(2), json!("1")] {
                let mut invalid = valid.clone();
                invalid["adapter_version"] = version;
                assert!(AdapterDescriptor::from_manifest(&invalid).is_err());
            }
            let mut wrong_name = valid;
            wrong_name["name"] = json!("Unknown/Model");
            assert!(AdapterDescriptor::from_manifest(&wrong_name).is_err());
        }
        assert!(ModelFamily::from_backend("native-objectclear").is_none());
        assert!(AdapterDescriptor::from_manifest(&json!({"backend":"unknown","name":"unknown","adapter_version":1})).is_err());
    }
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let p = std::env::temp_dir().join(format!(
                "lc-model-store-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&p).unwrap();
            Self(p)
        }
        fn package(&self, kind: &str, id: &str, backend: &str) -> PathBuf {
            let p = self.0.join(id);
            std::fs::create_dir(&p).unwrap();
            let name = if kind == "runtime" { "bin/python3" } else { "lama_fp32.onnx" };
            if kind == "runtime" {
                std::fs::create_dir(p.join("bin")).unwrap()
            }
            std::fs::write(p.join(name), b"data").unwrap();
            let m = json!({"version":1,"kind":kind,"id":id,"backend":backend,"name":if kind=="model" {"Carve/LaMa-ONNX"}else{id},"model_version":"revision-1","source":"test","adapter_version":1,"architecture":format!("{}-{}",std::env::consts::ARCH,std::env::consts::OS),"runtime_compatibility":["runtime-1"],"files":[{"path":name,"bytes":4,"sha256":format!("{:x}",Sha256::digest(b"data"))}]});
            std::fs::write(p.join("package.json"), serde_json::to_vec(&m).unwrap()).unwrap();
            p
        }
        fn root(&self) -> PathBuf {
            let p = self.0.join("store");
            std::fs::create_dir(&p).unwrap();
            write_registry(&p, &Registry::default()).unwrap();
            p
        }
        // Structural model fixtures deliberately do not contain real weights or
        // executable source. Hash verification is covered by the import tests.
        fn structural_model(&self, family: ModelFamily) -> PathBuf {
            let adapter = family.adapter();
            let p = self.0.join(family.backend());
            std::fs::create_dir(&p).unwrap();
            let mut files = Vec::new();
            let mut add = |path: &str, hash: String| {
                let target = p.join(path);
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::write(target, b"data").unwrap();
                files.push(json!({"path":path,"bytes":4,"sha256":hash}));
            };
            for path in adapter.required_files {
                add(path, format!("{:x}", Sha256::digest(b"data")));
            }
            for component in adapter.weight_components {
                add(&format!("{component}model.safetensors"), format!("{:x}", Sha256::digest(b"data")));
            }
            let pins = objectclear_pins().unwrap();
            if family == ModelFamily::ObjectClear {
                for (name, hash) in pins.as_object().unwrap() {
                    add(&format!("code/{name}"), hash.as_str().unwrap().to_owned());
                }
            }
            let mut m = json!({"version":1,"kind":"model","id":family.backend(),"backend":family.backend(),"name":adapter.display_name,"model_version":"revision-1","source":"synthetic-test","adapter_version":1,"files":files});
            if family == ModelFamily::ObjectClear {
                m["code_pins"] = pins;
            }
            std::fs::write(p.join("package.json"), serde_json::to_vec(&m).unwrap()).unwrap();
            p
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn descriptors_enforce_each_existing_model_asset_schema() {
        let f = Fixture::new();
        for family in [ModelFamily::Lama, ModelFamily::Rorem, ModelFamily::ObjectClear] {
            let p = f.structural_model(family);
            let m = manifest(&p, "model", false).unwrap();
            let adapter = AdapterDescriptor::from_manifest(&m).unwrap();
            let files: std::collections::HashSet<&str> = m["files"].as_array().unwrap().iter().map(|v| v["path"].as_str().unwrap()).collect();
            for path in adapter.required_files {
                let mut incomplete = files.clone();
                incomplete.remove(path);
                assert!(model_adapter(&p, &m, &incomplete).unwrap_err().to_string().contains("Model adapter asset missing"));
            }
            for component in adapter.weight_components {
                let incomplete = files.iter().copied().filter(|path| !(path.starts_with(*component) && path.ends_with(".safetensors"))).collect();
                assert!(model_adapter(&p, &m, &incomplete).unwrap_err().to_string().contains("Model safe tensor weights missing"));
            }
        }
    }
    #[test]
    fn import_default_verify_remove_and_immutable_revisions() {
        let f = Fixture::new();
        let root = f.root();
        let rt = f.package("runtime", "runtime-1", "");
        let model = f.package("model", "lama-revision-1", "lama");
        assert!(operate_at(&root, "import", &json!({"path":model})).is_err());
        assert!(!root.join("models/lama-revision-1").exists());
        assert!(operate_at(&root, "import", &json!({"path":rt})).is_err());
        std::fs::create_dir(root.join("runtimes")).unwrap();
        std::fs::rename(&rt, root.join("runtimes/runtime-1")).unwrap();
        let mut r = registry(&root).unwrap();
        r.runtimes.push(Runtime { id: "runtime-1".into(), path: "runtimes/runtime-1".into() });
        write_registry(&root, &r).unwrap();
        operate_at(&root, "import", &json!({"path":model})).unwrap();
        assert_eq!(registry(&root).unwrap().defaults.selected_model_id.as_deref(), Some("lama-revision-1"));
        assert!(operate_at(&root, "import", &json!({"path":model})).is_err());
        let resolved = resolve(&root, None, None, true).unwrap();
        assert_eq!(resolved.model, root.join("models/lama-revision-1"));
        assert_eq!(resolved.runtime, root.join("runtimes/runtime-1"));
        assert_eq!(resolved.adapter().unwrap().family, ModelFamily::Lama);
        let display = snapshot(&root).unwrap();
        assert_eq!(display["models"][0]["source"], "test");
        assert_eq!(display["models"][0]["licence_files"], json!([]));
        assert!(read_json(&root.join("registry.json")).unwrap()["models"][0].get("source").is_none());
        let second = f.package("model", "lama-revision-2", "lama");
        let mut second_m = read_json(&second.join("package.json")).unwrap();
        second_m["model_version"] = json!("revision-2");
        std::fs::write(second.join("package.json"), serde_json::to_vec(&second_m).unwrap()).unwrap();
        operate_at(&root, "import", &json!({"path":second})).unwrap();
        operate_at(&root, "setDefault", &json!({"id":"lama-revision-2"})).unwrap();
        assert_eq!(resolve(&root, None, None, false).unwrap().id, "lama-revision-2");
        operate_at(&root, "setDefault", &json!({"id":"lama-revision-1"})).unwrap();
        operate_at(&root, "remove", &json!({"id":"lama-revision-2"})).unwrap();
        // Recover a complete publication interrupted before the registry write.
        let mut interrupted = registry(&root).unwrap();
        interrupted.models.clear();
        interrupted.defaults.selected_model_id = None;
        write_registry(&root, &interrupted).unwrap();
        operate_at(&root, "import", &json!({"path":model})).unwrap();
        let mut shared = registry(&root).unwrap();
        let mut duplicate = shared.models[0].clone();
        duplicate.id = "other-id".into();
        shared.models.push(duplicate);
        write_registry(&root, &shared).unwrap();
        assert!(operate_at(&root, "remove", &json!({"id":"lama-revision-1"})).is_err());
        shared.models.pop();
        write_registry(&root, &shared).unwrap();
        let correct = shared.models[0].path.clone();
        shared.models[0].path = "models".into();
        write_registry(&root, &shared).unwrap();
        assert!(operate_at(&root, "remove", &json!({"id":"lama-revision-1"})).is_err());
        assert!(root.join("models").is_dir());
        shared.models[0].path = correct;
        write_registry(&root, &shared).unwrap();
        operate_at(&root, "verify", &json!({})).unwrap();
        assert!(operate_at(&root, "setDefault", &json!({"id":"missing"})).is_err());
        operate_at(&root, "remove", &json!({"id":"lama-revision-1"})).unwrap();
        assert!(registry(&root).unwrap().models.is_empty());
        assert!(registry(&root).unwrap().defaults.selected_model_id.is_none());
        assert!(root.join("runtimes/runtime-1").exists());
    }
    #[test]
    fn malformed_package_and_corrupt_weights_are_rejected() {
        let f = Fixture::new();
        let p = f.package("model", "lama-1", "lama");
        let path = p.join("package.json");
        let mut m = read_json(&path).unwrap();
        m["files"][0]["path"] = json!("../outside");
        std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
        assert!(manifest(&p, "model", true).is_err());
        m["files"][0]["path"] = json!("lama_fp32.onnx");
        m["adapter_version"] = json!(2);
        std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
        assert!(manifest(&p, "model", false).is_err());
        m["adapter_version"] = json!(1);
        std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
        std::fs::write(p.join("lama_fp32.onnx"), b"evil").unwrap();
        assert!(manifest(&p, "model", true).is_err());
    }
    #[test]
    fn extra_code_and_wrong_family_are_rejected() {
        let f = Fixture::new();
        let p = f.package("model", "lama-revision-1", "lama");
        std::fs::write(p.join("unlisted.py"), b"print('untrusted')").unwrap();
        assert!(manifest(&p, "model", true).is_err());
        std::fs::remove_file(p.join("unlisted.py")).unwrap();
        let mut m = read_json(&p.join("package.json")).unwrap();
        m["name"] = json!("Unknown/LaMa");
        std::fs::write(p.join("package.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        assert!(manifest(&p, "model", true).is_err());
    }
    #[test]
    fn store_lock_excludes_other_handles() {
        let f = Fixture::new();
        let root = f.root();
        let held = store_lock(&root).unwrap();
        let contender = std::fs::OpenOptions::new().append(true).open(root.join(".store.lock")).unwrap();
        assert!(contender.try_lock().is_err());
        drop(held);
        assert!(contender.try_lock().is_ok());
    }
    #[test]
    #[cfg(unix)]
    fn escaping_symlink_is_rejected() {
        let f = Fixture::new();
        let p = f.package("model", "lama-1", "lama");
        std::fs::remove_file(p.join("lama_fp32.onnx")).unwrap();
        std::fs::write(f.0.join("outside"), b"data").unwrap();
        std::os::unix::fs::symlink("../outside", p.join("lama_fp32.onnx")).unwrap();
        assert!(manifest(&p, "model", true).is_err());
    }
    #[test]
    fn missing_assets_do_not_touch_photo_edits() {
        let f = Fixture::new();
        let mut s = crate::Session::with_demo();
        let before = serde_json::to_value(&*s.develop_of(s.active().unwrap()).unwrap()).unwrap();
        assert!(resolve(&f.0.join("missing"), None, None, false).is_err());
        assert_eq!(before, serde_json::to_value(&*s.develop_of(s.active().unwrap()).unwrap()).unwrap());
        s.ai_remove_poll();
    }
}
