"""Protocol-only synthetic review fixture; this is not a semantic correctness oracle."""
import json

FIXTURE_VERSION = "source-support-full-current-user-v3-development"


def source_support_review(request):
    payload = json.loads(request["input"])
    candidates = payload["frozen_candidates"]
    # A fixed protocol fixture, not evidence that a real USER turn is fully covered.
    # Semantic coverage tests must explicitly supply complete/incomplete/uncertain results.
    return {"review_binding": payload["review_binding"], "omission_supported": not candidates,
            "coverage": {"status": "complete", "missing_user_spans": []},
            "reviews": [{"candidate_index": index, "issues": [],
                         "current_user_quote": payload["current_user_turn"],
                         "checks": dict.fromkeys("subject relation value polarity scope temporal intent source metadata".split(), "supported"),
                         "evidence": [{"turn_index": 0, "quote": payload["current_user_turn"]}]}
                        for index in range(len(candidates))]}
