#!/usr/bin/env python3
"""Queue one identified Codex continuation without blind delivery retries."""
from __future__ import annotations

import argparse
import base64
import contextlib
import fcntl
import hashlib
import json
import os
import re
import select
import socket
import stat
import struct
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Callable, Dict, Iterator, List, Mapping, Optional


MAX_FRAME_BYTES = 8 * 1024 * 1024
MAX_MESSAGE_BYTES = 256 * 1024
MAX_HANDSHAKE_BYTES = 16 * 1024
MAX_PAGES = 100
MAX_HISTORY_PAGES = 1000
QUEUE_PAGE_SIZE = 100
HISTORY_PAGE_SIZE = 100
MARKER_RE = re.compile(
    r"\[\[hex-continuation:v1 action=([^ ]+) owner=([^ ]+) sha256=([0-9a-f]{64})\]\]"
)
ID_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,127}\Z")
WEBSOCKET_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


class ContinuationError(RuntimeError):
    """A visible validation, state, or server failure."""


class UncertainDelivery(ContinuationError):
    """Delivery cannot be proven, so the action must not be submitted again."""


class IntentConflict(ContinuationError):
    """An action ID is already bound to different delivery intent."""


class TransportError(ContinuationError):
    """A bounded local WebSocket transport failure."""


def _validate_id(label: str, value: str) -> None:
    if not ID_RE.fullmatch(value):
        raise ContinuationError(
            f"{label} must be 1-128 ASCII letters, digits, dots, colons, underscores, or hyphens"
        )


def marker_for(action_id: str, owner: str, payload_sha256: str) -> str:
    """Return the stable marker persisted in queue and turn history."""
    return (
        f"[[hex-continuation:v1 action={action_id} owner={owner} "
        f"sha256={payload_sha256}]]"
    )


def _render_message(action_id: str, owner: str, message: str) -> tuple[str, str]:
    encoded = message.encode("utf-8")
    if not encoded or not message.strip():
        raise ContinuationError("message must not be empty")
    if len(encoded) > MAX_MESSAGE_BYTES:
        raise ContinuationError(f"message exceeds {MAX_MESSAGE_BYTES} bytes")
    payload_sha256 = hashlib.sha256(encoded).hexdigest()
    return f"{marker_for(action_id, owner, payload_sha256)}\n{message}", payload_sha256


