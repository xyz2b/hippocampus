"""生成 R4 证据判断样本（历史 r4.v3 + AF227 当前 r4.v7 修订）。标签由 Claude 起草，用户于 2026-10-03 抽查确认（reviewed）。

输入是简写的 R3 证据包（评测适配器负责展开成 contracts 3.2 的结构）：
- records：{id, sentence, said_on, source, status(active|superseded), ended(not_ended|known|end_unknown),
  termination(null|state_changed|correction|retracted), update(judged|pending:<原因>|null), legacy_gaps, conditions, times}
- raw：{source, text, said_on, enrichment}
- current：本轮消息；current_evidence 取自 R1。
expect 只列需要核对的内容：verdict、facts（按 record_ids/来源匹配，列出必须的 current/applies/polarity/限定语）、
compute（op、items 数、operands 结构、coverage）、must_not。split：dev 或 holdout。
"""
import json
import pathlib

OUT = pathlib.Path(__file__).parent / "samples"


def rec(id, sentence, said_on, source, status="active", ended="not_ended", termination=None,
        update="judged", legacy_gaps=None, conditions=(), times=()):
    return {"id": id, "sentence": sentence, "said_on": said_on, "source": source, "status": status,
            "ended": ended, "termination": termination, "update": update, "legacy_gaps": legacy_gaps,
            "conditions": list(conditions), "times": list(times)}


def raw(source, text, said_on, enrichment="queued"):
    return {"source": source, "text": text, "said_on": said_on, "enrichment": enrichment}


def fact(record_ids=(), sources=(), **must):
    return {"record_ids": list(record_ids), "sources": list(sources), "must": must}


def sample(id, split, focus, query, plan, records=(), raws=(), current_evidence="query_only",
           verdict="supported", facts=(), compute=None, must_not=(), at="2026-10-03", note=None,
           relations=(), completeness=None):
    return {
        "id": id, "contract": "r4.v3", "label_status": "reviewed", "split": split, "focus": focus,
        "input": {"query": query, "at": at, "current_evidence": current_evidence, "plan": plan,
                  "records": list(records), "raw": list(raws), "relations": list(relations),
                  "completeness": completeness or {"retrieval_truncated": False, "plan_complete": True}},
        "expect": {"verdict": verdict, "facts": list(facts), "compute": compute},
        "must_not": list(must_not), "note": note,
    }


def plan(type, op, relation=None):
    return {"type": type, "op": op, "relation": relation}


