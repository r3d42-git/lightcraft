# External offline AI store

Normal `build-ai-local.sh` / `build-ai-dev.sh` builds copy only
`Contents/Resources/ai-retouch-worker.py`. They never read, hash, copy, modify or
sign the external Python/runtime/model assets. Existing Mach-O signatures are
preserved during one-time migration. No network requests are made.

Source `r3d/dev-env.sh` in **bash** before these commands. It verifies the separate
APFS scratch volume and places temporary preparation there. Migration uses a
short-lived `.incoming-*` transaction adjacent to each final package because
atomic rename cannot cross APFS volume boundaries. This transaction is final
asset installation, not build or backup staging. The installed app remains
untouched. No catalogue, configuration or edit files are accessed.

```bash
source r3d/dev-env.sh
python3 tools/ai_retouch/external_store.py plan \
  --source '/Applications/LightCraft R3D.app/Contents/Resources/ai-retouch' \
  --store /Volumes/Media/LightCraft/AI
python3 tools/ai_retouch/external_store.py migrate \
  --source '/Applications/LightCraft R3D.app/Contents/Resources/ai-retouch' \
  --store /Volumes/Media/LightCraft/AI
python3 tools/ai_retouch/external_store.py verify --store /Volumes/Media/LightCraft/AI
```

Migration validates the old embedded manifest, hashes all package contents,
checks native dependencies/signatures and runs Python imports offline before
and after relocation. Final packages are immutable: a repeated migration accepts
identical contents and rejects changed/conflicting contents without replacing
them. Registry publication occurs only after all packages validate. An interrupted
transaction cannot expose a partial registered package. A process killed without
cleanup may leave an unreferenced `.incoming-*` folder; inspect it before removing
it. The app/library/configuration rollback set is managed separately.

`registry.json` version 1 lists `runtimes`, `models` and
`defaults.selected_model_id`, plus optional per-model
`defaults.runtime_overrides`. Registry paths are relative to the store root.
Each package contains `package.json`: `version`, `kind`, immutable `id`, `name`,
`model_version`, `source`, `adapter_version: 1` and `files` records with relative
`path`, `bytes`, `sha256`. Runtime packages add `architecture` (`aarch64-macos`
on Apple Silicon). Model packages add `backend` (`lama`, `rorem`, `objectclear`)
and `runtime_compatibility`. Symlink records include their relative `symlink`
target and hash/size of the resolved file; escaping/broken links and directory symlinks are rejected explicitly.
All files, including symlinks and native libraries, are covered. The manifest
itself is excluded from its file list.

Migrated model revisions live in `models/<backend>/<revision>`, with IDs
`<backend>-<revision>`. The migration runtime is
`runtimes/cpython311-ai-retouch-v1`. Changes to this runtime require a separately
reviewed immutable runtime ID; this tool does not import arbitrary runtimes.

A prepared, fully hashed model package can be imported offline:

```bash
python3 tools/ai_retouch/external_store.py import \
  --source /path/to/prepared/model-package --store /Volumes/Media/LightCraft/AI
```

A new Python-worker model revision must declare the same supported backend/family and adapter
version and an installed compatible runtime. New Python/native code is rejected.
ObjectClear's `code_pins` must exactly match the trusted worker's pinned source
hashes; every executable source file must belong to this allowlist. Model package
metadata is passed as `model_package: {path, manifest}` to the worker, which
reports the actual `model_version` in results. Legacy worker requests retain their
old pinned-model checks. Initial migration selects ObjectClear when no default is set; repeated migration preserves
an existing selection. Import/verify never change the selected default.

## Optional reviewed native runtimes

The engine also supports separately installed runtime packages with
`execution: "native-protocol-v1"` and the fixed entrypoint
`bin/lightcraft-ai-native`. This is an additional execution path; omitted
`execution` and explicit `python-protocol-v1` retain the existing Python worker
behavior. Normal app builds do not install or update either external runtime.

