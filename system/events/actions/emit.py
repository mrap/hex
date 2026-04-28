"""Emit event action (recipe chaining). Supports delayed emit + cancel_group."""
import json
from datetime import datetime, timedelta

from actions import register
from actions.render import render_templates


@register("emit")
class EmitAction:
    def run(self, params: dict, event_payload: dict, db=None,
            workflow_context=None) -> dict:
        if not params.get("event"):
            return {"status": "error", "output": "emit action missing required 'event' parameter"}

        event_type = params["event"]
        payload = params.get("payload", {})
        tpl_ctx = {"event": event_payload}
        if workflow_context:
            tpl_ctx["workflow"] = workflow_context
        if isinstance(payload, str) and "{{" in payload:
            try:
                from jinja2 import Template
                payload = json.loads(Template(payload).render(**tpl_ctx))
            except Exception as e:
                return {"status": "error", "output": f"Template render failed: {e}"}
        elif isinstance(payload, dict):
            payload = render_templates(payload, tpl_ctx)
        delay = params.get("delay")
        cancel_group = params.get("cancel_group")
        if isinstance(cancel_group, str) and "{{" in cancel_group:
            cancel_group = render_templates({"cancel_group": cancel_group}, tpl_ctx)["cancel_group"]

        if delay is not None:
            from db import parse_duration
            seconds = parse_duration(delay)
            if seconds > 0:
                # Deferred emit: write to deferred_events table
                if db is None:
                    return {"status": "error", "output": "Cannot defer event: no database connection"}
                fire_at = (datetime.utcnow() + timedelta(seconds=seconds)).isoformat()
                source = params.get("source", "policy-emit")
                db.insert_deferred(
                    event_type,
                    json.dumps(payload),
                    source,
                    fire_at,
                    cancel_group=cancel_group,
                )
                return {"status": "success", "emitted": event_type, "deferred": True,
                        "delay": delay}
            # delay == 0: fall through to immediate emit

        # Immediate emit
        if db:
            source = params.get("source", "policy-emit")
            db.insert_event(event_type, json.dumps(payload), source)
        return {"status": "success", "emitted": event_type}