class WebSocketRPC:
    """Small, bounded JSON-RPC client for a local Unix WebSocket endpoint."""

    def __init__(
        self,
        endpoint: Path,
        timeout: float = 10.0,
        max_frame_bytes: int = MAX_FRAME_BYTES,
    ):
        if timeout <= 0 or timeout > 120:
            raise TransportError("timeout must be greater than zero and at most 120 seconds")
        endpoint = Path(endpoint)
        if not endpoint.is_absolute():
            raise TransportError("Codex socket path must be absolute")
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(timeout)
        self.timeout = timeout
        self.max_frame_bytes = max_frame_bytes
        self.buffer = b""
        self.sequence = 0
        deadline = time.monotonic() + timeout
        try:
            self.sock.connect(str(endpoint))
            self._handshake(deadline)
            self.call(
                "initialize",
                {
                    "clientInfo": {
                        "name": "hex_codex_continuation",
                        "title": "Hex Codex continuation",
                        "version": "1.0",
                    },
                    "capabilities": {"experimentalApi": True},
                },
                deadline=deadline,
            )
            self.notify("initialized", {}, deadline=deadline)
        except Exception:
            self.sock.close()
            raise

    @classmethod
    def from_connected_socket(
        cls,
        connected: socket.socket,
        *,
        timeout: float,
        max_frame_bytes: int = MAX_FRAME_BYTES,
    ) -> "WebSocketRPC":
        """Build a protocol client around a connected socket for local tests."""
        value = cls.__new__(cls)
        value.sock = connected
        value.sock.settimeout(timeout)
        value.timeout = timeout
        value.max_frame_bytes = max_frame_bytes
        value.buffer = b""
        value.sequence = 0
        return value

    def __enter__(self) -> "WebSocketRPC":
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()

    def _handshake(self, deadline: float) -> None:
        key = base64.b64encode(os.urandom(16)).decode("ascii")
        request = (
            "GET / HTTP/1.1\r\n"
            "Host: localhost\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n\r\n"
        ).encode("ascii")
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TransportError("WebSocket handshake timed out")
        self.sock.settimeout(min(self.timeout, remaining))
        try:
            self.sock.sendall(request)
        except socket.timeout as exc:
            raise TransportError("WebSocket handshake timed out") from exc
        except OSError as exc:
            raise TransportError(f"WebSocket handshake send failed: {exc}") from exc
        while b"\r\n\r\n" not in self.buffer:
            if len(self.buffer) >= MAX_HANDSHAKE_BYTES:
                raise TransportError("WebSocket handshake is too large")
            self.buffer += self._recv(
                min(4096, MAX_HANDSHAKE_BYTES - len(self.buffer)), deadline
            )
        head, self.buffer = self.buffer.split(b"\r\n\r\n", 1)
        try:
            lines = head.decode("ascii", errors="strict").split("\r\n")
        except UnicodeError as exc:
            raise TransportError("WebSocket handshake is not valid ASCII") from exc
        status = lines[0].split() if lines else []
        if len(status) < 2 or status[0] != "HTTP/1.1" or status[1] != "101":
            raise TransportError("WebSocket handshake did not return HTTP 101")
        headers = {}
        for line in lines[1:]:
            if ":" not in line:
                raise TransportError("WebSocket handshake contains a malformed header")
            name, value = line.split(":", 1)
            headers[name.strip().lower()] = value.strip()
        expected = base64.b64encode(
            hashlib.sha1((key + WEBSOCKET_GUID).encode("ascii")).digest()
        ).decode("ascii")
        if headers.get("upgrade", "").lower() != "websocket":
            raise TransportError("WebSocket handshake upgrade header is invalid")
        connection_tokens = {
            token.strip() for token in headers.get("connection", "").lower().split(",")
        }
        if "upgrade" not in connection_tokens:
            raise TransportError("WebSocket handshake connection header is invalid")
        if headers.get("sec-websocket-accept") != expected:
            raise TransportError("WebSocket handshake accept value is invalid")

    def _recv(self, size: int, deadline: float) -> bytes:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TransportError("WebSocket receive timed out")
        self.sock.settimeout(min(self.timeout, remaining))
        try:
            data = self.sock.recv(size)
        except socket.timeout as exc:
            raise TransportError("WebSocket receive timed out") from exc
        except OSError as exc:
            raise TransportError(f"WebSocket receive failed: {exc}") from exc
        if not data:
            raise TransportError("WebSocket connection closed")
        return data

    def _exact(self, size: int, deadline: float) -> bytes:
        while len(self.buffer) < size:
            self.buffer += self._recv(max(4096, size - len(self.buffer)), deadline)
        value, self.buffer = self.buffer[:size], self.buffer[size:]
        return value

    def _send_frame(
        self, payload: bytes, opcode: int = 1, deadline: Optional[float] = None
    ) -> None:
        if len(payload) > self.max_frame_bytes:
            raise TransportError("outbound WebSocket frame is too large")
        mask = os.urandom(4)
        length = len(payload)
        if length < 126:
            header = bytes([0x80 | opcode, 0x80 | length])
        elif length < 65536:
            header = bytes([0x80 | opcode, 0xFE]) + struct.pack("!H", length)
        else:
            header = bytes([0x80 | opcode, 0xFF]) + struct.pack("!Q", length)
        masked = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
        try:
            send_timeout = self.timeout
            if deadline is not None:
                send_timeout = min(send_timeout, max(0.001, deadline - time.monotonic()))
            self.sock.settimeout(send_timeout)
            self.sock.sendall(header + mask + masked)
        except socket.timeout as exc:
            raise TransportError("WebSocket send timed out") from exc
        except OSError as exc:
            raise TransportError(f"WebSocket send failed: {exc}") from exc

    def receive(self, deadline: Optional[float] = None) -> Mapping[str, Any]:
        deadline = deadline if deadline is not None else time.monotonic() + self.timeout
        chunks: List[bytes] = []
        total = 0
        started = False
        while True:
            first, second = self._exact(2, deadline)
            final = bool(first & 0x80)
            if first & 0x70:
                raise TransportError("WebSocket frame has unsupported reserved bits")
            opcode = first & 0x0F
            masked = bool(second & 0x80)
            if masked:
                raise TransportError("server WebSocket frame must not be masked")
            length = second & 0x7F
            if length == 126:
                length = struct.unpack("!H", self._exact(2, deadline))[0]
            elif length == 127:
                length = struct.unpack("!Q", self._exact(8, deadline))[0]
            if length > self.max_frame_bytes or total + length > self.max_frame_bytes:
                raise TransportError("WebSocket message is too large")
            payload = self._exact(length, deadline)
            if opcode == 8:
                raise TransportError("WebSocket server closed the connection")
            if opcode == 9:
                if not final or length > 125:
                    raise TransportError("malformed WebSocket ping")
                self._send_frame(payload, opcode=10, deadline=deadline)
                continue
            if opcode == 10:
                if not final or length > 125:
                    raise TransportError("malformed WebSocket pong")
                continue
            if opcode == 1:
                if started:
                    raise TransportError("unexpected WebSocket text frame")
                started = True
            elif opcode == 0:
                if not started:
                    raise TransportError("unexpected WebSocket continuation frame")
            else:
                raise TransportError(f"unsupported WebSocket opcode: {opcode}")
            chunks.append(payload)
            total += length
            if final:
                break
        try:
            value = json.loads(b"".join(chunks).decode("utf-8"))
        except (UnicodeError, ValueError) as exc:
            raise TransportError("WebSocket message contains invalid JSON") from exc
        if not isinstance(value, dict):
            raise TransportError("JSON-RPC message must be an object")
        return value

    def notify(
        self,
        method: str,
        params: Mapping[str, Any],
        deadline: Optional[float] = None,
    ) -> None:
        self._send_frame(
            json.dumps({"method": method, "params": params}, separators=(",", ":")).encode(),
            deadline=deadline,
        )

    def call(
        self,
        method: str,
        params: Mapping[str, Any],
        deadline: Optional[float] = None,
    ) -> Mapping[str, Any]:
        self.sequence += 1
        request_id = self.sequence
        deadline = deadline if deadline is not None else time.monotonic() + self.timeout
        if deadline <= time.monotonic():
            raise TransportError(f"Codex RPC {method} timed out")
        self._send_frame(
            json.dumps(
                {"id": request_id, "method": method, "params": params},
                separators=(",", ":"),
            ).encode(),
            deadline=deadline,
        )
        while time.monotonic() < deadline:
            response = self.receive(deadline)
            if response.get("id") != request_id:
                continue
            if "error" in response:
                raise TransportError(
                    f"Codex RPC {method} failed: "
                    + json.dumps(response["error"], sort_keys=True)
                )
            result = response.get("result")
            if not isinstance(result, dict):
                raise TransportError(f"Codex RPC {method} returned a non-object result")
            return result
        raise TransportError(f"Codex RPC {method} timed out")

    def close(self) -> None:
        sock = getattr(self, "sock", None)
        if sock is None:
            return
        try:
            self._send_frame(
                struct.pack("!H", 1000),
                opcode=8,
                deadline=time.monotonic() + min(0.1, self.timeout),
            )
        except (ContinuationError, OSError):
            pass
        finally:
            sock.close()
            self.sock = None


