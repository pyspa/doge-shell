//! Embedded output-schema assets and lookup.
//!
//! Mirrors `completion/json_loader.rs`: the repository-root `output-schemas/`
//! directory is the single canonical source, embedded via rust-embed and
//! cached in a `OnceLock`. `~/.config/dsh/output-schemas/` overrides embedded
//! definitions per command. NOTE: rust-embed tracks files, not the directory
//! — after *adding* a schema JSON, `touch` this file so release builds
//! re-embed (same caveat as `completions/`).

use dsh_types::output_schema::{OutputSchema, OutputSpec};
use rust_embed::RustEmbed;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use tracing::{debug, warn};

#[derive(RustEmbed)]
#[folder = "../output-schemas/"]
struct OutputSchemaAssets;

pub struct OutputSchemaDatabase {
    schemas: HashMap<String, OutputSchema>,
}

static SCHEMA_DATABASE_CACHE: OnceLock<Arc<OutputSchemaDatabase>> = OnceLock::new();

/// The process-wide database: embedded schemas, then user overrides. Loaded
/// once — no per-command dispatch, matching the completion loader's design.
pub fn database() -> Arc<OutputSchemaDatabase> {
    SCHEMA_DATABASE_CACHE
        .get_or_init(|| Arc::new(OutputSchemaDatabase::load()))
        .clone()
}

/// Find the spec for a command line (`argv[0]` is the command). Returns a
/// clone so callers hold nothing across the pipeline run.
pub fn lookup(argv: &[String]) -> Option<OutputSpec> {
    database().lookup(argv).cloned()
}

impl OutputSchemaDatabase {
    fn load() -> Self {
        let mut schemas = HashMap::new();

        for name in OutputSchemaAssets::iter() {
            if !name.ends_with(".json") {
                continue;
            }
            let Some(file) = OutputSchemaAssets::get(&name) else {
                continue;
            };
            match serde_json::from_slice::<OutputSchema>(&file.data) {
                Ok(schema) => {
                    schemas.insert(schema.command.clone(), schema);
                }
                Err(err) => warn!("invalid embedded output schema {name}: {err}"),
            }
        }

        // User overrides win over embedded definitions. Same directory set the
        // completion loader uses, so `~/.config/dsh/` works everywhere.
        // Applied back to front because later inserts win and the list is
        // most-specific first.
        for dir in crate::environment::user_asset_override_dirs("output-schemas")
            .iter()
            .rev()
        {
            Self::load_dir_into(dir, &mut schemas);
        }

        debug!("loaded {} output schemas", schemas.len());
        Self { schemas }
    }

    fn load_dir_into(dir: &Path, schemas: &mut HashMap<String, OutputSchema>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            match std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|data| {
                    serde_json::from_str::<OutputSchema>(&data).map_err(|e| e.to_string())
                }) {
                Ok(schema) => {
                    debug!("output schema override: {:?}", path);
                    schemas.insert(schema.command.clone(), schema);
                }
                Err(err) => warn!("invalid output schema {:?}: {err}", path),
            }
        }
    }

    #[cfg(test)]
    fn from_schemas(list: Vec<OutputSchema>) -> Self {
        Self {
            schemas: list
                .into_iter()
                .map(|schema| (schema.command.clone(), schema))
                .collect(),
        }
    }

    /// Match `argv` (command plus arguments) against the loaded schemas.
    pub fn lookup(&self, argv: &[String]) -> Option<&OutputSpec> {
        let command = argv.first()?;
        // `/usr/bin/ps` and `ps` are the same command.
        let command = Path::new(command).file_name()?.to_str()?;
        let schema = self.schemas.get(command)?;
        schema.outputs.iter().find(|spec| spec.matches(&argv[1..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| part.to_string()).collect()
    }

    /// The only guard against a schema file that silently fails to
    /// deserialize (mirrors `embedded_completion_definitions_are_valid`).
    #[test]
    fn embedded_output_schemas_are_valid() {
        let mut count = 0;
        for name in OutputSchemaAssets::iter() {
            if !name.ends_with(".json") {
                continue;
            }
            let file = OutputSchemaAssets::get(&name).expect("embedded asset should exist");
            let schema: OutputSchema = serde_json::from_slice(&file.data)
                .unwrap_or_else(|err| panic!("output schema {name} failed to parse: {err}"));
            assert!(
                schema
                    .outputs
                    .iter()
                    .all(|spec| { spec.prefer.is_some() || spec.text.is_some() }),
                "output schema {name}: every spec needs prefer or text"
            );
            for spec in &schema.outputs {
                if let Some(text) = &spec.text {
                    assert!(
                        !text.columns.is_empty(),
                        "output schema {name}: text spec needs columns"
                    );
                    for column in &text.columns[..text.columns.len() - 1] {
                        assert!(
                            !column.rest,
                            "output schema {name}: only the last column may be rest"
                        );
                    }
                }
            }
            count += 1;
        }
        assert!(count >= 7, "expected the MVP schema set, found {count}");
    }

    #[test]
    fn lookup_matches_command_basename_and_variant() {
        let database = OutputSchemaDatabase::from_schemas(vec![
            serde_json::from_str(
                r#"{"command":"ps","outputs":[
                    {"when":{"args_include":["aux"]},"text":{"header_lines":1,"columns":[{"name":"a"}]}},
                    {"when":{"args_include":["-ef"]},"text":{"header_lines":1,"columns":[{"name":"b"}]}}
                ]}"#,
            )
            .unwrap(),
        ]);

        let aux = database.lookup(&args(&["ps", "aux"])).unwrap();
        assert_eq!(aux.text.as_ref().unwrap().columns[0].name, "a");
        let ef = database.lookup(&args(&["/usr/bin/ps", "-ef"])).unwrap();
        assert_eq!(ef.text.as_ref().unwrap().columns[0].name, "b");
        assert!(database.lookup(&args(&["ps"])).is_none());
        assert!(database.lookup(&args(&["top"])).is_none());
        assert!(database.lookup(&[]).is_none());
    }

    #[test]
    fn embedded_ps_aux_schema_resolves() {
        let database = OutputSchemaDatabase::load();
        let spec = database
            .lookup(&args(&["ps", "aux"]))
            .expect("ps aux schema");
        let columns = &spec.text.as_ref().unwrap().columns;
        assert!(columns.iter().any(|column| column.name == "cpu"));
        // `docker ps --format x` must NOT match (user asked for their own format).
        assert!(
            database
                .lookup(&args(&["docker", "ps", "--format", "{{.Names}}"]))
                .is_none()
        );
        assert!(database.lookup(&args(&["docker", "ps"])).is_some());
    }
}
