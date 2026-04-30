#!/usr/bin/env python3
"""
Parse .jsonl session transcripts into readable daily markdown files.

Reads raw .jsonl files from raw/transcripts/, extracts user and assistant
messages, and outputs one readable file per day at raw/transcripts/YYYY-MM-DD.md.

Multiple sessions on the same day are combined into one file.

Usage:
    python3 parse_transcripts.py                     # Process all .jsonl files
    python3 parse_transcripts.py --file <path.jsonl>  # Process a specific file
    python3 parse_transcripts.py --dry-run            # Preview without writing
    python3 parse_transcripts.py --force              # Re-process all files

Part of the Hex system.
"""

import json
import argparse
import re
from datetime import datetime
from pathlib import Path
from collections import defaultdict


def _find_root():
    """Walk up from script location to find the agent root."""
    d = Path(__file__).resolve().parent
    for _ in range(6):
        if (d / "CLAUDE.md").exists():
            return d
        d = d.parent
    return Path(__file__).resolve().parent.parent


HEX_DIR = _find_root()
TRANSCRIPTS_DIR = HEX_DIR / "raw" / "transcripts"
PROCESSED_FILE = TRANSCRIPTS_DIR / ".parsed_transcripts"
AGENT_NAME = HEX_DIR.name.capitalize()


def load_processed():
    """Load set of already-processed .jsonl filenames."""
    if PROCESSED_FILE.exists():
        return set(PROCESSED_FILE.read_text().strip().split("\n"))
    return set()


def save_processed(processed):
    """Save set of processed .jsonl filenames."""
    PROCESSED_FILE.write_text("\n".join(sorted(processed)) + "\n")


def extract_text(content):
    """Extract readable text from message content blocks."""
    if isinstance(content, str):
        return content.strip()
    if isinstance(content, list):
        texts = []
        for block in content:
            if isinstance(block, dict) and block.get("type") == "text":
                text = block.get("text", "").strip()
                if text:
                    texts.append(text)
        return "\n\n".join(texts)
    return ""


def extract_tools_used(content):
    """Extract tool names from assistant content blocks."""
    if not isinstance(content, list):
        return []
    tools = []
    agent_lower = AGENT_NAME.lower()
    for block in content:
        if isinstance(block, dict) and block.get("type") == "tool_use":
            name = block.get("name", "")
            inp = block.get("input", {})
            if name in ("Write", "Edit"):
                path = inp.get("file_path", "")
                if f"/{agent_lower}/" in path:
                    path = path[path.index(f"/{agent_lower}/") + len(agent_lower) + 2:]
                tools.append(f"{name}({path})")
            elif name == "Read":
                path = inp.get("file_path", "")
                if f"/{agent_lower}/" in path:
                    path = path[path.index(f"/{agent_lower}/") + len(agent_lower) + 2:]
                tools.append(f"Read({path})")
            elif name == "Bash":
                cmd = inp.get("command", "")[:80]
                tools.append(f"Bash({cmd})")
            elif name == "Task":
                desc = inp.get("description", "")[:50]
                tools.append(f"Task({desc})")
            else:
                tools.append(name)
    return tools


def clean_user_text(text):
    """Remove system-reminder tags and other noise from user messages."""
    text = re.sub(r"<system-reminder>.*?</system-reminder>", "", text, flags=re.DOTALL)
    text = re.sub(r"<task-notification>.*?</task-notification>", "", text, flags=re.DOTALL)
    text = re.sub(r"\n{3,}", "\n\n", text)
    return text.strip()


def format_timestamp(ts):
    """Format ISO timestamp to readable time."""
    if not ts:
        return ""
    try:
        dt = datetime.fromisoformat(ts.replace("Z", "+00:00"))
        return dt.strftime("%H:%M")
    except (ValueError, AttributeError):
        return ""


def _parse_user_content_text(content):
    """Extract raw text from user message content (str or list of blocks)."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for block in content:
            if isinstance(block, dict) and block.get("type") == "text":
                parts.append(block.get("text", ""))
            elif isinstance(block, str):
                parts.append(block)
        return "\n".join(parts)
    return ""


def _process_user_msg(data, timestamp):
    """Process a user message record into an exchange entry, or None if empty."""
    message = data.get("message", {})
    content = message.get("content", "")
    text = clean_user_text(_parse_user_content_text(content))
    if not text:
        return None
    return {
        "role": "user",
        "text": text,
        "time": format_timestamp(timestamp),
        "tools": [],
    }


def _process_assistant_msg(data, timestamp):
    """Process an assistant message record into an exchange entry, or None if empty."""
    message = data.get("message", {})
    content = message.get("content", [])
    text = extract_text(content)
    tools = extract_tools_used(content)
    if not text and not tools:
        return None
    return {
        "role": "assistant",
        "text": text,
        "time": format_timestamp(timestamp),
        "tools": tools,
    }


def _parse_date_from_ts(first_ts):
    """Extract YYYY-MM-DD date string from an ISO timestamp."""
    if not first_ts:
        return "unknown"
    try:
        dt = datetime.fromisoformat(first_ts.replace("Z", "+00:00"))
        return dt.strftime("%Y-%m-%d")
    except (ValueError, AttributeError):
        return "unknown"


def parse_jsonl(jsonl_path):
    """Parse a .jsonl file into structured exchanges."""
    exchanges = []
    session_id = jsonl_path.stem
    first_ts = None

    with open(jsonl_path, "r") as f:
        for line in f:
            try:
                data = json.loads(line)
            except json.JSONDecodeError:
                continue

            msg_type = data.get("type")
            timestamp = data.get("timestamp", "")

            if msg_type == "user":
                entry = _process_user_msg(data, timestamp)
                if entry:
                    if not first_ts:
                        first_ts = timestamp
                    exchanges.append(entry)
            elif msg_type == "assistant":
                entry = _process_assistant_msg(data, timestamp)
                if entry:
                    if not first_ts:
                        first_ts = timestamp
                    exchanges.append(entry)

    return session_id, _parse_date_from_ts(first_ts), exchanges


def format_session_markdown(session_id, exchanges):
    """Format a single session's exchanges into readable markdown."""
    if not exchanges:
        return ""

    lines = []
    first_time = next((e["time"] for e in exchanges if e["time"]), "")
    lines.append(f"### Session {session_id[:8]}... — {first_time}")
    lines.append("")

    turn = 0
    i = 0
    while i < len(exchanges):
        ex = exchanges[i]

        if ex["role"] == "user":
            turn += 1
            time_tag = f" `{ex['time']}`" if ex["time"] else ""
            lines.append(f"**{turn}. User{time_tag}:**")
            lines.append(f"> {ex['text']}")
            lines.append("")

            assistant_text_parts = []
            all_tools = []
            while i + 1 < len(exchanges) and exchanges[i + 1]["role"] == "assistant":
                i += 1
                if exchanges[i]["text"]:
                    assistant_text_parts.append(exchanges[i]["text"])
                all_tools.extend(exchanges[i]["tools"])

            if assistant_text_parts:
                full_text = "\n\n".join(assistant_text_parts)
                lines.append("**Assistant:**")
                lines.append(full_text)
                lines.append("")

            if all_tools:
                unique_tools = list(dict.fromkeys(all_tools))
                if len(unique_tools) > 10:
                    summary = ", ".join(unique_tools[:10]) + f" (+{len(unique_tools) - 10} more)"
                else:
                    summary = ", ".join(unique_tools)
                lines.append(f"*Tools: {summary}*")
                lines.append("")

            i += 1
        else:
            i += 1

    return "\n".join(lines)


