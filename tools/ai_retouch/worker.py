"""Private, automatically launched LightCraft image worker. No server, no downloads.

Input/output are bounded raw files in the job directory. The parent owns job
cancellation and commits only an explicitly accepted result to the catalogue.
"""
import json
import os
from pathlib import Path
import sys

MODEL = "LetsThink/RORem"
REVISION = "22d02de974c8dc92c717cd3431f8b0fe0eeecc3b"
LAMA_MODEL = "Carve/LaMa-ONNX"
LAMA_REVISION = "c3c0c9e468934d62e79c329e35d82dd09ff8c444"
LAMA_SHA256 = "1faef5301d78db7dda502fe59966957ec4b79dd64e16f03ed96913c7a4eb68d6"
OBJECTCLEAR_MODEL = "jixin0101/ObjectClear"
OBJECTCLEAR_REVISION = "c73af80888dbd519819d0a521b5c8d0f3cda6859"
OBJECTCLEAR_SOURCE_REVISION = "c052d91ecd8772744a5ab97527441c813ab83009"
OBJECTCLEAR_SOURCES = {'LICENSE': '73c84d2c2ebcdde03cb7fc853aaec0d4b3b280ee8b47f98a3d6c1931430d5aae',
 'objectclear/__init__.py': '1993604ebfb1097873491ead796e5ad3fd6711387452145f04a79aae7012e39b',
 'objectclear/models/__init__.py': 'c0a02b976d31b4aba72ee728fa9445d915ce59374f5740c595ac41c3df3b1b95',
 'objectclear/models/clip_image_encoder.py': '8deb2e9634168a8f96723a8f7487df5eaea229d36a4bdaaceed8878b7b1c596f',
 'objectclear/models/postfuse_module.py': '3384daa88be0d463b3117d5f2065387f02e6b7589f3036ecfb746ff1a39cb768',
 'objectclear/pipelines/__init__.py': '5666324f2e22aebd7954a33d4ff14f3b6d9004ccbec18756045f9e92d441d02d',
 'objectclear/pipelines/pipeline_objectclear.py': 'f0919c0dc8e2b5e5d9768b907e9d78e6182722a20b2626716311c43bbe0474bc',
 'objectclear/utils/__init__.py': '0bd93dc8b73246d7516feabfd3c9ea22721991732a9a6ead89e4db54192348dd',
 'objectclear/utils/attention_guided_fusion.py': '66f92ed57791e7e453b93f07205f8d4dcadf1b8d776898d3d824456233cbfdb1',
 'objectclear/utils/image_utils.py': 'a374fa4b0c7b7a1fb663b67fc0f2563900b7c38cf0f59b568bbb298a20881e71'}
MAX_PIXELS = 1024 * 1024


def local_path(value, root):
    path = Path(value).resolve()
    if path.parent != root:
        raise ValueError("Job files must be directly inside the private job directory")
    return path


def read_request(request_path):
    if request_path.stat().st_size > 16384:
        raise ValueError("Request is too large")
    request = json.loads(request_path.read_text())
    w, h = request.get("width"), request.get("height")
    if request.get("version") != 1 or type(w) is not int or type(h) is not int:
        raise ValueError("Unsupported request")
    if not (1 <= w <= 1024 and 1 <= h <= 1024 and w * h <= MAX_PIXELS):
        raise ValueError("Image exceeds the initial retouch resolution limit")
    if request.get("backend", "rorem") not in ("lama", "rorem", "objectclear"):
        raise ValueError("Unsupported retouch model")
    seed = request.get("seed", 0)
    steps = request.get("steps", 20)
    if type(seed) is not int or not 0 <= seed < 2**32:
        raise ValueError("Invalid seed")
    if type(steps) is not int or not 1 <= steps <= 50:
        raise ValueError("Invalid step count")
    rect = request.get("rect", [0, 0, 1, 1])
    if (not isinstance(rect, list) or len(rect) != 4
            or any(type(v) not in (int, float) or not 0 <= v <= 1 for v in rect)
            or rect[0] >= rect[2] or rect[1] >= rect[3]):
        raise ValueError("Invalid source rectangle")
    root = request_path.resolve().parent
    for key, length in (("input", w * h * 12), ("mask", w * h)):
        path = local_path(request[key], root)
        if path.stat().st_size != length:
            raise ValueError("Invalid " + key + " byte length")
    local_path(request["output"], root)
    return request


