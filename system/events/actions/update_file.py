"""Update file action (regex find/replace)."""
import os
import re
import tempfile
from actions import register
from actions.render import render_templates

@register("update-file")
class UpdateFileAction:
    def run(self, params: dict, event_payload: dict, db=None,
            workflow_context=None) -> dict:
        target = params["target"]
        pattern = params["pattern"]
        replace = params["replace"]
        tpl_ctx = {"event": event_payload}
        if workflow_context:
            tpl_ctx["workflow"] = workflow_context
        rendered = render_templates({"target": target, "pattern": pattern, "replace": replace}, tpl_ctx)
        target, pattern, replace = rendered["target"], rendered["pattern"], rendered["replace"]
        tmp_path = None
        try:
            with open(target) as f:
                content = f.read()
            new_content = re.sub(pattern, replace, content)
            target_dir = os.path.dirname(os.path.abspath(target))
            with tempfile.NamedTemporaryFile(mode="w", dir=target_dir, delete=False) as tmp:
                tmp_path = tmp.name
                tmp.write(new_content)
            os.replace(tmp_path, target)
            changed = content != new_content
            return {"status": "success", "changed": changed}
        except Exception as e:
            if tmp_path and os.path.exists(tmp_path):
                os.unlink(tmp_path)
            return {"status": "error", "output": str(e)}
