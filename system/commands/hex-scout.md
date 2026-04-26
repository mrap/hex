# /hex-scout — Proactive Tech Research Scout

Run the tech scout to find relevant developments across Mike's active projects.

## Usage

`/hex-scout [run|dry-run|status]`

## Commands

### `/hex-scout run`
Execute a full research cycle:
1. Parse `~/hex/todo.md` for active projects
2. Generate 5-10 research queries
3. Search HackerNews, GitHub, and X/Twitter
4. Filter for relevance (keyword + project match + quality signals)
5. Write top 3-5 findings to `~/hex/raw/research/scout/YYYY-MM-DD.md`
6. Update `~/hex/raw/research/scout/index.md`

Run this in a terminal (not inside Claude):
```bash
bash ~/hex/.hex/scripts/tech-scout.sh
```

### `/hex-scout dry-run`
Show what would be searched without executing:
```bash
bash ~/hex/.hex/scripts/tech-scout.sh --dry-run
```

### `/hex-scout status`
Check recent scout output:
```bash
ls -lt ~/hex/raw/research/scout/ | head -10
cat ~/hex/raw/research/scout/index.md 2>/dev/null || echo "No briefs yet"
```

## Notes
- The scout reads from `~/hex/.hex/secrets/x-api.env` for X/Twitter bearer token
- URL deduplication tracked at `~/.boi/state/scout-seen-urls.jsonl`
- Runs without any API credentials (HN + GitHub are free/public)
- X/Twitter search activates automatically if `TWITTER_BEARER_TOKEN` is set in secrets
