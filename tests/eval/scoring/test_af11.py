"""AF11 fixed inputs retain original recordings and reject mismatched provenance."""
import copy
import importlib.util
import json
import pathlib
import sys
import tempfile
import unittest
from unittest.mock import patch

FIXED = pathlib.Path(__file__).resolve().parent.parent / "fixed_inputs"
sys.path.insert(0, str(FIXED))
spec = importlib.util.spec_from_file_location("af11_inputs", FIXED / "af11.py")
af11 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(af11)
sys.path.remove(str(FIXED))


class AF11Inputs(unittest.TestCase):
    def test_seven_frozen_requests_and_fresh_event_evidence_are_reproducible(self):
        data = af11.extract()
        self.assertEqual(data, json.loads((FIXED / "af11.json").read_text()))
        self.assertEqual([s["stage"] for s in data["samples"]].count("r1"), 2)
        self.assertEqual([s["stage"] for s in data["samples"]].count("r4"), 5)
        for s in data["samples"]:
            o = s["origin"]
            old, _, _ = af11.recorded(o["label"], o["case"], o["operation"].split(":")[1], s["stage"])
            if "evidence_origin" not in s:
                self.assertEqual(s["input"], old)
            else:
                eo = s["evidence_origin"]
                fresh, _, _ = af11.recorded(eo["label"], eo["case"], "event_q3", "r4")
                expected = copy.deepcopy(old)
                expected["evidence"] = fresh["evidence"]
                expected["evidence"]["current_message"] = old["evidence"]["current_message"]
                self.assertEqual(s["input"], expected)
                self.assertEqual(s["input"]["evidence"]["records"][0]["update"]["status"], "judged")
            self.assertNotIn("model", s["input"])
            self.assertNotIn("expect", s["input"])

    def test_source_fingerprint_mismatch_is_rejected(self):
        label, case = af11.TARGET, "unresolved_conflict"
        doc = json.loads((af11.ROOT / "end_to_end" / label / (case + ".json")).read_text())
        row = next(r for r in doc["operations"]["recall:conflict_q3"] if r["schema"] == "r4_v3")
        row["request"] = "sha256:changed"
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            dest = root / "end_to_end" / label
            dest.mkdir(parents=True)
            (dest / (case + ".json")).write_text(json.dumps(doc))
            with patch.object(af11, "ROOT", root):
                with self.assertRaisesRegex(ValueError, "指纹不符"):
                    af11.recorded(label, case, "conflict_q3", "r4")


if __name__ == "__main__":
    unittest.main()
