"""Trigger Dagu workflow action."""
import subprocess
from actions import register

@register("dagu")
class DaguAction:
    def run(self, params: dict, event_payload: dict, db=None,
            workflow_context=None) -> dict:
        workflow = params["workflow"]
        try:
            result = subprocess.run(
                ["dagu", "start", workflow],
                capture_output=True, text=True, timeout=30,
            )
            return {"status": "success" if result.returncode == 0 else "error", "output": result.stdout.strip()}
        except Exception as e:
            return {"status": "error", "output": str(e)}
