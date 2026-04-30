#!/usr/bin/env python3
"""
migrate-agent-channels.py — Migrate #hex-{agent} channels to #agent-{id} naming.

For each agent channel in agent-channels.yaml:
  1. Create new #agent-{agent_id} Slack channel (skip if exists)
  2. Post a migration notice in the old #hex-{name} channel
  3. Archive the old #hex-{name} channel
  4. Update agent-channels.yaml with the new channel name key

System channels (#hex-main, #hex-vitals, #hex-announcements, #hex-taxes, etc.)
that have no agent binding are left untouched.

Usage:
  python3 migrate-agent-channels.py [--dry-run]
"""

import json
import os
import sys
import urllib.request
import urllib.error

SECRETS_FILE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "../secrets/slack-bot.env")
AGENT_CHANNELS_YAML = os.path.expanduser("~/.cc-connect/agent-channels.yaml")
HEX_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "../..")

DRY_RUN = "--dry-run" in sys.argv


def load_bot_token():
    with open(SECRETS_FILE) as f:
        for line in f:
            line = line.strip()
            if line.startswith("HEX_SLACK_BOT_TOKEN="):
                return line.split("=", 1)[1].strip()
    raise RuntimeError("HEX_SLACK_BOT_TOKEN not found in slack-bot.env")


def slack_post(token, endpoint, payload):
    data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        f"https://slack.com/api/{endpoint}",
        data=data,
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        },
    )
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.loads(resp.read())


def slack_get(token, endpoint, params=""):
    url = f"https://slack.com/api/{endpoint}"
    if params:
        url += "?" + params
    req = urllib.request.Request(url, headers={"Authorization": f"Bearer {token}"})
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.loads(resp.read())


def list_all_channels(token):
    """Return dict of channel_name -> {id, is_archived} for all channels."""
    channels = {}
    cursor = ""
    while True:
        params = "types=public_channel&limit=200&exclude_archived=false"
        if cursor:
            params += f"&cursor={cursor}"
        result = slack_get(token, "conversations.list", params)
        if not result.get("ok"):
            print(f"  [warn] conversations.list error: {result.get('error')}", file=sys.stderr)
            break
        for ch in result.get("channels", []):
            channels[ch["name"]] = {
                "id": ch["id"],
                "is_archived": ch.get("is_archived", False),
            }
        cursor = result.get("response_metadata", {}).get("next_cursor", "")
        if not cursor:
            break
    return channels


def read_charter_name(agent_id):
    charter_path = os.path.join(HEX_DIR, "projects", agent_id, "charter.yaml")
    if not os.path.exists(charter_path):
        return agent_id
    try:
        import re
        with open(charter_path) as f:
            content = f.read()
        m = re.search(r"^name:\s*(.+)$", content, re.MULTILINE)
        if m:
            return m.group(1).strip().strip("\"'")
    except Exception:
        pass
    return agent_id


def read_charter_role(agent_id):
    charter_path = os.path.join(HEX_DIR, "projects", agent_id, "charter.yaml")
    if not os.path.exists(charter_path):
        return f"{agent_id} agent"
    try:
        import re
        with open(charter_path) as f:
            content = f.read()
        m = re.search(r"^role:\s*(.+)$", content, re.MULTILINE)
        if m:
            return m.group(1).strip().strip("\"'")
    except Exception:
        pass
    return f"{agent_id} agent"


def load_yaml_raw(path):
    """Load YAML file as raw text (to preserve formatting on re-write)."""
    with open(path) as f:
        return f.read()


def load_bindings(path):
    """Parse agent-channels.yaml into a simple dict without yaml library dependency."""
    import re
    bindings = {}
    current_channel = None
    current_binding = {}

    with open(path) as f:
        lines = f.readlines()

    # Simple state machine to parse the yaml structure
    in_channels = False
    for line in lines:
        stripped = line.rstrip()
        indent = len(line) - len(line.lstrip())

        if stripped.strip() == "channels:":
            in_channels = True
            continue

        if not in_channels:
            continue

        # Top-level channel key (2 spaces indent)
        if indent == 2 and stripped.endswith(":") and not stripped.strip().startswith("#"):
            if current_channel and current_binding:
                bindings[current_channel] = current_binding
            current_channel = stripped.strip().rstrip(":")
            current_binding = {}
        elif indent == 4 and current_channel and ":" in stripped:
            key, _, val = stripped.strip().partition(":")
            val = val.strip().strip("\"'")
            if val and not val.startswith("#"):
                current_binding[key.strip()] = val

    if current_channel and current_binding:
        bindings[current_channel] = current_binding

    return bindings


def rekey_yaml(content, old_key, new_key):
    """Replace a top-level channel key in the yaml text."""
    # Replace "  old_key:" with "  new_key:" at 2-space indent
    import re
    pattern = re.compile(r"^(  )" + re.escape(old_key) + r"(:)", re.MULTILINE)
    return pattern.sub(r"\g<1>" + new_key + r"\g<2>", content)


def create_channel(token, name):
    """Create a public Slack channel. Returns channel_id or None."""
    result = slack_post(token, "conversations.create", {
        "name": name,
        "is_private": False,
    })
    if result.get("ok"):
        return result["channel"]["id"]
    err = result.get("error", "unknown")
    if err == "name_taken":
        return "name_taken"
    print(f"    [error] conversations.create({name}): {err}", file=sys.stderr)
    return None


def archive_channel(token, channel_id, channel_name):
    result = slack_post(token, "conversations.archive", {"channel": channel_id})
    if not result.get("ok"):
        err = result.get("error", "")
        if err == "already_archived":
            return True
        print(f"    [warn] archive({channel_name}): {err}", file=sys.stderr)
        return False
    return True


