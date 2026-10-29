#!/usr/bin/env python3
"""Exercise the actual stdio server, unsaved modules, and navigation."""
import json
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]

def exchange(messages, env=None):
    data = b''
    for message in messages:
        body = json.dumps({'jsonrpc': '2.0', **message}).encode()
        data += f'Content-Length: {len(body)}\r\n\r\n'.encode() + body
    result = subprocess.run([str(ROOT / 'target/debug/tarn-lsp')], input=data, capture_output=True, timeout=30, env=env)
    assert result.returncode == 0, result.stderr.decode()
    output = result.stdout
    parsed = []
    while output:
        header, output = output.split(b'\r\n\r\n', 1)
        size = int(header.split(b':', 1)[1])
        parsed.append(json.loads(output[:size]))
        output = output[size:]
    return parsed

def opened(uri, text, version=1):
    return {'method': 'textDocument/didOpen', 'params': {'textDocument': {'uri': uri, 'languageId': 'tarn', 'version': version, 'text': text}}}

def changed(uri, text, version):
    return {'method': 'textDocument/didChange', 'params': {'textDocument': {'uri': uri, 'version': version}, 'contentChanges': [{'text': text}]}}

def request(method, ident, uri, line, character):
    return {'id': ident, 'method': method, 'params': {'textDocument': {'uri': uri}, 'position': {'line': line, 'character': character}}}

with tempfile.TemporaryDirectory(prefix='tarn lsp ñ ') as directory:
    root = Path(directory)
    entry = root / 'main.tarn'
    entry.write_text('fn main() {}\n')
    uri = entry.as_uri()
    bad = 'fn main() {\n    print("😀", missing)\n}\n'
    good = 'fn main() {\n    value := 42\n    print(value)\n}\n'
    messages = [
        {'id': 1, 'method': 'initialize', 'params': {}},
        opened(uri, bad), changed(uri, good, 2),
        request('textDocument/hover', 2, uri, 2, 11),
        request('textDocument/definition', 3, uri, 2, 11),
        {'method': 'textDocument/didClose', 'params': {'textDocument': {'uri': uri}}},
        {'id': 4, 'method': 'shutdown'}, {'method': 'exit'},
    ]
    out = exchange(messages)
    responses = {m['id']: m for m in out if 'id' in m}
    assert responses[1]['result']['capabilities']['textDocumentSync']['change'] == 1
    diagnostics = [m['params'] for m in out if m.get('method') == 'textDocument/publishDiagnostics']
    error = next(d for d in diagnostics[0]['diagnostics'] if d['code'] == 'E2001')
    assert error['range']['start'] == {'line': 1, 'character': 16}, error
    assert diagnostics[1]['diagnostics'] == [], diagnostics[1]
    assert diagnostics[1]['version'] == 2
    assert responses[2]['result']['contents']['value'] == 'value: i64', responses[2]
    assert responses[3]['result']['uri'] == uri, responses[3]
    assert responses[3]['result']['range']['start'] == {'line': 1, 'character': 4}
    assert diagnostics[-1]['diagnostics'] == []
    assert entry.read_text() == 'fn main() {}\n', 'editor buffers must not modify disk'

    child = root / 'util.tarn'
    child.write_text('pub fn get() i32 { return 1 }\n')
    main = 'import "util"\nfn main() {\n    value := util.get()\n    print(value)\n}\n'
    out = exchange([
        {'id': 1, 'method': 'initialize', 'params': {}}, opened(uri, main),
        opened(child.as_uri(), 'pub fn get() i32 { return unknown }\n'),
        changed(child.as_uri(), 'pub fn get() i32 { return 2 }\n', 2),
        request('textDocument/definition', 2, uri, 2, 19),
        {'id': 3, 'method': 'shutdown'}, {'method': 'exit'},
    ])
    child_ds = [m['params']['diagnostics'] for m in out if m.get('method') == 'textDocument/publishDiagnostics' and m['params']['uri'] == child.as_uri()]
    assert any(d['code'] == 'E2001' for d in child_ds[0]), child_ds
    assert child_ds[-1] == [], child_ds
    definition = next(m for m in out if m.get('id') == 2)['result']
    assert definition['uri'] == child.as_uri(), definition
    assert child.read_text() == 'pub fn get() i32 { return 1 }\n'
