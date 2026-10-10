//! AF230 recall comparison uses the shared durable, non-retrying recording engine.
use super::*;

const CONFIG_IDS: [&str; 7] = [
    "ds-pro",
    "ds-flash",
    "gpt-6.1-sol",
    "gpt-6-luna",
    "claude-haiku-5-5",
    "claude-sonnet-5-5",
    "qwen38-flash-nothink",
];
const LEGACY_IDS: [&str; 6] = [
    "ds-pro",
    "ds-flash",
    "gpt-6.1-sol",
    "gpt-6-luna",
    "claude-haiku-5-5",
    "qwen38-flash-nothink",
];
const IDS: [&str; 5] = [
    "ds-pro",
    "ds-flash",
    "gpt-6.1-sol",
    "gpt-6-luna",
    "qwen38-flash-nothink",
];
const ROOT: &str = "target/eval-providers/recall-af228";
const AUTHORIZED: usize = 549;
const FORMAL: usize = 440;
const SMOKE: usize = 15;
const HISTORICAL_SMOKE: usize = 18;
const ADDITIONAL_SMOKE: usize = 3;
const REVOKED: usize = 88;
const AVAILABLE: usize = AUTHORIZED - REVOKED;

#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Role {
    effort: ReasoningEffort,
    max_output_tokens: u32,
    timeout_seconds: u64,
}
#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    id: String,
    model: String,
    provider: ProviderConfig,
    r1: Role,
    r4: Role,
    r4_compute: Role,
    note: String,
    #[serde(default)]
    endpoint_env: Option<String>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Profiles {
    version: String,
    profiles: Vec<Profile>,
}
fn configured_profiles(text: &str) -> Result<Vec<Profile>, String> {
    let data: Profiles = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if data.version != "recall.compare@1"
        || data
            .profiles
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>()
            != CONFIG_IDS
    {
        return Err("recall comparison requires the approved profiles in fixed order".into());
    }
    for p in &data.profiles {
        let model = match p.id.as_str() {
            "ds-pro" => "ep-20261003130756-trhj6",
            "ds-flash" => "ep-20260923215039-vxgxg",
            "qwen38-flash-nothink" => model_provider::DASHSCOPE_QWEN38_MODEL,
            _ => p.id.as_str(),
        };
        if p.model != model || (p.id.starts_with("qwen") && p.provider.thinking != Some(false)) {
            return Err("AF224 model/thinking differs from the approved profile".into());
        }
        if p.endpoint_env.as_deref()
            != if matches!(p.id.as_str(), "ds-pro" | "ds-flash") {
                Some("HUOSHAN_DEEPSEEK_BASE_URL")
            } else {
                None
            }
        {
            return Err("recall endpoint routing differs from approval".into());
        }
        p.provider
            .require_native_schema()
            .map_err(|e| e.to_string())?;
        let effort = if p.id.starts_with("ds-") {
            ReasoningEffort::Minimal
        } else if p.id.starts_with("qwen") {
            ReasoningEffort::Omit
        } else {
            ReasoningEffort::Low
        };
        for (role, compute) in [(&p.r1, false), (&p.r4, false), (&p.r4_compute, true)] {
            let expected_effort = if compute && effort != ReasoningEffort::Omit {
                ReasoningEffort::Low
            } else {
                effort
            };
            if role.effort != expected_effort
                || role.max_output_tokens
                    != if compute || p.id == "ds-flash" {
                        16384
                    } else {
                        4096
                    }
                || role.timeout_seconds != if compute { 240 } else { 180 }
            {
                return Err(
                    "AF224 role effort/output/timeout differs from approved settings".into(),
                );
            }
            p.provider
                .check_model(&p.model, role.effort)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(data.profiles)
}
fn profiles(text: &str) -> Result<Vec<Profile>, String> {
    Ok(configured_profiles(text)?
        .into_iter()
        .filter(|p| IDS.contains(&p.id.as_str()))
        .collect())
}

fn cohort() -> Result<Vec<(String, Value)>, String> {
    let selected: Vec<_> = ["r1", "r4"]
        .into_iter()
        .flat_map(|stage| samples(stage).into_iter().map(move |s| (stage.into(), s)))
        .collect();
    let ids: std::collections::BTreeSet<_> = selected
        .iter()
        .filter_map(|(_, s)| s["id"].as_str())
        .collect();
    if selected.len() != 44
        || selected.iter().filter(|(s, _)| s == "r1").count() != 20
        || ids.len() != selected.len()
        || selected
            .iter()
            .any(|(_, s)| s["label_status"] != "reviewed")
    {
        return Err("recall comparison needs distinct reviewed samples".into());
    }
    Ok(selected)
}

fn plan_groups(
    root: &Path,
    selected: &[(String, Value)],
    profiles: &[Profile],
    smoke: bool,
    variable: impl Fn(&str) -> Option<String>,
) -> Result<Vec<Vec<Case>>, String> {
    let mut groups = Vec::new();
    let selected: Vec<_> = if smoke {
        let ids = [
            "r1_01_lookup_residence",
            "r4_01_single_supported",
            "r4_14_count_dedupe",
        ];
        let cases: Vec<_> = selected
            .iter()
            .filter(|(_, s)| ids.contains(&s["id"].as_str().unwrap()))
            .cloned()
            .collect();
        if cases.len() != ids.len() {
            return Err("recall smoke trio missing".into());
        }
        // Lookup has the stored-only schema; count exercises current-eligible schema + low compute.
        cases
            .into_iter()
            .map(|(stage, mut s)| {
                if s["id"] == "r4_14_count_dedupe" {
                    s["input"]["current_evidence"] = json!("unknown");
                }
                (stage, s)
            })
            .collect()
    } else {
        selected.to_vec()
    };
    for p in profiles {
        // AF230 changes Flash's request identity; preserve its previous smoke campaign.
        let replacement = root.parent().unwrap_or(root).join("smoke-af230");
        let root = if smoke && p.id == "ds-flash" && p.r1.max_output_tokens == 16384 {
            &replacement
        } else {
            root
        };
        let recording_dir = root.join("recordings").join(&p.id);
        let routes = SmokeProfile {
            id: p.id.clone(),
            model: p.model.clone(),
            provider: p.provider.clone(),
            effort: p.r1.effort,
            max_output_tokens: Default::default(),
            timeout_seconds: 180,
            endpoint_env: p.endpoint_env.clone(),
        };
        let base = profile_base(&routes, &recording_dir, &variable)?;
        p.provider.endpoint(&base).map_err(|e| e.to_string())?;
        let options = ProviderOptions {
            provider: p.provider.clone(),
            campaign: root.join("campaigns").join(&p.id),
        };
        let mut cases = Vec::new();
        for (stage, sample) in &selected {
            let op = if stage == "r4" {
                recall_adapter::R4Input::new(&sample["input"])?
                    .plan
                    .output
                    .unwrap()
                    .plan
                    .op
            } else {
                R1Op::Lookup
            };
            let role = if stage == "r1" {
                &p.r1
            } else if matches!(op, R1Op::Count | R1Op::Sum | R1Op::Elapsed | R1Op::Compare) {
                &p.r4_compute
            } else {
                &p.r4
            };
            let model = EvalModel {
                label: p.id.clone(),
                model: Some(p.model.clone()),
                base: Some(base.clone()),
                effort: role.effort,
            };
            let mut request =
                stage_request_with_output_tokens(stage, sample, role.max_output_tokens)?;
            request["model"] = json!(p.model);
            role.effort.apply(&mut request);
            let input = request["input"].as_str().ok_or("missing recall input")?;
            if input.contains(sample["id"].as_str().ok_or("missing sample ID")?)
                || input.contains("\"expect\"")
                || input.contains("\"label_status\"")
            {
                return Err("recall model-visible input contains evaluation metadata".into());
            }
            for repeat in 1..=if smoke { 1 } else { 2 } {
                let mut case = case_from_request_with_provider(
                    stage.clone(),
                    sample.clone(),
                    &recording_dir.join(format!("repeat-{repeat}")),
                    &model,
                    repeat,
                    request.clone(),
                    Some(&options),
                    Some(role.timeout_seconds),
                )?;
                case.endpoint_env = p.endpoint_env.clone();
                cases.push(case);
            }
        }
        compare_mark_interrupted(&mut cases)?;
        campaign_check(&cases, cases.len())?;
        groups.push(cases);
    }
    Ok(groups)
}

fn dry_plan(
    root: &Path,
    selected_profiles: Option<&str>,
    smoke: bool,
    variable: impl Fn(&str) -> Option<String>,
) -> Result<String, String> {
    let profiles = profiles(include_str!("providers.json"))?;
    let selected_ids = compare_selection(selected_profiles, &IDS)?;
    let groups = plan_groups(root, &cohort()?, &profiles, smoke, &variable)?;
    let mut lines = vec![format!("AF230 召回{}干跑（实际发送 0）", if smoke {"冒烟"} else {"比较"}),
        "AF227 标注修订；当前 R1 20 / R4 24；请求 r1.v2/r4.v7；不调用写入、检索或嵌入。".into(),
        "档案 | 槽位 | R1/R4 | 待录制 | 复用 | 过期 | 拒绝 | 中断在途 | 熔断未发送 | 已占用 | 输入粗估 | 输出额度上界".into()];
    let mut total = 0;
    let mut input_tokens = 0;
    let mut output_tokens = 0u64;
    let mut refused = 0;
    for group in &groups {
        let id = &group[0].model.label;
        if !selected_ids.contains(id) {
            continue;
        }
        let circuits = provider_circuits_with_native_stop(group, true)?;
        let count = |pred: fn(&Case) -> bool| group.iter().filter(|c| pred(c)).count();
        let needed = group
            .iter()
            .filter(|c| {
                matches!(c.replay, Replay::Missing | Replay::Stale) && !circuit_blocks(c, &circuits)
            })
            .count();
        let rejected = count(|c| matches!(c.replay, Replay::Rejected(_)));
        let estimate: usize = group
            .iter()
            .map(|c| {
                serde_json::to_vec(&c.wire_request)
                    .unwrap()
                    .len()
                    .div_ceil(3)
            })
            .sum();
        let capacity: u64 = group
            .iter()
            .map(|c| c.request["max_output_tokens"].as_u64().unwrap())
            .sum();
        let occupied = count_sends([group[0].campaign.as_ref().unwrap().join("sends")])?;
        if occupied + needed > group.len() {
            return Err("recall campaign allowance exhausted".into());
        }
        total += group.len();
        input_tokens += estimate;
        output_tokens += capacity;
        refused += rejected;
        lines.push(format!("{id} | {} | {}/{} | {needed} | {} | {} | {rejected} | {} | {} | {occupied} | {estimate} | {capacity}",
            group.len(), count(|c| c.stage=="r1"), count(|c| c.stage=="r4"),
            count(|c| matches!(c.replay, Replay::Output(_) | Replay::Failed(_))), count(|c| matches!(c.replay, Replay::Stale)),
            count(|c| matches!(c.replay, Replay::Interrupted)), group.iter().filter(|c| circuit_blocks(c, &circuits)).count()));
        let p = profiles.iter().find(|p| &p.id == id).unwrap();
        lines.push(format!(
            "  R1={}/{}/{}；R4={}/{}/{}；计算={}/{}/{}；{}",
            json!(p.r1.effort),
            p.r1.max_output_tokens,
            p.r1.timeout_seconds,
            json!(p.r4.effort),
            p.r4.max_output_tokens,
            p.r4.timeout_seconds,
            json!(p.r4_compute.effort),
            p.r4_compute.max_output_tokens,
            p.r4_compute.timeout_seconds,
            p.note
        ));
        let recorded: Vec<_> = group.iter().filter(|c| !c.record.is_null()).map(|c| json!({"id":c.sample["id"],"stage":c.stage,"usage":c.record["usage"],"status":"未评分"})).collect();
        if !recorded.is_empty() {
            lines.push(format!(
                "  历史录制计费（复用现有 usage 口径，非费用预测）：{}",
                compare_cost(id, &recorded)
            ));
        }
    }
    lines.push(format!(
        "所选精确槽位: {total}；发送前拒绝: {refused}；实际发送: 0。"
    ));
    lines.push(format!("输入粗估: {input_tokens} token（完整 wire UTF-8 字节逐请求/3向上取整，非上下文证明）；输出额度上界: {output_tokens} token（含推理，非预计用量）。"));
    lines.push(if smoke {
        format!("独立 campaign: {ROOT}/smoke-af230/campaigns/ds-flash；其他四档沿用 {ROOT}/smoke/campaigns/<profile>；每档冒烟3槽，筛选不扩额；干跑不创建产物。")
    } else {
        format!("独立 campaign: {ROOT}/campaigns/<profile>；每档正式88槽，筛选不扩额；干跑不创建授权/账本/录制/报告。")
    });
    lines.push("历史授权546 + 新冒烟3 = 549；撤销Haiku正式88槽后可用461 = 旧冒烟18 + 新冒烟3 + 正式440。当前五档冒烟15槽中12槽沿用旧录制；Haiku/Sonnet配置保留但选择器拒绝。原生失败立即停档，不自动回落 prompt_schema。".into());
    if !smoke {
        let smoke_groups = plan_groups(&root.join("smoke"), &cohort()?, &profiles, true, |key| {
            variable(key)
        })?;
        let gates = smoke_gates(&smoke_groups);
        let mut passed = 0;
        for gate in items(&gates) {
            if selected_ids.iter().any(|id| gate["profile"] == *id) {
                passed += usize::from(gate["passed"] == true);
                lines.push(format!(
                    "  {} 冒烟门控: {}（正式实发前核验；不影响干跑槽位统计）。",
                    gate["profile"],
                    if gate["passed"] == true {
                        "已通过"
                    } else {
                        "未通过/缺失，该档正式实发拒绝"
                    }
                ));
            }
        }
        lines.push(format!(
            "冒烟已通过档案: {passed}/{}；未通过档案的正式发送数强制为0，440为完整潜在槽位。",
            selected_ids.len()
        ));
    }
    Ok(lines.join("\n"))
}

#[derive(Debug)]
struct Options {
    smoke: bool,
    dry: bool,
    record: bool,
    selected: Vec<String>,
    gate: bool,
}
fn options(variable: impl Fn(&str) -> Option<String>) -> Result<Options, String> {
    let smoke = match variable("EVAL_RECALL_COMPARE").as_deref() {
        Some("af230" | "af228" | "af224") => false,
        Some("af230-smoke" | "af228-smoke" | "af224-smoke") => true,
        _ => return Err("EVAL_RECALL_COMPARE must be af230 or af230-smoke".into()),
    };
    let limit = variable("EVAL_RECORD")
        .map(|v| v.parse::<usize>().map_err(|_| "invalid EVAL_RECORD"))
        .transpose()?;
    if limit.is_some_and(|n| n != 0 && n != AUTHORIZED) {
        return Err(
            "AF230 requires EVAL_RECORD=549 (historical 546 + new smoke 3; Haiku formal 88 revoked), even for a profile subset"
                .into(),
        );
    }
    for key in [
        "EVAL_COMPARE",
        "EVAL_SMOKE",
        "EVAL_AF216",
        "EVAL_AF206",
        "EVAL_AF198",
        "EVAL_AF193",
        "EVAL_AF196",
        "EVAL_STAGES",
        "EVAL_CASES",
        "EVAL_MODEL",
        "EVAL_MODEL_LABEL",
        "EVAL_BASE_URL",
        "EVAL_REASONING_EFFORT",
        "EVAL_MAX_OUTPUT_TOKENS",
        "EVAL_REPEAT",
        "EVAL_PROVIDER_CONFIG",
        "EVAL_CAMPAIGN_DIR",
        "EVAL_M15",
        "EVAL_FIXED_INPUTS",
    ] {
        if variable(key).is_some() {
            return Err(format!("AF230 does not accept {key}"));
        }
    }
    Ok(Options {
        smoke,
        dry: variable("EVAL_DRY_RUN").as_deref() == Some("1"),
        record: limit == Some(AUTHORIZED),
        selected: compare_selection(variable("EVAL_RECALL_PROFILES").as_deref(), &IDS)?,
        gate: variable("EVAL_GATE").as_deref() == Some("1"),
    })
}
pub fn dry_run_from_env() -> Result<(), String> {
    let o = options(|key| std::env::var(key).ok())?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(ROOT);
    let ps = profiles(include_str!("providers.json"))?;
    let samples = cohort()?;
    let formal = plan_groups(&root, &samples, &ps, false, |_| None)?;
    let smoke = plan_groups(&root.join("smoke"), &samples, &ps, true, |_| None)?;
    approval_check(&root, &approval(&formal, &smoke), false)?;
    println!(
        "{}",
        dry_plan(
            &if o.smoke { root.join("smoke") } else { root },
            Some(&o.selected.join(",")),
            o.smoke,
            |key| std::env::var(key).ok()
        )?
    );
    Ok(())
}

fn compute(case: &Case) -> bool {
    case.stage == "r4"
        && matches!(
            recall_adapter::R4Input::new(&case.sample["input"])
                .unwrap()
                .plan
                .output
                .unwrap()
                .plan
                .op,
            R1Op::Count | R1Op::Sum | R1Op::Elapsed | R1Op::Compare
        )
}
fn smoke_check(group: &[Case]) -> Result<(), String> {
    campaign_check(group, 3)?;
    if group.len() != 3 {
        return Err("AF230 smoke needs all three slots".into());
    }
    for c in group {
        let Replay::Output(text) = &c.replay else {
            return Err(format!(
                "{} smoke {} missing/stale/failed/interrupted",
                c.model.label, c.sample["id"]
            ));
        };
        let output: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let row = score(&c.stage, &c.sample, Some(text));
        let usage = if c.record["usage"].is_null() {
            &c.record["response"]["usage"]
        } else {
            &c.record["usage"]
        };
        if c.record["response"]["status"] != "completed"
            || model_provider::check_output(&c.request["text"]["format"]["schema"], &output)
                .is_err()
            || !row["final_output"].is_object()
            || (c.stage == "r4" && !row["validated_output"].is_object())
            || items(&row["fields"]).iter().any(|f| {
                // Label checks such as facts[i].identity are automatic semantics, too.
                matches!(f["field"].as_str(), Some("decode/validate" | "assembly"))
                    && f["passed"] != true
            })
            || usage["input_tokens"].as_u64().is_none()
            || usage["output_tokens"].as_u64().is_none()
        {
            return Err(format!(
                "{} smoke {} failed protocol/schema/final-mechanical-validation/usage",
                c.model.label, c.sample["id"]
            ));
        }
    }
    Ok(())
}
fn smoke_gates(groups: &[Vec<Case>]) -> Value {
    json!(
        groups
            .iter()
            .map(|g| match smoke_check(g) {
                Ok(()) => json!({"profile":g[0].model.label,"passed":true}),
                Err(e) => json!({"profile":g[0].model.label,"passed":false,"reason":e}),
            })
            .collect::<Vec<_>>()
    )
}
fn apply_gates(groups: &mut [Vec<Case>], gates: &Value, selected: &[String]) {
    for group in groups {
        let id = &group[0].model.label;
        if selected.contains(id)
            && items(gates)
                .iter()
                .any(|g| g["profile"] == *id && g["passed"] != true)
        {
            for c in group {
                if matches!(c.replay, Replay::Missing | Replay::Stale) {
                    c.replay = Replay::Rejected(
                        "AF230 smoke missing/stale/failed; no formal send permitted".into(),
                    );
                }
            }
        }
    }
}

fn approval_slots(groups: &[Vec<Case>]) -> Vec<Value> {
    // Endpoints are separately frozen per campaign; env credentials never enter this manifest.
    groups.iter().flat_map(|g|g.iter()).map(|c| json!({
        "profile":c.model.label,"id":c.sample["id"],"stage":c.stage,"repeat":c.repeat,
        "binding":hippocampus::recall_models::fingerprint(&json!([model_provider::ADAPTER_VERSION,c.request,c.wire_request,c.provider,c.timeout_seconds]))
    })).collect::<Vec<_>>()
}
fn approval(groups: &[Vec<Case>], smoke: &[Vec<Case>]) -> Value {
    json!({"version":"af230.authorization@1","total":AUTHORIZED,"available":AVAILABLE,
        "formal":FORMAL,"smoke":SMOKE,"historical_smoke":HISTORICAL_SMOKE,
        "additional_smoke":ADDITIONAL_SMOKE,"revoked_formal":REVOKED,
        "profiles":IDS,"formal_requests":approval_slots(groups),"smoke_requests":approval_slots(smoke)})
}
fn legacy_approval() -> Result<Value, String> {
    let mut ps = configured_profiles(include_str!("providers.json"))?;
    ps.retain(|p| LEGACY_IDS.contains(&p.id.as_str()));
    for p in &mut ps {
        if p.id == "ds-flash" {
            p.r1.max_output_tokens = 4096;
            p.r4.max_output_tokens = 4096;
        }
    }
    // Build only request identities; no files or campaign state from the historical run are touched.
    let root = std::env::temp_dir().join(format!(
        "af230-identity-{}",
        hippocampus::diagnostics::new_id()
    ));
    let samples = cohort()?;
    let formal = plan_groups(&root, &samples, &ps, false, |_| None)?;
    let smoke = plan_groups(&root.join("smoke"), &samples, &ps, true, |_| None)?;
    Ok(
        json!({"version":"af228.authorization@1","total":546,"formal":528,"smoke":18,
        "profiles":LEGACY_IDS,"formal_requests":approval_slots(&formal),"smoke_requests":approval_slots(&smoke)}),
    )
}
fn approval_check(root: &Path, expected: &Value, freeze: bool) -> Result<(), String> {
    if expected["version"] != "af230.authorization@1"
        || expected["total"] != AUTHORIZED
        || expected["available"] != AVAILABLE
        || expected["formal"] != FORMAL
        || expected["smoke"] != SMOKE
        || expected["historical_smoke"] != HISTORICAL_SMOKE
        || expected["additional_smoke"] != ADDITIONAL_SMOKE
        || expected["revoked_formal"] != REVOKED
        || expected["profiles"] != json!(IDS)
        || items(&expected["formal_requests"]).len() != FORMAL
        || items(&expected["smoke_requests"]).len() != SMOKE
    {
        return Err("AF230 authorization shape changed".into());
    }
    let _lock = if freeze {
        Some(w3_trial_lock(&root.join("authorization-lock"))?)
    } else {
        None
    };
    let previous_path = root.join("authorization.json");
    let previous = if previous_path.exists() {
        let stored: Value =
            serde_json::from_slice(&std::fs::read(previous_path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if stored != legacy_approval()? {
            return Err("AF228 historical frozen identity changed; cannot apply AF230".into());
        }
        Some(hippocampus::recall_models::fingerprint(&stored))
    } else {
        None
    };
    let mut expected = expected.clone();
    expected["previous_authorization"] = json!(previous);
    let path = root.join("authorization-af230.json");
    if path.exists() {
        let stored: Value =
            serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if stored != expected {
            return Err("AF230 frozen profile/request/allowance identity changed".into());
        }
    } else if freeze {
        m15_write_json(&path, &expected)?;
    }
    let occupied = count_sends(
        LEGACY_IDS
            .iter()
            .flat_map(|id| {
                [
                    root.join("campaigns").join(id).join("sends"),
                    root.join("smoke/campaigns").join(id).join("sends"),
                ]
            })
            .chain([root.join("smoke-af230/campaigns/ds-flash/sends")]),
    )?;
    if occupied > AVAILABLE {
        return Err("AF230 remaining allowance exceeded (Haiku formal 88 revoked)".into());
    }
    Ok(())
}
fn latency_splits(rows: &[Value]) -> Vec<Value> {
    let mut groups = std::collections::BTreeMap::<(String, bool, String), Vec<Value>>::new();
    for r in rows {
        groups
            .entry((
                r["stage"].as_str().unwrap().into(),
                r["compute"].as_bool().unwrap(),
                r["structured_mode"].as_str().unwrap().into(),
            ))
            .or_default()
            .push(r.clone());
    }
    groups.into_iter().map(|((stage,compute,mode),rows)|json!({"stage":stage,"compute":compute,"structured_mode":mode,"metrics":stage_metrics(&rows)[0]})).collect()
}
fn costs(id: &str, rows: &[Value]) -> Value {
    let mut cost = compare_cost(id, rows);
    cost["automatic_fields_only"] = json!(true);
    let mut by_sample = std::collections::BTreeMap::<(String, String), Vec<&Value>>::new();
    for r in rows {
        by_sample
            .entry((r["stage"].to_string(), r["id"].to_string()))
            .or_default()
            .push(r);
    }
    let confirmed = by_sample
        .values()
        .filter(|rs| {
            rs.len() == 2
                && rs
                    .iter()
                    .all(|r| r["manual_review"]["status"] == "confirmed")
        })
        .count();
    cost["human_confirmed_stable_samples"] = json!(confirmed);
    cost["cost_per_human_confirmed_sample_cny"] = json!(
        cost["estimated_total_cny"]
            .as_f64()
            .filter(|_| confirmed > 0)
            .map(|n| n / confirmed as f64)
    );
    cost
}
fn recall_summaries(rows: &[Value], cases: &[Case]) -> Vec<Value> {
    stage_summaries(rows, cases)
        .into_iter()
        .filter(|s| matches!(s["stage"].as_str(), Some("r1" | "r4")))
        .collect()
}
fn report(
    groups: &[Vec<Case>],
    reports: &[Value],
    sent: usize,
    smoke: bool,
    gates: Value,
) -> Value {
    let rows: Vec<Value> = groups
        .iter()
        .zip(reports)
        .flat_map(|(g, r)| {
            items(&r["results"]).iter().zip(g).map(|(r, c)| {
                let mut row = r.clone();
                row["compute"] = json!(compute(c));
                row
            })
        })
        .collect();
    let cases: Vec<_> = groups.iter().flatten().cloned().collect();
    let review = recall_review::summary(&rows);
    let queue: Vec<_> = items(&review)
        .iter()
        .flat_map(|r| items(&r["review_queue"]).iter().cloned())
        .collect();
    let ps = profiles(include_str!("providers.json")).unwrap();
    json!({"comparison":if smoke {"af230-smoke"} else {"af230"},"authorized_slots":AUTHORIZED,
        "available_slots":AVAILABLE,"revoked_formal_slots":REVOKED,
        "historical_smoke_slots":HISTORICAL_SMOKE,"additional_smoke_slots":ADDITIONAL_SMOKE,
        "smoke_slots":SMOKE,"formal_slots":FORMAL,"fixed_slots":if smoke {SMOKE} else {FORMAL},"profiles":IDS,
        "sent_calls":sent,"automatic_fields_only":true,"smoke_gates":gates,
        "summary":recall_summaries(&rows,&cases),"splits":split_reports(&rows),"per_sample":per_sample(&rows),
        "review_summary":review,"review_queue":queue,
        "profile_reports":IDS.iter().map(|id|{
            let rows: Vec<_>=rows.iter().filter(|r|r["model"]==*id).cloned().collect();
            let p = ps.iter().find(|p|p.id==*id).unwrap();
            json!({"profile":id,"roles":{"r1":p.r1,"r4":p.r4,"r4_compute":p.r4_compute},
                "configuration_note":p.note,"automatic_fields":recall_summaries(&rows,&cases),
                "review_summary":recall_review::summary(&rows),"latency":latency_splits(&rows),"cost":costs(id,&rows),
                "stages":(["r1","r4"]).iter().map(|s|{
                    let rs:Vec<_>=rows.iter().filter(|r|r["stage"]==*s).cloned().collect();
                    json!({"stage":s,"cost":costs(id,&rs),"usage":{
                        "known_input_tokens":rs.iter().filter_map(|r|r["usage"]["input_tokens"].as_u64()).sum::<u64>(),
                        "known_output_tokens":rs.iter().filter_map(|r|r["usage"]["output_tokens"].as_u64()).sum::<u64>(),
                        "reasoning_tokens":rs.iter().filter_map(|r|r.pointer("/usage/output_tokens_details/reasoning_tokens").and_then(Value::as_u64)).sum::<u64>(),
                        "reasoning_known_attempts":rs.iter().filter(|r|r.pointer("/usage/output_tokens_details/reasoning_tokens").and_then(Value::as_u64).is_some()).count()
                    }})
                }).collect::<Vec<_>>()})
        }).collect::<Vec<_>>(),
        "note":"自动字段结果不等于完整语义确认；人工确认另列；独立阶段延迟不能相加当端到端延迟。官方价估算，代理实际计费可能不同。",
        "results":rows})
}
fn markdown(r: &Value) -> String {
    let mut text = format!(
        "# AF230 召回对比\n\n本次发送 {}；历史授权546 + 新冒烟3 = 549，撤销Haiku正式88槽，可用461（旧冒烟18 + 新冒烟3 + 正式440）。ds-flash评测上限16384，与生产普通阶段4096不同。冒烟只核协议、结构、raw→final机械核验及usage，语义评分不阻断正式。自动字段通过与人工确认分列。\n\n| 档案 | 阶段 | 自动通过/槽位 | 人工确认 | 待复核 | 估算费用（元） | 每两次字段均通过样本（元） |\n|---|---|---|---|---|---|---|\n",
        r["sent_calls"]
    );
    for p in items(&r["profile_reports"]) {
        for (i, s) in items(&p["automatic_fields"]).iter().enumerate() {
            text.push_str(&format!(
                "| {} | {} | {}/{} | {} | {} | {} | {} |\n",
                p["profile"].as_str().unwrap(),
                s["stage"].as_str().unwrap(),
                s["passed"],
                s["total"],
                p["review_summary"][i]["human_confirmed"],
                p["review_summary"][i]["pending"],
                p["stages"][i]["cost"]["estimated_total_cny"],
                p["stages"][i]["cost"]["cost_per_stable_passed_sample_cny"]
            ));
        }
    }
    text.push_str("\n官方价估算（AF163/AF165），代理实际计费可能不同；null 表示没有通过样本或 usage 不全，不能算作免费。\n\n| 档案 | 阶段 | 计算 | 结构化模式 | 计时条数 | P50 ms | P90 ms |\n|---|---|---|---|---|---|---|\n");
    for p in items(&r["profile_reports"]) {
        for l in items(&p["latency"]) {
            text.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} |\n",
                p["profile"].as_str().unwrap(),
                l["stage"].as_str().unwrap(),
                l["compute"],
                l["structured_mode"].as_str().unwrap(),
                l["metrics"]["elapsed_ms"]["count"],
                l["metrics"]["elapsed_ms"]["p50"],
                l["metrics"]["elapsed_ms"]["p90"]
            ));
        }
    }
    text.push_str("\n原生失败停止该档，须另批；不自动 prompt_schema 重发。JSON 保留 raw/final、review_queue、分阶段 usage/费用与冒烟门控原因。\n");
    text
}

pub fn run_from_env() -> Result<(), String> {
    let variable = |k: &str| std::env::var(k).ok();
    let o = options(variable)?;
    let report_dir = offline_report_dir(o.record, variable("EVAL_RECALL_REPORT_DIR"))?;
    if o.dry {
        return dry_run_from_env();
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(ROOT);
    let ps = profiles(include_str!("providers.json"))?;
    let samples = cohort()?;
    let formal = plan_groups(&root, &samples, &ps, false, variable)?;
    let smoke = plan_groups(&root.join("smoke"), &samples, &ps, true, variable)?;
    approval_check(&root, &approval(&formal, &smoke), false)?;
    let gates = smoke_gates(&smoke);
    let run_root = if o.smoke {
        root.join("smoke-af230")
    } else {
        root.clone()
    };
    let mut groups = if o.smoke { smoke } else { formal.clone() };
    if o.record && !o.smoke {
        apply_gates(&mut groups, &gates, &o.selected);
    }
    let limit = o.record.then_some(if o.smoke { 3 } else { 88 });
    let selected: Vec<_> = groups
        .iter()
        .filter(|g| o.selected.contains(&g[0].model.label))
        .collect();
    let mut pending = 0;
    for g in &selected {
        pending += preflight_with_native_stop(g, limit, true)?;
    }
    let mut sendable = Vec::new();
    for g in &selected {
        let circuits = provider_circuits_with_native_stop(g, true)?;
        sendable.extend(
            g.iter()
                .filter(|c| {
                    matches!(c.replay, Replay::Missing | Replay::Stale)
                        && !circuit_blocks(c, &circuits)
                })
                .cloned(),
        );
    }
    let keys = if pending > 0 {
        smoke_credentials(&sendable, variable)?
    } else {
        Default::default()
    };
    let http = if pending > 0 {
        Some(
            reqwest::blocking::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    if pending > 0 {
        let smoke = plan_groups(&root.join("smoke"), &samples, &ps, true, variable)?;
        approval_check(&root, &approval(&formal, &smoke), true)?;
        for g in &selected {
            compare_freeze(g)?;
        }
    }
    let mut sent = 0;
    for g in selected {
        let n = preflight_with_native_stop(g, limit, true)?;
        let _resource = if n > 0 {
            Some(w3_trial_lock(
                &root.join("locks").join(compare_resource(&g[0].model.label)),
            )?)
        } else {
            None
        };
        let r = evaluate_cases_with_native_stop(
            g,
            limit,
            &mut |c| {
                let http = http
                    .as_ref()
                    .ok_or("offline recall comparison cannot send")?;
                let request = smoke_http_request(http, c, &keys[&c.model.label])?;
                let response = http.execute(request).map_err(|e| {
                    if e.is_timeout() {
                        "provider_timeout".into()
                    } else {
                        e.to_string()
                    }
                })?;
                model_provider::read_http_response(response, c.provider.as_ref().unwrap().protocol)
                    .map(|(_, e)| e)
                    .map_err(|e| e.to_string())
            },
            true,
        )?;
        sent += r["sent_calls"].as_u64().unwrap() as usize;
    }
    let smoke = plan_groups(&root.join("smoke"), &samples, &ps, true, variable)?;
    let gates = smoke_gates(&smoke);
    let mut groups = if o.smoke {
        smoke
    } else {
        plan_groups(&root, &samples, &ps, false, variable)?
    };
    if o.record && !o.smoke {
        apply_gates(&mut groups, &gates, &o.selected);
    }
    let reports = groups
        .iter()
        .map(|g| {
            evaluate_cases_with_native_stop(g, None, &mut |_| panic!("offline report sent"), true)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let r = report(&groups, &reports, sent, o.smoke, gates);
    let out = run_root.join(report_dir).join(o.selected.join("--"));
    m15_write_json(&out.join("report.json"), &r)?;
    std::fs::write(out.join("report.md"), markdown(&r)).map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&r).map_err(|e| e.to_string())?
    );
    if o.gate
        && items(&r["results"])
            .iter()
            .any(|r| o.selected.iter().any(|id| r["model"] == *id) && r["status"] != "通过")
    {
        return Err("AF230 EVAL_GATE: selected automatic result failed/missing/refused".into());
    }
    Ok(())
}

fn offline_report_dir(record: bool, value: Option<String>) -> Result<String, String> {
    let Some(name) = value else {
        return Ok("reports-af230".into());
    };
    if record
        || !name.starts_with("reports-")
        || name.len() == "reports-".len()
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(
            "EVAL_RECALL_REPORT_DIR requires an offline reports-<revision> directory name".into(),
        );
    }
    Ok(name)
}

#[test]
fn af247_report_directory_is_offline_only_and_cannot_escape_the_report_root() {
    assert_eq!(offline_report_dir(false, None).unwrap(), "reports-af230");
    assert_eq!(
        offline_report_dir(false, Some("reports-af247".into())).unwrap(),
        "reports-af247"
    );
    for name in [
        "../recordings",
        "/tmp/reports-af247",
        "recordings",
        "reports-",
        "reports-af247/nested",
    ] {
        assert!(offline_report_dir(false, Some(name.into())).is_err());
    }
    assert!(offline_report_dir(true, Some("reports-af247".into())).is_err());
}

#[test]
fn af228_entry_requires_exact_authorization_and_rejects_conflicting_modes() {
    for value in ["1", "581", "invalid", "-1"] {
        assert!(
            options(|key| match key {
                "EVAL_RECALL_COMPARE" => Some("af224".into()),
                "EVAL_RECORD" => Some(value.into()),
                _ => None,
            })
            .is_err()
        );
    }
    assert!(
        !options(|key| match key {
            "EVAL_RECALL_COMPARE" => Some("af224".into()),
            "EVAL_RECORD" => Some("0".into()),
            _ => None,
        })
        .unwrap()
        .record
    );
    assert!(
        options(|key| match key {
            "EVAL_RECALL_COMPARE" => Some("af224".into()),
            "EVAL_COMPARE" => Some("af137".into()),
            _ => None,
        })
        .is_err()
    );
}

#[test]
fn af230_compare_plan_routes_roles_and_has_440_unique_slots_and_15_smoke_slots() {
    let root =
        std::env::temp_dir().join(format!("af224-dry-{}", hippocampus::diagnostics::new_id()));
    let ps = profiles(include_str!("providers.json")).unwrap();
    let cases = plan_groups(&root, &cohort().unwrap(), &ps, false, |_| None).unwrap();
    assert_eq!(cases.iter().map(Vec::len).sum::<usize>(), 440);
    for group in &cases {
        assert_eq!(group.len(), 88);
        assert_eq!(
            group
                .iter()
                .map(campaign_slot)
                .map(|s| s.to_string())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            88
        );
        for c in group {
            assert!(matches!(c.replay, Replay::Missing));
            let compute = c.stage == "r4"
                && matches!(
                    recall_adapter::R4Input::new(&c.sample["input"])
                        .unwrap()
                        .plan
                        .output
                        .unwrap()
                        .plan
                        .op,
                    R1Op::Count | R1Op::Sum | R1Op::Elapsed | R1Op::Compare
                );
            assert_eq!(
                c.request["max_output_tokens"],
                if compute || c.model.label == "ds-flash" {
                    16384
                } else {
                    4096
                }
            );
            assert_eq!(c.timeout_seconds, Some(if compute { 240 } else { 180 }));
            assert!(
                !c.request["input"]
                    .as_str()
                    .unwrap()
                    .contains(c.sample["id"].as_str().unwrap())
            );
            assert_eq!(
                c.provider.as_ref().unwrap().structured_mode,
                StructuredMode::NativeSchema
            );
        }
    }
    let ds = &cases[0];
    assert_eq!(
        ds.iter().find(|c| c.stage == "r1").unwrap().model.effort,
        ReasoningEffort::Minimal
    );
    assert_eq!(
        ds.iter()
            .find(|c| c.sample["id"] == "r4_01_single_supported")
            .unwrap()
            .model
            .effort,
        ReasoningEffort::Minimal
    );
    assert_eq!(
        ds.iter()
            .find(|c| c.sample["id"] == "r4_14_count_dedupe")
            .unwrap()
            .model
            .effort,
        ReasoningEffort::Low
    );
    let smoke = plan_groups(&root.join("smoke"), &cohort().unwrap(), &ps, true, |_| None).unwrap();
    assert_eq!(smoke.iter().map(Vec::len).sum::<usize>(), 15);
    let formal_count = cases[0]
        .iter()
        .find(|c| c.sample["id"] == "r4_14_count_dedupe")
        .unwrap();
    let smoke_count = smoke[0]
        .iter()
        .find(|c| c.sample["id"] == "r4_14_count_dedupe")
        .unwrap();
    assert_ne!(campaign_slot(smoke_count), campaign_slot(formal_count));
    assert!(!root.exists(), "dry-run must not create artifacts");
}

#[test]
fn af224_profiles_cannot_silently_change_effort_or_fall_back_to_prompt_schema() {
    for mutation in [
        "effort",
        "output",
        "timeout",
        "prompt_schema",
        "model",
        "thinking",
    ] {
        let mut data: Value = serde_json::from_str(include_str!("providers.json")).unwrap();
        match mutation {
            "effort" => data["profiles"][0]["r4"]["effort"] = json!("low"),
            "output" => data["profiles"][0]["r1"]["max_output_tokens"] = json!(16384),
            "timeout" => data["profiles"][0]["r1"]["timeout_seconds"] = json!(360),
            "prompt_schema" => {
                data["profiles"][4]["provider"]["structured_mode"] = json!("prompt_schema")
            }
            "model" => data["profiles"][0]["model"] = json!("ep-other"),
            _ => data["profiles"][6]["provider"]["thinking"] = json!(true),
        }
        assert!(profiles(&data.to_string()).is_err(), "{mutation}");
    }
}

#[test]
fn af224_strict_bindings_reuse_campaign_interruption_circuit_and_cost_rules() {
    let root = std::env::temp_dir().join(format!(
        "af224-campaign-{}",
        hippocampus::diagnostics::new_id()
    ));
    let ps = profiles(include_str!("providers.json")).unwrap();
    let mut group = plan_groups(&root, &cohort().unwrap(), &ps[..1], true, |_| None)
        .unwrap()
        .remove(0);
    compare_freeze(&group).unwrap();
    campaign_check(&group, 3).unwrap();
    assert!(campaign_check(&group, 4).is_err());
    let old = case_fingerprint(&group[0]);
    group[0].timeout_seconds = Some(181);
    assert_ne!(old, case_fingerprint(&group[0]));
    assert!(campaign_check(&group, 3).is_err());
    group[0].timeout_seconds = Some(180);
    let log = campaign_start(&group[0]).unwrap();
    assert!(log.exists());
    compare_mark_interrupted(&mut group).unwrap();
    assert!(matches!(group[0].replay, Replay::Interrupted));
    assert_eq!(
        count_sends([group[0].campaign.as_ref().unwrap().join("sends")]).unwrap(),
        1
    );
    let mut circuits = ProviderCircuits::new();
    let circuit = circuits.entry("ds-pro".into()).or_default();
    for _ in 0..3 {
        circuit.observe(
            &json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}),
        );
    }
    persist_provider_circuits(&group, &circuits).unwrap();
    let loaded = provider_circuits(&group).unwrap();
    assert!(!circuit_blocks(&group[0], &loaded));
    assert!(circuit_blocks(&group[1], &loaded));
    let cost = compare_cost(
        "ds-pro",
        &[
            json!({"id":"sample","stage":"r1","status":"失败","usage":{"input_tokens":1000,"output_tokens":1000}}),
        ],
    );
    assert_eq!(cost["estimated_total_cny"], json!(0.036));
    assert!(cost["cost_per_passed_attempt_cny"].is_null());
    std::fs::remove_dir_all(root).unwrap();
}

fn fixture_response(c: &Case) -> Value {
    let output = if c.stage == "r1" {
        fake_r1(&c.sample)
    } else {
        fake_r4(&c.sample)
    };
    let text = output.to_string();
    match c.provider.as_ref().unwrap().protocol {
        ProviderProtocol::OpenaiResponses => fake_envelope(output),
        ProviderProtocol::AnthropicMessages => {
            json!({"stop_reason":"end_turn","usage":{"input_tokens":12,"output_tokens":7},"content":[{"type":"text","text":text}]})
        }
        ProviderProtocol::OpenaiChat => {
            json!({"usage":{"prompt_tokens":12,"completion_tokens":7},"choices":[{"finish_reason":"stop","message":{"content":text}}]})
        }
    }
}

fn fixture_smoke(group: &mut [Case]) {
    for c in group {
        c.replay = Replay::Output(
            if c.stage == "r1" {
                fake_r1(&c.sample)
            } else {
                fake_r4(&c.sample)
            }
            .to_string(),
        );
        c.record = json!({"response":{"status":"completed"},"usage":{"input_tokens":12,"output_tokens":7}});
    }
}

#[test]
fn af230_smoke_ignores_semantic_labels_but_requires_final_mechanical_validation() {
    let temp = TempRecords::new();
    let ps = profiles(include_str!("providers.json")).unwrap();
    let mut g = plan_groups(
        &temp.0.join("smoke"),
        &cohort().unwrap(),
        &ps[..1],
        true,
        |_| None,
    )
    .unwrap()
    .remove(0);
    fixture_smoke(&mut g);
    g[0].sample["expect"]["need"] = json!("no_recall");
    g[1].sample["expect"]["facts"][0]["record_ids"] = json!([999]);
    let mut branches = expectation_branches(&g[2].sample["expect"]);
    for branch in &mut branches {
        branch["compute"]["expected_bound"] = json!("wrong-semantic-bound");
    }
    g[2].sample["expect"] = json!({"alternatives":branches});
    for c in &g {
        let Replay::Output(text) = &c.replay else {
            unreachable!()
        };
        assert_eq!(score(&c.stage, &c.sample, Some(text))["status"], "失败");
    }
    smoke_check(&g).unwrap();
    let mut invalid = g.clone();
    let Replay::Output(text) = &invalid[1].replay else {
        unreachable!()
    };
    let mut output: Value = serde_json::from_str(text).unwrap();
    output["facts"][0]["evidence"][0]["quote"]["text"] = json!("not present in the source");
    invalid[1].replay = Replay::Output(output.to_string());
    assert!(
        smoke_check(&invalid).is_err(),
        "semantic relaxation must not exempt bad references"
    );
    for field in ["status", "usage", "schema"] {
        let mut invalid = g.clone();
        match field {
            "status" => invalid[0].record["response"]["status"] = json!("incomplete"),
            "usage" => invalid[0].record["usage"]["output_tokens"] = Value::Null,
            _ => invalid[0].replay = Replay::Output("{}".into()),
        }
        assert!(smoke_check(&invalid).is_err(), "{field}");
    }
}

#[test]
fn af230_smoke_allows_raw_basis_failure_only_when_z18_final_validates() {
    let temp = TempRecords::new();
    let ps = profiles(include_str!("providers.json")).unwrap();
    let mut g = plan_groups(
        &temp.0.join("smoke"),
        &cohort().unwrap(),
        &ps[..1],
        true,
        |_| None,
    )
    .unwrap()
    .remove(0);
    fixture_smoke(&mut g);
    let mut sample = samples("r4")
        .into_iter()
        .find(|s| s["id"] == "r4_10_superseded_history")
        .unwrap();
    sample["expect"]["facts"]
        .as_array_mut()
        .unwrap()
        .push(json!({"record_ids":[11],"sources":["s-11"],"must":{"current":"yes"}}));
    let mut output = fake_r4(&sample);
    output["facts"][1]["supersedes_record_ids"] = json!([10]);
    g[1].sample = sample;
    g[1].request = stage_request("r4", &g[1].sample).unwrap();
    g[1].replay = Replay::Output(output.to_string());
    let row = score("r4", &g[1].sample, Some(&output.to_string()));
    assert_eq!(row["raw_result"]["status"], "失败");
    assert_eq!(row["final_result"]["status"], "通过");
    smoke_check(&g).unwrap();
    output["facts"][1]["supersedes_record_ids"] = json!([999]);
    g[1].replay = Replay::Output(output.to_string());
    assert!(smoke_check(&g).is_err());
}

#[test]
fn af243_huoshan_routing_keeps_historical_requests_and_frozen_authorizations() {
    let temp = TempRecords::new();
    let ps = profiles(include_str!("providers.json")).unwrap();
    let mut historical = ps.clone();
    for p in &mut historical {
        p.endpoint_env = None;
    }
    let samples = cohort().unwrap();
    let variable = |name: &str| match name {
        "HUOSHAN_DEEPSEEK_BASE_URL" => Some("https://ark.fixture.invalid/api/v3".into()),
        "HUOSHAN_DEEPSEEK_API_KEY" => Some("huoshan-fixture-key".into()),
        "DEEPSEEK_BASE_URL" => Some("https://api.deepseek.com".into()),
        "DEEPSEEK_API_KEY" => panic!("Huoshan must not read the official key"),
        _ => None,
    };
    let old = plan_groups(&temp.0, &samples, &historical, false, variable).unwrap();
    let new = plan_groups(&temp.0, &samples, &ps, false, variable).unwrap();
    let old_smoke =
        plan_groups(&temp.0.join("smoke"), &samples, &historical, true, variable).unwrap();
    let new_smoke = plan_groups(&temp.0.join("smoke"), &samples, &ps, true, variable).unwrap();
    assert_eq!(approval(&old, &old_smoke), approval(&new, &new_smoke));
    for (old, new) in old.iter().take(2).zip(new.iter().take(2)) {
        for (a, b) in old.iter().zip(new) {
            assert_eq!(a.request, b.request);
            assert_eq!(a.wire_request, b.wire_request);
            assert_eq!(case_fingerprint(a), case_fingerprint(b));
            assert_eq!(campaign_slot(a), campaign_slot(b));
            assert_eq!(
                b.provider.as_ref().unwrap().base_url_env,
                "DEEPSEEK_BASE_URL"
            );
            assert_eq!(
                b.provider.as_ref().unwrap().api_key_env.as_deref(),
                Some("DEEPSEEK_API_KEY")
            );
            assert_eq!(
                b.model.base.as_deref(),
                Some("https://ark.fixture.invalid/api/v3")
            );
        }
        assert_eq!(
            smoke_credentials(&new[..1], variable).unwrap()[&new[0].model.label],
            "huoshan-fixture-key"
        );
        assert!(
            smoke_credentials(&new[..1], |name| if name == "DEEPSEEK_BASE_URL" {
                Some("https://api.deepseek.com".into())
            } else {
                None
            })
            .unwrap_err()
            .contains("HUOSHAN_DEEPSEEK_BASE_URL")
        );
    }
    let empty = plan_groups(&temp.0, &samples, &ps[..2], true, |_| None).unwrap();
    assert_eq!(
        empty[0][0].model.base.as_deref(),
        Some("https://deepseek_base_url.invalid/v1")
    );
    let old_group = &old_smoke[0];
    compare_freeze(old_group).unwrap();
    evaluate_cases(old_group, Some(3), &mut |c| Ok(fixture_response(c))).unwrap();
    let replay = plan_groups(&temp.0.join("smoke"), &samples, &ps[..1], true, |name| {
        assert_ne!(name, "DEEPSEEK_BASE_URL");
        assert_ne!(name, "DEEPSEEK_API_KEY");
        None
    })
    .unwrap();
    assert!(
        replay[0]
            .iter()
            .all(|c| matches!(c.replay, Replay::Output(_)))
    );
    campaign_check(&replay[0], 3).unwrap();
    assert_eq!(
        evaluate_cases(&replay[0], None, &mut |_| panic!("strict replay sent")).unwrap()["sent_calls"],
        0
    );
}

#[test]
fn af230_amendment_preserves_legacy_approval_smoke_and_strict_flash_identity() {
    let temp = TempRecords::new();
    let historical = legacy_approval().unwrap();
    m15_write_json(&temp.0.join("authorization.json"), &historical).unwrap();
    let original = std::fs::read(temp.0.join("authorization.json")).unwrap();
    let ps = profiles(include_str!("providers.json")).unwrap();
    let mut old_flash = ps[1].clone();
    old_flash.r1.max_output_tokens = 4096;
    old_flash.r4.max_output_tokens = 4096;
    let samples = cohort().unwrap();
    let old = plan_groups(&temp.0.join("smoke"), &samples, &[old_flash], true, |_| {
        None
    })
    .unwrap()
    .remove(0);
    compare_freeze(&old).unwrap();
    evaluate_cases_with_native_stop(&old, Some(3), &mut |c| Ok(fixture_response(c)), true).unwrap();
    let old_manifest =
        std::fs::read(old[0].campaign.as_ref().unwrap().join("authorization.json")).unwrap();
    let formal = plan_groups(&temp.0, &samples, &ps, false, |_| None).unwrap();
    let smoke = plan_groups(&temp.0.join("smoke"), &samples, &ps, true, |_| None).unwrap();
    assert!(smoke[1].iter().all(|c| matches!(c.replay, Replay::Missing)));
    assert!(smoke[1][0].path.starts_with(temp.0.join("smoke-af230")));
    assert_ne!(case_fingerprint(&old[0]), case_fingerprint(&smoke[1][0]));
    assert!(smoke_check(&smoke[1]).is_err());
    approval_check(&temp.0, &approval(&formal, &smoke), true).unwrap();
    assert_eq!(
        std::fs::read(temp.0.join("authorization.json")).unwrap(),
        original
    );
    assert_eq!(
        std::fs::read(old[0].campaign.as_ref().unwrap().join("authorization.json")).unwrap(),
        old_manifest
    );
    assert_eq!(
        count_sends([old[0].campaign.as_ref().unwrap().join("sends")]).unwrap(),
        3
    );
    assert!(!temp.0.join("campaigns/claude-haiku-5-5").exists());
    let current: Value =
        serde_json::from_slice(&std::fs::read(temp.0.join("authorization-af230.json")).unwrap())
            .unwrap();
    assert_eq!(current["available"], 461);
    assert_eq!(current["revoked_formal"], 88);
    assert_eq!(
        current["previous_authorization"],
        hippocampus::recall_models::fingerprint(&historical)
    );
    let mut corrupted = historical;
    corrupted["formal_requests"][0]["binding"] = json!("tampered");
    m15_write_json(&temp.0.join("authorization.json"), &corrupted).unwrap();
    assert!(approval_check(&temp.0, &approval(&formal, &smoke), false).is_err());
}

#[test]
fn af230_freezes_549_and_revokes_haiku_without_expanding_subsets() {
    let temp = TempRecords::new();
    let ps = profiles(include_str!("providers.json")).unwrap();
    let formal = plan_groups(&temp.0, &cohort().unwrap(), &ps, false, |_| None).unwrap();
    let smoke = plan_groups(&temp.0.join("smoke"), &cohort().unwrap(), &ps, true, |_| {
        None
    })
    .unwrap();
    let approved = approval(&formal, &smoke);
    approval_check(&temp.0, &approved, false).unwrap();
    assert!(!temp.0.join("authorization-af230.json").exists());
    approval_check(&temp.0, &approved, true).unwrap();
    approval_check(&temp.0, &approved, true).unwrap();
    let mut changed = approved.clone();
    changed["formal_requests"][0]["binding"] = json!("changed-request");
    assert!(approval_check(&temp.0, &changed, false).is_err());
    let mut changed = approved.clone();
    changed["total"] = json!(547);
    assert!(approval_check(&temp.0, &changed, true).is_err());
    for mode in ["af228", "af228-smoke"] {
        let o = options(|k| match k {
            "EVAL_RECALL_COMPARE" => Some(mode.into()),
            "EVAL_RECORD" => Some("549".into()),
            "EVAL_RECALL_PROFILES" => Some("ds-pro".into()),
            _ => None,
        })
        .unwrap();
        assert!(o.record);
        assert_eq!(o.selected, vec!["ds-pro"]);
    }
    assert!(
        options(|k| match k {
            "EVAL_RECALL_COMPARE" => Some("af228".into()),
            "EVAL_RECALL_PROFILES" => Some("claude-sonnet-5-5".into()),
            _ => None,
        })
        .is_err()
    );
    assert!(
        options(|k| match k {
            "EVAL_RECALL_COMPARE" => Some("af230".into()),
            "EVAL_RECALL_PROFILES" => Some("claude-haiku-5-5".into()),
            _ => None,
        })
        .is_err()
    );
    for n in ["18", "88", "440", "528", "546", "547"] {
        assert!(
            options(|k| match k {
                "EVAL_RECALL_COMPARE" => Some("af228".into()),
                "EVAL_RECORD" => Some(n.into()),
                _ => None,
            })
            .is_err()
        );
    }
    assert_eq!(formal.len(), 5);
    assert_eq!(smoke.len(), 5);
    for g in &formal {
        compare_freeze(g).unwrap();
        campaign_check(&g[..2], 88).unwrap();
        assert!(campaign_check(g, 89).is_err());
    }
    // Removed Haiku slots cannot be recycled; historical + replacement sends are both counted.
    let old_sends = temp.0.join("smoke/campaigns/claude-haiku-5-5/sends");
    for i in 0..HISTORICAL_SMOKE {
        m15_write_json(
            &old_sends.join(format!("{i}.json")),
            &json!({"state":"started"}),
        )
        .unwrap();
    }
    let new_sends = temp.0.join("smoke-af230/campaigns/ds-flash/sends");
    for i in 0..AVAILABLE - HISTORICAL_SMOKE {
        m15_write_json(
            &new_sends.join(format!("{i}.json")),
            &json!({"state":"responded"}),
        )
        .unwrap();
    }
    approval_check(&temp.0, &approved, false).unwrap();
    m15_write_json(
        &new_sends.join("overflow.json"),
        &json!({"state":"started"}),
    )
    .unwrap();
    assert!(approval_check(&temp.0, &approved, false).is_err());
}

#[test]
fn af228_all_three_strict_smokes_are_required_before_formal_send() {
    let temp = TempRecords::new();
    let ps = profiles(include_str!("providers.json")).unwrap();
    let samples = cohort().unwrap();
    let smoke = plan_groups(&temp.0.join("smoke"), &samples, &ps, true, |_| None).unwrap();
    let mut previous: Option<PathBuf> = None;
    for g in &smoke {
        assert!(smoke_check(g).is_err());
        compare_freeze(g).unwrap();
        let r = evaluate_cases_with_native_stop(
            g,
            Some(3),
            &mut |c| {
                assert!(
                    count_sends([c.campaign.as_ref().unwrap().join("sends")]).unwrap() > 0,
                    "send must already be durable"
                );
                if let Some(path) = &previous {
                    assert!(path.exists(), "previous call must already be recorded");
                }
                previous = Some(c.path.clone());
                Ok(fixture_response(c))
            },
            true,
        )
        .unwrap();
        assert_eq!(r["sent_calls"], 3);
        assert_eq!(r["review_summary"][0]["human_confirmed"], 0);
    }
    let smoke = plan_groups(&temp.0.join("smoke"), &samples, &ps, true, |_| None).unwrap();
    for g in &smoke {
        smoke_check(g).unwrap();
    }
    let gates = smoke_gates(&smoke);
    assert!(items(&gates).iter().all(|g| g["passed"] == true));
    let mut broken = smoke[0].clone();
    broken.pop();
    assert!(smoke_check(&broken).is_err());
    let mut broken = smoke[0].clone();
    broken[0].record["usage"] = Value::Null;
    broken[0].record["response"]["usage"] = Value::Null;
    assert!(smoke_check(&broken).is_err());
    let mut changed = ps.clone();
    changed[0].r1.timeout_seconds += 1;
    assert!(
        plan_groups(&temp.0.join("smoke"), &samples, &changed, true, |_| None).is_err(),
        "changed frozen request cannot reuse smoke approval"
    );
    let mut stale = smoke[0].clone();
    stale[0].replay = Replay::Stale;
    assert!(smoke_check(&stale).is_err());
    let mut formal = plan_groups(&temp.0, &samples, &ps, false, |_| None).unwrap();
    let mut one_bad = gates.clone();
    one_bad[0]["passed"] = json!(false);
    let selected = IDS.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    apply_gates(&mut formal, &one_bad, &selected);
    assert_eq!(
        preflight_with_native_stop(&formal[0], Some(88), true).unwrap(),
        0
    );
    assert_eq!(
        preflight_with_native_stop(&formal[1], Some(88), true).unwrap(),
        88
    );
    let r = evaluate_cases_with_native_stop(
        &formal[0],
        Some(88),
        &mut |_| panic!("failed smoke must never send"),
        true,
    )
    .unwrap();
    assert!(
        items(&r["results"])
            .iter()
            .all(|r| r["status"] == "发送前拒绝")
    );
    assert!(!temp.0.join("campaigns/ds-pro/sends").exists());
}

#[test]
fn af228_resume_skips_inflight_and_never_resends_or_expands_allowance() {
    let temp = TempRecords::new();
    let ps = profiles(include_str!("providers.json")).unwrap();
    let mut group = plan_groups(&temp.0, &cohort().unwrap(), &ps[..1], false, |_| None)
        .unwrap()
        .remove(0);
    compare_freeze(&group).unwrap();
    campaign_start(&group[0]).unwrap();
    compare_mark_interrupted(&mut group).unwrap();
    let r = evaluate_cases_with_native_stop(
        &group[..4],
        Some(88),
        &mut |c| Ok(fixture_response(c)),
        true,
    )
    .unwrap();
    assert_eq!(r["sent_calls"], 3);
    assert_eq!(r["results"][0]["status"], "未录制（中断在途）");
    let group = plan_groups(&temp.0, &cohort().unwrap(), &ps[..1], false, |_| None)
        .unwrap()
        .remove(0);
    assert_eq!(
        preflight_with_native_stop(&group, Some(88), true).unwrap(),
        84
    );
    let r = evaluate_cases_with_native_stop(
        &group[..4],
        Some(88),
        &mut |_| panic!("resume must not resend"),
        true,
    )
    .unwrap();
    assert_eq!(r["sent_calls"], 0);
    assert_eq!(
        count_sends([group[0].campaign.as_ref().unwrap().join("sends")]).unwrap(),
        4
    );
    let cost = compare_cost("ds-pro", items(&r["results"]));
    assert_eq!(cost["unknown_usage_or_price_attempts"], 1);
    assert!(cost["estimated_total_cny"].is_null());
}

#[test]
fn af228_native_failure_stops_after_one_while_other_providers_trip_after_three() {
    for (id, incomplete, expected) in [
        ("claude-haiku-5-5", false, 1),
        ("ds-pro", false, 3),
        ("qwen38-flash-nothink", true, 3),
    ] {
        let temp = TempRecords::new();
        let ps = configured_profiles(include_str!("providers.json"))
            .unwrap()
            .into_iter()
            .filter(|p| p.id == id)
            .collect::<Vec<_>>();
        let group = plan_groups(&temp.0, &cohort().unwrap(), &ps, false, |_| None)
            .unwrap()
            .remove(0);
        compare_freeze(&group).unwrap();
        let r=evaluate_cases_with_native_stop(&group,Some(88),&mut |_|Ok(if incomplete {
            json!({"usage":{"prompt_tokens":12,"completion_tokens":4096},"choices":[{"finish_reason":"length","message":{"content":"DIAGNOSTIC_ONLY","reasoning_content":"REASONING_ONLY"}}]})
        } else {json!({"provider_http_error":{"http_status":400,"error_type":"invalid_request_error","code":"schema_grammar_too_large"}})}),true).unwrap();
        assert_eq!(r["sent_calls"], expected);
        assert_eq!(r["circuit_not_sent"], 88 - expected);
        assert_eq!(
            r["provider_circuits"][id]["consecutive"],
            if id.starts_with("claude") { 1 } else { 3 }
        );
        let g = plan_groups(&temp.0, &cohort().unwrap(), &ps, false, |_| None)
            .unwrap()
            .remove(0);
        // Native failures must still stop after losing the separate circuit write.
        if id.starts_with("claude") {
            std::fs::remove_file(
                g[0].campaign
                    .as_ref()
                    .unwrap()
                    .join("provider_circuit.json"),
            )
            .unwrap();
        }
        assert_eq!(preflight_with_native_stop(&g, Some(88), true).unwrap(), 0);
        let replay = evaluate_cases_with_native_stop(
            &g,
            Some(88),
            &mut |_| panic!("open circuit must not send"),
            true,
        )
        .unwrap();
        assert_eq!(replay["sent_calls"], 0);
        assert_eq!(
            count_sends([g[0].campaign.as_ref().unwrap().join("sends")]).unwrap(),
            expected
        );
        if incomplete {
            assert!(replay["results"][0]["diagnostic_fragments"].is_object());
        }
    }
}

#[test]
fn af228_report_separates_stages_compute_modes_usage_cost_and_human_review() {
    let temp = TempRecords::new();
    let ps = profiles(include_str!("providers.json")).unwrap();
    let mut groups = plan_groups(&temp.0.join("smoke"), &cohort().unwrap(), &ps, true, |_| {
        None
    })
    .unwrap();
    for g in &mut groups {
        for (i, c) in g.iter_mut().enumerate() {
            c.replay = Replay::Output(
                if c.stage == "r1" {
                    fake_r1(&c.sample)
                } else {
                    fake_r4(&c.sample)
                }
                .to_string(),
            );
            c.record = json!({"elapsed_ms":(i+1)*100,"usage":{"input_tokens":1000,"output_tokens":1000,"input_tokens_details":{"cached_tokens":100},"output_tokens_details":{"reasoning_tokens":0}}});
        }
    }
    let reports = groups
        .iter()
        .map(|g| {
            evaluate_cases_with_native_stop(g, None, &mut |_| panic!("report cannot send"), true)
                .unwrap()
        })
        .collect::<Vec<_>>();
    let r = report(&groups, &reports, 0, true, json!([]));
    assert_eq!(r["sent_calls"], 0);
    assert_eq!(r["authorized_slots"], 549);
    assert_eq!(r["formal_slots"], 440);
    assert_eq!(
        r["profile_reports"][1]["roles"]["r1"]["max_output_tokens"],
        16384
    );
    assert!(
        r["profile_reports"][1]["configuration_note"]
            .as_str()
            .unwrap()
            .contains("与生产")
    );
    assert_eq!(r["summary"].as_array().unwrap().len(), 2);
    assert_eq!(r["summary"][0]["passed"], 5);
    assert_eq!(r["summary"][1]["passed"], 10);
    assert_eq!(
        r["profile_reports"][0]["latency"].as_array().unwrap().len(),
        3
    );
    assert_eq!(r["profile_reports"][0]["latency"][2]["compute"], true);
    assert_eq!(
        r["profile_reports"][0]["latency"][2]["metrics"]["elapsed_ms"]["p50"],
        300
    );
    assert_eq!(
        r["profile_reports"][0]["latency"][2]["metrics"]["elapsed_ms"]["p90"],
        300
    );
    assert_eq!(r["review_summary"][0]["human_confirmed"], 0);
    assert_eq!(r["review_summary"][0]["pending"], 5);
    assert_eq!(
        r["review_queue"].as_array().unwrap().len(),
        3,
        "same input/output review deduplicates across providers"
    );
    let qwen = &r["profile_reports"][4]["cost"];
    assert!((qwen["estimated_total_cny"].as_f64().unwrap() - 0.01029).abs() < 1e-9);
    assert!(qwen["cost_per_human_confirmed_sample_cny"].is_null());
    assert!(markdown(&r).contains("P90 ms"));
    assert!(
        items(&r["results"])
            .iter()
            .all(|r| r["final_output"].is_object())
    );
}
