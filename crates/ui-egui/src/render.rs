//! Background rendering: engine [`RenderJob`]s run on a [`JobPool`]; results become textures.
//!
//! Each visible thing that needs pixels is a *slot* (a grid thumbnail, the loupe, the "before"
//! image…). Requests are deduplicated per slot (a newer request replaces a queued older one) and
//! prioritised (loupe first, then on-screen thumbnails, then prefetch); thumbnails scrolled far
//! out of view are dropped from the queue. Thumbnail jobs hit the engine's preview cache (memory +
//! disk) before rendering. On wasm the jobs run inline, one per frame, unless the host installs a
//! [`RenderOffload`] (the browser build's Web Workers): then queued jobs are handed to it as it has
//! room, and its results are collected every frame.
//!
//! Stand-ins ([`QuickJob`]s, once per photo and settings): [`Slot::Preview`] holds what the loupe
//! shows until [`Slot::Main`] has the photo's render (cached view render, embedded camera JPEG or
//! a thumbnail render); [`Slot::ThumbQuick`] holds a raw's embedded preview in the grid until its
//! rendered thumbnail arrives (a cached thumbnail found by the quick job becomes the
//! [`Slot::Thumb`] texture directly).

use std::collections::HashMap;

use lightcraft_catalog::PhotoId;
use lightcraft_engine::Session;
use lightcraft_engine::media::{QuickJob, QuickSource, RenderJob, RenderResult};
use lightcraft_engine::pipeline::StageCache;
use lightcraft_preview::JobPool;
use lightcraft_raster::Histogram;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    Thumb(PhotoId),
    /// A thumbnail's stand-in (embedded preview of an unedited raw).
    ThumbQuick(PhotoId),
    Main,
    /// The loupe zoomed past what `Main` can hold: the window of the frame that is on screen,
    /// rendered at the zoom scale and drawn over `Main` (see [`crate::region`]).
    Region,
    /// The Before side of a Before/After view zoomed the same way (see [`Slot::Region`]).
    RegionBefore,
    /// The loupe's stand-in until `Main` has the photo.
    Preview,
    Before,
    Compare(u8),
    /// Compare or Reference zoomed past what `Compare(i)` holds: the window of pane `i`'s frame
    /// that is on screen, drawn over it (see [`crate::region`] and `panels::compare`).
    CompareRegion(u8),
    /// Background preparation of a neighbouring photo (no texture: its decoded source and view
    /// render are cached by the engine). 0 = next, 1 = previous.
    Prefetch(u8),
    /// The loupe's stand-in for a photo the user is likely to step to next (the camera JPEG of an
    /// unedited raw, or a cached render), made ahead in the direction of travel so a step shows it
    /// at once: [`Renderer::request_quick`] takes the texture over into [`Slot::Preview`].
    QuickAhead(u8),
    /// The loupe zoomed in on an unedited raw: its camera JPEG at full size, drawn under the window
    /// render until that arrives (a focus check at 100 % without waiting for the raw).
    QuickFull,
    /// A variant thumbnail (profile / preset browsers), by its job key ([`Session::variant_job`]).
    /// Kept in a bounded LRU ([`VARIANT_TEXTURES`]).
    Variant(u64),
    /// The loupe while hovering a preset or profile: the photo with the hovered look (nothing is
    /// committed).
    Hover,
    /// An import candidate's thumbnail in the import review dialog (by index).
    Import(u32),
    /// The second window's view of the active photo.
    Second,
    /// A slideshow slide (the one up, the one fading out, the next ones prepared in advance; see
    /// [`crate::panels::slideshow`]): a photo's, or a title screen.
    Slide(lightcraft_engine::slideshow::Item),
    /// The small composed slide in Settings ▸ Slideshow (the active photo with the layout being
    /// edited).
    SlidePreview,
    /// A slider drag's live HDR histogram: a small full-quality render of an HDR edit, beside the
    /// drafts (which have no HDR histogram), with no texture ([`Renderer::live_hdr_histogram`]).
    HdrHistogram,
}

/// Photo inspection preserves individual pixels when enlarged; reduced images stay smooth.
/// Stand-ins, thumbnails and composed slideshow images keep linear filtering in both directions.
fn texture_options(slot: Slot) -> egui::TextureOptions {
    match slot {
        Slot::Main | Slot::Before | Slot::Region | Slot::RegionBefore | Slot::Compare(_) | Slot::CompareRegion(_) | Slot::Second | Slot::Hover => {
            egui::TextureOptions { magnification: egui::TextureFilter::Nearest, ..egui::TextureOptions::LINEAR }
        }
        _ => egui::TextureOptions::LINEAR,
    }
}

pub struct Tex {
    pub key: u64,
    pub photo: PhotoId,
    pub tex: egui::TextureHandle,
    pub size: [usize; 2],
    pub histogram: Option<Histogram>,
    pub ms: f64,
    /// Set for stand-ins: where the image came from.
    pub quick: Option<QuickSource>,
    /// CPU copy of the pixels (only with [`Renderer::keep_pixels`]; for headless screenshots).
    pub pixels: Option<std::sync::Arc<egui::ColorImage>>,
}

/// Runs render jobs outside this thread's job pool (the browser build: Web Workers, each with its
/// own wasm instance). The [`Renderer`] keeps the queue (priorities, per-slot de-duplication) and
/// hands jobs over one at a time.
pub trait RenderOffload {
    /// Start `job` if an executor is free; otherwise give it back (`Some`: it stays queued).
    fn try_start(&mut self, slot: Slot, job: RenderJob) -> Option<RenderJob>;
    /// Finished jobs since the last call: (slot, result, run time in ms).
    fn finished(&mut self) -> Vec<(Slot, RenderResult, f64)>;
}

/// A job's result as the pool hands it back: the render and its texture pixels, converted on the
/// worker (`None` for prefetches and failures). A 10 MP zoom window's conversion took a few ms of
/// the UI thread on every slider step.
struct Finished {
    result: RenderResult,
    color: Option<Arc<egui::ColorImage>>,
}

/// Immutable identity captured when a request is submitted, not when its result is collected.
struct Requested {
    id: u64,
    source: Option<lightcraft_catalog::Source>,
}

struct Queued {
    slot: Slot,
    priority: u32,
    seq: u64,
    job: RenderJob,
}

/// The texture of a render: its pixels as egui wants them. Renders are opaque, so the common case
/// is a plain copy; only a pixel with alpha pays for premultiplying. (The generic conversion, on
/// the UI thread, was most of the time a 5 MP zoom window took to show.)
pub(crate) fn color_image(img: &lightcraft_raster::Rgba8) -> egui::ColorImage {
    let pixels = img
        .data
        .iter()
        .map(|p| if p[3] == 255 { egui::Color32::from_rgb(p[0], p[1], p[2]) } else { egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]) })
        .collect();
    egui::ColorImage { size: [img.width, img.height], source_size: egui::vec2(img.width as f32, img.height as f32), pixels }
}

/// How far over the budget the open photo's window and before caches may run before they are cut.
const HARD_FACTOR: usize = 4;

/// Which stage caches to clear, in order, so that the rest fit `budget` bytes.
///
/// What nothing is looking at goes first, the cheapest to rebuild and least reused first (hover,
/// compare, second window, background work). The open photo's own views are what a slider drag
/// reuses on every tick, and a cache cleared every frame is rebuilt every frame (a drag on a
/// 2800 px canvas got 3.6x slower), so they stay — counted and reported — up to
/// [`HARD_FACTOR`] times the budget; past that (a runaway: 8:1 on a huge canvas, both sides of
/// Before/After) the before side's and the zoom window's go, the before side first. The
/// whole-frame view's cache is never named. Empty caches are never named.
pub(crate) fn stage_trim_order(sizes: &[(Slot, usize)], budget: usize) -> Vec<Slot> {
    let idle_rank = |s: Slot| match s {
        Slot::Hover => 0,
        Slot::Compare(_) | Slot::CompareRegion(_) => 2,
        Slot::Second => 3,
        _ => 5,
    };
    let view_rank = |s: Slot| match s {
        Slot::RegionBefore => Some(0),
        Slot::Before => Some(1),
        Slot::Region => Some(2),
        _ => None,
    };
    let is_view = |s: Slot| matches!(s, Slot::Main) || view_rank(s).is_some();
    let mut held: usize = sizes.iter().map(|(_, b)| *b).fold(0, usize::saturating_add);
    let mut out = Vec::new();
    let mut idle: Vec<(Slot, usize)> = sizes.iter().copied().filter(|(s, b)| *b > 0 && !is_view(*s)).collect();
    idle.sort_by_key(|(s, _)| idle_rank(*s));
    for (slot, bytes) in idle {
        if held <= budget {
            break;
        }
        held = held.saturating_sub(bytes);
        out.push(slot);
    }
    let hard = budget.saturating_mul(HARD_FACTOR);
    let mut views: Vec<(Slot, usize, u8)> = sizes.iter().filter_map(|(s, b)| view_rank(*s).filter(|_| *b > 0).map(|r| (*s, *b, r))).collect();
    views.sort_by_key(|(_, _, r)| *r);
    for (slot, bytes, _) in views {
        if held <= hard {
            break;
        }
        held = held.saturating_sub(bytes);
        out.push(slot);
    }
    out
}

/// The share of the memory budget the per-view stage caches (the loupe's, the zoom window's, the
/// before / hover renders') may hold together before the idle ones are cleared (see
/// [`stage_trim_order`] for the open photo's own): they are the decoded-image sized buffers that the
/// engine's own caches do not count.
const STAGE_BUDGET_SHARE: usize = 4;

/// Variant thumbnails kept as textures (LRU), when nothing asks for more.
pub const VARIANT_TEXTURES: usize = 96;
/// The most a view can raise that to ([`Renderer::want_variants`]): a screenful of small faces is a few hundred pictures of
/// a few tens of kilobytes each.
pub const MAX_VARIANT_TEXTURES: usize = 800;
/// Priority of variant thumbnail jobs: below on-screen grid thumbnails and the loupe.
const VARIANT_PRIORITY: u32 = 6;

/// The camera-preview edge a loupe stand-in is made at (0: one that doesn't depend on the size).
fn quick_edge(job: &QuickJob) -> usize {
    job.embedded.as_ref().map_or(0, |e| e.2)
}

pub struct Renderer {
    #[cfg(test)]
    pub(crate) thumb_jobs_built: usize,
    requests: HashMap<Slot, Requested>,
    #[cfg(test)]
    failure_messages: Vec<String>,
    /// Request id of the pixels each interactive view (loupe, before, hover) shows: a draft that
    /// finishes after a newer request was made still replaces older pixels, so a long slider drag
    /// keeps updating the loupe instead of waiting for the drag to pause.
    shown_ids: HashMap<Slot, u64>,
    /// Requests made before this id belong to a library or preview cache that is gone.
    epoch: u64,
    preview_generation: Option<(std::sync::Weak<lightcraft_preview::PreviewCache>, u64)>,
    /// Last thumbnail inputs; weak identities do not retain photo histories or force deep clones.
    /// (photo, size bucket, job key, whether the request went through an embedded stand-in)
    thumb_inputs: HashMap<PhotoId, (std::sync::Weak<lightcraft_catalog::Photo>, usize, u64, bool)>,
    /// Photos whose grid thumbnail is the camera's embedded preview itself (no render behind it).
    embedded_thumbs: HashMap<PhotoId, (std::sync::Weak<lightcraft_catalog::Photo>, usize, u64)>,
    pool: JobPool<Slot, Finished>,
    /// Jobs run elsewhere (see [`RenderOffload`]); `queue` holds the ones not started yet.
    offload: Option<Box<dyn RenderOffload>>,
    queue: Vec<Queued>,
    seq: u64,
    /// Slot → (key, priority) of the request in flight (queued or running).
    pending: HashMap<Slot, (u64, u32)>,
    pub textures: HashMap<Slot, Tex>,
    /// The last [`Slot::HdrHistogram`] result: (photo, job key, histogram).
    pub live_hdr_histogram: Option<(PhotoId, u64, Histogram)>,
    pub last_main_ms: f64,
    /// Jobs finished since start (for inspect/perf).
    pub completed: u64,
    /// Finished jobs thrown away because a newer request for their slot had replaced them.
    pub discarded: u64,
    /// Photos given a grid thumbnail since start (a render replacing a stand-in isn't counted again):
    /// unlike [`Renderer::thumb_textures`] it never goes down when old thumbnails are evicted (for
    /// benchmarks).
    pub thumbs_made: u64,
    /// Keep a CPU copy of every texture so the UI can be rasterized headlessly (set when the app
    /// is driven by the control channel).
    pub keep_pixels: bool,
    /// Per-view intermediate results (loupe and "before"), so slider drags redo only what changed.
    stages: HashMap<Slot, Arc<StageCache>>,
    /// Slot → key of the last quick job requested for it (each is tried once).
    quick_tried: HashMap<Slot, u64>,
    /// The camera-preview edge each loupe stand-in ([`Slot::Preview`], [`Slot::QuickAhead`]) was
    /// asked for (0: one that doesn't depend on the size): a stand-in made ahead for a smaller loupe
    /// isn't taken over by a larger one.
    quick_edge: HashMap<Slot, usize>,
    /// Jobs that failed (unreadable or missing file…), by slot and job key, with the error and
    /// the photo: not requested again until the key changes (an edit, another size) — the grid
    /// asks every frame.
    failed: HashMap<Slot, (u64, String, u64, PhotoId)>,
    /// The catalog revision seen at the last poll: a failure is retried once the catalog changes
    /// (a relinked or re-imported file).
    catalog_rev: u64,
    /// Prefetch slot → key of the last job submitted (each runs once).
    prefetched: HashMap<Slot, u64>,
    /// Variant key → frame it was last asked for (LRU of [`Slot::Variant`] textures).
    variant_used: HashMap<u64, u64>,
    /// How many variants the last frame's views showed at once (see [`Self::want_variants`]).
    variant_want: usize,
    /// Frames polled so far.
    frame: u64,
    /// Slideshow slides may be held ([`Self::release_slides`] has something to do).
    slides_live: bool,
    /// Stage caches cleared to stay in budget (see [`stage_trim_order`]).
    stages_trimmed: u64,
    /// A budget other than the memory budget's share (tests).
    #[cfg(test)]
    pub(crate) stage_budget_override: Option<usize>,
    /// The monitor profile textures are made for (`app.displayProfile`; `None`: sRGB).
    display: Option<Arc<lightcraft_engine::display::Display>>,
    /// Since when nothing has been pending, and whether the GPU pool was trimmed since.
    #[cfg(not(target_arch = "wasm32"))]
    idle: Option<(std::time::Instant, bool)>,
}

/// How many [`Slot::QuickAhead`] stand-ins the loupe keeps ready.
pub const AHEAD_SLOTS: u8 = 6;

/// After this long without renders the GPU renderer's pool of recycled buffers is freed.
#[cfg(not(target_arch = "wasm32"))]
const IDLE_TRIM: std::time::Duration = std::time::Duration::from_secs(3);

impl Slot {
    /// An interactive view of the open photo: slider drags redo only the stages they feed, and a
    /// draft that finishes late still replaces older pixels.
    pub fn is_view(self) -> bool {
        matches!(self, Slot::Main | Slot::Region | Slot::RegionBefore | Slot::Before | Slot::Hover | Slot::HdrHistogram)
    }
}

impl Default for Renderer {
    fn default() -> Self {
        let threads = if cfg!(target_arch = "wasm32") { 0 } else { JobPool::<Slot, Finished>::default_threads().min(6) };
        Self::with_worker_threads(threads)
    }
}

/// Workers kept for the stand-ins ([`Slot::is_stand_in`]) on top of the others: the cheapest
/// pictures (a raw's embedded JPEG, a cached thumbnail or render) never wait for a worker while the
/// others render raws for seconds ("Rendering…" in the loupe and empty
/// cells in the filmstrip and grid while every worker decoded a 45 MP raw).
const STAND_IN_WORKERS: usize = 2;

/// Jobs below this priority are background work (the real thumbnails rendered behind the
/// stand-ins, [`crate::panels::grid::BACKGROUND_THUMB_PRIORITY`]: for a folder of raws with a
/// camera profile picked, every cell's): at most [`BACKGROUND_AT_ONCE`] of them run at once, so a
/// worker is always free for the photo on screen. (The neighbours prepared ahead have their own
/// limit.)
const BACKGROUND_BELOW: u32 = 4;
const BACKGROUND_AT_ONCE: usize = 2;

