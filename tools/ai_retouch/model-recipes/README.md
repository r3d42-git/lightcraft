# Pinned model preparation recipes

These recipes reconstruct the exact model packages accepted by the current
LightCraft R3D worker. They contain inventories and a few deterministic local
metadata files, **no weights and no upstream executable source**. Every external
file has its original pinned repository, revision, relative path, and direct
`download_url`. Original licenses and any access conditions still apply.

A native setup client can read these JSON files directly. `prepare_model.py` is a
stdlib-only developer reference implementation, not an end-user Python runtime.
It performs no network access, imports no downloaded Python, and loads no model.
Use materialized downloaded files: Git LFS pointer files and symlink-based HF
cache directories are rejected. Unlisted snapshot files are ignored.

```sh
python3 tools/ai_retouch/prepare_model.py --list
python3 tools/ai_retouch/prepare_model.py --model lama \
  --snapshot /path/to/materialized-lama-snapshot --output /path/to/new-lama-package
python3 tools/ai_retouch/prepare_model.py --model objectclear \
  --snapshot /path/to/materialized-objectclear-snapshot \
  --objectclear-source /path/to/pinned-objectclear-source \
  --output /path/to/new-objectclear-package
```

`--output` must not exist and its parent must already exist. Preparation verifies
size and SHA256 before each copy and after copying, stages beside the destination,
and publishes through an atomic no-replace rename. Existing files, directories,
and symlinks cannot be replaced, including a destination created concurrently.
The reference supports macOS and Linux with `renameat2`; unsupported platforms
fail safely. No app, registry, or runtime is installed by this tool.

The recipe `package` entry pins exact package.json bytes. Its `files` inventory
supplies each output file's `bytes` and `sha256`. The recipe `files` list maps
exactly those paths to a named source root and `source_path`:

- `snapshot`: downloaded Hugging Face repository at the pinned revision.
- `objectclear-source`: downloaded GitHub source tree at its pinned revision;
  destination `code/` is removed from the original source path.
- `templates`: this `model-recipes/` directory, containing only local metadata.

| Model | Package inventory bytes (excluding package.json) | Files |
| --- | ---: | ---: |
| LaMa | 208,044,986 | 2 |
| RORem | 6,941,219,381 | 21 |
| ObjectClear | 7,824,984,431 | 34 |

`source-verification.json` records a read-only source mapping check: 54 original
upstream files matched every approved size/hash. Small original files were
fetched and hashed; large weight identities were checked against the pinned
Hugging Face tree's LFS SHA256/size metadata without downloading weight bytes.
This is source mapping validation, not an independent full model download or
inference test. No source transformation is necessary.

Preparation does not make a model runnable by itself. Each model needs the
separately trusted compatible native runtime package, including dependencies and
notices. The approved ObjectClear package retains ten pinned upstream source
files as package/provenance data; preparation only copies their verified bytes.
Current native workers do not execute that Python. No Python interpreter or
Python dependencies are part of these model recipes. The historical
`runtime_compatibility` values in exact package.json templates are preserved;
they do not authorize substitution of an arbitrary runtime.

Offline synthetic verification:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
  -s tools/ai_retouch -p test_prepare_model.py -v
```
