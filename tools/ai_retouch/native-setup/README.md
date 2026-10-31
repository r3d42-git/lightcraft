# Native model setup helper

Build with `tools/ai_retouch/native-setup/build.sh`. The script sources the project's
validated scratch environment and writes the executable plus recipe directory to
`/Volumes/LightCraft Build/lightcraft/native-model-setup/`. Building needs Xcode
command line tools; running the resulting executable only needs macOS (Foundation,
CryptoKit, AppKit). Python is used only by the optional developer test harness and
existing project build-volume validator, never by the shipped helper.

Distribute `lightcraft-model-setup` alongside `model-recipes/`. No model weights or
runtime packages are bundled. `--help` documents CLI usage; omitted source/output
paths open native folder pickers. `--recipes` supports a separately located recipe
bundle. `--check-inputs` verifies the complete input inventory without writing a package.
`--list` reports exact assembled file sizes and pinned sources.

Example:

```sh
./lightcraft-model-setup --model lama --snapshot /path/to/pinned-snapshot --output /path/to/LaMa.modelpkg
./lightcraft-model-setup --model objectclear --snapshot /path/to/model-snapshot --objectclear-source /path/to/code-snapshot --output /path/to/ObjectClear.modelpkg
./lightcraft-model-setup --model rorem --download-script /path/to/review-download.sh
```

Download scripts contain only pinned HTTPS downloads and directory creation. Review
all upstream licenses and the script, save it in a new download directory and run it with
`LIGHTCRAFT_ACCEPT_MODEL_LICENSES=yes sh /path/to/review-download.sh`. This is an
explicit acknowledgement, not a legal license determination. The assembler checks
all file sizes and SHA-256 hashes afterwards; a download is not an accepted package
until assembly succeeds. The script changes into its own folder before writing. Interrupted scripts deliberately refuse an existing download
directory: remove only their generated `snapshot/` and `objectclear-source/` folders
and restart from an empty working directory. Downloaded model/code files are never executed.

Assembly requires a new output path, rejects symlinks in roots and selected files,
unsafe relative paths, duplicate JSON keys, duplicate/aliased inventory paths, and
file/directory collisions. It verifies sources and completed copies against the
approved exact package template, then renames a sibling staging directory into
place. Partial staging is removed after failure. Concurrent hostile modification
of source/output directories is outside the intended local setup workflow.

Import the separately approved native runtime package in LightCraft Remove → Manage models…
first, then the assembled model package. This helper cannot build, download or
approve runtimes and does not change app trust, the registry, or photo data.

Developer tests (no network/real weights):

```sh
python3 tools/ai_retouch/native-setup/test_setup.py '/Volumes/LightCraft Build/lightcraft/native-model-setup/lightcraft-model-setup'
```

Keep the binary's signing/notarization and final distributable verification in the
project release workflow. A local successful build does not establish Gatekeeper
acceptance on another Mac or confirm the native picker UI there.

For a fresh machine, create a new native AI store before selecting the AI folder in
LightCraft. The existing app requires a registry and at least one installed runtime:

```sh
./lightcraft-model-setup --create-store /path/to/NEW-LightCraft-AI
```

By default this reads `Runtimes/` and `runtime-trust.json` beside the helper. Explicit
`--runtimes DIRECTORY --trust FILE` overrides support inspecting an extracted kit.
The runtime directory must contain exactly the runtime IDs from that trusted JSON;
every listed package file, the worker binary, and pinned native dependencies are
verified. Symlinks, unlisted entries, wrong architecture/protocol and non-executable
workers fail. The helper preserves file permissions and creates a registry with no
models; it never executes a runtime. Select the resulting folder in LightCraft
Remove → Manage models…, then import verified model packages. The app checks its
own embedded trust again. Existing AI stores are never overwritten.