impl Slot {
    /// A stand-in picture: cheap to make (a raw's embedded JPEG, a cached picture), and what a view
    /// shows first, before any render ([`STAND_IN_WORKERS`]).
    pub fn is_stand_in(&self) -> bool {
        matches!(self, Slot::ThumbQuick(_) | Slot::Preview | Slot::QuickAhead(_))
    }
}

impl Renderer {
    fn pool(threads: usize) -> JobPool<Slot, Finished> {
        let pool = if threads == 0 { JobPool::new(0) } else { JobPool::with_fast_lane(threads, STAND_IN_WORKERS, Slot::is_stand_in) };
        pool.cap_background(BACKGROUND_BELOW, BACKGROUND_AT_ONCE);
        pool
    }

    /// Explicit worker count for hosts and bounded validation runs (one to six on native).
    /// Browser hosts continue to use inline jobs or their installed worker offload.
    pub fn with_worker_threads(threads: usize) -> Self {
        let threads = if cfg!(target_arch = "wasm32") { 0 } else { threads.clamp(1, 6) };
        Renderer {
            #[cfg(test)]
            thumb_jobs_built: 0,
            requests: HashMap::new(),
            #[cfg(test)]
            failure_messages: Vec::new(),
            shown_ids: HashMap::new(),
            epoch: 0,
            preview_generation: None,
            pool: Self::pool(threads),
            thumb_inputs: HashMap::new(),
            embedded_thumbs: HashMap::new(),
            offload: None,
            queue: Vec::new(),
            seq: 0,
            pending: HashMap::new(),
            textures: HashMap::new(),
            live_hdr_histogram: None,
            last_main_ms: 0.0,
            completed: 0,
            discarded: 0,
            thumbs_made: 0,
            keep_pixels: false,
            stages: HashMap::new(),
            quick_tried: HashMap::new(),
            quick_edge: HashMap::new(),
            failed: HashMap::new(),
            catalog_rev: 0,
            prefetched: HashMap::new(),
            variant_used: HashMap::new(),
            variant_want: 0,
            frame: 0,
            slides_live: false,
            stages_trimmed: 0,
            #[cfg(test)]
            stage_budget_override: None,
            display: None,
            #[cfg(not(target_arch = "wasm32"))]
            idle: None,
        }
    }
}

impl Renderer {
    /// Would asking for this thumbnail again (at `priority`) change nothing?
    pub fn thumb_current(&self, photo: &Arc<lightcraft_catalog::Photo>, bucket: usize, priority: u32) -> bool {
        let Some(&(ref old, size, key, quick)) = self.thumb_inputs.get(&photo.id) else { return false };
        if old.as_ptr() != Arc::as_ptr(photo) || size != bucket {
            return false;
        }
        let slot = Slot::Thumb(photo.id);
        if quick && !self.textures.contains_key(&slot) {
            // Stand-in path (`grid::request_thumb`): the embedded preview is still on its way, or
            // the real thumbnail is requested at the background priority whatever the caller's.
            let quick_slot = Slot::ThumbQuick(photo.id);
            if let Some(&(k, p)) = self.pending.get(&quick_slot)
                && k == key
            {
                return p == crate::panels::grid::stand_in_priority(priority) || !self.is_queued(quick_slot);
            }
            return !self.request_needed(slot, key, crate::panels::grid::BACKGROUND_THUMB_PRIORITY);
        }
        !self.request_needed(slot, key, priority)
    }

    /// Remember the inputs of the thumbnail just requested (`quick`: through a stand-in).
    /// Whether the camera's embedded preview *is* this photo's thumbnail: its quick render arrived
    /// (so `Slot::ThumbQuick` holds the picture) and no edit has moved the photo on to a settings
    /// hash of its own. The grid stops here instead of decoding the raw behind a thumbnail the
    /// embedded JPEG already covers — see `grid::skips_the_real_render`.
    pub fn thumb_quick_is_final(&self, photo: &Arc<lightcraft_catalog::Photo>, bucket: usize, priority: u32) -> bool {
        let Some((old, size, key)) = self.embedded_thumbs.get(&photo.id) else { return false };
        old.as_ptr() == Arc::as_ptr(photo) && *size == bucket && !self.request_needed(Slot::ThumbQuick(photo.id), *key, priority)
    }

    /// Whether this photo's embedded-preview thumbnail is pending at the requested priority, or
    /// already running. A queued priority change must reach the grid's stand-in request.
    pub fn thumb_quick_pending(&self, photo: &Arc<lightcraft_catalog::Photo>, bucket: usize, priority: u32) -> bool {
        let Some((old, size, key, quick)) = self.thumb_inputs.get(&photo.id) else { return false };
        let slot = Slot::ThumbQuick(photo.id);
        old.as_ptr() == Arc::as_ptr(photo)
            && *size == bucket
            && *quick
            && self.pending.contains_key(&slot)
            && !self.request_needed(slot, *key, crate::panels::grid::stand_in_priority(priority))
    }

    /// Remember that the camera's embedded preview is this photo's final thumbnail, so the grid's
    /// per-frame requests stop at it (the quick render's key, which [`Self::thumb_quick_is_final`]
    /// compares against).
    pub fn remember_embedded_thumb(&mut self, photo: &Arc<lightcraft_catalog::Photo>, bucket: usize, key: u64) {
        let slot = Slot::Thumb(photo.id);
        if self.pending.get(&slot).is_some_and(|(pending_key, _)| *pending_key != key) {
            // An edit can still be rendering when Undo restores this embedded-final thumbnail.
            // Retire its full-render identity before it can replace the correct stand-in.
            self.pending.remove(&slot);
            self.requests.remove(&slot);
            self.pool.reprioritize(|s, p| if *s == slot { None } else { Some(p) });
            self.queue.retain(|q| q.slot != slot);
        }
        self.remember_thumb(photo, bucket, key, true);
        self.embedded_thumbs.insert(photo.id, (Arc::downgrade(photo), bucket, key));
    }

    pub fn remember_thumb(&mut self, photo: &Arc<lightcraft_catalog::Photo>, bucket: usize, key: u64, quick: bool) {
        if self.thumb_inputs.len() >= 1024 && !self.thumb_inputs.contains_key(&photo.id) {
            self.thumb_inputs.retain(|_, (p, _, _, _)| p.strong_count() > 0);
            if self.thumb_inputs.len() >= 1024
                && let Some(id) = self.thumb_inputs.keys().next().copied()
            {
                self.thumb_inputs.remove(&id);
            }
        }
        self.thumb_inputs.insert(photo.id, (Arc::downgrade(photo), bucket, key, quick));
    }

    /// Views that render straight into the display profile's primaries (wide gamut); everything
    /// else is rendered for sRGB and converted ([`lightcraft_engine::display::present`]).
    fn renders_for_display(slot: Slot) -> bool {
        matches!(
            slot,
            Slot::Main
                | Slot::Region
                | Slot::Before
                | Slot::RegionBefore
                | Slot::Hover
                | Slot::Compare(_)
                | Slot::CompareRegion(_)
                | Slot::Second
                | Slot::Slide(_)
        )
    }

    /// `job` as [`Self::request`] would run it for `slot` (its key included): views render for
    /// the display profile in use.
    pub fn job_for(&self, slot: Slot, job: RenderJob) -> RenderJob {
        if Self::renders_for_display(slot) { job.with_display(self.display.clone()) } else { job }
    }

    /// The display profile in use (id).
    pub fn display_id(&self) -> Option<u64> {
        self.display.as_ref().map(|d| d.id())
    }

    /// The display profile textures are made for (`None`: sRGB).
    pub fn display(&self) -> Option<&Arc<lightcraft_engine::display::Display>> {
        self.display.as_ref()
    }

    /// Show previews through `display` from now on (`app.displayProfile`): when it changes, every
    /// texture is made again for the new one.
    pub fn set_display(&mut self, display: Option<Arc<lightcraft_engine::display::Display>>) {
        if display.as_ref().map(|d| d.id()) != self.display_id() {
            self.forget_all();
        }
        self.display = display;
    }

    /// Run `job` and make its image ready for `display` (on the worker thread).
    fn run_for(job: impl FnOnce() -> RenderResult, display: &Option<Arc<lightcraft_engine::display::Display>>) -> Finished {
        let mut r = job();
        if let Some(d) = display {
            lightcraft_engine::display::present(&mut r, d);
        }
        let color = r.rendered.as_ref().ok().map(|x| Arc::new(color_image(&x.image)));
        Finished { result: r, color }
    }

    fn request_needed(&self, slot: Slot, key: u64, priority: u32) -> bool {
        // A matching old texture must not mask a different newer request (e.g. undo).
        if let Some(&(k, p)) = self.pending.get(&slot) {
            return k != key || (p != priority && self.is_queued(slot));
        }
        if self.textures.get(&slot).is_some_and(|t| t.key == key) || self.failed.get(&slot).is_some_and(|f| f.0 == key && f.2 == self.catalog_rev) {
            return false;
        }
        true
    }
    /// Free a view slot's texture and stage cache (the view is no longer shown).
    pub fn release(&mut self, slot: Slot) {
        self.textures.remove(&slot);
        self.stages.remove(&slot);
    }

    /// Run jobs through `offload` from now on (instead of the job pool / inline).
    pub fn set_offload(&mut self, offload: Box<dyn RenderOffload>) {
        self.offload = Some(offload);
    }

    fn is_queued(&self, slot: Slot) -> bool {
        self.queue.iter().any(|q| q.slot == slot) || self.pool.is_queued(slot)
    }

    /// Is the slot's current texture (or pending request) already for `key`?
    /// Key of what `slot` shows or is rendering next (the pending request wins).
    pub fn wanted(&self, slot: Slot) -> Option<u64> {
        self.pending.get(&slot).map(|p| p.0).or_else(|| self.textures.get(&slot).map(|t| t.key))
    }

    pub fn is_current(&self, slot: Slot, key: u64) -> bool {
        self.wanted(slot) == Some(key)
    }

    /// Request a render for `slot` (no-op if already current or pending at the same priority).
    pub fn request(&mut self, slot: Slot, job: RenderJob, priority: u32) {
        if matches!(slot, Slot::Slide(_)) {
            self.slides_live = true;
        }
        let mut job = self.job_for(slot, job);
        if !self.request_needed(slot, job.key, priority) {
            return;
        }
        self.pending.insert(slot, (job.key, priority));
        job.request_id = lightcraft_preview::next_tick();
        self.requests.insert(slot, Requested { id: job.request_id, source: Some(job.origin.clone()) });
        // (an offload keeps its own per-view stage caches; the flag tells it to)
        let job = if slot.is_view() { job.with_stages(self.stages.entry(slot).or_default().clone()) } else { job };
        if self.offload.is_some() {
            self.seq += 1;
            self.queue.retain(|q| q.slot != slot);
            self.queue.push(Queued { slot, priority, seq: self.seq, job });
            self.dispatch(); // an idle worker starts now rather than next frame
            return;
        }
        let key = job.key;
        let background = matches!(slot, Slot::Thumb(_) | Slot::ThumbQuick(_) | Slot::Prefetch(_));
        let display = self.display.clone();
        self.pool.submit(
            slot,
            key,
            priority,
            Box::new(move || {
                let run = || if background { lightcraft_engine::memory::in_background(|| job.run()) } else { job.run() };
                Self::run_for(run, &display)
            }),
        );
    }

    /// Request a stand-in for `slot` (once per job key). The loupe's [`Slot::Preview`] takes over
    /// a stand-in already made ahead for the same photo ([`Slot::QuickAhead`]) instead of making it
    /// again: shown in this frame when it is ready, or as soon as that job finishes.
    pub fn request_quick(&mut self, slot: Slot, mut job: QuickJob, priority: u32) {
        if self.quick_tried.get(&slot) == Some(&job.key) {
            if self.pending.get(&slot).is_some_and(|p| p.0 == job.key && p.1 != priority) {
                // Scrolling changes a stand-in's priority too. Keep the original closure and
                // request identity; running or completed work must never be started again.
                let mut queued = false;
                self.pool.reprioritize(|s, p| {
                    if *s == slot {
                        queued = true;
                        Some(priority)
                    } else {
                        Some(p)
                    }
                });
                if queued && let Some(p) = self.pending.get_mut(&slot) {
                    p.1 = priority;
                }
            }
            return;
        }
        self.quick_tried.insert(slot, job.key);
        if matches!(slot, Slot::Preview | Slot::QuickAhead(_)) {
            self.quick_edge.insert(slot, quick_edge(&job));
        }
        if slot == Slot::Preview && self.take_ahead(job.photo, job.key, quick_edge(&job)) {
            self.pending.remove(&slot);
            self.requests.remove(&slot);
            return;
        }
        self.pending.insert(slot, (job.key, priority));
        job.request_id = lightcraft_preview::next_tick();
        let source = job
            .small
            .as_ref()
            .map(|job| job.origin.clone())
            .or_else(|| job.embedded.as_ref().map(|(path, _, _)| lightcraft_catalog::Source::File { path: path.clone() }));
        self.requests.insert(slot, Requested { id: job.request_id, source });
        let key = job.key;
        let display = self.display.clone();
        // Fast camera-JPEG workers must not wait behind background RAW memory reservations.
        self.pool.submit(slot, key, priority, Box::new(move || Self::run_for(|| job.run(), &display)));
    }

    /// Whether the stand-in asked for in `slot` is at least `edge` pixels (see `quick_edge`).
    fn ahead_covers(&self, slot: Slot, edge: usize) -> bool {
        self.quick_edge.get(&slot).copied().unwrap_or(0) >= edge
    }

    /// Move a [`Slot::QuickAhead`] texture of `photo` made for `key`, at least `edge` pixels, into
    /// [`Slot::Preview`].
    fn take_ahead(&mut self, photo: PhotoId, key: u64, edge: usize) -> bool {
        let Some(slot) = self
            .textures
            .iter()
            .find(|(s, t)| matches!(s, Slot::QuickAhead(_)) && t.photo == photo && t.key == key && self.ahead_covers(**s, edge))
            .map(|(s, _)| *s)
        else {
            return false;
        };
        let Some(tex) = self.textures.remove(&slot) else { return false };
        self.quick_tried.remove(&slot);
        self.textures.insert(Slot::Preview, tex);
        true
    }

    /// Make the loupe's stand-ins for the photos it is likely to be stepped to next (see
    /// [`Slot::QuickAhead`]), most likely first: `jobs[0]` gets `priority`, each next one less.
    /// A slot keeps a photo for as long as the photo stays wanted (made or being made, it is not
    /// made again when the user steps and the photos shift places); slots holding photos not
    /// wanted any more are freed for the new ones, and their queued jobs dropped.
    pub fn look_ahead(&mut self, jobs: Vec<QuickJob>, priority: u32) {
        let slots: Vec<Slot> = (0..AHEAD_SLOTS).map(Slot::QuickAhead).collect();
        let held = |r: &Self, s: Slot| r.textures.get(&s).map(|t| t.key).or_else(|| r.quick_tried.get(&s).copied());
        // (a stand-in asked for at a smaller loupe size isn't kept for a larger one)
        let wanted: HashMap<u64, usize> = jobs.iter().map(|j| (j.key, quick_edge(j))).collect();
        let mut free = Vec::new();
        for &s in &slots {
            let key = held(self, s);
            if key.is_none_or(|k| wanted.get(&k).is_none_or(|&edge| !self.ahead_covers(s, edge))) {
                self.textures.remove(&s);
                // a job already running: its result is thrown away when it arrives (not left as a
                // hidden stand-in), unless the loupe is waiting for just that one (a step to the
                // photo): then the slot stays reserved for it until it arrives
                let preview_edge = self.quick_edge.get(&Slot::Preview).copied().unwrap_or(0);
                let loupe_waits =
                    key.is_some_and(|k| self.pending.get(&Slot::Preview).is_some_and(|p| p.0 == k)) && self.ahead_covers(s, preview_edge);
                if !self.drop_queued(s) && loupe_waits {
                    continue;
                }
                self.quick_tried.remove(&s);
                self.pending.remove(&s);
                self.requests.remove(&s);
                free.push(s);
            }
        }
        let have: HashMap<u64, Slot> = slots.iter().filter_map(|&s| held(self, s).map(|k| (k, s))).collect();
        let mut free = free.into_iter();
        for (rank, job) in jobs.into_iter().enumerate() {
            let priority = priority.saturating_sub(rank as u32);
            if let Some(&s) = have.get(&job.key) {
                // kept: a job still queued moves to its new place in the order
                if self.quick_tried.get(&s) == Some(&job.key) {
                    self.request_quick(s, job, priority);
                }
                continue;
            }
            let Some(slot) = free.next() else { break };
            self.request_quick(slot, job, priority);
        }
    }

