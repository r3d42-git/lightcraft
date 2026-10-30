"""Synthetic package tests; no installed models or production app access."""
import argparse
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import external_store as store
import package_runtime


class StoreTests(unittest.TestCase):
    def package(self, root):
        root.mkdir()
        (root / 'weight.bin').write_bytes(b'fixture weights')
        item = dict(version=1, kind='model', id='lama-test', backend='lama',
                    name='Carve/LaMa-ONNX', model_version='test', source='synthetic fixture',
                    adapter_version=1, runtime_compatibility=[store.RUNTIME_ID], files=store.files(root))
        store.write_json(root / 'package.json', item)
        return item

    def test_publish_is_idempotent_and_tamper_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            item = self.package(root / 'source')
            target = root / 'store'
            final = store.publish(root / 'source', target, 'models/lama/test', item)
            self.assertEqual(store.publish(root / 'source', target, 'models/lama/test', item), final)
            (final / 'weight.bin').write_bytes(b'tampered')
            with self.assertRaisesRegex(ValueError, 'hash mismatch'):
                store.publish(root / 'source', target, 'models/lama/test', item)
            self.assertEqual((root / 'source/weight.bin').read_bytes(), b'fixture weights')

    def test_interrupted_copy_never_publishes_package_or_registry(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            item = self.package(root / 'source')
            def fail_copy(source, destination):
                destination.mkdir()
                (destination / 'partial').write_bytes(b'partial')
                raise OSError('interrupted')
            with patch.object(store, 'clone_copy', side_effect=fail_copy):
                with self.assertRaises(OSError):
                    store.publish(root / 'source', root / 'store', 'models/lama/test', item)
            self.assertFalse((root / 'store/models/lama/test').exists())
            self.assertFalse((root / 'store/registry.json').exists())
            self.assertEqual(list((root / 'store/models/lama').iterdir()), [])

    def test_manifest_rejects_traversal_and_unlisted_files(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / 'package'
            item = self.package(root)
            item['files'][0]['path'] = '../outside'
            store.write_json(root / 'package.json', item)
            with self.assertRaisesRegex(ValueError, 'Invalid package path'):
                store.manifest(root)
            item['files'] = []
            store.write_json(root / 'package.json', item)
            with self.assertRaisesRegex(ValueError, 'Unlisted'):
                store.manifest(root)

    def test_symlink_integrity_and_escape(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / 'package'
            self.package(root)
            (root / 'alias').symlink_to('weight.bin')
            recorded = store.files(root)
            self.assertEqual(recorded[0]['symlink'], 'weight.bin')
            (root / 'alias').unlink()
            (root / 'alias').symlink_to('../outside')
            with self.assertRaises(ValueError):
                store.files(root)

    def test_registry_initial_default_preserved_on_rerun(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            item = self.package(root / 'source')
            target = root / 'store'
            final = store.publish(root / 'source', target, 'models/lama/test', item)
            store.register(target, [(final, item)], default_model_id='lama-test')
            self.assertEqual(store.registry(target)['defaults']['selected_model_id'], 'lama-test')
            store.register(target, [(final, item)], default_model_id='objectclear-new')
            self.assertEqual(store.registry(target)['defaults']['selected_model_id'], 'lama-test')

    def test_directory_symlinks_fail_explicitly(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'actual').mkdir()
            (root / 'alias').symlink_to('actual', target_is_directory=True)
            with self.assertRaisesRegex(ValueError, 'Directory symlinks are unsupported'):
                store.files(root)

    def test_license_notices_and_original_receipt_preserved_without_asset_copy(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = root / 'embedded'
            (source / 'licenses').mkdir(parents=True)
            (source / 'licenses/LICENSE-model.txt').write_text('permissive model license')
            (source / 'package-manifest.json').write_text('{"original":"receipt"}')
            target = root / 'store'
            first = store.migrate_notices(source, target)
            final = target / first['path']
            self.assertEqual((final / 'LICENSE-model.txt').read_text(), 'permissive model license')
            self.assertEqual((final / 'original-package-receipt.json').read_bytes(), (source / 'package-manifest.json').read_bytes())
            self.assertEqual(store.migrate_notices(source, target), first)
            self.assertEqual(store.verify_notices(target), [first['id']])
            self.assertFalse((target / 'runtimes').exists())
            self.assertFalse((target / 'models').exists())
            (final / 'LICENSE-model.txt').write_text('tampered')
            with self.assertRaisesRegex(ValueError, 'hash mismatch'):
                store.verify_notices(target)

    def test_normal_packaging_never_reads_runtime(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            worker = root / 'worker.py'
            worker.write_text('# fixture worker\n')
            args = argparse.Namespace(runtime=root / 'nonexistent-runtime', worker=worker, app=root / 'App.app')
            with patch.object(package_runtime, 'assert_closed'), patch.object(package_runtime, 'clone_copy', side_effect=AssertionError('runtime copied')), patch.object(package_runtime, 'sign_runtime', side_effect=AssertionError('runtime signed')):
                package_runtime.embed(args)
            resources = root / 'App.app/Contents/Resources'
            self.assertEqual([p.name for p in resources.iterdir()], ['ai-retouch-worker.py'])


if __name__ == '__main__':
    unittest.main()
