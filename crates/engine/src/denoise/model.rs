use super::gpu::GPU_SETUP_MARKER;
#[cfg(feature = "denoise")]
use super::gpu::{GPU_CRASHED, GPU_MAX_FALLBACKS, GPU_PARALLEL, GPU_SLOW, GPU_STUCK, card_is_faster, make_gpu};
#[cfg(feature = "denoise")]
use super::gpu::{GPU_SETUP_LIMIT, card_tile_ms, set_up_card};
#[cfg(feature = "denoise")]
use super::settings::{installed_models, read_settings};
#[cfg(feature = "denoise")]
use super::state::Pace;
use super::{Arc, DenoiserManifest, Mutex, OnceLock, Path, PathBuf, TileRunner, Value, json};
#[cfg(feature = "denoise")]
use super::{AtomicBool, AtomicUsize, Ordering, PoisonError, RunError};
/// Stack of the threads that run the model (the runtime recurses through its graph).
pub(super) const STACK: usize = 16 << 20;

// ---------------------------------------------------------------------------------------------------------------------
// the model

/// Where the model runs (Settings ▸ AI Denoise ▸ Speed; `denoise.settings {runOn}`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunOn {
    /// The graphics card where it is faster than the processor at the pace the work goes at (both are timed on this
    /// computer when the card is set up), else the processor.
    #[default]
    Auto,
    /// The graphics card whenever it can run the model, even where the processor would be faster.
    Gpu,
    /// Always the processor.
    Cpu,
}

impl RunOn {
    /// `auto`, `gpu` or `cpu`.
    pub fn parse(name: &str) -> Option<RunOn> {
        match name {
            "auto" => Some(RunOn::Auto),
            "gpu" => Some(RunOn::Gpu),
            "cpu" => Some(RunOn::Cpu),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            RunOn::Auto => "auto",
            RunOn::Gpu => "gpu",
            RunOn::Cpu => "cpu",
        }
    }
}

/// A loaded denoise model: what runs tiles, and a self-check.
pub(crate) trait Model: Send + Sync {
    /// What runs a photo's tiles, and how many to run at once, when `threads` may be used and the setting is `run_on`
    /// (a graphics card is kept busy by a few). The first time the card may be used it is set up and checked here.
    fn runner(&self, run_on: RunOn, threads: usize) -> (&dyn TileRunner, usize);
    /// Check it does something sensible: the test's result, or why not. Unless `run_on` is the processor the graphics
    /// card is set up and checked too, and the result says where the work will run.
    fn self_test(&self, run_on: RunOn) -> Result<Value, String>;
    /// Look at the photo's whole mosaic before its tiles run (a model that measures the noise from the photo does it
    /// here, so every tile is cleaned by the same measure).
    fn prepare(&self, _mosaic: &[f32], _width: usize, _height: usize) {}
    /// Conservative scratch bound for an unknown runner.
    fn bytes_per_tile(&self) -> usize {
        512 * 1024 * 1024
    }
    /// Where the tiles run: `{kind: "gpu", adapter}`, `{kind: "cpu", reason?}` or `{kind: "pending"}` (the card is not
    /// set up yet), with the card's and the processor's time for the check tile (`cardMs`, `cpuMs`) once both were
    /// timed. Never starts anything.
    fn device(&self, _run_on: RunOn) -> Value {
        json!({"kind": "cpu"})
    }
}

/// The built-in denoiser (`lightcraft_denoise::builtin`): nothing to load, so it is what runs until a model is chosen. Its
/// filter runs on the graphics card when there is one that agrees with the CPU on a check tile and is faster, else on
/// the CPU (see [`BuiltinCard`]); the rest of it always runs on the CPU.
pub(super) struct BuiltinModel {
    pub(super) runner: OnceLock<lightcraft_denoise::builtin::Builtin>,
    #[cfg(feature = "denoise")]
    card: Arc<BuiltinCard>,
    /// The runner of a photo that runs on the card.
    #[cfg(feature = "denoise")]
    hybrid: OnceLock<CardRunner>,
}

impl BuiltinModel {
    /// For a photo, with the card's state for the set-up marker `marker` (where its set-up is recorded).
    pub(super) fn new(#[cfg_attr(not(feature = "denoise"), allow(unused_variables))] marker: &Path) -> Self {
        BuiltinModel {
            runner: OnceLock::new(),
            #[cfg(feature = "denoise")]
            card: builtin_card(marker),
            #[cfg(feature = "denoise")]
            hybrid: OnceLock::new(),
        }
    }
}