def _atomic_json(path: Path, value: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    encoded = (json.dumps(value, sort_keys=True, indent=2) + "\n").encode("utf-8")
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=str(path.parent))
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "wb") as stream:
            stream.write(encoded)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        directory_fd = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    except BaseException:
        with contextlib.suppress(FileNotFoundError):
            os.unlink(temporary)
        raise


def _read_journal(path: Path) -> Optional[Dict[str, Any]]:
    if not path.exists():
        return None
    try:
        if not stat.S_ISREG(path.stat().st_mode) or path.is_symlink():
            raise ContinuationError(f"action journal is not a regular file: {path}")
        value = json.loads(path.read_text(encoding="utf-8"))
    except ContinuationError:
        raise
    except (OSError, UnicodeError, ValueError) as exc:
        raise ContinuationError(f"cannot read action journal {path}: {exc}") from exc
    if not isinstance(value, dict) or value.get("schema_version") != 1:
        raise ContinuationError(f"action journal has an unsupported schema: {path}")
    return value


@contextlib.contextmanager
def _action_lock(
    state_dir: Path, action_id: str, deadline: float
) -> Iterator[None]:
    locks = state_dir / "locks"
    locks.mkdir(parents=True, exist_ok=True, mode=0o700)
    lock_path = locks / f"{action_id}.lock"
    fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o600)
    try:
        while True:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise UncertainDelivery(
                        "timed out waiting for the action delivery lock"
                    )
                select.select([], [], [], min(0.05, remaining))
        yield
    finally:
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)


