"""生成 W3 语义核验样本（w3.v1）。标签由 Claude 起草，状态一律 draft，须经用户抽查。

input.candidates 是 W2 已通过的候选（有些故意带错），expect.reviews 按候选索引给出
verdict、必须指出的 problems.aspect 集合（supported 时为空）与 episode_value。
"""
import json
import pathlib

OUT = pathlib.Path(__file__).parent / "samples"


def cand(sentence, *, subject="self", relation="fact", memory_type="fact", value=None,
         kind="assertion", speech="user_statement", polarity="affirm", uncertainty=None,
         conditions=(), times=(), strength=None):
    return {
        "kind": kind, "speech": speech, "subject": subject, "relation": relation,
        "memory_type": memory_type, "value": sentence if value is None else value,
        "sentence": sentence, "polarity": polarity, "uncertainty": uncertainty,
        "conditions": list(conditions), "times": list(times), "strength": strength,
    }


def review(verdict, aspects=(), episode="not_applicable"):
    return {"verdict": verdict, "aspects": list(aspects), "episode_value": episode}


def sample(id, focus, text, candidates, reviews, said_on="2026-09-20", requests=(),
           request_reviews=(), coverage="complete", missing=(), note=None):
    assert len(candidates) == len(reviews)
    return {
        "id": id, "contract": "w3.v1", "label_status": "draft", "focus": focus,
        "input": {"text": text, "said_on": said_on, "candidates": list(candidates),
                  "requests": list(requests)},
        "expect": {
            "reviews": [dict(candidate=i, **r) for i, r in enumerate(reviews)],
            "coverage": {"status": coverage, "missing_contains": list(missing)},
            "request_reviews": [dict(request=i, verdict=v) for i, v in enumerate(request_reviews)],
        },
        "note": note,
    }


def month(year_offset, m):
    return {"kind": "month", "year_offset": year_offset, "month": m}