impl Model for BuiltinModel {
    fn runner(&self, run_on: RunOn, threads: usize) -> (&dyn TileRunner, usize) {
        let cpu = self.runner.get_or_init(Default::default);
        #[cfg(feature = "denoise")]
        {
            let threads = threads.max(1);
            self.card.last_threads.store(threads, Ordering::Relaxed);
            if let Ok(side) = self.card.choose(run_on, threads, true)
                && let Ok(gpu) = lightcraft_gpu::bm3d::BuiltinRunner::new(side.gpu.clone(), *cpu)
            {
                let hybrid = self.hybrid.get_or_init(|| CardRunner { gpu, cpu: *cpu, card: self.card.clone() });
                return (hybrid, threads.min(GPU_PARALLEL));
            }
        }
        #[cfg(not(feature = "denoise"))]
        let _ = run_on;
        (cpu, threads)
    }

    /// Measure the whole photo's noise once, so every tile is cleaned by the photo's noise instead of its own.
    fn prepare(&self, mosaic: &[f32], width: usize, height: usize) {
        let _ = self.runner.set(lightcraft_denoise::builtin::Builtin::with_noise(lightcraft_denoise::builtin::estimate_noise(mosaic, width, height)));
    }

    fn self_test(&self, _run_on: RunOn) -> Result<Value, String> {
        let side = 16;
        let out = lightcraft_denoise::builtin::Builtin::default().run(&vec![0.25; 4 * side * side]).map_err(|e| e.to_string())?;
        if out.len() != 12 * side * side || out.iter().any(|v| !v.is_finite()) {
            return Err("the built-in denoiser gave an unexpected result".into());
        }
        Ok(json!({"ok": true, "builtin": true}))
    }

    /// Planes, the two mosaics and the working copies of a 256-cell tile.
    fn bytes_per_tile(&self) -> usize {
        64 * 1024 * 1024
    }

    fn device(&self, run_on: RunOn) -> Value {
        #[cfg(feature = "denoise")]
        {
            self.card.device(run_on)
        }
        #[cfg(not(feature = "denoise"))]
        {
            let _ = run_on;
            json!({"kind": "cpu", "reason": "built-in"})
        }
    }
}

/// Loads the model at a path as its manifest describes it.
pub(crate) type Loader = Arc<dyn Fn(&Path, &DenoiserManifest) -> Result<Arc<dyn Model>, String> + Send + Sync>;

/// A set-up that may work another time (a crash, the time limit), unlike one the card or the model cannot do.
#[cfg(feature = "denoise")]
pub(super) fn worth_retrying(why: &str) -> bool {
    why == GPU_CRASHED || why == GPU_STUCK || why.starts_with(GPU_SLOW)
}

/// The model on our pure-Rust CPU runner, and on the graphics card when there is one that can run it: the card is set up for the
/// first photo that may use it, checked against the CPU's answer on a check tile and timed against it, and used while
/// it keeps working; a tile it gets wrong is run on the CPU, so a photo is always finished.
#[cfg(feature = "denoise")]
pub(super) struct Network {
    pub(super) cpu: lightcraft_denoise::runtime::CpuRunner,
    pub(super) path: PathBuf,
    pub(super) tile: usize,
    pub(super) gpu: OnceLock<Result<GpuSide, String>>,
    /// Tiles the card got wrong (an error, numbers that are not finite) and the CPU ran.
    pub(super) fallbacks: AtomicUsize,
    /// Tiles at once of the photo started last (0: none yet), to say where the work runs.
    pub(super) last_threads: AtomicUsize,
    /// Another computer's graphics card (`LIGHTCRAFT_DENOISE_URL` + `LIGHTCRAFT_DENOISE_TOKEN`; see
    /// `docs/denoise.md` → Another computer's graphics card), greeted on first use: it takes tiles while it has a free
    /// slot, on top of this computer's runner.
    pub(super) remote: Option<RemoteSide>,
    /// Whether this computer's share runs on the card (else the CPU), as chosen for the photo started last.
    pub(super) local_card: AtomicBool,
}

#[cfg(feature = "denoise")]
pub(super) struct RemoteSide {
    runner: lightcraft_denoise::remote::Remote,
    /// The greeting, and the server's time for the check tile there and back (ms; the faster of two runs).
    greeted: OnceLock<Result<(lightcraft_denoise::remote::ServerInfo, f64), String>>,
    /// Whether the photo started last shares tiles with the server, and if not because it is slower, the two times.
    share: AtomicBool,
    slower: Mutex<Option<(f64, f64)>>,
}

