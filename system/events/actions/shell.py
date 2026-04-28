"""Shell command action plugin."""
import subprocess
from actions import register
from actions.render import render_templates

@register("shell")
class ShellAction:
    def run(self, params: dict, event_payload: dict, db=None,
            workflow_context=None) -> dict:
        command = params["command"]
        timeout = int(params.get("timeout", 60))
        tpl_ctx = {"event": event_payload}
        if workflow_context:
            tpl_ctx["workflow"] = workflow_context
        try:
            command = render_templates({"command": command}, tpl_ctx)["command"]
        except Exception as e:
            return {"status": "error", "output": f"Template render failed: {e}",
                    "_action_result": {"stderr": str(e), "returncode": -1}}
        try:
            result = subprocess.run(
                command, shell=True, executable="/bin/bash",
                capture_output=True, text=True, timeout=timeout,
            )
            if result.returncode == 0:
                action_result = {"stdout": result.stdout.strip(), "returncode": 0}
                return {"status": "success", "output": result.stdout.strip(),
                        "_action_result": action_result}
            else:
                error_output = result.stderr.strip() or result.stdout.strip() or f"exit code {result.returncode}"
                action_result = {"stderr": error_output,
                                 "returncode": result.returncode}
                return {"status": "error", "output": error_output,
                        "code": result.returncode,
                        "_action_result": action_result}
        except subprocess.TimeoutExpired:
            return {"status": "error", "output": f"timeout after {timeout}s"}