    /// Render a slideshow title screen into `slot` (once per screen key, like [`Self::request`]):
    /// a slide with no photo, composed on a worker (on wasm: the job pool's inline runs, not the
    /// offload, which only knows photo renders).
    pub fn request_title(&mut self, slot: Slot, compose: Arc<lightcraft_engine::slide_title::TitleCompose>, priority: u32) {
        if matches!(slot, Slot::Slide(_)) {
            self.slides_live = true;
        }
        let key = compose.key();
        if !self.request_needed(slot, key, priority) {
            return;
        }
        self.pending.insert(slot, (key, priority));
        let request_id = lightcraft_preview::next_tick();
        self.requests.insert(slot, Requested { id: request_id, source: None });
        let display = self.display.clone();
        self.pool.submit(slot, key, priority, Box::new(move || Self::run_for(|| compose.result(request_id), &display)));
    }

    /// Prepare a photo in the background (a [`Slot::Prefetch`] job runs once per key; a newer one
    /// replaces it while queued). Memory stays bounded by the engine's source caches.
    pub fn prefetch(&mut self, slot: Slot, mut job: RenderJob, priority: u32) {
        if self.prefetched.get(&slot) == Some(&job.key) {
            return;
        }
        self.prefetched.insert(slot, job.key);
        self.pending.insert(slot, (job.key, priority));
        job.request_id = lightcraft_preview::next_tick();
        self.requests.insert(slot, Requested { id: job.request_id, source: Some(job.origin.clone()) });
        let key = job.key;
        self.pool.submit(
            slot,
            key,
            priority,
            Box::new(move || Finished { result: lightcraft_engine::memory::in_background(|| job.run()), color: None }),
        );
    }

    /// Prepared-ahead jobs ([`Slot::Prefetch`]) queued or running.
    pub fn prefetches_pending(&self) -> usize {
        self.pending.keys().filter(|s| matches!(s, Slot::Prefetch(_))).count()
    }

    /// Prepared-ahead slots asked for since the renderer last forgot everything.
    pub fn prefetch_slots_asked(&self) -> usize {
        self.prefetched.keys().filter(|s| matches!(s, Slot::Prefetch(_))).count()
    }

    /// Whether `slot` was last asked for the job of `key` (queued, running or done).
    pub fn prefetch_asked(&self, slot: Slot, key: u64) -> bool {
        self.prefetched.get(&slot) == Some(&key)
    }

    /// Drop the slideshow slides other than `keep`: their textures and their requests that have
    /// not started (a slide that is no longer near is not worth rendering).
    pub fn release_slides(&mut self, keep: &[lightcraft_engine::slideshow::Item]) {
        if !self.slides_live {
            return;
        }
        let gone = |s: &Slot| matches!(s, Slot::Slide(id) if !keep.contains(id));
        self.textures.retain(|s, _| !gone(s));
        let _ = self.pool.reprioritize(|s, p| if gone(s) { None } else { Some(p) });
        self.queue.retain(|q| !gone(&q.slot));
        // (a render already running finishes unclaimed: its result is dropped)
        self.pending.retain(|s, _| !gone(s));
        self.requests.retain(|s, _| !gone(s));
        self.failed.retain(|s, _| !gone(s));
        self.slides_live = !keep.is_empty();
    }

    /// Is a request for `slot` queued or running?
    /// Drop every texture and cached stage (another library was opened: photo ids changed meaning).
    pub fn forget_all(&mut self) {
        self.requests.clear();
        self.shown_ids.clear();
        self.epoch = lightcraft_preview::next_tick();
        self.preview_generation = None;
        self.pool.reprioritize(|_, _| None);
        self.thumb_inputs.clear();
        self.textures.clear();
        self.stages.clear();
        self.quick_tried.clear();
        self.failed.clear();
        self.prefetched.clear();
        self.pending.clear();
        self.queue.clear();
    }

    pub fn is_pending(&self, slot: Slot) -> bool {
        self.pending.contains_key(&slot)
    }

    /// The app is closing: drop the jobs that have not started and wait up to `timeout` for the
    /// ones running on the worker threads. Returns whether all of them have ended. Nothing is
    /// rendered afterwards.
    pub fn shutdown(&mut self, timeout: std::time::Duration) -> bool {
        self.queue.clear();
        self.pool.shutdown(timeout)
    }

    /// Worker threads that are still alive (0 before the first render).
    pub fn live_workers(&self) -> usize {
        self.pool.live_workers()
    }

    /// The texture to show for a grid/filmstrip thumbnail: the rendered one, else its stand-in.
    pub fn thumb(&self, id: PhotoId) -> Option<&Tex> {
        self.textures.get(&Slot::Thumb(id)).or_else(|| self.textures.get(&Slot::ThumbQuick(id)))
    }

    pub fn queued(&self) -> usize {
        self.pool.queued() + self.queue.len()
    }

    /// Hand queued jobs to the offload (best first) while it has room.
    fn dispatch(&mut self) {
        let Some(off) = self.offload.as_mut() else { return };
        while let Some(i) = self.queue.iter().enumerate().max_by_key(|(_, q)| (q.priority, q.seq)).map(|(i, _)| i) {
            let q = self.queue.swap_remove(i);
            if let Some(job) = off.try_start(q.slot, q.job) {
                self.queue.push(Queued { job, ..q });
                break;
            }
        }
    }

    /// Requests queued or running.
    pub fn in_flight(&self) -> usize {
        self.pending.len()
    }

    /// Why the last render for `slot` failed (until a render with another key succeeds).
    pub fn failure(&self, slot: Slot) -> Option<&str> {
        self.failed.get(&slot).map(|f| f.1.as_str())
    }

    /// Why the last render of `photo` for `slot` failed: a view slot shows one photo after
    /// another, and another photo's error isn't this one's.
    pub fn failure_of(&self, slot: Slot, photo: PhotoId) -> Option<&str> {
        self.failed.get(&slot).filter(|f| f.3 == photo).map(|f| f.1.as_str())
    }

    /// Whether `slot`'s pending render asks again for the key that last failed (a library change
    /// retries failures): what is on screen stays what the failure left until it answers.
    pub fn retrying(&self, slot: Slot) -> bool {
        self.pending.get(&slot).zip(self.failed.get(&slot)).is_some_and(|(p, f)| p.0 == f.0)
    }

    /// The slots with a render pending (diagnostics: `ui.inspect`).
    pub fn pending_slots(&self) -> Vec<String> {
        self.pending.keys().map(|s| format!("{s:?}")).collect()
    }

    /// Thumbnail textures currently loaded.
    /// A variant thumbnail ([`Slot::Variant`]): its texture once rendered; until then the job is
    /// queued at low priority (below on-screen grid thumbnails). Call it every frame the variant
    /// is visible: variants not asked for during the last frames leave the queue, and textures
    /// beyond [`VARIANT_TEXTURES`] are evicted least recently used first.
    pub fn variant(&mut self, job: RenderJob) -> Option<&Tex> {
        let key = job.key;
        let slot = Slot::Variant(key);
        self.variant_used.insert(key, self.frame);
        if !self.textures.contains_key(&slot) {
            self.request(slot, job, VARIANT_PRIORITY);
        }
        self.textures.get(&slot)
    }

    /// A view that shows `n` variants at once (the People view's faces) says so each frame, so the cache holds them all:
    /// with a fixed budget a screen of more than [`VARIANT_TEXTURES`] pictures evicts, and re-requests, the same few every
    /// frame, and those tiles stay blank. The budget is the most asked for in the last frame, a quarter more, up to
    /// [`MAX_VARIANT_TEXTURES`].
    pub fn want_variants(&mut self, n: usize) {
        self.variant_want = self.variant_want.max(n);
    }

    /// How many variant textures are kept now.
    fn variant_budget(&self) -> usize {
        VARIANT_TEXTURES.max(self.variant_want + self.variant_want / 4).min(MAX_VARIANT_TEXTURES)
    }

    /// Variant thumbnail textures currently loaded.
    pub fn variant_textures(&self) -> usize {
        self.textures.keys().filter(|s| matches!(s, Slot::Variant(_))).count()
    }

    /// Drop variant jobs nobody asked for in the last frames, and the least recently used
    /// variant textures beyond the budget.
    fn evict_variants(&mut self) {
        let now = self.frame;
        let stale = |used: Option<&u64>| used.is_none_or(|f| now.saturating_sub(*f) > 2);
        let used = &self.variant_used;
        let dropped = self.pool.reprioritize(|s, p| match s {
            Slot::Variant(k) if stale(used.get(k)) => None,
            _ => Some(p),
        });
        for s in dropped {
            self.pending.remove(&s);
            self.requests.remove(&s);
        }
        let requests = &mut self.requests;
        let pending = &mut self.pending;
        self.queue.retain(|q| match q.slot {
            Slot::Variant(k) if stale(used.get(&k)) => {
                pending.remove(&q.slot);
                requests.remove(&q.slot);
                false
            }
            _ => true,
        });
        let budget = self.variant_budget();
        self.variant_want = 0;
        let n = self.variant_textures();
        if n > budget {
            let mut have: Vec<(u64, u64)> = self
                .textures
                .keys()
                .filter_map(|s| if let Slot::Variant(k) = s { Some((self.variant_used.get(k).copied().unwrap_or(0), *k)) } else { None })
                .collect();
            have.sort_unstable();
            for (_, k) in have.into_iter().take(n - budget) {
                self.textures.remove(&Slot::Variant(k));
                self.variant_used.remove(&k);
            }
        }
        if self.variant_used.len() > 4 * budget {
            let textures = &self.textures;
            let pending = &self.pending;
            self.variant_used.retain(|k, _| textures.contains_key(&Slot::Variant(*k)) || pending.contains_key(&Slot::Variant(*k)));
        }
    }

    /// Frames polled so far (one a frame).
    pub fn frame(&self) -> u64 {
        self.frame
    }

    pub fn thumb_textures(&self) -> usize {
        self.textures.keys().filter(|s| matches!(s, Slot::Thumb(_) | Slot::ThumbQuick(_))).count()
    }

    /// Collect finished jobs into textures. Returns true if anything changed.
    pub fn poll(&mut self, ctx: &egui::Context, session: &mut Session) -> bool {
        let cache = &session.media.rendered;
        let generation = cache.generation();
        if self.preview_generation.as_ref().is_some_and(|(old, g)| old.as_ptr() != Arc::as_ptr(cache) || *g != generation) {
            self.forget_all();
        }
        self.preview_generation = Some((Arc::downgrade(cache), generation));
        self.catalog_rev = session.catalog.revision;
        self.frame += 1;
        self.trim_stages();
        // wasm: run one job per frame on this thread, timed with the host clock
        #[cfg(target_arch = "wasm32")]
        let inline_ms = {
            let t0 = crate::now_ms();
            self.pool.run_inline(1);
            crate::now_ms() - t0
        };
        #[cfg(not(target_arch = "wasm32"))]
        let inline_ms = 0.0;
        let changed = self.collect(ctx, session, inline_ms);
        self.evict_variants();
        #[cfg(not(target_arch = "wasm32"))]
        self.trim_when_idle(ctx);
        if !self.pending.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        changed
    }

