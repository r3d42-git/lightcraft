//! AI denoise, the session's side (`docs/denoise.md`): which photos have a denoised picture, making the missing ones
//! in the background, and keeping the cache within its size.
//!
//! The denoised picture of a raw photo is *cached data*, never a file in the library: it is made from the untouched
//! original by the model the user chose, kept in `<library>/denoise/<key>.lcdn`, and thrown away whenever space is
//! needed (it is made again when it is next wanted). Its name is a hash of the model, the algorithm and the photo's
//! content, so a changed file or another model simply does not find it. The Denoise *amount* is an ordinary develop
//! setting (`enhance.denoise`): a render mixes the plain and the denoised picture by it ([`crate::media`]).
//!
//! What runs where: the session thread only decides (which photos, which model) and keeps an index of what is ready
//! ([`crate::media::DenoiseIndex`], memory only). One background thread at a time decodes a photo, runs the model on
//! its mosaic tile by tile and writes the product ([`make_product`]); an export that finds its photo's product missing
//! makes it itself, on its own thread, so it never comes out without the denoise it was asked for.
//!
//! Failure is a message, never a panic: a file that is not a Bayer raw, a model that does not load, a full disk, a
//! panic inside the runtime and a cancelled job all end as an [`Outcome`] the pump records.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, SystemTime};

use lightcraft_catalog::{Photo, PhotoId, Source};
use lightcraft_denoise::bayer::Layout;
use lightcraft_denoise::manifest::DenoiserManifest;
use lightcraft_denoise::run::{Control, Params, TileRunner, denoise_bayer};
use lightcraft_denoise::{Error as RunError, product};
use lightcraft_develop::{DenoiseModelPin, DevelopSettings};
use lightcraft_preview::Hasher128;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::media::{DenoiseSpec, MakeProduct, PairLoader};
use crate::merge::ByteReader;
use crate::model_download::Downloads;
use crate::{Selection, Session};

mod gpu;
mod model;
mod products;
mod session;
mod settings;
mod state;

#[cfg(test)]
pub(crate) use gpu::GPU_SETUP_MARKER;
#[cfg(all(feature = "denoise", test))]
use gpu::{GPU_CRASHED, GPU_MAX_FALLBACKS, GPU_PARALLEL, SETTING_UP, card_is_faster, set_up_card};
pub use model::RunOn;
#[cfg(all(feature = "denoise", test))]
pub(crate) use model::default_loader;
#[cfg(all(test, feature = "denoise"))]
use model::{BUILTIN_AGREE_MEAN, BUILTIN_AGREE_WORST, BuiltinModel, CHECK_NOISE, builtin_check_tile, forget_builtin_card};
#[cfg(all(feature = "denoise", test))]
use model::{GpuSide, Network, RemoteSide, network, worth_retrying};
pub(crate) use model::{Loader, Model};
#[cfg(feature = "denoise")]
pub use model::{Serving, serving};
pub(crate) use products::eligible;
#[cfg(all(test, feature = "denoise"))]
use products::loaded;
#[cfg(test)]
pub(crate) use products::{JobSpec, MakeError, Progress, make_product};
pub use settings::{DEFAULT_CACHE_GB, DIR, EXT};
pub(crate) use settings::{Settings, forget_gpu_set_ups, installed_models, read_capped, read_settings, write_atomic, write_settings};
pub(crate) use state::State;
pub use state::{Pace, PhotoState};

#[cfg(all(test, feature = "denoise"))]
mod gpu_tests;

/// Read the immutable pre-feature identity; callers that migrate must first persist it with `capture_legacy_pin`.
pub(crate) fn legacy_pin(dir: Option<&Path>) -> DenoiseModelPin {
    let st = dir.map(read_settings).unwrap_or_default();
    legacy_pin_from_settings(dir, &st)
}

fn legacy_pin_from_settings(dir: Option<&Path>, st: &Settings) -> DenoiseModelPin {
    if let Some(pin) = &st.legacy_model {
        return pin.clone();
    }
    if let Some(id) = &st.model {
        if let Some(i) = dir.and_then(|dir| installed_models(dir).into_iter().find(|i| &i.manifest.id == id)) {
            return i.pin();
        }
        return DenoiseModelPin { id: id.clone(), version: "unknown".into(), sha256: None };
    }
    let m = lightcraft_denoise::builtin::manifest();
    DenoiseModelPin { id: m.id, version: m.version, sha256: None }
}

pub(crate) fn migrate_legacy(catalog: &lightcraft_catalog::Catalog, pin: &DenoiseModelPin) -> Vec<lightcraft_catalog::Op> {
    use lightcraft_catalog::Op;
    fn assign(d: &mut Arc<DevelopSettings>, pin: &DenoiseModelPin) -> bool {
        if d.denoise_model.is_none() && d.enhance.denoise > 0.0 {
            Arc::make_mut(d).denoise_model = Some(pin.clone());
            true
        } else {
            false
        }
    }
    let mut ops = Vec::new();
    for p in catalog.photos() {
        let mut migrated = (**p).clone();
        let baseline = migrated.local_baseline == Some(migrated.develop.hash64());
        let mut changed = assign(&mut migrated.develop, pin);
        if baseline && changed {
            migrated.local_baseline = Some(migrated.develop.hash64());
        }
        for h in &mut migrated.history {
            changed |= assign(&mut h.settings, pin);
        }
        for v in &mut migrated.versions {
            changed |= assign(&mut v.settings, pin);
        }
        if changed {
            ops.push(Op::SetDenoiseState {
                id: p.id,
                settings: migrated.develop,
                history: migrated.history,
                versions: migrated.versions,
                local_baseline: migrated.local_baseline,
            });
        }
    }
    ops
}

pub(crate) use settings::{capture_legacy_pin, ensure_legacy_settings};