SAMPLES = [
    sample("r4_01_single_supported", "dev", "单一当前事实", "我住在哪？", plan("single", "lookup", "lives_in"),
           records=[rec(1, "我住在杭州。", "2026-05-01", "s-1")],
           facts=[fact([1], ["s-1"], current="yes", polarity="affirm")]),
    sample("r4_02_uncertainty_kept", "dev", "限定语必须保留到 facts", "我对什么过敏？", plan("aggregate", "list", "allergic_to"),
           records=[rec(2, "我可能对花生过敏。", "2026-04-02", "s-2")],
           facts=[fact([2], ["s-2"], uncertainty="可能")],
           must_not=["statement 写成确定过敏", "丢掉 uncertainty"]),
    sample("r4_03_condition_applies_unknown", "dev", "查询没提条件：标注适用条件而不是过滤", "我平时用什么笔？", plan("single", "lookup", "uses"),
           records=[rec(3, "我画速写的时候固定用炭笔。", "2026-03-01", "s-3", conditions=["画速写的时候"])],
           facts=[fact([3], ["s-3"], conditions=["画速写的时候"], applies=["unknown", "no"])],
           must_not=["把炭笔说成无条件的常用笔", "applies=yes"],
           note="roadmap M1：条件不匹配时从“过滤掉”改为“标注适用条件”。"),
    sample("r4_04_new_raw_overrides_old", "dev", "新原文未整理，旧记录不能作为确定当前值（target-design 2.6）", "我住在哪？",
           plan("single", "lookup", "lives_in"),
           records=[rec(4, "我住在上海。", "2026-01-10", "s-4")],
           raws=[raw("s-40", "我上周已经搬到南京了。", "2026-09-30")],
           facts=[fact([], ["s-40"], current="yes"), fact([4], ["s-4"], current="no")],
           verdict="supported",
           must_not=["上海 current=yes", "忽略未整理的南京原文", "判为 conflict（先后变化不是冲突）"],
           note="用户裁定（2026-10-03）：原文明确“已经搬到”，必须 supported；南京来自未整理原文，grade 由代码标为 uncommitted。"),
    sample("r4_05_hypothetical_raw_not_update", "holdout", "新原文只是假设，不更新旧值", "我住在哪？",
           plan("single", "lookup", "lives_in"),
           records=[rec(5, "我住在上海。", "2026-01-10", "s-5")],
           raws=[raw("s-50", "如果明年搬去南京，我得换工作。", "2026-09-30")],
           facts=[fact([5], ["s-5"], current=["yes", "unknown"])],
           must_not=["南京作为当前住址", "上海 current=no"]),
    sample("r4_06_query_only_not_evidence", "dev", "问句本身不是证据", "我是不是对花生过敏？",
           plan("single", "lookup", "allergic_to"), verdict="insufficient",
           must_not=["以问题文本为证据给出 supported 事实"]),
    sample("r4_07_current_message_assertion", "dev", "本轮陈述可作为证据", "我现在住在苏州，附近有什么好吃的？",
           plan("single", "lookup", "lives_in"), current_evidence="assertion_present",
           records=[rec(7, "我住在杭州。", "2026-02-01", "s-7")],
           facts=[fact([], ["current"], current=["yes", "unknown"]), fact([7], ["s-7"], current=["no", "unknown"])],
           verdict=["supported", "conflict"],
           must_not=["杭州 current=yes"]),
    sample("r4_08_pending_update_not_unique", "holdout", "更新待定的记录不能包装成唯一当前值", "我在哪工作？",
           plan("single", "lookup", "works_at"),
           records=[rec(8, "我在一家咨询公司工作。", "2026-08-01", "s-8", update="pending:update_scope_incomplete")],
           facts=[fact([8], ["s-8"])], verdict=["supported", "insufficient"],
           must_not=["声称这是唯一当前雇主"],
           note="update.pending 由 R5 合并带到宿主；R4 不得据 supported 清除。"),
    sample("r4_09_legacy_gap", "dev", "legacy 缺来源时不能新增 supported 事实", "我养过什么宠物？",
           plan("aggregate", "list", "owns"),
           records=[rec(9, None, "2025-12-01", None, update=None, legacy_gaps=["source_missing"])],
           verdict="insufficient", must_not=["用无来源的 legacy 片段给出 supported 事实"]),
    sample("r4_10_superseded_history", "dev", "被替代的旧值是历史而非当前", "我以前住在哪？",
           plan("temporal", "lookup", "lives_in"),
           records=[rec(10, "我住在上海。", "2025-03-01", "s-10", status="superseded", ended="known",
                        termination="state_changed"),
                    rec(11, "我搬到杭州了。", "2026-02-01", "s-11")],
           facts=[fact([10], ["s-10"], current="no"), fact([11], ["s-11"], current="yes")]),
    sample("r4_11_corrected_not_history", "holdout", "被纠正的旧值不能作为历史事实", "我以前在哪家公司？",
           plan("temporal", "lookup", "works_at"),
           records=[rec(12, "我在字节工作。", "2026-01-01", "s-12", status="superseded", ended="known",
                        termination="correction"),
                    rec(13, "我说错了，我在阿里工作。", "2026-01-02", "s-13")],
           facts=[fact([13], ["s-13"], current="yes")],
           must_not=["把字节当作曾经的雇主"]),
    sample("r4_12_end_unknown", "dev", "结束时间未知的记录不能当作一直有效", "我去年三月住在哪？",
           plan("temporal", "lookup", "lives_in"),
           records=[rec(14, "我住在上海。", "2024-06-01", "s-14", status="superseded", ended="end_unknown",
                        termination="state_changed"),
                    rec(15, "我已经搬到杭州了。", "2025-09-01", "s-15")],
           verdict=["supported", "insufficient"],
           facts=[fact([14], ["s-14"], applies=["yes", "unknown"])],
           must_not=["断言上海一直住到某个具体日期"],
           note="2025-03 时上海记录尚未被替代（替代消息 2025-09 才收到），但结束时间未知，R4 可判 supported 或不足，不能编造结束日。"),
    sample("r4_13_open_conflict", "dev", "未解决冲突两条都返回", "我的血型是什么？",
           plan("single", "lookup", "fact"),
           records=[rec(16, "我是 A 型血。", "2025-05-01", "s-16"), rec(17, "我是 O 型血。", "2026-05-01", "s-17")],
           relations=[{"id": "rel-1", "members": [16, 17], "status": "open"}],
           verdict="conflict", must_not=["选出一个作为唯一答案"]),
    sample("r4_14_count_dedupe", "dev", "计数：同一事件不重复计", "我今年去了几次北京？",
           plan("aggregate", "count", "visited"),
           records=[rec(18, "我三月去了北京出差。", "2026-03-20", "s-18"),
                    rec(19, "三月那次北京出差挺累的。", "2026-03-25", "s-19"),
                    rec(20, "我八月又去了一趟北京。", "2026-08-12", "s-20")],
           compute={"op": "count", "items": 3, "distinct_groups": 2},
           note="18、19 指同一次出差，event_group 应相同；代码按组去重得 2。"),
    sample("r4_15_sum_quantities", "holdout", "求和：需要数量与单位", "我上个月买咖啡一共花了多少钱？",
           plan("aggregate", "sum", "fact"),
           records=[rec(21, "九月我买咖啡豆花了120元。", "2026-09-10", "s-21"),
                    rec(22, "九月下旬又买了一袋，80元。", "2026-09-25", "s-22")],
           compute={"op": "sum", "items": 2, "quantities": ["120", "80"], "unit": "元"}),
    sample("r4_16_elapsed_request_at", "dev", "距今：终点用 @request.at", "我搬到成都多久了？",
           plan("temporal", "elapsed", "lives_in"),
           records=[rec(23, "我2026年7月1日搬到成都了。", "2026-07-02", "s-23")],
           compute={"op": "elapsed", "items": 1, "operands": ["@request.at", "<item>"]},
           must_not=["模型自己给出天数", "把 said_on 当作终点"]),
    sample("r4_17_elapsed_month_precision", "holdout", "端点只有月精度时不足", "我搬到成都多少天了？",
           plan("temporal", "elapsed", "lives_in"),
           records=[rec(24, "我七月搬到成都的。", "2026-07-15", "s-24")],
           compute={"op": "elapsed", "insufficient_ok": True},
           must_not=["把七月补成具体日期"]),
    sample("r4_18_compare_dates", "dev", "比较先后", "我是先学的日语还是先学的韩语？",
           plan("comparison", "compare", "fact"),
           records=[rec(25, "我2024年3月开始学日语。", "2024-03-05", "s-25"),
                    rec(26, "我2025年开始学韩语。", "2025-02-01", "s-26")],
           compute={"op": "compare", "items": 2, "operands": 2, "expected_value": "less"},
           note="用户裁定（2026-10-03）：精度不同的日期按各自区间比较，区间不重叠即可确定先后（此处日语在前）；区间重叠返回不足。R4 只给端点，结论由代码计算。"),
    sample("r4_19_truncated_count_lower_bound", "holdout", "检索截断时计数只能给下限或不足", "我总共去过几个国家？",
           plan("aggregate", "count", "visited"),
           records=[rec(27, "我去过日本。", "2025-01-01", "s-27"), rec(28, "我去过泰国。", "2025-06-01", "s-28")],
           completeness={"retrieval_truncated": True, "plan_complete": False},
           compute={"op": "count", "coverage": ["incomplete", "unknown"]},
           must_not=["coverage=complete"]),
    sample("r4_20_retracted", "dev", "已撤回的记录不再作为当前值", "我在哪工作？",
           plan("single", "lookup", "works_at"),
           records=[rec(29, "我在字节工作。", "2026-01-01", "s-29", status="superseded", ended="known",
                        termination="retracted")],
           verdict="insufficient", must_not=["把字节作为当前雇主"]),
]


