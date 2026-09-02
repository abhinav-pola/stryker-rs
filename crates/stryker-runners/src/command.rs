//! The generic command runner: re-run a user-supplied test command per
//! mutant. Exit 0 = Survived, nonzero = Killed, our kill = Timeout.
//!
//! Execution modes, chosen per command:
//! - **Direct argv** (default): split with shell-words, leading `VAR=value`
//!   tokens become child env, the rest is exec'd with no shell — paths with
//!   `(`/`)` (Next.js route groups) can't break quoting.
//! - **bash -c**: commands with real shell syntax (`&&`, `|`, `$(...)`, …)
//!   run through `bash` (never `/bin/sh`, whose quoting broke route-group
//!   paths in the wild). Auto-detected, or forced via
//!   `commandRunner.shell: true`.

use async_trait::async_trait;
use camino::Utf8PathBuf;

use crate::process::run_with_timeout;
use crate::{
    Capabilities, DryRunOptions, DryRunResult, MutantRunOptions, MutantRunOutcome, TestResult,
    TestRunner,
};

pub const ACTIVE_MUTANT_ENV: &str = "__STRYKER_ACTIVE_MUTANT__";
pub const HIT_LIMIT_ENV: &str = "__STRYKER_HIT_LIMIT__";

const SHELL_ONLY_TOKENS: &[&str] = &["&&", "||", "|", ";", ">", ">>", "<", "2>", "&"];

enum Invocation {
    /// (env prefix assignments, argv)
    Direct(Vec<(String, String)>, Vec<String>),
    /// Full command string for `bash -c`.
    Shell(String),
}

pub struct CommandRunner {
    invocation: Invocation,
    cwd: Utf8PathBuf,
}

/// Does the raw command require a real shell?
fn needs_shell(command: &str, argv: &[String]) -> bool {
    argv.iter().any(|t| SHELL_ONLY_TOKENS.contains(&t.as_str()))
        || command.contains("$(")
        || command.contains('`')
}

fn is_env_assignment(token: &str) -> bool {
    match token.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.chars().next().unwrap().is_ascii_digit()
        }
        None => false,
    }
}

impl CommandRunner {
    pub fn new(command: &str, cwd: Utf8PathBuf) -> anyhow::Result<Self> {
        Self::with_shell(command, cwd, false)
    }

    pub fn with_shell(command: &str, cwd: Utf8PathBuf, force_shell: bool) -> anyhow::Result<Self> {
        let argv = shell_words::split(command)
            .map_err(|e| anyhow::anyhow!("cannot parse commandRunner.command {command:?}: {e}"))?;
        if argv.is_empty() {
            anyhow::bail!("commandRunner.command is empty");
        }
        let invocation = if force_shell || needs_shell(command, &argv) {
            tracing::debug!("command runner using bash -c for {command:?}");
            Invocation::Shell(command.to_string())
        } else {
            // Leading VAR=value tokens become child env (sh-style prefixes,
            // common in generated test commands).
            let env_end = argv.iter().position(|t| !is_env_assignment(t)).unwrap_or(argv.len());
            if env_end == argv.len() {
                anyhow::bail!("commandRunner.command {command:?} has no command word");
            }
            let env = argv[..env_end]
                .iter()
                .map(|t| {
                    let (name, value) = t.split_once('=').expect("checked by is_env_assignment");
                    (name.to_string(), value.to_string())
                })
                .collect();
            Invocation::Direct(env, argv[env_end..].to_vec())
        };
        Ok(Self { invocation, cwd })
    }

    fn command(&self) -> tokio::process::Command {
        let mut cmd = match &self.invocation {
            Invocation::Direct(env, argv) => {
                let mut cmd = tokio::process::Command::new(&argv[0]);
                cmd.args(&argv[1..]);
                cmd.envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
                cmd
            }
            Invocation::Shell(command) => {
                let mut cmd = tokio::process::Command::new("bash");
                cmd.arg("-c").arg(command);
                cmd
            }
        };
        cmd.current_dir(&self.cwd);
        cmd
    }
}