    /// In a slider drag, give the loupe's draft (just requested) until `budget` to finish, so that it
    /// is drawn in this frame rather than picked up at the start of the next one. Only when the last
    /// loupe render took well under the budget (a GPU draft takes a few milliseconds; a CPU render
    /// that would only make the frame late is not waited for).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn wait_for_draft(&mut self, ctx: &egui::Context, session: &mut Session, budget: std::time::Duration) {
        if self.offload.is_some() || self.last_main_ms * 1.5 >= budget.as_secs_f64() * 1e3 {
            return;
        }
        let Some(deadline) = std::time::Instant::now().checked_add(budget) else { return };
        while self.is_pending(Slot::Main) {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() || !self.pool.wait(left) {
                break;
            }
            self.collect(ctx, session, 0.0);
        }
    }

    /// Turn finished jobs into textures. Returns true if anything changed.
    fn collect(&mut self, ctx: &egui::Context, session: &mut Session, inline_ms: f64) -> bool {
        let mut finished = Vec::new();
        while let Some(done) = self.pool.try_recv() {
            finished.push((done.slot, done.result.result, done.result.color, if done.ms > 0.0 { done.ms } else { inline_ms }));
        }
        if let Some(off) = self.offload.as_mut() {
            finished.extend(off.finished().into_iter().map(|(slot, r, ms)| (slot, r, None, ms)));
        }
        self.dispatch();
        let mut changed = false;
        for (mut slot, mut r, mut color, ms) in finished {
            self.completed += 1;
            // made for another display profile than the one in use: obsolete
            if r.display.is_some_and(|id| Some(id) != self.display_id()) {
                continue;
            }
            // Ignore obsolete pixels, failures and decoded sources, even for the same render key;
            // an interactive view still takes a superseded draft that is newer than what it
            // shows, and the source it decoded: a drag supersedes every request before it
            // finishes, and a source thrown away with them is decoded again for every tick.
            let source = if self.requests.get(&slot).map(|request| request.id) != Some(r.request_id) {
                let newer_draft = slot.is_view()
                    && self.requests.contains_key(&slot)
                    && r.request_id >= self.epoch
                    && self.shown_ids.get(&slot).is_none_or(|shown| r.request_id > *shown)
                    && r.rendered.is_ok();
                if !newer_draft {
                    self.discarded += 1;
                    continue;
                }
                session.accept(&r);
                None // superseded successful drafts have no failure diagnostic
            } else {
                let source = self.requests.remove(&slot).and_then(|request| request.source);
                session.accept(&r);
                source
            };
            if slot.is_view() {
                self.shown_ids.insert(slot, r.request_id);
            }
            // (results from an offload, which doesn't know the display profile: converted here)
            if let Some(d) = &self.display {
                let before = r.display;
                lightcraft_engine::display::present(&mut r, d);
                if r.display != before {
                    color = None; // converted after the worker made its texture pixels
                }
            }
            if self.pending.get(&slot).is_some_and(|p| p.0 == r.key) {
                self.pending.remove(&slot);
            }
            // made ahead for the photo the loupe is now waiting for: it is the loupe's stand-in
            // (the loupe's own request for it, if one was started, is dropped when it arrives)
            // (also when the loupe's own request failed and it still wants that photo: the stand-in
            // made ahead still shows, but never over another photo's request)
            if matches!(slot, Slot::QuickAhead(_))
                && r.rendered.is_ok()
                && (self.pending.get(&Slot::Preview).is_some_and(|p| p.0 == r.key)
                    || (!self.pending.contains_key(&Slot::Preview)
                        && self.quick_tried.get(&Slot::Preview) == Some(&r.key)
                        && self.failed.get(&Slot::Preview).is_some_and(|f| f.0 == r.key && f.3 == r.photo)))
                && self.ahead_covers(slot, self.quick_edge.get(&Slot::Preview).copied().unwrap_or(0))
            {
                self.pending.remove(&Slot::Preview);
                self.requests.remove(&Slot::Preview);
                self.quick_tried.remove(&slot);
                slot = Slot::Preview;
            }
            let rendered = match r.rendered {
                Ok(x) => {
                    self.failed.remove(&slot);
                    x
                }
                Err(e) => {
                    if !matches!(slot, Slot::Prefetch(_) | Slot::QuickAhead(_)) {
                        let message = render_failure_message(slot, r.photo, source.as_ref(), &e);
                        #[cfg(test)]
                        self.failure_messages.push(message.clone());
                        log::warn!("{message}");
                        self.failed.insert(slot, (r.key, e, self.catalog_rev, r.photo));
                    }
                    continue;
                }
            };
            if matches!(slot, Slot::Prefetch(_)) {
                continue;
            }
            if slot == Slot::HdrHistogram {
                if rendered.histogram.hdr.is_some() {
                    self.live_hdr_histogram = Some((r.photo, r.key, rendered.histogram));
                    changed = true;
                }
                continue;
            }
            if let Slot::ThumbQuick(id) = slot {
                if self.textures.contains_key(&Slot::Thumb(id)) {
                    continue; // the real thumbnail won the race
                }
                if r.quick == Some(QuickSource::Cached) {
                    // the photo's own cached thumbnail: final
                    slot = Slot::Thumb(id);
                }
            }
            // (a photo's first thumbnail: not a render replacing its stand-in)
            let first_thumb = match slot {
                Slot::Thumb(id) | Slot::ThumbQuick(id) => {
                    !self.textures.contains_key(&Slot::Thumb(id)) && !self.textures.contains_key(&Slot::ThumbQuick(id))
                }
                _ => false,
            };
            if let Slot::Thumb(id) = slot {
                self.textures.remove(&Slot::ThumbQuick(id));
            }
            let img = &rendered.image;
            let color = color.unwrap_or_else(|| Arc::new(color_image(img)));
            let pixels = self.keep_pixels.then(|| color.clone());
            let name = format!("{slot:?}");
            match self.textures.get_mut(&slot) {
                Some(t) => {
                    t.tex.set(color, texture_options(slot));
                    t.pixels = pixels;
                    t.key = r.key;
                    t.photo = r.photo;
                    t.size = [img.width, img.height];
                    t.histogram = Some(rendered.histogram);
                    t.ms = ms;
                    t.quick = r.quick;
                }
                None => {
                    let tex = ctx.load_texture(name, color, texture_options(slot));
                    self.textures.insert(
                        slot,
                        Tex {
                            key: r.key,
                            photo: r.photo,
                            tex,
                            size: [img.width, img.height],
                            histogram: Some(rendered.histogram),
                            ms,
                            quick: r.quick,
                            pixels,
                        },
                    );
                }
            }
            if first_thumb {
                self.thumbs_made += 1;
            }
            // (a preview taken from the cache says nothing about how long a render takes)
            if slot == Slot::Main && r.quick.is_none() {
                self.last_main_ms = ms;
            }
            changed = true;
        }
        changed
    }

    /// Free the GPU renderer's recycled buffers once nothing has rendered for [`IDLE_TRIM`] (they
    /// are reallocated by the next render; while working, renders reuse them). A zoomed view
    /// showing its windows is not idle: the device copy of the original they are cut from stays for
    /// the next pan (a 64 MP raw is 800 MB to upload again, over a second).
    #[cfg(not(target_arch = "wasm32"))]
    fn trim_when_idle(&mut self, ctx: &egui::Context) {
        if !self.pending.is_empty() || self.textures.contains_key(&Slot::Region) || self.textures.contains_key(&Slot::RegionBefore) {
            self.idle = None;
            return;
        }
        let now = std::time::Instant::now();
        let (since, trimmed) = *self.idle.get_or_insert((now, false));
        if trimmed {
            return;
        }
        let waited = now.duration_since(since);
        if waited >= IDLE_TRIM {
            lightcraft_engine::gpu::trim_pool(0);
            lightcraft_engine::memory::release();
            self.idle = Some((since, true));
        } else {
            ctx.request_repaint_after(IDLE_TRIM - waited);
        }
    }

    /// Bytes the per-view stage caches (CPU images and GPU buffers) may hold together.
    pub fn stage_budget(&self) -> usize {
        #[cfg(test)]
        if let Some(b) = self.stage_budget_override {
            return b;
        }
        lightcraft_engine::memory::budget() / STAGE_BUDGET_SHARE
    }

    /// Clear stage caches, least useful first, until what they hold fits [`Self::stage_budget`].
    fn trim_stages(&mut self) {
        let sizes: Vec<(Slot, usize)> = self.stages.iter().map(|(slot, c)| (*slot, c.bytes() + lightcraft_engine::gpu::stage_bytes(c))).collect();
        for slot in stage_trim_order(&sizes, self.stage_budget()) {
            if let Some(c) = self.stages.get(&slot) {
                c.clear();
                self.stages_trimmed += 1;
            }
        }
    }

    /// What the renderer holds: per-view stage caches (CPU images and GPU buffers) and textures.
    pub fn memory(&self) -> serde_json::Value {
        let cpu: usize = self.stages.values().map(|s| s.bytes()).sum();
        let gpu: usize = self.stages.values().map(|s| lightcraft_engine::gpu::stage_bytes(s)).sum();
        let tex: usize = self.textures.values().map(|t| t.size[0] * t.size[1] * 4).sum();
        let copies: usize = self.textures.values().filter_map(|t| t.pixels.as_ref()).map(|p| p.pixels.len() * 4).sum();
        serde_json::json!({
            "stageCaches": {"count": self.stages.len(), "cpuBytes": cpu, "gpuBytes": gpu, "budgetBytes": self.stage_budget(), "trimmed": self.stages_trimmed, "sharedSourceBytes": lightcraft_engine::gpu::shared_source_bytes()},
            "textures": {"count": self.textures.len(), "bytes": tex, "cpuCopyBytes": copies},
        })
    }

    /// CPU copies of the current textures by id (see [`Self::keep_pixels`]).
    pub fn cpu_textures(&self) -> HashMap<egui::TextureId, crate::softpaint::CpuTexture> {
        self.textures
            .iter()
            .filter_map(|(slot, t)| {
                let options = texture_options(*slot);
                Some((
                    t.tex.id(),
                    crate::softpaint::CpuTexture {
                        image: t.pixels.clone()?,
                        magnification: options.magnification,
                        minification: options.minification,
                    },
                ))
            })
            .collect()
    }

    /// Forget everything about `slot`: its texture, its queued job, and that its stand-in was asked
    /// for (so asking again makes it again).
    pub fn forget(&mut self, slot: Slot) {
        self.drop_queued(slot);
        self.pending.remove(&slot);
        self.requests.remove(&slot);
        self.quick_tried.remove(&slot);
        self.release(slot);
    }

    /// Drop `slot`'s job if it is still queued (a running one finishes). Returns whether one was.
    pub fn drop_queued(&mut self, slot: Slot) -> bool {
        let dropped = self.pool.reprioritize(|s, p| if *s == slot { None } else { Some(p) });
        let before = self.queue.len();
        self.queue.retain(|q| q.slot != slot);
        let any = !dropped.is_empty() || self.queue.len() != before;
        if any {
            self.pending.remove(&slot);
            self.requests.remove(&slot);
        }
        any
    }

    /// Drop the import review thumbnails (a new review, or the dialog closed).
    pub fn forget_imports(&mut self) {
        self.pool.reprioritize(|s, p| if matches!(s, Slot::Import(_)) { None } else { Some(p) });
        self.queue.retain(|q| !matches!(q.slot, Slot::Import(_)));
        self.pending.retain(|s, _| !matches!(s, Slot::Import(_)));
        self.requests.retain(|s, _| !matches!(s, Slot::Import(_)));
        self.textures.retain(|s, _| !matches!(s, Slot::Import(_)));
        self.quick_tried.retain(|s, _| !matches!(s, Slot::Import(_)));
    }

    /// Drop thumbnails that aren't in `keep` (bounded memory for huge libraries), and queued
    /// thumbnail jobs for photos that scrolled out of `keep`.
    pub fn evict_thumbs(&mut self, keep: &std::collections::HashSet<PhotoId>, max: usize) {
        self.thumb_inputs.retain(|id, _| keep.contains(id));
        let dropped = self.pool.reprioritize(|s, p| match s {
            Slot::Thumb(id) | Slot::ThumbQuick(id) if !keep.contains(id) && (p <= 11 || p == crate::panels::grid::STAND_IN_ON_SCREEN) => None,
            _ => Some(p),
        });
        for s in dropped {
            self.pending.remove(&s);
            self.requests.remove(&s);
            self.quick_tried.remove(&s);
        }
        let requests = &mut self.requests;
        let pending = &mut self.pending;
        self.queue.retain(|q| match q.slot {
            Slot::Thumb(id) if !keep.contains(&id) && q.priority <= 10 => {
                pending.remove(&q.slot);
                requests.remove(&q.slot);
                false
            }
            _ => true,
        });
        let thumbs = self.thumb_textures();
        if thumbs <= max {
            return;
        }
        self.textures.retain(|s, _| !matches!(s, Slot::Thumb(id) | Slot::ThumbQuick(id) if !keep.contains(id)));
        self.quick_tried.retain(|s, _| !matches!(s, Slot::ThumbQuick(id) if !keep.contains(id)));
    }
}

/// Include the source identity in diagnostics for every catalog-backed render slot (#1890).
/// Debug-format paths so whitespace/control characters cannot forge another log entry.
fn render_failure_message(slot: Slot, photo: PhotoId, source: Option<&lightcraft_catalog::Source>, error: &str) -> String {
    match source {
        Some(lightcraft_catalog::Source::File { path }) => format!("render {slot:?}, {photo:?}, file {path:?}: {error}"),
        Some(lightcraft_catalog::Source::Demo { scene }) => format!("render {slot:?}, {photo:?}, demo scene {scene}: {error}"),
        None => format!("render {slot:?}, {photo:?}, source unavailable: {error}"),
    }
}

#[cfg(test)]
mod thumbnail_tests {
    use super::*;
    use lightcraft_catalog::{Catalog, Op, Photo, Source};

    #[test]
    fn render_failures_identify_the_file_for_thumbnail_and_view_slots() {
        let source = Source::File { path: "/photos/trip/IMG_0042.nef".into() };
        for slot in [Slot::Thumb(PhotoId(42)), Slot::Main, Slot::Before, Slot::Compare(1), Slot::Region] {
            let message = render_failure_message(slot, PhotoId(42), Some(&source), "unsupported compression");
            assert!(message.contains("/photos/trip/IMG_0042.nef"), "{message}");
            assert!(message.contains("PhotoId(42)"), "{message}");
            assert!(message.ends_with(": unsupported compression"), "{message}");
        }
    }

    #[test]
    fn render_failure_paths_are_escaped_and_missing_sources_still_identified() {
        let source = Source::File { path: "line\nbreak.nef".into() };
        let message = render_failure_message(Slot::Main, PhotoId(42), Some(&source), "decode failed");
        assert!(!message.contains('\n'));
        assert!(message.contains("line\\nbreak.nef"));
        assert!(render_failure_message(Slot::Main, PhotoId(42), None, "decode failed").contains("PhotoId(42), source unavailable"));
        let demo = Source::Demo { scene: 7 };
        assert!(render_failure_message(Slot::Main, PhotoId(42), Some(&demo), "decode failed").contains("demo scene 7"));
    }