# AF227 labels and explicit production-shaped inputs; never derive requests from labels.
import copy
import datetime
REVISION = {"decision": "AF227", "date": "2026-10-10"}


def ref(source, text):
    return {"source": {"kind": "stored", "source_id": source}, "quote": {"text": text}}


def qualifier(text):
    return {"quote": {"text": text}, "resolved": True}


def ti(text, kind="unknown", **fields):
    return {"role": "event", "quote": {"text": text}, "interp": {"kind": kind, **fields}, "anchor": "said_on"}


def timestamp(day):
    return int(datetime.datetime.fromisoformat(day).replace(tzinfo=datetime.timezone(datetime.timedelta(hours=8))).timestamp())


def entity(id, name):
    return {"id": id, "owner_id": "eval", "entity_type": "Person", "canonical_name": name, "aliases": []}


def full_record(id, source, text, said_on, *, value=None, subject="self", speech="user_statement",
                polarity="affirm", uncertainty=None, times=(), status="active", ended="not_ended",
                termination=None, history=()):
    resolutions = []
    for t in times:
        if t["interp"]["kind"] == "date":
            start = timestamp(t["interp"]["date"])
            resolutions.append({"status": "resolved", "start": start, "end_exclusive": start + 86400, "precision": "day"})
        else:
            resolutions.append({"status": "unknown", "start": None, "end_exclusive": None, "precision": "unknown"})
    value = text if value is None else value
    return {"format": "current", "id": id, "stored_status": status, "termination_reason": termination,
            "ended": ended, "candidate": {"kind": "assertion", "speech": speech, "subject": {"entity_id": subject},
            "relation": "fact", "memory_type": "fact", "value": {"kind": "text", "text": value, "quote": {"text": value}},
            "sentence": {"text": text}, "polarity": polarity, "strength": None, "uncertainty": uncertainty,
            "conditions": [], "times": list(times)}, "evidence": [ref(source, text)], "grade": "committed",
            "said_on": said_on, "time_resolutions": resolutions, "history": list(history), "open_relations": [],
            "update": {"status": "judged", "reasons": []}}


