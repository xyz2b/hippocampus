import copy
import unittest
from adapters import adapt_recall, V8_ADAPTER, V9_ADAPTER, V10_ADAPTER, V11_ADAPTER


class SnapshotV9(unittest.TestCase):
    def test_snapshot_is_not_a_question_answer_and_history_adapter_is_unchanged(self):
        q = {"query": "测试问题", "context": [], "task_id": None, "at": 1791000000}
        raw = {"version": "recall.v8", "request": dict(q, request_id="test"),
               "status": "context_only", "facts": [], "records": [], "raw": [],
               "semantic_judgment": "skipped", "retrieval": {
                   "status": "disabled", "sources_total": 0, "sources_indexed": 0,
                   "segments_total": 0, "segments_indexed": 0}}
        old = adapt_recall(q, {}, raw, {}, source_texts={})
        self.assertEqual(old["adapter"], V8_ADAPTER)
        new_raw = copy.deepcopy(raw)
        new_raw["version"] = "recall.v9"
        new_raw["statement_snapshot"] = {"records": [{"candidate": {"sentence": {
            "text": "快照陈述不能变成答案"}}}], "coverage_unknown": True}
        new = adapt_recall(q, {}, new_raw, {}, source_texts={})
        self.assertEqual(new["adapter"], V9_ADAPTER)
        self.assertEqual(new["binding"], [])
        old.pop("adapter"); new.pop("adapter")
        self.assertEqual(new, old)
        new_raw["version"] = "recall.v10"
        current = adapt_recall(q, {}, new_raw, {}, source_texts={})
        self.assertEqual(current["adapter"], V10_ADAPTER)
        current.pop("adapter")
        self.assertEqual(current, new)
        new_raw["version"] = "recall.v11"
        latest = adapt_recall(q, {}, new_raw, {}, source_texts={})
        self.assertEqual(latest.pop("adapter"), V11_ADAPTER)
        self.assertEqual(latest, new)
        del new_raw["retrieval"]
        self.assertTrue(adapt_recall(q, {}, new_raw, {}, source_texts={})["binding"])


if __name__ == "__main__":
    unittest.main()
