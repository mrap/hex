import importlib.util
import base64
import fcntl
import hashlib
import json
import multiprocessing
import os
import socket
import struct
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock


SOURCE = Path(__file__).parents[1] / "system/scripts/codex_continuation.py"
spec = importlib.util.spec_from_file_location("codex_continuation", SOURCE)
CONTINUATION = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = CONTINUATION
spec.loader.exec_module(CONTINUATION)


class FakeServer:
    def __init__(self):
        self.queue = []
        self.turns = []
        self.add_calls = 0
        self.add_delay = 0
        self.fail_after_add = False
        self.queue_error = None
        self.turns_error = None
        self.lock = threading.Lock()

    def connect(self, _endpoint, _timeout):
        return FakeRPC(self)


class FakeRPC:
    def __init__(self, server):
        self.server = server

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return None

    def call(self, method, params):
        if method == "thread/queue/list":
            if self.server.queue_error:
                raise self.server.queue_error
            return {"data": list(self.server.queue), "nextCursor": None}
        if method == "thread/items/list":
            if self.server.turns_error:
                raise self.server.turns_error
            data = []
            for turn in self.server.turns:
                for item in turn.get("items", []):
                    data.append({"turnId": turn["id"], "item": item})
            return {"data": data, "nextCursor": None}
        if method == "thread/queue/add":
            time.sleep(self.server.add_delay)
            queued = {
                "id": f"queue-{len(self.server.queue) + 1}",
                "clientUserMessageId": params["clientUserMessageId"],
                "input": params["input"],
            }
            with self.server.lock:
                self.server.add_calls += 1
                self.server.queue.append(queued)
            if self.server.fail_after_add:
                raise TimeoutError("reply lost after add")
            return {"queuedSubmission": queued}
        raise AssertionError(f"unexpected RPC: {method}")


class SimulatedCrash(BaseException):
    pass


class ProcessRPC:
    def __init__(self, queue_file):
        self.queue_file = Path(queue_file)

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return None

    def call(self, method, params):
        if method == "thread/queue/list":
            if not self.queue_file.exists():
                data = []
            else:
                data = [json.loads(line) for line in self.queue_file.read_text().splitlines()]
            return {"data": data, "nextCursor": None}
        if method == "thread/items/list":
            return {"data": [], "nextCursor": None}
        if method == "thread/queue/add":
            queued = {
                "id": "process-queue-1",
                "clientUserMessageId": params["clientUserMessageId"],
                "input": params["input"],
            }
            with self.queue_file.open("a", encoding="utf-8") as stream:
                stream.write(json.dumps(queued) + "\n")
            return {"queuedSubmission": queued}
        raise AssertionError(f"unexpected RPC: {method}")


class ProcessRPCFactory:
    def __init__(self, queue_file):
        self.queue_file = queue_file

    def __call__(self, _endpoint, _timeout):
        return ProcessRPC(self.queue_file)


def process_enqueue(start, output, kwargs, queue_file):
    start.wait()
    try:
        result = CONTINUATION.enqueue_once(
            **kwargs, rpc_factory=ProcessRPCFactory(queue_file)
        )
        output.put(("ok", result))
    except Exception as exc:
        output.put(("error", repr(exc)))


class ContinuationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.state = Path(self.temp.name) / "state"
        self.message = "Finish the reviewed source change."
        self.server = FakeServer()
        self.kwargs = {
            "action_id": "fix-continuation-20260909",
            "owner": "root",
            "thread_id": "thread-123",
            "message": self.message,
            "state_dir": self.state,
            "endpoint": Path("/tmp/test-codex.sock"),
            "rpc_factory": self.server.connect,
        }
        self.addCleanup(self.temp.cleanup)

    def enqueue(self, **changes):
        values = dict(self.kwargs)
        values.update(changes)
        return CONTINUATION.enqueue_once(**values)

    def test_two_concurrent_callers_add_once(self):
        self.server.add_delay = 0.05
        barrier = threading.Barrier(3)
        results = []
        errors = []

        def run():
            barrier.wait()
            try:
                results.append(self.enqueue())
            except Exception as exc:  # pragma: no cover - assertion reports details
                errors.append(exc)

        threads = [threading.Thread(target=run) for _ in range(2)]
        for thread in threads:
            thread.start()
        barrier.wait()
        for thread in threads:
            thread.join(2)
        self.assertFalse(errors)
        self.assertEqual(self.server.add_calls, 1)
        self.assertEqual({result["submission_id"] for result in results}, {"queue-1"})
        self.assertEqual({result["delivery"] for result in results}, {"queued"})

    def test_two_concurrent_processes_add_once(self):
        context = multiprocessing.get_context("fork")
        start = context.Event()
        output = context.Queue()
        queue_file = Path(self.temp.name) / "server-queue.jsonl"
        kwargs = dict(self.kwargs)
        kwargs.pop("rpc_factory")
        processes = [
            context.Process(
                target=process_enqueue,
                args=(start, output, kwargs, queue_file),
            )
            for _ in range(2)
        ]
        for process in processes:
            process.start()
        start.set()
        results = [output.get(timeout=3) for _ in processes]
        for process in processes:
            process.join(3)
            self.assertEqual(process.exitcode, 0)
        self.assertEqual({status for status, _ in results}, {"ok"})
        self.assertEqual(len(queue_file.read_text().splitlines()), 1)

    def test_repeat_returns_durable_receipt_without_contacting_server(self):
        first = self.enqueue()
        second_server = FakeServer()
        second = self.enqueue(rpc_factory=second_server.connect)
        self.assertEqual(first, second)
        self.assertEqual(second_server.add_calls, 0)

    def test_sequential_process_restart_returns_same_receipt(self):
        context = multiprocessing.get_context("fork")
        start = context.Event()
        output = context.Queue()
        queue_file = Path(self.temp.name) / "restart-server-queue.jsonl"
        kwargs = dict(self.kwargs)
        kwargs.pop("rpc_factory")
        start.set()

        first_process = context.Process(
            target=process_enqueue, args=(start, output, kwargs, queue_file)
        )
        first_process.start()
        first = output.get(timeout=3)
        first_process.join(3)
        self.assertEqual(first_process.exitcode, 0)

        second_process = context.Process(
            target=process_enqueue, args=(start, output, kwargs, queue_file)
        )
        second_process.start()
        second = output.get(timeout=3)
        second_process.join(3)
        self.assertEqual(second_process.exitcode, 0)

        self.assertEqual(first, second)
        self.assertEqual(first[0], "ok")
        self.assertEqual(len(queue_file.read_text().splitlines()), 1)

    def test_timeout_after_server_add_recovers_from_queue(self):
        self.server.fail_after_add = True
        receipt = self.enqueue()
        self.assertEqual(receipt["delivery"], "queued")
        self.assertEqual(receipt["submission_id"], "queue-1")
        self.assertEqual(self.server.add_calls, 1)

    def test_process_crash_after_add_is_recovered_on_restart(self):
        class CrashAfterAdd(FakeRPC):
            def call(inner_self, method, params):
                if method != "thread/queue/add":
                    return super().call(method, params)
                queued = {
                    "id": "queue-crash",
                    "clientUserMessageId": params["clientUserMessageId"],
                    "input": params["input"],
                }
                inner_self.server.add_calls += 1
                inner_self.server.queue.append(queued)
                raise SimulatedCrash()

        self.server.connect = lambda _endpoint, _timeout: CrashAfterAdd(self.server)
        with self.assertRaises(SimulatedCrash):
            self.enqueue(rpc_factory=self.server.connect)
        recovered = self.enqueue(rpc_factory=self.server.connect)
        self.assertEqual(recovered["submission_id"], "queue-crash")
        self.assertEqual(self.server.add_calls, 1)

    def test_consumed_action_is_recovered_from_full_history(self):
        first = self.enqueue()
        queued = self.server.queue.pop()
        self.server.turns.append({
            "id": "turn-9",
            "status": "completed",
            "itemsView": "full",
            "items": [{
                "id": "message-9",
                "type": "userMessage",
                "clientId": queued["clientUserMessageId"],
                "content": queued["input"],
            }],
        })
        journal = self.state / "actions" / f"{self.kwargs['action_id']}.json"
        journal.unlink()
        recovered = self.enqueue()
        self.assertEqual(first["payload_sha256"], recovered["payload_sha256"])
        self.assertEqual(recovered["delivery"], "history")
        self.assertEqual(recovered["turn_id"], "turn-9")
        self.assertEqual(self.server.add_calls, 1)

    def test_reused_action_with_different_payload_is_rejected(self):
        self.enqueue()
        with self.assertRaisesRegex(CONTINUATION.ContinuationError, "different intent") as caught:
            self.enqueue(message="Do something else.")
        self.assertNotIsInstance(caught.exception, CONTINUATION.UncertainDelivery)
        self.assertEqual(self.server.add_calls, 1)

    def test_unknown_history_is_loud_and_does_not_add(self):
        self.server.turns_error = TimeoutError("history timeout")
        with self.assertRaisesRegex(CONTINUATION.UncertainDelivery, "history"):
            self.enqueue()
        self.assertEqual(self.server.add_calls, 0)

    def test_action_lock_wait_is_bounded(self):
        locks = self.state / "locks"
        locks.mkdir(parents=True)
        lock_path = locks / f"{self.kwargs['action_id']}.lock"
        fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o600)
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        started = time.monotonic()
        try:
            with self.assertRaisesRegex(CONTINUATION.UncertainDelivery, "delivery lock"):
                self.enqueue(timeout=0.05)
        finally:
            fcntl.flock(fd, fcntl.LOCK_UN)
            os.close(fd)
        self.assertLess(time.monotonic() - started, 0.2)
        self.assertEqual(self.server.add_calls, 0)

    def test_pagination_uses_one_total_deadline(self):
        class EndlessQueue(FakeRPC):
            def __init__(inner_self, server):
                super().__init__(server)
                inner_self.page = 0

            def call(inner_self, method, params):
                if method == "thread/queue/list":
                    time.sleep(0.03)
                    inner_self.page += 1
                    return {"data": [], "nextCursor": f"page-{inner_self.page}"}
                return super().call(method, params)

        self.server.connect = lambda _endpoint, _timeout: EndlessQueue(self.server)
        started = time.monotonic()
        with self.assertRaisesRegex(CONTINUATION.UncertainDelivery, "total timeout"):
            self.enqueue(rpc_factory=self.server.connect, timeout=0.05)
        self.assertLess(time.monotonic() - started, 0.2)
        self.assertEqual(self.server.add_calls, 0)

    def test_ambiguous_absent_send_is_never_retried(self):
        class LostBeforeVisible(FakeRPC):
            def call(inner_self, method, params):
                if method == "thread/queue/add":
                    inner_self.server.add_calls += 1
                    raise TimeoutError("unknown send boundary")
                return super().call(method, params)

        self.server.connect = lambda _endpoint, _timeout: LostBeforeVisible(self.server)
        with self.assertRaisesRegex(CONTINUATION.UncertainDelivery, "not resubmitted"):
            self.enqueue(rpc_factory=self.server.connect)
        with self.assertRaisesRegex(CONTINUATION.UncertainDelivery, "not resubmitted"):
            self.enqueue(rpc_factory=self.server.connect)
        self.assertEqual(self.server.add_calls, 1)

    def test_incomplete_turn_items_are_loud(self):
        class IncompleteHistory(FakeRPC):
            def call(inner_self, method, params):
                if method == "thread/items/list":
                    return {"data": [{"turnId": "turn-incomplete"}], "nextCursor": None}
                return super().call(method, params)

        self.server.connect = lambda _endpoint, _timeout: IncompleteHistory(self.server)
        with self.assertRaisesRegex(CONTINUATION.UncertainDelivery, "incomplete"):
            self.enqueue(rpc_factory=self.server.connect)
        self.assertEqual(self.server.add_calls, 0)

    def test_malformed_queue_input_is_loud_and_does_not_add(self):
        self.server.queue.append({
            "id": "malformed-queue",
            "clientUserMessageId": "another-action",
            "input": [{"type": "text"}],
        })
        with self.assertRaisesRegex(CONTINUATION.UncertainDelivery, "malformed user input"):
            self.enqueue()
        self.assertEqual(self.server.add_calls, 0)

    def test_malformed_history_user_content_is_loud_and_does_not_add(self):
        self.server.turns.append({
            "id": "malformed-turn",
            "items": [{
                "id": "malformed-user",
                "type": "userMessage",
                "clientId": None,
                "content": [{"type": "unknown"}],
            }],
        })
        with self.assertRaisesRegex(CONTINUATION.UncertainDelivery, "malformed user input"):
            self.enqueue()
        self.assertEqual(self.server.add_calls, 0)

    def test_valid_non_text_input_does_not_block_add(self):
        self.server.queue.append({
            "id": "image-queue",
            "clientUserMessageId": "another-action",
            "input": [{"type": "image", "url": "data:image/png;base64,AA=="}],
        })
        receipt = self.enqueue()
        self.assertEqual(receipt["submission_id"], "queue-2")
        self.assertEqual(self.server.add_calls, 1)

    def test_marker_on_second_history_page_is_recovered(self):
        rendered, _payload_hash = CONTINUATION._render_message(
            self.kwargs["action_id"], self.kwargs["owner"], self.message
        )
        self.server.history_cursors = []

        class PagedHistory(FakeRPC):
            def call(inner_self, method, params):
                if method != "thread/items/list":
                    return super().call(method, params)
                self.assertEqual(params["limit"], 100)
                cursor = params.get("cursor")
                inner_self.server.history_cursors.append(cursor)
                if cursor is None:
                    return {
                        "data": [{
                            "turnId": "other-turn",
                            "item": {"id": "agent-1", "type": "agentMessage", "text": "other"},
                        }],
                        "nextCursor": "history-page-2",
                    }
                self.assertEqual(cursor, "history-page-2")
                return {
                    "data": [{
                        "turnId": "in-progress-turn",
                        "item": {
                            "id": "user-2",
                            "type": "userMessage",
                            "clientId": self.kwargs["action_id"],
                            "content": [{"type": "text", "text": rendered}],
                        },
                    }],
                    "nextCursor": None,
                }

        self.server.connect = lambda _endpoint, _timeout: PagedHistory(self.server)
        receipt = self.enqueue(rpc_factory=self.server.connect)
        self.assertEqual(receipt["delivery"], "history")
        self.assertEqual(receipt["turn_id"], "in-progress-turn")
        self.assertEqual(self.server.history_cursors, [None, "history-page-2"])
        self.assertEqual(self.server.add_calls, 0)

    def test_exact_marker_with_wrong_client_id_is_rejected(self):
        rendered, _payload_hash = CONTINUATION._render_message(
            self.kwargs["action_id"], self.kwargs["owner"], self.message
        )
        for location, client_id in (("queue", "other-client"), ("history", None)):
            with self.subTest(location=location):
                server = FakeServer()
                if location == "queue":
                    server.queue.append({
                        "id": "marker-collision",
                        "clientUserMessageId": client_id,
                        "input": [{"type": "text", "text": rendered}],
                    })
                else:
                    server.turns.append({
                        "id": "marker-collision-turn",
                        "items": [{
                            "id": "marker-collision-user",
                            "type": "userMessage",
                            "clientId": client_id,
                            "content": [{"type": "text", "text": rendered}],
                        }],
                    })
                with self.assertRaisesRegex(
                    CONTINUATION.IntentConflict, "different client ID"
                ):
                    self.enqueue(
                        state_dir=self.state / location,
                        rpc_factory=server.connect,
                    )
                self.assertEqual(server.add_calls, 0)

    def test_history_page_bound_is_uncertain_and_does_not_add(self):
        self.server.history_calls = 0

        class EndlessHistory(FakeRPC):
            def call(inner_self, method, params):
                if method != "thread/items/list":
                    return super().call(method, params)
                inner_self.server.history_calls += 1
                return {
                    "data": [],
                    "nextCursor": f"page-{inner_self.server.history_calls}",
                }

        self.server.connect = lambda _endpoint, _timeout: EndlessHistory(self.server)
        with self.assertRaisesRegex(CONTINUATION.UncertainDelivery, "bounded history scan"):
            self.enqueue(rpc_factory=self.server.connect, timeout=2)
        self.assertEqual(self.server.history_calls, CONTINUATION.MAX_HISTORY_PAGES)
        self.assertEqual(self.server.add_calls, 0)

    def test_marker_after_one_hundred_history_pages_is_recovered(self):
        rendered, _payload_hash = CONTINUATION._render_message(
            self.kwargs["action_id"], self.kwargs["owner"], self.message
        )
        self.server.history_calls = 0

        class LongHistory(FakeRPC):
            def call(inner_self, method, params):
                if method != "thread/items/list":
                    return super().call(method, params)
                inner_self.server.history_calls += 1
                if inner_self.server.history_calls <= 100:
                    return {
                        "data": [],
                        "nextCursor": f"page-{inner_self.server.history_calls}",
                    }
                return {
                    "data": [{
                        "turnId": "late-turn",
                        "item": {
                            "id": "late-user",
                            "type": "userMessage",
                            "clientId": self.kwargs["action_id"],
                            "content": [{"type": "text", "text": rendered}],
                        },
                    }],
                    "nextCursor": None,
                }

        self.server.connect = lambda _endpoint, _timeout: LongHistory(self.server)
        receipt = self.enqueue(rpc_factory=self.server.connect)
        self.assertEqual(receipt["delivery"], "history")
        self.assertEqual(receipt["turn_id"], "late-turn")
        self.assertEqual(self.server.history_calls, 101)
        self.assertEqual(self.server.add_calls, 0)

    def test_deep_duplicate_history_cursor_is_uncertain_and_does_not_add(self):
        self.server.history_calls = 0

        class CyclicHistory(FakeRPC):
            def call(inner_self, method, params):
                if method != "thread/items/list":
                    return super().call(method, params)
                inner_self.server.history_calls += 1
                if inner_self.server.history_calls <= 100:
                    return {
                        "data": [],
                        "nextCursor": f"page-{inner_self.server.history_calls}",
                    }
                return {"data": [], "nextCursor": "page-100"}

        self.server.connect = lambda _endpoint, _timeout: CyclicHistory(self.server)
        with self.assertRaisesRegex(CONTINUATION.UncertainDelivery, "invalid pagination"):
            self.enqueue(rpc_factory=self.server.connect)
        self.assertEqual(self.server.history_calls, 101)
        self.assertEqual(self.server.add_calls, 0)

    def test_complete_long_history_without_marker_adds_once_and_repeats(self):
        self.server.history_calls = 0

        class CompleteLongHistory(FakeRPC):
            def call(inner_self, method, params):
                if method != "thread/items/list":
                    return super().call(method, params)
                inner_self.server.history_calls += 1
                if inner_self.server.history_calls <= 101:
                    return {
                        "data": [],
                        "nextCursor": f"page-{inner_self.server.history_calls}"
                        if inner_self.server.history_calls < 101
                        else None,
                    }
                raise AssertionError("history was read after complete first scan")

        self.server.connect = lambda _endpoint, _timeout: CompleteLongHistory(self.server)
        first = self.enqueue(rpc_factory=self.server.connect)
        second = self.enqueue(rpc_factory=self.server.connect)
        self.assertEqual(first["delivery"], "queued")
        self.assertEqual(first["submission_id"], "queue-1")
        self.assertEqual(second, first)
        self.assertEqual(self.server.history_calls, 101)
        self.assertEqual(self.server.add_calls, 1)

    def test_queue_and_history_conflicts_are_definite(self):
        payload_hash = hashlib.sha256(self.message.encode()).hexdigest()
        for location in ("queue", "history"):
            for variant in ("hash", "owner", "body"):
                with self.subTest(location=location, variant=variant):
                    action_id = f"conflict-{location}-{variant}"
                    owner = self.kwargs["owner"]
                    marker_hash = "0" * 64 if variant == "hash" else payload_hash
                    marker_owner = "someone-else" if variant == "owner" else owner
                    body = "Wrong body" if variant == "body" else self.message
                    text = CONTINUATION.marker_for(
                        action_id, marker_owner, marker_hash
                    ) + "\n" + body
                    server = FakeServer()
                    queued = {
                        "id": "conflict-message",
                        "clientUserMessageId": action_id,
                        "input": [{"type": "text", "text": text}],
                    }
                    if location == "queue":
                        server.queue.append(queued)
                    else:
                        server.turns.append({
                            "id": "conflict-turn",
                            "items": [{
                                "id": "conflict-user",
                                "type": "userMessage",
                                "clientId": action_id,
                                "content": queued["input"],
                            }],
                        })
                    with self.assertRaisesRegex(
                        CONTINUATION.IntentConflict, "different intent"
                    ):
                        self.enqueue(
                            action_id=action_id,
                            state_dir=self.state / action_id,
                            rpc_factory=server.connect,
                        )
                    self.assertEqual(server.add_calls, 0)

    def test_uncertain_journal_write_failure_after_send_stays_uncertain(self):
        original = CONTINUATION._atomic_json

        def fail_uncertain(path, value):
            if value.get("status") == "uncertain":
                raise OSError("injected uncertain journal failure")
            return original(path, value)

        self.server.fail_after_add = True
        CONTINUATION._atomic_json = fail_uncertain
        try:
            with self.assertRaisesRegex(
                CONTINUATION.UncertainDelivery, "could not be persisted"
            ):
                self.enqueue()
        finally:
            CONTINUATION._atomic_json = original
        journal = self.state / "actions" / f"{self.kwargs['action_id']}.json"
        self.assertEqual(json.loads(journal.read_text())["status"], "sending")
        recovered = self.enqueue()
        self.assertEqual(recovered["submission_id"], "queue-1")
        self.assertEqual(self.server.add_calls, 1)

    def test_receipt_write_failure_after_send_stays_uncertain(self):
        original = CONTINUATION._atomic_json

        def fail_receipt(path, value):
            if value.get("status") == "delivered":
                raise OSError("injected receipt failure")
            return original(path, value)

        CONTINUATION._atomic_json = fail_receipt
        try:
            with self.assertRaisesRegex(
                CONTINUATION.UncertainDelivery, "could not be persisted"
            ):
                self.enqueue()
        finally:
            CONTINUATION._atomic_json = original
        journal = self.state / "actions" / f"{self.kwargs['action_id']}.json"
        self.assertEqual(json.loads(journal.read_text())["status"], "sending")
        recovered = self.enqueue()
        self.assertEqual(recovered["submission_id"], "queue-1")
        self.assertEqual(self.server.add_calls, 1)

    def test_restart_receipt_write_failure_stays_uncertain(self):
        rendered, payload_hash = CONTINUATION._render_message(
            self.kwargs["action_id"], self.kwargs["owner"], self.message
        )
        identity = CONTINUATION._identity(
            self.kwargs["action_id"],
            self.kwargs["owner"],
            self.kwargs["thread_id"],
            payload_hash,
            self.kwargs["endpoint"],
        )
        journal = self.state / "actions" / f"{self.kwargs['action_id']}.json"
        sending = dict(identity, status="sending", recorded_at=1)
        CONTINUATION._atomic_json(journal, sending)
        self.server.queue.append({
            "id": "restart-queue",
            "clientUserMessageId": self.kwargs["action_id"],
            "input": [{"type": "text", "text": rendered}],
        })
        original = CONTINUATION._atomic_json

        def fail_receipt(path, value):
            if value.get("status") == "delivered":
                raise OSError("injected restart receipt failure")
            return original(path, value)

        CONTINUATION._atomic_json = fail_receipt
        try:
            with self.assertRaisesRegex(
                CONTINUATION.UncertainDelivery, "could not be persisted"
            ):
                self.enqueue()
        finally:
            CONTINUATION._atomic_json = original
        self.assertEqual(json.loads(journal.read_text())["status"], "sending")
        recovered = self.enqueue()
        self.assertEqual(recovered["submission_id"], "restart-queue")
        self.assertEqual(self.server.add_calls, 0)