def full_input(query, records, sources, *, type="single", op="lookup", subject=None, at="2026-10-03", people=()):
    return {"format": "recall.full@1", "owner_id": "eval",
            "request": {"request_id": "stage-eval", "query": query, "context": [], "task_id": None, "at": timestamp(at)},
            "plan": {"need": "judge", "current_evidence": "query_only", "type": type,
                "subject": subject or {"entity_id": "self"}, "relation": "fact", "conditions": [], "time": None,
                "plan": {"op": op, "filter": {"relation": "fact", "time": None}, "unit": None}},
            "entities": [entity("self", "本人"), *people], "relations": ["fact"], "subject_ids": None,
            "utc_offset_seconds": 28800, "sources": sources,
            "evidence": {"current_message": {"source_id": None, "text": query, "said_on": at, "sequence": None,
                "enrichment": "not_stored", "memory_requests": []}, "records": records, "raw": [], "relations": [],
                "completeness": {"retrieval_truncated": False, "pending_related_sources": 0, "pending_updates": 0,
                    "enrichment_gaps": 0, "plan_complete": True}}}


def branches(s, *expectations):
    # Prohibitions belong to each complete branch, including common prohibitions.
    for e in expectations:
        e["must_not"] = list(s["must_not"])
    s["expect"] = {"alternatives": list(expectations)}


