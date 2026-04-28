"""Shared Jinja2 template rendering helper for action plugins."""
from jinja2 import Template


def render_templates(params: dict, event_payload: dict, workflow_context: dict = None) -> dict:
    ctx = {**event_payload, **(workflow_context or {})}
    result = {}
    for k, v in params.items():
        if isinstance(v, str) and '{{' in v:
            result[k] = Template(v).render(**ctx)
        elif isinstance(v, dict):
            result[k] = {dk: Template(dv).render(**ctx) if isinstance(dv, str) and '{{' in dv else dv for dk, dv in v.items()}
        else:
            result[k] = v
    return result
