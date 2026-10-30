import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import numpy as np

spec = importlib.util.spec_from_file_location("worker", Path(__file__).with_name("worker.py"))
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


class ProtocolTests(unittest.TestCase):
    def test_external_model_revision_uses_same_adapter_and_reports_actual_revision(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = np.full((8, 8, 3), .1, dtype='<f4')
            mask = np.full((8, 8), 255, dtype=np.uint8)
            source.tofile(root / 'input.rgb')
            mask.tofile(root / 'mask.u8')
            descriptor = dict(version=1, kind='model', id='rorem-new', backend='rorem',
                              name=worker.MODEL, model_version='new-revision', adapter_version=1,
                              runtime_compatibility=['runtime-v1'])
            request = dict(width=8, height=8, backend='rorem', input=str(root / 'input.rgb'),
                           mask=str(root / 'mask.u8'), output=str(root / 'output.rgba'),
                           model_dir=str(root), model_package=dict(path=str(root), manifest=descriptor))
            self.assertEqual(worker.package_metadata(request), descriptor)
            with patch.object(worker, 'rorem', return_value=np.full((8, 8, 3), 90, dtype=np.uint8)):
                response = worker.run(request)
            self.assertEqual(response['model_version'], 'new-revision')
            descriptor['adapter_version'] = 2
            with self.assertRaisesRegex(ValueError, 'Incompatible'):
                worker.package_metadata(request)

    def test_external_models_cannot_add_arbitrary_python(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'evil.py').write_text('raise RuntimeError()')
            descriptor = dict(version=1, kind='model', id='rorem-new', backend='rorem',
                              name=worker.MODEL, model_version='new', adapter_version=1,
                              runtime_compatibility=['runtime-v1'])
            with self.assertRaisesRegex(ValueError, 'Untrusted executable'):
                worker.package_metadata(dict(backend='rorem', model_dir=str(root),
                                             model_package=dict(path=str(root), manifest=descriptor)))

    def test_rejects_oversize_before_loading_pixels(self):
        with tempfile.TemporaryDirectory() as temp:
            request = Path(temp) / "request.json"
            request.write_text(json.dumps({"version": 1, "width": 100000, "height": 100000}))
            with self.assertRaises(ValueError):
                worker.read_request(request)

    def test_rejects_job_path_escape(self):
        with tempfile.TemporaryDirectory() as temp:
            with self.assertRaises(ValueError):
                worker.local_path(Path(temp).parent / "outside", Path(temp))

    def test_rejects_nan_coordinates(self):
        with tempfile.TemporaryDirectory() as temp:
            request = Path(temp) / "request.json"
            request.write_text(json.dumps({"version": 1, "width": 1, "height": 1,
                                          "rect": [float("nan"), 0, 1, 1]}))
            with self.assertRaises(ValueError):
                worker.read_request(request)

    def test_rejects_unknown_backend_before_reading_files(self):
        with tempfile.TemporaryDirectory() as temp:
            request = Path(temp) / "request.json"
            request.write_text(json.dumps({"version": 1, "width": 1, "height": 1,
                                          "backend": "remote-command"}))
            with self.assertRaisesRegex(ValueError, "Unsupported retouch model"):
                worker.read_request(request)

    def test_lama_requires_pinned_manifest_before_loading_runtime(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "manifest.json").write_text('{}')
            image = np.zeros((8, 8, 3), dtype=np.uint8)
            mask = np.ones((8, 8), dtype=np.uint8)
            with self.assertRaisesRegex(ValueError, "does not match"):
                worker.lama(image, mask, root, np)

    def test_objectclear_rejects_unpinned_runtime_before_import(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "lightcraft-model.json").write_text('{}')
            with self.assertRaisesRegex(ValueError, "does not match"):
                worker.verify_objectclear(root)

    def test_objectclear_preserves_unselected_source_and_model_identity(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = np.full((8, 8, 3), .1, dtype='<f4')
            mask = np.zeros((8, 8), dtype=np.uint8)
            mask[2:6, 2:6] = 255
            source.tofile(root / 'input.rgb')
            mask.tofile(root / 'mask.u8')
            request = dict(width=8, height=8, backend='objectclear', seed=42,
                           input=str(root / 'input.rgb'), mask=str(root / 'mask.u8'),
                           output=str(root / 'output.rgba'), model_dir=str(root))
            with patch.object(worker, 'objectclear', return_value=np.full((8, 8, 3), 90, dtype=np.uint8)):
                response = worker.run(request)
            result = np.fromfile(root / 'output.rgba', dtype='<f4').reshape(8, 8, 4)
            np.testing.assert_array_equal(result[mask == 0, :3], source[mask == 0])
            np.testing.assert_array_equal(result[..., 3], mask / 255)
            self.assertEqual(response['model'], worker.OBJECTCLEAR_MODEL)
            self.assertEqual(response['model_version'], worker.OBJECTCLEAR_REVISION)
            self.assertEqual(response['seed'], 42)

    def test_srgb_reference_roundtrip_keeps_hdr_scale(self):
        # Source corresponding to in-gamut sRGB values, including super-white.
        _, inverse = worker.matrices(np)
        source = np.array([[[0.1, 0.5, 2.0], [0.8, 0.2, 0.3]]], dtype=np.float32) @ inverse.T
        image, scale = worker.prepare(source, np)
        result = worker.restore(image, scale, np)
        self.assertGreater(scale, 1.9)
        np.testing.assert_allclose(result, source, atol=0.012)

    def test_dark_raw_normalization_is_inverted_without_lifting_brightness(self):
        _, inverse = worker.matrices(np)
        source = np.array([[[.01, .05, .2], [.08, .02, .03]]], dtype=np.float32) @ inverse.T
        image, scale = worker.prepare(source, np)
        self.assertLess(scale, .21)
        self.assertEqual(int(image.max()), 255)
        np.testing.assert_allclose(worker.restore(image, scale, np), source, atol=.0012)

    def test_boundary_match_removes_colour_offset_without_copying_object(self):
        reference = np.full((64, 64, 3), .4, dtype=np.float32)
        mask = np.zeros((64, 64), dtype=np.uint8)
        mask[24:40, 24:40] = 255
        reference[24:40, 24:40] = .8  # object must not enter colour fit
        generated = np.full((64, 64, 3), [.44, .46, .48], dtype=np.float32)
        corrected = worker.match_boundary(generated, reference, mask, np)
        np.testing.assert_allclose(corrected, np.full((64, 64, 3), .4), atol=1e-4)

    def test_boundary_match_follows_varying_illumination_and_preserves_detail(self):
        yy, xx = np.indices((96, 96), dtype=np.float32)
        base = .2 + .003 * xx + .002 * yy
        reference = np.repeat(base[..., None], 3, axis=-1)
        generated = reference + (.05 + xx[..., None] * .001)
        generated[45:50, 45:50] += .03  # generated texture must survive
        mask = np.zeros((96, 96), dtype=np.uint8)
        mask[20:76, 20:76] = 255
        reference[25:70, 25:70] = 0  # original object is excluded
        corrected = worker.match_boundary(generated, reference, mask, np)
        expected = np.repeat(base[..., None], 3, axis=-1)
        expected[45:50, 45:50] += .03
        np.testing.assert_allclose(corrected[mask > 0], expected[mask > 0], atol=5e-4)

    def test_boundary_match_full_mask_has_no_reference_and_stays_finite(self):
        reference = np.full((8, 8, 3), 200, dtype=np.uint8)
        generated = np.full((8, 8, 3), 30, dtype=np.uint8)
        mask = np.full((8, 8), 255, dtype=np.uint8)
        np.testing.assert_array_equal(worker.match_boundary(generated, reference, mask, np), generated)


if __name__ == "__main__":
    unittest.main()
