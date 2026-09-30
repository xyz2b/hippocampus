# Generates tests/eval/w4/samples/*.json. The JSON files are the reviewed source of truth;
# edit here and regenerate, then re-run check_samples.py and get the changes reviewed.
import json, pathlib, sys
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from review_status import status_for  # noqa: E402
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
# A time expression that is present but has no computable interval ("现在", "最近"):
# the quote is kept with interp unknown, never dropped (contracts 1.2/1.3).
def vague(q, role="validity"): return {"role": role, "quote": Q(q), "interp": {"kind": "unknown"}, "anchor": "said_on"}
UNRESOLVABLE = ("recurring", "unknown")
def rec(id, c, sid, said_on, status="active", reason=None, ended="not_ended", history=(), rel=(), update=JUDGED,
        time_resolutions=None):
    tr = time_resolutions if time_resolutions is not None else [UNKNOWN_RES for t in c["times"] if t["interp"]["kind"] in UNRESOLVABLE]
    return {"format": "current", "id": id, "stored_status": status, "termination_reason": reason, "ended": ended,
            "candidate": c, "evidence": [ev(sid, c["sentence"]["text"])], "grade": "committed", "said_on": said_on,
            "time_resolutions": tr, "history": list(history), "open_relations": list(rel), "update": update}
def incand(c, sid, said_on, index=0, committed_id=None, time_resolutions=None):
    tr = list(time_resolutions) if time_resolutions is not None else [UNKNOWN_RES for t in c["times"] if t["interp"]["kind"] in UNRESOLVABLE]
    return {"index": index, "committed_id": committed_id, "candidate": c,
            "source": {"kind": "stored", "source_id": sid}, "said_on": said_on, "time_resolutions": tr}
