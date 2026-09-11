#!/usr/bin/env python3
"""Portable, advisory-only Google Developer Docs format observations.

This hook never calls a model, reads credentials, rewrites a reply, or blocks
delivery. It reports a small set of objective format findings to stderr and
metadata-only JSONL telemetry.
"""
import datetime
import json
import os
import re
import stat
import sys
from pathlib import Path


# Kept for local tooling that imports these names from the instance helper.
HARD_CAP = 40
SOFT_CAP = 28
SOFT_ALLOW = 2
PARA_SENT_CAP = 3
PARA_NUM_CAP = 2
MAX_INPUT = 256 * 1024
MAX_TRANSCRIPT = 512 * 1024


def _hex_dir():
    configured = os.environ.get("HEX_DIR")
    if configured:
        return Path(configured)
    here = Path(__file__).resolve()
    for parent in (here.parent, *here.parents):
        if (parent / ".hex").is_dir():
            return parent
    return None


_ROOT = _hex_dir()
LOG_PATH = os.environ.get(
    "GDD_GATE_LOG",
    str(_ROOT / ".hex" / "telemetry" / "gdd-gate.jsonl")
    if _ROOT else "gdd-gate.jsonl",
)


def _structured(text):
    try:
        value = json.loads(text)
    except (TypeError, ValueError, RecursionError):
        return False
    return isinstance(value, (dict, list))


def strip_exempt(text):
    """Remove code, quotes, URLs, and paths before objective checks."""
    lines = []
    fence = None
    for line in text.splitlines():
        if fence:
            if re.fullmatch(r" {0,3}" + re.escape(fence[0]) +
                            "{" + str(len(fence)) + r",}[ \t]*", line):
                fence = None
            lines.append("")
            continue
        opening = re.match(r"^ {0,3}(`{3,}|~{3,})", line)
        if opening:
            fence = opening.group(1)
            lines.append("")
        elif line.startswith(("    ", "\t")):
            lines.append("")
        else:
            lines.append(line)
    text = "\n".join(lines)
    text = re.sub(r"`[^`]*`", " ", text)
    text = "\n".join(
        "" if line.lstrip().startswith(">") else line
        for line in text.splitlines()
    )
    text = re.sub(r"\bhttps?://\S+", " ", text)
    return re.sub(r"(?:~|/)[\w.\-/]+", " ", text)


def _prose_paragraphs(body):
    paragraphs, current = [], []
    for line in body.splitlines():
        value = line.strip()
        listed = bool(re.match(r"^(?:[-*•]|\d+[.)])\s", value)) \
            or value.startswith("|") or bool(re.match(r"^#{1,6}\s", value))
        if not value or listed:
            if current:
                paragraphs.append(" ".join(current))
                current = []
            continue
        current.append(value)
    if current:
        paragraphs.append(" ".join(current))
    return paragraphs


def mechanical_lint(text):
    """Preserve the legacy pure diagnostic API for offline fixture tooling."""
    if not isinstance(text, str) or _structured(text):
        return []
    body = strip_exempt(text)
    findings = []
    if "—" in body or re.search(r"\s–\s", body):
        findings.append("em dash (or spaced en dash) used; rewrite without dashes")
    lines = [line for line in body.splitlines() if line.strip()]
    if lines and re.match(r"\s{0,3}#{1,6}\s", lines[0]):
        findings.append("reply starts with a header; line 1 is the verdict sentence, headers come after")
    if any(
        re.match(r"\s{0,3}#{1,6}\s", line)
        and len(re.sub(r"^\s{0,3}#{1,6}\s+", "", line).split()) > 6
        for line in lines
    ):
        findings.append("header(s) over 6 words; headers are short plain group labels")
    if re.search(r"^\s{0,3}\*\*[^*\n]{1,200}\*\*:?\s*$", body, flags=re.M):
        findings.append("bold-label pseudo-header on its own line; fold it into prose")
    sentences = [sentence.strip() for sentence in
                 re.split(r"[.!?]+(?:\s|$)|\n+", body) if sentence.strip()]
    packed = [sentence for sentence in sentences if sentence.count(",") >= 4]
    if packed:
        findings.append(f"{len(packed)} sentence(s) chain 4+ comma-separated items; break them into a bullet list")
    counts = [len(sentence.split()) for sentence in sentences]
    over_hard = [count for count in counts if count > HARD_CAP]
    over_soft = [count for count in counts if count > SOFT_CAP]
    if over_hard:
        findings.append(f"{len(over_hard)} sentence(s) over {HARD_CAP} words; split them")
    elif len(over_soft) > SOFT_ALLOW:
        findings.append(f"{len(over_soft)} sentences over {SOFT_CAP} words (max {SOFT_ALLOW}); shorten or split them")
    dense = sum(1 for paragraph in _prose_paragraphs(body)
                if len([sentence for sentence in
                        re.split(r"[.!?]+(?:\s|$)", paragraph)
                        if sentence.strip()]) > PARA_SENT_CAP)
    numbery = sum(1 for paragraph in _prose_paragraphs(body)
                  if len(re.findall(r"\d[\d,./:x%-]*", paragraph)) > PARA_NUM_CAP)
    if dense:
        findings.append(f"{dense} paragraph(s) pack more than {PARA_SENT_CAP} sentences; write one verdict line, then a bullet list with one fact per bullet")
    if numbery:
        findings.append(f"{numbery} paragraph(s) carry more than {PARA_NUM_CAP} numbers; move the numbers into a bullet list, one per line")
    return findings


