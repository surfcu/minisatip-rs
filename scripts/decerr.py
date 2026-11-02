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


def _median(values):
    ordered = sorted(values)
    if not ordered:
        return 0.0
    middle = len(ordered) // 2
    if len(ordered) % 2:
        return float(ordered[middle])
    return (ordered[middle - 1] + ordered[middle]) / 2.0


def _episodes(rows, is_startup, interval):
    durations = []
    start = None
    last = None
    for row in rows:
        if row["decerr"] != 0 and not is_startup(row):
            if start is None:
                start = row["ts"]
            last = row["ts"]
        elif start is not None:
            durations.append(last - start + interval)
            start = None
    if start is not None:
        durations.append(last - start + interval)
    return durations


def summarise(rows, join_grace=20.0, adapter=None) -> dict:
    """Summarise one label (and optionally one adapter) of sampled rows."""
    poll_errors = sum(1 for row in rows if "error" in row)
    valid = [row for row in rows if "error" not in row]
    if adapter is not None:
        valid = [row for row in valid if row["adapter"] == adapter]
    valid.sort(key=lambda row: row["ts"])

    gaps = [valid[i]["ts"] - valid[i - 1]["ts"] for i in range(1, len(valid))]
    interval = _median(gaps)

    joins = []
    previous = None
    for row in valid:
        if previous is not None and (
            (previous["sidcount"] == 0 and row["sidcount"] > 0)
            or row["freq"] != previous["freq"]
        ):
            joins.append(row["ts"])
        previous = row

    def is_startup(row):
        return row["decerr"] != 0 and any(
            0 <= row["ts"] - join_ts <= join_grace for join_ts in joins
        )

    startup = [row for row in valid if is_startup(row)]
    steady = [row for row in valid if not is_startup(row)]
    steady_nonzero = [row for row in steady if row["decerr"] != 0]
    durations = _episodes(valid, is_startup, interval)

    return {
        "samples": len(steady),
        "nonzero_samples": len(steady_nonzero),
        "nonzero_share": len(steady_nonzero) / len(steady) if steady else 0.0,
        "peak": max((row["decerr"] for row in steady_nonzero), default=0),
        "episodes": len(durations),
        "episode_max_s": max(durations, default=0.0),
        "episode_p50_s": _median(durations),
        "joins": len(joins),
        "startup_nonzero_samples": len(startup),
        "poll_errors": poll_errors,
    }


def _read_file(path):
    rows = []
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except ValueError:
                continue
    return rows


def _load_rows(paths):
    rows = []
    for path in paths:
        file_rows = _read_file(path)
        if not any("error" not in row for row in file_rows):
            print(f"{path}: no usable rows", file=sys.stderr)
            return None
        rows.extend(file_rows)
    return rows


def build_summaries(rows, adapter=None, join_grace=20.0):
    by_label = {}
    for row in rows:
        by_label.setdefault(row.get("label", ""), []).append(row)

    summaries = []
    for label in sorted(by_label):
        label_rows = by_label[label]
        adapters = sorted(
            {row["adapter"] for row in label_rows if "error" not in row}
        )
        for index in adapters:
            if adapter is not None and index != adapter:
                continue
            summary = summarise(label_rows, join_grace, index)
            summaries.append({"label": label, "adapter": index, **summary})
    return summaries


TABLE_COLUMNS = (
    "label", "adapter", "samples", "nonzero_samples", "nonzero_share", "peak",
    "episodes", "episode_max_s", "episode_p50_s", "joins",
    "startup_nonzero_samples", "poll_errors",
)


def _print_table(summaries):
    print(" ".join(TABLE_COLUMNS))
    for summary in summaries:
        cells = []
        for column in TABLE_COLUMNS:
            value = summary[column]
            if column in ("nonzero_share", "episode_max_s", "episode_p50_s"):
                cells.append(f"{value:.3f}")
            else:
                cells.append(str(value))
        print(" ".join(cells))


def cmd_compare(args):
    rows = _load_rows(args.files)
    if rows is None:
        return 2
    summaries = build_summaries(rows, args.adapter, args.join_grace)
    if args.json:
        print(json.dumps(summaries, indent=2))
    else:
        _print_table(summaries)
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

    compare = sub.add_parser("compare", help="compare sampled JSONL files")
    compare.add_argument("files", nargs="+", help="JSONL files written by poll")
    compare.add_argument("--adapter", type=int, help="only this adapter index")
    compare.add_argument("--join-grace", type=float, default=20.0,
                         help="seconds after a join treated as start-up")
    compare.add_argument("--json", action="store_true", help="emit JSON")
    compare.set_defaults(func=cmd_compare)

    return parser


def main(argv=None):
    args = build_parser().parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