#[async_trait]
impl TestRunner for CommandRunner {
    fn capabilities(&self) -> Capabilities {
        Capabilities { per_test_coverage: false }
    }

    async fn dry_run(&mut self, options: &DryRunOptions) -> anyhow::Result<DryRunResult> {
        let output = run_with_timeout(self.command(), options.timeout).await?;
        if output.timed_out {
            return Ok(DryRunResult::Timeout);
        }
        let gross_ms = output.elapsed.as_millis() as u64;
        if output.success() {
            Ok(DryRunResult::Complete {
                tests: vec![TestResult {
                    id: "all".into(),
                    name: "All tests".into(),
                    file: None,
                    time_ms: gross_ms as f64,
                    failed: false,
                    failure: None,
                }],
                coverage: None,
                gross_ms,
            })
        } else {
            Ok(DryRunResult::Error(format!(
                "test command failed in the dry run (exit {:?}):\n{}",
                output.exit_code,
                output.diagnostic_tail()
            )))
        }
    }

    async fn mutant_run(&mut self, options: &MutantRunOptions) -> anyhow::Result<MutantRunOutcome> {
        let mut cmd = self.command();
        cmd.env(ACTIVE_MUTANT_ENV, options.active_mutant.to_string());
        if let Some(limit) = options.hit_limit {
            cmd.env(HIT_LIMIT_ENV, limit.to_string());
        }
        let output = run_with_timeout(cmd, options.timeout).await?;
        if output.timed_out {
            return Ok(MutantRunOutcome::Timeout { reason: None });
        }
        if output.success() {
            Ok(MutantRunOutcome::Survived { tests_ran: 1 })
        } else if output.stderr.contains("Stryker: Hit limit reached")
            || output.stdout.contains("Stryker: Hit limit reached")
        {
            Ok(MutantRunOutcome::Timeout { reason: Some("Hit limit reached".into()) })
        } else {
            Ok(MutantRunOutcome::Killed {
                killed_by: vec!["all".into()],
                tests_ran: 1,
                failure: Some(output.diagnostic_tail()),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direct(runner: &CommandRunner) -> (&[(String, String)], &[String]) {
        match &runner.invocation {
            Invocation::Direct(env, argv) => (env, argv),
            Invocation::Shell(_) => panic!("expected direct invocation"),
        }
    }

    #[test]
    fn shell_syntax_switches_to_bash() {
        let runner = CommandRunner::new("bun test && echo done", Utf8PathBuf::from(".")).unwrap();
        assert!(matches!(runner.invocation, Invocation::Shell(_)));
        let runner = CommandRunner::new("echo `date`", Utf8PathBuf::from(".")).unwrap();
        assert!(matches!(runner.invocation, Invocation::Shell(_)));
    }

    #[test]
    fn parses_quoted_args_with_parens() {
        let runner =
            CommandRunner::new("bun test 'app/(user)/page.test.tsx'", Utf8PathBuf::from("."))
                .unwrap();
        let (_, argv) = direct(&runner);
        assert_eq!(argv[2], "app/(user)/page.test.tsx");
    }

    #[test]
    fn env_prefix_assignments() {
        let runner = CommandRunner::new(
            "COVERAGE_ENABLED=false TEST_SEQUENTIAL=1 bash ../../scripts/run-tests.sh --preload ./setup.ts",
            Utf8PathBuf::from("."),
        )
        .unwrap();
        let (env, argv) = direct(&runner);
        assert_eq!(env.len(), 2);
        assert_eq!(env[0], ("COVERAGE_ENABLED".to_string(), "false".to_string()));
        assert_eq!(argv[0], "bash");
        assert_eq!(argv[2], "--preload");
    }

    #[test]
    fn env_only_command_is_an_error() {
        assert!(CommandRunner::new("FOO=1 BAR=2", Utf8PathBuf::from(".")).is_err());
    }
}