def format_findings(text):
    """Return only the objective findings used by the advisory hook."""
    if not isinstance(text, str) or _structured(text):
        return []
    body = strip_exempt(text)
    findings = []
    if "—" in body or re.search(r"\s–\s", body):
        findings.append("em dash or spaced en dash")
    lines = [line for line in body.splitlines() if line.strip()]
    if lines and re.match(r"\s{0,3}#{1,6}\s", lines[0]):
        findings.append("reply starts with a header")
    if any(
        re.match(r"\s{0,3}#{1,6}\s", line)
        and len(re.sub(r"^\s{0,3}#{1,6}\s+", "", line).split()) > 6
        for line in lines
    ):
        findings.append("header over six words")
    if re.search(r"^\s{0,3}\*\*[^*\n]{1,200}\*\*:?\s*$", body, flags=re.M):
        findings.append("bold label used as a standalone header")
    return findings


def _open_regular(path, flags):
    """Nonblocking open rejects pipes/devices before any stream I/O."""
    descriptor = os.open(path, flags | os.O_NONBLOCK, 0o600)
    try:
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            raise OSError("advisory input/output must be a regular file")
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


def _log(row):
    row = dict(row)
    row.update({
        "ts": datetime.datetime.now().astimezone().isoformat(timespec="seconds"),
        "advisory": True,
    })
    try:
        descriptor = _open_regular(LOG_PATH, os.O_WRONLY | os.O_APPEND | os.O_CREAT)
        with os.fdopen(descriptor, "a", encoding="utf-8") as stream:
            stream.write(json.dumps(row, ensure_ascii=False) + "\n")
    except Exception as error:
        print(f"gdd-style-gate: log write failed: {error}", file=sys.stderr)


def _is_user(entry):
    if entry.get("type") != "user":
        return False
    content = (entry.get("message") or {}).get("content")
    if isinstance(content, str):
        return True
    if isinstance(content, list):
        blocks = [block for block in content if isinstance(block, dict)]
        return not blocks or any(block.get("type") != "tool_result" for block in blocks)
    return True


def _text_blocks(entry):
    content = (entry.get("message") or {}).get("content")
    if isinstance(content, str):
        return [content]
    if isinstance(content, list):
        return [
            block["text"] for block in content
            if isinstance(block, dict) and block.get("type") == "text"
            and isinstance(block.get("text"), str)
        ]
    return []


def transcript_final(path):
    """Read only a bounded tail and return the current turn's final text."""
    descriptor = _open_regular(path, os.O_RDONLY)
    with os.fdopen(descriptor, "rb") as stream:
        stream.seek(0, 2)
        size = stream.tell()
        stream.seek(max(0, size - MAX_TRANSCRIPT))
        lines = stream.read(MAX_TRANSCRIPT).decode("utf-8", errors="replace").splitlines()
    messages = []
    for line in lines:
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        if not isinstance(entry, dict):
            continue
        if _is_user(entry):
            messages = []
        elif entry.get("type") == "assistant":
            messages.extend(_text_blocks(entry))
    return messages[-1] if messages else None


def main(stream=None):
    stream = sys.stdin if stream is None else stream
    try:
        raw = stream.read(MAX_INPUT + 1)
        if len(raw) > MAX_INPUT:
            print("gdd-style-gate: input exceeds advisory limit", file=sys.stderr)
            return 0
        payload = json.loads(raw)
    except Exception as error:
        print(f"gdd-style-gate: malformed input, advisory skipped: {error}", file=sys.stderr)
        return 0
    if not isinstance(payload, dict) or payload.get("stop_hook_active"):
        return 0

    text = payload.get("last_assistant_message")
    if not isinstance(text, str) or not text:
        path = payload.get("transcript_path")
        if not isinstance(path, str) or not path:
            return 0
        try:
            text = transcript_final(path)
        except Exception as error:
            print(f"gdd-style-gate: transcript read failed, advisory skipped: {error}", file=sys.stderr)
            return 0
    if not text:
        return 0

    try:
        findings = format_findings(text)
    except Exception as error:
        print(f"gdd-style-gate: format analysis failed, advisory skipped: {error}", file=sys.stderr)
        return 0
    if not findings:
        return 0
    session = payload.get("session_id") or "unknown"
    for finding in findings:
        print(f"gdd-style-gate advisory: {finding}", file=sys.stderr)
    _log({"kind": "advisory", "session": session, "findings": findings,
          "count": len(findings)})
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
