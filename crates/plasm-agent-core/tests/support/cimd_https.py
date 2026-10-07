"""Real TLS fixture for CIMD retrieval tests; requires Python stdlib and openssl."""
import json
import ssl
import subprocess
import sys
import tempfile
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


class Handler(BaseHTTPRequestHandler):
    counts = {}

    def log_message(self, *_):
        pass

    def do_GET(self):
        self.counts[self.path] = self.counts.get(self.path, 0) + 1
        if self.path == "/counts":
            data = json.dumps(self.counts).encode()
        else:
            identity = f"https://client.invalid:{self.server.server_port}{self.path}"
            data = json.dumps({"client_id": identity, "client_name": "TLS fixture", "redirect_uris": ["https://example.com/callback"]}).encode()
        if self.path == "/slow":
            time.sleep(0.3)
        if self.path == "/mismatch":
            data = data.replace(b"client.invalid", b"other.invalid")
        if self.path in ("/large", "/stream-large"):
            data = b" " * 65537
        self.send_response(302 if self.path == "/redirect" else 404 if self.path == "/missing" else 200)
        self.send_header("Content-Type", "text/html" if self.path == "/html" else "application/json")
        if self.path == "/redirect":
            self.send_header("Location", "/valid")
        if self.path != "/stream-large":
            self.send_header("Content-Length", str(len(data)))
        if self.path == "/no-store":
            self.send_header("Cache-Control", "no-store")
        if self.path == "/max-age-zero":
            self.send_header("Cache-Control", "max-age=0")
        self.end_headers()
        try:
            self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError, ssl.SSLError):
            pass


with tempfile.TemporaryDirectory(prefix="plasm-cimd-", dir=sys.argv[1]) as directory:
    cert, key = Path(directory) / "cert.pem", Path(directory) / "key.pem"
    subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=client.invalid", "-addext", "subjectAltName=DNS:client.invalid", "-addext", "basicConstraints=critical,CA:FALSE", "-keyout", str(key), "-out", str(cert)], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(cert, key)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    print(json.dumps({"port": server.server_port, "certificate": cert.read_text()}), flush=True)
    server.serve_forever()
