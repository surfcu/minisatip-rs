import importlib.util
import io
import json
import socket
import tempfile
import threading
import unittest
from contextlib import redirect_stdout
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE.parent / "decerr.py"
FIXTURES = HERE / "fixtures"

spec = importlib.util.spec_from_file_location("decerr", SCRIPT)
decerr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(decerr)


def _read(name):
    return (FIXTURES / name).read_text(encoding="utf-8")


class ParseStateTest(unittest.TestCase):
    def test_strict_and_loose_parse_the_same(self):
        strict = decerr.parse_state(_read("state-strict.json"))
        loose = decerr.parse_state(_read("state-loose.txt"))
        self.assertEqual(strict, loose)
        self.assertEqual(strict["ad_enabled"], [1, 0, 1])
        self.assertEqual(strict["ad_decerr"], [0, 0, 5])

    def test_missing_key_raises(self):
        with self.assertRaisesRegex(KeyError, "ad_decerr"):
            decerr.parse_state('{"ad_enabled": [1], "ad_type": [0], '
                               '"ad_freq": [1], "ad_sidcount": [1]}')

    def test_disabled_adapter_yields_no_row(self):
        state = decerr.parse_state(_read("state-strict.json"))
        rows = decerr.sample_rows(state, 100.0, "cpp")
        self.assertEqual([row["adapter"] for row in rows], [0, 2])
        self.assertEqual(rows[1]["decerr"], 5)
        self.assertEqual(rows[0]["ts"], 100.0)
        self.assertEqual(rows[0]["label"], "cpp")


class _QuietHandler(SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


class PollTest(unittest.TestCase):
    def setUp(self):
        handler = partial(_QuietHandler, directory=str(FIXTURES))
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.tmp = tempfile.TemporaryDirectory()
        self.out = Path(self.tmp.name) / "rows.jsonl"

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.tmp.cleanup()

    def test_poll_writes_enabled_adapters(self):
        url = f"http://127.0.0.1:{self.server.server_port}/state-strict.json"
        code = decerr.main(["poll", "--url", url, "--label", "cpp",
                            "--count", "2", "--interval", "0.1", "--out", str(self.out)])
        self.assertEqual(code, 0)
        rows = [json.loads(line) for line in self.out.read_text().splitlines()]
        self.assertEqual(len(rows), 4)
        self.assertEqual([row["adapter"] for row in rows], [0, 2, 0, 2])

    def test_poll_keeps_going_after_fetch_error(self):
        sock = socket.socket()
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
        sock.close()
        url = f"http://127.0.0.1:{port}/state.json"
        code = decerr.main(["poll", "--url", url, "--label", "rs",
                            "--count", "1", "--out", str(self.out)])
        self.assertEqual(code, 0)
        rows = [json.loads(line) for line in self.out.read_text().splitlines()]
        self.assertEqual(len(rows), 1)
        self.assertIn("error", rows[0])
        self.assertEqual(rows[0]["label"], "rs")


class SummariseTest(unittest.TestCase):
    def rows(self, name):
        return decerr._read_file(str(FIXTURES / name))

    def test_run_a_numbers(self):
        summary = decerr.summarise(self.rows("run-a.jsonl"))
        self.assertEqual(summary, {
            "samples": 8,
            "nonzero_samples": 2,
            "nonzero_share": 0.25,
            "peak": 4,
            "episodes": 1,
            "episode_max_s": 4.0,
            "episode_p50_s": 4.0,
            "joins": 1,
            "startup_nonzero_samples": 3,
            "poll_errors": 0,
        })

    def test_run_b_numbers(self):
        summary = decerr.summarise(self.rows("run-b.jsonl"))
        self.assertEqual(summary, {
            "samples": 9,
            "nonzero_samples": 2,
            "nonzero_share": 2 / 9,
            "peak": 5,
            "episodes": 2,
            "episode_max_s": 2.0,
            "episode_p50_s": 2.0,
            "joins": 1,
            "startup_nonzero_samples": 0,
            "poll_errors": 1,
        })

    def test_startup_samples_are_not_in_steady_episodes(self):
        summary = decerr.summarise(self.rows("run-a.jsonl"))
        self.assertEqual(summary["startup_nonzero_samples"], 3)
        self.assertEqual(summary["episodes"], 1)

    def test_startup_burst_splits_steady_episodes(self):
        rows = [
            {"ts": 0.0, "label": "rs", "adapter": 0, "type": 0, "freq": 100, "sidcount": 1, "decerr": 3},
            {"ts": 2.0, "label": "rs", "adapter": 0, "type": 0, "freq": 200, "sidcount": 1, "decerr": 9},
            {"ts": 4.0, "label": "rs", "adapter": 0, "type": 0, "freq": 200, "sidcount": 1, "decerr": 4},
        ]
        summary = decerr.summarise(rows, join_grace=1.0)
        self.assertEqual(summary["startup_nonzero_samples"], 1)
        self.assertEqual(summary["episodes"], 2)
        self.assertEqual(summary["episode_max_s"], 2.0)

    def test_adapter_filter(self):
        base = self.rows("run-a.jsonl")
        other = [dict(row, adapter=1, decerr=row["decerr"] * 2) for row in base]
        combined = base + other
        self.assertEqual(decerr.summarise(combined, adapter=0),
                         decerr.summarise(base))
        self.assertEqual(decerr.summarise(combined, adapter=1)["peak"], 8)


class CompareTest(unittest.TestCase):
    def test_compare_prints_two_rows(self):
        out = io.StringIO()
        with redirect_stdout(out):
            code = decerr.main([
                "compare", str(FIXTURES / "run-a.jsonl"),
                str(FIXTURES / "run-b.jsonl"),
            ])
        self.assertEqual(code, 0)
        lines = out.getvalue().strip().splitlines()
        self.assertEqual(len(lines), 3)
        self.assertIn("cpp", lines[1])
        self.assertIn("rs", lines[2])

    def test_compare_json_parses(self):
        out = io.StringIO()
        with redirect_stdout(out):
            code = decerr.main([
                "compare", str(FIXTURES / "run-a.jsonl"),
                str(FIXTURES / "run-b.jsonl"), "--json",
            ])
        self.assertEqual(code, 0)
        data = json.loads(out.getvalue())
        self.assertEqual([row["label"] for row in data], ["cpp", "rs"])

    def test_compare_empty_file_exits_2(self):
        with tempfile.TemporaryDirectory() as tmp:
            empty = Path(tmp) / "empty.jsonl"
            empty.write_text("")
            code = decerr.main(["compare", str(empty)])
        self.assertEqual(code, 2)


if __name__ == "__main__":
    unittest.main()