def _text_inputs(inputs: Any) -> List[str]:
    if not isinstance(inputs, list):
        raise UncertainDelivery("Codex returned malformed user input; delivery is unknown")
    texts = []
    required = {
        "text": ("text",),
        "image": ("url",),
        "localImage": ("path",),
        "audio": ("url",),
        "localAudio": ("path",),
        "skill": ("name", "path"),
        "mention": ("name", "path"),
    }
    for item in inputs:
        if not isinstance(item, dict) or item.get("type") not in required:
            raise UncertainDelivery("Codex returned unknown or malformed user input")
        if not all(isinstance(item.get(field), str) for field in required[item["type"]]):
            raise UncertainDelivery("Codex returned unknown or malformed user input")
        if item["type"] == "text":
            texts.append(item["text"])
    return texts


def _match_inputs(
    inputs: Any,
    *,
    client_id: Any,
    action_id: str,
    owner: str,
    payload_sha256: str,
    rendered_message: str,
) -> bool:
    texts = _text_inputs(inputs)
    markers = [match for text in texts for match in MARKER_RE.finditer(text)]
    action_markers = [match for match in markers if match.group(1) == action_id]
    if client_id == action_id and not action_markers:
        raise IntentConflict("action ID exists with different intent")
    if action_markers and client_id != action_id:
        raise IntentConflict("action marker exists with a different client ID")
    for match in action_markers:
        if match.group(2) != owner or match.group(3) != payload_sha256:
            raise IntentConflict("action ID exists with different intent")
        if rendered_message not in texts:
            raise IntentConflict("action ID exists with different intent")
        return True
    return False


def _paged(
    rpc: Any,
    method: str,
    params: Mapping[str, Any],
    deadline: float,
    max_pages: int,
) -> Iterator[Mapping[str, Any]]:
    cursor = None
    seen = set()
    for _page in range(max_pages):
        request = dict(params)
        if cursor is not None:
            request["cursor"] = cursor
        try:
            result = _rpc_call(rpc, method, request, deadline)
        except Exception as exc:
            raise UncertainDelivery(f"cannot completely read Codex {method}: {exc}") from exc
        data = result.get("data")
        if not isinstance(data, list):
            raise UncertainDelivery(f"Codex {method} returned malformed data")
        for item in data:
            if not isinstance(item, dict):
                raise UncertainDelivery(f"Codex {method} returned a malformed item")
            yield item
        cursor = result.get("nextCursor")
        if cursor is None:
            return
        if not isinstance(cursor, str) or not cursor or cursor in seen:
            raise UncertainDelivery(f"Codex {method} returned invalid pagination")
        seen.add(cursor)
    raise UncertainDelivery(f"Codex {method} exceeded the bounded history scan")


def _reconcile(
    rpc: Any,
    *,
    thread_id: str,
    action_id: str,
    owner: str,
    payload_sha256: str,
    rendered_message: str,
    deadline: float,
) -> Optional[Dict[str, Any]]:
    match_args = {
        "action_id": action_id,
        "owner": owner,
        "payload_sha256": payload_sha256,
        "rendered_message": rendered_message,
    }
    for queued in _paged(
        rpc,
        "thread/queue/list",
        {"threadId": thread_id, "limit": QUEUE_PAGE_SIZE},
        deadline,
        MAX_PAGES,
    ):
        if (
            not isinstance(queued.get("id"), str)
            or not queued["id"]
            or not isinstance(queued.get("clientUserMessageId"), str)
        ):
            raise UncertainDelivery("Codex queue contains a malformed submission")
        if _match_inputs(
            queued.get("input"),
            client_id=queued.get("clientUserMessageId"),
            **match_args,
        ):
            submission_id = queued.get("id")
            if not isinstance(submission_id, str) or not submission_id:
                raise UncertainDelivery("matching Codex queue entry has no submission ID")
            return {"delivery": "queued", "submission_id": submission_id}

    for entry in _paged(
        rpc,
        "thread/items/list",
        {
            "threadId": thread_id,
            "limit": HISTORY_PAGE_SIZE,
            "sortDirection": "desc",
        },
        deadline,
        MAX_HISTORY_PAGES,
    ):
        item = entry.get("item")
        turn_id = entry.get("turnId")
        if not isinstance(item, dict) or not isinstance(turn_id, str) or not turn_id:
            raise UncertainDelivery("Codex item history is incomplete or malformed")
        if item.get("type") != "userMessage":
            continue
        if (
            not isinstance(item.get("id"), str)
            or not item["id"]
            or (
                item.get("clientId") is not None
                and not isinstance(item.get("clientId"), str)
            )
        ):
            raise UncertainDelivery("Codex history contains a malformed user message")
        if _match_inputs(
            item.get("content"), client_id=item.get("clientId"), **match_args
        ):
            return {"delivery": "history", "turn_id": turn_id}
    return None


