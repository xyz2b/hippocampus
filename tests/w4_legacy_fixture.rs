//! W4 samples: every legacy record's `data` must be a complete, decodable
//! pre-phase-1 `Memory` (contracts.md 3.2, LegacyRecordView). Decoding and
//! re-encoding must give back the same JSON, so the fixture carries exactly
//! the frozen serialized shape.

use hippocampus::Memory;

#[test]
fn w4_legacy_records_decode_as_memory() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/eval/w4/samples");
    let mut checked = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let sample: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for record in sample["input"]["records"].as_array().unwrap() {
            if record["format"] != "legacy" {
                continue;
            }
            let data = &record["data"];
            let memory: Memory = serde_json::from_value(data.clone())
                .unwrap_or_else(|e| panic!("{}: legacy data does not decode: {e}", path.display()));
            assert_eq!(memory.id.0, record["id"].as_u64().unwrap(), "{}", path.display());
            assert_eq!(&serde_json::to_value(&memory).unwrap(), data, "{}", path.display());
            checked += 1;
        }
    }
    assert!(checked > 0, "no legacy record found in W4 samples");
}
