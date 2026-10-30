#!/usr/bin/env python3
"""Embed the already provisioned private AI runtime. Never downloads models.

Only embed/copy-app mutate destinations; both refuse to touch a running app.
Runtime signatures are ad-hoc for this local test, not distribution signatures.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import runpy
import shutil
import subprocess
import tempfile

MACHO = {b'\xfe\xed\xfa\xce', b'\xce\xfa\xed\xfe', b'\xfe\xed\xfa\xcf',
         b'\xcf\xfa\xed\xfe', b'\xca\xfe\xba\xbe', b'\xbe\xba\xfe\xca',
         b'\xca\xfe\xba\xbf', b'\xbf\xba\xfe\xca'}
EXCLUDE = {'.cache', '__pycache__', 'pkgconfig'}


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        while chunk := stream.read(16 * 1024 * 1024):
            h.update(chunk)
    return h.hexdigest()


def assert_closed(app):
    if not app:
        return
    app = app.resolve()
    result = subprocess.check_output(['/bin/ps', '-axo', 'pid=,comm='], text=True)
    for line in result.splitlines():
        fields = line.strip().split(None, 1)
        if len(fields) == 2 and fields[1].startswith(str(app) + '/'):
            raise RuntimeError(f'Close the test app before packaging: {app} (PID {fields[0]})')


def clone_copy(source, destination):
    """APFS clone when possible; portable regular-copy fallback, no hard links."""
    if destination.exists() or destination.is_symlink():
        raise ValueError(f'Copy target must not exist: {destination}')
    result = subprocess.run(['/bin/cp', '-cR', str(source), str(destination)],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if result.returncode == 0:
        return 'apfs-clone'
    if destination.is_dir():
        shutil.rmtree(destination)
    elif destination.exists() or destination.is_symlink():
        destination.unlink()
    if source.is_dir():
        shutil.copytree(source, destination, symlinks=True)
    else:
        shutil.copy2(source, destination, follow_symlinks=False)
    return 'regular-copy'


def paths(root):
    return sorted(root.rglob('*'))


def audit_links(root):
    for path in paths(root):
        if path.is_symlink():
            target = os.readlink(path)
            if os.path.isabs(target) or not path.resolve().is_relative_to(root.resolve()):
                raise ValueError(f'Runtime symlink escapes bundle: {path}: {target}')
            if not path.exists():
                raise ValueError(f'Broken runtime symlink: {path}')


def macho_paths(root):
    for path in paths(root):
        if path.is_file() and not path.is_symlink():
            with path.open('rb') as stream:
                if stream.read(4) in MACHO:
                    yield path


def sign_runtime(root):
    binaries = list(macho_paths(root))
    for path in binaries:
        # Standalone CPython retains its build location as this dylib's own
        # LC_ID_DYLIB. This is not an active dependency, but normalize it in
        # the packaged clone so no install identity points outside the app.
        identity = subprocess.check_output(['/usr/bin/otool', '-D', str(path)], text=True).splitlines()
        identities = [line.strip() for line in identity if not line.rstrip().endswith(':')]
        if any(value.startswith('/') for value in identities):
            subprocess.run(['/usr/bin/install_name_tool', '-id', '@rpath/' + path.name, str(path)], check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        commands = subprocess.check_output(['/usr/bin/otool', '-l', str(path)], text=True).splitlines()
        external_rpaths = set()
        for i, line in enumerate(commands):
            if line.strip() == 'cmd LC_RPATH':
                rpath = commands[i+2].strip().split('path ',1)[1].split(' (offset',1)[0]
                if rpath.startswith('/') and not rpath.startswith(('/usr/lib/', '/System/Library/')):
                    external_rpaths.add(rpath)
        for rpath in sorted(external_rpaths):
            # Wheel build-machine search paths are stale fallbacks.
            # Remove them only in this disposable packaged clone;
            # actual non-system absolute dependencies still fail below.
            subprocess.run(['/usr/bin/install_name_tool', '-delete_rpath', rpath, str(path)],
                           check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        # Absolute dependencies must be macOS system libraries. @rpath and
        # @loader_path dependencies travel with their owning Python wheels.
        deps = subprocess.check_output(['/usr/bin/otool', '-L', str(path)], text=True)
        for line in deps.splitlines():
            if not line.startswith((' ', '\t')):
                continue  # Header, including repeated universal architecture headers.
            dep = line.strip().split(' (', 1)[0]
            if dep.startswith('/') and not dep.startswith(('/usr/lib/', '/System/Library/')):
                raise ValueError(f'External native dependency: {path}: {dep}')
        subprocess.run(['/usr/bin/codesign', '--force', '--sign', '-', '--timestamp=none', str(path)],
                       check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    for path in binaries:
        subprocess.run(['/usr/bin/codesign', '--verify', '--strict', str(path)], check=True,
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    return len(binaries)


def validate(root, without_models=False):
    root = root.resolve(strict=True)
    audit_links(root)
    if (root / 'pyvenv.cfg').exists():
        raise ValueError('Embedded Python must not depend on a venv home')
    script = '''import sys,json,ssl,sqlite3,tempfile
from pathlib import Path
root=Path(sys.argv[1]).resolve()
assert Path(sys.prefix).resolve()==root,sys.prefix
assert Path(sys.base_prefix).resolve()==root,sys.base_prefix
initial_paths=list(sys.path)
assert all(Path(p).resolve().is_relative_to(root) for p in initial_paths if p),initial_paths
import numpy,torch,diffusers,onnxruntime,PIL
generated_paths=[p for p in sys.path if p not in initial_paths]
assert all(Path(p).resolve().is_relative_to(Path(tempfile.gettempdir()).resolve()) for p in generated_paths),generated_paths
mods=[numpy,torch,diffusers,onnxruntime,PIL]
assert all(Path(m.__file__).resolve().is_relative_to(root) for m in mods)
print(json.dumps(dict(prefix=sys.prefix,base_prefix=sys.base_prefix,generated_temp_paths=generated_paths,versions={m.__name__:m.__version__ for m in mods})))'''
    env = dict(os.environ, HF_HUB_OFFLINE='1', TRANSFORMERS_OFFLINE='1',
               PYTHONDONTWRITEBYTECODE='1')
    info = json.loads(subprocess.check_output([str(root/'bin/python3'), '-I', '-B', '-c', script, str(root)],
                                             env=env, text=True))
    packaged = root/'package-manifest.json'
    if packaged.exists():
        manifest = json.loads(packaged.read_text())
        for item in manifest['files']:
            path = root/item['path']
            if not path.resolve().is_relative_to(root):
                raise ValueError('Invalid package manifest path')
            if path.stat().st_size != item['size'] or digest(path) != item['sha256']:
                raise ValueError(f'Packaged file integrity mismatch: {path}')
    # Load only worker definitions: no model loading, GPU work or external tools.
    worker = runpy.run_path(str(root/'worker.py'), run_name='packaging_validation')
    if not without_models:
        expected = {'model': worker['MODEL'], 'revision': worker['REVISION']}
        if json.loads((root/'model/lightcraft-model.json').read_text()) != expected:
            raise ValueError('RORem manifest does not match current source worker')
        worker['verify_objectclear'](root/'objectclear')
        expected = {'model': worker['LAMA_MODEL'], 'revision': worker['LAMA_REVISION'],
                    'sha256': worker['LAMA_SHA256']}
        if json.loads((root/'lama/manifest.json').read_text()) != expected:
            raise ValueError('LaMa manifest does not match current source worker')
        if digest(root/'lama/lama_fp32.onnx') != worker['LAMA_SHA256']:
            raise ValueError('LaMa weight digest does not match current worker')
    return info


def embed(args):
    runtime = args.runtime.resolve(strict=True)
    worker = args.worker.resolve(strict=True)
    app = args.app.resolve()
    assert_closed(app)
    if not (runtime/'lib/python3.11/os.py').is_file():
        raise ValueError('Expected standalone CPython 3.11 runtime')
    destination = app/'Contents/Resources/ai-retouch'
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists():
        raise ValueError('Embed only into a freshly assembled app; old runtime exists')
    with tempfile.TemporaryDirectory(prefix='.ai-retouch-', dir=destination.parent) as temporary:
        stage = Path(temporary)/'runtime'
        stage.mkdir()
        (stage/'bin').mkdir()
        copies = [clone_copy(runtime/'bin/python3.11', stage/'bin/python3.11'),
                  clone_copy(runtime/'lib', stage/'lib')]
        for name in ('python', 'python3'):
            (stage/'bin'/name).symlink_to('python3.11')
        if not args.without_models:
            for name in ('model', 'lama', 'objectclear'):
                copies.append(clone_copy(runtime/name, stage/name))
        # All source wheel license texts remain inside their dist-info dirs;
        # additional model/source licenses are included beside the worker.
        licenses = Path(__file__).parent/'licenses'
        if licenses.is_dir():
            copies.append(clone_copy(licenses, stage/'licenses'))
        for path in sorted(paths(stage), key=lambda p: len(p.parts), reverse=True):
            if path.is_dir() and path.name in EXCLUDE:
                shutil.rmtree(path)
        shutil.copy2(worker, stage/'worker.py')
        audit_links(stage)
        signed = sign_runtime(stage)
        info = validate(stage, args.without_models)
        entries = []
        for path in paths(stage):
            if path.is_file() and not path.is_symlink():
                entries.append(dict(path=str(path.relative_to(stage)), size=path.stat().st_size,
                                    sha256=digest(path)))
        manifest = dict(format=1, model_folders=[] if args.without_models else ['model','lama','objectclear'],
                        copy_methods=sorted(set(copies)), macho_files_signed=signed,
                        worker_sha256=digest(stage/'worker.py'), files=entries,
                        logical_bytes=sum(x['size'] for x in entries))
        (stage/'package-manifest.json').write_text(json.dumps(manifest, indent=2)+'\n')
        stage.rename(destination)
    # Prefix changes with relocation; validate again at final bundle path.
    info = validate(destination, args.without_models)
    print(json.dumps(dict(runtime=str(destination), validation=info,
                         logical_bytes=manifest['logical_bytes'],
                         macho_files_signed=signed, copy_methods=manifest['copy_methods']), indent=2))


def copy_app(args):
    source = args.source.resolve(strict=True)
    destination = args.destination.absolute()
    if source == destination:
        raise ValueError('App copy source/destination must differ')
    assert_closed(destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.ai-app-copy-', dir=destination.parent) as temporary:
        staged = Path(temporary)/destination.name
        method = clone_copy(source, staged)
        subprocess.run(['/usr/bin/codesign','--verify','--strict','--deep',str(staged)],check=True)
        # The old app is kept until a complete verified replacement is ready.
        assert_closed(destination)
        backup = Path(temporary)/'previous.app'
        if destination.exists():
            destination.rename(backup)
        try:
            staged.rename(destination)
        except BaseException:
            if backup.exists():
                backup.rename(destination)
            raise
    print(json.dumps(dict(app=str(destination),copy_method=method)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command',required=True)
    p=sub.add_parser('embed');p.add_argument('--runtime',type=Path,required=True)
    p.add_argument('--worker',type=Path,required=True);p.add_argument('--app',type=Path,required=True)
    p.add_argument('--without-models',action='store_true',help='Temporary runtime relocation verification only')
    p.set_defaults(action=embed)
    p=sub.add_parser('validate');p.add_argument('--runtime',type=Path,required=True)
    p.add_argument('--without-models',action='store_true')
    p.set_defaults(action=lambda a:print(json.dumps(validate(a.runtime,a.without_models),indent=2)))
    p=sub.add_parser('copy-app');p.add_argument('--source',type=Path,required=True)
    p.add_argument('--destination',type=Path,required=True);p.set_defaults(action=copy_app)
    p=sub.add_parser('check-closed');p.add_argument('--app',type=Path,required=True)
    p.set_defaults(action=lambda a:assert_closed(a.app))
    args=parser.parse_args();args.action(args)


if __name__=='__main__':
    main()
