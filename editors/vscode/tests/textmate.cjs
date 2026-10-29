// Exercise the shipped grammar with VS Code's own TextMate/Oniguruma engines.
// Run with ELECTRON_RUN_AS_NODE=1 /path/to/code this-file (no GUI is opened).
const fs = require('fs');
const path = require('path');
const assert = require('assert/strict');
// Distribution builds (e.g. Code - OSS on a system Electron) keep the modules
// elsewhere: TARN_VSCODE_MODULES=/usr/lib/code/node_modules.asar.
const dependencies = process.env.TARN_VSCODE_MODULES || path.join(path.dirname(process.execPath), 'resources/app/node_modules.asar');
const textmate = require(path.join(dependencies, 'vscode-textmate'));
const oniguruma = require(path.join(dependencies, 'vscode-oniguruma'));
(async () => {
    const wasm = fs.readFileSync(path.join(dependencies, 'vscode-oniguruma/release/onig.wasm'));
    await oniguruma.loadWASM(wasm.buffer.slice(wasm.byteOffset, wasm.byteOffset + wasm.byteLength));
    const onigLib = Promise.resolve({ createOnigScanner: patterns => new oniguruma.OnigScanner(patterns), createOnigString: text => new oniguruma.OnigString(text) });
    const registry = new textmate.Registry({
        onigLib,
        loadGrammar: async scope => scope === 'source.tarn' ? textmate.parseRawGrammar(fs.readFileSync(path.join(__dirname, '../syntaxes/tarn.tmLanguage.json'), 'utf8'), 'tarn.tmLanguage.json') : null,
    });
    const grammar = await registry.loadGrammar('source.tarn');
    const tokens = source => grammar.tokenizeLine(source, textmate.INITIAL).tokens.map(t => ({ text: source.slice(t.startIndex, t.endIndex), scopes: t.scopes }));
    const has = (source, text, scope) => {
        const result = tokens(source);
        assert.ok(result.some(t => t.text === text && t.scopes.includes(scope)), JSON.stringify(result));
    };
    has('n := 42', '42', 'constant.numeric.integer.tarn');
    has('n := 0xff', '0xff', 'constant.numeric.integer.tarn');
    has('n := 1.25e-3', '1.25e-3', 'constant.numeric.float.tarn');
    has('fn Pair<A, B>.first(&self)', 'first', 'entity.name.function.tarn');
    has('unsafe extern "C" fn write(fd i32)', 'write', 'entity.name.function.tarn');
    has('once fn(i32) i32', 'once', 'keyword.other.contextual.tarn');
    has('/// document fn hello', '/// document fn hello', 'comment.line.documentation.tarn');
    const stringTokens = tokens('text := "fn hello // string"');
    assert.ok(stringTokens.some(t => t.text.includes('fn hello // string') && t.scopes.includes('string.quoted.double.tarn')));
    assert.ok(!stringTokens.some(t => t.scopes.includes('comment.line.double-slash.tarn')));
    has('msg := f"n={user.name} {{x}}"', 'user.name', 'variable.other.tarn');
    has('msg := f"n={user.name} {{x}}"', '{{', 'constant.character.escape.tarn');
    has('msg := f"{a + b}"', '{a + b}', 'invalid.illegal.interpolation.tarn');
    for (const name of ['tarn-diag', 'tarn-res', 'tarn-ast']) {
        const raw = textmate.parseRawGrammar(fs.readFileSync(path.join(__dirname, `../syntaxes/${name}.tmLanguage.json`), 'utf8'), `${name}.json`);
        const other = new textmate.Registry({ onigLib, loadGrammar: async () => raw });
        // Compile all regexes while tokenizing representative snapshot text.
        const g = await other.loadGrammar(raw.scopeName);
        g.tokenizeLine('== main == error[E2001]: missing (fn add)', textmate.INITIAL);
        other.dispose();
    }
    registry.dispose();
    console.log('PASS: actual TextMate tokenization, numbers, methods, FFI, contextual keywords, comments, strings and snapshots');
})().catch(error => { console.error(error); process.exitCode = 1; });