print('PASS: stdio lifecycle, unsaved diagnostics/clearing, UTF-16, hover, definition, imported buffers, unchanged disk')

# Async source facts, lowering-clean diagnostics and source-level ownership errors.
with tempfile.TemporaryDirectory(prefix='tarn async lsp ') as directory:
    entry = Path(directory) / 'main.tarn'
    entry.write_text('fn main() {}\n')
    uri = entry.as_uri()
    source = 'async fn get() i32 { return 42 }\nasync fn twice() i32 {\n    value := await get()\n    return value * 2\n}\nfn main() {\n    op := get()\n    op\n}\n'
    moved = 'fn take(s string) {}\nasync fn get() i32 { return 1 }\nasync fn f(s string) {\n    take(s)\n    n := await get()\n    print(&s)\n}\nfn main() {}\n'
    out = exchange([
        {'id': 1, 'method': 'initialize', 'params': {}}, opened(uri, source),
        request('textDocument/hover', 2, uri, 6, 5),
        request('textDocument/definition', 3, uri, 6, 11),
        request('textDocument/hover', 5, uri, 2, 5),
        changed(uri, 'fn main() { await 42 }\n', 2),
        changed(uri, moved, 3),
        {'id': 4, 'method': 'shutdown'}, {'method': 'exit'},
    ])
    diagnostics = [m['params']['diagnostics'] for m in out if m.get('method') == 'textDocument/publishDiagnostics']
    assert diagnostics[0] == [], diagnostics
    assert [d['code'] for d in diagnostics[1]] == ['E3060'], diagnostics
    # Ownership across a suspension is reported on source code, never generated frame state.
    assert [d['code'] for d in diagnostics[2]] == ['E4001'], diagnostics
    assert diagnostics[2][0]['range']['start'] == {'line': 5, 'character': 10}, diagnostics
    assert 'async' not in diagnostics[2][0]['message'] and '_' not in diagnostics[2][0]['message'].replace('`s`', ''), diagnostics
    responses = {m['id']: m for m in out if 'id' in m}
    assert responses[2]['result']['contents']['value'] == 'op: async computation<i32>', responses[2]
    assert responses[3]['result']['range']['start'] == {'line': 0, 'character': 9}, responses[3]
    assert responses[5]['result']['contents']['value'] == 'value: i32', responses[5]
    assert entry.read_text() == 'fn main() {}\n'
print('PASS: async source types, definition, context diagnostics and source-level ownership across await')

