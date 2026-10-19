"""Parse the current CLI's named tracing events, not historical campaign output."""

import re


def node_event(line):
    """Ignore presentation prefixes and only admit known scalar event fields."""
    fields = {}
    for match in re.finditer(r'(?:^|\s)(\w+)=(?:"([^"]*)"|(\S+))', line):
        fields[match[1]] = match[2] if match[2] is not None else match[3]
    if fields.get("event") == "rpc_listening":
        return {"event": "rpc_listening", "address": fields["rpc_listen"]}
    if fields.get("event") == "sync_progress":
        return {"event": "sync_progress", "connections": int(fields["connections"])}
    return None
