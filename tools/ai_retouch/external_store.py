#!/usr/bin/env python3
"""Offline immutable AI package store. Never accesses catalogues/configuration.

plan is read-only. migrate copies only the existing embedded installation;
import adds an already prepared package. Neither action replaces packages.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import runpy
import shutil
import subprocess
import tempfile
from package_runtime import audit_links, clone_copy, digest, macho_paths, validate

RUNTIME_ID = 'cpython311-ai-retouch-v1'
ARCHITECTURE = {'arm64': 'aarch64', 'x86_64': 'x86_64'}.get(platform.machine(), platform.machine()) + '-macos'


def relative(root, value):
    part = Path(value)
    if not value or part.is_absolute() or '..' in part.parts or '.' == value:
        raise ValueError('Invalid package path: ' + value)
    path = root / part
    if not path.resolve().is_relative_to(root.resolve()):
        raise ValueError('Package path escapes root: ' + value)
    return path


def files(root):
    audit_links(root)
    result = []
    for path in sorted(root.rglob('*')):
        if path.is_symlink():
            if not path.is_file():
                raise ValueError('Directory symlinks are unsupported in immutable packages: ' + str(path))
            target = os.readlink(path)
            result.append(dict(path=path.relative_to(root).as_posix(), bytes=path.stat().st_size,
                               sha256=digest(path), symlink=target))
        elif path.is_file() and path != root / 'package.json':
            result.append(dict(path=path.relative_to(root).as_posix(), bytes=path.stat().st_size,
                               sha256=digest(path)))
    return sorted(result, key=lambda item: item['path'])


def manifest(root):
    data = json.loads((root / 'package.json').read_text())
    if (data.get('version') != 1 or data.get('kind') not in ('model', 'runtime', 'notices')
            or data.get('adapter_version') != 1 or not data.get('id')
            or not data.get('model_version') or not data.get('source')):
        raise ValueError('Unsupported package descriptor')
    if data['kind'] == 'runtime' and data.get('architecture') != ARCHITECTURE:
        raise ValueError('Unsupported runtime architecture')
    if data['kind'] == 'model':
        if data.get('backend') not in ('lama', 'rorem', 'objectclear') or not data.get('runtime_compatibility'):
            raise ValueError('Unsupported model adapter/runtime')
    seen = set()
    for item in data['files']:
        path = relative(root, item['path'])
        if item['path'] in seen or not path.is_file():
            raise ValueError('Invalid or duplicate package file')
        seen.add(item['path'])
        if 'symlink' in item:
            target = os.readlink(path) if path.is_symlink() else ''
            valid = target == item['symlink'] and path.stat().st_size == item['bytes'] and digest(path) == item['sha256']
        else:
            valid = not path.is_symlink() and path.stat().st_size == item['bytes'] and digest(path) == item['sha256']
        if not valid:
            raise ValueError('Package hash mismatch: ' + item['path'])
    if files(root) != sorted(data['files'], key=lambda item: item['path']):
        raise ValueError('Unlisted or missing package files')
    return data


def native_audit(root):
    # Existing signatures are preserved. Never install_name_tool or re-sign here.
    for path in macho_paths(root):
        deps = subprocess.check_output(['/usr/bin/otool', '-L', str(path)], text=True)
        for line in deps.splitlines():
            if line.startswith((' ', '\t')):
                dep = line.strip().split(' (', 1)[0]
                if dep.startswith('/') and not dep.startswith(('/usr/lib/', '/System/Library/')):
                    raise ValueError('External native dependency: ' + dep)
        subprocess.run(['/usr/bin/codesign', '--verify', '--strict', str(path)], check=True,
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)


def registry(root):
    path = root / 'registry.json'
    if not path.exists():
        return dict(version=1, models=[], runtimes=[], defaults=dict(selected_model_id=None))
    data = json.loads(path.read_text())
    if data.get('version') != 1:
        raise ValueError('Unsupported registry version')
    for kind in ('models', 'runtimes'):
        for entry in data[kind]:
            relative(root, entry['path'])
    return data


def write_json(path, data):
    path.write_text(json.dumps(data, indent=2, sort_keys=True) + '\n')


def publish(source, store, destination, descriptor):
    final = relative(store, destination)
    if final.is_symlink():
        raise ValueError('Immutable package directory cannot be a symlink')
    if final.exists():
        if manifest(final) != descriptor:
            raise ValueError('Conflicting immutable package: ' + str(final))
        return final
    final.parent.mkdir(parents=True, exist_ok=True)
    # Atomic installation requires a sibling transaction on the destination
    # filesystem. This contains final assets, never build/backup staging.
    with tempfile.TemporaryDirectory(prefix='.incoming-', dir=final.parent) as temporary:
        staged = Path(temporary) / 'package'
        clone_copy(source, staged)
        if manifest(staged) != descriptor:
            raise ValueError('Copy validation failed')
        if final.exists():
            raise ValueError('Concurrent package install; retry')
        staged.rename(final)
    return final


def register(store, packages, default_model_id=None):
    data = registry(store)
    for path, item in packages:
        kind = 'models' if item['kind'] == 'model' else 'runtimes'
        entry = dict(id=item['id'], path=path.relative_to(store).as_posix())
        if kind == 'models':
            entry.update(backend=item['backend'], name=item['name'], model_version=item['model_version'],
                         runtime_id=item['runtime_compatibility'][0], bytes=sum(f['bytes'] for f in item['files']))
        found = next((row for row in data[kind] if row['id'] == item['id']), None)
        if found is not None and found != entry:
            raise ValueError('Conflicting registry ID: ' + item['id'])
        if found is None:
            data[kind].append(entry)
    if default_model_id is not None and not data.setdefault('defaults', {}).get('selected_model_id'):
        data['defaults']['selected_model_id'] = default_model_id
    with tempfile.NamedTemporaryFile(mode='w', prefix='.registry-', dir=store, delete=False) as stream:
        temporary = Path(stream.name)
        json.dump(data, stream, indent=2, sort_keys=True)
        stream.write('\n')
        stream.flush()
        os.fsync(stream.fileno())
    temporary.replace(store / 'registry.json')


def package_specs(worker):
    return [('lama', 'lama', worker['LAMA_MODEL'], worker['LAMA_REVISION']),
            ('model', 'rorem', worker['MODEL'], worker['REVISION']),
            ('objectclear', 'objectclear', worker['OBJECTCLEAR_MODEL'], worker['OBJECTCLEAR_REVISION'])]


def migrate(source, store, worker_path):
    source = source.resolve(strict=True)
    store = store.absolute()
    if store.resolve().is_relative_to(source) or source.is_relative_to(store.resolve()):
        raise ValueError('Store and source must be separate')
    worker = runpy.run_path(str(worker_path), run_name='store_validation')
    validate(source)  # Existing bundle full integrity + offline imports before copying.
    store.mkdir(parents=True, exist_ok=True)
    # Scratch policy is supplied by r3d/dev-env.sh via TMPDIR.
    with tempfile.TemporaryDirectory(prefix='ai-store-migration-') as temporary:
        scratch = Path(temporary)
        runtime = scratch / 'runtime'
        runtime.mkdir()
        for name in ('bin', 'lib'):
            clone_copy(source / name, runtime / name)
        audit_links(runtime)
        native_audit(runtime)
        item = dict(version=1, kind='runtime', id=RUNTIME_ID, name='LightCraft AI Python',
                    model_version='python311', source='Existing signed LightCraft embedded runtime',
                    adapter_version=1, architecture=ARCHITECTURE, files=files(runtime))
        write_json(runtime / 'package.json', item)
        final = publish(runtime, store, 'runtimes/' + RUNTIME_ID, item)
        # Prefix/import verification after relocation; no worker/model dependencies.
        validate(final, True, worker_path=worker_path)
        installed = [(final, item)]
        for folder, backend, name, revision in package_specs(worker):
            staged = scratch / backend
            clone_copy(source / folder, staged)
            item = dict(version=1, kind='model', id=backend + '-' + revision, backend=backend,
                        name=name, model_version=revision, source=name + '@' + revision,
                        adapter_version=1, runtime_compatibility=[RUNTIME_ID], files=files(staged))
            if backend == 'objectclear':
                item['source_revision'] = worker['OBJECTCLEAR_SOURCE_REVISION']
                item['code_pins'] = worker['OBJECTCLEAR_SOURCES']
            write_json(staged / 'package.json', item)
            final = publish(staged, store, 'models/' + backend + '/' + revision, item)
            installed.append((final, item))
        register(store, installed, default_model_id='objectclear-' + worker['OBJECTCLEAR_REVISION'])
        migrate_notices(source, store)
    return registry(store)


def migrate_notices(source, store):
    """Preserve original bundled notices without recopying runtime/model assets."""
    source = source.resolve(strict=True)
    notices = source / 'licenses'
    if not notices.is_dir():
        raise ValueError('Original AI license notices directory is missing')
    entries = files(notices)
    content_id = hashlib.sha256(json.dumps(entries, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
    item = dict(version=1, kind='notices', id=content_id, name='Original AI package license notices',
                model_version=content_id, source='Existing LightCraft embedded ai-retouch/licenses',
                adapter_version=1, files=entries)
    receipt = source / 'package-manifest.json'
    if receipt.is_file():
        item['source_receipt_sha256'] = digest(receipt)
    store.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='ai-store-notices-') as temporary:
        staged = Path(temporary) / 'notices'
        clone_copy(notices, staged)
        if receipt.is_file():
            clone_copy(receipt, staged / 'original-package-receipt.json')
            item['files'] = files(staged)
        write_json(staged / 'package.json', item)
        final = publish(staged, store, 'licenses/' + content_id, item)
    return dict(id=content_id, path=final.relative_to(store).as_posix(),
                files=len(item['files']), bytes=sum(entry['bytes'] for entry in item['files']))


def verify_notices(store):
    root = store / 'licenses'
    verified = []
    if root.exists():
        for path in sorted(root.iterdir()):
            if path.name.startswith('.incoming-'):
                continue
            if not path.is_dir() or path.is_symlink():
                raise ValueError('Invalid notices package directory')
            item = manifest(path)
            if item['kind'] != 'notices' or item['id'] != path.name:
                raise ValueError('Invalid notices package identity')
            verified.append(item['id'])
    return verified


def import_package(source, store):
    item = manifest(source)
    if item['kind'] == 'runtime':
        raise ValueError('Runtime imports require reviewed native dependency/import validation; use migrate')
    # Validate adapter and pinned code before registry visibility. No arbitrary code.
    worker = runpy.run_path(str(Path(__file__).with_name('worker.py')), run_name='import_validation')
    worker['package_metadata'](dict(backend=item['backend'], model_dir=str(source.resolve()),
                                    model_package=dict(path=str(source.resolve()), manifest=item)))
    if item['backend'] == 'objectclear':
        worker['verify_objectclear'](source, item)
    if any(f['path'].endswith(('.py', '.pyc', '.so', '.dylib')) and not (item['backend'] == 'objectclear' and f['path'].startswith('code/')) for f in item['files']):
        raise ValueError('Model packages cannot add executable modules')
    data = registry(store)
    runtimes = {r['id'] for r in data['runtimes']}
    if not runtimes.intersection(item['runtime_compatibility']):
        raise ValueError('No compatible installed runtime')
    final = publish(source, store, 'models/' + item['backend'] + '/' + item['model_version'], item)
    register(store, [(final, item)])
    return registry(store)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['plan', 'migrate', 'verify', 'import', 'migrate-notices'])
    parser.add_argument('--source', type=Path)
    parser.add_argument('--store', type=Path, required=True)
    parser.add_argument('--worker', type=Path, default=Path(__file__).with_name('worker.py'))
    args = parser.parse_args()
    if args.action == 'plan':
        print(json.dumps(dict(source=str(args.source), store=str(args.store), runtime_id=RUNTIME_ID,
                              packages=package_specs(runpy.run_path(str(args.worker)))), indent=2))
    elif args.action in ('migrate', 'import', 'migrate-notices'):
        args.store.mkdir(parents=True, exist_ok=True)
        with (args.store / '.store.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            if args.action == 'migrate':
                result = migrate(args.source, args.store, args.worker)
            elif args.action == 'migrate-notices':
                result = migrate_notices(args.source, args.store)
            else:
                result = import_package(args.source, args.store)
            print(json.dumps(result, indent=2))
    else:
        data = registry(args.store)
        for kind in ('runtimes', 'models'):
            for entry in data[kind]:
                item = manifest(relative(args.store, entry['path']))
                if item['id'] != entry['id']:
                    raise ValueError('Registry/package ID mismatch')
        print(json.dumps(dict(verified=True, registry=data, notices=verify_notices(args.store)), indent=2))


if __name__ == '__main__':
    main()
