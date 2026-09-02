mod run;

use camino::{Utf8Path, Utf8PathBuf};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "stryker", version, about = "Mutation testing for JavaScript and TypeScript, in Rust")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run mutation testing.
    Run {
        /// Path to a stryker config file (JSON/JSONC/.mjs/.js). May also be
        /// given as a positional argument (stryker-js compatibility:
        /// `stryker run stryker.config.json`).
        #[arg(long)]
        config: Option<Utf8PathBuf>,
        /// Positional config path (same as --config).
        #[arg(value_name = "CONFIG")]
        config_positional: Option<Utf8PathBuf>,
        /// Mutate files even when they have uncommitted changes.
        #[arg(long)]
        force_dirty: bool,
        /// Stop after a successful dry run.
        #[arg(long)]
        dry_run_only: bool,
    },
    /// Restore in-place-mutated files from the backup manifest after a crash.
    Restore {
        /// Temp directory containing the backup manifest.
        #[arg(long, default_value = ".stryker-tmp")]
        temp_dir: Utf8PathBuf,
    },
    /// Inspection helpers.
    #[command(subcommand)]
    Debug(DebugCommand),
}

#[derive(Subcommand)]
enum DebugCommand {
    /// List the files that would be mutated.
    Files {
        #[arg(long)]
        config: Option<Utf8PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Run { config, config_positional, force_dirty, dry_run_only } => {
            let runtime = tokio::runtime::Runtime::new()?;
            let code = runtime.block_on(run::run(run::RunFlags {
                config: config.or(config_positional),
                force_dirty,
                dry_run_only,
            }))?;
            std::process::exit(code);
        }
        Command::Restore { temp_dir } => restore(&temp_dir),
        Command::Debug(DebugCommand::Files { config }) => debug_files(config),
    }
}

fn restore(temp_dir: &Utf8Path) -> anyhow::Result<()> {
    let cwd = Utf8PathBuf::from_path_buf(std::env::current_dir()?)
        .map_err(|p| anyhow::anyhow!("non-UTF8 cwd: {}", p.display()))?;
    let temp_dir = if temp_dir.is_absolute() { temp_dir.to_owned() } else { cwd.join(temp_dir) };
    let restored = stryker_core::sandbox::restore_from_manifest(&cwd, &temp_dir)?;
    println!("restored {restored} file(s) from {temp_dir}");

    // Second-pass safety net: any file still carrying the header?
    let config = stryker_core::config::discover_config(&cwd)
        .map(|p| stryker_core::config::load_config(&p))
        .transpose()?
        .unwrap_or_default();
    if let Ok(project) = stryker_core::project::read_project(&cwd, &config) {
        let leftovers = stryker_core::sandbox::find_instrumented_leftovers(
            &cwd,
            project.targets.iter().map(|t| t.path.clone()),
            stryker_instrumenter::HEADER_MARKER,
        );
        for path in &leftovers {
            eprintln!("WARNING: {path} still contains instrumented code (no backup found)");
        }
        if !leftovers.is_empty() {
            eprintln!("recover these from git: git checkout -- <file>");
        }
    }
    Ok(())
}

fn load_config_or_default(
    config_path: Option<Utf8PathBuf>,
) -> anyhow::Result<(Utf8PathBuf, stryker_core::config::StrykerConfig)> {
    let cwd = Utf8PathBuf::from_path_buf(std::env::current_dir()?)
        .map_err(|p| anyhow::anyhow!("non-UTF8 cwd: {}", p.display()))?;
    let config = match &config_path {
        Some(path) => stryker_core::config::load_config(path)?,
        None => match stryker_core::config::discover_config(&cwd) {
            Some(path) => stryker_core::config::load_config(&path)?,
            None => stryker_core::config::StrykerConfig::default(),
        },
    };
    Ok((cwd, config))
}

fn debug_files(config_path: Option<Utf8PathBuf>) -> anyhow::Result<()> {
    let (cwd, config) = load_config_or_default(config_path)?;
    let project = stryker_core::project::read_project(Utf8Path::new(&cwd), &config)?;
    eprintln!(
        "{} project files, {} mutate targets",
        project.files.len(),
        project.targets.len()
    );
    for target in &project.targets {
        let ranges = target
            .ranges
            .iter()
            .map(|r| format!("{}-{}", r.start_line, r.end_line))
            .collect::<Vec<_>>()
            .join(",");
        if ranges.is_empty() {
            println!("{}", target.path);
        } else {
            println!("{}:{ranges}", target.path);
        }
    }
    Ok(())
}
