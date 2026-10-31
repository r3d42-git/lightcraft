"""Small offline fixtures exercise preparation without model assets or imports."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import prepare_model as prep


class PreparationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.recipes = self.root / 'recipes'
        self.snapshot = self.root / 'snapshot'
        self.recipes.mkdir()
        self.snapshot.mkdir()
        self.output = self.root / 'output'
        (self.snapshot / 'weight').write_bytes(b'test-weight')
        (self.recipes / 'metadata').write_bytes(b'{"test":true}\n')
        self.entries = [{'path': 'component/weight', 'source': 'snapshot', 'source_path': 'weight'},
                        {'path': 'manifest.json', 'source': 'templates', 'source_path': 'metadata'}]
        self.write_recipe()

    def write_recipe(self):
        inventory = []
        for entry in self.entries:
            source = self.snapshot if entry['source'] == 'snapshot' else self.recipes
            raw = (source / entry['source_path']).read_bytes()
            inventory.append({'path': entry['path'], 'bytes': len(raw),
                              'sha256': hashlib.sha256(raw).hexdigest()})
        self.package = json.dumps({'files': inventory}).encode()
        (self.recipes / 'package.json').write_bytes(self.package)
        self.recipe = {'version': 1, 'model': 'test', 'sources': {'snapshot': {}, 'templates': {}},
                       'package': {'path': 'package.json', 'bytes': len(self.package),
                                   'sha256': hashlib.sha256(self.package).hexdigest()},
                       'files': self.entries}
        self.save()

    def save(self):
        (self.recipes / 'test.json').write_text(json.dumps(self.recipe))

    def prepare(self):
        return prep.prepare('test', self.snapshot, self.output, recipes=self.recipes)

    def assert_clean_failure(self, exception=ValueError):
        with self.assertRaises(exception):
            self.prepare()
        self.assertFalse(self.output.exists())
        self.assertFalse(list(self.root.glob('.lightcraft-model-*')))

    def test_exact_inventory_and_metadata(self):
        result = self.prepare()
        self.assertEqual(result['files'], 2)
        self.assertEqual((self.output / 'component/weight').read_bytes(), b'test-weight')
        self.assertEqual((self.output / 'package.json').read_bytes(), self.package)
        self.assertEqual(sorted(str(p.relative_to(self.output)) for p in self.output.rglob('*') if p.is_file()),
                         ['component/weight', 'manifest.json', 'package.json'])

    def test_wrong_hash(self):
        (self.snapshot / 'weight').write_bytes(b'wrong-value')
        self.assert_clean_failure()

    def test_wrong_size(self):
        (self.snapshot / 'weight').write_bytes(b'bad')
        self.assert_clean_failure()

    def test_unsafe_source_paths(self):
        for name in ('../outside', '/absolute', 'a/../weight', 'a//weight', 'a\\weight', 'C:/weight'):
            with self.subTest(name=name):
                self.recipe['files'][0]['source_path'] = name
                self.save()
                self.assert_clean_failure()

    def test_unsafe_destination(self):
        self.recipe['files'][0]['path'] = '../outside'
        self.save()
        self.assert_clean_failure()

    def test_file_symlink(self):
        (self.snapshot / 'real').write_bytes((self.snapshot / 'weight').read_bytes())
        (self.snapshot / 'weight').unlink()
        (self.snapshot / 'weight').symlink_to('real')
        self.assert_clean_failure()

    def test_directory_symlink(self):
        alias = self.root / 'alias'
        alias.symlink_to(self.snapshot, target_is_directory=True)
        self.snapshot = alias
        self.assert_clean_failure()

    def test_existing_empty_directory_not_overwritten(self):
        self.output.mkdir()
        with self.assertRaises(FileExistsError):
            self.prepare()
        self.assertTrue(self.output.is_dir())

    def test_existing_dangling_symlink_not_overwritten(self):
        self.output.symlink_to(self.root / 'missing')
        with self.assertRaises(FileExistsError):
            self.prepare()
        self.assertTrue(self.output.is_symlink())

    def test_racing_destination_not_overwritten(self):
        original = prep.publish
        def race(staged, output):
            output.mkdir()
            return original(staged, output)
        with patch.object(prep, 'publish', race):
            with self.assertRaises(FileExistsError):
                self.prepare()
        self.assertTrue(self.output.is_dir())
        self.assertFalse(list(self.output.iterdir()))

    def test_changed_source_during_copy(self):
        def corrupt(source, target, length):
            target.write(b'wrong-value')
        with patch.object(prep.shutil, 'copyfileobj', corrupt):
            self.assert_clean_failure()

    def test_missing_source_tree(self):
        self.recipe['files'][0]['source'] = 'objectclear-source'
        self.recipe['sources']['objectclear-source'] = {}
        self.save()
        self.assert_clean_failure()

    def test_package_template_tampered(self):
        (self.recipes / 'package.json').write_bytes(b'{}')
        self.assert_clean_failure()

    def test_inventory_requires_each_file_once(self):
        self.recipe['files'].append(dict(self.recipe['files'][0]))
        self.save()
        self.assert_clean_failure()

    def test_shipped_recipe_metadata_hashes(self):
        for model in ('lama', 'rorem', 'objectclear'):
            recipe, _, inventory = prep.load_recipe(model)
            for entry in recipe['files']:
                if entry['source'] == 'templates':
                    raw = prep.regular(prep.RECIPES, entry['source_path']).read_bytes()
                    self.assertEqual(len(raw), inventory[entry['path']]['bytes'])
                    self.assertEqual(hashlib.sha256(raw).hexdigest(), inventory[entry['path']]['sha256'])


if __name__ == '__main__':
    unittest.main()
