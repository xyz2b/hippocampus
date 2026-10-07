import copy
import json
import pathlib
import sys
import unittest

from adapters import adapt_recall, question_input, NEW_ADAPTER, V4_ADAPTER, V5_ADAPTER, V6_ADAPTER
from scoring import score

M15 = pathlib.Path(__file__).resolve().parent.parent / "m15"
sys.path.insert(0, str(M15))
from check_samples import check


class M15Scoring(unittest.TestCase):
    def fixture(self, version="recall.v6"):
        case = json.loads((M15 / "mvp.json").read_text())
        q = case["questions"][1]
        source = {"kind":"stored", "source_id":"runtime-m3"}
        raw = {"version":version, "request":question_input(case, q), "status":"answered", "records":[],
               "facts":[{"statement":"这是未来计划，条件成立时可能散步。", "polarity":"affirm", "current":"yes",
                         "record_ids":[], "evidence":[{"source":source, "quote":{"text":"如果不下雨"}}],
                         "conditions":[{"quote":{"text":"如果不下雨"}}], "uncertainty":{"quote":{"text":"可能"}}}]}
        return case, q, raw

    def view(self, case, q, raw):
        return adapt_recall(question_input(case, q), q["query_meta"], raw, {"m3":"runtime-m3"},
                            source_texts={m["id"]:m["text"] for m in case["messages"]})

    def test_v6_is_supported_and_history_adapters_unchanged(self):
        c,q,raw=self.fixture()
        self.assertEqual(self.view(c,q,raw)["adapter"], V6_ADAPTER)
        self.assertEqual(self.view(c,q,raw)["binding"], [])
        raw["version"]="recall.v5"
        self.assertEqual(self.view(c,q,raw)["adapter"], V5_ADAPTER)
        raw["version"]="recall.v4"
        self.assertEqual(self.view(c,q,raw)["adapter"], V4_ADAPTER)
        self.assertEqual(self.view(c,q,raw)["binding"], [])
        raw["version"]="recall.v3"
        self.assertEqual(self.view(c,q,raw)["adapter"], NEW_ADAPTER)
        raw["version"]="recall.v7"
        self.assertTrue(self.view(c,q,raw)["binding"])
        raw["version"]="recall.v4";raw["request"]["query"]="别的问题"
        self.assertTrue(self.view(c,q,raw)["binding"])

    def test_presentation_and_date_meaning_never_auto_pass(self):
        c,q,raw=self.fixture()
        # Deliberately wrong free text even though structural qualifiers exist.
        raw["facts"][0]["statement"]="已经完成全部出行和散步。"
        raw["records"]=[{"id":1,"format":"current","candidate":{"speech":"plan",
                         "subject":{"entity_id":"self"},"value":{"text":"未来计划"}},
                         "evidence":raw["facts"][0]["evidence"],"said_on":None}]
        raw["facts"][0]["record_ids"]=[1]
        result=score(q["expect_common"],q["query_meta"],self.view(c,q,raw))
        semantic=[i for i in result["items"] if (i["id"].startswith("answer_requirements:") and not i["id"].endswith(".sources")) or i.get("kind")=="plan_as_occurred"]
        self.assertEqual(len(semantic),4)
        self.assertTrue(all(i["result"]=="review" for i in semantic))
        # A reviewer key depends on both the presented statement and criterion.
        plan = next(i for i in semantic if i["id"] == "answer_requirements:latest_plan")
        self.assertIn("2027-06-09",plan["content"]["requirement"]["requirement"])
        self.assertEqual(plan["content"]["answer"]["records"][0]["state"]["candidate"]["speech"],"plan")

    def test_missing_condition_or_uncertainty_fails_mechanically(self):
        c,q,raw=self.fixture()
        raw["facts"][0]["conditions"]=[];raw["facts"][0]["uncertainty"]=None
        result=score(q["expect_common"],q["query_meta"],self.view(c,q,raw))
        items=[i for i in result["items"] if i["id"].startswith("required_qualifiers:")]
        self.assertEqual(len(items),2)
        self.assertTrue(all(i["result"]=="auto_fail" for i in items))

    def test_fictional_sample_scan(self):
        self.assertGreater(check()["prior_inputs"],0)


if __name__ == "__main__":
    unittest.main()