    struct Blocked;
    impl RenderOffload for Blocked {
        fn try_start(&mut self, _: Slot, job: RenderJob) -> Option<RenderJob> {
            Some(job)
        }
        fn finished(&mut self) -> Vec<(Slot, RenderResult, f64)> {
            vec![]
        }
    }
    fn photo() -> Arc<Photo> {
        Arc::new(Photo::new(PhotoId(1), Source::File { path: "audit.jpg".into() }, "audit.jpg", "JPEG", 6000, 4000, "2026-01-01"))
    }
    #[test]
    fn snapshot_tracks_cow_edits_undo_reload_and_replacement() {
        let mut catalog = Catalog::default();
        catalog.apply(Op::AddPhoto { photo: Box::new((*photo()).clone()) }).unwrap();
        let old = catalog.photo(PhotoId(1)).unwrap().clone();
        let mut r = Renderer::default();
        r.remember_thumb(&old, 384, 77, false);
        r.pending.insert(Slot::Thumb(PhotoId(1)), (77, 10));
        assert!(r.thumb_current(&old, 384, 10));
        assert!(!r.thumb_current(&old, 512, 10));
        let mut settings = lightcraft_develop::DevelopSettings::default();
        settings.light.exposure = 1.0;
        let undo = catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(settings), label: "edit".into(), edited: None }).unwrap();
        assert!(!r.thumb_current(catalog.photo(PhotoId(1)).unwrap(), 384, 10));
        catalog.apply(undo).unwrap();
        assert!(!r.thumb_current(catalog.photo(PhotoId(1)).unwrap(), 384, 10));
        catalog
            .apply(Op::SetContent {
                id: PhotoId(1),
                width: 6000,
                height: 4000,
                file_size: 99,
                content_hash: Some("changed".into()),
                preview_only: None,
                preview_checked: String::new(),
            })
            .unwrap();
        assert!(!r.thumb_current(catalog.photo(PhotoId(1)).unwrap(), 384, 10));
        assert!(!r.thumb_current(&photo(), 384, 10));
        r.forget_all();
        assert!(r.thumb_inputs.is_empty());
    }
    #[test]
    fn queue_promotions_running_work_and_cancellation() {
        let p = photo();
        let slot = Slot::Thumb(p.id);
        let mut session = Session::new();
        session.catalog.apply(Op::AddPhoto { photo: Box::new((*p).clone()) }).unwrap();
        let job = session.thumb_job(p.id, 384).unwrap();
        let key = job.key;
        let mut r = Renderer::default();
        r.set_offload(Box::new(Blocked));
        r.remember_thumb(&p, 384, key, false);
        r.request(slot, job, 5);
        assert!(r.thumb_current(&p, 384, 5));
        assert!(!r.thumb_current(&p, 384, 10));
        r.request(slot, session.thumb_job(p.id, 384).unwrap(), 10);
        assert_eq!(r.pending.get(&slot), Some(&(key, 10)));
        r.queue.clear(); // running work: cannot reprioritize, just as Renderer::request
        assert!(r.thumb_current(&p, 384, 5));
        r.pending.clear();
        assert!(!r.thumb_current(&p, 384, 10));
        r.evict_thumbs(&Default::default(), 600);
        assert!(r.thumb_inputs.is_empty());
    }
    fn queued_quick_thumbnail(id: PhotoId) -> (Arc<Photo>, QuickJob) {
        let mut session = Session::new();
        session.media.preview_loader =
            Some(Arc::new(|_, _| Some(lightcraft_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })));
        let mut raw = (*photo()).clone();
        raw.id = id;
        raw.kind = lightcraft_catalog::MediaKind::Raw;
        raw.develop = Arc::new(raw.camera_defaults());
        session.catalog.apply(Op::AddPhoto { photo: Box::new(raw) }).unwrap();
        let p = session.catalog.photo(id).unwrap().clone();
        let job = session.thumb_job(id, 384).unwrap();
        (p, session.quick_thumb_job(&job).unwrap())
    }

    #[test]
    fn queued_standin_promotes_from_prefetch_to_visible_without_restarting() {
        let (p, quick) = queued_quick_thumbnail(PhotoId(1));
        let (_, other) = queued_quick_thumbnail(PhotoId(2));
        let slot = Slot::ThumbQuick(p.id);
        // Deterministic queue: no native worker starts.
        let mut r = Renderer { pool: JobPool::new(0), ..Default::default() };
        r.remember_thumb(&p, 384, quick.key, true);
        r.request_quick(slot, quick.clone(), 6); // grid prefetch: 5 + 1
        r.request_quick(Slot::ThumbQuick(PhotoId(2)), other, 9); // filmstrip: 8 + 1
        let request_id = r.requests[&slot].id;
        assert!(r.thumb_current(&p, 384, 5));
        assert!(!r.thumb_current(&p, 384, 10));
        // visible grid: right after the loupe's own stand-in (PERF-CULL-TIERS)
        let on_screen = crate::panels::grid::stand_in_priority(10);
        r.request_quick(slot, quick.clone(), on_screen);
        assert!(r.thumb_current(&p, 384, 10));
        r.request_quick(slot, quick.clone(), on_screen);
        assert_eq!(r.requests[&slot].id, request_id);
        assert_eq!(r.pool.queued(), 2);
        r.pool.run_inline(1);
        let done = r.pool.try_recv().unwrap();
        assert_eq!(done.slot, slot);
        assert_eq!(done.result.result.request_id, request_id);
        assert_eq!(done.result.result.quick, Some(QuickSource::Embedded));
        // A finished result awaiting collection also must not start another extraction.
        r.request_quick(slot, quick, 6);
        assert!(r.thumb_current(&p, 384, 5));
        assert_eq!(r.pool.queued(), 1);
    }

    #[test]
    fn queued_standin_demotes_when_it_leaves_the_visible_grid() {
        let (p, quick) = queued_quick_thumbnail(PhotoId(1));
        let (_, other) = queued_quick_thumbnail(PhotoId(2));
        let slot = Slot::ThumbQuick(p.id);
        let mut r = Renderer { pool: JobPool::new(0), ..Default::default() };
        r.remember_thumb(&p, 384, quick.key, true);
        r.request_quick(slot, quick.clone(), 11);
        r.request_quick(Slot::ThumbQuick(PhotoId(2)), other, 9);
        assert!(!r.thumb_current(&p, 384, 5));
        r.request_quick(slot, quick, 6);
        assert!(r.thumb_current(&p, 384, 5));
        r.pool.run_inline(1);
        assert_eq!(r.pool.try_recv().unwrap().slot, Slot::ThumbQuick(PhotoId(2)));
    }

    #[test]
    fn failures_retry_after_revision_and_quick_only_is_not_final() {
        let p = photo();
        let slot = Slot::Thumb(p.id);
        let mut r = Renderer::default();
        r.remember_thumb(&p, 384, 77, false);
        r.pending.insert(Slot::ThumbQuick(p.id), (77, 11));
        assert!(!r.thumb_current(&p, 384, 10));
        r.failed.insert(slot, (77, "no decoder".into(), 0, p.id));
        assert!(r.thumb_current(&p, 384, 10));
        assert_eq!(r.failure_of(slot, p.id), Some("no decoder"));
        assert_eq!(r.failure_of(slot, PhotoId(p.id.0 + 1)), None, "another photo's error");
        r.catalog_rev = 1;
        assert!(!r.thumb_current(&p, 384, 10));
    }

    #[test]
    fn a_fast_draft_is_drawn_in_the_frame_that_asked_for_it() {
        let mut s = Session::new();
        s.media.file_loader = Some(Arc::new(|_, _| Ok((lightcraft_raster::Rgb32f::new(8, 8), Default::default()))));
        s.catalog.apply(Op::AddPhoto { photo: Box::new((*photo()).clone()) }).unwrap();
        let mut r = Renderer::with_worker_threads(1);
        let ctx = egui::Context::default();
        let budget = std::time::Duration::from_secs(10);
        // the last loupe render took longer than the budget (a CPU render): not waited for
        r.last_main_ms = 1e6;
        r.request(Slot::Main, s.render_job(PhotoId(1), 64, 64, false, true).unwrap(), 100);
        r.wait_for_draft(&ctx, &mut s, budget);
        assert!(r.is_pending(Slot::Main) && !r.textures.contains_key(&Slot::Main));
        // a fast one (GPU drafts): the texture is the new render when the wait returns
        r.last_main_ms = 1.0;
        r.request(Slot::Main, s.render_job(PhotoId(1), 32, 32, false, true).unwrap(), 100);
        let want = r.wanted(Slot::Main);
        r.wait_for_draft(&ctx, &mut s, budget);
        assert!(!r.is_pending(Slot::Main));
        assert_eq!(r.textures.get(&Slot::Main).map(|t| t.key), want);
    }

    #[derive(Default)]
    struct Work {
        jobs: Vec<(Slot, RenderJob)>,
        finished: Vec<(Slot, RenderResult, f64)>,
    }
    struct Manual(std::rc::Rc<std::cell::RefCell<Work>>);
    impl RenderOffload for Manual {
        fn try_start(&mut self, slot: Slot, job: RenderJob) -> Option<RenderJob> {
            self.0.borrow_mut().jobs.push((slot, job));
            None
        }
        fn finished(&mut self) -> Vec<(Slot, RenderResult, f64)> {
            std::mem::take(&mut self.0.borrow_mut().finished)
        }
    }
    fn fixture() -> (crate::LightcraftApp, std::rc::Rc<std::cell::RefCell<Work>>, egui::Context) {
        fixture_with(Session::new(), (*photo()).clone())
    }
    fn fixture_with(mut s: Session, photo: Photo) -> (crate::LightcraftApp, std::rc::Rc<std::cell::RefCell<Work>>, egui::Context) {
        s.media.file_loader = Some(Arc::new(|_, _| {
            let mut image = lightcraft_raster::Rgb32f::new(8, 8);
            image.data.fill([0.3, 0.2, 0.1]);
            Ok((image, Default::default()))
        }));
        // (`with_fs` sets the file system's thumbnail loader, which wins over `file_loader`: thumbnails use the stub too)
        s.media.reduced_loader = None;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        let mut app = crate::LightcraftApp::new(s, crate::Services::default());
        app.renderer.keep_pixels = true;
        let work = std::rc::Rc::new(std::cell::RefCell::new(Work::default()));
        app.renderer.set_offload(Box::new(Manual(work.clone())));
        let ctx = egui::Context::default();
        app.renderer.poll(&ctx, &mut app.session);
        (app, work, ctx)
    }
    fn take_job(work: &std::rc::Rc<std::cell::RefCell<Work>>) -> (Slot, RenderJob) {
        work.borrow_mut().jobs.pop().unwrap()
    }
    fn finish(app: &mut crate::LightcraftApp, work: &std::rc::Rc<std::cell::RefCell<Work>>, ctx: &egui::Context, slot: Slot, result: RenderResult) {
        work.borrow_mut().finished.push((slot, result, 0.0));
        app.renderer.poll(ctx, &mut app.session);
    }
    fn request(app: &mut crate::LightcraftApp) {
        crate::panels::grid::request_thumb(app, PhotoId(1), 256, 10);
    }

    /// Exercise both texture uploads, the delta mirror used by Headless, and the CPU overrides
    /// used by the native shadow screenshot. Synthetic checker pixels make interpolation visible.
    #[test]
    fn zoom_pixel_filter_uploads_and_shadow_match_at_one_and_two_x() {
        use crate::headless::HeadlessView;
        use egui::{Color32, Rect, pos2, vec2};
        let inspected =
            [Slot::Main, Slot::Before, Slot::Region, Slot::RegionBefore, Slot::Compare(0), Slot::CompareRegion(1), Slot::Second, Slot::Hover];
        let smooth = [
            Slot::Thumb(PhotoId(1)),
            Slot::ThumbQuick(PhotoId(1)),
            Slot::Preview,
            Slot::Variant(7),
            Slot::Import(0),
            Slot::Slide(PhotoId(1).into()),
            Slot::SlidePreview,
        ];
        for (slot, pixel_blocks) in inspected.into_iter().map(|s| (s, true)).chain(smooth.into_iter().map(|s| (s, false))) {
            let (mut app, work, ctx) = fixture();
            let mut normal = HeadlessView::new();
            normal.ctx = ctx;
            let mut shadow = HeadlessView::new();
            for round in 0..2 {
                let job = app.session.render_job(PhotoId(1), 8 + round, 8 + round, false, true).unwrap();
                app.renderer.request(slot, job, 100);
                let (_, job) = take_job(&work);
                let mut result = job.run();
                result.rendered.as_mut().unwrap().image = lightcraft_raster::Rgba8 {
                    width: 2,
                    height: 2,
                    data: vec![[255, 0, 0, 255], [0, 0, 255, 255], [0, 0, 255, 255], [255, 0, 0, 255]],
                };
                finish(&mut app, &work, &normal.ctx, slot, result);
                let id = app.renderer.textures.get(&slot).unwrap().tex.id();
                for ppp in [1.0, 2.0] {
                    // Zoom is physical screen pixels per image pixel, independent of Retina scale.
                    for factor in [2_usize, 8, 16] {
                        let side = 2 * factor;
                        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(side as f32 / ppp, side as f32 / ppp));
                        for view in [&mut normal, &mut shadow] {
                            view.run(HeadlessView::raw_input(vec2(40.0, 40.0), ppp, 0.0, vec![]), |ui| {
                                ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("photo"))).image(
                                    id,
                                    rect,
                                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                            });
                        }
                        let image = normal.paint(&HashMap::new());
                        let mirrored = shadow.paint(&app.renderer.cpu_textures());
                        assert!(image.pixels == mirrored.pixels, "screenshot mismatch: {slot:?}, upload {round}, {ppp}x, zoom {}", factor * 100);
                        if pixel_blocks {
                            for y in 0..side {
                                for x in 0..side {
                                    let expected = if (x / factor + y / factor) % 2 == 0 { Color32::RED } else { Color32::BLUE };
                                    assert_eq!(image.pixels[y * image.size[0] + x], expected, "{slot:?}, upload {round}, {ppp}x, ({x}, {y})");
                                }
                            }
                        } else {
                            let middle = image.pixels[(factor - 1) * image.size[0] + factor - 1];
                            assert!(middle.r() > 0 && middle.b() > 0, "{slot:?} must retain linear magnification: {middle:?}");
                        }
                    }
                    // A checker reduced to one physical pixel averages red and blue, for every slot.
                    let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(1.0 / ppp, 1.0 / ppp));
                    for view in [&mut normal, &mut shadow] {
                        view.run(HeadlessView::raw_input(vec2(40.0, 40.0), ppp, 0.0, vec![]), |ui| {
                            ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("photo"))).image(
                                id,
                                rect,
                                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        });
                    }
                    let image = normal.paint(&HashMap::new());
                    assert!(image.pixels == shadow.paint(&app.renderer.cpu_textures()).pixels, "minified screenshot mismatch: {slot:?}");
                    let pixel = image.pixels[0];
                    assert!(
                        (i16::from(pixel.r()) - 128).abs() <= 1 && pixel.g() == 0 && (i16::from(pixel.b()) - 128).abs() <= 1,
                        "{slot:?} must retain linear minification: {pixel:?}"
                    );
                }
            }
        }
    }

    fn relink_destination() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("lc-render-relinked-{}-{}.nef", std::process::id(), lightcraft_preview::next_tick()));
        std::fs::write(&path, b"replacement source").unwrap();
        path
    }

    #[test]
    fn completed_failure_keeps_requested_source_after_relink() {
        let replacement = relink_destination();
        for slot in [Slot::Thumb(PhotoId(1)), Slot::Main, Slot::Before, Slot::Compare(1), Slot::Region] {
            let (mut app, work, ctx) = fixture();
            app.session.media.file_loader = Some(Arc::new(|_, _| Err("simulated decode failure".into())));
            let job = app.session.render_job(PhotoId(1), 64, 64, false, true).unwrap();
            app.renderer.request(slot, job, 100);
            let (submitted, job) = take_job(&work);
            assert_eq!(submitted, slot);
            let result = job.run();
            assert!(result.rendered.is_err(), "the old request has already failed");
            app.session.execute("photo.relink", &serde_json::json!({"id": 1, "path": replacement})).unwrap();
            assert!(
                matches!(&app.session.catalog.photo(PhotoId(1)).unwrap().source, Source::File { path } if path.ends_with(replacement.file_name().unwrap().to_str().unwrap()))
            );
            finish(&mut app, &work, &ctx, slot, result);
            assert_eq!(app.renderer.failure(slot), Some("simulated decode failure"));
            assert!(!app.renderer.is_pending(slot));
            let message = app.renderer.failure_messages.last().expect("the collector emitted a diagnostic");
            assert!(message.contains("audit.jpg"), "{message}");
            assert!(!message.contains("lc-render-relinked-"), "the failed request did not read the replacement: {message}");
            assert!(message.ends_with(": simulated decode failure"), "{message}");
        }
        std::fs::remove_file(replacement).unwrap();
    }

    #[test]
    fn superseded_failure_does_not_take_the_new_requests_source() {
        let replacement = relink_destination();
        let (mut app, work, ctx) = fixture();
        app.session.media.file_loader = Some(Arc::new(|_, _| Err("simulated decode failure".into())));
        let slot = Slot::Main;
        let old_job = app.session.render_job(PhotoId(1), 64, 64, false, true).unwrap();
        app.renderer.request(slot, old_job, 100);
        let (_, old_job) = take_job(&work);
        let old_result = old_job.run();
        app.session.execute("photo.relink", &serde_json::json!({"id": 1, "path": replacement})).unwrap();
        let new_job = app.session.render_job(PhotoId(1), 64, 64, false, true).unwrap();
        app.renderer.request(slot, new_job, 100);
        let (_, new_job) = take_job(&work);
        finish(&mut app, &work, &ctx, slot, old_result);
        assert!(app.renderer.failure_messages.is_empty(), "obsolete failures stay suppressed");
        assert!(app.renderer.is_pending(slot));
        finish(&mut app, &work, &ctx, slot, new_job.run());
        let message = app.renderer.failure_messages.last().expect("new failure is reported");
        assert!(message.contains("lc-render-relinked-"), "{message}");
        assert!(!message.contains("audit.jpg"), "{message}");
        assert!(app.renderer.requests.is_empty(), "completed request identity is released");
        std::fs::remove_file(replacement).unwrap();
    }

    #[test]
    fn grid_queued_standin_changes_priority_without_restarting() {
        let mut s = Session::new();
        s.media.preview_loader = Some(Arc::new(|_, _| Some(lightcraft_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })));
        let mut raw = (*photo()).clone();
        raw.kind = lightcraft_catalog::MediaKind::Raw;
        raw.develop = Arc::new(raw.camera_defaults());
        let (mut app, work, ctx) = fixture_with(s, raw);
        app.renderer.pool = JobPool::new(0); // deterministic: no quick job starts until run_inline
        let id = PhotoId(1);
        let slot = Slot::ThumbQuick(id);

        crate::panels::grid::request_thumb(&mut app, id, 384, 5);
        let request_id = app.renderer.requests[&slot].id;
        assert_eq!(app.renderer.pending[&slot].1, 6);
        assert_eq!(app.renderer.pool.queued(), 1);
        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        // on screen, a stand-in comes right after the loupe's own (PERF-CULL-TIERS)
        assert_eq!(app.renderer.pending[&slot].1, crate::panels::grid::STAND_IN_ON_SCREEN, "the grid must promote its queued stand-in");
        assert_eq!(app.renderer.requests[&slot].id, request_id);
        assert_eq!(app.renderer.pool.queued(), 1);
        // asked again every frame while it waits: no job is built again
        let built_on_screen = app.renderer.thumb_jobs_built;
        for _ in 0..5 {
            crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        }
        assert_eq!(app.renderer.thumb_jobs_built, built_on_screen, "a queued on-screen stand-in is current");

        crate::panels::grid::request_thumb(&mut app, id, 384, 5);
        assert_eq!(app.renderer.pending[&slot].1, 6, "the grid must demote its queued stand-in");
        assert_eq!(app.renderer.requests[&slot].id, request_id);
        assert_eq!(app.renderer.pool.queued(), 1);
        let built = app.renderer.thumb_jobs_built;
        app.renderer.pool.run_inline(1);
        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        assert_eq!(app.renderer.pending[&slot].1, 6, "completed work must not be restarted");
        assert_eq!(app.renderer.requests[&slot].id, request_id);
        assert_eq!(app.renderer.pool.queued(), 0);
        app.renderer.poll(&ctx, &mut app.session);
        assert_eq!(app.renderer.textures[&slot].quick, Some(QuickSource::Embedded));

        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        crate::panels::grid::request_thumb(&mut app, id, 384, 5);
        assert_eq!(app.renderer.thumb_jobs_built, built);
        assert_eq!(app.renderer.pool.queued(), 0);
        assert!(!app.renderer.is_pending(Slot::Thumb(id)));
        assert!(!app.renderer.textures.contains_key(&Slot::Thumb(id)));
        assert!(work.borrow().jobs.is_empty(), "the final embedded thumbnail needs no real render");
    }

    #[test]
    fn unchanged_ready_and_running_frames_build_no_jobs() {
        let (mut app, work, ctx) = fixture();
        request(&mut app);
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        for _ in 0..180 {
            request(&mut app);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        let (slot, job) = take_job(&work);
        finish(&mut app, &work, &ctx, slot, job.run());
        for _ in 0..180 {
            request(&mut app);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        assert!(work.borrow().jobs.is_empty());
    }

    #[test]
    fn a_slider_drag_leaves_the_edited_photos_thumbnail_until_it_ends() {
        let (mut app, work, ctx) = fixture();
        request(&mut app);
        let (slot, job) = take_job(&work);
        finish(&mut app, &work, &ctx, slot, job.run());
        app.session.execute("library.select", &serde_json::json!({"ids": [1], "active": 1})).unwrap();
        app.session.execute("develop.beginInteraction", &serde_json::json!({"label": "Exposure"})).unwrap();
        for v in [0.1, 0.2, 0.3] {
            app.session.execute("develop.set", &serde_json::json!({"control": "light.exposure", "value": v})).unwrap();
            app.renderer.poll(&ctx, &mut app.session);
            request(&mut app);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 1, "the drag's frames render no thumbnail of the photo");
        assert!(work.borrow().jobs.is_empty());
        app.session.execute("develop.endInteraction", &serde_json::json!({})).unwrap();
        request(&mut app);
        assert_eq!(app.renderer.thumb_jobs_built, 2, "the thumbnail follows once the drag ends");
    }

    #[test]
    fn unchanged_queued_frames_build_no_jobs() {
        let (mut app, _, _) = fixture();
        app.renderer.set_offload(Box::new(Blocked));
        request(&mut app);
        for _ in 0..180 {
            request(&mut app);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        assert_eq!(app.renderer.queue.len(), 1);
    }

    #[test]
    fn resizing_the_disk_cache_keeps_textures_and_skips_rebuilds() {
        let lib = std::env::temp_dir().join(format!("lc-ui-thumb-resize-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&lib);
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let (mut app, work, ctx) = fixture_with(s, (*photo()).clone());
        request(&mut app);
        let (slot, job) = take_job(&work);
        finish(&mut app, &work, &ctx, slot, job.run());
        assert!(app.renderer.thumb(PhotoId(1)).is_some());
        let cache = app.session.media.rendered.clone();
        app.session.execute("library.preferences", &serde_json::json!({"cacheMb": 300})).unwrap();
        app.renderer.poll(&ctx, &mut app.session);
        // thumbnails are keyed by content: a new budget leaves them (and the cache) valid
        assert!(app.renderer.thumb(PhotoId(1)).is_some());
        assert!(Arc::ptr_eq(&cache, &app.session.media.rendered));
        for _ in 0..30 {
            request(&mut app);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        assert!(work.borrow().jobs.is_empty());
        // an explicit clear still drops them
        app.session.execute("library.clearPreviews", &serde_json::json!({})).unwrap();
        app.renderer.poll(&ctx, &mut app.session);
        assert!(app.renderer.thumb(PhotoId(1)).is_none());
        drop(app);
        let _ = std::fs::remove_dir_all(&lib);
    }

    /// A render that panics (a decoder bug) is a failure like any other: the slot stops waiting,
    /// the error is kept (not retried every frame), and the pool's workers live on. Before, the
    /// worker died without an answer and the slot stayed pending forever.
    #[test]
    fn a_panicking_render_is_a_failure_not_a_dead_worker() {
        let (mut app, _, ctx) = fixture_with(Session::new(), (*photo()).clone());
        app.session.media.file_loader = Some(Arc::new(|_, _| panic!("decoder bug")));
        app.renderer.offload = None; // the real job pool
        let slot = Slot::Thumb(PhotoId(1));
        let ask = |app: &mut crate::LightcraftApp| {
            if let Some(job) = app.session.thumb_job(PhotoId(1), 256) {
                app.renderer.request(slot, job, 30);
            }
            app.renderer.poll(&ctx, &mut app.session);
        };
        ask(&mut app);
        let t0 = std::time::Instant::now();
        while app.renderer.is_pending(slot) {
            assert!(t0.elapsed() < std::time::Duration::from_secs(20), "the render never answered");
            std::thread::sleep(std::time::Duration::from_millis(5));
            app.renderer.poll(&ctx, &mut app.session);
        }
        assert_eq!(app.renderer.failure(slot), Some("the render failed unexpectedly: decoder bug"));
        let workers = app.renderer.live_workers();
        assert!(workers > 0, "the worker that panicked is still there");
        let done = app.renderer.completed;
        for _ in 0..30 {
            ask(&mut app);
        }
        assert!(!app.renderer.is_pending(slot) && app.renderer.completed == done, "no retry loop");
        assert_eq!(app.renderer.live_workers(), workers);
    }

    #[test]
    fn raw_quick_path_frames_build_no_jobs() {
        // an unedited raw: the grid shows its embedded preview (held here until released) and stops
        // there — the camera JPEG at 4 416 × 2 944 already covers a 512 px thumbnail, so decoding the
        // whole raw behind it only costs the file read (45 s a photo on a 1.2 MB/s share)
        let gate = Arc::new(std::sync::Mutex::new(()));
        let held = gate.lock().unwrap();
        let mut s = Session::new();
        let g = gate.clone();
        s.media.preview_loader = Some(Arc::new(move |_: &str, _| {
            drop(g.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
            Some(lightcraft_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })
        }));
        let mut raw = (*photo()).clone();
        raw.kind = lightcraft_catalog::MediaKind::Raw;
        raw.develop = Arc::new(raw.camera_defaults());
        let (mut app, _, ctx) = fixture_with(s, raw);
        app.renderer.set_offload(Box::new(Blocked));
        let quick = Slot::ThumbQuick(PhotoId(1));
        // the stand-in is running
        for _ in 0..180 {
            request(&mut app);
        }
        assert!(app.renderer.is_pending(quick));
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        drop(held);
        let t0 = std::time::Instant::now();
        while !app.renderer.textures.contains_key(&quick) {
            assert!(t0.elapsed() < std::time::Duration::from_secs(20), "the embedded preview never arrived");
            std::thread::sleep(std::time::Duration::from_millis(5));
            app.renderer.poll(&ctx, &mut app.session);
        }
        // the embedded preview is the thumbnail: no real render is queued behind it, and the grid
        // asking every frame neither rebuilds the job nor re-requests the preview
        for _ in 0..180 {
            request(&mut app);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 1, "an unedited raw's embedded preview is the thumbnail");
        assert!(app.renderer.queue.is_empty(), "nothing is decoded behind it");
        assert!(!app.renderer.is_pending(Slot::Thumb(PhotoId(1))));

        // an edit takes the embedded preview out of the picture: the real thumbnail is rendered then
        let mut edited = (*photo()).clone();
        edited.kind = lightcraft_catalog::MediaKind::Raw;
        edited.develop = Arc::new({
            let mut d = edited.camera_defaults();
            d.light.exposure = 0.5;
            d
        });
        let (mut app, _, _ctx) = fixture_with(Session::new(), edited);
        app.renderer.set_offload(Box::new(Blocked));
        for _ in 0..180 {
            request(&mut app);
        }
        assert_eq!(app.renderer.thumb_jobs_built, 1);
        assert!(app.renderer.is_pending(Slot::Thumb(PhotoId(1))) || !app.renderer.queue.is_empty(), "an edited raw renders its thumbnail");
    }

    #[test]
    fn closing_import_review_cancels_queued_and_running_identities() {
        let (mut app, work, ctx) = fixture();
        let job = app.session.thumb_job(PhotoId(1), 256).unwrap();
        app.renderer.request(Slot::Import(0), job.clone(), 10);
        let (slot, running) = take_job(&work);
        app.renderer.set_offload(Box::new(Blocked));
        app.renderer.request(Slot::Import(1), job, 10);
        app.renderer.forget_imports();
        assert!(!app.renderer.is_pending(Slot::Import(0)));
        assert!(!app.renderer.is_pending(Slot::Import(1)));
        assert!(app.renderer.queue.is_empty());
        work.borrow_mut().finished.push((slot, running.run(), 0.0));
        app.renderer.set_offload(Box::new(Manual(work.clone())));
        app.renderer.poll(&ctx, &mut app.session);
        assert!(!app.renderer.textures.contains_key(&slot));
        assert_eq!(app.session.media.source_usage().0, 0);
    }

    #[test]
    fn loupe_shows_superseded_drafts_newer_than_its_pixels_during_a_drag() {
        let (mut app, work, ctx) = fixture();
        let mut jobs = Vec::new();
        for exposure in [0.0, 1.0, 2.0] {
            let mut edit = lightcraft_develop::DevelopSettings::default();
            edit.light.exposure = exposure;
            app.session.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "drag".into(), edited: None }).unwrap();
            let job = app.session.loupe_job(PhotoId(1), 8, 8, true).unwrap().draft();
            app.renderer.request(Slot::Main, job, 100);
            jobs.push(take_job(&work).1);
        }
        let keys: Vec<u64> = jobs.iter().map(|j| j.key).collect();
        let mut jobs = jobs.into_iter();
        let (first, second, last) = (jobs.next().unwrap(), jobs.next().unwrap(), jobs.next().unwrap());
        // the first draft lands while the newest is still rendering: the loupe moves on
        finish(&mut app, &work, &ctx, Slot::Main, first.run());
        assert_eq!(app.renderer.textures.get(&Slot::Main).map(|t| t.key), Some(keys[0]));
        assert!(app.renderer.is_pending(Slot::Main));
        finish(&mut app, &work, &ctx, Slot::Main, last.run());
        assert_eq!(app.renderer.textures.get(&Slot::Main).map(|t| t.key), Some(keys[2]));
        // a draft older than the pixels shown never replaces them
        finish(&mut app, &work, &ctx, Slot::Main, second.run());
        assert_eq!(app.renderer.textures.get(&Slot::Main).map(|t| t.key), Some(keys[2]));
        assert!(!app.renderer.is_pending(Slot::Main));
    }

    #[test]
    fn a_drag_gets_its_live_hdr_histogram_beside_the_drafts() {
        let (mut app, work, ctx) = fixture();
        let mut jobs = Vec::new();
        for exposure in [1.0, 2.0] {
            let mut edit = lightcraft_develop::DevelopSettings::default();
            edit.hdr.enabled = true;
            edit.light.exposure = exposure;
            app.session.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "drag".into(), edited: None }).unwrap();
            let job = app.session.render_job(PhotoId(1), 32, 32, false, true).unwrap();
            app.renderer.request(Slot::HdrHistogram, job, 95);
            jobs.push(take_job(&work).1);
        }
        let keys: Vec<u64> = jobs.iter().map(|j| j.key).collect();
        let mut jobs = jobs.into_iter();
        let (first, last) = (jobs.next().unwrap(), jobs.next().unwrap());
        // superseded while the drag goes on, it still lands: the histogram keeps moving
        finish(&mut app, &work, &ctx, Slot::HdrHistogram, first.run());
        let live = app.renderer.live_hdr_histogram.clone().expect("a live HDR histogram");
        assert_eq!((live.0, live.1), (PhotoId(1), keys[0]));
        assert!(live.2.hdr.is_some(), "HDR bins");
        finish(&mut app, &work, &ctx, Slot::HdrHistogram, last.run());
        assert_eq!(app.renderer.live_hdr_histogram.as_ref().map(|l| l.1), Some(keys[1]));
        // only the histogram is kept: no texture
        assert!(!app.renderer.textures.contains_key(&Slot::HdrHistogram));
    }

    #[test]
    fn library_reset_rejects_old_job_even_when_photo_id_and_key_are_reused() {
        let (mut app, work, ctx) = fixture();
        request(&mut app);
        let (slot, old) = take_job(&work);
        let old_result = old.run();
        app.renderer.forget_all();
        let photo = app.session.catalog.photo(PhotoId(1)).unwrap().as_ref().clone();
        app.session = lightcraft_engine::Session::new();
        app.session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        app.session.media.file_loader = Some(Arc::new(|_, _| {
            let mut image = lightcraft_raster::Rgb32f::new(8, 8);
            image.data.fill([0.1, 0.2, 0.9]);
            Ok((image, Default::default()))
        }));
        app.renderer.poll(&ctx, &mut app.session);
        request(&mut app);
        let (_, new) = take_job(&work);
        assert_eq!(old_result.key, new.key);
        assert_ne!(old_result.request_id, new.request_id);
        let expected = new.clone().run().rendered.unwrap().image.as_bytes();
        finish(&mut app, &work, &ctx, slot, new.run());
        finish(&mut app, &work, &ctx, slot, old_result);
        assert_eq!(
            app.renderer.thumb(PhotoId(1)).unwrap().pixels.as_ref().unwrap().pixels.iter().flat_map(|c| c.to_array()).collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn late_results_and_errors_cannot_replace_newest_pixels_or_sources() {
        for order in [0, 1, 2] {
            let (mut app, work, ctx) = fixture();
            request(&mut app);
            let (slot, old) = take_job(&work);
            let mut edit = lightcraft_develop::DevelopSettings::default();
            edit.light.exposure = 1.0;
            app.session.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "edit".into(), edited: None }).unwrap();
            request(&mut app);
            let (_, new) = take_job(&work);
            let expected = new.clone().run().rendered.unwrap().image.as_bytes();
            if order != 0 {
                finish(&mut app, &work, &ctx, slot, new.run());
                // An obsolete source and error must not poison the current successful request.
                let mut stale = old.run();
                if order == 2 {
                    stale.rendered = Err("old failure".into());
                }
                finish(&mut app, &work, &ctx, slot, stale);
            } else {
                finish(&mut app, &work, &ctx, slot, old.run());
                assert!(app.renderer.thumb(PhotoId(1)).is_none());
                assert_eq!(app.session.media.source_usage().0, 0);
                finish(&mut app, &work, &ctx, slot, new.run());
            }
            assert_eq!(
                app.renderer.thumb(PhotoId(1)).unwrap().pixels.as_ref().unwrap().pixels.iter().flat_map(|c| c.to_array()).collect::<Vec<_>>(),
                expected
            );
            assert!(app.renderer.failure(slot).is_none());
        }
    }

    #[test]
    fn undo_to_existing_texture_supersedes_pending_edit() {
        let (mut app, work, ctx) = fixture();
        request(&mut app);
        let (slot, first) = take_job(&work);
        let key = first.key;
        finish(&mut app, &work, &ctx, slot, first.run());
        let original = app.renderer.thumb(PhotoId(1)).unwrap().pixels.clone().unwrap();
        let mut edit = lightcraft_develop::DevelopSettings::default();
        edit.light.exposure = 1.0;
        let undo =
            app.session.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "edit".into(), edited: None }).unwrap();
        request(&mut app);
        let (_, edited) = take_job(&work);
        app.session.catalog.apply(undo).unwrap();
        request(&mut app);
        let (_, restored) = take_job(&work);
        assert_eq!(restored.key, key);
        finish(&mut app, &work, &ctx, slot, restored.run());
        finish(&mut app, &work, &ctx, slot, edited.run());
        assert_eq!(app.renderer.thumb(PhotoId(1)).unwrap().pixels.as_ref().unwrap().pixels, original.pixels);
    }

    #[test]
    fn explicit_clear_repairs_same_key_bad_pixels_and_rejects_old_same_key_job() {
        let (mut app, work, ctx) = fixture();
        let job = app.session.thumb_job(PhotoId(1), 256).unwrap();
        let expected = job.clone().run().rendered.unwrap().image;
        let (cache, key) = job.cache.clone().unwrap();
        let mut bad = expected.clone();
        bad.data.fill([255, 0, 255, 255]);
        cache.put(key, Arc::new(bad.clone()));
        request(&mut app);
        let (slot, corrupt) = take_job(&work);
        finish(&mut app, &work, &ctx, slot, corrupt.run());
        assert_eq!(app.renderer.thumb(PhotoId(1)).unwrap().pixels.as_ref().unwrap().pixels[0].to_array(), [255, 0, 255, 255]);
        // An old job can have the SAME render key and stale decoded pixels.
        let mut old = job.clone();
        old.source = lightcraft_engine::media::SourceRef::Loaded(Box::new(lightcraft_engine::media::DecodedSource::new(
            Arc::new(lightcraft_raster::Rgb32f::new(8, 8)),
            None,
        )));
        app.renderer.textures.clear();
        app.renderer.request(slot, old, 10);
        let (_, old) = take_job(&work);
        app.session.execute("library.clearPreviews", &serde_json::json!({})).unwrap();
        app.renderer.poll(&ctx, &mut app.session);
        assert!(app.renderer.thumb(PhotoId(1)).is_none());
        request(&mut app);
        let (_, repaired) = take_job(&work);
        assert_eq!(repaired.key, job.key);
        finish(&mut app, &work, &ctx, slot, repaired.run());
        finish(&mut app, &work, &ctx, slot, old.run());
        assert_eq!(cache.get(key).unwrap().as_bytes(), expected.as_bytes());
        assert_eq!(
            app.renderer.thumb(PhotoId(1)).unwrap().pixels.as_ref().unwrap().pixels.iter().flat_map(|c| c.to_array()).collect::<Vec<_>>(),
            expected.as_bytes()
        );
    }

    #[test]
    fn undo_to_embedded_thumb_rejects_late_full_render() {
        let mut s = Session::new();
        s.media.preview_loader = Some(Arc::new(|_, _| Some(lightcraft_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })));
        let mut raw = (*photo()).clone();
        raw.kind = lightcraft_catalog::MediaKind::Raw;
        raw.develop = Arc::new(raw.camera_defaults());
        let (mut app, work, ctx) = fixture_with(s, raw);
        app.renderer.pool = JobPool::new(0);
        let id = PhotoId(1);
        let quick_slot = Slot::ThumbQuick(id);
        let full_slot = Slot::Thumb(id);
        let settings_a = app.session.catalog.photo(id).unwrap().develop.clone();

        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        assert_eq!(app.renderer.pool.run_inline(1), 1);
        app.renderer.poll(&ctx, &mut app.session);
        let (key_a, pixels_a) = {
            let shown = app.renderer.thumb(id).unwrap();
            assert_eq!(shown.quick, Some(QuickSource::Embedded));
            (shown.key, shown.pixels.as_ref().unwrap().pixels.clone())
        };
        assert!(app.renderer.textures.contains_key(&quick_slot));
        assert!(!app.renderer.textures.contains_key(&full_slot));
        assert!(work.borrow().jobs.is_empty());

        let mut settings_b = (*settings_a).clone();
        settings_b.light.exposure += 0.5;
        app.session.commit("Exposure", Op::SetDevelop { id, settings: Arc::new(settings_b), label: "Exposure".into(), edited: None }).unwrap();
        assert!(!app.session.embedded_thumb_is_final(app.session.catalog.photo(id).unwrap()));
        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        // the edited raw's camera JPEG is its stand-in first (PERF-CULL-TIERS); the render behind it
        // is asked for once the stand-in is there
        app.renderer.pool.run_inline(1);
        app.renderer.poll(&ctx, &mut app.session);
        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        // Manual owns dispatched jobs: hold B until the real Undo command restores A.
        let (slot, held_b) = take_job(&work);
        assert_eq!(slot, full_slot);
        let key_b = held_b.key;
        assert_ne!(key_b, key_a);
        assert!(app.renderer.is_pending(full_slot));

        app.run("edit.undo", serde_json::json!({})).unwrap();
        assert_eq!(app.session.catalog.photo(id).unwrap().develop.as_ref(), settings_a.as_ref());
        assert!(app.session.embedded_thumb_is_final(app.session.catalog.photo(id).unwrap()));
        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        // (B's stand-in is the same camera JPEG under B's key: A's is made again, a quick job)
        app.renderer.pool.run_inline(1);
        app.renderer.poll(&ctx, &mut app.session);
        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        {
            let restored = app.renderer.thumb(id).unwrap();
            assert_eq!(restored.key, key_a);
            assert_eq!(restored.pixels.as_ref().unwrap().pixels, pixels_a);
            assert_eq!(restored.quick, Some(QuickSource::Embedded));
        }
        assert!(work.borrow().jobs.is_empty(), "restoring embedded-final A needs no full render");
        assert_eq!(app.renderer.pool.queued(), 0);

        let result_b = held_b.run();
        assert_eq!(result_b.key, key_b);
        let rendered_b = result_b.rendered.as_ref().expect("held edited thumbnail must render before the stale-result oracle");
        assert_ne!(color_image(&rendered_b.image).pixels, pixels_a, "the held result must be distinguishable from A");
        finish(&mut app, &work, &ctx, slot, result_b);

        let shown = app.renderer.thumb(id).unwrap();
        assert_eq!(shown.key, key_a, "late edited Thumb(B) must not replace restored embedded ThumbQuick(A)");
        assert_eq!(shown.pixels.as_ref().unwrap().pixels, pixels_a);
        assert_eq!(shown.quick, Some(QuickSource::Embedded));
        let quick = app.renderer.textures.get(&quick_slot).expect("the correct embedded thumbnail must remain installed");
        assert_eq!(quick.key, key_a);
        assert_eq!(quick.quick, Some(QuickSource::Embedded));
        assert_eq!(quick.pixels.as_ref().unwrap().pixels, pixels_a);
        assert!(!app.renderer.is_pending(full_slot));
    }

    /// PERF-CULL-2: the loupe's stand-ins made ahead are taken over at the step (no job, the texture
    /// at once), a photo still wanted keeps its slot as the photos shift, and a step to a photo whose
    /// stand-in is still being made shows it when it arrives.
    #[test]
    fn a_step_takes_over_the_camera_preview_made_ahead() {
        let gate = Arc::new(std::sync::Mutex::new(()));
        let mut s = Session::new();
        let g = gate.clone();
        s.media.preview_loader = Some(Arc::new(move |_: &str, _| {
            drop(g.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
            Some(lightcraft_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })
        }));
        let raw = |id: u64| {
            let mut p = Photo::new(PhotoId(id), Source::File { path: format!("r{id}.cr3") }, &format!("r{id}.cr3"), "CR3", 6000, 4000, "2026-01-01");
            p.kind = lightcraft_catalog::MediaKind::Raw;
            p.develop = Arc::new(p.camera_defaults());
            p
        };
        let (mut app, _, ctx) = fixture_with(s, raw(1));
        for id in 2..=4 {
            app.session.catalog.apply(Op::AddPhoto { photo: Box::new(raw(id)) }).unwrap();
        }
        let job = |app: &mut crate::LightcraftApp, id: u64| app.session.quick_view_job(PhotoId(id), 64, true).unwrap();
        let wait = |app: &mut crate::LightcraftApp, done: &dyn Fn(&Renderer) -> bool| {
            let t0 = std::time::Instant::now();
            while !done(&app.renderer) {
                assert!(t0.elapsed() < std::time::Duration::from_secs(20), "the stand-ins never arrived");
                std::thread::sleep(std::time::Duration::from_millis(5));
                app.renderer.poll(&ctx, &mut app.session);
            }
        };
        let ahead_of = |r: &Renderer, id: u64| r.textures.iter().any(|(s, t)| matches!(s, Slot::QuickAhead(_)) && t.photo == PhotoId(id));
        let jobs = vec![job(&mut app, 2), job(&mut app, 3)];
        app.renderer.look_ahead(jobs, 96);
        wait(&mut app, &|r| ahead_of(r, 2) && ahead_of(r, 3));
        // the step to photo 2: its stand-in is the loupe's at once, nothing is made again
        let q = job(&mut app, 2);
        app.renderer.request_quick(Slot::Preview, q, 110);
        assert!(!app.renderer.is_pending(Slot::Preview));
        assert_eq!(app.renderer.textures.get(&Slot::Preview).map(|t| t.photo), Some(PhotoId(2)));
        assert!(!ahead_of(&app.renderer, 2));
        // the next look-ahead (3 and 4): 3 keeps its slot and is not made again
        let held = gate.lock().unwrap();
        let jobs = vec![job(&mut app, 3), job(&mut app, 4)];
        app.renderer.look_ahead(jobs, 96);
        assert!(ahead_of(&app.renderer, 3), "a photo still wanted keeps what was made for it");
        let pending_ahead: Vec<Slot> = app.renderer.pending.keys().copied().filter(|s| matches!(s, Slot::QuickAhead(_))).collect();
        assert_eq!(pending_ahead.len(), 1, "only photo 4 is being made: {pending_ahead:?}");
        // the step to photo 4 while its stand-in is still being made: shown when it arrives
        let q = job(&mut app, 4);
        app.renderer.request_quick(Slot::Preview, q, 110);
        drop(held);
        wait(&mut app, &|r| r.textures.get(&Slot::Preview).is_some_and(|t| t.photo == PhotoId(4)) && !r.is_pending(Slot::Preview));
        wait(&mut app, &|r| !r.pending.keys().any(|s| matches!(s, Slot::QuickAhead(_) | Slot::Preview)));
        assert_eq!(app.renderer.textures.get(&Slot::Preview).map(|t| t.photo), Some(PhotoId(4)));
    }

    /// A loupe with unedited raws 1..=n whose camera previews are read at once, on a renderer
    /// that runs nothing until asked (`run_inline`).
    fn raw_loupe(n: u64) -> (crate::LightcraftApp, egui::Context) {
        let mut s = Session::new();
        s.media.preview_loader =
            Some(Arc::new(|_: &str, _| Some(lightcraft_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })));
        let raw = |id: u64| {
            let mut p = Photo::new(PhotoId(id), Source::File { path: format!("r{id}.cr3") }, &format!("r{id}.cr3"), "CR3", 6000, 4000, "2026-01-01");
            p.kind = lightcraft_catalog::MediaKind::Raw;
            p.develop = Arc::new(p.camera_defaults());
            p
        };
        let (mut app, _, ctx) = fixture_with(s, raw(1));
        for id in 2..=n {
            app.session.catalog.apply(Op::AddPhoto { photo: Box::new(raw(id)) }).unwrap();
        }
        app.renderer.pool = JobPool::new(0);
        (app, ctx)
    }

    fn quick(app: &mut crate::LightcraftApp, id: u64, edge: usize) -> QuickJob {
        app.session.quick_view_job(PhotoId(id), edge, true).unwrap()
    }

    /// A stand-in still queued ahead whose photo moves up in the order (the user stepped) is
    /// queued at its new place, not left behind photos now further away (CodeRabbit on #3055).
    #[test]
    fn a_kept_ahead_job_moves_to_its_new_place_in_the_order() {
        let (mut app, _ctx) = raw_loupe(3);
        let jobs = vec![quick(&mut app, 2, 64), quick(&mut app, 3, 64)];
        let (k2, k3) = (jobs[0].key, jobs[1].key);
        app.renderer.look_ahead(jobs, 96);
        let priority_of = |r: &Renderer, key: u64| {
            let slot = r.quick_tried.iter().find(|(s, k)| matches!(s, Slot::QuickAhead(_)) && **k == key).map(|(s, _)| *s).unwrap();
            r.pending[&slot].1
        };
        assert_eq!((priority_of(&app.renderer, k2), priority_of(&app.renderer, k3)), (96, 95));
        let jobs = vec![quick(&mut app, 3, 64), quick(&mut app, 2, 64)];
        app.renderer.look_ahead(jobs, 96);
        assert_eq!((priority_of(&app.renderer, k3), priority_of(&app.renderer, k2)), (96, 95));
        assert_eq!(app.renderer.pool.queued(), 2, "nothing is queued twice");
    }

    /// A stand-in made ahead that is no longer wanted while it is being made is thrown away when it
    /// arrives, not kept as a hidden texture (CodeRabbit on #3077); one the loupe waits for is kept.
    #[test]
    fn an_ahead_stand_in_no_longer_wanted_is_dropped_when_it_arrives() {
        let gate = Arc::new(std::sync::Mutex::new(()));
        let mut s = Session::new();
        let g = gate.clone();
        s.media.preview_loader = Some(Arc::new(move |_: &str, _| {
            drop(g.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
            Some(lightcraft_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })
        }));
        let raw = |id: u64| {
            let mut p = Photo::new(PhotoId(id), Source::File { path: format!("r{id}.cr3") }, &format!("r{id}.cr3"), "CR3", 6000, 4000, "2026-01-01");
            p.kind = lightcraft_catalog::MediaKind::Raw;
            p.develop = Arc::new(p.camera_defaults());
            p
        };
        let (mut app, _, ctx) = fixture_with(s, raw(1));
        app.session.catalog.apply(Op::AddPhoto { photo: Box::new(raw(2)) }).unwrap();
        let wait = |app: &mut crate::LightcraftApp, done: &dyn Fn(&Renderer) -> bool, what: &str| {
            let t0 = std::time::Instant::now();
            while !done(&app.renderer) {
                assert!(t0.elapsed() < std::time::Duration::from_secs(20), "{what}");
                std::thread::sleep(std::time::Duration::from_millis(5));
                app.renderer.poll(&ctx, &mut app.session);
            }
        };
        let any_ahead = |r: &Renderer| r.textures.keys().any(|s| matches!(s, Slot::QuickAhead(_)));
        let held = gate.lock().unwrap();
        let job = quick(&mut app, 2, 64);
        app.renderer.look_ahead(vec![job], 96);
        wait(&mut app, &|r| r.pool.queued() == 0, "the stand-in never started");
        // not wanted any more (the user went elsewhere) while it is being made
        app.renderer.look_ahead(Vec::new(), 96);
        assert!(!app.renderer.pending.keys().any(|s| matches!(s, Slot::QuickAhead(_))));
        let done = app.renderer.completed;
        drop(held);
        wait(&mut app, &|r| r.completed > done, "the stand-in never finished");
        assert!(!any_ahead(&app.renderer), "a stand-in nobody wants is left as a hidden texture");
        // the step to photo 2 while its stand-in is being made: the loupe takes it when it arrives
        let held = gate.lock().unwrap();
        let job = quick(&mut app, 2, 32);
        app.renderer.look_ahead(vec![job], 96);
        wait(&mut app, &|r| r.pool.queued() == 0, "the stand-in never started");
        let job = quick(&mut app, 2, 32);
        app.renderer.request_quick(Slot::Preview, job, 110);
        app.renderer.look_ahead(Vec::new(), 96);
        drop(held);
        wait(&mut app, &|r| r.textures.get(&Slot::Preview).is_some_and(|t| t.photo == PhotoId(2)), "the loupe never got its stand-in");
    }

    /// The loupe waits for a stand-in made ahead that is still being made when the next look-ahead
    /// brings new photos: its slot stays reserved, so the result still reaches the loupe (CodeRabbit
    /// on #3055 and #3080).
    #[test]
    fn a_running_stand_in_the_loupe_waits_for_keeps_its_slot() {
        // the first camera preview read (photo 2's, made ahead) waits for `first`, every later one
        // for `later`
        let first = Arc::new(std::sync::Mutex::new(()));
        let later = Arc::new(std::sync::Mutex::new(()));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut s = Session::new();
        let (f, l, c) = (first.clone(), later.clone(), calls.clone());
        s.media.preview_loader = Some(Arc::new(move |_: &str, _| {
            let gate = if c.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 { &f } else { &l };
            drop(gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
            Some(lightcraft_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })
        }));
        let raw = |id: u64| {
            let mut p = Photo::new(PhotoId(id), Source::File { path: format!("r{id}.cr3") }, &format!("r{id}.cr3"), "CR3", 6000, 4000, "2026-01-01");
            p.kind = lightcraft_catalog::MediaKind::Raw;
            p.develop = Arc::new(p.camera_defaults());
            p
        };
        let (mut app, _, ctx) = fixture_with(s, raw(1));
        for id in 2..=3 {
            app.session.catalog.apply(Op::AddPhoto { photo: Box::new(raw(id)) }).unwrap();
        }
        app.renderer.pool = JobPool::new(4);
        let wait = |app: &mut crate::LightcraftApp, done: &dyn Fn(&Renderer) -> bool, what: &str| {
            let t0 = std::time::Instant::now();
            while !done(&app.renderer) {
                assert!(t0.elapsed() < std::time::Duration::from_secs(20), "{what}");
                std::thread::sleep(std::time::Duration::from_millis(5));
                app.renderer.poll(&ctx, &mut app.session);
            }
        };
        let held_first = first.lock().unwrap();
        let held_later = later.lock().unwrap();
        let job = quick(&mut app, 2, 64);
        app.renderer.look_ahead(vec![job], 96);
        wait(&mut app, &|r| r.pool.queued() == 0, "photo 2's stand-in never started");
        wait(&mut app, &|_| calls.load(std::sync::atomic::Ordering::SeqCst) == 1, "photo 2's preview was never read");
        // the step to photo 2, then the next look-ahead, for photos 3 and 1
        let job = quick(&mut app, 2, 64);
        app.renderer.request_quick(Slot::Preview, job, 110);
        assert!(app.renderer.is_pending(Slot::Preview));
        let jobs = vec![quick(&mut app, 3, 64), quick(&mut app, 1, 64)];
        app.renderer.look_ahead(jobs, 96);
        let done = app.renderer.completed;
        drop(held_first);
        wait(&mut app, &|r| r.completed > done, "photo 2's stand-in never finished");
        assert_eq!(app.renderer.textures.get(&Slot::Preview).map(|t| t.photo), Some(PhotoId(2)), "the loupe lost the stand-in it waited for");
        drop(held_later);
        wait(&mut app, &|r| r.pool.queued() == 0 && !r.pending.keys().any(|s| matches!(s, Slot::QuickAhead(_))), "the look-ahead never finished");
    }

    /// The stand-in made ahead still shows when the loupe's own request for it failed first
    /// (CodeRabbit on #3055).
    #[test]
    fn a_stand_in_made_ahead_shows_after_the_loupes_own_request_failed() {
        let (mut app, ctx) = raw_loupe(2);
        let job = quick(&mut app, 2, 64);
        let key = job.key;
        app.renderer.look_ahead(vec![job], 96);
        let job = quick(&mut app, 2, 64);
        app.renderer.request_quick(Slot::Preview, job, 110);
        assert!(app.renderer.is_pending(Slot::Preview));
        // the loupe's request fails (its job never runs)
        assert!(app.renderer.drop_queued(Slot::Preview));
        app.renderer.failed.insert(Slot::Preview, (key, "unreadable".into(), 0, PhotoId(2)));
        assert_eq!(app.renderer.pool.run_inline(1), 1);
        app.renderer.poll(&ctx, &mut app.session);
        assert_eq!(app.renderer.textures.get(&Slot::Preview).map(|t| t.photo), Some(PhotoId(2)));
        assert!(!app.renderer.failed.contains_key(&Slot::Preview));
        // the user has stepped on to photo 3 when photo 2's stand-in arrives: 3's request stays
        let (mut app, ctx) = raw_loupe(3);
        let job = quick(&mut app, 2, 64);
        let key = job.key;
        app.renderer.look_ahead(vec![job], 96);
        let job = quick(&mut app, 2, 64);
        app.renderer.request_quick(Slot::Preview, job, 110);
        assert!(app.renderer.drop_queued(Slot::Preview));
        app.renderer.failed.insert(Slot::Preview, (key, "unreadable".into(), 0, PhotoId(2)));
        let job = quick(&mut app, 3, 64);
        let key3 = job.key;
        // (queued behind the stand-in made ahead, so that one arrives first)
        app.renderer.request_quick(Slot::Preview, job, 50);
        let ahead = app.renderer.pool.run_inline(1);
        assert_eq!(ahead, 1);
        app.renderer.poll(&ctx, &mut app.session);
        assert!(app.renderer.textures.get(&Slot::Preview).is_none_or(|t| t.photo != PhotoId(2)), "photo 2 took photo 3's place");
        assert_eq!(app.renderer.pending.get(&Slot::Preview).map(|p| p.0), Some(key3), "photo 3's request is kept");
    }

    /// A stand-in made ahead for a smaller loupe isn't taken over when the loupe has grown: the
    /// larger one is made (CodeRabbit on #3077).
    #[test]
    fn a_stand_in_made_ahead_for_a_smaller_loupe_is_not_taken_over() {
        let (mut app, ctx) = raw_loupe(2);
        let small = quick(&mut app, 2, 32);
        let large = quick(&mut app, 2, 64);
        assert_eq!(small.key, large.key, "the same stand-in, at another size");
        app.renderer.look_ahead(vec![small], 96);
        assert_eq!(app.renderer.pool.run_inline(1), 1);
        app.renderer.poll(&ctx, &mut app.session);
        assert!(app.renderer.textures.keys().any(|s| matches!(s, Slot::QuickAhead(_))));
        app.renderer.request_quick(Slot::Preview, large, 110);
        assert!(app.renderer.is_pending(Slot::Preview), "the larger stand-in is made");
        assert!(!app.renderer.textures.contains_key(&Slot::Preview));
        // the same size is taken over at once
        let (mut app, ctx) = raw_loupe(2);
        let job = quick(&mut app, 2, 64);
        app.renderer.look_ahead(vec![job], 96);
        assert_eq!(app.renderer.pool.run_inline(1), 1);
        app.renderer.poll(&ctx, &mut app.session);
        let job = quick(&mut app, 2, 64);
        app.renderer.request_quick(Slot::Preview, job, 110);
        assert!(!app.renderer.is_pending(Slot::Preview));
        assert_eq!(app.renderer.textures.get(&Slot::Preview).map(|t| t.photo), Some(PhotoId(2)));
    }

    /// The benchmark's thumbnail count is per photo: a render replacing a photo's stand-in isn't
    /// counted again (CodeRabbit on #3055).
    #[test]
    fn a_thumbnail_replacing_its_stand_in_is_not_counted_again() {
        let mut s = Session::new();
        s.media.preview_loader = Some(Arc::new(|_, _| Some(lightcraft_raster::Rgba8 { width: 4, height: 4, data: vec![[200, 100, 50, 255]; 16] })));
        let mut raw = (*photo()).clone();
        raw.kind = lightcraft_catalog::MediaKind::Raw;
        raw.develop = Arc::new(raw.camera_defaults());
        let (mut app, work, ctx) = fixture_with(s, raw);
        app.renderer.pool = JobPool::new(0);
        let id = PhotoId(1);
        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        assert_eq!(app.renderer.pool.run_inline(1), 1);
        app.renderer.poll(&ctx, &mut app.session);
        assert!(app.renderer.textures.contains_key(&Slot::ThumbQuick(id)));
        assert_eq!(app.renderer.thumbs_made, 1);
        let mut edited = (*app.session.catalog.photo(id).unwrap().develop).clone();
        edited.light.exposure += 0.5;
        app.session.commit("Exposure", Op::SetDevelop { id, settings: Arc::new(edited), label: "Exposure".into(), edited: None }).unwrap();
        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        // the edited raw's camera JPEG is its stand-in first (PERF-CULL-TIERS), replacing the old one
        app.renderer.pool.run_inline(1);
        app.renderer.poll(&ctx, &mut app.session);
        assert_eq!(app.renderer.thumbs_made, 1, "a new stand-in for the same photo");
        crate::panels::grid::request_thumb(&mut app, id, 384, 10);
        let (slot, job) = take_job(&work);
        assert_eq!(slot, Slot::Thumb(id));
        finish(&mut app, &work, &ctx, slot, job.run());
        assert!(app.renderer.textures.contains_key(&Slot::Thumb(id)), "the render replaced the stand-in");
        assert_eq!(app.renderer.thumbs_made, 1, "still one photo");
    }

    /// The loupe's look-ahead asks again when only the crop changes (its stand-ins' keys include the
    /// crop), instead of keeping stand-ins the loupe can no longer use (CodeRabbit on #3077).
    #[test]
    fn the_look_ahead_asks_again_when_only_the_crop_changes() {
        let (mut app, _ctx) = raw_loupe(3);
        let ahead_keys = |r: &Renderer| r.quick_tried.iter().filter(|(s, _)| matches!(s, Slot::QuickAhead(_))).map(|(_, k)| *k).collect::<Vec<u64>>();
        crate::panels::detail::look_ahead(&mut app, PhotoId(1), 64, true);
        let cropped = app.session.quick_view_job(PhotoId(2), 64, true).unwrap().key;
        assert!(ahead_keys(&app.renderer).contains(&cropped));
        crate::panels::detail::look_ahead(&mut app, PhotoId(1), 64, false);
        let uncropped = app.session.quick_view_job(PhotoId(2), 64, false).unwrap().key;
        assert_ne!(cropped, uncropped);
        assert!(ahead_keys(&app.renderer).contains(&uncropped), "the stand-ins are made again without the crop");
    }

    /// The loupe's look-ahead asks again when the view's order changes around the same photo (a new
    /// sort, filter or source changes its neighbours without an edit; CodeRabbit on #3055).
    #[test]
    fn the_look_ahead_follows_a_new_order_around_the_same_photo() {
        let (mut app, _ctx) = raw_loupe(5);
        let ahead_keys = |r: &Renderer| r.quick_tried.iter().filter(|(s, _)| matches!(s, Slot::QuickAhead(_))).map(|(_, k)| *k).collect::<Vec<u64>>();
        app.session.sort.key = lightcraft_catalog::SortKey::FileName;
        app.session.sort.ascending = true;
        crate::panels::detail::look_ahead(&mut app, PhotoId(3), 64, true);
        let one = quick(&mut app, 1, 64).key;
        assert!(!ahead_keys(&app.renderer).contains(&one), "r1 is two behind: not prepared");
        // reversed: r1 is two ahead now
        app.session.sort.ascending = false;
        crate::panels::detail::look_ahead(&mut app, PhotoId(3), 64, true);
        assert!(ahead_keys(&app.renderer).contains(&one), "the new neighbours are prepared");
    }
}

