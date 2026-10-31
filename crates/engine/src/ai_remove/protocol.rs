//! Protocol-v1 process boundary for separately verified Python/native runtimes.
use super::runner::{RetouchIdentity, RetouchOutput, RetouchProvenance, RetouchRequest, RetouchRunner};
use serde_json::{Value, json};
use std::path::Path;

pub(super) struct ProtocolRunner<'a> {
    pub runtime: &'a Path,
    pub backend: &'a str,
    pub package: Option<&'a crate::ai_models::Resolved>,
}
impl ProtocolRunner<'_> {
    fn execution(&self) -> crate::ai_models::ExecutionKind {
        self.package.map_or(crate::ai_models::ExecutionKind::PythonProtocolV1, |p| p.execution)
    }

    fn command(&self) -> std::result::Result<std::process::Command, String> {
        if let Some(package) = self.package {
            if package.runtime != self.runtime || package.backend != self.backend {
                return Err("AI request package identity changed.".into());
            }
            package.verify_execution().map_err(|e| e.to_string())?;
        }
        match self.execution() {
            crate::ai_models::ExecutionKind::PythonProtocolV1 => {
                let mut command = std::process::Command::new(self.runtime.join("bin/python3"));
                command.arg("-I").arg("-B").arg(self.package.map(|p| p.worker.clone()).unwrap_or_else(|| self.runtime.join("worker.py")));
                Ok(command)
            }
            crate::ai_models::ExecutionKind::NativeProtocolV1 => {
                let package = self.package.ok_or("Native AI execution needs a verified runtime package.")?;
                Ok(std::process::Command::new(&package.worker))
            }
        }
    }
}
impl RetouchRunner for ProtocolRunner<'_> {
    fn identity(&self) -> RetouchIdentity {
        RetouchIdentity {
            model: self.package.map(|p| p.id.clone()).unwrap_or_else(|| self.backend.into()),
            runtime: self.runtime.into(),
            adapter: match self.execution() {
                crate::ai_models::ExecutionKind::PythonProtocolV1 => "python-protocol-v1",
                crate::ai_models::ExecutionKind::NativeProtocolV1 => "native-protocol-v1",
            },
        }
    }
    fn run(&self, request: RetouchRequest<'_>, cancel: &std::sync::atomic::AtomicBool) -> std::result::Result<RetouchOutput, String> {
        if *request.identity != self.identity() {
            return Err("AI request package identity changed.".into());
        }
        let input = request.input;
        let (w, h, rect, seed, backend) = (input.width, input.height, input.rect, input.seed, self.backend);
        use std::{fs, sync::atomic::Ordering, time::Duration};
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!("lightcraft-ai-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&dir).map_err(|e| e.to_string())?;
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _scratch = Scratch(dir.clone());
        fs::write(dir.join("input.rgb"), encode_rgb(&input.rgb)).map_err(|e| e.to_string())?;
        fs::write(dir.join("mask.u8"), &input.mask).map_err(|e| e.to_string())?;
        let request_json = json!({"version":1,"input":dir.join("input.rgb"),"mask":dir.join("mask.u8"),"output":dir.join("output.rgba"),"width":w,"height":h,"rect":rect,"backend":backend,"model_dir":self.package.map(|p| p.model.clone()).unwrap_or_else(|| self.runtime.join(backend_directory(backend).unwrap_or(""))),"model_package":self.package.map(|p|json!({"path":p.model,"manifest":p.manifest})),"seed":seed,"steps":input.steps});
        fs::write(dir.join("request.json"), serde_json::to_vec(&request_json).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled.".into());
        }
        let mut command = self.command()?;
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled.".into());
        }
        let mut child = command
            .arg(dir.join("request.json"))
            .arg(dir.join("response.json"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("Cannot start AI Remove: {e}"))?;
        if let Err(failure) = wait_worker(&mut child, cancel, Duration::from_secs(600)) {
            if !cancel.load(Ordering::Relaxed) {
                let response = dir.join("response.json");
                if fs::metadata(&response).is_ok_and(|m| m.len() <= 65536)
                    && let Ok(bytes) = fs::read(response)
                    && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
                    && let Some(detail) = value.get("error").and_then(Value::as_str)
                {
                    return Err(format!("AI Remove failed: {}", detail.chars().take(1000).collect::<String>()));
                }
            }
            return Err(failure);
        }
        let response_path = dir.join("response.json");
        if fs::metadata(&response_path).map_err(|e| e.to_string())?.len() > 65536 {
            return Err("Worker response is too large.".into());
        }
        let response: Value = serde_json::from_slice(&fs::read(response_path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        if response.get("version").and_then(Value::as_u64) != Some(1)
            || response.get("width").and_then(Value::as_u64) != Some(w as u64)
            || response.get("height").and_then(Value::as_u64) != Some(h as u64)
        {
            return Err("AI worker returned mismatched dimensions.".into());
        }
        let output = dir.join("output.rgba");
        if self.execution() == crate::ai_models::ExecutionKind::NativeProtocolV1 {
            let package = self.package.ok_or("Native AI execution needs a verified runtime package.")?;
            let reported = response.get("pixels").and_then(Value::as_str).ok_or("Native AI worker omitted output provenance.")?;
            let reported = Path::new(reported).canonicalize().map_err(|e| e.to_string())?;
            let expected_output = output.canonicalize().map_err(|e| e.to_string())?;
            if response.get("model") != package.manifest.get("name")
                || response.get("model_version") != package.manifest.get("model_version")
                || response.get("seed").and_then(Value::as_u64) != Some(u64::from(seed))
                || response.get("rect") != Some(&json!(rect))
                || reported != expected_output
            {
                return Err("Native AI worker returned mismatched request provenance.".into());
            }
        }
        let expected = w.saturating_mul(h).saturating_mul(16);
        if fs::metadata(&output).map_err(|e| e.to_string())?.len() != expected as u64 {
            return Err("AI worker returned invalid pixel data.".into());
        }
        let raw = fs::read(output).map_err(|e| e.to_string())?;
        let rgb = decode_rgb(&raw)?;
        Ok(RetouchOutput {
            width: w,
            height: h,
            rgb,
            provenance: RetouchProvenance {
                model: response.get("model").and_then(Value::as_str).ok_or("Worker omitted model provenance.")?.into(),
                revision: response
                    .get("model_version")
                    .or_else(|| response.get("revision"))
                    .and_then(Value::as_str)
                    .ok_or("Worker omitted model revision.")?
                    .into(),
                identity: request.identity.clone(),
            },
        })
    }
}

fn encode_rgb(rgb: &[[f32; 3]]) -> Vec<u8> {
    rgb.iter().flatten().flat_map(|v| v.to_le_bytes()).collect()
}

fn decode_rgb(raw: &[u8]) -> std::result::Result<Vec<[f32; 3]>, String> {
    raw.chunks_exact(16)
        .map(|chunk| {
            let mut rgb = [0.0; 3];
            for (v, bytes) in rgb.iter_mut().zip(chunk.chunks_exact(4)) {
                *v = f32::from_le_bytes(<[u8; 4]>::try_from(bytes).map_err(|e| e.to_string())?);
            }
            Ok(rgb)
        })
        .collect()
}

pub(super) fn backend_directory(backend: &str) -> Option<&'static str> {
    match backend {
        "objectclear" => Some("objectclear"),
        "lama" => Some("lama"),
        "rorem" => Some("model"),
        _ => None,
    }
}

pub(super) fn wait_worker(
    child: &mut std::process::Child,
    cancel: &std::sync::atomic::AtomicBool,
    timeout: std::time::Duration,
) -> std::result::Result<(), String> {
    use std::{sync::atomic::Ordering, time::Duration};
    let started = std::time::Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) || started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(if cancel.load(Ordering::Relaxed) { "Cancelled." } else { "AI Remove timed out after ten minutes." }.into());
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => return Err("The local AI worker failed. Check the model/runtime setup.".into()),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.to_string());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_v1_rgb_is_little_endian_and_legacy_alpha_is_discarded() {
        let rgb = [[0.25, -0.5, 4.0], [1.0, 0.0, -0.0]];
        let expected: Vec<u8> = [0.25f32, -0.5, 4.0, 1.0, 0.0, -0.0].into_iter().flat_map(f32::to_le_bytes).collect();
        assert_eq!(encode_rgb(&rgb), expected);
        let rgba: Vec<u8> = [0.25f32, -0.5, 4.0, f32::NAN, 1.0, 0.0, -0.0, -100.0].into_iter().flat_map(f32::to_le_bytes).collect();
        assert_eq!(decode_rgb(&rgba).unwrap(), rgb);
    }
    #[test]
    fn legacy_python_launch_keeps_isolation_and_worker_arguments() {
        let runtime = Path::new("/synthetic/runtime");
        let runner = ProtocolRunner { runtime, backend: "objectclear", package: None };
        let command = runner.command().unwrap();
        assert_eq!(command.get_program(), runtime.join("bin/python3"));
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["-I", "-B", "/synthetic/runtime/worker.py"]);
        assert_eq!(runner.identity().adapter, "python-protocol-v1");
    }
    #[test]
    fn untrusted_native_runtime_cannot_construct_launch_command() {
        let package = crate::ai_models::Resolved {
            id: "objectclear-test".into(),
            runtime_id: "test-runtime".into(),
            runtime: "/missing/native-runtime".into(),
            model: "/missing/model".into(),
            worker: "/missing/native-runtime/bin/lightcraft-ai-native".into(),
            backend: "objectclear".into(),
            execution: crate::ai_models::ExecutionKind::NativeProtocolV1,
            manifest: json!({}),
        };
        let runner = ProtocolRunner { runtime: &package.runtime, backend: "objectclear", package: Some(&package) };
        assert_eq!(runner.identity().adapter, "native-protocol-v1");
        assert!(runner.command().is_err());
        let mismatched = ProtocolRunner { runtime: Path::new("/different"), backend: "objectclear", package: Some(&package) };
        assert!(mismatched.command().unwrap_err().contains("identity changed"));
    }
}
