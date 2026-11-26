#!/usr/bin/env python3
"""Send IPC requests to a RustMeeter host and print every reply.

The RustMeeter GUI and `rsmtr serve` expose a JSON-over-TCP API on 127.0.0.1
(see DOCS.md §5). Each request is one JSON object on its own line; the reply is
the line the server writes back.

Usage:
    python ipc_test.py [--host 127.0.0.1] [--port 7590] [--timeout 30] [method ...]

Examples:
    python ipc_test.py
    python ipc_test.py status enablemixer list_devices
    python ipc_test.py 'toolcall={"text": "turn on the light"}'
    python ipc_test.py 'set_gain={"bus": 1, "db": -6}' 'set_mute={"strip": 1, "muted": true}'

A method argument may carry params as JSON after '='. A `ping` and a
`list_devices` are always sent first as a sanity check; pass `--skip-sanity`
to send only what you ask for.
"""

import argparse
import json
import socket
import sys

DEFAULT_PORT = 7590


def read_reply(sock: socket.socket, timeout: float) -> str:
    """Read one newline-terminated reply from the server."""
    buf = b""
    while b"\n" not in buf:
        chunk = sock.recv(65536)
        if not chunk:
            raise EOFError("connection closed by server")
        buf += chunk
    return buf.split(b"\n", 1)[0].decode(errors="replace")


def call(sock: socket.socket, req_id: int, method: str, params: dict, timeout: float) -> str:
    line = (json.dumps({"id": req_id, "method": method, "params": params}) + "\n").encode()
    sock.sendall(line)
    return read_reply(sock, timeout)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=DEFAULT_PORT)
    ap.add_argument("--timeout", type=float, default=30.0, help="seconds to wait per reply")
    ap.add_argument("--skip-sanity", action="store_true", help="don't send the ping/list_devices sanity checks")
    ap.add_argument("methods", nargs="*", metavar="method[=json-params]",
                    help="method names to call; '=' suffix adds a JSON params object")
    args = ap.parse_args()

    requests = []
    if not args.skip_sanity:
        requests += [("ping", {}), ("list_devices", {})]
    for method in args.methods:
        name, _, params_json = method.partition("=")
        if not name:
            ap.error(f"bad method {method!r}")
        params = json.loads(params_json) if params_json else {}
        if not isinstance(params, dict):
            ap.error(f"params for {name!r} must be a JSON object")
        requests.append((name, params))

    exit_status = 0
    try:
        with socket.create_connection((args.host, args.port), timeout=args.timeout) as sock:
            sock.settimeout(args.timeout)
            for req_id, (method, params) in enumerate(requests, start=1):
                reply = call(sock, req_id, method, params, args.timeout)
                try:
                    parsed = json.loads(reply)
                except json.JSONDecodeError:
                    parsed = reply
                print(json.dumps(parsed, indent=2) if isinstance(parsed, dict) else parsed)
                if isinstance(parsed, dict) and not parsed.get("ok", True):
                    # Keep testing other methods, but remember the exit status.
                    exit_status = 1
    except OSError as e:
        print(f"error: cannot reach {args.host}:{args.port}: {e}", file=sys.stderr)
        return 2
    except EOFError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    except TimeoutError:
        print(f"error: timed out after {args.timeout}s waiting for a reply", file=sys.stderr)
        return 2

    return exit_status


if __name__ == "__main__":
    sys.exit(main())
