//! External, revisioned AI assets. This store never changes photo settings.
use crate::{EngineError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

fn err(e: impl ToString) -> EngineError {
    EngineError::Other(e.to_string())
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
    pub id: String,
    pub runtime: PathBuf,
    pub model: PathBuf,
    pub worker: PathBuf,
    pub backend: String,
    pub manifest: Value,
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
    let backend = m["backend"].as_str().unwrap_or_default();
    let name = match backend {
        "lama" => "Carve/LaMa-ONNX",
        "rorem" => "LetsThink/RORem",
        "objectclear" => "jixin0101/ObjectClear",
        _ => return Err(err("Unsupported AI model family.")),
    };
    if m["name"] != name {
        return Err(err("Unsupported AI model family."));
    }
    let required: &[&str] = match backend {
        "lama" => &["lama_fp32.onnx"],
        "rorem" => &[
            "model_index.json",
            "unet/config.json",
            "vae/config.json",
            "scheduler/scheduler_config.json",
            "text_encoder/config.json",
            "tokenizer/tokenizer_config.json",
        ],
        _ => &[
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
    };
    for path in required {
        if !files.contains(path) {
            return Err(err(format!("Model adapter asset missing: {path}")));
        }
    }
    if backend != "lama" {
        for component in if backend == "objectclear" {
            vec!["unet/", "vae/", "text_encoder/", "text_encoder_2/", "image_prompt_encoder/", "postfuse_module/"]
        } else {
            vec!["unet/", "vae/", "text_encoder/"]
        } {
            if !files.iter().any(|p| p.starts_with(component) && p.ends_with(".safetensors")) {
                return Err(err(format!("Model safe tensor weights missing: {component}")));
            }
        }
    }
    let pins = objectclear_pins()?;
    if backend == "objectclear" {
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
            let pinned = backend == "objectclear" && rel.strip_prefix("code/").is_some_and(|n| pins.get(n).is_some());
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
fn manifest(dir: &Path, kind: &str, hashes: bool) -> Result<Value> {
    let m = read_json(&inside(dir, "package.json")?)?;
    if m["version"] != 1 || m["kind"] != kind || m["adapter_version"] != 1 {
        return Err(err("Unsupported AI package or worker adapter."));
    }
    id(m["id"].as_str().ok_or_else(|| err("Missing AI package ID."))?)?;
    if kind == "runtime" && m["architecture"] != format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS) {
        return Err(err("AI runtime architecture is incompatible with this computer."));
    }
    if kind == "model" && !matches!(m["backend"].as_str(), Some("lama" | "rorem" | "objectclear")) {
        return Err(err("Unsupported AI model architecture."));
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
    if kind == "runtime" && hashes {
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
    let r = registry(root)?;
    let model = selected(root, model_id, backend)?;
    let runtime = r.runtimes.iter().find(|rt| rt.id == model.runtime_id).ok_or_else(|| err("AI runtime is missing."))?;
    let model_dir = inside(root, &model.path)?;
    let runtime_dir = inside(root, &runtime.path)?;
    let m = manifest(&model_dir, "model", hashes)?;
    let rt = manifest(&runtime_dir, "runtime", hashes)?;
    if m["id"] != model.id
        || m["backend"] != model.backend
        || m["model_version"] != model.model_version
        || rt["id"] != runtime.id
        || !m["runtime_compatibility"].as_array().is_some_and(|a| a.iter().any(|v| v == &runtime.id))
    {
        return Err(err("AI model/runtime registry mismatch."));
    }
    inside(&runtime_dir, "bin/python3")?;
    let worker = std::env::current_exe()
        .map_err(err)?
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| err("Cannot locate the AI worker."))?
        .join("Resources/ai-retouch-worker.py");
    Ok(Resolved { id: model.id.clone(), runtime: runtime_dir, model: model_dir, worker, backend: model.backend.clone(), manifest: m })
}
fn write_registry(root: &Path, r: &Registry) -> Result<()> {
    let path = root.join("registry.json.pending");
    std::fs::write(&path, serde_json::to_vec_pretty(r).map_err(err)?).map_err(err)?;
    std::fs::rename(path, root.join("registry.json")).map_err(err)
}
fn snapshot(root: &Path) -> Result<Value> {
    let r = registry(root)?;
    let mut v = serde_json::to_value(&r).map_err(err)?;
    v["root"] = json!(root);
    for m in v["models"].as_array_mut().into_iter().flatten() {
        m["status"] = json!(match resolve(root, m["id"].as_str(), None, false) {
            Ok(_) => "Installed · structural checks passed".to_string(),
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
                manifest(&path, "runtime", false)?;
                inside(&path, "bin/python3")?;
            }
            let c = crate::config::config_dir().ok_or_else(|| err("Settings folder unavailable."))?;
            std::fs::create_dir_all(&c).map_err(err)?;
            std::fs::write(c.join("ai-models.json"), serde_json::to_vec(&json!({"root":new})).map_err(err)?).map_err(err)?;
            return snapshot(&new);
        }
        "setDefault" => {
            let mut r = registry(root)?;
            let chosen = p["id"].as_str().ok_or_else(|| err("Choose a model."))?;
            resolve(root, Some(chosen), None, false)?;
            r.defaults.selected_model_id = Some(chosen.into());
            write_registry(root, &r)?
        }
        "verify" => {
            resolve(root, p["id"].as_str(), None, true)?;
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
            if raw["kind"] != "model" {
                return Err(err("Only model packages can be imported here. Install or repair the runtime with the reviewed LightCraft installer."));
            }
            let m = manifest(&source, "model", true)?;
            let package_id = m["id"].as_str().ok_or_else(|| err("Missing package ID."))?;
            let mut r = registry(root)?;
            if r.models.iter().any(|v| v.id == package_id) {
                return Err(err("This immutable model revision is already registered."));
            }
            let runtime_id = m["runtime_compatibility"]
                .as_array()
                .and_then(|a| a.iter().filter_map(Value::as_str).find(|s| r.runtimes.iter().any(|rt| rt.id == *s)))
                .ok_or_else(|| err("Install a compatible runtime with the LightCraft installer first."))?
                .to_owned();
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
    snapshot(root)
}
pub(crate) fn specs() -> Vec<crate::cmd::CommandSpec> {
    use crate::cmd::{always, cmd};
    vec![
        cmd!(query "aiModels.status","AI Model Status",[],None,"{}",always,|s,_|{s.ai_models.poll();if s.ai_models.snapshot.is_null()&&s.ai_models.message.is_empty()&&!s.ai_models.busy(){s.ai_models.start("status".into(),json!({}))?;}Ok(json!({"running":s.ai_models.busy(),"message":s.ai_models.message,"store":s.ai_models.snapshot}))}),
        cmd!(query "aiModels.refresh","Refresh AI Models",[],None,"{}",always,|s,p|s.ai_models.start("status".into(),p.clone())),
        cmd!(query "aiModels.setRoot","Choose AI Model Folder",[],None,"{path}",always,|s,p|{if s.ai_remove.busy(){return Err(err("Wait for AI Remove to finish."))}s.ai_models.start("setRoot".into(),p.clone())}),
        cmd!(query "aiModels.setDefault","Select Default AI Model",[],None,"{id}",always,|s,p|s.ai_models.start("setDefault".into(),p.clone())),
        cmd!(query "aiModels.import","Import AI Package",[],None,"{path}",always,|s,p|{if s.ai_remove.busy(){return Err(err("Wait for AI Remove to finish."))}s.ai_models.start("import".into(),p.clone())}),
        cmd!(query "aiModels.verify","Verify AI Model",[],None,"{id?}",always,|s,p|s.ai_models.start("verify".into(),p.clone())),
        cmd!(query "aiModels.remove","Remove AI Model",[],None,"{id}",always,|s,p|{if s.ai_remove.busy(){return Err(err("Wait for AI Remove to finish; its model is in use."))}s.ai_models.start("remove".into(),p.clone())}),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
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
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
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