class TransportTests(unittest.TestCase):
    def socket_pair_client(self, incoming=b"", timeout=0.05, max_frame=64):
        client_sock, server_sock = socket.socketpair()
        client_sock.settimeout(timeout)
        if incoming:
            server_sock.sendall(incoming)
        client = CONTINUATION.WebSocketRPC.from_connected_socket(
            client_sock, timeout=timeout, max_frame_bytes=max_frame
        )
        self.addCleanup(client.close)
        self.addCleanup(server_sock.close)
        return client

    def test_oversized_frame_is_bounded(self):
        frame = bytes([0x81, 126]) + struct.pack("!H", 65)
        client = self.socket_pair_client(frame)
        with self.assertRaisesRegex(CONTINUATION.TransportError, "too large"):
            client.receive()

    def test_malformed_json_is_loud(self):
        client = self.socket_pair_client(b"\x81\x01{")
        with self.assertRaisesRegex(CONTINUATION.TransportError, "JSON"):
            client.receive()

    def test_timeout_is_bounded(self):
        client = self.socket_pair_client()
        started = time.monotonic()
        with self.assertRaisesRegex(CONTINUATION.TransportError, "timed out"):
            client.receive()
        self.assertLess(time.monotonic() - started, 0.5)

    def test_rpc_deadline_is_absolute_across_notifications(self):
        client_sock, server_sock = socket.socketpair()
        client = CONTINUATION.WebSocketRPC.from_connected_socket(
            client_sock, timeout=0.05, max_frame_bytes=1024
        )
        self.addCleanup(client.close)
        self.addCleanup(server_sock.close)

        def notifications():
            request = server_sock.recv(4096)
            self.assertTrue(request)
            frame = b'\x81\x21{"method":"progress","params":{}}'
            deadline = time.monotonic() + 0.2
            while time.monotonic() < deadline:
                try:
                    server_sock.sendall(frame)
                except OSError:
                    return
                time.sleep(0.01)

        sender = threading.Thread(target=notifications)
        sender.start()
        started = time.monotonic()
        with self.assertRaisesRegex(CONTINUATION.TransportError, "timed out"):
            client.call("test", {})
        self.assertLess(time.monotonic() - started, 0.15)
        sender.join(1)

    def test_constructor_uses_one_handshake_and_initialize_deadline(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "slow.sock"
            ready = threading.Event()
            errors = []

            def exact(conn, size):
                data = b""
                while len(data) < size:
                    chunk = conn.recv(size - len(data))
                    if not chunk:
                        raise EOFError()
                    data += chunk
                return data

            def receive_client_frame(conn):
                first, second = exact(conn, 2)
                size = second & 0x7F
                if size == 126:
                    size = struct.unpack("!H", exact(conn, 2))[0]
                elif size == 127:
                    size = struct.unpack("!Q", exact(conn, 8))[0]
                mask = exact(conn, 4)
                payload = exact(conn, size)
                return first & 0x0F, bytes(
                    value ^ mask[index % 4] for index, value in enumerate(payload)
                )

            def server():
                listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                try:
                    listener.bind(str(path))
                    listener.listen()
                    ready.set()
                    conn, _address = listener.accept()
                    with conn:
                        head = b""
                        while b"\r\n\r\n" not in head:
                            head += conn.recv(4096)
                        key = next(
                            line.split(b":", 1)[1].strip()
                            for line in head.split(b"\r\n")
                            if line.lower().startswith(b"sec-websocket-key:")
                        )
                        time.sleep(0.035)
                        accept = base64.b64encode(
                            hashlib.sha1(
                                key + CONTINUATION.WEBSOCKET_GUID.encode()
                            ).digest()
                        )
                        conn.sendall(
                            b"HTTP/1.1 101 Switching Protocols\r\n"
                            b"Upgrade: websocket\r\nConnection: Upgrade\r\n"
                            b"Sec-WebSocket-Accept: " + accept + b"\r\n\r\n"
                        )
                        _opcode, payload = receive_client_frame(conn)
                        request = json.loads(payload)
                        time.sleep(0.035)
                        response = json.dumps(
                            {"id": request["id"], "result": {}}
                        ).encode()
                        conn.sendall(bytes([0x81, len(response)]) + response)
                except (BrokenPipeError, ConnectionResetError, EOFError) as exc:
                    errors.append(exc)
                finally:
                    listener.close()

            thread = threading.Thread(target=server)
            thread.start()
            self.assertTrue(ready.wait(1))
            started = time.monotonic()
            with self.assertRaisesRegex(CONTINUATION.TransportError, "timed out"):
                CONTINUATION.WebSocketRPC(path, timeout=0.06)
            elapsed = time.monotonic() - started
            thread.join(1)
            self.assertLess(elapsed, 0.12)
            self.assertFalse(thread.is_alive())

    def test_fragmented_message_over_limit_is_bounded(self):
        incoming = b"\x01\x28" + (b"a" * 40) + b"\x80\x28" + (b"b" * 40)
        client = self.socket_pair_client(incoming)
        with self.assertRaisesRegex(CONTINUATION.TransportError, "too large"):
            client.receive()


class CliTests(unittest.TestCase):
    def test_cli_error_is_structured_and_nonzero(self):
        with tempfile.TemporaryDirectory() as temp:
            message = Path(temp) / "message.txt"
            message.write_text("Continue", encoding="utf-8")
            code, output = CONTINUATION.run_cli([
                "--action-id", "action-one",
                "--owner", "root",
                "--thread-id", "thread-one",
                "--message-file", str(message),
                "--state-dir", str(Path(temp) / "state"),
                "--socket", str(Path(temp) / "missing.sock"),
                "--timeout", "0.05",
            ])
        self.assertEqual(code, 3)
        value = json.loads(output)
        self.assertEqual(value["status"], "uncertain")
        self.assertIn("error", value)

    def test_cli_conflict_is_exit_two(self):
        original = CONTINUATION.enqueue_once

        def conflict(**_kwargs):
            raise CONTINUATION.IntentConflict("different intent")

        CONTINUATION.enqueue_once = conflict
        try:
            with tempfile.TemporaryDirectory() as temp:
                message = Path(temp) / "message.txt"
                message.write_text("Continue", encoding="utf-8")
                code, output = CONTINUATION.run_cli([
                    "--action-id", "action-one",
                    "--owner", "root",
                    "--thread-id", "thread-one",
                    "--message-file", str(message),
                    "--state-dir", str(Path(temp) / "state"),
                    "--socket", str(Path(temp) / "fake.sock"),
                ])
        finally:
            CONTINUATION.enqueue_once = original
        self.assertEqual(code, 2)
        self.assertEqual(json.loads(output)["status"], "error")

    def test_default_socket_honors_codex_home(self):
        with mock.patch.dict(os.environ, {"CODEX_HOME": "/tmp/codex-child"}):
            self.assertEqual(
                CONTINUATION._default_socket(),
                Path("/tmp/codex-child/app-server-control/app-server-control.sock"),
            )


if __name__ == "__main__":
    unittest.main()
