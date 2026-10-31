#!/usr/bin/env python3
"""Offline reference assembler for pinned model packages (stdlib only).

No model code is imported or executed. Inputs must be materialized regular files,
not Hugging Face cache symlinks or Git LFS pointer files. Native setup clients can
consume the same model-recipes JSON without requiring Python on users' machines.
"""
import argparse
import ctypes
import errno
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import sys
import tempfile

RECIPES = Path(__file__).resolve().parent / 'model-recipes'


def relative(value):
    if not isinstance(value, str) or not value or '\\' in value or '\x00' in value:
        raise ValueError(f'Unsafe relative path: {value!r}')
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in ('', '.', '..') for part in value.split('/')) or ':' in value:
        raise ValueError(f'Unsafe relative path: {value!r}')
    return path


def regular(root, name):
    """Reject symlinks in the root and every component, including parent paths."""
    path = Path(os.path.abspath(root))
    for part in (path, *path.parents):
        if part.is_symlink():
            raise ValueError(f'Symlink forbidden: {part}')
    for part in relative(name).parts:
        path = path / part
        if path.is_symlink():
            raise ValueError(f'Symlink forbidden: {path}')
    if not stat.S_ISREG(path.stat().st_mode):
        raise ValueError(f'Expected regular file: {path}')
    return path


def verified_copy(source, destination, item):
    """Verify before copying; verify copied bytes too, detecting changed sources."""
    def verify(path):
        if path.stat().st_size != item['bytes']:
            raise ValueError(f'Size mismatch: {source}')
        h = hashlib.sha256()
        with path.open('rb') as stream:
            for chunk in iter(lambda: stream.read(8 * 1024 * 1024), b''):
                h.update(chunk)
        if h.hexdigest() != item['sha256']:
            raise ValueError(f'SHA256 mismatch: {source}')
    verify(source)
    destination.parent.mkdir(parents=True, exist_ok=True)
    with source.open('rb') as stream, destination.open('xb') as target:
        shutil.copyfileobj(stream, target, 8 * 1024 * 1024)
    verify(destination)


def publish(staged, output):
    """Atomic no-replace rename, including a racing empty directory/symlink."""
    libc = ctypes.CDLL(None, use_errno=True)
    if sys.platform == 'darwin':
        function = libc.renamex_np
        args = (os.fsencode(staged), os.fsencode(output), 4)  # RENAME_EXCL
    elif sys.platform.startswith('linux') and hasattr(libc, 'renameat2'):
        function = libc.renameat2
        args = (-100, os.fsencode(staged), -100, os.fsencode(output), 1)  # RENAME_NOREPLACE
    else:
        raise ValueError('Atomic no-replace publication unavailable on this platform')
    if function(*args) != 0:
        number = ctypes.get_errno()
        raise OSError(number, os.strerror(number), str(output))


def load_recipe(model, recipes=RECIPES):
    raw = regular(recipes, f'{model}.json').read_bytes()
    recipe = json.loads(raw)
    if recipe.get('version') != 1 or recipe.get('model') != model:
        raise ValueError('Invalid recipe identity/version')
    manifest_path = regular(recipes, recipe['package']['path'])
    manifest_raw = manifest_path.read_bytes()
    if len(manifest_raw) != recipe['package']['bytes'] or hashlib.sha256(manifest_raw).hexdigest() != recipe['package']['sha256']:
        raise ValueError('Package template integrity mismatch')
    manifest = json.loads(manifest_raw)
    inventory = {item['path']: item for item in manifest['files']}
    if len(inventory) != len(manifest['files']) or set(inventory) != {item['path'] for item in recipe['files']} or len(recipe['files']) != len(inventory):
        raise ValueError('Recipe does not exactly cover package inventory')
    paths = set()
    for name in inventory:
        relative(name)
        key = name.casefold()
        if key == 'package.json' or key in paths:
            raise ValueError('Conflicting package path')
        paths.add(key)
    for entry in recipe['files']:
        relative(entry['source_path'])
        if entry['source'] not in recipe['sources']:
            raise ValueError('Unknown source')
    return recipe, manifest_raw, inventory


def prepare(model, snapshot, output, objectclear_source=None, recipes=RECIPES):
    recipe, manifest_raw, inventory = load_recipe(model, recipes)
    output = Path(os.path.abspath(output))
    if output.exists() or output.is_symlink():
        raise FileExistsError(errno.EEXIST, 'Output already exists', str(output))
    # Require an existing parent, never create directories in input locations.
    parent = output.parent
    if not parent.is_dir() or any(p.is_symlink() for p in (parent, *parent.parents)):
        raise ValueError('Output parent must exist and contain no symlinks')
    roots = {'snapshot': snapshot, 'templates': recipes}
    if objectclear_source is not None:
        roots['objectclear-source'] = objectclear_source
    sources = []
    for entry in recipe['files']:
        source_id = entry['source']
        if source_id not in roots:
            raise ValueError(f'Missing input: {source_id} (use --objectclear-source)')
        sources.append((regular(roots[source_id], entry['source_path']), entry['path']))
    with tempfile.TemporaryDirectory(prefix='.lightcraft-model-', dir=parent) as temporary:
        staged = Path(temporary) / 'package'
        staged.mkdir()
        for source, name in sources:
            verified_copy(source, staged / name, inventory[name])
        (staged / 'package.json').write_bytes(manifest_raw)
        publish(staged, output)
    return {'model': model, 'output': str(output), 'files': len(inventory),
            'bytes': sum(item['bytes'] for item in inventory.values()),
            'package_sha256': hashlib.sha256(manifest_raw).hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--model', choices=('lama', 'rorem', 'objectclear'))
    parser.add_argument('--snapshot', type=Path)
    parser.add_argument('--objectclear-source', type=Path)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--list', action='store_true', help='Print pinned sources, paths, hashes, and byte sizes')
    args = parser.parse_args()
    try:
        if args.list:
            results = []
            for model in ([args.model] if args.model else ['lama', 'rorem', 'objectclear']):
                recipe, _, inventory = load_recipe(model)
                results.append(dict(recipe, total_bytes=sum(i['bytes'] for i in inventory.values()),
                                    files=[dict(e, **{k: inventory[e['path']][k] for k in ('bytes', 'sha256')}) for e in recipe['files']]))
            print(json.dumps(results, indent=2))
        elif args.model and args.snapshot and args.output:
            print(json.dumps(prepare(args.model, args.snapshot, args.output, args.objectclear_source), indent=2))
        else:
            parser.error('--model, --snapshot, and --output are required unless using --list')
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.exit(1, f'Preparation failed: {error}\n')


if __name__ == '__main__':
    main()
