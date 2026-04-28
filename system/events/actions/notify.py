"""Notification action (delegates to hex-notify.sh)."""
import os
import subprocess
from actions import register
from actions.render import render_templates

@register("notify")
class NotifyAction:
    def run(self, params: dict, event_payload: dict, db=None,
            workflow_context=None) -> dict:
        message = params["message"]
        tpl_ctx = {"event": event_payload}
        if workflow_context:
            tpl_ctx["workflow"] = workflow_context
        message = render_templates({"message": message}, tpl_ctx)["message"]
        try:
            result = subprocess.run(
                ["bash", os.path.expanduser("~/.claude/scripts/hex-notify.sh"), message],
                capture_output=True, text=True, timeout=30,
            )
            if result.returncode == 0:
                return {"status": "success", "output": result.stdout.strip()}
            error_out = result.stderr.strip() or result.stdout.strip() or f"exit code {result.returncode}"
            return {"status": "error", "output": error_out}
        except Exception as e:
            return {"status": "error", "output": str(e)}
