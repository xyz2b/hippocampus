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
        &[("w1", "medium"), ("w3", "minimal"), ("w4", "medium")]
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
    let efforts = object
        .entry("reasoning_effort")
        .or_insert_with(|| json!({}));
    let efforts = efforts
        .as_object_mut()
        .ok_or("reasoning_effort must be an object")?;
    for (role, effort) in defaults {
        efforts.entry(*role).or_insert_with(|| json!(effort));
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
            [Medium, Minimal, Medium]
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
        assert_eq!(write.effort("w3"), Minimal);
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
}
