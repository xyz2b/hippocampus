//! Runtime role defaults. Resolve the Pro endpoint only when an online provider is needed.
use super::*;
use crate::openai::recall_models::RecallModels;
use crate::write_models::WriteModels;

const PRO_MODEL_ENV: &str = "DEEPSEEK_PRO_MODEL";
const PRO_MODEL_DEFAULT: &str = "env:DEEPSEEK_PRO_MODEL";

pub(super) fn role_defaults(mut value: Value, write: bool) -> PersistenceResult<Value> {
    let object = value
        .as_object_mut()
        .ok_or("role models must be an object")?;
    let defaults: &[(&str, &str)] = if write {
        &[("w1", "low"), ("w3", "low"), ("w4", "medium")]
    } else {
        &[("r1", "minimal"), ("r4", "minimal")]
    };
    for (role, _) in defaults {
        if let Some(model) = object.get(*role) {
            if model.as_str().is_none_or(|s| s.trim().is_empty()) {
                return Err(format!("{role} model must be nonempty").into());
            }
        } else {
            // Resolve this configuration reference before constructing any model request.
            object.insert((*role).into(), json!(PRO_MODEL_DEFAULT));
        }
    }
    let provider_defaults = object
        .get("providers")
        .and_then(Value::as_object)
        .map(|providers| {
            providers
                .iter()
                .map(|(role, value)| {
                    let effort = if value["protocol"] == "openai_chat" {
                        "omit"
                    } else {
                        "medium"
                    };
                    (role.clone(), effort)
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    if !write && let Some(effort) = provider_defaults.get("r4") {
        object
            .entry("r4_compute_reasoning_effort")
            .or_insert_with(|| json!(effort));
    }
    let efforts = object
        .entry("reasoning_effort")
        .or_insert_with(|| json!({}));
    let efforts = efforts
        .as_object_mut()
        .ok_or("reasoning_effort must be an object")?;
    for (role, effort) in defaults {
        efforts
            .entry(*role)
            .or_insert_with(|| json!(provider_defaults.get(*role).unwrap_or(effort)));
    }
    Ok(value)
}

fn fill_models(models: &mut [&mut String], pro: Option<&str>) -> PersistenceResult<()> {
    for model in models {
        if model.as_str() == PRO_MODEL_DEFAULT {
            **model = pro.filter(|s| !s.trim().is_empty()).ok_or_else(|| {
                format!("default Pro model is unavailable: set {PRO_MODEL_ENV} or explicitly configure every required role model; no model request was sent")
            })?.trim().into();
        }
    }
    Ok(())
}

pub(super) fn validate_write_models(models: &WriteModels) -> PersistenceResult<()> {
    models.validate()
}

pub(super) fn validate_recall_models(models: &RecallModels) -> PersistenceResult<()> {
    models.validate()
}

impl ChatConfig {
    fn write_models_for(&self, pro: Option<&str>) -> PersistenceResult<WriteModels> {
        let mut models = match &self.write_models {
            Some(models) => models.clone(),
            None => serde_json::from_value(role_defaults(json!({}), true)?)?,
        };
        fill_models(&mut [&mut models.w1, &mut models.w3, &mut models.w4], pro)?;
        models.validate()?;
        Ok(models)
    }

    fn recall_models_for(&self, pro: Option<&str>) -> PersistenceResult<RecallModels> {
        let mut models = match &self.recall_models {
            Some(models) => models.clone(),
            None => serde_json::from_value(role_defaults(json!({}), false)?)?,
        };
        fill_models(&mut [&mut models.r1, &mut models.r4], pro)?;
        models.validate()?;
        Ok(models)
    }

    pub(super) fn resolved_write_models(&self) -> PersistenceResult<WriteModels> {
        self.write_models_for(std::env::var(PRO_MODEL_ENV).ok().as_deref())
    }

    pub(super) fn resolved_recall_models(&self) -> PersistenceResult<RecallModels> {
        self.recall_models_for(std::env::var(PRO_MODEL_ENV).ok().as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReasoningEffort::{Low, Medium, Minimal};

    #[test]
    fn af283_snapshot_is_opt_in_bounded_and_default_binding_is_unchanged() {
        let implicit: ChatConfig = serde_json::from_value(json!({})).unwrap();
        let explicit: ChatConfig =
            serde_json::from_value(json!({"statement_snapshot":{}})).unwrap();
        assert_eq!(
            serde_json::to_value(&implicit).unwrap(),
            serde_json::to_value(&explicit).unwrap()
        );
        assert!(!implicit.statement_snapshot.enabled);
        assert!(
            serde_json::to_value(implicit)
                .unwrap()
                .get("statement_snapshot")
                .is_none()
        );
        let configured: ChatConfig = serde_json::from_value(json!({"statement_snapshot":{
            "enabled":true, "max_records":4, "max_bytes":8192
        }}))
        .unwrap();
        configured.validate().unwrap();
        let reopened: ChatConfig =
            serde_json::from_value(serde_json::to_value(&configured).unwrap()).unwrap();
        assert_eq!(reopened.statement_snapshot, configured.statement_snapshot);
        for invalid in [
            json!({"max_records":0}),
            json!({"max_records":33}),
            json!({"max_bytes":511}),
            json!({"max_bytes":65537}),
            json!({"enabled":"true"}),
            json!({"relations":["fact"]}),
        ] {
            assert!(
                serde_json::from_value::<ChatConfig>(json!({"statement_snapshot":invalid}))
                    .is_err()
            );
        }
    }

    #[test]
    fn embedding_config_is_opt_in_strict_and_default_serialization_stays_compatible() {
        let implicit: ChatConfig = serde_json::from_value(json!({})).unwrap();
        let explicit: ChatConfig = serde_json::from_value(json!({"embedding":{}})).unwrap();
        assert!(!implicit.embedding.enabled);
        assert_eq!(
            serde_json::to_value(&implicit).unwrap(),
            serde_json::to_value(&explicit).unwrap()
        );
        assert!(
            serde_json::to_value(&implicit)
                .unwrap()
                .get("embedding")
                .is_none()
        );
        let configured:ChatConfig=serde_json::from_value(json!({"enrichment_mode":"off","embedding":{"enabled":true,"model":"bge-m3","base_url":"http://localhost:11434","batch":8,"query_timeout_ms":90000}})).unwrap();
        configured.validate().unwrap();
        assert!(configured.model_budget.is_none());
        let reopened: ChatConfig =
            serde_json::from_value(serde_json::to_value(&configured).unwrap()).unwrap();
        assert_eq!(reopened.embedding, configured.embedding);
        for invalid in [
            json!({"embedding":{"base_url":"https://cloud.example"}}),
            json!({"embedding":{"batch":17}}),
            json!({"embedding":{"query_timeout_ms":0}}),
            json!({"embedding":{"fallback":true}}),
            json!({"embedding":{"model":"qwen-cloud"}}),
        ] {
            assert!(serde_json::from_value::<ChatConfig>(invalid).is_err());
        }
    }
    #[test]
    fn omitted_roles_use_environment_pro_and_ac2_efforts_only_when_needed() {
        let config: ChatConfig = serde_json::from_value(json!({})).unwrap();
        assert_eq!(config.write_pipeline, WritePipeline::W4RuntimeV1);
        assert_eq!(config.recall_pipeline, RecallPipeline::RecallV3);
        config.validate().unwrap();
        assert!(
            config
                .write_models_for(None)
                .unwrap_err()
                .to_string()
                .contains(PRO_MODEL_ENV)
        );
        assert!(config.recall_models_for(None).is_err());
        let write = config.write_models_for(Some("pro-fixture")).unwrap();
        assert_eq!(
            (&write.w1, &write.w3, &write.w4),
            (
                &"pro-fixture".into(),
                &"pro-fixture".into(),
                &"pro-fixture".into()
            )
        );
        assert_eq!(
            [write.effort("w1"), write.effort("w3"), write.effort("w4")],
            [Low, Low, Medium]
        );
        let recall = config.recall_models_for(Some("pro-fixture")).unwrap();
        assert_eq!(
            (&recall.r1, &recall.r4),
            (&"pro-fixture".into(), &"pro-fixture".into())
        );
        assert_eq!(
            [
                recall.effort("r1"),
                recall.effort("r4"),
                recall.r4_compute_reasoning_effort
            ],
            [Minimal, Minimal, Low]
        );
    }

    #[test]
    fn af105_w3_defaults_and_explicit_efforts_survive_configuration_reopen() {
        for value in [
            json!({}),
            json!({"write_models":{}}),
            json!({"write_models":{"w1":"custom"}}),
        ] {
            let config: ChatConfig = serde_json::from_value(value).unwrap();
            let reopened: ChatConfig =
                serde_json::from_value(serde_json::to_value(&config).unwrap()).unwrap();
            for config in [&config, &reopened] {
                config.validate().unwrap();
                let models = config.write_models_for(Some("pro-fixture")).unwrap();
                assert_eq!(models.effort("w3"), Low);
                assert_eq!(models.effort("w1"), Low);
                assert_eq!(models.effort("w4"), Medium);
                assert_eq!(models.w3, "pro-fixture");
            }
        }
        for effort in [Minimal, Medium, crate::ReasoningEffort::Omit] {
            let config: ChatConfig = serde_json::from_value(json!({"write_models":{
                "w3":"custom-reviewer", "reasoning_effort":{"w3":effort}
            }}))
            .unwrap();
            let reopened: ChatConfig =
                serde_json::from_value(serde_json::to_value(&config).unwrap()).unwrap();
            for config in [&config, &reopened] {
                let models = config.write_models_for(Some("pro-fixture")).unwrap();
                assert_eq!(models.effort("w3"), effort);
                assert_eq!(models.w3, "custom-reviewer");
                assert_eq!(models.effort("w1"), Low);
                assert_eq!(models.effort("w4"), Medium);
            }
        }
    }

    #[test]
    fn partial_role_defaults_and_explicit_overrides_are_distinct() {
        let config: ChatConfig = serde_json::from_value(json!({
            "write_models":{"w1":"custom","reasoning_effort":{"w1":"low"}},
            "recall_models":{"r1":"custom","r4":"custom"}
        }))
        .unwrap();
        config.validate().unwrap();
        let config: ChatConfig =
            serde_json::from_value(serde_json::to_value(&config).unwrap()).unwrap();
        assert!(config.write_models_for(None).is_err());
        let write = config.write_models_for(Some("pro-fixture")).unwrap();
        assert_eq!(write.w1, "custom");
        assert_eq!(write.w3, "pro-fixture");
        assert_eq!(write.effort("w1"), Low);
        assert_eq!(write.effort("w3"), Low);
        assert_eq!(
            config.recall_models_for(None).unwrap().effort("r4"),
            Minimal
        );
        for invalid in [
            json!({"write_models":{"w1":""}}),
            json!({"recall_models":{"r1":null}}),
        ] {
            assert!(serde_json::from_value::<ChatConfig>(invalid).is_err());
        }
    }
    #[test]
    fn af119_new_providers_get_explicit_role_defaults_without_changing_production_defaults() {
        let config: ChatConfig=serde_json::from_value(json!({"write_models":{
            "w1":"gpt-6.1-sol","w3":"claude-opus-5-5","w4":"qwen3.8:27b-q4_K_M",
            "providers":{"w1":{"protocol":"openai_responses","base_url_env":"OPENAI_BASE_URL","api_key_env":"OPENAI_API_KEY"},
                "w3":{"protocol":"anthropic_messages","base_url_env":"ANTHROPIC_BASE_URL","api_key_env":"ANTHROPIC_API_KEY"},
                "w4":{"protocol":"openai_chat","base_url_env":"QWEN_BASE_URL","thinking":false,"context_tokens":65536}}},
            "recall_models":{"r1":"gpt-6.1-sol","r4":"claude-opus-5-5","providers":{
                "r1":{"protocol":"openai_responses","base_url_env":"OPENAI_BASE_URL","api_key_env":"OPENAI_API_KEY"},
                "r4":{"protocol":"anthropic_messages","base_url_env":"ANTHROPIC_BASE_URL","api_key_env":"ANTHROPIC_API_KEY"}}}})).unwrap();
        let write = config.write_models_for(None).unwrap();
        assert_eq!(write.effort("w1"), Medium);
        assert_eq!(write.effort("w3"), Medium);
        assert_eq!(write.effort("w4"), crate::ReasoningEffort::Omit);
        let recall = config.recall_models_for(None).unwrap();
        assert_eq!(recall.effort("r1"), Medium);
        assert_eq!(recall.r4_compute_reasoning_effort, Medium);
        let reopened: ChatConfig =
            serde_json::from_value(serde_json::to_value(&config).unwrap()).unwrap();
        assert_eq!(
            reopened.write_models_for(None).unwrap().providers,
            write.providers
        );
        for invalid in [
            json!({"write_models":{"w1":"gpt-6.1-sol","reasoning_effort":{"w1":"minimal"},
            "providers":{"w1":{"protocol":"openai_responses","base_url_env":"OPENAI_BASE_URL"}}}}),
            json!({"recall_models":{"r4":"claude-opus-5-5","providers":{"r4":{"protocol":"anthropic_messages","base_url_env":"ANTHROPIC_BASE_URL","thinking":false}}}}),
            json!({"write_models":{"w1":"gpt-6.1-sol","providers":{"w1":{"protocol":"openai_responses","base_url_env":"OPENAI_BASE_URL","api_key":"secret"}}}}),
        ] {
            assert!(serde_json::from_value::<ChatConfig>(invalid).is_err());
        }
        let old: ChatConfig = serde_json::from_value(json!({})).unwrap();
        assert!(
            serde_json::to_value(old.write_models_for(Some("pro")).unwrap())
                .unwrap()
                .get("providers")
                .is_none()
        );
    }
}
