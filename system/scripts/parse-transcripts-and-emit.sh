#!/usr/bin/env bash
# parse-transcripts-and-emit.sh — run parse_transcripts.py and emit hex.session.parsed for each new file
# Called by startup.sh and any other caller that previously invoked parse_transcripts.py directly.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HEX_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
TRANSCRIPTS_DIR="$HEX_ROOT/raw/transcripts"
HEX_EMIT="python3 $HOME/github.com/mrap/hex-events/hex_emit.py"

PARSE_OUT=$(python3 "$SCRIPT_DIR/parse_transcripts.py" "$@" 2>&1)
PARSE_EXIT=$?

echo "$PARSE_OUT"

if [[ $PARSE_EXIT -eq 0 ]]; then
  count=0
  while IFS= read -r f; do
    date_part=$(basename "$f" .md)
    path_escaped=$(echo "$f" | sed 's/"/\\"/g')
    payload="{\"date\":\"$date_part\",\"path\":\"$path_escaped\",\"source\":\"parse-transcripts\"}"
    $HEX_EMIT hex.session.parsed "$payload" hex:parse-transcripts
    count=$((count + 1))
  done < <(find "$TRANSCRIPTS_DIR" -maxdepth 1 -type f -name '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9].md' -mmin -10 2>/dev/null)

  if [[ $count -gt 0 ]]; then
    echo "[parse-transcripts-emit] emitted $count hex.session.parsed events"
  fi
fi

exit $PARSE_EXIT
