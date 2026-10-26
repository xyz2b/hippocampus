import copy
import unittest
from adapters import adapt_recall, V7_ADAPTER, V8_ADAPTER
import test_retrieval_v7


class FragmentValueV8(unittest.TestCase):
    def test_v8_and_historical_v7_keep_their_independent_adapter_identity(self):
        q, old = test_retrieval_v7.RetrievalV7().fixture()
        new = copy.deepcopy(old)
        new["version"] = "recall.v8"
        prior = adapt_recall(q, {}, old, {}, source_texts={})
        current = adapt_recall(q, {}, new, {}, source_texts={})
        self.assertEqual(prior.pop("adapter"), V7_ADAPTER)
        self.assertEqual(current.pop("adapter"), V8_ADAPTER)
        self.assertEqual(prior, current)
        del new["retrieval"]
        self.assertTrue(adapt_recall(q, {}, new, {}, source_texts={})["binding"])


if __name__ == "__main__":
    unittest.main()