#[cfg(feature = "denoise")]
impl RemoteSide {
    /// From the environment, for the model whose file has `sha256` and tiles of `tile` cells.
    fn from_env(sha256: &str, tile: usize) -> Option<RemoteSide> {
        let var = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let address = var("LIGHTCRAFT_DENOISE_URL")?;
        let token = var("LIGHTCRAFT_DENOISE_TOKEN").unwrap_or_default();
        Some(RemoteSide::new(lightcraft_denoise::remote::Remote::new(&address, &token, sha256, tile)))
    }

    pub(super) fn new(runner: lightcraft_denoise::remote::Remote) -> RemoteSide {
        RemoteSide { runner, greeted: OnceLock::new(), share: AtomicBool::new(false), slower: Mutex::new(None) }
    }

    /// Greet the server and time a check tile there and back: a server behind a slow link can take seconds a tile, and
    /// handing it tiles a fast local card would finish sooner only makes the photo wait for it.
    fn greet(&self) -> Result<(lightcraft_denoise::remote::ServerInfo, f64), String> {
        let info = self.runner.hello().map_err(|e| e.to_string())?;
        let input = lightcraft_denoise::runtime::check_tile(info.tile);
        let mut best = f64::INFINITY;
        for _ in 0..2 {
            let started = web_time::Instant::now();
            match self.runner.try_run(&input) {
                Some(Ok(_)) => best = best.min(started.elapsed().as_secs_f64() * 1000.0),
                Some(Err(e)) => return Err(format!("the check tile failed: {e}")),
                None => return Err("no free slot for the check tile".into()),
            }
        }
        Ok((info, best))
    }

    /// The server when it can take tiles now (greeting it the first time), else why not.
    fn usable(&self, start: bool) -> Result<&lightcraft_denoise::remote::Remote, String> {
        let greeted = if start { Some(self.greeted.get_or_init(|| self.greet())) } else { self.greeted.get() };
        match greeted {
            None => Err("not contacted yet".into()),
            Some(Err(why)) => Err(why.clone()),
            Some(Ok(_)) if !self.runner.healthy() => {
                Err(format!("it failed {} tiles ({})", self.runner.failures(), self.runner.last_error().unwrap_or_default()))
            }
            Some(Ok(_)) => Ok(&self.runner),
        }
    }

    /// The server's time for a tile, once greeted.
    fn tile_ms(&self) -> Option<f64> {
        self.greeted.get().and_then(|g| g.as_ref().ok()).map(|g| g.1)
    }

    fn describe(&self) -> Value {
        match self.greeted.get() {
            None => json!({"address": self.runner.address(), "state": "pending"}),
            Some(Err(why)) => json!({"address": self.runner.address(), "state": "unavailable", "reason": why}),
            Some(Ok((info, ms))) => {
                let slower = *self.slower.lock().unwrap_or_else(PoisonError::into_inner);
                let state = match () {
                    _ if !self.runner.healthy() => "given up",
                    _ if slower.is_some() => "slower",
                    _ if self.share.load(Ordering::Relaxed) => "used",
                    _ => "idle",
                };
                let mut v =
                    json!({"address": info.address, "device": info.device, "state": state, "remoteMs": ms, "failures": self.runner.failures()});
                if let (Some((_, local)), Some(o)) = (slower, v.as_object_mut()) {
                    o.insert("localMs".into(), json!(local));
                }
                if let (Some(e), Some(o)) = (self.runner.last_error(), v.as_object_mut()) {
                    o.insert("lastError".into(), json!(e));
                }
                v
            }
        }
    }
}

#[cfg(feature = "denoise")]
pub(super) struct GpuSide {
    pub(super) runner: Box<dyn TileRunner + Send>,
    pub(super) adapter: String,
    /// The check tile on the card, the fastest of a few runs (ms).
    pub(super) card_ms: f64,
    /// The check tile on the CPU, on one thread (ms).
    pub(super) cpu_ms: f64,
}

