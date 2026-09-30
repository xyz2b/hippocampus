# Generates tests/eval/w4/samples/*.json. The JSON files are the reviewed source of truth;
# edit here and regenerate, then re-run check_samples.py and get the changes reviewed.
import json, pathlib
OUT = pathlib.Path(__file__).resolve().parent / "samples"
JUDGED = {"status": "judged", "reasons": []}

def Q(t): return {"text": t}
def cand(subject, relation, sentence, value=None, value_quote=None, kind="assertion", memory_type="fact",
         speech="user_statement", polarity="affirm", conditions=(), times=(), uncertainty=None, strength=None):
    if kind == "assertion":
        if relation == "fact":
            v = {"kind": "text", "text": sentence, "quote": Q(sentence)}
        else:
            v = {"kind": "text", "text": value, "quote": Q(value_quote or value)}
    else:
        v = None
    return {"kind": kind, "speech": speech, "subject": {"entity_id": subject}, "relation": relation,
            "memory_type": memory_type, "value": v, "sentence": Q(sentence), "polarity": polarity,
            "strength": strength, "uncertainty": uncertainty,
            "conditions": [{"quote": Q(c), "resolved": True} for c in conditions], "times": list(times)}
def ev(sid, quote): return {"source": {"kind": "stored", "source_id": sid}, "quote": Q(quote)}
UNKNOWN_RES = {"status": "unknown", "start": None, "end_exclusive": None, "precision": "unknown"}
def recurring(q): return {"role": "event", "quote": Q(q), "interp": {"kind": "recurring"}, "anchor": "said_on"}
def rec(id, c, sid, said_on, status="active", reason=None, ended="not_ended", history=(), rel=(), update=JUDGED,
        time_resolutions=None):
    tr = time_resolutions if time_resolutions is not None else [UNKNOWN_RES for t in c["times"] if t["interp"]["kind"] == "recurring"]
    return {"format": "current", "id": id, "stored_status": status, "termination_reason": reason, "ended": ended,
            "candidate": c, "evidence": [ev(sid, c["sentence"]["text"])], "grade": "committed", "said_on": said_on,
            "time_resolutions": tr, "history": list(history), "open_relations": list(rel), "update": update}
def incand(c, sid, said_on, index=0, committed_id=None, time_resolutions=None):
    tr = list(time_resolutions) if time_resolutions is not None else [UNKNOWN_RES for t in c["times"] if t["interp"]["kind"] == "recurring"]
    return {"index": index, "committed_id": committed_id, "candidate": c,
            "source": {"kind": "stored", "source_id": sid}, "said_on": said_on, "time_resolutions": tr}
