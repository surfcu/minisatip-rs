#!/usr/bin/env python3
"""Sample and compare decrypt-error levels from minisatip /state.json."""

import argparse
import json
import re
import sys
import time
import urllib.request
from urllib.parse import urlsplit, urlunsplit

KEYS = ("ad_enabled", "ad_type", "ad_freq", "ad_sidcount", "ad_decerr")
ARRAY_RE = re.compile(
    r'"(ad_enabled|ad_type|ad_freq|ad_sidcount|ad_decerr)"\s*:\s*\[([^\]]*)\]'
)


def _to_int(value):
    if isinstance(value, str):
        value = value.strip()
        if not value:
            return None
    return int(float(value))


def parse_state(text: str) -> dict[str, list[int]]:
    """Return the per-adapter arrays the tool needs."""
    try:
        raw = json.loads(text)
    except ValueError:
        raw = {}
        for key, body in ARRAY_RE.findall(text):
            raw[key] = body.split(",")

    state = {}
    for key in KEYS:
        if key not in raw:
            raise KeyError(key)
        values = [_to_int(item) for item in raw[key]]
        state[key] = [value for value in values if value is not None]
    return state


def sample_rows(state, ts, label) -> list[dict]:
    rows = []
    for index, enabled in enumerate(state["ad_enabled"]):
        if not enabled:
            continue
        rows.append(
            {
                "ts": float(ts),
                "label": label,
                "adapter": index,
                "type": state["ad_type"][index],
                "freq": state["ad_freq"][index],
                "sidcount": state["ad_sidcount"][index],
                "decerr": state["ad_decerr"][index],
            }
        )
    return rows


def _state_url(url):
    parts = urlsplit(url)
    if parts.path:
        return url
    return urlunsplit((parts.scheme, parts.netloc, "/state.json", parts.query, parts.fragment))


def _fetch(url):
    with urllib.request.urlopen(url, timeout=5) as response:
        return response.read().decode("utf-8", "replace")


def _write_rows(rows, out):
    for row in rows:
        out.write(json.dumps(row) + "\n")
        out.flush()


def cmd_poll(args):
    url = _state_url(args.url)
    poll_index = 0
    out = open(args.out, "a", encoding="utf-8") if args.out else sys.stdout
    try:
        while args.count is None or poll_index < args.count:
            poll_index += 1
            ts = time.time()
            try:
                rows = sample_rows(parse_state(_fetch(url)), ts, args.label)
            except Exception as exc:
                rows = [{"ts": ts, "label": args.label, "error": str(exc)}]
            _write_rows(rows, out)
            if args.count is not None and poll_index >= args.count:
                break
            time.sleep(args.interval)
    except KeyboardInterrupt:
        pass
    finally:
        if out is not sys.stdout:
            out.close()
    return 0


def build_parser():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    poll = sub.add_parser("poll", help="sample a minisatip /state.json")
    poll.add_argument("--url", required=True, help="base URL or full /state.json URL")
    poll.add_argument("--label", required=True, help="build label for the samples")
    poll.add_argument("--interval", type=float, default=2.0, help="seconds between polls")
    poll.add_argument("--out", help="append JSON lines to this file")
    poll.add_argument("--count", type=int, help="stop after this many polls")
    poll.set_defaults(func=cmd_poll)

    return parser


def main(argv=None):
    args = build_parser().parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