SCOPE_PENDING = {"status": "pending", "reasons": ["update_scope_incomplete"]}
PARTIAL = {"complete": False, "omitted": 0}
def sample(id, focus, sources, candidates, records, expect, relations=(), coverage=None, call_w4=True,
           boundary_with=None, rejected=(), note=None, batch=1):
    d = {"id": id, "contract": "w4.v1", "batch": batch, "label_status": None, "focus": focus,
         "boundary_with": boundary_with, "sources": sources,
         "input": {"candidates": candidates, "records": records, "relations": list(relations),
                   "coverage": coverage or {"complete": True, "omitted": 0}},
         "call_w4": call_w4, "expect": expect, "rejected_alternatives": list(rejected)}
    if note: d["note"] = note
    # Review status is never generated: a sample stays `reviewed` only if its
    # content (everything except label_status) is identical to the reviewed
    # JSON already on disk; anything new or changed is written as `draft`.
    d["qualifier_marks"] = MARKS.pop(id, {})
    path = OUT / f"{id}.json"
    d["label_status"] = status_for(d, json.loads(path.read_text(encoding="utf-8")) if path.exists() else None)
    path.write_text(json.dumps(d, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

def S(text, said_on): return {"text": text, "said_on": said_on}
# Reviewer marks, bound to each assertion ("candidate:<index>" / "record:<id>"):
# the time, condition, uncertainty and strength expressions of that assertion.
# One sentence may carry qualifiers of different assertions ("我上个月…，妹妹去年…"),
# so marks are per assertion, not per source. check_samples.py requires each
# candidate/record to keep exactly its marks. Set just before each sample().
MARKS = {}

# ---------------- W01 现实变化（有生效时间）
t = "我上个月从北京搬到上海了。"
last_month = {"role": "event", "quote": Q("上个月"), "interp": {"kind": "relative", "unit": "month", "offset": -1}, "anchor": "said_on"}
MARKS["w4_01_state_change"] = {'candidate:0': {'times': ['上个月']}}
sample("w4_01_state_change", "现实变化 state_change：旧值曾经成立，现在变了",
  {"s-101": S("我住在北京。", "2026-03-02"), "s-205": S(t, "2026-09-10")},
  [incand(cand("self", "lives_in", t, "上海", times=[last_month]), "s-205", "2026-09-10",
          time_resolutions=[{"status": "resolved", "start": 1785513600, "end_exclusive": 1788192000, "precision": "month"}])],
  [rec(17, cand("self", "lives_in", "我住在北京。", "北京"), "s-101", "2026-03-02")],
  {"decisions": [{"candidate": 0, "status": "decided", "action": "state_change", "targets": [17], "resolves_relations": [],
                  "basis_sources": ["s-101", "s-205"], "effective": last_month, "pending_reason": None}],
   "after": {"records": {"17": {"stored_status": "superseded", "termination_reason": "StateChanged", "ended": "known",
                                "note": "结束时间取 effective：2026 年 8 月，精度为月"},
                         "new": {"from_candidate": 0, "history": [17], "stored_status": "active", "update": JUDGED}},
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
                         "new": {"from_candidate": 0, "history": [21], "stored_status": "active", "update": JUDGED}},
             "recall": {"current": ["new（杭州）"], "history": [], "audit_only": ["21（苏州，被纠正）"]}},
   "must_not": ["不得标为 StateChanged", "苏州不得作为“曾经住过”的历史事实出现（不变量 7）",
                "不得删除 s-102 原文（纠错不是删除）"]},
  boundary_with="w4_01_state_change",
  rejected=[{"action": "state_change", "why": "“说错了”“不是苏州”表明旧值从未成立，不是现实变化"}])

# ---------------- W03 整体细化
t = "更准确地说，我每天早上跑步五公里。"
MARKS["w4_03_refine_whole"] = {'candidate:0': {'times': ['每天早上']}, 'record:30': {'times': ['每天早上']}}
sample("w4_03_refine_whole", "细化 refine：新断言覆盖并修订整个旧断言",
  {"s-110": S("我每天早上都跑步。", "2026-05-06"), "s-240": S(t, "2026-09-15")},
  [incand(cand("self", "fact", t, times=[recurring("每天早上")]), "s-240", "2026-09-15")],
  [rec(30, cand("self", "fact", "我每天早上都跑步。", times=[recurring("每天早上")]), "s-110", "2026-05-06")],
  {"decisions": [{"candidate": 0, "status": "decided", "action": "refine", "targets": [30], "resolves_relations": [],
                  "basis_sources": ["s-110", "s-240"], "effective": None, "pending_reason": None}],
   "after": {"records": {"30": {"stored_status": "superseded", "termination_reason": "Refinement", "ended": "end_unknown"},
                         "new": {"from_candidate": 0, "history": [30], "stored_status": "active", "update": SCOPE_PENDING}},
             "recall": {"current": ["new（每天早上跑五公里，带 update.pending）"], "history": ["30 作为被细化的旧版本"]}},
   "must_not": ["不得判为 reinforce（新句增加了距离，不只是重复）", "不得保留 30 为并列的当前记录，造成同一习惯两条当前断言"]},
  coverage=PARTIAL, boundary_with="w4_04_coexist_partial",
  note="通用 fact 只按本次全文检索取候选，范围不完整；对已明确定位的目标 30，细化仍可执行，但新记录的更新状态保留 pending/update_scope_incomplete。“每天早上”为周期时间，保留原文，换算状态 unknown，不生成连续日期区间。",
  rejected=[{"action": "reinforce", "why": "新断言更具体，不是同一断言的重复"},
            {"action": "coexist", "why": "新句以“更准确地说”修订同一个整体习惯，没有新的限定条件"}])

# ---------------- W04 局部补充（并存）
t = "周末的时候我更喜欢喝手冲咖啡。"
MARKS["w4_04_coexist_partial"] = {'candidate:0': {'conditions': ['周末的时候']}}
sample("w4_04_coexist_partial", "局部补充 coexist：只在新条件下成立，不结束较一般的旧事实",
  {"s-111": S("我喜欢喝咖啡。", "2026-05-06"), "s-241": S(t, "2026-09-15")},
  [incand(cand("self", "fact", t, memory_type="preference", conditions=["周末的时候"]), "s-241", "2026-09-15")],
  [rec(31, cand("self", "fact", "我喜欢喝咖啡。", memory_type="preference"), "s-111", "2026-05-06")],
  {"decisions": [{"candidate": 0, "status": "decided", "action": "coexist", "targets": [], "resolves_relations": [],
                  "basis_sources": ["s-111", "s-241"], "effective": None, "pending_reason": None}],
   "after": {"records": {"31": {"stored_status": "active", "termination_reason": None, "ended": "not_ended"},
                         "new": {"from_candidate": 0, "stored_status": "active", "update": SCOPE_PENDING, "conditions": ["周末的时候"]}},
             "recall": {"current": ["31（喜欢喝咖啡）", "new（周末更喜欢手冲，带条件）"]}},
   "must_not": ["不得结束 31（现有 Refine 会结束旧版本，这里必须避免）", "新记录不得丢掉“周末的时候”条件"]},
  coverage=PARTIAL, boundary_with="w4_03_refine_whole",
  note="通用 fact 范围不完整：并存决定成立，但不能据此确认没有其他相关偏好，新记录更新状态为 pending/update_scope_incomplete。",
  rejected=[{"action": "refine", "why": "新断言只覆盖周末，不能替代无条件的旧偏好"},
            {"action": "conflict", "why": "两者不互斥"}])

# ---------------- W05 撤回（无新值）
t = "我之前说每周三晚上去游泳，那句不对，收回。"
MARKS["w4_05_retract"] = {'candidate:0': {'times': ['每周三晚上']}, 'record:40': {'times': ['每周三晚上']}}
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
MARKS["w4_07_conflict"] = {'candidate:0': {'uncertainty': ['还没核实哪个对']}}
sample("w4_07_conflict", "真实冲突 conflict：双方都有依据，均不能作为唯一当前值",
  {"s-130": S("我已确认栖风读书机的屏幕尺寸是7英寸。", "2026-07-01"), "s-260": S(t, "2026-09-20")},
  [incand(cand("device_qifeng_reader", "fact", t, uncertainty={"quote": Q("还没核实哪个对"), "resolved": True}),
          "s-260", "2026-09-20")],
  [rec(50, cand("device_qifeng_reader", "fact", "我已确认栖风读书机的屏幕尺寸是7英寸。"), "s-130", "2026-07-01")],
  {"decisions": [{"candidate": 0, "status": "decided", "action": "conflict", "targets": [50], "resolves_relations": [],
                  "basis_sources": ["s-130", "s-260"], "effective": None, "pending_reason": None}],
   "after": {"records": {"50": {"stored_status": "active", "termination_reason": None, "ended": "not_ended", "open_relations": 1},
                         "new": {"from_candidate": 0, "stored_status": "active", "update": SCOPE_PENDING, "open_relations": 1}},
             "relations": [{"kind": "conflict", "members": [50, "new"], "status": "open"}],
             "recall": {"current": [], "conflicts": ["50（7 英寸）与 new（8 英寸），关系 open"]}},
   "must_not": ["不得结束或取代 50", "不得丢弃 8 英寸或只保存为原文", "召回不得给出唯一当前值",
                "新记录不得丢掉“还没核实哪个对”的不确定表达"]},
  coverage=PARTIAL, boundary_with="w4_08_scope_incomplete_no_records",
  note="冲突由原文对 50 的明确指向和双方依据成立，不依赖“没有其他相关记录”；范围不完整，新记录更新状态保留 pending/update_scope_incomplete。contracts 2.4 列举的可在范围不完整时执行的动作只有 state_change/correction/refine/retract，未提 conflict；已确认：针对已明确定位的目标、新旧双方依据齐全的冲突，不依赖检索覆盖全部记录，可以保存（关系 open，旧记录不结束）。",
  rejected=[{"action": "state_change", "why": "原文没有说屏幕变了"}, {"action": "correction", "why": "用户未确认 7 英寸是错的"}])

# ---------------- W08 范围不足，不是冲突（不调用 W4）
t = "我的读书灯是暖白光。"
sample("w4_08_scope_incomplete_no_records", "检索范围不足而非冲突：未检索到相关记录，追加断言并标 pending",
  {"s-270": S(t, "2026-09-22")},
  [incand(cand("self", "fact", t), "s-270", "2026-09-22")],
  [],
  {"code_path": {"w4_called": False, "candidate_status": "committed", "memory_id": "new",
                 "update": {"status": "pending", "reasons": ["update_scope_incomplete"]}},
   "after": {"records": {"new": {"from_candidate": 0, "stored_status": "active", "update": {"status": "pending", "reasons": ["update_scope_incomplete"]}}},
             "relations": [],
             "recall": {"current_note": "可返回该断言，但须带 update.pending，不得声称是唯一当前值"}},
   "must_not": ["不得调用 W4", "不得因检索不足创建冲突关系", "不得把 update 标为 judged"]},
  coverage={"complete": False, "omitted": 0}, call_w4=False, boundary_with="w4_07_conflict",
  note="通用关系 fact 只按本次全文检索取候选，范围不保证完整（contracts 2.4）。检索到部分记录但范围不完整、需要调用 W4 的情形在第二批补充。")
print("batch1 ok")

# ======================= 第二批 =======================
COMPLETE = {"complete": True, "omitted": 0}

# ---------------- W09 新增（检索到同主体记录，但关系无关）
t = "我在字节跳动工作。"
sample("w4_09_new", "新增 new：检索到同主体的其他记录，但与新断言无关",
  {"s-301": S("我住在北京。", "2026-03-02"), "s-310": S(t, "2026-09-23")},
  [incand(cand("self", "works_at", t, "字节跳动"), "s-310", "2026-09-23")],
  [rec(60, cand("self", "lives_in", "我住在北京。", "北京"), "s-301", "2026-03-02")],
  {"decisions": [{"candidate": 0, "status": "decided", "action": "new", "targets": [], "resolves_relations": [],
                  "basis_sources": ["s-310"], "effective": None, "pending_reason": None}],
   "after": {"records": {"60": {"stored_status": "active", "ended": "not_ended"},
                         "new": {"from_candidate": 0, "stored_status": "active", "update": JUDGED}},
             "recall": {"current": ["60（北京）", "new（字节跳动）"]}},
   "must_not": ["不得改动 60", "不得因同一主体就判为 refine 或 state_change"]},
  coverage=COMPLETE, batch=2, boundary_with="w4_10_reinforce",
  rejected=[{"action": "coexist", "why": "coexist 用于同一关系在不同条件下并列；这里关系不同，直接新增"}],
  note="works_at 为目录内专用关系，同关系当前记录已全部取得（本例没有），范围完整。")

# ---------------- W10 强化
t = "我还是喜欢喝咖啡。"
sample("w4_10_reinforce", "强化 reinforce：独立的新原文重复同一断言",
  {"s-111": S("我喜欢喝咖啡。", "2026-05-06"), "s-320": S(t, "2026-09-24")},
  [incand(cand("self", "fact", t, memory_type="preference"), "s-320", "2026-09-24")],
  [rec(31, cand("self", "fact", "我喜欢喝咖啡。", memory_type="preference"), "s-111", "2026-05-06")],
  {"decisions": [{"candidate": 0, "status": "decided", "action": "reinforce", "targets": [31], "resolves_relations": [],
                  "basis_sources": ["s-111", "s-320"], "effective": None, "pending_reason": None}],
   "after": {"records": {"31": {"stored_status": "active", "reinforcement_count": "+1", "evidence_added": ["s-320"]}},
             "new_records": 0, "candidate_update": SCOPE_PENDING,
             "recall": {"current": ["31（喜欢喝咖啡，两条原文依据）"]}},
   "must_not": ["不得新增一条重复记录", "同一来源重放不得再次增加计数",
                "新旧任一方带强度表达（如“很喜欢”“有点喜欢”）而另一方不同或没有时，不得按同一断言强化（contracts 1.3），应按 w4_11 判断"]},
  coverage=PARTIAL, batch=2, boundary_with="w4_11_preference_strength",
  rejected=[{"action": "new", "why": "与 31 是同一偏好"}, {"action": "state_change", "why": "偏好没有变化"}])

# ---------------- W11 偏好强度变化
t = "我现在特别喜欢爵士乐。"
old_t = "我有点喜欢爵士乐。"
MARKS["w4_11_preference_strength"] = {'candidate:0': {'times': ['现在'], 'strength': ['特别喜欢']}, 'record:41': {'strength': ['有点喜欢']}}
sample("w4_11_preference_strength", "偏好强度变化 state_change（PreferenceChanged）：弱→强",
  {"s-121": S(old_t, "2026-04-10"), "s-330": S(t, "2026-09-24")},
  [incand(cand("self", "fact", t, memory_type="preference", strength="strong", times=[vague("现在")]), "s-330", "2026-09-24")],
  [rec(41, cand("self", "fact", old_t, memory_type="preference", strength="weak"), "s-121", "2026-04-10")],
  {"decisions": [{"candidate": 0, "status": "decided", "action": "state_change", "targets": [41], "resolves_relations": [],
                  "basis_sources": ["s-121", "s-330"], "effective": None, "pending_reason": None}],
   "after": {"records": {"41": {"stored_status": "superseded", "termination_reason": "PreferenceChanged", "ended": "end_unknown"},
                         "new": {"from_candidate": 0, "history": [41], "stored_status": "active", "strength": "strong", "update": SCOPE_PENDING}},
             "recall": {"current": ["new（特别喜欢，strong）"], "history": ["41（有点喜欢，weak）"]}},
   "must_not": ["不得判为 reinforce 而把强度差异吞掉（contracts 7：旧强度原样保留，不能按同一断言强化）",
                "结束时间不得回填为接收日"]},
  coverage=PARTIAL, batch=2, boundary_with="w4_10_reinforce",
  rejected=[{"action": "reinforce", "why": "强度由 weak 变为 strong，不是同一断言"}])

# ---------------- W12 多目标
t = "我已经搬到深圳了。"
sample("w4_12_multi_target", "多目标 state_change：两条旧的住址记录一起结束，只插入一个新版本",
  {"s-302": S("我住在北京。", "2026-02-01"), "s-303": S("我家在北京朝阳区。", "2026-05-01"),
   "s-340": S(t, "2026-09-25")},
  [incand(cand("self", "lives_in", t, "深圳"), "s-340", "2026-09-25")],
  [rec(62, cand("self", "lives_in", "我住在北京。", "北京"), "s-302", "2026-02-01"),
   rec(63, cand("self", "lives_in", "我家在北京朝阳区。", "北京朝阳区"), "s-303", "2026-05-01")],
  {"decisions": [{"candidate": 0, "status": "decided", "action": "state_change", "targets": [62, 63], "resolves_relations": [],
                  "basis_sources": ["s-302", "s-303", "s-340"], "effective": None, "pending_reason": None}],
   "after": {"records": {"62": {"stored_status": "superseded", "termination_reason": "StateChanged", "ended": "end_unknown"},
                         "63": {"stored_status": "superseded", "termination_reason": "StateChanged", "ended": "end_unknown"},
                         "new": {"from_candidate": 0, "history": [62, 63], "stored_status": "active", "update": JUDGED}},
             "new_records": 1,
             "recall": {"current": ["new（深圳）"], "history": ["62（北京）", "63（北京朝阳区）"]}},
   "must_not": ["不得只结束其中一条", "不得插入两个新版本（W5：最多插入一次）",
                "任一目标在提交前已变化时整组 needs_rejudge，不得部分成功（W5 代码验收项）"]},
  coverage=COMPLETE, batch=2,
  note="lives_in 为目录内关系，同关系当前记录全部取得，范围完整。")

# ---------------- W13 现实变化，日期未知
t = "我换工作了，现在在腾讯。"
MARKS["w4_13_state_change_date_unknown"] = {'candidate:0': {'times': ['现在']}}
sample("w4_13_state_change_date_unknown", "现实变化、没有生效时间：旧记录结束时间未知",
  {"s-304": S("我在字节跳动工作。", "2026-01-15"), "s-350": S(t, "2026-09-25")},
  [incand(cand("self", "works_at", t, "腾讯", times=[vague("现在")]), "s-350", "2026-09-25")],
  [rec(64, cand("self", "works_at", "我在字节跳动工作。", "字节跳动"), "s-304", "2026-01-15")],
  {"decisions": [{"candidate": 0, "status": "decided", "action": "state_change", "targets": [64], "resolves_relations": [],
                  "basis_sources": ["s-304", "s-350"], "effective": None, "pending_reason": None}],
   "after": {"records": {"64": {"stored_status": "superseded", "termination_reason": "StateChanged", "ended": "end_unknown",
                                "valid_to": None},
                         "new": {"from_candidate": 0, "history": [64], "stored_status": "active", "update": JUDGED}},
             "recall": {"current": ["new（腾讯）"], "history": ["64（字节跳动，结束时间未知）"]}},
   "must_not": ["结束时间不得回填为接收日 2026-09-25（W5）",
                "历史查询中不得把 64 当作一直有效（contracts 2.5 end_unknown）"]},
  coverage=COMPLETE, batch=2, boundary_with="w4_01_state_change")

# ---------------- W14 已提交断言的重判
t = "我现在每天喝两杯咖啡。"
MARKS["w4_14_rejudge_committed"] = {'candidate:0': {'times': ['现在', '每天']}, 'record:55': {'times': ['每天']}}
sample("w4_14_rejudge_committed", "重判已提交的断言：committed_id 非空，不重复插入",
  {"s-112": S("我每天喝一杯咖啡。", "2026-05-06"), "s-360": S(t, "2026-09-20")},
  [incand(cand("self", "fact", t, times=[vague("现在"), recurring("每天")]), "s-360", "2026-09-20", committed_id=70)],
  [rec(55, cand("self", "fact", "我每天喝一杯咖啡。", times=[recurring("每天")]), "s-112", "2026-05-06")],
  {"decisions": [{"candidate": 0, "status": "decided", "action": "state_change", "targets": [55], "resolves_relations": [],
                  "basis_sources": ["s-112", "s-360"], "effective": None, "pending_reason": None}],
   "after": {"records": {"55": {"stored_status": "superseded", "termination_reason": "StateChanged", "ended": "end_unknown"},
                         "70": {"from_candidate": 0, "history": [55], "stored_status": "active", "update": SCOPE_PENDING}},
             "new_records": 0,
             "recall": {"current": ["70（每天两杯）"], "history": ["55（每天一杯）"]}},
   "must_not": ["不得再插入一条“每天两杯”", "不得把 70 与自身比较或当作一次强化",
                "通用 fact 范围仍不完整，70 的 pending/update_scope_incomplete 不得因本次重判清除"]},
  coverage=PARTIAL, batch=2,
  note="70 此前已提交且 update.pending；本次后台检索到旧记录 55 后显式重判。代码不把 70 放进 records（候选不与自身比较）。")

# ---------------- W15 已有冲突的解决（撤回其中一方）
t = "我核实过了，栖风读书机屏幕7英寸那份是记错的。"
t2 = "另一份资料写栖风读书机的屏幕是8英寸，我还没核实哪份对。"
link = {"id": "rel-1", "kind": "conflict", "members": [50, 51], "conditions": [], "times": [],
        "basis": [ev("s-130", "我已确认栖风读书机的屏幕尺寸是7英寸。"), ev("s-261", t2)], "status": "open", "resolution": None}
MARKS["w4_15_resolve_conflict"] = {'record:51': {'uncertainty': ['我还没核实哪份对']}}
sample("w4_15_resolve_conflict", "解决已有冲突：用户确认 7 英寸记错，撤回该成员并解决关系",
  {"s-130": S("我已确认栖风读书机的屏幕尺寸是7英寸。", "2026-07-01"), "s-261": S(t2, "2026-09-20"),
   "s-370": S(t, "2026-09-26")},
  [incand(cand("device_qifeng_reader", "fact", t, kind="retraction"), "s-370", "2026-09-26")],
  [rec(50, cand("device_qifeng_reader", "fact", "我已确认栖风读书机的屏幕尺寸是7英寸。"), "s-130", "2026-07-01", rel=["rel-1"]),
   rec(51, cand("device_qifeng_reader", "fact", t2, uncertainty={"quote": Q("我还没核实哪份对"), "resolved": True}),
       "s-261", "2026-09-20", rel=["rel-1"], update=SCOPE_PENDING)],
  {"decisions": [{"candidate": 0, "status": "decided", "action": "retract", "targets": [50], "resolves_relations": ["rel-1"],
                  "basis_sources": ["s-130", "s-370"], "effective": None, "pending_reason": None}],
   "after": {"records": {"50": {"stored_status": "superseded", "termination_reason": "UserRetracted", "ended": "end_unknown"},
                         "51": {"stored_status": "active", "note": "不改动；其“还没核实”的原话照样保留"}},
             "relations": [{"id": "rel-1", "status": "resolved", "resolution_source": "s-370", "survivors": [51]}],
             "new_records": 0, "candidate_update": SCOPE_PENDING,
             "recall": {"current": ["51（8 英寸，带原话中的“还没核实”）"], "conflicts": []}},
   "must_not": ["不得由代码在看到一次撤回后自动推断关系已解决——必须是 W4 明确给出 resolves_relations",
                "不得把 51 改写成“已确认 8 英寸”：用户只确认了 7 英寸是错的",
                "不得删除 s-130 原文"]},
  relations=[link], coverage=PARTIAL, batch=2,
  note="用户确认的是“7 英寸记错了”，没有给出新值，按撤回处理（与 w4_05 一致）。关系解决后只有 51 保留当前资格，50 由撤回结束；不能只关闭关系而让双方都作为当前结论。51 自带的“还没核实”不因此变成已确认。用户同时确认“是 8 英寸”的情形见 w4_21。")

# ---------------- W16 W4 返回 pending：结论依赖“没有其他记录”
t = "我现在只用这一台笔记本电脑了。"
MARKS["w4_16_pending_scope_incomplete"] = {'candidate:0': {'times': ['现在']}}
sample("w4_16_pending_scope_incomplete", "W4 返回 pending/scope_incomplete：结论依赖检索未覆盖的记录",
  {"s-305": S("我用的是一台 MacBook Air。", "2026-06-01"), "s-380": S(t, "2026-09-26")},
  [incand(cand("self", "fact", t, times=[vague("现在")]), "s-380", "2026-09-26")],
  [rec(81, cand("self", "fact", "我用的是一台 MacBook Air。"), "s-305", "2026-06-01")],
  {"decisions": [{"candidate": 0, "status": "pending", "action": None, "targets": [], "resolves_relations": [],
                  "basis_sources": ["s-380"], "effective": None, "pending_reason": "scope_incomplete"}],
   "after": {"records": {"81": {"stored_status": "active", "ended": "not_ended"},
                         "new": {"from_candidate": 0, "stored_status": "active", "update": SCOPE_PENDING}},
             "recall": {"current_note": "可返回新断言，但带 update.pending，不得据此结束未检索到的其他设备记录"}},
   "must_not": ["不得结束任何未送入 W4 的记录", "不得把 81 以外的设备推断为已停用",
                "不得改判为 conflict"]},
  coverage=PARTIAL, batch=2, boundary_with="w4_08_scope_incomplete_no_records",
  note="“只用这一台”意味着其他设备已不再使用，但检索范围不完整，这一结论不能成立；W3 支持的断言本身仍可提交（contracts 2.4）。")

# ---------------- W17 W4 返回 pending：新旧关系不明
t = "我最近在学韩语。"
MARKS["w4_17_pending_unclear_relation"] = {'candidate:0': {'times': ['最近']}}
sample("w4_17_pending_unclear_relation", "W4 返回 pending/unclear_relation：可能改学、也可能同时在学",
  {"s-306": S("我在学日语。", "2026-03-20"), "s-390": S(t, "2026-09-27")},
  [incand(cand("self", "fact", t, times=[vague("最近")]), "s-390", "2026-09-27")],
  [rec(90, cand("self", "fact", "我在学日语。"), "s-306", "2026-03-20")],
  {"decisions": [{"candidate": 0, "status": "pending", "action": None, "targets": [90], "resolves_relations": [],
                  "basis_sources": ["s-306", "s-390"], "effective": None, "pending_reason": "unclear_relation"}],
   "after": {"records": {"90": {"stored_status": "active", "ended": "not_ended"},
                         "new": {"from_candidate": 0, "stored_status": "active",
                                 "update": {"status": "pending", "reasons": ["update_relation_unclear", "update_scope_incomplete"]}}},
             "recall": {"current": ["90（学日语）", "new（学韩语）"], "note": "两条都带待判定标注，不断言已放弃日语"}},
   "must_not": ["不得结束 90", "不得判为 conflict（两者并不互斥）", "不得判为 coexist 并标 judged 而掩盖关系不明"]},
  coverage=PARTIAL, batch=2,
  rejected=[{"action": "state_change", "why": "原文没说不学日语了"}, {"action": "coexist", "why": "也没说两门同时在学，关系不能确定"}])

# ---------------- W18 legacy 旧记录缺字段
t = "我搬到成都了。"
# LegacyMemory = the serialized src/model.rs Memory from before phase 1, complete and
# decodable (checked by tests/w4_legacy_fixture.rs). Its only evidence is a value
# fragment whose session/turn no longer link to a stored source, so the outer
# evidence is [] and the gaps say why.
legacy = {"format": "legacy", "id": 95,
          "data": {"id": 95, "owner_id": "user-1",
                   "evidence": [{"id": "ev-legacy-95", "session_id": "legacy-session-7", "turn_id": "legacy-turn-3",
                                 "quote": "广州"}],
                   "subject_id": "self", "memory_type": "Fact", "predicate": "lives_in", "value": {"Text": "广州"},
                   "polarity": None, "strength": None, "scope": {"domain": None, "conditions": []},
                   "retention_tier": "LongTerm", "status": "Active",
                   "temporal": {"observed_at": 1762099200, "valid_from": None, "valid_to": None, "expires_at": None,
                                "reference": None},
                   "extraction_confidence": "High", "certainty": "High",
                   "supersedes": None, "superseded_by": None, "supersede_reason": None,
                   "explicit_memory_request": False, "reinforcement_count": 0, "retrieval_count": 0,
                   "last_reinforced_at": None, "last_retrieved_at": None, "tags": []},
          "evidence": [], "grade": "committed", "update": None, "gaps": ["source_missing", "sentence_missing", "qualifiers_unmapped"]}
sample("w4_18_legacy_gaps", "legacy 旧记录缺来源和限定语：不能据此结束旧记录，保持 pending",
  {"s-400": S(t, "2026-09-27")},
  [incand(cand("self", "lives_in", t, "成都"), "s-400", "2026-09-27")],
  [legacy],
  {"decisions": [{"candidate": 0, "status": "pending", "action": None, "targets": [95], "resolves_relations": [],
                  "basis_sources": ["s-400"], "effective": None, "pending_reason": "unclear_relation"}],
   "after": {"records": {"95": {"format": "legacy", "status": "Active", "note": "原样保留，不补造 Candidate、Quote 或来源"},
                         "new": {"from_candidate": 0, "stored_status": "active", "update": {"status": "pending", "reasons": ["update_relation_unclear"]}}},
             "recall": {"current": ["new（成都，带 update.pending）"], "legacy": ["95（广州，带 gaps）"]}},
   "must_not": ["不得以“缺字段等于无限定”结束 95（contracts 3.2）", "不得为 95 伪造 source_id 或完整原句",
                "不得把 95 从召回中去掉"]},
  coverage=COMPLETE, batch=2,
  note="95 只有旧数据中的片段，没有可链接原文，也缺限定语，无法核准完整断言；新断言可提交，但与 95 的关系待判定。")

# ---------------- W19 跨任务范围
t = "在潮汐展陈任务中，折页导航终端的开机显示语言改为葡萄牙语。"
old_t = "在潮汐展陈任务中，我们正在为折页导航终端配置展台演示；该任务要求它的开机显示语言设为西班牙语。"
sample("w4_19_task_scope", "任务范围：只在可信任务内判定，其他任务的记录不进入输入",
  {"s-501": S(old_t, "2026-09-28"), "s-510": S(t, "2026-09-28")},
  [incand(cand("device_zheye_terminal", "fact", t), "s-510", "2026-09-28")],
  [rec(97, cand("device_zheye_terminal", "fact", old_t), "s-501", "2026-09-28")],
  {"decisions": [{"candidate": 0, "status": "decided", "action": "state_change", "targets": [97], "resolves_relations": [],
                  "basis_sources": ["s-501", "s-510"], "effective": None, "pending_reason": None}],
   "after": {"records": {"97": {"stored_status": "superseded", "termination_reason": "StateChanged", "ended": "end_unknown"},
                         "new": {"from_candidate": 0, "history": [97], "stored_status": "active", "update": SCOPE_PENDING}},
             "untouched_other_task": ["98（高原路演任务：意大利语）不在输入中，不得改动"]},
   "must_not": ["不得把其他任务的记录送入 W4 或作为目标", "不得改动 98"]},
  coverage=PARTIAL, batch=2,
  note="可信任务 s6-tide-exhibit 由运行时绑定；代码只取该任务范围内的记录。98 属于 s6-plateau-demo，被代码排除。")

# ---------------- W20 W4 返回 pending：撤回目标不唯一
t = "我之前说的那个住处不对，收回。"
MARKS["w4_20_pending_target_unknown"] = {'record:67': {'conditions': ['周末']}}
sample("w4_20_pending_target_unknown", "W4 返回 pending/target_unknown：撤回的目标在多条记录中不确定",
  {"s-307": S("我住在北京。", "2026-02-01"), "s-308": S("我周末住在天津的房子里。", "2026-04-01"),
   "s-410": S(t, "2026-09-28")},
  [incand(cand("self", "lives_in", t, kind="retraction"), "s-410", "2026-09-28")],
  [rec(66, cand("self", "lives_in", "我住在北京。", "北京"), "s-307", "2026-02-01"),
   rec(67, cand("self", "lives_in", "我周末住在天津的房子里。", "天津", conditions=["周末"]), "s-308", "2026-04-01")],
  {"decisions": [{"candidate": 0, "status": "pending", "action": None, "targets": [], "resolves_relations": [],
                  "basis_sources": ["s-410"], "effective": None, "pending_reason": "target_unknown"}],
   "after": {"records": {"66": {"stored_status": "active"}, "67": {"stored_status": "active"}},
             "new_records": 0, "candidate_status": "pending",
             "candidate_update": {"status": "pending", "reasons": ["update_target_unknown"]}},
   "must_not": ["不得任选一条撤回", "不得同时撤回两条", "撤回不得作为有值断言提交"]},
  coverage=COMPLETE, batch=2, boundary_with="w4_06_retract_target_unknown",
  note="与 w4_06 不同：这里检索到了两条候选目标，W4 被调用，但无法确定指的是哪一条。可在后续交互中请用户澄清。")
# ---------------- W21 确认冲突一方：胜方被细化为已核实，败方撤回，关系解决
t = "我核实过了，栖风读书机的屏幕是8英寸，7英寸那份是记错的。"
t2 = "另一份资料写栖风读书机的屏幕是8英寸，我还没核实哪份对。"
t50 = "我已确认栖风读书机的屏幕尺寸是7英寸。"
link = {"id": "rel-1", "kind": "conflict", "members": [50, 51], "conditions": [], "times": [],
        "basis": [ev("s-130", t50), ev("s-261", t2)], "status": "open", "resolution": None}
MARKS["w4_21_confirm_conflict_side"] = {'record:51': {'uncertainty': ['我还没核实哪份对']}}
sample("w4_21_confirm_conflict_side", "确认冲突一方并重述其值：败方 retract 并解决关系，胜方由“未核实”细化为已核实",
  {"s-130": S(t50, "2026-07-01"), "s-261": S(t2, "2026-09-20"), "s-371": S(t, "2026-09-27")},
  [incand(cand("device_qifeng_reader", "fact", t), "s-371", "2026-09-27", index=0),
   incand(cand("device_qifeng_reader", "fact", t, kind="retraction"), "s-371", "2026-09-27", index=1)],
  [rec(50, cand("device_qifeng_reader", "fact", t50), "s-130", "2026-07-01", rel=["rel-1"]),
   rec(51, cand("device_qifeng_reader", "fact", t2, uncertainty={"quote": Q("我还没核实哪份对"), "resolved": True}),
       "s-261", "2026-09-20", rel=["rel-1"], update=SCOPE_PENDING)],
  {"decisions": [{"candidate": 0, "status": "decided", "action": "refine", "targets": [51], "resolves_relations": [],
                  "basis_sources": ["s-261", "s-371"], "effective": None, "pending_reason": None},
                 {"candidate": 1, "status": "decided", "action": "retract", "targets": [50], "resolves_relations": ["rel-1"],
                  "basis_sources": ["s-130", "s-371"], "effective": None, "pending_reason": None}],
   "after": {"records": {"50": {"stored_status": "superseded", "termination_reason": "UserRetracted", "ended": "end_unknown"},
                         "51": {"stored_status": "superseded", "termination_reason": "Refinement", "ended": "end_unknown",
                                "note": "原话“还没核实哪份对”留在历史中"},
                         "new": {"from_candidate": 0, "history": [51], "stored_status": "active", "uncertainty": None, "update": SCOPE_PENDING}},
             "relations": [{"id": "rel-1", "status": "resolved", "resolution_source": "s-371", "survivors": []}],
             "new_records": 1, "candidate_update": SCOPE_PENDING,
             "recall": {"current": ["new（8 英寸，已核实）"], "history": ["51（8 英寸，当时未核实）"], "conflicts": []}},
   "must_not": ["不得把候选 0 判为 reinforce 51：51 带“还没核实”的不确定表达，新断言没有，限定语不同，不满足强化前提",
                "不得只关闭 rel-1 而让 50 与 51 或新记录同时作为当前结论",
                "不得由代码看到 refine 就推断 rel-1 已解决；解决只来自 W4 在撤回决定上给出的 resolves_relations",
                "不得删除 s-130、s-261 原文",
                "两个决定不得部分提交（W5 验收项）"]},
  relations=[link], coverage=PARTIAL, batch=2, boundary_with="w4_15_resolve_conflict",
  rejected=[{"action": "reinforce", "why": "不确定变确定属于限定语变化，不是同一断言；强化会把“还没核实”吞掉"},
            {"action": "correction", "why": "51 没有说错，它如实记录了当时未核实；新断言是在其基础上确认"}],
  note="一条原文拆成两个候选（W1）：有值断言与撤回，两者都引用同一完整原句（通用 fact 的 value.quote 与 sentence 相同）。关系由撤回决定解决：败方 50 结束，胜方 51 被新记录细化后进入历史，新记录不是关系成员。"
       "W5（contracts 2.5）：两个决定相互依赖，同一 T2 原子提交，任一 needs_rejudge 则整组重判，失败整组回滚；"
       "若只有细化而未解决关系，rel-1 保持 open 且成员不变，替代记录标 update_relation_unclear 并经 history 带出该关系（见 w4_22）。")
# ---------------- W22 冲突双方被同一新版本替代，关系不解决、成员不迁移
t = "我已经搬到深圳了。"
tb, ts = "我住在北京。", "我住在上海。"
link = {"id": "rel-2", "kind": "conflict", "members": [110, 111], "conditions": [], "times": [],
        "basis": [ev("s-501", tb), ev("s-502", ts)], "status": "open", "resolution": None}
sample("w4_22_replace_conflict_members", "冲突双方被同一新版本替代：state_change 结束两条，不解决关系，不迁移成员",
  {"s-501": S(tb, "2026-05-01"), "s-502": S(ts, "2026-05-03"), "s-510": S(t, "2026-09-28")},
  [incand(cand("self", "lives_in", t, "深圳"), "s-510", "2026-09-28")],
  [rec(110, cand("self", "lives_in", tb, "北京"), "s-501", "2026-05-01", rel=["rel-2"]),
   rec(111, cand("self", "lives_in", ts, "上海"), "s-502", "2026-05-03", rel=["rel-2"])],
  {"decisions": [{"candidate": 0, "status": "decided", "action": "state_change", "targets": [110, 111], "resolves_relations": [],
                  "basis_sources": ["s-501", "s-502", "s-510"], "effective": None, "pending_reason": None}],
   "after": {"records": {"110": {"stored_status": "superseded", "termination_reason": "StateChanged", "ended": "end_unknown"},
                         "111": {"stored_status": "superseded", "termination_reason": "StateChanged", "ended": "end_unknown"},
                         "new": {"from_candidate": 0, "stored_status": "active", "history": [110, 111],
                                 "update": {"status": "pending", "reasons": ["update_relation_unclear"]}}},
             "relations": [{"id": "rel-2", "status": "open", "members": [110, 111], "basis_unchanged": True}],
             "new_records": 1,
             "recall": {"current": ["new（深圳，前身所在冲突 rel-2 未解决）"],
                        "history": ["110（北京）与 111（上海）互相冲突，rel-2 open"]}},
   "must_not": ["不得把成员换成 [new, new] 或去重成单个成员",
                "不得由代码因成员都已结束而自动解决 rel-2",
                "不得把 rel-2 的旧依据当作新版本仍在冲突的依据",
                "不得在历史查询中把北京或上海单独当作确定的旧住址"]},
  relations=[link], coverage=COMPLETE, batch=2, boundary_with="w4_21_confirm_conflict_side",
  rejected=[{"action": "state_change + resolves_relations", "why": "“搬到深圳”不说明此前北京、上海哪条对，不能解决 rel-2"}],
  note="lives_in 为目录内关系，范围完整。替代记录的 update_relation_unclear 表示与前身所在冲突的关系待后续 W4 依据新原文判定，不是检索范围问题。")
print("batch2 ok")