for s in SAMPLES:
    number = int(s["id"].split("_")[1])
    if number not in {2, 4, 7, 8, 10, 11, 12, 14, 15, 16, 17, 18, 19}:
        continue
    s["label_revision"] = copy.deepcopy(REVISION)
    if number == 2:
        body = "我可能对花生过敏。"
        s["input"] = full_input(s["input"]["query"],
            [full_record(2, "s-2", body, "2026-04-02", uncertainty=qualifier("可能"))],
            {"s-2": {"text": body, "said_on": "2026-04-02"}}, type="aggregate", op="list")
    elif number == 4:
        s["expect"]["facts"] = s["expect"]["facts"][:1]
        s["expect"]["forbid_facts"] = [fact([4], [], current="yes")]
    elif number == 7:
        s["expect"] = {"verdict": "supported", "facts": [fact([], ["current"], current="yes")], "compute": None,
            "forbid_facts": [fact([7], [], current=["yes", "no"])], "forbid_supersedes": True}
        s["must_not"] = ["把旧住址确认成当前或确定已结束", "推断搬家、旧值真实历史、冲突或替代关系"]
        s["note"] = "AF227：只支持本轮当前住址自述；旧值可不提或保留不确定，不从新旧不同推断变化性质。"
    elif number == 8:
        base = copy.deepcopy(s["expect"])
        base["verdict"] = "supported"
        insufficient = {"verdict": "insufficient", "facts": [], "compute": None}
        branches(s, base, insufficient)
    elif number == 10:
        s["expect"]["facts"] = s["expect"]["facts"][:1]
        s["expect"]["forbid_facts"] = [fact([10], [], current="yes")]
    elif number == 11:
        base = copy.deepcopy(s["expect"])
        base["must_not"] = list(s["must_not"]) + ["把当前雇主当作未被证实的过去任职"]
        branches(s, base, {"verdict": "insufficient", "facts": [], "compute": None})
        for e in s["expect"]["alternatives"]:
            e["must_not"] = list(s["must_not"]) + ["未说明更正及历史任职缺证据"]
    elif number == 12:
        s["must_not"] += ["凭消息接收先后推断目标月份仍在旧住址", "声称已确认目标月份的住址"]
        branches(s, {"verdict": "supported", "facts": [fact([14], ["s-14"], applies="unknown")], "compute": None},
            {"verdict": "insufficient", "facts": [], "compute": None})
        for e in s["expect"]["alternatives"]:
            e["forbid_facts"] = [fact([14], [], applies="yes"), fact([15], [], applies="yes")]
        s["note"] = "AF227：旧陈述不证明连续有效；替代消息的接收日期不能证明现实结束日期。"
    elif number == 14:
        s["expect"]["compute"].pop("items")
        s["expect"]["compute"].update(expected_value="2", expected_bound="exact", groups=[["s-18", "s-19"], ["s-20"]])
    elif number == 15:
        s["expect"]["compute"].update(expected_value="200", expected_bound="exact", distinct_groups=2,
            groups=[["s-21"], ["s-22"]])
    elif number == 16:
        body = "我2026年7月1日搬到成都了。"
        s["input"] = full_input(s["input"]["query"], [full_record(23, "s-23", body, "2026-07-02")],
            {"s-23": {"text": body, "said_on": "2026-07-02"}}, type="temporal", op="elapsed")
        s["input"]["plan"]["plan"]["unit"] = "day"
        s["note"] = "AF227：固定获准 R1 plan.unit=day，事件日到请求日94天；单位不再从未指定单位的问句猜出。"
        s["expect"]["compute"].update(unit="day", expected_value="94", exact_days=94,
            rounding="calendar_days", approximate=False, operand_sources=["@request.at", "s-23"])
    elif number == 17:
        s["must_not"] += ["遗漏粗时间事实却判 supported", "输出精确天数或伪造精度"]
        branches(s, {"verdict": "supported", "facts": [fact([24], ["s-24"], time_quotes=["七月"],
                time_kinds=[["month", "unknown"]])], "compute": {"op": "elapsed", "insufficient_ok": True}},
            {"verdict": "insufficient", "facts": [], "compute": {"op": "elapsed", "insufficient_ok": True}})
        s["note"] = "AF227：两支均无计算结果，粗时间不得补日；supported 支须保留有出处的粗时间事实。"
    elif number == 18:
        s["expect"]["compute"]["operand_sources"] = ["s-25", "s-26"]
    elif number == 19:
        s["expect"]["compute"].update(items=2, distinct_groups=2, expected_value="2", expected_bound="at_least",
            groups=[["s-27"], ["s-28"]])
        s["must_not"] += ["将两个明确不同国家归为同一组或身份未知", "computed.bound=exact"]
        s["note"] = "AF227：已确认日本、泰国两个国家，范围截断只影响总量完整性，不抹掉已知下限。"


