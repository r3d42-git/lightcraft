#!/usr/bin/env python3
"""Developer-only synthetic CLI tests; no models, UI, network or production data."""
import hashlib, json, pathlib, subprocess, sys, tempfile
binary = pathlib.Path(sys.argv[1]).resolve()
def encoded(value): return (json.dumps(value, indent=2) + '\n').encode()
def h(path, data): return dict(path=path, bytes=len(data), sha256=hashlib.sha256(data).hexdigest())
with tempfile.TemporaryDirectory(prefix='native-setup-test-') as tmp:
    root = pathlib.Path(tmp).resolve(); recipes = root/'recipes'; recipes.mkdir(); (recipes/'lama').mkdir()
    snapshot = root/'snapshot'; snapshot.mkdir(); payload = b'synthetic ONNX bytes\0'
    (snapshot/'weights.onnx').write_bytes(payload)
    manifest = dict(files=[h('weights.onnx', payload)])
    recipe = dict(version=1, model='lama', package={}, sources={'snapshot':dict(kind='huggingface', repository='example/test',revision='a'*40,url='https://example.invalid/source')}, files=[dict(path='weights.onnx',source='snapshot',source_path='weights.onnx')])
    def save():
        data = encoded(manifest); (recipes/'lama/package.json').write_bytes(data)
        recipe['package'] = h('lama/package.json', data); (recipes/'lama.json').write_bytes(encoded(recipe))
    def run(ok, *extra):
        result = subprocess.run([str(binary),'--recipes',str(recipes),'--model','lama',*extra],capture_output=True,text=True)
        assert (result.returncode == 0) == ok, (result.returncode,result.stdout,result.stderr)
        return result
    args = ['--snapshot',str(snapshot),'--output',str(root/'result.modelpkg')]
    save(); run(True,'--snapshot',str(snapshot),'--check-inputs'); run(True,*args)
    assert (root/'result.modelpkg/weights.onnx').read_bytes() == payload
    run(False,*args) # existing output
    (snapshot/'weights.onnx').write_bytes(b'wrong'); run(False,'--snapshot',str(snapshot),'--output',str(root/'bad.modelpkg'))
    assert not (root/'bad.modelpkg').exists()
    (snapshot/'weights.onnx').unlink(); (snapshot/'weights.onnx').symlink_to(root/'result.modelpkg/weights.onnx')
    run(False,'--snapshot',str(snapshot),'--output',str(root/'link.modelpkg'))
    (snapshot/'weights.onnx').unlink(); (snapshot/'weights.onnx').write_bytes(payload)
    recipe['files'][0]['source_path']='../escape'; save(); run(False,*args)
    recipe['files'][0]['source_path']='weights.onnx'
    manifest['files'].append(h('WEIGHTS.onnx',payload)); save(); run(False,*args)
    manifest['files'].pop(); save()
    recipe['files'].append(recipe['files'][0]); save(); run(False,*args); recipe['files'].pop(); save()
    data = (recipes/'lama.json').read_text().replace('"version": 1,','"version": 1, "version": 1,'); (recipes/'lama.json').write_text(data); run(False,*args); save()
    # Valid recipe's template hash must be trusted before parsing/assembly.
    (recipes/'lama/package.json').write_bytes(b'{}'); run(False,*args); save()
    script = root/"quoted script's.sh"; run(True,'--download-script',str(script)); run(False,'--download-script',str(script))
    subprocess.run(['/bin/sh','-n',str(script)],check=True)
    assert 'resolve/'+'a'*40+'/weights.onnx' in script.read_text()
    print('PASS: assembly, conflict, corrupt payload, symlink, traversal, alias, duplicate recipe/key, template tamper, shell syntax and script conflict')
with tempfile.TemporaryDirectory(prefix='native-store-test-') as tmp:
    root = pathlib.Path(tmp).resolve(); runtimes = root/'Runtimes'; runtimes.mkdir()
    pkg = runtimes/'toy-native-1'; (pkg/'bin').mkdir(parents=True); (pkg/'lib').mkdir()
    worker = b'not executable model code: fixture only'; dep = b'fixture dependency'
    (pkg/'bin/lightcraft-ai-native').write_bytes(worker); (pkg/'bin/lightcraft-ai-native').chmod(0o755)
    (pkg/'lib/toy.dylib').write_bytes(dep)
    runtime = dict(version=1,kind='runtime',id=pkg.name,name='Fixture',model_version='v1',source='fixture',adapter_version=1,architecture='aarch64-macos',execution='native-protocol-v1',files=[h('bin/lightcraft-ai-native',worker),h('lib/toy.dylib',dep)])
    (pkg/'package.json').write_bytes(encoded(runtime))
    trust = root/'runtime-trust.json'; trust.write_bytes(encoded(dict(version=1,runtimes=[dict(id=pkg.name,binary_sha256=hashlib.sha256(worker).hexdigest(),model_id='toy-model',model_manifest_sha256='a'*64,dependency_sha256={'lib/toy.dylib':hashlib.sha256(dep).hexdigest()})])))
    def store(ok, name):
        result = subprocess.run([str(binary),'--create-store',str(root/name),'--runtimes',str(runtimes),'--trust',str(trust)],capture_output=True,text=True)
        assert (result.returncode == 0) == ok, (result.returncode,result.stdout,result.stderr)
    store(True,'AI'); store(False,'AI')
    registry = json.loads((root/'AI/registry.json').read_text())
    assert registry == dict(version=1,models=[],runtimes=[dict(id=pkg.name,path='runtimes/'+pkg.name)],defaults=dict(selected_model_id=None))
    assert (root/'AI/runtimes'/pkg.name/'bin/lightcraft-ai-native').stat().st_mode & 0o777 == 0o755
    (pkg/'lib/toy.dylib').write_bytes(b'corrupt'); store(False,'corrupt'); assert not (root/'corrupt').exists()
    (pkg/'lib/toy.dylib').write_bytes(dep)
    (pkg/'unexpected').write_bytes(b'not listed'); store(False,'unlisted'); (pkg/'unexpected').unlink()
    (pkg/'lib/toy.dylib').unlink(); (pkg/'lib/toy.dylib').symlink_to(root/'AI/runtimes'/pkg.name/'lib/toy.dylib'); store(False,'symlink')
    (pkg/'lib/toy.dylib').unlink(); (pkg/'lib/toy.dylib').write_bytes(dep)
    runtime['architecture']='wrong'; (pkg/'package.json').write_bytes(encoded(runtime)); store(False,'arch')
    runtime['architecture']='aarch64-macos'; runtime['execution']='python-protocol-v1'; (pkg/'package.json').write_bytes(encoded(runtime)); store(False,'python')
    runtime['execution']='native-protocol-v1'; (pkg/'package.json').write_bytes(encoded(runtime))
    (pkg/'bin/lightcraft-ai-native').chmod(0o644); store(False,'mode')
    print('PASS: native store registry/modes, conflict, corrupt dependency, unlisted file, symlink, architecture, Python runtime and non-executable worker rejection')