SCOPE_PENDING = {"status": "pending", "reasons": ["update_scope_incomplete"]}
PARTIAL = {"complete": False, "omitted": 0}
def sample(id, focus, sources, candidates, records, expect, relations=(), coverage=None, call_w4=True,
           boundary_with=None, rejected=(), note=None, batch=1, label_status="reviewed"):
    d = {"id": id, "contract": "w4.v1", "batch": batch, "label_status": label_status, "focus": focus,
         "boundary_with": boundary_with, "sources": sources,
         "input": {"candidates": candidates, "records": records, "relations": list(relations),
                   "coverage": coverage or {"complete": True, "omitted": 0}},
         "call_w4": call_w4, "expect": expect, "rejected_alternatives": list(rejected)}
    if note: d["note"] = note
    (OUT / f"{id}.json").write_text(json.dumps(d, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

def S(text, said_on): return {"text": text, "said_on": said_on}

# ---------------- W01 现实变化（有生效时间）
t = "我上个月从北京搬到上海了。"
last_month = {"role": "event", "quote": Q("上个月"), "interp": {"kind": "relative", "unit": "month", "offset": -1}, "anchor": "said_on"}
sample("w4_01_state_change", "现实变化 state_change：旧值曾经成立，现在变了",
  {"s-101": S("我住在北京。", "2026-03-02"), "s-205": S(t, "2026-09-10")},
  [incand(cand("self", "lives_in", t, "上海", times=[last_month]), "s-205", "2026-09-10",
          time_resolutions=[{"status": "resolved", "start": 1785513600, "end_exclusive": 1788192000, "precision": "month"}])],
  [rec(17, cand("self", "lives_in", "我住在北京。", "北京"), "s-101", "2026-03-02")],
  {"decisions": [{"candidate": 0, "status": "decided", "action": "state_change", "targets": [17], "resolves_relations": [],
                  "basis_sources": ["s-101", "s-205"], "effective": last_month, "pending_reason": None}],
   "after": {"records": {"17": {"stored_status": "superseded", "termination_reason": "StateChanged", "ended": "known",
                                "note": "结束时间取 effective：2026 年 8 月，精度为月"},
                         "new": {"stored_status": "active", "update": JUDGED}},
             "recall": {"current": ["new（上海）"], "history": ["17（北京，至 2026-08，月精度）"]}},
   "must_not": ["不得标为 Correction（用户没有说以前说错了）",
                "结束时间不得回填为接收日 2026-09-10，也不得把月精度补成具体日期",
                "北京不得从历史中消失"]},
  boundary_with="w4_02_correction",
  rejected=[{"action": "correction", "why": "原文是搬家，旧值当时为真"}, {"action": "coexist", "why": "同一主体同一关系的单值状态，新旧不能同时为当前"}])

# ---------------- W02 纠错
t = "之前说错了，我住在杭州，不是苏州。"
sample("w4_02_correction", "纠错 correction：旧值从来不对",
  {"s-102": S("我住在苏州。", "2026-04-01"), "s-230": S(t, "2026-09-12")},
  [incand(cand("self", "lives_in", t, "杭州"), "s-230", "2026-09-12")],
  [rec(21, cand("self", "lives_in", "我住在苏州。", "苏州"), "s-102", "2026-04-01")],
  {"decisions": [{"candidate": 0, "status": "decided", "action": "correction", "targets": [21], "resolves_relations": [],
                  "basis_sources": ["s-102", "s-230"], "effective": None, "pending_reason": None}],
   "after": {"records": {"21": {"stored_status": "superseded", "termination_reason": "Correction", "ended": "end_unknown"},
                         "new": {"stored_status": "active", "update": JUDGED}},
             "recall": {"current": ["new（杭州）"], "history": [], "audit_only": ["21（苏州，被纠正）"]}},
   "must_not": ["不得标为 StateChanged", "苏州不得作为“曾经住过”的历史事实出现（不变量 7）",
                "不得删除 s-102 原文（纠错不是删除）"]},
  boundary_with="w4_01_state_change",
  rejected=[{"action": "state_change", "why": "“说错了”“不是苏州”表明旧值从未成立，不是现实变化"}])

# ---------------- W03 整体细化
t = "更准确地说，我每天早上跑步五公里。"
sample("w4_03_refine_whole", "细化 refine：新断言覆盖并修订整个旧断言",
  {"s-110": S("我每天早上都跑步。", "2026-05-06"), "s-240": S(t, "2026-09-15")},
  [incand(cand("self", "fact", t, times=[recurring("每天早上")]), "s-240", "2026-09-15")],
  [rec(30, cand("self", "fact", "我每天早上都跑步。", times=[recurring("每天早上")]), "s-110", "2026-05-06")],
  {"decisions": [{"candidate": 0, "status": "decided", "action": "refine", "targets": [30], "resolves_relations": [],
                  "basis_sources": ["s-110", "s-240"], "effective": None, "pending_reason": None}],
   "after": {"records": {"30": {"stored_status": "superseded", "termination_reason": "Refinement", "ended": "end_unknown"},
                         "new": {"stored_status": "active", "update": SCOPE_PENDING}},
             "recall": {"current": ["new（每天早上跑五公里，带 update.pending）"], "history": ["30 作为被细化的旧版本"]}},
   "must_not": ["不得判为 reinforce（新句增加了距离，不只是重复）", "不得保留 30 为并列的当前记录，造成同一习惯两条当前断言"]},
  coverage=PARTIAL, boundary_with="w4_04_coexist_partial",
  note="通用 fact 只按本次全文检索取候选，范围不完整；对已明确定位的目标 30，细化仍可执行，但新记录的更新状态保留 pending/update_scope_incomplete。“每天早上”为周期时间，保留原文，换算状态 unknown，不生成连续日期区间。",
  rejected=[{"action": "reinforce", "why": "新断言更具体，不是同一断言的重复"},
            {"action": "coexist", "why": "新句以“更准确地说”修订同一个整体习惯，没有新的限定条件"}])

# ---------------- W04 局部补充（并存）
t = "周末的时候我更喜欢喝手冲咖啡。"
sample("w4_04_coexist_partial", "局部补充 coexist：只在新条件下成立，不结束较一般的旧事实",
  {"s-111": S("我喜欢喝咖啡。", "2026-05-06"), "s-241": S(t, "2026-09-15")},
  [incand(cand("self", "fact", t, memory_type="preference", conditions=["周末的时候"]), "s-241", "2026-09-15")],
  [rec(31, cand("self", "fact", "我喜欢喝咖啡。", memory_type="preference"), "s-111", "2026-05-06")],
  {"decisions": [{"candidate": 0, "status": "decided", "action": "coexist", "targets": [], "resolves_relations": [],
                  "basis_sources": ["s-111", "s-241"], "effective": None, "pending_reason": None}],
   "after": {"records": {"31": {"stored_status": "active", "termination_reason": None, "ended": "not_ended"},
                         "new": {"stored_status": "active", "update": SCOPE_PENDING, "conditions": ["周末的时候"]}},
             "recall": {"current": ["31（喜欢喝咖啡）", "new（周末更喜欢手冲，带条件）"]}},
   "must_not": ["不得结束 31（现有 Refine 会结束旧版本，这里必须避免）", "新记录不得丢掉“周末的时候”条件"]},
  coverage=PARTIAL, boundary_with="w4_03_refine_whole",
  note="通用 fact 范围不完整：并存决定成立，但不能据此确认没有其他相关偏好，新记录更新状态为 pending/update_scope_incomplete。",
  rejected=[{"action": "refine", "why": "新断言只覆盖周末，不能替代无条件的旧偏好"},
            {"action": "conflict", "why": "两者不互斥"}])

# ---------------- W05 撤回（无新值）
t = "我之前说每周三晚上去游泳，那句不对，收回。"
sample("w4_05_retract", "撤回 retract：用户收回旧断言，没有给出新值",
  {"s-120": S("我每周三晚上去游泳。", "2026-06-01"), "s-250": S(t, "2026-09-18")},
  [incand(cand("self", "fact", t, kind="retraction", times=[recurring("每周三晚上")]), "s-250", "2026-09-18")],
  [rec(40, cand("self", "fact", "我每周三晚上去游泳。", times=[recurring("每周三晚上")]), "s-120", "2026-06-01")],
  {"decisions": [{"candidate": 0, "status": "decided", "action": "retract", "targets": [40], "resolves_relations": [],
                  "basis_sources": ["s-120", "s-250"], "effective": None, "pending_reason": None}],
   "after": {"records": {"40": {"stored_status": "superseded", "termination_reason": "UserRetracted", "ended": "end_unknown"}},
             "new_records": 0, "candidate_update": SCOPE_PENDING,
             "recall": {"current": [], "history": [], "audit_only": ["40（已撤回）"]}},
   "must_not": ["不得新增一条断言记录（撤回没有值）", "不得删除 s-120 原文或索引（撤回不是 memory_forget）",
                "不得把“周三游泳”作为历史事实返回"]},
  coverage=PARTIAL, boundary_with="w4_06_retract_target_unknown",
  note="目标 40 已在输入中逐条比对且有双方依据，撤回可执行；范围不完整，回执中的更新状态保留 pending/update_scope_incomplete。“每周三晚上”为周期时间，换算状态 unknown。",
  rejected=[{"action": "state_change", "why": "用户说“那句不对”，不是现实从游泳变成不游泳"},
            {"action": "correction", "why": "correction 需要新值；没有新值的收回用 retract"}])

# ---------------- W06 撤回但目标不明（不调用 W4）
t = "那个说法不算数了。"
sample("w4_06_retract_target_unknown", "撤回目标不明：代码保留 pending，不调用 W4",
  {"s-251": S(t, "2026-09-18")},
  [incand(cand("self", "fact", t, kind="retraction"), "s-251", "2026-09-18")],
  [],
  {"code_path": {"w4_called": False, "candidate_status": "pending", "memory_id": None,
                 "update": {"status": "pending", "reasons": ["update_target_unknown", "update_scope_incomplete"]}},
   "after": {"records": {}, "new_records": 0},
   "must_not": ["不得调用 W4", "不得结束或修改任何现有记录", "不得作为有值断言提交",
                "不得把“没检索到”当作“没有旧记录”而丢弃该撤回"]},
  coverage=PARTIAL, call_w4=False, boundary_with="w4_05_retract",
  note="contracts 2.4：retraction 找不到目标时由代码保留 pending/target_unknown，不调用 W4；通用 fact 范围不完整，原因中同时含 update_scope_incomplete（contracts 1.6：范围不完整必含该原因）。")

# ---------------- W07 真实冲突
t = "我又查到栖风读书机的屏幕是8英寸，和之前记的7英寸对不上，还没核实哪个对。"
sample("w4_07_conflict", "真实冲突 conflict：双方都有依据，均不能作为唯一当前值",
  {"s-130": S("我已确认栖风读书机的屏幕尺寸是7英寸。", "2026-07-01"), "s-260": S(t, "2026-09-20")},
  [incand(cand("device_qifeng_reader", "fact", t, uncertainty={"quote": Q("还没核实哪个对"), "resolved": True}),
          "s-260", "2026-09-20")],
  [rec(50, cand("device_qifeng_reader", "fact", "我已确认栖风读书机的屏幕尺寸是7英寸。"), "s-130", "2026-07-01")],
  {"decisions": [{"candidate": 0, "status": "decided", "action": "conflict", "targets": [50], "resolves_relations": [],
                  "basis_sources": ["s-130", "s-260"], "effective": None, "pending_reason": None}],
   "after": {"records": {"50": {"stored_status": "active", "termination_reason": None, "ended": "not_ended", "open_relations": 1},
                         "new": {"stored_status": "active", "update": SCOPE_PENDING, "open_relations": 1}},
             "relations": [{"kind": "conflict", "members": [50, "new"], "status": "open"}],
             "recall": {"current": [], "conflicts": ["50（7 英寸）与 new（8 英寸），关系 open"]}},
   "must_not": ["不得结束或取代 50", "不得丢弃 8 英寸或只保存为原文", "召回不得给出唯一当前值",
                "新记录不得丢掉“还没核实哪个对”的不确定表达"]},
  coverage=PARTIAL, boundary_with="w4_08_scope_incomplete_no_records",
  note="冲突由原文对 50 的明确指向和双方依据成立，不依赖“没有其他相关记录”；范围不完整，新记录更新状态保留 pending/update_scope_incomplete。contracts 2.4 列举的可在范围不完整时执行的动作只有 state_change/correction/refine/retract，未提 conflict；此处按“针对已明确定位目标、双方依据齐全”理解为可执行，待确认。",
  rejected=[{"action": "state_change", "why": "原文没有说屏幕变了"}, {"action": "correction", "why": "用户未确认 7 英寸是错的"}])

# ---------------- W08 范围不足，不是冲突（不调用 W4）
t = "我的读书灯是暖白光。"
sample("w4_08_scope_incomplete_no_records", "检索范围不足而非冲突：未检索到相关记录，追加断言并标 pending",
  {"s-270": S(t, "2026-09-22")},
  [incand(cand("self", "fact", t), "s-270", "2026-09-22")],
  [],
  {"code_path": {"w4_called": False, "candidate_status": "committed", "memory_id": "new",
                 "update": {"status": "pending", "reasons": ["update_scope_incomplete"]}},
   "after": {"records": {"new": {"stored_status": "active", "update": {"status": "pending", "reasons": ["update_scope_incomplete"]}}},
             "relations": [],
             "recall": {"current_note": "可返回该断言，但须带 update.pending，不得声称是唯一当前值"}},
   "must_not": ["不得调用 W4", "不得因检索不足创建冲突关系", "不得把 update 标为 judged"]},
  coverage={"complete": False, "omitted": 0}, call_w4=False, boundary_with="w4_07_conflict",
  note="通用关系 fact 只按本次全文检索取候选，范围不保证完整（contracts 2.4）。检索到部分记录但范围不完整、需要调用 W4 的情形在第二批补充。")
print("batch1 ok")