def new_sample(id, focus, input, expect, must_not):
    expect["must_not"] = list(must_not)
    return {"id": id, "contract": "r4.v7", "label_status": "reviewed", "split": "holdout", "focus": focus,
        "input": input, "expect": expect, "must_not": must_not, "label_revision": copy.deepcopy(REVISION),
        "note": "AF227 虚构完整输入；语义检查需人工确认，自动字段通过不等于完整语义通过。"}

p1 = "我原计划2026年10月10日去青汀。"
p2 = "我已订好2026年10月10日去青汀的车票。"
p3 = "我取消了2026年10月10日去青汀的出行计划，车票还没有退。"
t = ti("2026年10月10日", "date", date="2026-10-10")
SAMPLES.append(new_sample("r4_21_plan_booking_not_execution", "计划修改/取消不证明执行或退票",
    full_input("我去过青汀吗？原计划和车票分别是什么状态？", [
        full_record(101, "p1", p1, "2026-10-01", speech="plan", times=[t], status="superseded", ended="known", termination="StateChanged"),
        full_record(102, "p2", p2, "2026-10-02", times=[t]),
        full_record(103, "p3", p3, "2026-10-04", value="我取消了2026年10月10日去青汀的出行计划", speech="plan", polarity="negate", times=[t], history=[101]),
        full_record(104, "p3", p3, "2026-10-04", value="车票还没有退", polarity="negate")],
        {"p1": {"text": p1, "said_on": "2026-10-01"}, "p2": {"text": p2, "said_on": "2026-10-02"}, "p3": {"text": p3, "said_on": "2026-10-04"}},
        type="open", op="list", at="2026-10-11"),
    {"verdict": "insufficient", "facts": [fact([101], ["p1"], current="no"), fact([102], ["p2"]),
        fact([103], ["p3"], polarity="negate"), fact([104], ["p3"], polarity="negate")], "compute": None},
    ["计划或订票当作已去过", "没有执行证据就断言没去过", "取消计划自动推断退票", "把旧计划说成已发生活动"]))
body = "我不使用银匙，我使用陶杯。"
SAMPLES.append(new_sample("r4_22_negative_sibling_spans", "同句否定/肯定断言身份分别保留",
    full_input("我用什么？是否使用银匙？", [
        full_record(201, "n2", body, "2026-10-01", value="我不使用银匙", polarity="negate"),
        full_record(202, "n2", body, "2026-10-01", value="我使用陶杯")], {"n2": {"text": body, "said_on": "2026-10-01"}}),
    {"verdict": "supported", "facts": [fact([201], ["n2"], polarity="negate"), fact([202], ["n2"], polarity="affirm")], "compute": None},
    ["银匙说成肯定使用", "交换兄弟记录 ID", "借同句证据合并不同断言身份"]))
