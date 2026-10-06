#!/usr/bin/env python3
"""Pure scoring of archived legacy local/assisted results; execution was retired."""
import json

def source_rows(result):
    return result["source_evidence"] + [item["source"] for item in result["historical_or_conflicting"]]


def no_answer_evidence(result):
    lanes = ("mandatory", "constraints", "preferences", "facts", "episodes", "conflicts")
    empty_facts = all(all(snapshot.get(lane) == [] for lane in lanes)
                      for snapshot in (result.get("result", {}),
                                       result.get("global_context", {}).get("result", {})))
    return (result.get("recall_status") == "completed"
            and result.get("source_evidence") == []
            and result.get("historical_or_conflicting") == []
            and empty_facts)


def unresolved_without_evidence(result):
    return (no_answer_evidence(result)
            and result.get("evidence_status") == "sufficiency_not_verified"
            and result.get("reference_resolution", {}).get("status") == "unconfirmed")


def score(result, label, source_map, candidates):
    required = set(label["required_sources"])
    candidate_ids = {source_map[item["source_id"]] for item in candidates}
    final = {source_map[item["source_id"]]: item["text"] for item in source_rows(result)}
    final_ids = set(final)
    point_coverage = {sid: {point: point in final.get(sid, "") for point in points}
                      for sid, points in label["required_points"].items() if sid in required}
    available_complete = required <= final_ids and all(all(points.values()) for points in point_coverage.values())
    answerability = label["answerability"]
    clarification = (result.get("reference_resolution", {}).get("needs_clarification") is True
                     and no_answer_evidence(result))
    safety = ((result["evidence_status"] == "ambiguous" and not final_ids) or clarification
              if answerability == "ambiguous" else not final_ids if answerability == "absent"
              else result["evidence_status"] == "sufficiency_not_verified" or clarification)
    forbidden = sorted(final_ids & set(label["forbidden_sources"]))
    safety = safety and not forbidden
    runtime_ambiguous = (result["evidence_status"] == "ambiguous"
                         and result["reference_resolution"]["status"] == "ambiguous")
    return {"answerability": answerability, "candidate_sources": sorted(candidate_ids),
            "candidate_required_sources": sorted(candidate_ids & required),
            "candidate_required_ids_complete": required <= candidate_ids if required else None,
            "final_sources": sorted(final_ids), "final_required_sources": sorted(final_ids & required),
            "required_point_coverage": point_coverage,
            "available_required_evidence_complete": available_complete if required else None,
            "complete_answerable_evidence": available_complete if answerability == "answerable" else None,
            "unnecessary_sources": sorted(final_ids - required),
            "forbidden_found": forbidden,
            "evidence_status": result["evidence_status"], "safety_behavior_passed": safety,
            "unresolved_without_evidence": unresolved_without_evidence(result),
            "runtime_ambiguous_status": runtime_ambiguous,
            "clarification_handoff_proxy": (no_answer_evidence(result)
                                             and (clarification or unresolved_without_evidence(result) or runtime_ambiguous)),
            "semantic_resolution": result["reference_resolution"]["semantic_resolution"],
            "host_asked_project": None,
            "recall_status": result["recall_status"], "result_bytes": len(json.dumps(result, ensure_ascii=False).encode()),
            "duration_ms": result["duration_ms"], "budget_bytes": result["budget"]["content_bytes"]}
