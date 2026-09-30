"""A small client for Weir's control socket, using only the standard library.

Copy this file next to your script, or put its directory on PYTHONPATH.

    import weir

    mixer = weir.connect()
    mixer.call("set_strip", id="Mic", mute="toggle")

    mixer.subscribe("state")
    for method, params in mixer.events():
        if method == "state_changed":
            print("Mic muted:", weir.find(params["strips"], "Mic")["mute"])

The protocol is documented in docs/API.md.
"""

import itertools
import json
import os
import socket

__all__ = ["Weir", "WeirError", "bus_label", "connect", "default_socket_path", "find"]


class WeirError(Exception):
    """The daemon refused a request.

    `code` says what kind of problem it was (see "Errors" in docs/API.md) and
    the message says what exactly, in words meant for people.
    """

    def __init__(self, code, message, data=None):
        super().__init__(message)
        self.code = code
        self.message = message
        self.data = data


def default_socket_path():
    """Where the daemon listens unless told otherwise."""
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if runtime:
        return os.path.join(runtime, "weir", "control.sock")
    return f"/tmp/weir-{os.getuid()}/control.sock"


def find(items, key):
    """The strip, bus or application in `items` with this id or name.

    Names are compared ignoring case, as the daemon does. Returns None when
    there is none.
    """
    for item in items:
        if item["id"] == key or (
            isinstance(key, str) and item["name"].lower() == key.lower()
        ):
            return item
    return None


def bus_label(buses, bus):
    """A bus's short label, as the mixer shows it: A1, A2... for buses that
    play to a device and B1, B2... for virtual ones, counted in the order
    `buses` lists them. Requests accept these in place of an id."""
    same_kind = [b["id"] for b in buses if b["kind"] == bus["kind"]]
    letter = "A" if bus["kind"] == "hardware" else "B"
    return f"{letter}{same_kind.index(bus['id']) + 1}"


class Weir:
    """One connection to the daemon.

    Requests are answered in order, and notifications that arrive while
    waiting for an answer are kept for `events()`, so nothing is lost.
    """

    def __init__(self, path=None):
        self._sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._sock.connect(path or default_socket_path())
        self._file = self._sock.makefile("rw", encoding="utf-8", newline="\n")
        self._ids = itertools.count(1)
        self._waiting = []

    def call(self, method, params=None, **fields):
        """Call `method` and return its result.

        Parameters can be given as a dict, as keywords, or both:

            mixer.call("set_route", strip="Music", bus="B1")
            mixer.call("set_strip", {"id": "Mic", "gate": {"enabled": True}})

        Raises WeirError when the daemon refuses the request.
        """
        request_id = next(self._ids)
        self._send(_request(method, _merge(params, fields), request_id))
        while True:
            message = self._read()
            if message.get("id") == request_id:
                return _result(message)
            if "method" in message:
                self._waiting.append(message)

    def send(self, method, params=None, **fields):
        """Send a request without waiting for, or getting, an answer.

        Good for a stream of changes, such as a knob being turned, where
        only the last one matters.
        """
        self._send(_request(method, _merge(params, fields)))

    def batch(self, *calls):
        """Send several requests in one message, run one after the other.

        Each call is a (method, params) pair. Returns one entry per call, in
        order: its result, or the WeirError it failed with. A failed request
        does not stop the ones after it.
        """
        first = next(self._ids)
        ids = [first] + [next(self._ids) for _ in calls[1:]]
        self._send([_request(m, p, i) for i, (m, p) in zip(ids, calls)])
        while True:
            message = self._read()
            if isinstance(message, list):
                by_id = {m["id"]: m for m in message}
                return [_result(by_id[i], raise_error=False) for i in ids]
            if "method" in message:
                self._waiting.append(message)

    def subscribe(self, *topics, meter_rate_hz=None):
        """Start getting notifications for these topics; all of them when
        none are given. Returns the topics now active."""
        params = {"topics": list(topics)}
        if meter_rate_hz is not None:
            params["meter_rate_hz"] = meter_rate_hz
        return self.call("subscribe", params)

    def events(self):
        """Yield each notification as a (method, params) pair, waiting for
        the next one when there are none. Stops when the daemon goes away."""
        while True:
            if self._waiting:
                message = self._waiting.pop(0)
            else:
                try:
                    message = self._read()
                except ConnectionError:
                    return
            if isinstance(message, dict) and "method" in message:
                yield message["method"], message.get("params")

    def close(self):
        """Close the connection. Subscriptions end with it."""
        self._file.close()
        self._sock.close()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()

    def _send(self, message):
        self._file.write(json.dumps(message) + "\n")
        self._file.flush()

    def _read(self):
        line = self._file.readline()
        if not line:
            raise ConnectionError("the Weir daemon closed the connection")
        return json.loads(line)


def connect(path=None):
    """Connect to the daemon, at `path` or where it listens by default."""
    return Weir(path)


def _merge(params, fields):
    merged = dict(params or {})
    merged.update(fields)
    return merged or None


def _request(method, params, request_id=None):
    message = {"jsonrpc": "2.0", "method": method}
    if request_id is not None:
        message["id"] = request_id
    if params is not None:
        message["params"] = params
    return message


def _result(message, raise_error=True):
    error = message.get("error")
    if error is None:
        return message.get("result")
    failure = WeirError(error["code"], error["message"], error.get("data"))
    if raise_error:
        raise failure
    return failure
