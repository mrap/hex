#!/usr/bin/env python3
"""
parse_excalidraw.py — Convert an .excalidraw JSON file to canonical mental map markdown.

Usage:
    python3 parse_excalidraw.py [file.excalidraw]
    cat file.excalidraw | python3 parse_excalidraw.py

Output goes to stdout in the FORMAT.md canonical format.
"""

import json
import sys
import argparse
import re
import os
import datetime
from collections import defaultdict


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def slugify(text):
    """Convert text to snake_case node id (max 30 chars)."""
    text = text.lower().strip()
    text = re.sub(r'[^\w\s]', '', text)
    text = re.sub(r'\s+', '_', text)
    text = re.sub(r'_+', '_', text)
    text = text.strip('_')
    if len(text) > 30:
        text = text[:30].rstrip('_')
    return text or 'node'


def ensure_unique_id(base_id, seen_ids):
    """Append a numeric suffix to make base_id unique in seen_ids."""
    if base_id not in seen_ids:
        seen_ids.add(base_id)
        return base_id
    counter = 2
    while f"{base_id}_{counter}" in seen_ids:
        counter += 1
    new_id = f"{base_id}_{counter}"
    seen_ids.add(new_id)
    return new_id


def get_element_text(el):
    return (el.get('text') or '').strip()


def get_center(el):
    x = el.get('x', 0)
    y = el.get('y', 0)
    w = el.get('width', 0)
    h = el.get('height', 0)
    return x + w / 2, y + h / 2


def distance(el1, el2):
    cx1, cy1 = get_center(el1)
    cx2, cy2 = get_center(el2)
    return ((cx1 - cx2) ** 2 + (cy1 - cy2) ** 2) ** 0.5


# ---------------------------------------------------------------------------
# Spatial clustering (union-find)
# ---------------------------------------------------------------------------

def _uf_find(parent, i):
    while parent[i] != i:
        parent[i] = parent[parent[i]]
        i = parent[i]
    return i


def _uf_union(parent, rank, i, j):
    ri, rj = _uf_find(parent, i), _uf_find(parent, j)
    if ri == rj:
        return
    if rank[ri] < rank[rj]:
        ri, rj = rj, ri
    parent[rj] = ri
    if rank[ri] == rank[rj]:
        rank[ri] += 1


def detect_spatial_clusters(node_elements, proximity_threshold=250):
    """Group elements by spatial proximity. Returns {cluster_label: [indices]}."""
    n = len(node_elements)
    if n == 0:
        return {}
    parent = list(range(n))
    rank = [0] * n
    for i in range(n):
        for j in range(i + 1, n):
            if distance(node_elements[i], node_elements[j]) < proximity_threshold:
                _uf_union(parent, rank, i, j)
    clusters = defaultdict(list)
    for i in range(n):
        clusters[_uf_find(parent, i)].append(i)
    return {k: v for k, v in clusters.items() if len(v) > 1}


# ---------------------------------------------------------------------------
# Core parser
# ---------------------------------------------------------------------------

