//! Stdio LSP: full-buffer synchronization, compiler diagnostics/navigation and shared formatting.
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{self, BufRead, Write},
    path::PathBuf,
};
use tarn_diagnostics::{Severity, Span};
use tarn_resolve::{Res, SymbolId};
use url::Url;

fn path(uri: &str) -> Option<PathBuf> {
    Url::parse(uri).ok()?.to_file_path().ok()
}
fn uri(path: &std::path::Path) -> Option<String> {
    Url::from_file_path(path).ok().map(|u| u.to_string())
}
fn position(text: &str, byte: u32) -> Value {
    let prefix = &text[..byte as usize];
    let line = prefix.bytes().filter(|&b| b == b'\n').count();
    let last = prefix.rsplit('\n').next().unwrap_or("");
    json!({"line":line,"character":last.encode_utf16().count()})
}
fn offset(text: &str, pos: &Value) -> Option<u32> {
    let line = pos["line"].as_u64()? as usize;
    let column = pos["character"].as_u64()? as usize;
    let mut base = 0;
    let slice = text
        .split_inclusive('\n')
        .nth(line)
        .or_else(|| (line == text.bytes().filter(|&b| b == b'\n').count()).then_some(""))?;
    for part in text.split_inclusive('\n').take(line) {
        base += part.len();
    }
    let mut units = 0;
    for (i, ch) in slice.char_indices() {
        if units == column {
            return Some((base + i) as u32);
        }
        units += ch.len_utf16();
        if units > column {
            return None;
        }
    }
    (units == column).then_some((base + slice.len()) as u32)
}
fn range(result: &tarn_driver::CheckResult, span: Span) -> Value {
    let text = &result.program.sources.file(span.file).text;
    json!({"start":position(text,span.start),"end":position(text,span.end)})
}
fn send(out: &mut impl Write, value: Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(&value)?;
    write!(out, "Content-Length: {}\r\n\r\n", bytes.len())?;
    out.write_all(&bytes)?;
    out.flush()
}
fn read(input: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        if line.trim().is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("Content-Length") {
                length = value.trim().parse::<usize>().ok();
            }
        }
    }
    let size = length
        .filter(|n| *n <= 16 * 1024 * 1024)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length"))?;
    let mut body = vec![0; size];
    input.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body)?))
}
#[derive(Default)]
struct Server {
    buffers: HashMap<PathBuf, String>,
    versions: HashMap<PathBuf, i64>,
    shutdown: bool,
}
impl Server {
    fn publish(&self, out: &mut impl Write) -> io::Result<()> {
        // Recheck every open entry so changing an imported buffer refreshes dependents.
        let mut files: Vec<_> = self.buffers.keys().collect();
        files.sort();
        for file in files {
            let Some(u) = uri(file) else { continue };
            let ds = match check(file, &self.buffers) {
                Ok(result) => result.diagnostics.iter().filter_map(|d| {
                    let span = d.primary_span()?;
                    if std::path::Path::new(&result.program.sources.file(span.file).name) != file { return None; }
                    let mut message = d.message.clone();
                    for note in &d.notes { message.push_str(&format!("\nnote: {note}")); }
                    if let Some(help) = &d.help { message.push_str(&format!("\nhelp: {help}")); }
                    let related: Vec<_> = d.labels.iter().filter(|l| !l.primary).filter_map(|l| {
                        let name = &result.program.sources.file(l.span.file).name;
                        let u = uri(std::path::Path::new(name))?;
                        Some(json!({"location":{"uri":u,"range":range(&result,l.span)},"message":l.message}))
                    }).collect();
                    Some(json!({"range":range(&result,span),"severity":match d.severity { Severity::Error=>1,Severity::Warning=>2,Severity::Note=>3 },"code":d.code,"source":"tarn","message":message,"relatedInformation":related}))
                }).collect::<Vec<_>>(),
                Err(message) => vec![json!({"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"severity":1,"source":"tarn","message":message})],
            };
            send(
                out,
                json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":u,"version":self.versions.get(file),"diagnostics":ds}}),
            )?;
        }
        Ok(())
    }
    fn format_document(&self, params: &Value) -> Result<Value, (i32, &'static str)> {
        let file = params["textDocument"]["uri"]
            .as_str()
            .and_then(path)
            .ok_or((-32602, "formatting requires a file URI"))?;
        let text = self
            .buffers
            .get(&file)
            .ok_or((-32803, "formatting requires an open document"))?;
        let formatted = tarn_fmt::format(text).map_err(|error| {
            (
                -32803,
                match error {
                    tarn_fmt::FormatError::Syntax(_) => {
                        "cannot format a document with syntax errors"
                    }
                    tarn_fmt::FormatError::Invariant => {
                        "formatter could not preserve document syntax"
                    }
                },
            )
        })?;
        if formatted == *text {
            return Ok(json!([]));
        }
        Ok(
            json!([{"range":{"start":{"line":0,"character":0},"end":position(text,text.len() as u32)},"newText":formatted}]),
        )
    }
    fn symbol(&self, params: &Value, definition: bool) -> Option<Value> {
        let file = path(params["textDocument"]["uri"].as_str()?)?;
        let result = check(&file, &self.buffers).ok()?;
        let source = result
            .program
            .sources
            .file(result.program.modules[0].1.span.file);
        let byte = offset(&source.text, &params["position"])?;
        let resolved = result.resolved.as_ref()?;
        let contains = |s: Span| {
            s.file == result.program.modules[0].1.span.file && s.start <= byte && byte < s.end
        };
        let used = resolved.tables[0]
            .uses
            .values()
            .filter(|u| contains(u.span))
            .min_by_key(|u| u.span.len());
        let id = match used.map(|u| &u.res) {
            Some(Res::Symbol(id)) => *id,
            _ => resolved
                .symbols
                .iter()
                .enumerate()
                .find(|(_, s)| s.span.is_some_and(contains))
                .map(|(i, _)| SymbolId(i as u32))?,
        };
        let symbol = resolved.symbol(id);
        if definition {
            let span = symbol.span?;
            let name = &result.program.sources.file(span.file).name;
            // Embedded core is only navigable when its source exists in this checkout.
            let dest = if std::path::Path::new(name).is_absolute() {
                PathBuf::from(name)
            } else {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../..")
                    .join(name)
            };
            if !dest.is_file() {
                return None;
            }
            return Some(json!({"uri":uri(&dest)?,"range":range(&result,span)}));
        }
        let typ = result
            .typed
            .as_ref()
            .and_then(|t| t.locals.get(&id).map(|ty| t.display(ty, resolved)));
        let label = typ
            .map(|t| format!("{}: {t}", symbol.name))
            .unwrap_or_else(|| format!("{} ({:?})", symbol.name, symbol.kind));
        Some(json!({"contents":{"kind":"plaintext","value":label}}))
    }
    fn handle(&mut self, msg: Value, out: &mut impl Write) -> io::Result<bool> {
        let method = msg["method"].as_str().unwrap_or("");
        let p = &msg["params"];
        if method == "exit" {
            return Ok(false);
        }
        if let Some(id) = msg.get("id") {
            if self.shutdown && method != "shutdown" {
                send(
                    out,
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32600,"message":"server is shutting down"}}),
                )?;
                return Ok(true);
            }
            let result = match method {
                "initialize" => {
                    json!({"capabilities":{"positionEncoding":"utf-16","textDocumentSync":{"openClose":true,"change":1,"save":{"includeText":false}},"hoverProvider":true,"definitionProvider":true,"documentFormattingProvider":true},"serverInfo":{"name":"tarn-lsp","version":env!("CARGO_PKG_VERSION")}})
                }
                "shutdown" => {
                    self.shutdown = true;
                    Value::Null
                }
                "textDocument/formatting" => match self.format_document(p) {
                    Ok(edits) => edits,
                    Err((code, message)) => {
                        send(
                            out,
                            json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}),
                        )?;
                        return Ok(true);
                    }
                },
                "textDocument/hover" => self.symbol(p, false).unwrap_or(Value::Null),
                "textDocument/definition" => self.symbol(p, true).unwrap_or(Value::Null),
                _ => {
                    send(
                        out,
                        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"method not supported"}}),
                    )?;
                    return Ok(true);
                }
            };
            send(out, json!({"jsonrpc":"2.0","id":id,"result":result}))?;
        } else if !self.shutdown {
            match method {
                "textDocument/didOpen" | "textDocument/didChange" => {
                    if let Some(file) = p["textDocument"]["uri"].as_str().and_then(path) {
                        let text = if method.ends_with("didOpen") {
                            p["textDocument"]["text"].as_str()
                        } else {
                            p["contentChanges"]
                                .as_array()
                                .and_then(|a| a.last())
                                .and_then(|v| v["text"].as_str())
                        };
                        if let Some(text) = text {
                            self.buffers.insert(file.clone(), text.into());
                            if let Some(v) = p["textDocument"]["version"].as_i64() {
                                self.versions.insert(file, v);
                            }
                            self.publish(out)?;
                        }
                    }
                }
                "textDocument/didSave" | "workspace/didChangeWatchedFiles" => self.publish(out)?,
                "textDocument/didClose" => {
                    if let Some(file) = p["textDocument"]["uri"].as_str().and_then(path) {
                        self.buffers.remove(&file);
                        self.versions.remove(&file);
                        send(
                            out,
                            json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":p["textDocument"]["uri"],"diagnostics":[]}}),
                        )?;
                        self.publish(out)?;
                    }
                }
                _ => {}
            }
        }
        Ok(true)
    }
}
/// Formatting and navigation walk syntax trees recursively, so the server
/// runs on the compiler's large stack (ADR 0081).
fn main() -> io::Result<()> {
    // Language clients may pass `--stdio` and `--clientProcessId=N`; stdio is the only transport.
    match std::env::args().nth(1).as_deref() {
        None | Some("--stdio") => tarn_driver::with_compiler_stack(serve),
        Some(arg) if arg.starts_with("--clientProcessId") => {
            tarn_driver::with_compiler_stack(serve)
        }
        Some("--version" | "-V") => {
            println!("tarn-lsp {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help" | "-h") => {
            println!(
                "tarn-lsp {}\nLanguage server for Tarn over stdio.\n\nUsage: tarn-lsp [--stdio | --version | --help]",
                env!("CARGO_PKG_VERSION")
            );
            Ok(())
        }
        Some(other) => {
            eprintln!("tarn-lsp: unknown argument `{other}`; try --help");
            std::process::exit(2)
        }
    }
}

fn serve() -> io::Result<()> {
    let mut server = Server::default();
    let mut input = io::BufReader::new(io::stdin().lock());
    let mut out = io::stdout().lock();
    while let Some(msg) = read(&mut input)? {
        if !server.handle(msg, &mut out)? {
            break;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf16_positions() {
        let text = "a😀x\r\nñz\n";
        assert_eq!(position(text, 5), json!({"line":0,"character":3}));
        assert_eq!(offset(text, &json!({"line":0,"character":3})), Some(5));
        assert_eq!(offset(text, &json!({"line":0,"character":2})), None);
        assert_eq!(
            offset(text, &json!({"line":2,"character":0})),
            Some(text.len() as u32)
        );
    }
}

/// Editor checking with the project's verified package dependencies (ADR
/// 0051). Package loading is the tool's job; the compiler receives plain data.
fn check(
    file: &std::path::Path,
    buffers: &std::collections::HashMap<std::path::PathBuf, String>,
) -> Result<tarn_driver::CheckResult, String> {
    let packages = tarn_packages::graph::package_set(file)?;
    tarn_driver::check_editor_with_packages(file, buffers, packages.as_ref())
}
