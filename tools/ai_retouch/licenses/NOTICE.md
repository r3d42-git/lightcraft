# Private local AI test bundle — third-party notices

These files accompany three already-downloaded local models. No weights are
published by this packaging helper. Runtime Python dependencies retain their own
licenses in the bundled stdlib and site-packages dist-info directories.

- **RORem:** Original author leeruibin/RORem README and LetsThink/RORem model card
  declare Apache-2.0. The author's LICENSE link returns 404 at the pinned source
  revision; the declaration is retained, without inventing a missing copyright
  notice. The full Apache-2.0 terms are included with original LaMa's LICENSE.
  The author explicitly identifies SDXL Inpainting 0.1 as the pretrained base;
  its CreativeML Open RAIL++-M terms remain relevant alongside the RORem notice.
- **ObjectClear:** Unmodified pinned source carries **S-Lab License 1.0,
  non-commercial** (full text retained both here and in model code/LICENSE).
  Its model card names SDXL base 1.0; the model card does not independently
  specify a separate weights license. Do not infer Apache/MIT for its weights.
  SDXL's CreativeML Open RAIL++-M terms are included as base-model terms.
- **LaMa:** Original advimman/lama source is Apache-2.0; Carve/LaMa-ONNX model card
  also declares Apache-2.0 and identifies the original big-lama checkpoint.
  Both original author's license and exporter model card are retained; an ONNX
  format conversion alone is not used as proof of upstream licensing.
- **SDXL base and SDXL Inpainting 0.1:** Official model cards identify
  CreativeML Open RAIL++-M; the inpainting card links to the same SDXL base
  LICENSE.md included here. Its own separate LICENSE.md URL returned 404.
- Shared CLIP/OpenCLIP software license texts are retained as MIT notices; these
  software notices do not replace the SDXL model weights' RAIL terms.

Exact official source URLs, pinned revisions where available and content hashes
are recorded in sources.json and the metadata files. Missing upstream license
files and underspecified weights terms remain explicit above. This is a private
experiment, not a clearance for commercial distribution or production use.