def parse_excalidraw(data):
    """
    Parse Excalidraw JSON. Returns a dict:
        nodes          — list of node dicts
        edges          — list of edge dicts
        named_groups   — {group_name: [node_id, ...]}
        open_questions — list of question strings
        observations   — list of observation strings
    """
    elements = data.get('elements', [])

    # build id->element map, skip deleted
    by_id = {}
    for el in elements:
        if 'id' in el and not el.get('isDeleted', False):
            by_id[el['id']] = el

    container_shapes = {}
    text_elements = {}
    arrow_elements = []

    for eid, el in by_id.items():
        t = el.get('type', '')
        if t == 'arrow':
            arrow_elements.append(el)
        elif t == 'text':
            text_elements[eid] = el
        elif t in ('rectangle', 'ellipse', 'diamond', 'freedraw'):
            container_shapes[eid] = el

    # resolve shape labels via containerId
    shape_labels = {}
    for tid, tel in text_elements.items():
        cid = tel.get('containerId')
        if cid and cid in container_shapes:
            label = get_element_text(tel)
            if label:
                shape_labels[cid] = label

    # also try boundElements on shapes
    for sid, sel in container_shapes.items():
        if sid not in shape_labels:
            for be in sel.get('boundElements', []):
                if be.get('type') == 'text':
                    tel = by_id.get(be.get('id', ''))
                    if tel:
                        label = get_element_text(tel)
                        if label:
                            shape_labels[sid] = label

    # build node list
    nodes = []

    for sid, label in shape_labels.items():
        sel = container_shapes[sid]
        cx, cy = get_center(sel)
        nodes.append({
            'excalidraw_id': sid,
            'label': label,
            'groupIds': sel.get('groupIds', []),
            'x': cx, 'y': cy,
            'type': 'shape',
        })

    # standalone text elements
    bound_text_ids = set()
    for tid, tel in text_elements.items():
        if tel.get('containerId'):
            bound_text_ids.add(tid)

    arrow_label_ids = set()
    for arr in arrow_elements:
        for be in arr.get('boundElements', []):
            if be.get('type') == 'text':
                arrow_label_ids.add(be.get('id', ''))

    for tid, tel in text_elements.items():
        if tid in bound_text_ids or tid in arrow_label_ids:
            continue
        label = get_element_text(tel)
        if not label:
            continue
        cx, cy = get_center(tel)
        nodes.append({
            'excalidraw_id': tid,
            'label': label,
            'groupIds': tel.get('groupIds', []),
            'x': cx, 'y': cy,
            'type': 'standalone_text',
        })

    # assign stable node IDs
    seen_ids = set()
    excalidraw_id_to_node_id = {}
    for node in nodes:
        base_id = slugify(node['label'])
        node_id = ensure_unique_id(base_id, seen_ids)
        node['node_id'] = node_id
        excalidraw_id_to_node_id[node['excalidraw_id']] = node_id

    # edges
    edges = []
    for arr in arrow_elements:
        start_b = arr.get('startBinding') or {}
        end_b   = arr.get('endBinding')   or {}
        from_eid = start_b.get('elementId')
        to_eid   = end_b.get('elementId')
        from_nid = excalidraw_id_to_node_id.get(from_eid)
        to_nid   = excalidraw_id_to_node_id.get(to_eid)
        if not from_nid or not to_nid:
            continue
        edge_label = ''
        for be in arr.get('boundElements', []):
            if be.get('type') == 'text':
                tel = by_id.get(be.get('id', ''))
                if tel:
                    edge_label = get_element_text(tel)
        edges.append({'from': from_nid, 'to': to_nid, 'label': edge_label})

    # explicit groups from groupIds
    named_groups = {}
    explicit_group_map = defaultdict(list)
    for node in nodes:
        for gid in node.get('groupIds', []):
            explicit_group_map[gid].append(node)

    for i, (gid, group_nodes) in enumerate(explicit_group_map.items(), 1):
        named_groups[f'Group{i}'] = [n['node_id'] for n in group_nodes]

    # spatial clusters for ungrouped nodes
    ungrouped = [n for n in nodes if not n.get('groupIds')]
    if ungrouped:
        spatial = detect_spatial_clusters(ungrouped)
        for i, (_, indices) in enumerate(spatial.items(), 1):
            cluster_nodes = [ungrouped[idx] for idx in indices]
            named_groups[f'Cluster{i}'] = [n['node_id'] for n in cluster_nodes]

    # heuristic open questions / observations from standalone text
    open_questions = []
    observations = []
    obs_date_re = re.compile(r'^\d{4}-\d{2}-\d{2}\s')
    for node in nodes:
        if node['type'] != 'standalone_text':
            continue
        label = node['label']
        if label.startswith('?') or label.endswith('?'):
            open_questions.append(label)
        elif obs_date_re.match(label):
            observations.append(label)

    return {
        'nodes': nodes,
        'edges': edges,
        'named_groups': named_groups,
        'open_questions': open_questions,
        'observations': observations,
    }


# ---------------------------------------------------------------------------
# Markdown renderer
# ---------------------------------------------------------------------------

