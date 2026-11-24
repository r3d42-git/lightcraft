//! Okeinla 1.0 uses an opset-20 NAFNet graph. Its tiles are run by the bundled
//! ONNX Runtime on the CPU. The Bayer tiler handles the author's output gain.
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use lightcraft_denoise::known::{OKEINLA_SHA256, OKEINLA_SIZE};
use lightcraft_denoise::manifest::{DenoiserManifest, Gain};
use lightcraft_denoise::run::{Error, TileRunner};
use ort::session::Session;
use ort::tensor::TensorElementType;
use ort::value::{Outlet, TensorRef, ValueType};
use serde_json::{Value, json};

use super::model::{Model, RunOn, checked_self_test};
use super::settings::read_capped;

const TILE: usize = 512;
const INPUT: [i64; 4] = [1, 4, 512, 512];
const OUTPUT: [i64; 4] = [1, 3, 1024, 1024];
const LIBRARY: &str = "libonnxruntime.1.23.2.dylib";

pub(super) struct Okeinla {
    session: Mutex<Session>,
    manifest: DenoiserManifest,
    load_ms: f64,
}

impl Okeinla {
    pub(super) fn load(path: &Path, manifest: &DenoiserManifest) -> Result<Self, String> {
        let start = web_time::Instant::now();
        lightcraft_denoise::manifest::validate(manifest).map_err(|e| e.to_string())?;
        if manifest.version != "1.0"
            || manifest.tile != TILE as u32
            || manifest.gain != Gain::CfaLeastSquares
            || manifest.sha256.as_deref() != Some(OKEINLA_SHA256)
            || manifest.size_bytes != Some(OKEINLA_SIZE)
        {
            return Err("the manifest does not describe the supported Okeinla 1.0 weights and Bayer adapter".into());
        }
        // Execute exactly the bytes verified here, even if an installation is being replaced.
        let bytes = read_capped(path, OKEINLA_SIZE as usize).ok_or("cannot read the Okeinla 1.0 weights at their expected size")?;
        if bytes.len() != OKEINLA_SIZE as usize || lightcraft_denoise::hash::sha256_hex(&bytes) != OKEINLA_SHA256 {
            return Err("the file does not match the Okeinla 1.0 weights".into());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let dir = exe.parent().ok_or("cannot locate the Okeinla inference runtime")?;
        let lib = if dir.ends_with("Contents/MacOS") { dir.join("../Frameworks").join(LIBRARY) } else { dir.join(LIBRARY) };
        // An absolute path selects the shipped library even when ORT_DYLIB_PATH is set.
        ort::init_from(&lib).map_err(|e| format!("cannot load {}: {e}", lib.display()))?.commit();
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
        let session = Session::builder()
            .and_then(|b| b.with_intra_threads(threads))
            .and_then(|b| b.with_intra_op_spinning(false))
            .and_then(|b| b.commit_from_memory(&bytes))
            .map_err(|e| format!("cannot prepare the Okeinla model: {e}"))?;
        let matches = |ports: &[Outlet], name, dims: &[i64]| {
            matches!(ports, [port] if port.name() == name && matches!(port.dtype(),
                ValueType::Tensor { ty: TensorElementType::Float32, shape, .. } if &**shape == dims))
        };
        if !matches(session.inputs(), "input", &INPUT) || !matches(session.outputs(), "output", &OUTPUT) {
            return Err("unexpected Okeinla input or output type, shape or name".into());
        }
        Ok(Self { session: Mutex::new(session), manifest: manifest.clone(), load_ms: start.elapsed().as_secs_f64() * 1000.0 })
    }
}

impl TileRunner for Okeinla {
    fn run(&self, input: &[f32]) -> Result<Vec<f32>, Error> {
        if input.len() != 4 * TILE * TILE || input.iter().any(|v| !v.is_finite()) {
            return Err(Error::Input("expected four finite 512 × 512 Bayer planes".into()));
        }
        let runtime_error = |e: ort::Error| Error::Runtime(e.to_string());
        let tensor = TensorRef::from_array_view((INPUT, input)).map_err(runtime_error)?;
        let mut session = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        let outputs = session.run(ort::inputs![tensor]).map_err(runtime_error)?;
        let output = outputs.get("output").ok_or_else(|| Error::Model("missing Okeinla output".into()))?;
        let (shape, data) = output.try_extract_tensor::<f32>().map_err(runtime_error)?;
        if **shape != OUTPUT || data.len() != 12 * TILE * TILE || data.iter().any(|v| !v.is_finite()) {
            return Err(Error::Model("invalid Okeinla RGB tile".into()));
        }
        Ok(data.to_vec())
    }
}

impl Model for Okeinla {
    fn runner(&self, _run_on: RunOn, _threads: usize) -> (&dyn TileRunner, usize) {
        (self, 1)
    }

    fn bytes_per_tile(&self) -> usize {
        // ORT's intermediate activations and workspace for a full 512-cell tile.
        1024 * 1024 * 1024
    }

    fn self_test(&self, run_on: RunOn) -> Result<Value, String> {
        let mut result = checked_self_test(lightcraft_denoise::runtime::self_test(self, &self.manifest, self.load_ms))?;
        result["device"] = self.device(run_on);
        Ok(result)
    }

    fn device(&self, _run_on: RunOn) -> Value {
        json!({"kind": "cpu", "reason": "Okeinla 1.0 runs with ONNX Runtime on the CPU"})
    }
}