def main():
    if DRY_RUN:
        print("=== DRY RUN — no Slack API calls, no file writes ===\n")
        token = None
    else:
        token = load_bot_token()

    # Load bindings from yaml
    bindings = load_bindings(AGENT_CHANNELS_YAML)
    yaml_content = load_yaml_raw(AGENT_CHANNELS_YAML)
    print(f"Loaded {len(bindings)} agent channel bindings")

    if not DRY_RUN:
        print("Fetching existing Slack channels...")
        existing = list_all_channels(token)
        print(f"Found {len(existing)} channels (including archived)\n")
    else:
        existing = {}

    results = {
        "created": [],
        "already_existed": [],
        "archived": [],
        "skipped_system": [],
        "errors": [],
    }

    updated_yaml = yaml_content

    for old_channel, binding in bindings.items():
        agent_id = binding.get("agent_id", "")
        if not agent_id:
            print(f"  [skip] {old_channel}: no agent_id in binding")
            continue

        new_channel = f"agent-{agent_id}"
        agent_name = read_charter_name(agent_id)
        agent_role = read_charter_role(agent_id)

        print(f"\n  [{old_channel}] → [{new_channel}] ({agent_name})")

        if DRY_RUN:
            print(f"    would create: #{new_channel}")
            print(f"    would post migration notice in: #{old_channel}")
            print(f"    would archive: #{old_channel}")
            print(f"    would rekey yaml: {old_channel} → {new_channel}")
            continue

        # Step 1: Create new channel
        if new_channel in existing:
            ch_info = existing[new_channel]
            if ch_info["is_archived"]:
                print(f"    [warn] #{new_channel} exists but is archived — skipping create")
                results["errors"].append((new_channel, "exists-archived"))
            else:
                new_channel_id = ch_info["id"]
                print(f"    [ok] #{new_channel} already exists ({new_channel_id})")
                results["already_existed"].append(new_channel)
        else:
            new_channel_id = create_channel(token, new_channel)
            if new_channel_id == "name_taken":
                print(f"    [warn] #{new_channel} name taken (deleted channel name reserved)")
                results["errors"].append((new_channel, "name_taken"))
                new_channel_id = None
            elif new_channel_id:
                print(f"    [created] #{new_channel} ({new_channel_id})")
                results["created"].append(new_channel)

                # Set topic and purpose
                topic = agent_role[:250]
                slack_post(token, "conversations.setTopic", {
                    "channel": new_channel_id,
                    "topic": topic,
                })
                slack_post(token, "conversations.setPurpose", {
                    "channel": new_channel_id,
                    "purpose": f"Direct line to {agent_name}. Messages routed with full charter context.",
                })
                # Post intro message
                slack_post(token, "chat.postMessage", {
                    "channel": new_channel_id,
                    "text": (
                        f":wave: *This is {agent_name}'s dedicated channel.*\n"
                        f"Messages here are routed directly to the {agent_name} agent with full charter context loaded.\n\n"
                        f"Migrated from: #{old_channel}"
                    ),
                })
            else:
                results["errors"].append((new_channel, "create_failed"))
                continue

        # Step 2: Post migration notice in OLD channel (if it exists and not archived)
        if old_channel in existing:
            old_info = existing[old_channel]
            if not old_info["is_archived"]:
                old_channel_id = old_info["id"]
                msg = (
                    f":arrow_right: *This channel has been replaced by <#{new_channel_id}|{new_channel}>.*\n"
                    f"All future messages to the {agent_name} agent should go to <#{new_channel_id}|{new_channel}>.\n\n"
                    f"This channel will be archived. History is preserved."
                )
                result = slack_post(token, "chat.postMessage", {
                    "channel": old_channel_id,
                    "text": msg,
                })
                if not result.get("ok"):
                    print(f"    [warn] postMessage to #{old_channel}: {result.get('error')}")

                # Step 3: Archive old channel
                archived = archive_channel(token, old_channel_id, old_channel)
                if archived:
                    print(f"    [archived] #{old_channel}")
                    results["archived"].append(old_channel)
            else:
                print(f"    [skip] #{old_channel} already archived")
                results["archived"].append(old_channel)
        else:
            print(f"    [skip] #{old_channel} not found in Slack (may not exist)")

        # Step 4: Rekey yaml
        updated_yaml = rekey_yaml(updated_yaml, old_channel, new_channel)

    if not DRY_RUN and updated_yaml != yaml_content:
        # Update header comment
        updated_yaml = updated_yaml.replace(
            "# Updated by: BOI worker q-810 t-4 (2026-04-25) — added channel_id fields",
            "# Updated by: BOI worker q-810 t-4 (2026-04-25) — added channel_id fields\n"
            "# Updated by: BOI worker q-810 t-6 (2026-04-25) — migrated keys to agent-{id} naming",
        )
        tmp_path = AGENT_CHANNELS_YAML + ".tmp"
        with open(tmp_path, "w") as f:
            f.write(updated_yaml)
        os.replace(tmp_path, AGENT_CHANNELS_YAML)
        print(f"\nUpdated {AGENT_CHANNELS_YAML} with new channel keys.")

    print("\n=== Migration Summary ===")
    print(f"  New channels created:  {len(results['created'])}: {results['created']}")
    print(f"  Already existed:       {len(results['already_existed'])}: {results['already_existed']}")
    print(f"  Old channels archived: {len(results['archived'])}: {results['archived']}")
    if results["errors"]:
        print(f"  Errors:                {len(results['errors'])}: {results['errors']}")
        sys.exit(1)
    else:
        print("  No errors.")


if __name__ == "__main__":
    main()
