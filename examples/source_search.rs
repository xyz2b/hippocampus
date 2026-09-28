//! Internal S3 source search. Queries never migrate or repair the database.
use std::{path::PathBuf, time::Instant};

use hippocampus::{PersistenceResult, SqliteMemoryStore, UserId, diagnostics};
use serde_json::{Value, json};

fn main() -> PersistenceResult<()> {
    let mut args = std::env::args().skip(1);
    let (mut db, mut owner, mut query, mut task, mut limit) = (None, None, None, None, None);
    let (mut rebuild, mut disabled) = (false, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!(
                    "source_search --db PATH --owner ID --query TEXT [--task ID] [--limit N] [--disabled]\nsource_search --db PATH --owner ID --rebuild\nQueries are read-only. --rebuild explicitly migrates and rebuilds this owner's index.\nInternal CLI only; no MCP response and no model calls."
                );
                return Ok(());
            }
            "--db" => db = Some(PathBuf::from(args.next().ok_or("--db requires a path")?)),
            "--owner" => owner = Some(args.next().ok_or("--owner requires an ID")?),
            "--query" => query = Some(args.next().ok_or("--query requires text")?),
            "--task" => task = Some(args.next().ok_or("--task requires an ID")?),
            "--limit" => {
                limit = Some(
                    args.next()
                        .ok_or("--limit requires a number")?
                        .parse::<usize>()?,
                )
            }
            "--rebuild" => rebuild = true,
            "--disabled" => disabled = true,
            _ => return Err(format!("unknown argument {arg}; use --help").into()),
        }
    }
    let db = db.ok_or("--db is required; no default database is used")?;
    let owner = UserId(owner.ok_or("--owner is required")?);
    if owner.0.trim().is_empty() || db.as_os_str().is_empty() {
        return Err("--db and --owner must not be empty".into());
    }
    if rebuild && (query.is_some() || task.is_some() || limit.is_some() || disabled) {
        return Err("--rebuild cannot be combined with query options".into());
    }
    if !rebuild && query.is_none() {
        return Err("provide --query TEXT or --rebuild".into());
    }
    if limit == Some(0) || task.as_ref().is_some_and(|value| value.trim().is_empty()) {
        return Err("--limit must be positive and --task must not be empty".into());
    }
    let limit = limit.unwrap_or(10);
    let tool = if rebuild {
        "source_index_rebuild"
    } else {
        "lexical_search"
    };
    let run_id = diagnostics::new_id();
    let _operation = diagnostics::Operation::enter(&run_id);
    let started = Instant::now();
    diagnostics::record_for(&run_id, "run_start", || {
        json!({"tool":tool,"owner":owner,"entrypoint":"internal_cli",
            "arguments":{"query":query,"task_id":task,"limit":limit,"enabled":!disabled}})
    });
    let result: PersistenceResult<Value> = (|| {
        if rebuild {
            let mut store = SqliteMemoryStore::open(&db, owner)?;
            store.rebuild_source_index()?;
            Ok(json!({"status":"completed","operation":"source_index_rebuild"}))
        } else {
            let mut store = SqliteMemoryStore::open_read_only(&db, owner)?;
            store.set_source_search_enabled(!disabled);
            store.search_sources(query.as_deref().unwrap(), task.as_deref(), limit)
        }
    })();
    diagnostics::record_for(&run_id, "run_end", || {
        json!({"tool":tool,"entrypoint":"internal_cli",
            "status":if result.is_ok() { "completed" } else { "failed" },
            "result":result.as_ref().ok(),"error":result.as_ref().err().map(ToString::to_string),
            "duration_ms":started.elapsed().as_millis(),
            "evidence_status":if diagnostics::incomplete() { "evidence_incomplete" } else { "complete" }})
    });
    println!("{}", serde_json::to_string_pretty(&result?)?);
    Ok(())
}