#[cfg(feature = "denoise")]
impl Network {
    /// The card when it can be used now, else why not. `start` sets it up when that has not been tried yet; without it
    /// nothing is started.
    fn card(&self, start: bool) -> Result<&GpuSide, String> {
        let side = if start {
            Some(self.gpu.get_or_init(|| {
                let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| make_gpu(&self.cpu, &self.path, self.tile)))
                    .unwrap_or_else(|_| Err("the GPU runner gave up".into()));
                if let Err(why) = &made {
                    log::info!("denoise: running on the CPU ({why})");
                }
                made
            }))
        } else {
            self.gpu.get()
        };
        let g = match side {
            None => return Err("the graphics card is not set up yet".into()),
            Some(Err(why)) => return Err(why.clone()),
            Some(Ok(g)) => g,
        };
        if let Some(why) = lightcraft_gpu::nn::broken_reason() {
            return Err(format!("the graphics card stopped working ({why})"));
        }
        if self.fallbacks.load(Ordering::Relaxed) >= GPU_MAX_FALLBACKS {
            return Err(format!("{} got {GPU_MAX_FALLBACKS} tiles wrong", g.adapter));
        }
        Ok(g)
    }

    /// The card, when a photo made `threads` tiles at a time under `run_on` runs on it; else why the CPU does.
    fn choose(&self, run_on: RunOn, threads: usize, start: bool) -> Result<&GpuSide, String> {
        if run_on == RunOn::Cpu {
            return Err("the processor is chosen in Settings".into());
        }
        let g = self.card(start)?;
        if run_on == RunOn::Auto && !card_is_faster(g.card_ms, g.cpu_ms, threads) {
            return Err(format!("it is faster than {} on this computer", g.adapter));
        }
        Ok(g)
    }

    fn fell_back(&self, why: &str) {
        let n = self.fallbacks.fetch_add(1, Ordering::Relaxed) + 1;
        if n == 1 {
            log::warn!("denoise: a tile came back wrong from the GPU ({why}); the CPU runs it");
        } else if n == GPU_MAX_FALLBACKS {
            log::warn!("denoise: the GPU got {n} tiles wrong; the CPU does the rest");
        }
    }
}

/// A remote server's free slot first (when there is one), then the card when this photo runs on it, with the CPU
/// running any tile either gets wrong. Handed out for photos that run on the card or have a server to share.
#[cfg(feature = "denoise")]
impl TileRunner for Network {
    fn run(&self, input: &[f32]) -> Result<Vec<f32>, RunError> {
        if let Some(Ok(remote)) = self.remote.as_ref().filter(|r| r.share.load(Ordering::Relaxed)).map(|r| r.usable(false))
            && let Some(Ok(out)) = remote.try_run(input)
        {
            return Ok(out);
        }
        if !self.local_card.load(Ordering::Relaxed) {
            return self.cpu.run(input);
        }
        if let Ok(g) = self.card(false) {
            match g.runner.run(input) {
                Ok(out) if out.iter().all(|v| v.is_finite()) => return Ok(out),
                Ok(_) => self.fell_back("numbers that are not finite"),
                Err(e) => self.fell_back(&e.to_string()),
            }
        }
        self.cpu.run(input)
    }
}

#[cfg(feature = "denoise")]
impl Model for Network {
    fn bytes_per_tile(&self) -> usize {
        self.cpu.bytes_per_tile()
    }
    fn runner(&self, run_on: RunOn, threads: usize) -> (&dyn TileRunner, usize) {
        let threads = threads.max(1);
        self.last_threads.store(threads, Ordering::Relaxed);
        let (card, local) = match self.choose(run_on, threads, true) {
            Ok(_) => (true, threads.min(GPU_PARALLEL)),
            Err(_) => (false, threads),
        };
        self.local_card.store(card, Ordering::Relaxed);
        if let Some(r) = &self.remote {
            // this computer's time for a tile: the card's, or the CPU's with `threads` tiles sharing it (as
            // `card_is_faster` counts them); unknown when the card was never set up (then the server is used)
            let local_ms = match self.gpu.get() {
                Some(Ok(g)) if card => g.card_ms,
                Some(Ok(g)) => g.cpu_ms / (1.0 + 0.5 * threads.saturating_sub(1) as f64),
                _ => f64::INFINITY,
            };
            let share = match (r.usable(true), r.tile_ms()) {
                (Ok(_), Some(ms)) if ms < local_ms => {
                    *r.slower.lock().unwrap_or_else(PoisonError::into_inner) = None;
                    true
                }
                (Ok(_), Some(ms)) => {
                    *r.slower.lock().unwrap_or_else(PoisonError::into_inner) = Some((ms, local_ms));
                    false
                }
                _ => false,
            };
            r.share.store(share, Ordering::Relaxed);
            if let (true, Ok(remote)) = (share, r.usable(false)) {
                // the server's slots come on top of this computer's own tiles at once
                return (self, local + remote.slots());
            }
        }
        if card { (self, local) } else { (&self.cpu, local) }
    }