#[cfg(test)]
mod stage_budget_tests {
    use super::*;

    const MB: usize = 1 << 20;

    // Given a zoomed view showing a window, the app idling does not free the GPU copy of the
    // original the next window is cut from; once the windows are gone the idle trim runs again
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_zoomed_view_is_not_trimmed_while_idle() {
        let ctx = egui::Context::default();
        let mut r = Renderer::default();
        let tex = ctx.load_texture("window", egui::ColorImage::new([1, 1], vec![egui::Color32::BLACK]), Default::default());
        r.textures.insert(Slot::Region, Tex { key: 1, photo: PhotoId(1), tex, size: [1, 1], histogram: None, ms: 0.0, quick: None, pixels: None });
        let long_ago = std::time::Instant::now().checked_sub(IDLE_TRIM * 2).unwrap();
        r.idle = Some((long_ago, false));
        r.trim_when_idle(&ctx);
        assert_eq!(r.idle, None, "a zoomed view is not idle");
        r.release(Slot::Region);
        r.idle = Some((long_ago, false));
        r.trim_when_idle(&ctx);
        assert_eq!(r.idle, Some((long_ago, true)), "trimmed once the window is gone");
    }

    // Given stage caches within the budget, nothing is cleared
    #[test]
    fn nothing_is_trimmed_within_the_budget() {
        let sizes = [(Slot::Main, 300 * MB), (Slot::Region, 100 * MB)];
        assert_eq!(stage_trim_order(&sizes, 512 * MB), vec![]);
    }