def _identity(
    action_id: str,
    owner: str,
    thread_id: str,
    payload_sha256: str,
    endpoint: Path,
) -> Dict[str, Any]:
    return {
        "schema_version": 1,
        "action_id": action_id,
        "owner": owner,
        "thread_id": thread_id,
        "payload_sha256": payload_sha256,
        "endpoint": str(endpoint),
    }


def _assert_same_intent(journal: Mapping[str, Any], identity: Mapping[str, Any]) -> None:
    for key in ("action_id", "owner", "thread_id", "payload_sha256", "endpoint"):
        if journal.get(key) != identity[key]:
            raise IntentConflict(f"action ID is already bound to different intent ({key})")


def _receipt(identity: Mapping[str, Any], found: Mapping[str, Any]) -> Dict[str, Any]:
    value = dict(identity)
    value.update(found)
    value.update({"status": "delivered", "recorded_at": int(time.time())})
    return value


def _persist_or_uncertain(
    path: Path, value: Mapping[str, Any], description: str
) -> None:
    try:
        _atomic_json(path, value)
    except Exception as exc:
        raise UncertainDelivery(
            f"{description} could not be persisted; delivery state is uncertain: {exc}"
        ) from exc


def _connect(factory: Callable[[Path, float], Any], endpoint: Path, timeout: float) -> Any:
    try:
        return factory(endpoint, timeout)
    except ContinuationError:
        raise
    except Exception as exc:
        raise TransportError(f"cannot connect to Codex Unix socket {endpoint}: {exc}") from exc


def _remaining(deadline: float) -> float:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise UncertainDelivery("continuation delivery reached its total timeout")
    return remaining


def _rpc_call(
    rpc: Any,
    method: str,
    params: Mapping[str, Any],
    deadline: float,
) -> Mapping[str, Any]:
    _remaining(deadline)
    if isinstance(rpc, WebSocketRPC):
        result = rpc.call(method, params, deadline=deadline)
    else:
        result = rpc.call(method, params)
    _remaining(deadline)
    return result


def enqueue_once(
    *,
    action_id: str,
    owner: str,
    thread_id: str,
    message: str,
    state_dir: Path,
    endpoint: Path,
    timeout: float = 10.0,
    rpc_factory: Callable[[Path, float], Any] = WebSocketRPC,
) -> Dict[str, Any]:
    """Queue an action once, or return durable evidence of its existing delivery."""
    _validate_id("action ID", action_id)
    _validate_id("owner", owner)
    _validate_id("thread ID", thread_id)
    endpoint = Path(endpoint)
    if not endpoint.is_absolute():
        raise ContinuationError("Codex socket path must be absolute")
    rendered_message, payload_sha256 = _render_message(action_id, owner, message)
    if timeout <= 0 or timeout > 120:
        raise ContinuationError("timeout must be greater than zero and at most 120 seconds")
    deadline = time.monotonic() + timeout
    identity = _identity(action_id, owner, thread_id, payload_sha256, endpoint)
    state_dir = Path(state_dir).absolute()
    journal_path = state_dir / "actions" / f"{action_id}.json"

    with _action_lock(state_dir, action_id, deadline):
        journal = _read_journal(journal_path)
        if journal is not None:
            _assert_same_intent(journal, identity)
            if journal.get("status") == "delivered":
                return dict(journal)
            if journal.get("status") not in ("prepared", "sending", "uncertain"):
                raise ContinuationError("action journal has an invalid state")
        else:
            journal = dict(identity)
            journal.update({"status": "prepared", "recorded_at": int(time.time())})
            _atomic_json(journal_path, journal)

        try:
            with _connect(rpc_factory, endpoint, _remaining(deadline)) as rpc:
                found = _reconcile(
                    rpc,
                    thread_id=thread_id,
                    action_id=action_id,
                    owner=owner,
                    payload_sha256=payload_sha256,
                    rendered_message=rendered_message,
                    deadline=deadline,
                )
        except UncertainDelivery:
            raise
        except TransportError as exc:
            raise UncertainDelivery(
                f"cannot reconcile Codex queue and history: {exc}"
            ) from exc
        except ContinuationError:
            raise
        except Exception as exc:
            raise UncertainDelivery(f"cannot reconcile Codex queue and history: {exc}") from exc
        if found is not None:
            receipt = _receipt(identity, found)
            _persist_or_uncertain(journal_path, receipt, "the delivery receipt")
            return receipt

        if journal["status"] in ("sending", "uncertain"):
            raise UncertainDelivery(
                "prior send remains uncertain and was not resubmitted; inspect Codex queue/history"
            )

        sending = dict(identity)
        sending.update({"status": "sending", "recorded_at": int(time.time())})
        _atomic_json(journal_path, sending)
        try:
            with _connect(rpc_factory, endpoint, _remaining(deadline)) as rpc:
                _rpc_call(
                    rpc,
                    "thread/queue/add",
                    {
                        "threadId": thread_id,
                        "clientUserMessageId": action_id,
                        "input": [{"type": "text", "text": rendered_message}],
                    },
                    deadline,
                )
        except Exception as exc:
            uncertain = dict(identity)
            uncertain.update(
                {
                    "status": "uncertain",
                    "recorded_at": int(time.time()),
                    "last_error": str(exc),
                }
            )
            _persist_or_uncertain(journal_path, uncertain, "uncertain delivery state")

        try:
            with _connect(rpc_factory, endpoint, _remaining(deadline)) as rpc:
                found = _reconcile(
                    rpc,
                    thread_id=thread_id,
                    action_id=action_id,
                    owner=owner,
                    payload_sha256=payload_sha256,
                    rendered_message=rendered_message,
                    deadline=deadline,
                )
        except UncertainDelivery:
            raise
        except Exception as exc:
            raise UncertainDelivery(
                f"send outcome is uncertain because read-back failed; not resubmitted: {exc}"
            ) from exc
        if found is None:
            raise UncertainDelivery(
                "send outcome is uncertain and action is absent from complete "
                "read-back; not resubmitted"
            )
        receipt = _receipt(identity, found)
        _persist_or_uncertain(journal_path, receipt, "the delivery receipt")
        return receipt