    fn self_test(&self, run_on: RunOn) -> Result<Value, String> {
        let t = self.cpu.self_test();
        if !t.ok {
            let failed: Vec<&str> = t.checks.iter().filter(|(_, ok)| !*ok).map(|(c, _)| c.as_str()).collect();
            return Err(format!("the model failed its self-test: it does not {}", failed.join(", and does not ")));
        }
        let mut v = serde_json::to_value(&t).map_err(|e| e.to_string())?;
        // the card is set up and checked here, so the test says where the work will run
        if run_on != RunOn::Cpu {
            let _ = self.card(true);
        }
        if let Some(o) = v.as_object_mut() {
            o.insert("device".into(), self.device(run_on));
        }
        Ok(v)
    }

    fn device(&self, run_on: RunOn) -> Value {
        if run_on != RunOn::Cpu && self.gpu.get().is_none() {
            return json!({"kind": "pending"});
        }
        let threads = match self.last_threads.load(Ordering::Relaxed) {
            0 => Pace::Full.parallel(std::thread::available_parallelism().map_or(1, |n| n.get()), None),
            n => n,
        };
        let mut v = match self.choose(run_on, threads, false) {
            Ok(g) => json!({"kind": "gpu", "adapter": g.adapter}),
            // a set-up that crashed or ran out of time can be tried again (`denoise.settings {runOn}`)
            Err(why) => json!({"kind": "cpu", "retry": worth_retrying(&why), "reason": why}),
        };
        if let (Some(Ok(g)), Some(o)) = (self.gpu.get(), v.as_object_mut()) {
            // measured on this computer, on the check tile: the card's best run, and one tile alone on the CPU
            o.insert("cardMs".into(), json!(g.card_ms));
            o.insert("cpuMs".into(), json!(g.cpu_ms));
            o.insert("fellBack".into(), json!(self.fallbacks.load(Ordering::Relaxed)));
        }
        if let (Some(r), Some(o)) = (&self.remote, v.as_object_mut()) {
            o.insert("remote".into(), r.describe());
        }
        v
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// the built-in denoiser on the graphics card

/// The noise the check tile is made with and the built-in denoiser is told for it (variance = a · signal + b).
#[cfg(feature = "denoise")]
pub(super) const CHECK_NOISE: (f32, f32) = (0.01, 0.001);
/// How far the card's filter may be from the CPU's on the check tile, in the noise's deviations: at most this at one place
/// (a few blocks whose match or threshold falls either side of a tie in the two devices' roundings change by up to about a
/// tenth; a broken kernel by far more), and this on average.
#[cfg(feature = "denoise")]
pub(super) const BUILTIN_AGREE_WORST: f32 = 0.3;
#[cfg(feature = "denoise")]
pub(super) const BUILTIN_AGREE_MEAN: f32 = 1e-3;

/// A mosaic tile (four planes of `side` cells): a ramp, edges and some texture with noise of [`CHECK_NOISE`], the same
/// every time.
#[cfg(feature = "denoise")]
pub(super) fn builtin_check_tile(side: usize) -> Vec<f32> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut uniform = move || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((state >> 40) as f32 + 0.5) / (1u64 << 24) as f32
    };
    let mut out = Vec::with_capacity(4 * side * side);
    for plane in 0..4 {
        for i in 0..side * side {
            let (x, y) = (i % side, i / side);
            let step = if (x / 37 + y / 29) % 2 == 0 { 0.25 } else { 0.0 };
            let signal =
                (0.1 + 0.4 * x as f32 / side as f32 + 0.1 * y as f32 / side as f32 + step + 0.05 * ((x as f32 + 4.0 * plane as f32) * 0.7).sin())
                    .clamp(0.02, 0.95);
            let (u, v) = (uniform(), uniform());
            let noise = (-2.0 * u.ln()).sqrt() * (std::f32::consts::TAU * v).cos();
            out.push((signal + noise * (CHECK_NOISE.0 * signal + CHECK_NOISE.1).sqrt()).max(0.0));
        }
    }
    out
}

/// The card set up for the built-in denoiser: its kernels, and the check tile's time on it and on the CPU.
#[cfg(feature = "denoise")]
struct BuiltinGpu {
    gpu: Arc<lightcraft_gpu::bm3d::Bm3dGpu>,
    adapter: String,
    /// The check tile on the card, the fastest of a few runs (ms).
    card_ms: f64,
    /// The check tile on the CPU, on one thread (ms).
    cpu_ms: f64,
}

