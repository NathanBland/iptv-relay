#!/usr/bin/env bash
set -euo pipefail

repo_root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
DOCS_ROOT="$repo_root/docs/docs" python3 - <<'PY'
import os
import re
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit

root = Path(os.environ["DOCS_ROOT"])
link_pattern = re.compile(r"(?<!!)(?:\[[^]]*\])\(([^)]+)\)")
heading_pattern = re.compile(r"^#{1,6}\s+(.+?)\s*#*\s*$")

def heading_id(value: str) -> str:
    value = re.sub(r"[`*_~]", "", value).lower()
    return re.sub(r"[^a-z0-9 -]", "", value).replace(" ", "-")

errors = []
for source in sorted(root.rglob("*.md")):
    headings = {
        heading_id(match.group(1))
        for line in source.read_text(encoding="utf-8").splitlines()
        if (match := heading_pattern.match(line))
    }
    for match in link_pattern.finditer(source.read_text(encoding="utf-8")):
        target = match.group(1).strip().split()[0]
        parsed = urlsplit(target)
        if parsed.scheme or parsed.netloc:
            continue
        fragment = unquote(parsed.fragment)
        if parsed.path:
            destination = (source.parent / unquote(parsed.path)).resolve()
            if destination.suffix == "":
                destination = destination.with_suffix(".md")
            if not destination.is_file():
                errors.append(f"{source.relative_to(root.parent.parent)}: missing target {target}")
                continue
            if fragment:
                destination_headings = {
                    heading_id(match.group(1))
                    for line in destination.read_text(encoding="utf-8").splitlines()
                    if (match := heading_pattern.match(line))
                }
                if fragment.lower() not in destination_headings:
                    errors.append(f"{source.relative_to(root.parent.parent)}: missing anchor {target}")
        elif fragment and fragment.lower() not in headings:
            errors.append(f"{source.relative_to(root.parent.parent)}: missing anchor {target}")

if errors:
    print("Documentation link validation failed:", file=sys.stderr)
    print("\n".join(errors), file=sys.stderr)
    raise SystemExit(1)
print(f"Documentation link validation passed for {len(list(root.rglob('*.md')))} Markdown files.")
PY