SAMPLES = [
    sample("w3_01_supported_simple", "正确候选应 supported", "我现在住在杭州。",
           [cand("我现在住在杭州。", relation="lives_in", value="杭州")],
           [review("supported")]),
    sample("w3_02_dropped_uncertainty", "丢掉“可能”使断言比原文更强", "我可能对花生过敏。",
           [cand("我可能对花生过敏。", relation="allergic_to", value="花生")],
           [review("contradicted", ["uncertainty"])],
           note="contradicted 还是 insufficient 由人工复核：候选宣称确定过敏，原文只说可能，草稿按 contradicted。"),
    sample("w3_03_dropped_condition", "丢掉活动条件", "我画速写的时候固定用炭笔。",
           [cand("我画速写的时候固定用炭笔。", relation="uses", value="炭笔")],
           [review("contradicted", ["condition"])]),
    sample("w3_04_wrong_polarity", "极性反了", "我不喝咖啡。",
           [cand("我不喝咖啡。", relation="likes", memory_type="preference", value="咖啡")],
           [review("contradicted", ["polarity"])]),
    sample("w3_05_wrong_subject", "主体错绑为用户本人", "我妈对青霉素过敏。",
           [cand("我妈对青霉素过敏。", relation="allergic_to", value="青霉素")],
           [review("contradicted", ["subject"])]),
    sample("w3_06_hypothetical_as_fact", "假设被标成用户陈述", "如果我以后去深圳工作，我会考虑租房。",
           [cand("如果我以后去深圳工作，我会考虑租房。", relation="works_at", value="深圳")],
           [review("contradicted", ["speech"])],
           note="value 也不被支持（并没有在深圳工作）；aspects 至少含 speech，是否另含 value 不扣分。"),
    sample("w3_07_reported_as_statement", "转述被标成用户陈述", "我妈说我小时候特别怕狗。",
           [cand("我妈说我小时候特别怕狗。")],
           [review("contradicted", ["speech"])]),
    sample("w3_08_wrong_strength", "偏好强度夸大", "我有点喜欢爬山。",
           [cand("我有点喜欢爬山。", relation="likes", memory_type="preference", value="爬山",
                 strength="strong")],
           [review("contradicted", ["strength"])]),
    sample("w3_09_wrong_time", "时间解释错了一年", "去年五月我搬到了成都。",
           [cand("去年五月我搬到了成都。", relation="lives_in", value="成都",
                 times=[{"role": "event", "quote": "去年五月", "interp": month(0, 5)}])],
           [review("contradicted", ["time"])]),
    sample("w3_10_value_not_supported", "值超出原文", "我住在杭州附近。",
           [cand("我住在杭州附近。", relation="lives_in", value="杭州")],
           [review("insufficient", ["value"])],
           note="“杭州附近”不能证明住在杭州，也不能证明不住在杭州：草稿按 insufficient。"),
    sample("w3_11_coverage_gap", "漏提另一个断言", "我对芒果过敏，而且最近在学游泳。",
           [cand("我对芒果过敏，而且最近在学游泳。", relation="allergic_to", value="芒果")],
           [review("supported")], coverage="gaps", missing=["学游泳"]),
    sample("w3_12_coverage_complete_multi", "多个候选全部正确", "我住在上海，在一家银行做产品经理。",
           [cand("我住在上海，在一家银行做产品经理。", relation="lives_in", value="上海"),
            cand("我住在上海，在一家银行做产品经理。", relation="job_title", value="产品经理")],
           [review("supported"), review("supported")],
           note="是否把“在一家银行”视为漏提由人工复核；草稿按 complete。"),
    sample("w3_13_episode_useful", "有跨轮价值的经历", "我今天第一次跑完了半程马拉松，用时2小时05分。",
           [cand("我今天第一次跑完了半程马拉松，用时2小时05分。", memory_type="episode",
                 times=[{"role": "event", "quote": "今天",
                         "interp": {"kind": "relative", "unit": "day", "offset": 0}}])],
           [review("supported", episode="useful")]),
    sample("w3_14_episode_low_value", "低价值经历", "我刚吃了午饭。",
           [cand("我刚吃了午饭。", memory_type="episode")],
           [review("supported", episode="low_value")]),
    sample("w3_15_remember_bound", "remember 请求绑定正确", "记住：我的车牌是浙A12345。",
           [cand("我的车牌是浙A12345。", relation="owns", value="浙A12345")],
           [review("supported")],
           requests=[{"kind": "remember", "quote": "记住", "target": {"object": "facts", "candidate_indices": [0]},
                      "deadline": None}],
           request_reviews=["supported"]),
    sample("w3_16_remember_overbound", "remember 被扩展到无关候选", "记住我的车牌是浙A12345，另外今天好累。",
           [cand("记住我的车牌是浙A12345，另外今天好累。", relation="owns", value="浙A12345"),
            cand("记住我的车牌是浙A12345，另外今天好累。", memory_type="episode", value="今天好累")],
           [review("supported"), review("supported", episode="low_value")],
           requests=[{"kind": "remember", "quote": "记住", "target": {"object": "facts", "candidate_indices": [0, 1]},
                      "deadline": None}],
           request_reviews=["contradicted"],
           note="请求只针对车牌；绑定到“今天好累”会让低价值经历绕过准入。"),
    sample("w3_17_event_time_as_retention", "事件时间被误当保存期限（历史上 59/237 的误填类型）", "我明天要去北京开会。",
           [cand("我明天要去北京开会。", speech="plan",
                 times=[{"role": "event", "quote": "明天", "interp": {"kind": "relative", "unit": "day", "offset": 1}}])],
           [review("supported")],
           requests=[{"kind": "retention", "quote": "明天", "target": {"object": "facts", "candidate_indices": [0]},
                      "deadline": {"role": "retention", "quote": "明天",
                                   "interp": {"kind": "relative", "unit": "day", "offset": 1}}}],
           request_reviews=["contradicted"]),
    sample("w3_18_unresolved_subject_ok", "如实保留的未解析主体应 supported", "他上周辞职了。",
           [cand("他上周辞职了。", subject={"unresolved": "他"},
                 times=[{"role": "event", "quote": "上周", "interp": {"kind": "relative", "unit": "week", "offset": -1}}])],
           [review("supported")]),
    sample("w3_19_unresolved_condition_honest", "无法理解的条件如实标 resolved=false", "那种场合我一般不喝酒。",
           [cand("那种场合我一般不喝酒。", relation="avoids", memory_type="preference", value="酒",
                 conditions=[{"quote": "那种场合", "resolved": False}])],
           [review("supported")],
           note="“一般”是否应进 uncertainty 由人工复核；草稿认为它是频率而非不确定，不扣分。"),
    sample("w3_20_wrong_relation", "关系选错（owns 与 uses）", "我在用 Neovim 写代码。",
           [cand("我在用 Neovim 写代码。", relation="owns", value="Neovim")],
           [review("contradicted", ["relation"])]),
]


def main():
    OUT.mkdir(exist_ok=True)
    for s in SAMPLES:
        (OUT / f"{s['id']}.json").write_text(json.dumps(s, ensure_ascii=False, indent=1) + "\n")
    print(f"wrote {len(SAMPLES)} samples")


if __name__ == "__main__":
    main()
