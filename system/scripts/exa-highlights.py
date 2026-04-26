#!/usr/bin/env python3
"""Exa Highlights API — token-efficient web content extraction.

Usage:
  exa-highlights.py <url> [<url>...] [--query "relevant topic"]
  exa-highlights.py --search "search query" [--num 5] [--query "highlight focus"]

Modes:
  URL mode:   Fetch highlights from specific URLs
  Search mode: Search + highlights in one call (search_and_contents)

Outputs JSON with highlights array per result.
"""

import argparse
import json
import os
import sys
import urllib.request

SECRETS_PATH = os.path.expanduser("~/hex/.hex/secrets/mcp-exa.env")

def load_api_key():
    key = os.environ.get("EXA_API_KEY")
    if key:
        return key
    if os.path.exists(SECRETS_PATH):
        with open(SECRETS_PATH) as f:
            for line in f:
                line = line.strip()
                if line.startswith("EXA_API_KEY="):
                    return line.split("=", 1)[1].strip().strip('"').strip("'")
    print("Error: EXA_API_KEY not found", file=sys.stderr)
    sys.exit(1)

def exa_request(endpoint, payload, api_key):
    data = json.dumps(payload).encode()
    req = urllib.request.Request(
        f"https://api.exa.ai/{endpoint}",
        data=data,
        headers={
            "Content-Type": "application/json",
            "x-api-key": api_key,
            "User-Agent": "hex-agent/1.0",
        },
    )
    with urllib.request.urlopen(req) as resp:
        return json.loads(resp.read())

def fetch_highlights(urls, query, api_key, max_chars=3000):
    highlights_opts = {"maxCharacters": max_chars}
    if query:
        highlights_opts["query"] = query
    return exa_request("contents", {
        "urls": urls,
        "highlights": highlights_opts,
    }, api_key)

def search_highlights(search_query, highlight_query, api_key, num=5, max_chars=3000):
    highlights_opts = {"maxCharacters": max_chars}
    if highlight_query:
        highlights_opts["query"] = highlight_query
    return exa_request("search", {
        "query": search_query,
        "numResults": num,
        "contents": {
            "highlights": highlights_opts,
        },
    }, api_key)

def print_results(data, compact=False):
    if compact:
        for r in data.get("results", []):
            title = r.get("title", "")
            url = r.get("url", "")
            highlights = r.get("highlights", [])
            print(f"\n## {title}")
            print(f"URL: {url}")
            for h in highlights:
                print(f"  > {h}")
    else:
        json.dump(data, sys.stdout, indent=2)
        print()

def main():
    parser = argparse.ArgumentParser(description="Exa Highlights — token-efficient web extraction")
    parser.add_argument("urls", nargs="*", help="URLs to extract highlights from")
    parser.add_argument("--search", help="Search query (search+highlights mode)")
    parser.add_argument("--query", help="Highlight focus query (what to extract)")
    parser.add_argument("--num", type=int, default=5, help="Number of search results (default: 5)")
    parser.add_argument("--max-chars", type=int, default=3000, help="Max chars per highlight (default: 3000)")
    parser.add_argument("--compact", action="store_true", help="Human-readable output instead of JSON")
    args = parser.parse_args()

    if not args.urls and not args.search:
        parser.print_help()
        sys.exit(1)

    api_key = load_api_key()

    if args.search:
        data = search_highlights(args.search, args.query, api_key, args.num, args.max_chars)
    else:
        data = fetch_highlights(args.urls, args.query, api_key, args.max_chars)

    print_results(data, args.compact)

if __name__ == "__main__":
    main()