# Formatting uses the unsaved buffer and leaves disk and server buffers unchanged.
with tempfile.TemporaryDirectory(prefix='tarn format lsp ') as directory:
    entry = Path(directory) / 'main.tarn'
    entry.write_text('fn main() {}\n')
    uri = entry.as_uri()
    source = 'fn main(){print("😀")} // keep ñ'
    formatted = 'fn main() { print("😀") }  // keep ñ\n'
    formatting = lambda ident: {'id': ident, 'method': 'textDocument/formatting', 'params': {'textDocument': {'uri': uri}, 'options': {'tabSize': 8, 'insertSpaces': False}}}
    out = exchange([
        {'id': 1, 'method': 'initialize', 'params': {}}, opened(uri, source),
        formatting(2), formatting(3), changed(uri, formatted, 2), formatting(4),
        changed(uri, 'fn main() {', 3), formatting(5),
        {'method': 'textDocument/didClose', 'params': {'textDocument': {'uri': uri}}}, formatting(6),
        {'id': 7, 'method': 'shutdown'}, {'method': 'exit'},
    ])
    responses = {m['id']: m for m in out if 'id' in m}
    assert responses[1]['result']['capabilities']['documentFormattingProvider'] is True
    edit = responses[2]['result'][0]
    assert edit['newText'] == formatted, edit
    assert edit['range'] == {'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': len(source.encode('utf-16-le')) // 2}}, edit
    assert responses[3]['result'] == responses[2]['result'], 'format request must not change buffer state'
    assert responses[4]['result'] == [], responses[4]
    assert responses[5]['error']['code'] == -32803, responses[5]
    assert responses[6]['error']['code'] == -32803, responses[6]
    assert entry.read_text() == 'fn main() {}\n'
print('PASS: unsaved formatting, UTF-16 full-document edits, idempotence, invalid/closed buffer refusal, unchanged disk')

# Locked packages use the real frontend and verified dependency source snapshots.
with tempfile.TemporaryDirectory(prefix='tarn package lsp ') as directory:
    import os
    root = Path(directory)
    env = dict(os.environ, TARN_HOME=str(root / 'home'))
    cli = ROOT / 'target/debug/tarn'
    def package_command(cwd, *args):
        result = subprocess.run([str(cli), *args], cwd=cwd, env=env, capture_output=True, text=True)
        assert result.returncode == 0, (args, result.stderr)
    package_command(root, 'init', 'greeting', '--lib')
    (root / 'greeting/lib.tarn').write_text('pub fn answer() i32 { return 42 }\n')
    package_command(root / 'greeting', 'publish', '--registry', str(root / 'registry'))
    package_command(root, 'init', 'application')
    app = root / 'application'
    package_command(app, 'add', 'greeting', '--registry', str(root / 'registry'))
    source = 'import "greeting"\nfn main() {\n    print(greeting.answer())\n}\n'
    entry = app / 'main.tarn'
    entry.write_text(source)
    uri = entry.as_uri()
    lock = json.loads((app / 'tarn.lock').read_text())
    cached = root / 'home/sources' / lock['packages'][0]['release']['hash'] / 'lib.tarn'
    out = exchange([
        {'id': 1, 'method': 'initialize', 'params': {}}, opened(uri, source),
        request('textDocument/definition', 2, uri, 2, 20),
        opened(cached.as_uri(), 'pub fn answer() i32 { return 99 }\n'),
        changed(uri, source, 2),
        {'id': 3, 'method': 'shutdown'}, {'method': 'exit'},
    ], env)
    responses = {m['id']: m for m in out if 'id' in m}
    assert responses[2]['result']['uri'] == cached.as_uri(), responses[2]
    diagnostics = [m['params'] for m in out if m.get('method') == 'textDocument/publishDiagnostics' and m['params']['uri'] == uri]
    assert diagnostics[0]['diagnostics'] == [], diagnostics[0]
    assert any('cannot be overridden' in d['message'] for d in diagnostics[-1]['diagnostics']), diagnostics[-1]
    assert cached.read_text() == 'pub fn answer() i32 { return 42 }\n'
print('PASS: package definition navigation and locked dependency overlay refusal')

binary = str(ROOT / 'target/debug/tarn-lsp')
version = subprocess.run([binary, '--version'], capture_output=True, timeout=5, stdin=subprocess.DEVNULL)
assert version.returncode == 0 and version.stdout.decode().startswith('tarn-lsp '), version
unknown = subprocess.run([binary, '--bogus'], capture_output=True, timeout=5, stdin=subprocess.DEVNULL)
assert unknown.returncode == 2 and b'--bogus' in unknown.stderr, unknown
print('PASS: --version prints and exits; unknown arguments are rejected')

# A module opened on its own resolves imports from the ancestor its entry uses.
with tempfile.TemporaryDirectory() as tmp:
    project = Path(tmp)
    (project / 'tarn.toml').write_text('[package]\nname = "nested"\nversion = "0.1.0"\nentry = "main.tarn"\n')
    (project / 'src').mkdir()
    (project / 'src/b.tarn').write_text('pub fn answer() i32 { return 42 }\n')
    module = project / 'src/a.tarn'
    module.write_text('import "src/b"\npub fn twice() i32 { return b.answer() * 2 }\n')
    (project / 'main.tarn').write_text('import "src/a"\nfn main() { print(a.twice()) }\n')
    out = exchange([
        {'id': 1, 'method': 'initialize', 'params': {}},
        opened(module.as_uri(), module.read_text()),
        {'id': 2, 'method': 'shutdown', 'params': None},
    ])
    diagnostics = [m['params'] for m in out if m.get('method') == 'textDocument/publishDiagnostics' and m['params']['uri'] == module.as_uri()]
    assert diagnostics and diagnostics[-1]['diagnostics'] == [], diagnostics
print('PASS: nested modules resolve imports from the project root')
