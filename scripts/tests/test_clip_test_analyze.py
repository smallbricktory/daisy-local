#!/usr/bin/env python3
"""Tests for scripts/clip-test-analyze.py on a fictional fixture session.

  python3 -m unittest discover -s scripts/tests
"""
import importlib.util
import io
import json
import os
import unittest
from contextlib import redirect_stdout

HERE = os.path.dirname(os.path.abspath(__file__))
SESSION = os.path.join(HERE, "fixtures", "clip-test-session")
REF = os.path.join(HERE, "fixtures", "clip-test-ref.tsv")

spec = importlib.util.spec_from_file_location("clip_test_analyze", os.path.join(HERE, "..", "clip-test-analyze.py"))
cta = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cta)


class Normalize(unittest.TestCase):
    def test_fillers_and_punctuation_are_removed(self):
        self.assertEqual(cta.normalize("Um, the Borealis order — uh, on Friday. Hmm"), ["the", "borealis", "order", "on", "friday"])

    def test_word_internal_apostrophes_survive(self):
        self.assertEqual(cta.normalize("We'll ship 'today'"), ["we'll", "ship", "today"])


class Wer(unittest.TestCase):
    def test_counts_each_edit_kind(self):
        ref = "northwind ships the order".split()
        self.assertEqual(cta.edit_ops(ref, ref), (0, 0, 0))
        self.assertEqual(cta.edit_ops(ref, "northwind ships a order".split()), (1, 0, 0))
        self.assertEqual(cta.edit_ops(ref, "northwind ships the big order".split()), (0, 1, 0))
        self.assertEqual(cta.edit_ops(ref, "northwind the order".split()), (0, 0, 1))
        self.assertEqual(cta.wer(ref, [])["wer"], 1.0)


class Anchors(unittest.TestCase):
    def test_audio_maps_through_the_latest_anchor_at_or_before_it(self):
        a = cta.Anchors([(5000, 1_005_000), (0, 1_000_000), (10_000, 1_020_000)])
        self.assertEqual(a.wall_of(900), 1_000_900)
        self.assertEqual(a.wall_of(10_300), 1_020_300)
        self.assertEqual(a.wall_of(-100), 999_900, "before the first anchor extrapolates from it")


class Report(unittest.TestCase):
    def setUp(self):
        self.r = cta.analyze(SESSION, REF)

    def test_caption_lags_per_track_and_engine(self):
        (c,) = self.r["captions"]
        self.assertEqual((c["track"], c["engine"], c["words"], c["words_seen_in_interim"]), ("system", "nemotron", 6, 5))
        self.assertEqual(c["first_interim_lag_ms"], {"p50": 500, "p90": 600})
        self.assertEqual(c["final_lag_ms"], {"p50": 1100, "p90": 1700})

    def test_wer_per_track_and_merged(self):
        by = {a["source"]: a for a in self.r["accuracy"]}
        self.assertEqual(by["live system"]["wer"], 0.0)
        self.assertEqual(by["live mic"]["wer"], 1.0)
        self.assertEqual(by["live both tracks"]["wer"], 0.125)
        self.assertEqual(by["live both tracks"]["ins"], 1)

    def test_decode_cost_rtf_and_backlog(self):
        (d,) = self.r["decode"]
        self.assertEqual(d["cost_ms"], {"p50": 50, "p90": 60, "max": 60})
        self.assertAlmostEqual(d["rtf"], 150 / 1680, places=4)
        self.assertEqual(d["backlog_max_ms"], 300)

    def test_result_arrival_lag(self):
        (x,) = self.r["results"]
        self.assertEqual((x["engine"], x["final"], x["lag_ms"]["p50"], x["fed_ahead_ms"]["p50"]), ("speechanalyzer", True, 700, 600))

    def test_process_and_finalize(self):
        p = self.r["process"]
        self.assertEqual((p["cpu_pct"]["mean"], p["cpu_pct"]["max"], p["rss_peak_mb"], p["threads_max"]), (15.0, 20.0, 950.0, 42))
        self.assertEqual(p["busiest_threads"][0], {"name": "daisy-live-pipe", "cpu_pct_mean": 10.0})
        (run,) = self.r["finalize"]
        self.assertEqual(run["total_ms"], 11_000)
        stages = {s["stage"]: s["ms"] for s in run["stages"]}
        self.assertEqual(stages["transcribing"], 3_000)
        self.assertEqual(stages["summarizing"], 4_000)
        self.assertIsNone(stages["done"])

    def test_a_window_limits_words_and_hypotheses(self):
        r = cta.analyze(SESSION, REF, window_s=(9, 13))
        (c,) = r["captions"]
        self.assertEqual(c["words"], 3)
        by = {a["source"]: a for a in r["accuracy"]}
        self.assertEqual(by["live system"]["wer"], 0.375)
        self.assertEqual(r["decode"], [])

    def test_an_offset_windows_around_the_timed_reference(self):
        r = cta.analyze(SESSION, REF, offset_s=0.5)
        self.assertEqual(r["window_ms"], (-500, 15_000))

    def test_text_and_json_output(self):
        buf = io.StringIO()
        with redirect_stdout(buf):
            cta.main([SESSION, "--ref", REF])
        text = buf.getvalue()
        self.assertIn("first-interim p50/p90", text)
        self.assertIn("transcribing", text)
        buf = io.StringIO()
        with redirect_stdout(buf):
            cta.main([SESSION, "--json"])
        self.assertEqual(json.loads(buf.getvalue())["header"]["engine"]["engine"], "nemotron")

    def test_a_folder_without_telemetry_still_reports(self):
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            r = cta.analyze(d, REF)
        self.assertIsNone(r["header"])
        self.assertEqual(r["captions"], [])


if __name__ == "__main__":
    unittest.main()