/// Set the card up for the built-in denoiser: kernels built, the check tile run on both and compared, both timed.
#[cfg(feature = "denoise")]
fn make_builtin_gpu(marker: &Path) -> Result<BuiltinGpu, String> {
    use lightcraft_denoise::builtin::{Builtin, TILE};
    let tile = TILE as usize;
    let builtin = Builtin::with_noise(CHECK_NOISE);
    let params = builtin.bm3d_params().ok_or("the built-in denoiser does not use the block-matching filter")?;
    let input = Arc::new(builtin_check_tile(tile));
    let on_card = input.clone();
    let card = move || -> Result<_, String> {
        let gpu = Arc::new(lightcraft_gpu::bm3d::Bm3dGpu::new(&params, tile)?);
        let runner = lightcraft_gpu::bm3d::BuiltinRunner::new(gpu.clone(), builtin)?;
        let agreement = runner.agreement(&on_card).map_err(|e| format!("the GPU could not run the check tile: {e}"))?;
        let whole = runner.run(&on_card).map_err(|e| format!("the GPU could not run the check tile: {e}"))?;
        let card_ms = card_tile_ms(&runner, &on_card);
        Ok((gpu, agreement, whole, card_ms))
    };
    let on_cpu = || {
        let started = web_time::Instant::now();
        (builtin.run(&input), started.elapsed().as_secs_f64() * 1000.0)
    };
    let ((gpu, agreement, whole, card_ms), (want, cpu_ms)) = set_up_card(marker, GPU_SETUP_LIMIT, card, on_cpu)?;
    let want = want.map_err(|e| format!("the CPU could not check the GPU: {e}"))?;
    if whole.len() != want.len() || !whole.iter().all(|v| v.is_finite()) {
        return Err("its picture of the check tile is not the CPU's size or has numbers that are not finite: not used".into());
    }
    if !(agreement.worst <= BUILTIN_AGREE_WORST && agreement.mean <= BUILTIN_AGREE_MEAN) {
        return Err(format!(
            "its filter's answer on the check tile is not the CPU's (off by {:.2} at most and {:.1e} on average, in noise deviations): not used",
            agreement.worst, agreement.mean
        ));
    }
    let adapter = gpu.adapter();
    Ok(BuiltinGpu { gpu, adapter, card_ms, cpu_ms })
}

/// What the built-in denoiser knows about the graphics card in this process: set up for the first photo that may use it,
/// and used while it keeps working. Kept by the marker of its set-up (see [`builtin_card`]) so that every photo shares it.
#[cfg(feature = "denoise")]
pub(super) struct BuiltinCard {
    marker: PathBuf,
    gpu: OnceLock<Result<BuiltinGpu, String>>,
    /// Tiles the card got wrong (an error, numbers that are not finite) and the CPU ran.
    fallbacks: AtomicUsize,
    /// Tiles at once of the photo started last (0: none yet), to say where the work runs.
    last_threads: AtomicUsize,
}

#[cfg(feature = "denoise")]
static BUILTIN_CARDS: Mutex<Vec<(PathBuf, Arc<BuiltinCard>)>> = Mutex::new(Vec::new());

/// The card's state for the set-up marker `marker`, made the first time it is asked for.
#[cfg(feature = "denoise")]
pub(super) fn builtin_card(marker: &Path) -> Arc<BuiltinCard> {
    let mut all = BUILTIN_CARDS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, c)) = all.iter().find(|(m, _)| m == marker) {
        return c.clone();
    }
    let c = Arc::new(BuiltinCard {
        marker: marker.to_path_buf(),
        gpu: OnceLock::new(),
        fallbacks: AtomicUsize::new(0),
        last_threads: AtomicUsize::new(0),
    });
    all.push((marker.to_path_buf(), c.clone()));
    c
}

/// Forget the card's state for the marker (and the marker): the card is tried afresh for the next photo.
#[cfg(feature = "denoise")]
pub(crate) fn forget_builtin_card(marker: &Path) {
    BUILTIN_CARDS.lock().unwrap_or_else(PoisonError::into_inner).retain(|(m, _)| m != marker);
    let _ = std::fs::remove_file(marker);
}

