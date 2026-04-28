"""
conftest.py — hex-events pytest configuration.

Provides a terminal summary for the stress test suite: total pass/fail counts,
events/sec throughput (from test_rapid_fire), daemon recovery time
(from test_daemon_recovery), and any accumulated warnings.
"""
from __future__ import annotations


def pytest_terminal_summary(terminalreporter, exitstatus, config):
    """Print a stress-test summary after the full test run."""
    # Only emit the summary when test_stress tests were collected/run.
    stress_tests = [
        r
        for r in terminalreporter.stats.get("passed", [])
        + terminalreporter.stats.get("failed", [])
        + terminalreporter.stats.get("error", [])
        if "test_stress" in (r.nodeid or "")
    ]
    if not stress_tests:
        return

    passed = len(terminalreporter.stats.get("passed", []))
    failed = len(terminalreporter.stats.get("failed", []))

    # Try to read metrics that the stress tests populated
    try:
        import sys
        import os
        base_dir = os.path.expanduser("~/.hex-events")
        if base_dir not in sys.path:
            sys.path.insert(0, base_dir)
        import test_stress as _ts
        report = _ts.STRESS_REPORT
    except Exception:
        report = {}

    throughput = report.get("throughput_events_per_sec")
    recovery_time = report.get("recovery_time_sec")
    warnings_list = report.get("warnings") or []

    tw = terminalreporter._tw
    tw.sep("=", "stress test summary", bold=True)
    tw.line(f"  Total: {passed} passed, {failed} failed")
    if throughput is not None:
        tw.line(f"  Throughput (t-2): {throughput:.1f} events/sec")
    else:
        tw.line("  Throughput (t-2): n/a (test not run)")
    if recovery_time is not None:
        tw.line(f"  Daemon recovery (t-6): {recovery_time:.1f}s")
    else:
        tw.line("  Daemon recovery (t-6): n/a (test not run)")
    if warnings_list:
        tw.line(f"  Warnings ({len(warnings_list)}):")
        for w in warnings_list:
            tw.line(f"    - {w}")
    else:
        tw.line("  Warnings: none")
    tw.sep("=", "", bold=True)
