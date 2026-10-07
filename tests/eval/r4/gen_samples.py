"""生成 R4 证据判断样本（r4.v3）。标签由 Claude 起草，用户于 2026-10-03 抽查确认（reviewed）。

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


def main():
    OUT.mkdir(exist_ok=True)
    for s in SAMPLES:
        (OUT / f"{s['id']}.json").write_text(json.dumps(s, ensure_ascii=False, indent=1) + "\n")
    print(f"wrote {len(SAMPLES)} samples")


if __name__ == "__main__":
    main()
