//! E2 development-only reader/judge jobs. Preparing jobs never sends a model request.
use std::{
    collections::HashSet,
    env, fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use hippocampus::{DEEPSEEK_MODEL, ModelBudgetConfig, ModelStage, OpenAiClient};
use rusqlite::OptionalExtension;
use serde::Deserialize;
use serde_json::{Value, json};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Job {
    case_id: String,
    owner_id: String,
    evidence_group: String,
    strategy: String,
    stage: String,
    prompt: String,
    prompt_sha256: String,
}

impl Job {
    fn model_stage(&self) -> Result<ModelStage> {
        Ok(match self.stage.as_str() {
            "reader" => ModelStage::Reader,
            "judge" => ModelStage::Judge,
            _ => return Err(format!("unknown stage: {}", self.stage).into()),
        })
    }

    fn operation_id(&self) -> String {
        format!(
            "s5-e2:{}:{}:{}:{}",
            self.case_id, self.evidence_group, self.strategy, self.stage
        )
    }

    fn check(&self) -> Result<()> {
        for (name, value) in [
            ("case_id", &self.case_id),
            ("evidence_group", &self.evidence_group),
        ] {
            if value.is_empty()
                || value.len() > 100
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return Err(format!("{name} must be 1..=100 ASCII letters, digits, - or _").into());
            }
        }
        if self.owner_id.trim().is_empty()
            || self.owner_id.len() > 512
            || !matches!(self.strategy.as_str(), "direct" | "con")
            || self.prompt.trim().is_empty()
        {
            return Err("invalid owner, strategy or empty prompt".into());
        }
        self.model_stage()?;
        if self.prompt_sha256 != sha256(self.prompt.as_bytes())? {
            return Err(format!("prompt SHA-256 mismatch for {}", self.operation_id()).into());
        }
        Ok(())
    }
}

fn sha256(bytes: &[u8]) -> Result<String> {
    // Reuse the repository's shasum convention; no hashing dependency for this one-off example.
    let mut child = Command::new("shasum")
        .args(["-a", "256"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("shasum stdin missing")?
        .write_all(bytes)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err("shasum failed".into());
    }
    Ok(String::from_utf8(output.stdout)?
        .split_whitespace()
        .next()
        .ok_or("shasum output missing")?
        .to_owned())
}

fn read_budget(path: &Path) -> Result<ModelBudgetConfig> {
    let config: ModelBudgetConfig = serde_json::from_slice(&fs::read(path)?)?;
    config.validate()?;
    Ok(config)
}

fn required_path(value: Option<String>, name: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(
        value.ok_or_else(|| format!("{name} is required"))?,
    ))
}

