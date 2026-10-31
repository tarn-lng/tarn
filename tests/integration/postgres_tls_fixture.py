"""Local PostgreSQL SSLRequest adversarial fixture; no external credentials."""
import collections
import os
from pathlib import Path
import re
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import time

REQUEST = b'\x00\x00\x00\x08\x04\xd2\x16\x2f'
errors = []
connections = []


def exact(conn, n):
    out = b''
    while len(out) < n:
        chunk = conn.recv(n - len(out))
        if not chunk:
            raise EOFError(out)
        out += chunk
    return out


def start(context, behavior):
    listener = socket.create_server(('127.0.0.1', 0))
    listener.settimeout(0.2)
    connections.append(listener)

    def handle(raw):
        raw.settimeout(3)
        try:
            assert exact(raw, 8) == REQUEST, 'Startup leaked before TLS'
            if behavior == 'silent':
                time.sleep(0.4)
                assert raw.recv(1024) == b'', 'Startup sent after negotiation timeout'
            elif behavior != 'tls':
                raw.sendall(behavior)
                assert raw.recv(1024) == b'', 'Startup sent after TLS refusal'
            else:
                raw.sendall(b'S')
                with context.wrap_socket(raw, server_side=True) as encrypted:
                    # Failure connections abort verification; successful one echoes.
                    payload = encrypted.recv(1024)
                    if payload:
                        assert payload == b'encrypted startup'
                        encrypted.sendall(payload[:3])
                        encrypted.sendall(payload[3:])
        except (ssl.SSLError, ConnectionResetError, BrokenPipeError):
            pass
        except BaseException as exc:
            errors.append(repr(exc))
        finally:
            raw.close()

    def accept():
        while listener.fileno() >= 0:
            try:
                raw, _ = listener.accept()
            except socket.timeout:
                continue
            except OSError:
                break
            threading.Thread(target=handle, args=(raw,), daemon=True).start()

    threading.Thread(target=accept, daemon=True).start()
    return listener.getsockname()[1]


with tempfile.TemporaryDirectory(prefix='tarn-pg-tls-') as directory:
    root = Path(directory)
    cert, key = root / 'cert.pem', root / 'key.pem'
    subprocess.run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes',
                    '-keyout', str(key), '-out', str(cert), '-days', '1', '-subj', '/CN=localhost',
                    '-addext', 'subjectAltName=DNS:localhost'], check=True, capture_output=True)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(cert, key)
    tls, refused, invalid, silent = [start(context, behavior) for behavior in ('tls', b'N', b'X', 'silent')]
    began = time.monotonic()
    result = subprocess.run([sys.argv[1], f'localhost:{tls}', str(cert), f'127.0.0.1:{tls}',
                             f'localhost:{refused}', f'localhost:{invalid}', f'localhost:{silent}'],
                            capture_output=True, timeout=15, env=dict(os.environ, TARN_TRACE_NET='1'))
    assert time.monotonic() - began < 10
    time.sleep(0.5)
    for listener in connections:
        listener.close()
    assert result.returncode == 0, (result.returncode, result.stdout, result.stderr)
    assert result.stdout == b'PostgreSQL TLS fixture passed\n', result.stdout
    assert not errors, errors
    balance = collections.Counter()
    for event, fd in re.findall(rb'net:(open|close):(\d+)', result.stderr):
        balance[fd] += 1 if event == b'open' else -1
        assert balance[fd] in (0, 1), (fd, result.stderr)
    assert not +balance, (balance, result.stderr)
    print('PostgreSQL SSLRequest, TLS, hostname, CA, refusal, invalid preface, timeout: passed')