def format_daily_file(date, sessions):
    """Format all sessions for a day into one markdown file."""
    lines = [f"# Transcript — {date}", ""]

    for session_id, exchanges in sessions:
        session_md = format_session_markdown(session_id, exchanges)
        if session_md:
            lines.append(session_md)
            lines.append("---")
            lines.append("")

    return "\n".join(lines)


def _parse_args():
    """Parse command-line arguments."""
    parser = argparse.ArgumentParser(description="Parse .jsonl transcripts into daily markdown")
    parser.add_argument("--file", type=str, help="Process a specific .jsonl file")
    parser.add_argument("--dry-run", action="store_true", help="Preview without writing")
    parser.add_argument("--force", action="store_true", help="Re-process all files")
    return parser.parse_args()


def _discover_files(args):
    """Return sorted list of .jsonl files to process, or None if none found."""
    if args.file:
        return [Path(args.file)]
    if not TRANSCRIPTS_DIR.exists():
        print("No transcripts directory found.")
        return None
    files = sorted(TRANSCRIPTS_DIR.glob("*.jsonl"))
    if not files:
        print("No .jsonl files found.")
        return None
    return files


def _collect_sessions(jsonl_files, processed, args):
    """Process jsonl files and group exchanges by date. Returns (daily_sessions, newly_processed)."""
    daily_sessions = defaultdict(list)
    newly_processed = []
    for jsonl_path in jsonl_files:
        fname = jsonl_path.name
        if not args.force and fname in processed:
            continue
        session_id, date, exchanges = parse_jsonl(jsonl_path)
        if exchanges:
            daily_sessions[date].append((session_id, exchanges))
            newly_processed.append(fname)
            print(f"  Parsed: {fname} -> {date} ({len(exchanges)} messages)")
        else:
            newly_processed.append(fname)
            print(f"  Skipped (empty): {fname}")
    return daily_sessions, newly_processed


def _write_date_file(output_path, sessions, content):
    """Write sessions for a date to file, merging with existing content."""
    if output_path.exists():
        existing = output_path.read_text()
        new_sessions = [(sid, exch) for sid, exch in sessions if sid[:8] not in existing]
        if new_sessions:
            additions = []
            for sid, exch in new_sessions:
                md = format_session_markdown(sid, exch)
                if md:
                    additions.append(md + "\n\n---\n")
            output_path.write_text(existing.rstrip() + "\n\n" + "\n".join(additions) + "\n")
            print(f"  Updated: {output_path.name} (+{len(new_sessions)} session(s))")
        else:
            print(f"  Unchanged: {output_path.name} (sessions already present)")
    else:
        output_path.write_text(content)
        print(f"  Created: {output_path.name} ({len(sessions)} session(s))")


def _write_date_output(date, sessions, args):
    """Write or display output for a single day's sessions."""
    content = format_daily_file(date, sessions)
    output_path = TRANSCRIPTS_DIR / f"{date}.md"
    if args.dry_run:
        print(f"\n--- {output_path.name} ({len(sessions)} session(s)) ---")
        for line in content.split("\n")[:100]:
            print(line)
        if content.count("\n") > 100:
            print(f"... ({content.count(chr(10)) - 100} more lines)")
        return
    _write_date_file(output_path, sessions, content)


def main():
    args = _parse_args()
    jsonl_files = _discover_files(args)
    if jsonl_files is None:
        return
    processed = load_processed() if not args.force else set()
    daily_sessions, newly_processed = _collect_sessions(jsonl_files, processed, args)
    if not daily_sessions:
        print("No new transcripts to process.")
        return
    for date in sorted(daily_sessions.keys()):
        _write_date_output(date, daily_sessions[date], args)
    if not args.dry_run:
        processed.update(newly_processed)
        save_processed(processed)
        print(f"\nDone. Processed {len(newly_processed)} file(s) across {len(daily_sessions)} day(s).")


if __name__ == "__main__":
    main()
