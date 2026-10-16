//! Shared AF302/AF319 accounting and label-only amendments.
use super::*;
pub(super) fn rates(id: &str) -> (u64, u64, u64) {
    match id {
        "ds-pro" | "ds-pro-minimal" | "ds-pro-low" => (9000, 27000, 300),
        "gpt-6.1-sol" => (14200, 71000, 14200),
        _ => (710, 3550, 710),
    }
}
pub(super) fn input_bound(c: &Case) -> u64 {
    // Conservative byte estimate, including schema and protocol framing allowance.
    serde_json::to_vec(&c.wire_request).unwrap().len() as u64 + 1024
}
pub(super) fn upper(c: &Case) -> u64 {
    let (i, o, _) = rates(&c.model.label);
    input_bound(c) * i + c.request["max_output_tokens"].as_u64().unwrap() * o
}
pub(super) fn actual(c: &Case, usage: &Value) -> Option<u64> {
    let input = usage["input_tokens"].as_u64()?;
    let output = usage["output_tokens"].as_u64()?;
    let cached = usage["input_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap_or(0);
    if cached > input {
        return None;
    }
    let (i, o, cache) = rates(&c.model.label);
    Some((input - cached) * i + cached * cache + output * o)
}
pub(super) fn key(c: &Case) -> String {
    hippocampus::recall_models::fingerprint(&campaign_slot(c))
}
pub(super) fn read(path: &Path, default: Value) -> Result<Value, String> {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(default),
        Err(e) => Err(e.to_string()),
    }
}
pub(super) fn total(ledger: &Value) -> Result<u64, String> {
    ledger
        .as_object()
        .ok_or("invalid cost ledger")?
        .values()
        .try_fold(0u64, |n, e| {
            n.checked_add(e["charged_nano_cny"].as_u64().ok_or("invalid cost entry")?)
                .ok_or("cost overflow".into())
        })
}
pub(super) const TRANSPORT: &str = "af308_transport_failed: ";
pub(super) fn transport_error(error: &Value) -> bool {
    error
        .as_str()
        .is_some_and(|s| s.starts_with(TRANSPORT) || s.starts_with("error sending request"))
}
pub(super) fn transport_log(log: &Value) -> bool {
    log["state"] == "responded"
        && transport_error(&log["error"])
        && log["usage"].is_null()
        && log["status"].is_null()
        && log["provider_http_error"].is_null()
}
pub(super) fn transport_record(record: &Value) -> bool {
    transport_error(&record["error"])
        && record["usage"].is_null()
        && record["response"]["usage"].is_null()
        && record["response"]["status"].is_null()
        && record["provider_http_error"].is_null()
}
pub(super) fn campaign_manifest(root: &Path) -> Result<Option<Value>, String> {
    let Some(base) = root.parent().and_then(Path::parent) else {
        return Ok(None);
    };
    let m = read(&base.join("manifest.json"), Value::Null)?;
    Ok(matches!(
        m["version"].as_str(),
        Some("af302.v1" | "af319.v1" | "af342.v1" | "af352.v1")
    )
    .then_some(m))
}
pub(super) fn logs(root: &Path) -> Result<Vec<Value>, String> {
    let dir = root.join("sends");
    if !dir.exists() {
        return Ok(vec![]);
    }
    std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| match e {
            Ok(e) if e.path().extension().is_some_and(|s| s == "json") => {
                Some(read(&e.path(), Value::Null))
            }
            Ok(_) => None,
            Err(e) => Some(Err(e.to_string())),
        })
        .collect()
}
// Only opted-in cost-campaign retries may discount physical attempts from the slot quota.
pub(super) fn occupied_slots(root: &Path) -> Result<Option<usize>, String> {
    if campaign_manifest(root)?.is_none() {
        return Ok(None);
    }
    Ok(Some(
        logs(root)?
            .iter()
            .map(|l| l["slot"].to_string())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
    ))
}
pub(super) fn retryable_send(case: &Case, log: &Value) -> Result<bool, String> {
    let Some(root) = &case.campaign else {
        return Ok(false);
    };
    let Some(m) = campaign_manifest(root)? else {
        return Ok(false);
    };
    let slot = campaign_slot(case);
    if !items(&m["profiles"])
        .iter()
        .any(|p| items(&p["slots"]).iter().any(|s| s["slot"] == slot))
    {
        return Ok(false);
    }
    let ledger = read(
        &root
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("cost-ledger.json"),
        Value::Null,
    )?;
    let entry = &ledger[key(case)];
    Ok(transport_log(log)
        && (entry["state"] == "transport_failed"
            || (entry["state"] == "reserved" && entry["retry_transport"] == true)))
}
pub(super) fn settlement(c: &Case, usage: &Value, transport: bool) -> Value {
    let free = transport && usage.is_null();
    json!({"state":if free {"transport_failed"}else{"settled"},
        "charged_nano_cny":if free {0}else{actual(c,usage).unwrap_or(upper(c))},
        "upper_nano_cny":upper(c),"usage":usage})
}
pub(super) fn enable_retries(groups: &mut [Vec<Case>], ledger: &Value) -> Result<(), String> {
    for c in groups.iter_mut().flatten() {
        if ledger[key(c)]["state"] != "transport_failed" {
            continue;
        }
        if !matches!(&c.replay,Replay::Failed(r) if transport_record(r)) {
            return Err("AF308 retry lacks matching transport recording".into());
        }
        let attempts: Vec<_> = logs(c.campaign.as_ref().unwrap())?
            .into_iter()
            .filter(|l| l["slot"] == campaign_slot(c))
            .collect();
        if attempts.is_empty() {
            return Err("AF308 retry missing send log".into());
        }
        for log in &attempts {
            if !retryable_send(c, log)? {
                return Err("AF308 retry has HTTP/inflight attempt".into());
            }
        }
        c.replay = Replay::Missing;
    }
    Ok(())
}
pub(super) fn reserve_with_cap(ledger: &mut Value, c: &Case, cap: u64) -> Result<bool, String> {
    let retry = ledger[key(c)]["state"] == "transport_failed";
    if ledger.get(key(c)).is_some() && !retry {
        return Err("cost slot already occupied".into());
    }
    if total(ledger)?.saturating_add(upper(c)) > cap {
        return Ok(false);
    }
    ledger[key(c)] = json!({"state":"reserved","charged_nano_cny":upper(c),"upper_nano_cny":upper(c),"retry_transport":retry});
    Ok(true)
}
pub(super) fn refreeze_labels(
    root: &Path,
    frozen: &Value,
    groups: &[Vec<Case>],
    amendment: &str,
) -> Result<Value, String> {
    if amendment.is_empty()
        || !amendment
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err("invalid label amendment ID".into());
    }
    let path = root.join("manifest.json");
    let old = read(&path, Value::Null)?;
    if !matches!(
        old["version"].as_str(),
        Some("af302.v1" | "af319.v1" | "af352.v1")
    ) || old["version"] != frozen["version"]
    {
        return Err("label refreeze requires an existing matching campaign manifest".into());
    }
    let without_labels = |m: &Value| -> Result<Value, String> {
        let mut m = m.clone();
        for p in m["profiles"].as_array_mut().ok_or("invalid profiles")? {
            for slot in p["slots"].as_array_mut().ok_or("invalid slots")? {
                slot.as_object_mut()
                    .ok_or("invalid slot")?
                    .remove("labels")
                    .ok_or("missing label digest")?;
            }
        }
        Ok(m)
    };
    if without_labels(&old)? != without_labels(frozen)? {
        return Err(
            "AF313 only permits label changes; request/config/slot/rate/budget changed".into(),
        );
    }
    let changes: Vec<_> = items(&old["profiles"]).iter().zip(items(&frozen["profiles"]))
        .flat_map(|(a,b)| items(&a["slots"]).iter().zip(items(&b["slots"])).filter_map(|(x,y)| {
            if x["labels"] == y["labels"] { return None; }
            let c = groups.iter().flatten().find(|c| c.model.label == b["id"].as_str().unwrap()
                && campaign_slot(c) == y["slot"]).unwrap();
            Some(json!({"profile":b["id"],"slot":y["slot"],"before_labels":x["labels"],
                "after_labels":y["labels"],"before_label_revision":"not stored in original manifest",
                "after_label_revision":c.sample["label_revision"]}))
        })).collect();
    if changes
        .iter()
        .any(|c| c["after_label_revision"] != amendment)
    {
        return Err("changed labels must carry the requested amendment revision".into());
    }
    let audit = json!({"amendment":amendment,"sent":0,"changes":changes,
        "before_manifest":hippocampus::recall_models::fingerprint(&old),
        "after_manifest":hippocampus::recall_models::fingerprint(frozen)});
    if !changes.is_empty() {
        let backup = root.join(format!(
            "manifest.before-{}.json",
            amendment.to_ascii_lowercase()
        ));
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        if backup.exists() {
            if std::fs::read(&backup).map_err(|e| e.to_string())? != bytes {
                return Err("AF313 backup already exists with different content".into());
            }
        } else {
            std::fs::copy(&path, &backup).map_err(|e| e.to_string())?;
        }
        m15_write_json(
            &root.join(format!(
                "label-amendment-{}.json",
                amendment.to_ascii_lowercase()
            )),
            &audit,
        )?;
        m15_write_json(&path, frozen)?;
    }
    Ok(audit)
}
