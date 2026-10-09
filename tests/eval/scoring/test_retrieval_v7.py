import copy
import unittest
from adapters import adapt_recall, V7_ADAPTER, V6_ADAPTER


class RetrievalV7(unittest.TestCase):
    def fixture(self):
        q = {"query": "测试问题", "context": [], "task_id": None, "at": 1791000000}
        raw = {"version": "recall.v7", "request": dict(q, request_id="test"),
               "status": "insufficient", "facts": [], "records": [], "raw": [],
               "retrieval": {"status": "pending", "sources_total": 5, "sources_indexed": 2,
                             "segments_total": 7, "segments_indexed": 3}}
        return q, raw

    def view(self, q, raw):
        return adapt_recall(q, {}, raw, {}, source_texts={})

    def test_metadata_is_separate_from_evidence_and_history_versions(self):
        q, raw = self.fixture()
        current = self.view(q, raw)
        self.assertEqual(current["adapter"], V7_ADAPTER)
        self.assertEqual(current["binding"], [])
        old = copy.deepcopy(raw)
        old["version"] = "recall.v6"
        del old["retrieval"]
        history = self.view(q, old)
        self.assertEqual(history["adapter"], V6_ADAPTER)
        current.pop("adapter"); history.pop("adapter")
        self.assertEqual(current, history)

    def test_missing_bad_status_or_impossible_coverage_fails_binding(self):
        q, raw = self.fixture()
        for change in [None, {"status": "bad"}, {"sources_indexed": 6}, {"segments_indexed": -1}]:
            changed = copy.deepcopy(raw)
            if change is None:
                del changed["retrieval"]
            else:
                changed["retrieval"].update(change)
            self.assertTrue(self.view(q, changed)["binding"])


if __name__ == "__main__":
    unittest.main()
