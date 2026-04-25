#!/usr/bin/env python3
"""E2E verification guard — standard BOI phase for web projects."""
import argparse, json, os, sys, time, urllib.request, urllib.error
from datetime import datetime, timezone

def parse_args():
    p = argparse.ArgumentParser(description="E2E guard for web projects")
    p.add_argument("--url", required=True)
    p.add_argument("--selectors", default="")
    p.add_argument("--check-sse", default="")
    p.add_argument("--check-api", default="")
    p.add_argument("--viewports", default="375,768,1440")
    p.add_argument("--timeout", type=int, default=30)
    p.add_argument("--output", default="")
    p.add_argument("--screenshot-on-fail", default="/tmp/e2e-guard-failures/")
    return p.parse_args()

def url_join(base, path):
    if not path:
        return base
    if path.startswith("/"):
        from urllib.parse import urlparse
        p = urlparse(base)
        return f"{p.scheme}://{p.netloc}{path}"
    return base.rstrip("/") + "/" + path

def fetch_url(url, timeout=10):
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "e2e-guard/1.0"})
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read().decode("utf-8", errors="replace"), None
    except urllib.error.HTTPError as e:
        return e.code, "", str(e)
    except Exception as e:
        return 0, "", str(e)

def run_test(name, fn):
    t0 = time.time()
    try:
        ok, detail, screenshot = fn()
        return {"name": name, "status": "PASS" if ok else "FAIL",
                "duration_ms": int((time.time()-t0)*1000),
                **({"detail": detail} if detail else {}),
                **({"screenshot": screenshot} if screenshot else {})}
    except Exception as e:
        return {"name": name, "status": "FAIL", "duration_ms": int((time.time()-t0)*1000),
                "detail": str(e)}

def save_screenshot(page, name, fail_dir):
    os.makedirs(fail_dir, exist_ok=True)
    path = os.path.join(fail_dir, f"{name}-{int(time.time())}.png")
    try:
        page.screenshot(path=path)
        return path
    except Exception:
        return None

def test_page_loads(page, url, timeout, fail_dir):
    page.goto(url, timeout=timeout*1000, wait_until="domcontentloaded")
    title = page.title()
    errors = []
    page.on("console", lambda m: errors.append(m.text) if m.type == "error" else None)
    if not title:
        return False, "No page title", save_screenshot(page, "page_loads", fail_dir)
    return True, None, None

def test_core_content(page, selectors, fail_dir):
    if not selectors:
        return True, None, None
    for sel in selectors:
        sel = sel.strip()
        if not sel:
            continue
        el = page.query_selector(sel)
        if not el or not el.is_visible():
            return False, f"{sel} not found or not visible", save_screenshot(page, "core_content", fail_dir)
    return True, None, None

def test_responsive(page, url, viewports, timeout, fail_dir):
    for w in viewports:
        page.set_viewport_size({"width": w, "height": 800})
        page.goto(url, timeout=timeout*1000, wait_until="domcontentloaded")
        overflow = page.evaluate("() => document.documentElement.scrollWidth > window.innerWidth")
        if overflow:
            return False, f"Horizontal overflow at {w}px", save_screenshot(page, f"responsive_{w}", fail_dir)
    return True, None, None

def test_no_crashes(page, url, timeout, fail_dir):
    js_errors = []
    page.on("pageerror", lambda e: js_errors.append(str(e)))
    page.goto(url, timeout=timeout*1000, wait_until="domcontentloaded")
    page.wait_for_timeout(5000)
    if js_errors:
        return False, "; ".join(js_errors[:3]), save_screenshot(page, "no_crashes", fail_dir)
    return True, None, None

def test_sse(base_url, sse_path, timeout):
    if not sse_path:
        return True, None, None
    url = url_join(base_url, sse_path)
    try:
        req = urllib.request.Request(url, headers={"Accept": "text/event-stream", "User-Agent": "e2e-guard/1.0"})
        with urllib.request.urlopen(req, timeout=timeout) as r:
            deadline = time.time() + timeout
            while time.time() < deadline:
                line = r.readline().decode("utf-8", errors="replace")
                if line.startswith("data:"):
                    return True, None, None
        return False, "No SSE events received within timeout", None
    except Exception as e:
        return False, str(e), None

def test_api_health(base_url, api_path, timeout):
    if not api_path:
        return True, None, None
    url = url_join(base_url, api_path)
    status, body, err = fetch_url(url, timeout)
    if err:
        return False, err, None
    if status not in (200, 201, 202):
        return False, f"HTTP {status}", None
    try:
        json.loads(body)
        return True, None, None
    except Exception:
        return False, "Response is not valid JSON", None

def test_accessibility(page, url, fail_dir):
    page.goto(url, wait_until="domcontentloaded")
    imgs_no_alt = page.evaluate("""() => {
        return Array.from(document.querySelectorAll('img')).filter(i => !i.alt && !i.getAttribute('aria-label')).length
    }""")
    if imgs_no_alt > 0:
        return False, f"{imgs_no_alt} image(s) missing alt text", save_screenshot(page, "accessibility", fail_dir)
    return True, None, None

def main():
    args = parse_args()
    selectors = [s for s in args.selectors.split(",") if s.strip()] if args.selectors else []
    viewports = [int(v) for v in args.viewports.split(",")]

    results = []

    # Non-browser tests first
    results.append(run_test("live_data", lambda: test_sse(args.url, args.check_sse, args.timeout)))
    results.append(run_test("api_health", lambda: test_api_health(args.url, args.check_api, args.timeout)))

    # Browser tests via Playwright
    try:
        from playwright.sync_api import sync_playwright
        with sync_playwright() as pw:
            browser = pw.chromium.launch(headless=True)
            page = browser.new_page()
            page.set_viewport_size({"width": 1440, "height": 900})

            results.insert(0, run_test("page_loads",
                lambda: test_page_loads(page, args.url, args.timeout, args.screenshot_on_fail)))
            results.insert(1, run_test("core_content",
                lambda: test_core_content(page, selectors, args.screenshot_on_fail)))
            results.insert(2, run_test("responsive",
                lambda: test_responsive(page, args.url, viewports, args.timeout, args.screenshot_on_fail)))
            results.insert(3, run_test("no_crashes",
                lambda: test_no_crashes(page, args.url, args.timeout, args.screenshot_on_fail)))
            results.insert(4, run_test("accessibility",
                lambda: test_accessibility(page, args.url, args.screenshot_on_fail)))

            browser.close()
    except ImportError:
        for name in ["page_loads", "core_content", "responsive", "no_crashes", "accessibility"]:
            results.insert(0, {"name": name, "status": "SKIP", "detail": "playwright not installed"})

    passed = sum(1 for r in results if r["status"] == "PASS")
    failed = sum(1 for r in results if r["status"] == "FAIL")
    verdict = "PASS" if failed == 0 else "FAIL"

    report = {
        "url": args.url,
        "ts": datetime.now(timezone.utc).isoformat(),
        "tests": results,
        "passed": passed,
        "failed": failed,
        "verdict": verdict,
    }

    print(json.dumps(report, indent=2))

    if args.output:
        os.makedirs(os.path.dirname(args.output) if os.path.dirname(args.output) else ".", exist_ok=True)
        with open(args.output + ".tmp", "w") as f:
            json.dump(report, f, indent=2)
        os.replace(args.output + ".tmp", args.output)

    sys.exit(0 if verdict == "PASS" else 1)

if __name__ == "__main__":
    main()