/// Where the card's set-up is recorded for the built-in denoiser whose `onnx` is its model's (made-up) path in the models
/// folder: beside it, and in the temporary folder when there is no models folder.
pub(crate) fn builtin_marker(onnx: &Path) -> PathBuf {
    match onnx.parent() {
        Some(p) if !p.as_os_str().is_empty() => onnx.with_file_name(GPU_SETUP_MARKER),
        _ => std::env::temp_dir().join(format!("lightcraft-builtin-{GPU_SETUP_MARKER}")),
    }
}

#[cfg(feature = "denoise")]
impl BuiltinCard {
    /// The card when it can be used now, else why not. `start` sets it up when that has not been tried yet; without it
    /// nothing is started.
    fn card(&self, start: bool) -> Result<&BuiltinGpu, String> {
        let side = if start {
            Some(self.gpu.get_or_init(|| {
                let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| make_builtin_gpu(&self.marker)))
                    .unwrap_or_else(|_| Err("the GPU runner gave up".into()));
                if let Err(why) = &made {
                    log::info!("denoise: the built-in denoiser runs on the CPU ({why})");
                }
                made
            }))
        } else {
            self.gpu.get()
        };
        let g = match side {
            None => return Err("the graphics card is not set up yet".into()),
            Some(Err(why)) => return Err(why.clone()),
            Some(Ok(g)) => g,
        };
        if let Some(why) = lightcraft_gpu::nn::broken_reason() {
            return Err(format!("the graphics card stopped working ({why})"));
        }
        if self.fallbacks.load(Ordering::Relaxed) >= GPU_MAX_FALLBACKS {
            return Err(format!("{} got {GPU_MAX_FALLBACKS} tiles wrong", g.adapter));
        }
        Ok(g)
    }

    /// The card, when a photo made `threads` tiles at a time under `run_on` runs on it; else why the CPU does.
    fn choose(&self, run_on: RunOn, threads: usize, start: bool) -> Result<&BuiltinGpu, String> {
        if run_on == RunOn::Cpu {
            return Err("the processor is chosen in Settings".into());
        }
        let g = self.card(start)?;
        if run_on == RunOn::Auto && !card_is_faster(g.card_ms, g.cpu_ms, threads) {
            return Err(format!("it is faster than {} on this computer", g.adapter));
        }
        Ok(g)
    }

    fn fell_back(&self, why: &str) {
        let n = self.fallbacks.fetch_add(1, Ordering::Relaxed) + 1;
        if n == 1 {
            log::warn!("denoise: a tile came back wrong from the GPU ({why}); the CPU runs it");
        } else if n == GPU_MAX_FALLBACKS {
            log::warn!("denoise: the GPU got {n} tiles wrong; the CPU does the rest");
        }
    }

    /// Where the tiles run (see [`Model::device`]).
    pub(super) fn device(&self, run_on: RunOn) -> Value {
        if run_on != RunOn::Cpu && self.gpu.get().is_none() {
            return json!({"kind": "pending"});
        }
        let threads = match self.last_threads.load(Ordering::Relaxed) {
            0 => Pace::Full.parallel(std::thread::available_parallelism().map_or(1, |n| n.get()), None),
            n => n,
        };
        let mut v = match self.choose(run_on, threads, false) {
            Ok(g) => json!({"kind": "gpu", "adapter": g.adapter}),
            // a set-up that crashed or ran out of time can be tried again (`denoise.settings {runOn}`)
            Err(why) => json!({"kind": "cpu", "retry": worth_retrying(&why), "reason": why}),
        };
        if let (Some(Ok(g)), Some(o)) = (self.gpu.get(), v.as_object_mut()) {
            o.insert("cardMs".into(), json!(g.card_ms));
            o.insert("cpuMs".into(), json!(g.cpu_ms));
            o.insert("fellBack".into(), json!(self.fallbacks.load(Ordering::Relaxed)));
        }
        v
    }
}

/// The built-in denoiser for a photo that runs on the card: the filter there, and the CPU running any tile the card gets
/// wrong, so a photo is always finished.
#[cfg(feature = "denoise")]
struct CardRunner {
    gpu: lightcraft_gpu::bm3d::BuiltinRunner,
    cpu: lightcraft_denoise::builtin::Builtin,
    card: Arc<BuiltinCard>,
}

#[cfg(feature = "denoise")]
impl TileRunner for CardRunner {
    fn run(&self, input: &[f32]) -> Result<Vec<f32>, RunError> {
        if self.card.card(false).is_ok() {
            match self.gpu.run(input) {
                Ok(out) if out.iter().all(|v| v.is_finite()) => return Ok(out),
                Ok(_) => self.card.fell_back("numbers that are not finite"),
                Err(e) => self.card.fell_back(&e.to_string()),
            }
        }
        self.cpu.run(input)
    }
}