def _read_message(path: Path) -> str:
    path = Path(path)
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except OSError as exc:
        raise ContinuationError(f"cannot open message file {path}: {exc}") from exc
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise ContinuationError(f"message file is not a regular file: {path}")
        data = os.read(fd, MAX_MESSAGE_BYTES + 1)
    finally:
        os.close(fd)
    if len(data) > MAX_MESSAGE_BYTES:
        raise ContinuationError(f"message file exceeds {MAX_MESSAGE_BYTES} bytes")
    try:
        return data.decode("utf-8")
    except UnicodeError as exc:
        raise ContinuationError("message file is not valid UTF-8") from exc


def _default_state_dir() -> Path:
    root = os.environ.get("HEX_DIR")
    if not root:
        raise ContinuationError("HEX_DIR is required unless --state-dir is provided")
    return Path(root) / ".hex/state/codex-continuation"


def _default_socket() -> Path:
    codex_home = os.environ.get("CODEX_HOME")
    root = Path(codex_home) if codex_home else Path.home() / ".codex"
    return root / "app-server-control/app-server-control.sock"


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Queue one identified continuation on an existing Codex thread."
    )
    parser.add_argument("--action-id", required=True)
    parser.add_argument("--owner", required=True)
    parser.add_argument("--thread-id", required=True)
    parser.add_argument("--message-file", required=True, type=Path)
    parser.add_argument("--state-dir", type=Path)
    parser.add_argument(
        "--socket",
        type=Path,
        default=_default_socket(),
    )
    parser.add_argument("--timeout", type=float, default=10.0)
    return parser


def run_cli(argv: Optional[List[str]] = None) -> tuple[int, str]:
    try:
        args = _parser().parse_args(argv)
        state_dir = args.state_dir if args.state_dir is not None else _default_state_dir()
        result = enqueue_once(
            action_id=args.action_id,
            owner=args.owner,
            thread_id=args.thread_id,
            message=_read_message(args.message_file),
            state_dir=state_dir,
            endpoint=args.socket,
            timeout=args.timeout,
        )
        return 0, json.dumps(result, sort_keys=True)
    except SystemExit:
        raise
    except Exception as exc:
        kind = "uncertain" if isinstance(exc, UncertainDelivery) else "error"
        value = {"status": kind, "error": str(exc), "error_type": type(exc).__name__}
        return 3 if kind == "uncertain" else 2, json.dumps(value, sort_keys=True)


def main() -> int:
    code, output = run_cli()
    print(output, file=sys.stdout if code == 0 else sys.stderr)
    return code


if __name__ == "__main__":
    raise SystemExit(main())
