#!/usr/bin/env python3
"""FormatJson — a subprocess tool for oli.

Speaks the standard oli subprocess contract:
  * receives the call's arguments object as JSON on stdin
  * emits the result on stdout
  * any failure goes to stderr with a non-zero exit code

Input shape:
    {"json": "<the document to pretty-print>", "indent": 2}

Output:
    the document, re-serialized with sorted keys and the given indent.
"""

from __future__ import annotations

import json
import sys


def main() -> int:
    try:
        args = json.load(sys.stdin)
    except json.JSONDecodeError as e:
        print(f"failed to parse arguments object: {e}", file=sys.stderr)
        return 2

    raw = args.get("json")
    if not isinstance(raw, str) or raw == "":
        print("`json` argument is required and must be a non-empty string", file=sys.stderr)
        return 2

    indent = args.get("indent", 2)
    if not isinstance(indent, int) or indent < 0 or indent > 8:
        print("`indent` must be an integer between 0 and 8", file=sys.stderr)
        return 2

    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError as e:
        print(f"input is not valid JSON: {e}", file=sys.stderr)
        return 1

    json.dump(parsed, sys.stdout, indent=indent, sort_keys=True, ensure_ascii=False)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