def matrices(np):
    # D65 linear RGB conversion, Rec.2020 -> sRGB; inverse is computed once.
    forward = np.array([[1.660491, -0.587641, -0.072850],
                        [-0.124550, 1.132900, -0.008349],
                        [-0.018151, -0.100579, 1.118730]], dtype=np.float32)
    return forward, np.linalg.inv(forward)


def prepare(source, np):
    forward, _ = matrices(np)
    linear = source @ forward.T
    # Present a useful exposure to SDR models even before the RAW tone curve.
    # The same scale is inverted below; it never changes the unselected original.
    scale = max(0.05, float(linear.max()))
    linear = np.clip(linear / scale, 0, 1)
    encoded = np.where(linear <= 0.0031308, 12.92 * linear,
                       1.055 * np.power(linear, 1 / 2.4) - 0.055)
    return np.rint(encoded * 255).astype(np.uint8), scale


def restore(encoded, scale, np):
    _, inverse = matrices(np)
    encoded = encoded.astype(np.float32) / 255
    linear = np.where(encoded <= 0.04045, encoded / 12.92,
                      np.power((encoded + 0.055) / 1.055, 2.4))
    return (linear @ inverse.T) * scale


def match_boundary(generated, reference, mask, np):
    """Harmonic, spatially varying boundary correction in linear source RGB.

    Solve Laplace(correction)=0 inside the selection with original-generated
    differences outside as Dirichlet boundaries. This preserves generated detail
    while matching local illumination on all sides. Original object pixels never
    enter the solve. A coarse-to-fine red/black SOR keeps 1024px jobs bounded.
    """
    from PIL import Image
    inside = mask > 0
    if not np.any(inside) or np.all(inside):
        return generated.copy()
    known = (~inside).astype(np.float32)
    delta = (reference - generated) * known[..., None]
    h, w = mask.shape

    def resize(a, size, method):
        if a.ndim == 3:
            return np.stack([resize(a[..., c], size, method) for c in range(3)], axis=-1)
        return np.asarray(Image.fromarray(a.astype(np.float32)).resize(size, method)).copy()

    sizes = [(w, h)]
    while min(sizes[-1]) > 24:
        x, y = sizes[-1]
        sizes.append((max(1, x // 2), max(1, y // 2)))
    field = None
    for size in reversed(sizes):
        weights = resize(known, size, Image.Resampling.BOX)
        boundary = resize(delta, size, Image.Resampling.BOX) / np.maximum(weights[..., None], 1e-6)
        active = weights < 0.5
        if field is None:
            field = np.zeros((*active.shape, 3), dtype=np.float32)
        else:
            field = resize(field, size, Image.Resampling.BILINEAR)
        field[~active] = boundary[~active]
        yy, xx = np.indices(active.shape)
        phases = [active & ((xx + yy) % 2 == phase) for phase in (0, 1)]
        for iteration in range(240):
            maximum = 0.0
            for phase in phases:
                p = np.pad(field, ((1, 1), (1, 1), (0, 0)), mode='edge')
                average = (p[:-2, 1:-1] + p[2:, 1:-1] + p[1:-1, :-2] + p[1:-1, 2:]) * 0.25
                change = 1.75 * (average[phase] - field[phase])
                if change.size:
                    maximum = max(maximum, float(np.max(np.abs(change))))
                    field[phase] += change
            if iteration > 15 and maximum < 1e-5:
                break
    result = generated + field
    result[~inside] = reference[~inside]
    return result


def rorem(encoded, mask, request, model_dir, np):
    import torch
    from PIL import Image
    from diffusers import AutoPipelineForInpainting
    if not torch.backends.mps.is_available():
        raise RuntimeError("Apple Metal is unavailable for AI retouch")
    manifest = json.loads((model_dir / "lightcraft-model.json").read_text())
    if manifest != {"model": MODEL, "revision": REVISION}:
        raise ValueError("The installed retouch model does not match this worker")
    h, w = mask.shape
    # Padding, rather than stretching, preserves scene geometry.
    pw, ph = max(64, (w + 7) // 8 * 8), max(64, (h + 7) // 8 * 8)
    padded = np.pad(encoded, ((0, ph-h), (0, pw-w), (0, 0)), mode="edge")
    padded_mask = np.pad(mask, ((0, ph-h), (0, pw-w)), mode="constant")
    pipe = AutoPipelineForInpainting.from_pretrained(
        str(model_dir), torch_dtype=torch.float16, use_safetensors=True,
        local_files_only=True)
    pipe.to("mps")
    # Bounded 1024px jobs fit this 64GB Mac. VAE tiling in torch 2.8 MPS
    # produces non-contiguous convolution inputs; use the unsliced path.
    seed = request.get("seed", 0)
    with torch.inference_mode():
        result = pipe(prompt="", image=Image.fromarray(padded),
                      mask_image=Image.fromarray(padded_mask),
                      width=pw, height=ph, strength=0.99,
                      guidance_scale=1.0, num_inference_steps=request.get("steps", 20),
                      generator=torch.Generator(device="cpu").manual_seed(seed)).images[0]
    return np.asarray(result.convert("RGB"))[:h, :w]


def lama(encoded, mask, model_dir, np):
    import hashlib
    import onnxruntime as ort
    from PIL import Image
    manifest = json.loads((model_dir / "manifest.json").read_text())
    expected = {"model": LAMA_MODEL, "revision": LAMA_REVISION, "sha256": LAMA_SHA256}
    model_path = model_dir / "lama_fp32.onnx"
    if manifest != expected or hashlib.sha256(model_path.read_bytes()).hexdigest() != LAMA_SHA256:
        raise ValueError("The installed LaMa model does not match this worker")
    h, w = mask.shape
    scale = min(1.0, 512 / max(w, h))
    rw, rh = max(1, round(w * scale)), max(1, round(h * scale))
    reduced = np.asarray(Image.fromarray(encoded).resize((rw, rh), Image.Resampling.LANCZOS))
    # BOX retains any selected input cell, even for very thin masks when reducing.
    reduced_mask = np.asarray(Image.fromarray(mask).resize((rw, rh), Image.Resampling.BOX)) > 0
    padded = np.pad(reduced, ((0, 512-rh), (0, 512-rw), (0, 0)), mode="edge")
    padded_mask = np.pad(reduced_mask, ((0, 512-rh), (0, 512-rw)), mode="constant")
    ort.disable_telemetry_events()
    opts = ort.SessionOptions()
    opts.intra_op_num_threads = min(8, os.cpu_count() or 1)
    opts.log_severity_level = 3
    session = ort.InferenceSession(str(model_path), sess_options=opts, providers=["CPUExecutionProvider"])
    result = session.run(None, {"image": padded.astype(np.float32).transpose(2, 0, 1)[None] / 255,
                                "mask": padded_mask.astype(np.float32)[None, None]})[0]
    if result.shape != (1, 3, 512, 512) or not np.isfinite(result).all():
        raise RuntimeError("LaMa returned invalid pixels")
    result = np.clip(result[0, :, :rh, :rw].transpose(1, 2, 0), 0, 255)
    return np.stack([np.asarray(Image.fromarray(result[..., c]).resize((w, h), Image.Resampling.BICUBIC))
                     for c in range(3)], axis=-1).clip(0, 255)


def verify_objectclear(model_dir):
    import hashlib
    manifest = json.loads((model_dir / "lightcraft-model.json").read_text())
    expected = {"model": OBJECTCLEAR_MODEL, "revision": OBJECTCLEAR_REVISION,
                "source_revision": OBJECTCLEAR_SOURCE_REVISION, "variant": "fp16"}
    if manifest != expected:
        raise ValueError("The installed ObjectClear model does not match this worker")
    code = model_dir / "code"
    for relative, digest in OBJECTCLEAR_SOURCES.items():
        if hashlib.sha256((code / relative).read_bytes()).hexdigest() != digest:
            raise ValueError("The installed ObjectClear code does not match this worker")
    return code


def objectclear(encoded, mask, request, model_dir, np):
    # Verify the small pinned custom module before importing it. The parent never
    # supplies arbitrary module names and this worker never fetches remote code.
    code = verify_objectclear(model_dir)
    sys.path.insert(0, str(code))
    import torch
    from PIL import Image
    from objectclear.pipelines import ObjectClearPipeline
    if not torch.backends.mps.is_available():
        raise RuntimeError("Apple Metal is unavailable for AI retouch")
    if request.get("steps", 20) < 2:
        raise ValueError("ObjectClear requires at least two inference steps")
    h, w = mask.shape
    # Bound even extreme aspect ratios. The engine normally sends a square
    # context; padding preserves geometry for cropped image-edge selections.
    factor = min(1.0, 512 / max(w, h))
    rw, rh = max(1, round(w * factor)), max(1, round(h * factor))
    reduced = np.asarray(Image.fromarray(encoded).resize((rw, rh), Image.Resampling.BICUBIC))
    reduced_mask = np.asarray(Image.fromarray(mask).resize((rw, rh), Image.Resampling.NEAREST))
    padded = np.pad(reduced, ((0, 512-rh), (0, 512-rw), (0, 0)), mode="edge")
    padded_mask = np.pad(reduced_mask, ((0, 512-rh), (0, 512-rw)), mode="constant")
    pipe = ObjectClearPipeline.from_pretrained_with_custom_modules(
        str(model_dir), torch_dtype=torch.float32, variant="fp16",
        apply_attention_guided_fusion=True, local_files_only=True,
        use_safetensors=True, add_watermarker=False)
    pipe.to("mps")
    with torch.inference_mode():
        result = pipe(prompt="remove the instance of object",
                      image=Image.fromarray(padded), mask_image=Image.fromarray(padded_mask),
                      width=512, height=512, guidance_scale=2.5,
                      num_inference_steps=request.get("steps", 20),
                      generator=torch.Generator(device="cpu").manual_seed(request.get("seed", 42))).images[0]
    return np.asarray(result.convert("RGB").crop((0, 0, rw, rh)).resize(
        (w, h), Image.Resampling.BICUBIC))


def run(request):
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["TRANSFORMERS_OFFLINE"] = "1"
    os.environ["HF_HUB_DISABLE_TELEMETRY"] = "1"
    import numpy as np
    w, h = request["width"], request["height"]
    source = np.fromfile(request["input"], dtype="<f4").reshape(h, w, 3)
    mask = np.fromfile(request["mask"], dtype=np.uint8).reshape(h, w)
    if not np.isfinite(source).all() or np.abs(source).max() > 65504:
        raise ValueError("Invalid source pixels")
    if not np.any(mask):
        raise ValueError("The selection is empty")
    model_dir = Path(request["model_dir"]).resolve()
    backend = request.get("backend", "rorem")
    encoded, scale = prepare(source, np)
    if backend == "lama":
        generated = lama(encoded, mask, model_dir, np)
        rgb = restore(generated, scale, np)
        model, revision, seed = LAMA_MODEL, LAMA_REVISION, 0
    elif backend == "rorem":
        generated = rorem(encoded, mask, request, model_dir, np)
        rgb = match_boundary(restore(generated, scale, np), source, mask, np)
        model, revision, seed = MODEL, REVISION, request.get("seed", 0)
    elif backend == "objectclear":
        generated = objectclear(encoded, mask, request, model_dir, np)
        rgb = match_boundary(restore(generated, scale, np), source, mask, np)
        model, revision, seed = OBJECTCLEAR_MODEL, OBJECTCLEAR_REVISION, request.get("seed", 42)
    else:
        raise ValueError("Unsupported retouch model")
    # Preserve exact source pixels wherever selection is zero, independently of model behavior.
    rgb[mask == 0] = source[mask == 0]
    rgba = np.concatenate((rgb, (mask.astype(np.float32) / 255)[..., None]), axis=2)
    if not np.isfinite(rgba).all():
        raise RuntimeError("The model produced invalid pixels")
    output = Path(request["output"])
    temporary = output.with_suffix(".part")
    rgba.astype("<f4").tofile(temporary)
    temporary.replace(output)
    return {"version": 1, "width": w, "height": h,
            "rect": request.get("rect", [0, 0, 1, 1]), "pixels": str(output),
            "model": model, "model_version": revision, "seed": seed}


def main():
    if len(sys.argv) != 3:
        raise ValueError("Expected request and response paths")
    request_path = Path(sys.argv[1]).resolve()
    response_path = local_path(sys.argv[2], request_path.parent)
    try:
        response = run(read_request(request_path))
    except Exception as error:
        import traceback
        traceback.print_exc(file=sys.stderr)
        response = {"version": 1, "error": str(error)[:2000]}
        response_path.write_text(json.dumps(response))
        return 1
    temporary = response_path.with_suffix(".part")
    temporary.write_text(json.dumps(response))
    temporary.replace(response_path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