a, b = "我对芝麻过敏。", "我的孩子小岑对芒果过敏。"
SAMPLES.append(new_sample("r4_23_open_multiple_subjects", "R1 歧义初判不阻断有依据的开放列举",
    full_input("家里谁对什么过敏？", [full_record(301, "n3a", a, "2026-09-01"),
        full_record(302, "n3b", b, "2026-09-02", subject="child")],
        {"n3a": {"text": a, "said_on": "2026-09-01"}, "n3b": {"text": b, "said_on": "2026-09-02"}},
        type="open", op="list", subject={"ambiguous": ["self", "child"]}, people=[entity("child", "小岑")]),
    {"verdict": "supported", "facts": [fact([301], ["n3a"]), fact([302], ["n3b"])], "compute": None},
    ["互换人物和过敏对象", "因 R1 ambiguous 强制选择一个人或要求澄清", "把不同人物的事实合并成同一人物"]))
body = "我3月15日去了白沙湾。"
SAMPLES.append(new_sample("r4_24_unknown_original_clock", "补录无可信原话时间不借接收日定年",
    full_input("我去过哪里，是什么时候？", [full_record(401, "n4", body, None, times=[ti("3月15日")])],
        {"n4": {"text": body, "said_on": None}}, type="open", op="list"),
    {"verdict": "supported", "facts": [fact([401], ["n4"], time_quotes=["3月15日"], time_kinds=["unknown"])], "compute": None},
    ["借本请求、接收日或其他来源补年份", "把未知时间换成精确日期", "因年份未知否认有出处的去过陈述"]))


# AF247 keeps requests frozen and binds each alternative to a complete expectation.
for s in SAMPLES:
    number = int(s["id"].split("_")[1])
    if number not in {3, 7, 14, 21}:
        continue
    s["label_revision"] = {"decision": "AF246/AF247" if number == 7 else "AF247", "date": "2026-10-11"}
    if number == 3:
        base = copy.deepcopy(s["expect"])
        branches(s, base, dict(copy.deepcopy(base), verdict="insufficient"))
        s["note"] = "AF247：支持有条件的自述或判一般场景证据不足均可；两支都须同一有出处事实、条件与 applies=unknown/no，不得回答成无条件常用笔。"
    elif number == 7:
        s["expect"]["forbid_facts"] = [fact([7], [], current="yes")]
        s["must_not"] = ["把旧住址确认成当前", "把旧自述当作真实历史", "推断搬家、冲突、替代关系或确定结束时间"]
        s["note"] = "AF246/AF247：旧记录可不提，或有出处地说明曾自述且 current=no/unknown；仍须本轮住址 current=yes，禁止 supersedes 与确认旧值真实历史。"
    elif number == 14:
        base = copy.deepcopy(s["expect"])
        s["must_not"] = ["将三月两次提及计为不同事件", "将八月事件合并到三月", "加入无依据的额外事件"]
        representatives = []
        for source in ("s-18", "s-19"):
            branch = copy.deepcopy(base)
            branch["compute"].update(items=2, groups=[[source], ["s-20"]])
            representatives.append(branch)
        branches(s, base, *representatives)
        s["note"] = "AF247：允许保留重复条目同组、合并双方依据，或取三月/八月各一个事件代表；两组、exact=2 不变。代表分支的重复事件同一性须人工确认。"
    elif number == 21:
        s["note"] = "AF247：保持 insufficient；问题包含是否实际去过，计划/订票/取消不能证明执行。记录103(plan)与104(user_statement)的 Candidate 均为 negate，表面肯定句式不能豁免方向或身份错误。"


def main():
    OUT.mkdir(exist_ok=True)
    for s in SAMPLES:
        (OUT / f"{s['id']}.json").write_text(json.dumps(s, ensure_ascii=False, indent=1) + "\n")
    print(f"wrote {len(SAMPLES)} samples")


if __name__ == "__main__":
    main()