/// The loader of this build: our pure-Rust executor with the `denoise` feature, else one that says why nothing can run.
pub(crate) fn default_loader() -> Loader {
    #[cfg(feature = "denoise")]
    {
        Arc::new(|path, manifest| {
            // the server must run this very file: identified by the manifest's hash, else the file's
            let sha256 = match &manifest.sha256 {
                Some(h) => Some(h.clone()),
                None if std::env::var_os("LIGHTCRAFT_DENOISE_URL").is_some() => lightcraft_denoise::hash::sha256_file(path).ok(),
                None => None,
            };
            let remote = sha256.and_then(|h| RemoteSide::from_env(&h, manifest.tile as usize));
            Ok(Arc::new(network(path, manifest, remote)?) as Arc<dyn Model>)
        })
    }
    #[cfg(not(feature = "denoise"))]
    {
        Arc::new(|_, _| Err("this build cannot run denoise models".to_string()))
    }
}

/// The model at `path` on this computer, sharing tiles with `remote` when there is one.
#[cfg(feature = "denoise")]
pub(super) fn network(path: &Path, manifest: &DenoiserManifest, remote: Option<RemoteSide>) -> Result<Network, String> {
    let cpu = lightcraft_denoise::runtime::CpuRunner::load(path, manifest).map_err(|e| e.to_string())?;
    Ok(Network {
        remote,
        local_card: AtomicBool::new(true),
        cpu,
        path: path.to_path_buf(),
        tile: manifest.tile as usize,
        gpu: OnceLock::new(),
        fallbacks: AtomicUsize::new(0),
        last_threads: AtomicUsize::new(0),
    })
}

/// A model ready to be served to other computers (`lightcraft-cli denoise-serve`).
#[cfg(feature = "denoise")]
pub struct Serving {
    pub model: lightcraft_denoise::remote::ModelId,
    pub tile: usize,
    /// Where its tiles run here: the graphics card's name, or why the CPU does.
    pub device: String,
    pub run: lightcraft_denoise::remote::RunTile,
}

/// The installed model `id` (default: the one selected in Settings) in the models folder `dir` (default: this user's),
/// set up to run on `run_on` as the app would: the card checked against the CPU first, the CPU when the card can't.
#[cfg(feature = "denoise")]
pub fn serving(dir: Option<&Path>, id: Option<&str>, run_on: RunOn) -> Result<Serving, String> {
    let dir = match dir {
        Some(d) => d.to_path_buf(),
        None => crate::config::default_denoise_models_dir().ok_or("no denoise models folder (set LIGHTCRAFT_DENOISE_MODELS)")?,
    };
    let want = match id {
        Some(i) => i.to_string(),
        None => read_settings(&dir).model.ok_or_else(|| {
            format!("no denoise model is selected in {} (install one in Settings ▸ AI Denoise, or name it with --model)", dir.display())
        })?,
    };
    let installed = installed_models(&dir)
        .into_iter()
        .find(|m| m.manifest.id == want)
        .ok_or_else(|| format!("no installed denoise model `{want}` in {}", dir.display()))?;
    let m = &installed.manifest;
    // the same identity a client derives (see `default_loader`)
    let sha256 = match &m.sha256 {
        Some(h) => h.clone(),
        None => lightcraft_denoise::hash::sha256_file(&installed.onnx).map_err(|e| format!("{}: {e}", installed.onnx.display()))?,
    };
    let net = Arc::new(network(&installed.onnx, m, None)?);
    let tile = m.tile as usize;
    // set the card up now (and say where the tiles run) rather than with the first client's tile
    let _ = net.runner(run_on, GPU_PARALLEL);
    let d = net.device(run_on);
    let device = match d["kind"].as_str() {
        Some("gpu") => format!("GPU: {}", d["adapter"].as_str().unwrap_or("?")),
        _ => format!("CPU ({})", d["reason"].as_str().unwrap_or("chosen")),
    };
    let run: lightcraft_denoise::remote::RunTile = Arc::new(move |input: &[f32]| net.runner(run_on, GPU_PARALLEL).0.run(input));
    Ok(Serving { model: lightcraft_denoise::remote::ModelId { id: m.id.clone(), version: m.version.clone(), sha256 }, tile, device, run })
}

/// A model that is loaded on first use and kept, shared by the background job and the exports.
pub(crate) type ModelSlot = Arc<Mutex<Option<Arc<dyn Model>>>>;