Native runtime approval is compiled into the app from
`crates/engine/src/ai_models_native_trust.json`. Each approved entry binds the
immutable runtime ID and executable SHA-256 to one model ID and the SHA-256 of
that model's exact `package.json`. Editing the external registry or manifest
cannot approve another binary or model revision. Import validates the complete
package inventory and hashes; runtime resolution and the pre-launch check verify
the native executable and exact model-manifest binding. The entrypoint must be
an executable ordinary file, and the package architecture must match the host.
A changed binary or model manifest requires a separately reviewed app trust entry.

Use the engine command `aiModels.import {"path":"/absolute/unpacked/package"}`
for an approved native runtime package, followed by its separately prepared model
package. The Python `external_store.py` commands above remain the migration and
Python-model workflow; they are not a native-runtime installer. Engine imports
place runtime packages in `runtimes/<id>` and model packages in `models/<id>`.
They copy through an adjacent `.incoming-*` transaction and publish the registry
only after validation. Unknown native binaries, incompatible architectures,
conflicting immutable revisions and arbitrary Python runtime imports are rejected.

A fresh native-only store can import the approved runtime first and then its
exact bound model without installing Python. The model remains a separate
package, with the existing supported family/adapter and inventory validation.
When a compatible Python runtime is already installed, model import preserves
the Python preference. Adding a native runtime does not change existing models,
their recorded `runtime_id`, or the selected model default.

Select an alternative explicitly with
`aiModels.setRuntime {"modelId":"<model-id>","runtimeId":"<runtime-id>"}`.
This writes `defaults.runtime_overrides[modelId]` after validating the pairing.
Passing `"runtimeId":null` clears the override and restores that model's recorded
`runtime_id`; clearing is not an automatic migration. A missing, damaged or
incompatible selected runtime returns an error. Execution does not silently retry
with Python, another native runtime, or another model.

`aiModels.removeRuntime {"id":"<runtime-id>"}` rejects runtimes referenced by
any model's recorded `runtime_id` or any explicit override. Clearing an override
alone therefore does not release a native-only model's recorded runtime.
`aiModels.remove` also clears that model's override; removing the selected model
sets the selected default to the first remaining model, if any. Package removal
uses a `.removing-*` transaction with registry-write rollback. Interrupted
transactions require inspection and cleanup before retrying. Imports, removals,
store-folder changes and runtime selection are blocked while AI Remove is busy. Saved retouch raster
patches remain independent of runtime selection and are not recomputed.

The current ObjectClear-specific runtime implementation and prepared test package
are private experimental material outside this product repository. They are not
a publication-ready installer or a completed macOS/CI acceptance result. The
package includes S-Lab non-commercial material; this document does not establish
redistribution rights. Separate installation by other users remains the intended
delivery model, subject to reviewing the exact package, notices, distribution
terms and installation/acceptance workflow before publication.

Focused tests (temporary synthetic fixtures on scratch):

```bash
source r3d/dev-env.sh
PYTHONDONTWRITEBYTECODE=1 \
  '/Applications/LightCraft R3D.app/Contents/Resources/ai-retouch/bin/python3' -B \
  -m unittest discover -s tools/ai_retouch -p 'test_*.py'
```

Original top-level model/source notices are preserved as a separate immutable
`licenses/<content-id>` package. Its `package.json` has `kind: "notices"`, full
file hashes and the SHA-256 of the original embedded package receipt; the receipt
itself is retained as `original-package-receipt.json`. These packages are separate
from the engine's model/runtime registry. `verify` validates their full inventory
too. The ordinary `migrate` command includes this preservation automatically.

For an already migrated store, copy **only** notices and receipt without reading
or recopying runtime/model weights:

```bash
source r3d/dev-env.sh
python3 tools/ai_retouch/external_store.py migrate-notices \
  --source '/Applications/LightCraft R3D.app/Contents/Resources/ai-retouch' \
  --store /Volumes/Media/LightCraft/AI
```