def render_markdown(parsed, source_file='unknown.excalidraw', date='', title=''):
    if not date:
        date = datetime.date.today().isoformat()
    if not title:
        title = 'Untitled Mental Map'

    nodes = parsed['nodes']
    edges = parsed['edges']
    named_groups = parsed['named_groups']

    node_to_groups = defaultdict(list)
    for gname, nids in named_groups.items():
        for nid in nids:
            node_to_groups[nid].append(gname)

    lines = []

    lines.append(f'# {title}')
    lines.append('')

    lines.append('## Metadata')
    lines.append(f'- **date:** {date}')
    lines.append(f'- **source:** {source_file}')
    lines.append('- **version:** 1')
    lines.append(f'- **title:** {title}')
    lines.append('- **session:** remodeling')
    lines.append('')

    lines.append('## Nodes')
    if nodes:
        for node in nodes:
            nid = node['node_id']
            label = node['label']
            tags = [f'[group:{g}]' for g in node_to_groups.get(nid, [])]
            tag_str = (' ' + ' '.join(tags)) if tags else ''
            lines.append(f'- `n:{nid}` **{label}**{tag_str}')
    else:
        lines.append('*(no nodes detected)*')
    lines.append('')

    lines.append('## Edges')
    if edges:
        for edge in edges:
            if edge['label']:
                lines.append(f'- `n:{edge["from"]}` → `n:{edge["to"]}` : {edge["label"]}')
            else:
                lines.append(f'- `n:{edge["from"]}` → `n:{edge["to"]}`')
    else:
        lines.append('*(no edges detected)*')
    lines.append('')

    lines.append('## Groups')
    if named_groups:
        for gname, nids in named_groups.items():
            lines.append('')
            lines.append(f'### {gname}')
            lines.append('Members:')
            for nid in nids:
                node_label = next(
                    (n['label'] for n in nodes if n['node_id'] == nid), nid
                )
                lines.append(f'- `n:{nid}` {node_label}')
    else:
        lines.append('')
        lines.append('*(no groups detected)*')
    lines.append('')

    lines.append('## Open Questions')
    if parsed['open_questions']:
        for q in parsed['open_questions']:
            lines.append(f'- [ ] {q}')
    else:
        lines.append('*(none detected — add manually)*')
    lines.append('')

    lines.append('## Observations')
    if parsed['observations']:
        for obs in parsed['observations']:
            lines.append(f'- **{obs}**')
    else:
        lines.append('*(none detected — add manually)*')
    lines.append('')

    return '\n'.join(lines)


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(
        description='Parse an .excalidraw JSON file into canonical mental map markdown.'
    )
    parser.add_argument(
        'file', nargs='?',
        help='.excalidraw file path (omit to read from stdin)'
    )
    parser.add_argument('--date', default='', help='Date for metadata (YYYY-MM-DD)')
    parser.add_argument('--title', default='', help='Title for the mental map')
    args = parser.parse_args()

    if args.file:
        try:
            with open(args.file) as f:
                data = json.load(f)
        except FileNotFoundError:
            print(f'Error: file not found: {args.file}', file=sys.stderr)
            sys.exit(1)
        except json.JSONDecodeError as e:
            print(f'Error: invalid JSON in {args.file}: {e}', file=sys.stderr)
            sys.exit(1)
        source_file = os.path.basename(args.file)
        if not args.title:
            name = os.path.splitext(source_file)[0]
            args.title = name.replace('-', ' ').replace('_', ' ').title()
    else:
        try:
            data = json.load(sys.stdin)
        except json.JSONDecodeError as e:
            print(f'Error: invalid JSON from stdin: {e}', file=sys.stderr)
            sys.exit(1)
        source_file = 'stdin.excalidraw'
        if not args.title:
            args.title = 'Untitled Mental Map'

    parsed = parse_excalidraw(data)
    output = render_markdown(
        parsed,
        source_file=source_file,
        date=args.date,
        title=args.title,
    )
    print(output)


if __name__ == '__main__':
    main()
