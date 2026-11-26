#!/usr/bin/env python3
"""Exercise the actual container as an application builder and stdio LSP."""
import json
import subprocess
import sys

image, version = sys.argv[1:]
script = r'''
set -eu
test "$(tarn version | cut -d ' ' -f 1-2)" = "tarn $1"
command -v cc
command -v curl
test -s /etc/ssl/certs/ca-certificates.crt
ldconfig -p | grep libcurl.so.4
tarn init /tmp/container-smoke
cd /tmp/container-smoke
cat > main.tarn <<'TARN'
import "encoding"
import "crypto"
fn main() {
    text := "abc"
    digest := crypto.sha256(text.bytes())
    if encoding.hex(&digest[..]) != "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad" { panic("SHA-256 mismatch") }
    if encoding.base64_encode(text.bytes()) != "YWJj" { panic("Base64 mismatch") }
    print("container smoke passed")
}
TARN
tarn check
tarn build -o /tmp/container-smoke-program
/tmp/container-smoke-program
'''
subprocess.run(["docker", "run", "--rm", "--network", "none", image,
                "sh", "-ec", script, "smoke", version], check=True)
messages = [{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}},
            {"jsonrpc": "2.0", "id": 2, "method": "shutdown"},
            {"jsonrpc": "2.0", "method": "exit"}]
data = b""
for message in messages:
    body = json.dumps(message).encode()
    data += f"Content-Length: {len(body)}\r\n\r\n".encode() + body
result = subprocess.run(["docker", "run", "--rm", "-i", "--network", "none", image, "tarn-lsp"],
                        input=data, capture_output=True, check=True, timeout=30)
output, replies = result.stdout, []
while output:
    header, output = output.split(b"\r\n\r\n", 1)
    size = int(header.split(b":", 1)[1])
    replies.append(json.loads(output[:size]))
    output = output[size:]
assert any(r.get("id") == 1 and "capabilities" in r.get("result", {}) for r in replies), replies
assert any(r.get("id") == 2 and "result" in r for r in replies), replies
print("PASS: container native build, embedded stdlib and LSP")
