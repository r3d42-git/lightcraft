#!/usr/bin/env python3
"""Developer-only assembly of a weight-free optional setup candidate.

Does not import into an AI store, change signatures, or publish any artifact.
Use r3d/build-ai-setup.sh so staging stays on the approved scratch volume.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile


def sha(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def plain(root, name):
    parts = name.split('/')
    if not name or any(p in ('', '.', '..') for p in parts) or '\\' in name:
        raise ValueError(f'Unsafe package path: {name}')
    path = root
    for part in parts:
        path = path / part
        if path.is_symlink():
            raise ValueError(f'Symlink rejected: {path}')
    if not path.is_file():
        raise ValueError(f'Ordinary file required: {path}')
    return path


def verified_runtime(root, pin):
    manifest = json.loads(plain(root, 'package.json').read_text())
    if (manifest.get('id') != pin['id'] or manifest.get('kind') != 'runtime'
            or manifest.get('execution') != 'native-protocol-v1'
            or manifest.get('architecture') != 'aarch64-macos'):
        raise ValueError(f'Runtime identity mismatch: {root}')
    names = set()
    for entry in manifest['files']:
        name = entry['path']
        if name in names:
            raise ValueError(f'Duplicate inventory path: {name}')
        names.add(name)
        path = plain(root, name)
        if path.stat().st_size != entry['bytes'] or sha(path) != entry['sha256']:
            raise ValueError(f'Runtime inventory mismatch: {path}')
    actual = {p.relative_to(root).as_posix() for p in root.rglob('*') if p.is_file() or p.is_symlink()}
    if actual != names | {'package.json'}:
        raise ValueError(f'Unexpected or missing runtime files: {root}')
    worker = plain(root, 'bin/lightcraft-ai-native')
    if sha(worker) != pin['binary_sha256'] or not worker.stat().st_mode & stat.S_IXUSR:
        raise ValueError(f'Unapproved runtime binary: {worker}')
    for name, digest in pin.get('dependency_sha256', {}).items():
        if sha(plain(root, name)) != digest:
            raise ValueError(f'Unapproved dependency: {name}')
    subprocess.run(['/usr/bin/codesign', '--verify', '--strict', str(worker)], check=True)
    return manifest


LAUNCHER = '''#!/bin/bash
set -euo pipefail
cd -- "$(dirname -- "$0")"
echo 'LightCraft KI: Modellgewichte bleiben separate Originaldownloads.'
echo '0) Neue KI-Ablage einrichten (einmalig)'
echo '1) LaMa  2) RORem  3) ObjectClear'
read -r -p 'Aktion wählen (0/1/2/3): ' choice
if [ "$choice" = 0 ]; then
  echo 'ANLEITUNG.md und Model-Notices lesen. Der optionale ObjectClear-Zusatz ist nur nichtkommerziell.'
  read -r -p 'Bedingungen gelesen und akzeptiert? (ja): ' accepted
  [ "$accepted" = ja ] || exit 1
  read -r -p 'Neuer dauerhafter KI-Ordner (absoluter Pfad, darf noch nicht existieren): ' store
  ./lightcraft-model-setup --create-store "$store"
  echo 'In LightCraft: Entfernen → Modelle verwalten… → Bestehende KI-Ablage wählen…'
  read -r -p 'Zum Schließen Return drücken. ' ignored
  exit 0
fi
case "$choice" in 1) model=lama;; 2) model=rorem;; 3) model=objectclear;; *) exit 1;; esac
echo 'Bitte zuerst ANLEITUNG.md und Model-Notices lesen.'
if [ "$model" = objectclear ]; then
  echo 'ObjectClear und sein nativer Zusatz: S-Lab License 1.0, nur nichtkommerziell.'
fi
read -r -p 'Bedingungen gelesen und akzeptiert? (ja): ' accepted
[ "$accepted" = ja ] || exit 1
echo '1) Bereits geladene Dateien vorbereiten (native Ordnerauswahl)'
echo '2) Downloadskript erzeugen; anschließend selbst starten'
read -r -p 'Aktion (1/2): ' action
case "$action" in
  1) ./lightcraft-model-setup --model "$model" ;;
  2) read -r -p 'Neuer Pfad für Downloadskript (z.B. /Users/name/Downloads/lama-download.command): ' script
     ./lightcraft-model-setup --model "$model" --download-script "$script"
     echo 'Skript prüfen. In einem leeren Ordner mit LIGHTCRAFT_ACCEPT_MODEL_LICENSES=yes starten.' ;;
  *) exit 1;;
esac
read -r -p 'Zum Schließen Return drücken. ' ignored
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runtime-root', required=True, type=Path)
    parser.add_argument('--helper', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[2]
    output = args.output.absolute()
    if output.exists() or output.is_symlink():
        raise ValueError('Output already exists; use a new staging folder.')
    output.parent.mkdir(parents=True, exist_ok=True)
    trust = json.loads((repo / 'crates/engine/src/ai_models_native_trust.json').read_text())
    pins = trust['runtimes']
    if len(pins) != 3:
        raise ValueError('Review packaging for the changed native runtime catalogue.')
    if not args.helper.is_file() or args.helper.is_symlink():
        raise ValueError('Native helper binary required.')
    for pin in pins:
        verified_runtime(args.runtime_root / pin['id'], pin)
    stage = Path(tempfile.mkdtemp(prefix='.setup-kit-', dir=output.parent))
    try:
        shutil.copy2(args.helper, stage / 'lightcraft-model-setup')
        os.chmod(stage / 'lightcraft-model-setup', 0o755)
        shutil.copytree(repo / 'tools/ai_retouch/model-recipes', stage / 'model-recipes',
                        ignore=shutil.ignore_patterns('__pycache__', '*.pyc'))
        shutil.copytree(repo / 'tools/ai_retouch/licenses', stage / 'Model-Notices')
        shutil.copy2(repo / 'docs/r3d-ai-install.md', stage / 'ANLEITUNG.md')
        for license_name in ('LICENSE-MIT', 'LICENSE-APACHE'):
            shutil.copy2(repo / license_name, stage / license_name)
        (stage / 'KI einrichten.command').write_text(LAUNCHER)
        os.chmod(stage / 'KI einrichten.command', 0o755)
        shutil.copy2(repo / 'crates/engine/src/ai_models_native_trust.json', stage / 'runtime-trust.json')
        for pin in pins:
            target = stage / 'Runtimes' / pin['id']
            shutil.copytree(args.runtime_root / pin['id'], target)
            verified_runtime(target, pin)
        result = {
            'schema': 1, 'candidate_only': True, 'published': False,
            'weights_included': False, 'python_required_by_end_user': False,
            'architecture': 'aarch64-macos', 'minimum_macos': '13.4',
            'runtime_ids': [p['id'] for p in pins],
            'runtime_trust': trust,
            'publication_gates': [
                'Developer-ID signing and notarization of final helper and runtimes',
                'New signed runtime identities and matching app trust bindings',
                'User test of setup and inference on a further Mac',
            ],
        }
        (stage / 'CANDIDATE.json').write_text(json.dumps(result, indent=2) + '\n')
        lines = []
        for path in sorted(stage.rglob('*')):
            if path.is_file():
                relative = path.relative_to(stage).as_posix()
                if path.suffix.lower() in ('.onnx', '.safetensors', '.ckpt', '.pt', '.pth'):
                    raise ValueError(f'Model weight unexpectedly included: {relative}')
                lines.append(f'{sha(path)}  {relative}\n')
        (stage / 'SHA256SUMS').write_text(''.join(lines))
        stage.rename(output)
    finally:
        if stage.exists():
            shutil.rmtree(stage)
    print(json.dumps({'output': str(output), 'files': len(lines),
                      'bytes': sum(p.stat().st_size for p in output.rglob('*') if p.is_file()),
                      'candidate_only': True, 'weights_included': False}, indent=2))


if __name__ == '__main__':
    main()
