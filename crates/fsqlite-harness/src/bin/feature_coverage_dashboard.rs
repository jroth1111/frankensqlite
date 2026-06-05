use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use fsqlite_harness::feature_coverage_dashboard::{
    DEFAULT_FEATURE_COVERAGE_MANIFEST_PATH, FEATURE_COVERAGE_DASHBOARD_BEAD_ID,
    FeatureCoverageRunMetadata, ReleaseGateOutcome, build_feature_coverage_dashboard,
    write_feature_coverage_dashboard,
};

#[derive(Debug, Clone)]
struct Config {
    workspace_root: PathBuf,
    manifest_path: PathBuf,
    output_json: PathBuf,
    run_metadata: FeatureCoverageRunMetadata,
}

impl Config {
    fn parse() -> Result<Self, String> {
        let mut workspace_root = default_workspace_root()?;
        let mut manifest_path = PathBuf::from(DEFAULT_FEATURE_COVERAGE_MANIFEST_PATH);
        let mut output_json: Option<PathBuf> = None;
        let mut run_metadata = FeatureCoverageRunMetadata::deterministic();

        let args = env::args().skip(1).collect::<Vec<_>>();
        let mut index = 0_usize;
        while index < args.len() {
            match args[index].as_str() {
                "--workspace-root" => {
                    index += 1;
                    let value = args
                        .get(index)
                        .ok_or_else(|| "missing value for --workspace-root".to_owned())?;
                    workspace_root = PathBuf::from(value);
                }
                "--manifest" => {
                    index += 1;
                    let value = args
                        .get(index)
                        .ok_or_else(|| "missing value for --manifest".to_owned())?;
                    manifest_path = PathBuf::from(value);
                }
                "--output-json" => {
                    index += 1;
                    let value = args
                        .get(index)
                        .ok_or_else(|| "missing value for --output-json".to_owned())?;
                    output_json = Some(PathBuf::from(value));
                }
                "--run-id" => {
                    index += 1;
                    let value = args
                        .get(index)
                        .ok_or_else(|| "missing value for --run-id".to_owned())?;
                    run_metadata.run_id = value.to_owned();
                }
                "--trace-id" => {
                    index += 1;
                    let value = args
                        .get(index)
                        .ok_or_else(|| "missing value for --trace-id".to_owned())?;
                    run_metadata.trace_id = value.to_owned();
                }
                "--scenario-id" => {
                    index += 1;
                    let value = args
                        .get(index)
                        .ok_or_else(|| "missing value for --scenario-id".to_owned())?;
                    run_metadata.scenario_id = value.to_owned();
                }
                "--seed" => {
                    index += 1;
                    let value = args
                        .get(index)
                        .ok_or_else(|| "missing value for --seed".to_owned())?;
                    run_metadata.seed = value
                        .parse::<u64>()
                        .map_err(|error| format!("invalid --seed value={value}: {error}"))?;
                }
                "--generated-unix-ms" => {
                    index += 1;
                    let value = args
                        .get(index)
                        .ok_or_else(|| "missing value for --generated-unix-ms".to_owned())?;
                    run_metadata.generated_unix_ms = value.parse::<u128>().map_err(|error| {
                        format!("invalid --generated-unix-ms value={value}: {error}")
                    })?;
                }
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => return Err(format!("unknown_argument: {other}")),
            }
            index += 1;
        }

        let output_json = output_json.unwrap_or_else(|| {
            workspace_root
                .join("artifacts")
                .join(FEATURE_COVERAGE_DASHBOARD_BEAD_ID)
                .join("feature_coverage_dashboard.json")
        });

        Ok(Self {
            workspace_root,
            manifest_path,
            output_json,
            run_metadata,
        })
    }
}

fn print_help() {
    println!(
        "\
feature_coverage_dashboard — canonical manifest coverage dashboard for bd-2yqp6.3.3

USAGE:
  cargo run -p fsqlite-harness --bin feature_coverage_dashboard -- [OPTIONS]

OPTIONS:
  --workspace-root <PATH>      Workspace root (default: auto-detected)
  --manifest <PATH>            Canonical corpus manifest (default: docs/contracts/corpus_manifest.toml)
  --output-json <PATH>         Machine-readable dashboard artifact
  --run-id <ID>                Run identifier embedded in the artifact
  --trace-id <ID>              Trace identifier embedded in the artifact
  --scenario-id <ID>           Scenario identifier embedded in the artifact
  --seed <U64>                 Deterministic run seed
  --generated-unix-ms <U128>   Generation timestamp; use 0 for reproducible reports
  -h, --help                   Show help
"
    );
}

fn default_workspace_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .map_err(|error| format!("workspace_root_canonicalize_failed: {error}"))
}

fn resolve_workspace_path(workspace_root: &Path, path: &Path) -> PathBuf {
    if path.is_relative() {
        workspace_root.join(path)
    } else {
        path.to_path_buf()
    }
}

fn run() -> Result<ExitCode, String> {
    let config = Config::parse()?;
    let manifest_path = resolve_workspace_path(&config.workspace_root, &config.manifest_path);
    let output_json = resolve_workspace_path(&config.workspace_root, &config.output_json);
    let dashboard = build_feature_coverage_dashboard(
        &config.workspace_root,
        &manifest_path,
        config.run_metadata,
    )?;
    write_feature_coverage_dashboard(&dashboard, &output_json)?;

    println!(
        "feature_coverage_dashboard result={:?} required_features={} missing={} partial={} artifact={}",
        dashboard.release_gate.outcome,
        dashboard.required_feature_count,
        dashboard.release_gate.missing_feature_count,
        dashboard.release_gate.partial_feature_count,
        output_json.display()
    );

    if matches!(dashboard.release_gate.outcome, ReleaseGateOutcome::Pass) {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
