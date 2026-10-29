# Tarn for VS Code

Official Linux language support: `.tarn` registration and icon, TextMate
highlighting, comments, doc-comment continuation, brackets, indentation and
snippets. Compiler diagnostics, hover, definition and Format Document come from
the real Tarn language server. No editor-local parser or type checker is used.

## Local installation

From the compiler repository:

```sh
cargo build -p tarn-lsp
cd editors/vscode
npm ci
ln -s "$PWD" ~/.vscode/extensions/tarn-lang.tarn-0.0.1
```

Create the symlink only if it does not already exist. Run **Developer: Reload
Window** after installing or changing extension JavaScript/grammar/snippets.
After rebuilding only the server binary, use **Tarn: Restart Language Server**.
A packaged extension needs `tarn-lsp` on PATH or an explicit server path; binaries
are not bundled in the extension. Marketplace publication is outside Phase 21.

## Server selection and lifecycle

The extension searches workspace `target/debug/tarn-lsp`, then release, then
this extension's compiler checkout targets, and finally PATH. Configure
`tarn.serverPath` for an explicit executable; relative paths resolve against the
first workspace folder. The language client uses that workspace as its working
directory, including external Tarn projects.

`tarn.lsp.enabled` can be changed immediately without reloading. Disabling stops
the server while leaving highlighting, snippets and icons active; enabling starts
it again. Changing `tarn.serverPath` or invoking **Tarn: Restart Language Server**
stops and starts the client serially. Startup failures release watchers and can
be retried using that command.

## Formatting

**Format Document** uses the shared `tarn_fmt` implementation on unsaved buffers.
It preserves comment/literal text and meaningful line breaks, returns UTF-16
edits and refuses invalid syntax. It does not write disk directly.

```json
"[tarn]": {
  "editor.defaultFormatter": "tarn-lang.tarn",
  "editor.formatOnSave": true
}
```

Canonical indentation is four spaces regardless of editor tab settings. CLI
behavior and boundaries: [formatting guide](../../docs/formatting.md).

## Language and snapshots

Highlighting covers current function/method declarations (including generic
owners), async/await, C extern calls/raw pointers, numeric categories, references,
operators, strings and comments. Contextual `any`, `scope`, `once` and `borrows`
are highlighted in their syntax contexts. Snippets use normal Tarn syntax.

Compiler snapshots have dedicated highlighting/icons: `.diag`, `.ast`, `.res`.
Resolution snapshots are recognized by their `== ` header; checkout settings
associate `tests/**/*.res` without taking over ReScript files globally. Snapshot
modes are text viewers; compiler navigation/formatting applies to `.tarn` only.
Icons come from `icons/make_icons.py`; edit that generator instead of the assets.

## Current limits

Only file-backed documents are supported. Each open file is checked as an entry;
its directory is the driver's module root. Unsaved imported buffers are included,
and official stdlib editor sources retain their verified module identity without
granting native trust to ordinary user files with the same names. Checks are
synchronous and recheck all open entries. One client uses the first workspace
folder for configuration/working directory. Completion, rename, range/on-type
formatting, workspace indexing and incremental analysis remain future work.

## Validation

```sh
cargo build -p tarn -p tarn-lsp
python3 tools/lsp/tests/smoke.py
cd editors/vscode
npm test
ELECTRON_RUN_AS_NODE=1 /usr/share/code/code tests/textmate.cjs
```

The TextMate test uses the engines bundled with the local VS Code installation;
its Electron process runs as Node without opening a window. Adjust the Code path
for your installation. Distribution builds that run Code - OSS on a system
Electron keep the engines elsewhere; point the test at them:

```sh
ELECTRON_RUN_AS_NODE=1 TARN_VSCODE_MODULES=/usr/lib/code/node_modules.asar electron42 tests/textmate.cjs
``` `tests/integration.cjs` is an extension-host test runner
for `--extensionTestsPath`, covering real activation, formatting, unsaved
compiler diagnostics and lifecycle. Run it in a temporary workspace containing
`main.tarn` with `fn main(){print("😀")}`, and use isolated `--user-data-dir` and
`--extensions-dir`. It writes `test-result.json` in that temporary workspace and
never saves edits to the Tarn file.