fn main() -> Result<()> {
    let all_args: Vec<_> = env::args().skip(1).collect();
    if matches!(all_args.first().map(String::as_str), Some("--help" | "-h")) {
        println!(
            "longmemeval_e2_send --jobs JOBS.jsonl --output NEW.jsonl --budget-db ISOLATED.sqlite3 --reader-budget READER.json --judge-budget JUDGE.json [--send [--live]]\nDefault prepares and verifies jobs without credentials or model calls. --send permits offline loopback fixtures; --send --live additionally requires explicit PMR_MODEL_NETWORK=live for an authorized provider run. Output is create_new; no overwrite or automatic retry."
        );
        return Ok(());
    }
    let mut jobs_path = None;
    let mut output_path = None;
    let mut budget_db = None;
    let mut reader_budget = None;
    let mut judge_budget = None;
    let mut send = false;
    let mut live = false;
    let mut args = all_args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--jobs" => jobs_path = args.next(),
            "--output" => output_path = args.next(),
            "--budget-db" => budget_db = args.next(),
            "--reader-budget" => reader_budget = args.next(),
            "--judge-budget" => judge_budget = args.next(),
            "--send" => send = true,
            "--live" => live = true,
            _ => return Err(format!("unknown or incomplete argument: {arg}").into()),
        }
    }
    let declared_network_mode = env::var("PMR_MODEL_NETWORK").ok();
    let network_mode = declared_network_mode.as_deref().unwrap_or("offline");
    if !matches!(network_mode, "deny" | "offline" | "live") {
        return Err("PMR_MODEL_NETWORK must be deny, offline, or live".into());
    }
    if live && (!send || declared_network_mode.as_deref() != Some("live")) {
        return Err("--live requires --send and explicit PMR_MODEL_NETWORK=live".into());
    }
    if send && network_mode == "live" && !live {
        return Err("live model sends also require --live".into());
    }
    if live
        && env::var("DEEPSEEK_BASE_URL")?.trim_end_matches('/')
            != "https://ark.cn-beijing.volces.com/api/v3"
    {
        return Err("E2 live endpoint differs from the frozen proposal".into());
    }
    let jobs_path = required_path(jobs_path, "--jobs")?;
    let output_path = required_path(output_path, "--output")?;
    let budget_db = required_path(budget_db, "--budget-db")?;
    let reader_budget = read_budget(&required_path(reader_budget, "--reader-budget")?)?;
    let judge_budget = read_budget(&required_path(judge_budget, "--judge-budget")?)?;
    if reader_budget.group_id == judge_budget.group_id {
        return Err("reader and judge need distinct budget groups".into());
    }
    if [&jobs_path, &output_path].contains(&&budget_db) || jobs_path == output_path {
        return Err("jobs, output and budget database paths must differ".into());
    }
    if budget_db.exists() {
        let db = rusqlite::Connection::open_with_flags(
            &budget_db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        if db
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='memory_profiles'",
                [],
                |row| row.get::<_, i32>(0),
            )
            .optional()?
            .is_some()
        {
            return Err("E2 budget DB must be separate from a memory database".into());
        }
    }
    let jobs: Vec<Job> = fs::read_to_string(&jobs_path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    if jobs.is_empty() {
        return Err("jobs file is empty".into());
    }
    let mut seen = HashSet::new();
    for job in &jobs {
        job.check()?;
        if !seen.insert(job.operation_id()) {
            return Err(format!("duplicate job: {}", job.operation_id()).into());
        }
    }
    let credentials = if send {
        Some((
            env::var("DEEPSEEK_BASE_URL").unwrap_or_default(),
            env::var("DEEPSEEK_API_KEY").unwrap_or_default(),
        ))
    } else {
        None
    };
    let mut output_options = OpenOptions::new();
    output_options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        output_options.mode(0o600);
    }
    let mut output = output_options.open(output_path)?;
    let mut failures = 0;
    for job in jobs {
        let stage = job.model_stage()?;
        let budget = if matches!(stage, ModelStage::Reader) {
            &reader_budget
        } else {
            &judge_budget
        };
        let (status, output_text, error, model_calls, physical_sends): (
            &str,
            Option<String>,
            Option<String>,
            Vec<Value>,
            u64,
        ) = if let Some((base_url, api_key)) = &credentials {
            match OpenAiClient::new(base_url, api_key).and_then(|client| {
                client.with_budget(
                    &budget_db,
                    &job.owner_id,
                    budget.clone(),
                    job.operation_id(),
                )
            }) {
                Ok(client) => {
                    let result = client.evaluation_text(stage, &job.prompt);
                    let (status, output_text, error) = match result {
                        Ok(text) => ("completed", Some(text), None),
                        Err(error) => ("failed", None, Some(error.to_string())),
                    };
                    (
                        status,
                        output_text,
                        error,
                        client.model_budget_report(),
                        client.model_call_count(),
                    )
                }
                Err(error) => ("failed", None, Some(error.to_string()), Vec::new(), 0),
            }
        } else {
            ("prepared", None, None, Vec::new(), 0)
        };
        let record = json!({
            "case_id":job.case_id,"owner_id":job.owner_id,"evidence_group":job.evidence_group,
            "strategy":job.strategy,"stage":job.stage,"prompt_sha256":job.prompt_sha256,
            "status":status,"network_mode":network_mode,"output_text":output_text,"error":error,
            "model":DEEPSEEK_MODEL,"max_output_tokens":stage.output_tokens(budget),
            "physical_sends":physical_sends,"model_calls":model_calls
        });
        writeln!(output, "{record}")?;
        output.flush()?;
        failures += usize::from(status == "failed");
    }
    if failures != 0 {
        return Err(format!("{failures} E2 jobs failed; each result was written, no retry").into());
    }
    Ok(())
}