    // Given too much held, the caches nothing is looking at go first (a hover render's, a compare
    // tile's: rebuilt cheaply), the least useful first, and only as many as needed
    #[test]
    fn the_idle_caches_go_first_and_only_as_many_as_needed() {
        let sizes =
            [(Slot::Main, 300 * MB), (Slot::Hover, 100 * MB), (Slot::Second, 80 * MB), (Slot::Compare(0), 60 * MB), (Slot::Compare(1), 60 * MB)];
        // 600 held, 500 allowed: the hover render's 100 is enough
        assert_eq!(stage_trim_order(&sizes, 500 * MB), vec![Slot::Hover]);
        // 400 allowed: the compare tiles' too (380 left), then, at 350, the second window's
        assert_eq!(stage_trim_order(&sizes, 400 * MB), vec![Slot::Hover, Slot::Compare(0), Slot::Compare(1)]);
        assert_eq!(stage_trim_order(&sizes, 350 * MB), vec![Slot::Hover, Slot::Compare(0), Slot::Compare(1), Slot::Second]);
    }

    // Given the open photo's views over the budget, they are not trimmed: they are what a slider
    // drag reuses on every tick, and clearing them every frame costs far more than the memory
    // (issue #323 review: 3.6x slower drags on a 2800 px canvas). They are counted and reported,
    // and only a runaway (more than HARD_FACTOR times the budget) cuts the window and before
    // caches, never the whole-frame view's.
    #[test]
    fn the_open_photos_views_are_trimmed_only_in_a_runaway_and_never_the_whole_frame() {
        let views = [(Slot::Main, 400 * MB), (Slot::Region, 500 * MB), (Slot::RegionBefore, 500 * MB), (Slot::Before, 100 * MB)];
        assert_eq!(stage_trim_order(&views, 400 * MB), vec![], "1.5 GB against a 400 MB budget is a busy Before/After, not a runaway");
        // against a 100 MB budget (HARD_FACTOR 4 = 400 MB) it is: the before side goes first, then the window
        assert_eq!(stage_trim_order(&views, 100 * MB), vec![Slot::RegionBefore, Slot::Before, Slot::Region]);
        let mut with_idle = views.to_vec();
        with_idle.push((Slot::Hover, 50 * MB));
        assert_eq!(stage_trim_order(&with_idle, 400 * MB), vec![Slot::Hover]);
        // a whole-frame view of any size stays
        assert_eq!(stage_trim_order(&[(Slot::Main, 9000 * MB)], MB), vec![]);
    }

    // Given caches of slots that hold nothing, they are never "trimmed"
    #[test]
    fn empty_caches_are_left_alone() {
        assert_eq!(stage_trim_order(&[(Slot::Hover, 0), (Slot::Second, 900 * MB)], 100 * MB), vec![Slot::Second]);
    }

    // Given real stage caches over the budget, the renderer clears them (the open photo's last)
    #[test]
    fn the_renderer_trims_real_stage_caches() {
        use lightcraft_engine::pipeline::{RenderRequest, SourceInfo, render_cached};
        let src = std::sync::Arc::new(lightcraft_raster::Rgb32f::from_fn(320, 240, |x, y| [x as f32 / 320.0, y as f32 / 240.0, 0.3]));
        let mut r = Renderer::default();
        for slot in [Slot::Main, Slot::Hover] {
            let cache: Arc<StageCache> = Default::default();
            let mut s = lightcraft_engine::develop::DevelopSettings::default();
            s.effects.clarity = 50.0;
            render_cached(&src, &SourceInfo::default(), &s, &RenderRequest::fit(320, 240), &cache);
            assert!(cache.bytes() > 0);
            r.stages.insert(slot, cache);
        }
        let total: usize = r.stages.values().map(|c| c.bytes()).sum();
        r.stage_budget_override = Some(total * 3 / 4);
        r.trim_stages();
        assert_eq!(r.stages[&Slot::Hover].bytes(), 0, "the hover render's cache went");
        assert!(r.stages[&Slot::Main].bytes() > 0, "the open photo's stayed");
        assert_eq!(r.stages_trimmed, 1);
        r.stage_budget_override = Some(0);
        r.trim_stages();
        assert!(r.stages[&Slot::Main].bytes() > 0, "the open photo's view is never trimmed");
        r.trim_stages();
        assert_eq!(r.stages_trimmed, 1, "and nothing is trimmed again");
    }

    // The renderer counts and trims what it holds, and says so
    #[test]
    fn the_renderer_reports_its_stage_budget() {
        let r = Renderer::default();
        let m = r.memory();
        assert!(m["stageCaches"]["budgetBytes"].as_u64().unwrap() > 0, "{m}");
        assert_eq!(m["stageCaches"]["trimmed"], 0);
        assert!(m["stageCaches"]["sharedSourceBytes"].is_u64(), "the one device copy of a big original is reported: {m}");
    }
}

#[cfg(test)]
mod color_image_tests {
    use super::*;

    // The texture a render becomes is what egui would make of its bytes, whatever the alpha
    #[test]
    fn the_fast_conversion_equals_eguis() {
        let img = lightcraft_raster::Rgba8::from_fn(37, 29, |x, y| {
            let a = match (x + y) % 5 {
                0 => 0,
                1 => 17,
                2 => 128,
                _ => 255,
            };
            [(x * 7) as u8, (y * 9) as u8, (x * y) as u8, a]
        });
        let want = egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.as_bytes());
        let got = color_image(&img);
        assert_eq!(got.size, want.size);
        assert_eq!(got.pixels, want.pixels);
    }

    // Given a finished render, the worker hands back its texture pixels with it (the UI thread no
    // longer converts a 10 MP window on every slider step); a failed render has none
    #[test]
    fn the_worker_converts_the_texture_pixels() {
        let image = lightcraft_raster::Rgba8::from_fn(5, 3, |x, y| [(x * 40) as u8, (y * 80) as u8, 7, 255]);
        let result = |rendered| RenderResult {
            request_id: 1,
            source_key: None,
            photo: PhotoId(1),
            level: lightcraft_engine::SourceLevel::Preview,
            key: 1,
            rendered,
            gpu_fallback: None,
            loaded: None,
            colour_source: None,
            quick: None,
            display: None,
        };
        let histogram = Histogram::of_srgb8(&image);
        let ok = Renderer::run_for(|| result(Ok(lightcraft_pipeline::Rendered { image: image.clone(), histogram, deep: None })), &None);
        assert_eq!(ok.color.map(|c| c.pixels.clone()), Some(color_image(&image).pixels));
        let failed = Renderer::run_for(|| result(Err("unreadable".into())), &None);
        assert!(failed.color.is_none());
    }

    #[test]
    fn an_empty_image_is_an_empty_texture() {
        let img = lightcraft_raster::Rgba8::new(0, 0);
        assert!(color_image(&img).pixels.is_empty());
    }
}
