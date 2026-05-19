use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use async_trait::async_trait;
use dsh_types::{Context, ExitStatus};

struct CockpitStep {
    title: &'static str,
    command: &'static str,
    args: &'static [&'static str],
}

const COCKPIT_STEPS: &[CockpitStep] = &[
    CockpitStep {
        title: "Project Status",
        command: "pm",
        args: &["status"],
    },
    CockpitStep {
        title: "Detected Tasks",
        command: "task",
        args: &["--list"],
    },
    CockpitStep {
        title: "Validation Hints",
        command: "doctor",
        args: &["validate"],
    },
    CockpitStep {
        title: "Activation Dry Run",
        command: "pm",
        args: &["activate", "--dry-run"],
    },
    CockpitStep {
        title: "Recent Failed Blocks",
        command: "blocks",
        args: &["list", "--limit", "5", "--failed"],
    },
];

pub struct ProjectCockpitAction;

impl ProjectCockpitAction {
    fn steps() -> &'static [CockpitStep] {
        COCKPIT_STEPS
    }

    fn step_argv(step: &CockpitStep) -> Vec<String> {
        let mut argv = vec![step.command.to_string()];
        argv.extend(step.args.iter().map(|arg| (*arg).to_string()));
        argv
    }
}

#[async_trait(?Send)]
impl Action for ProjectCockpitAction {
    fn name(&self) -> &str {
        "Project Cockpit"
    }

    fn description(&self) -> &str {
        "Show project status, tasks, validation, activation, and failed blocks"
    }

    fn icon(&self) -> &str {
        ">"
    }

    fn usage(&self) -> Option<&str> {
        Some("pm status; task --list; doctor validate; pm activate --dry-run; blocks list --failed")
    }

    fn related(&self) -> Option<&str> {
        Some("pm status, task --list, doctor validate, blocks list --failed")
    }

    fn category(&self) -> &str {
        "Project"
    }

    async fn execute(&self, shell: &mut Shell, _input: &str) -> Result<()> {
        let ctx = Context::new_safe(shell.pid, shell.pgid, true);

        for step in Self::steps() {
            let _ = ctx.write_stdout(&format!("== {} ==", step.title));
            let Some(command_fn) = dsh_builtin::get_command(step.command) else {
                let _ = ctx.write_stderr(&format!(
                    "project cockpit: builtin command not found: {}",
                    step.command
                ));
                continue;
            };

            let argv = Self::step_argv(step);
            match command_fn(&ctx, argv, shell) {
                ExitStatus::ExitedWith(0)
                | ExitStatus::Running(_)
                | ExitStatus::Break
                | ExitStatus::Continue
                | ExitStatus::Return => {}
                ExitStatus::ExitedWith(code) => {
                    let _ = ctx.write_stderr(&format!(
                        "project cockpit: `{}` exited with {}",
                        step.command, code
                    ));
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_palette::Action;

    #[test]
    fn project_cockpit_properties_describe_noninteractive_fallback() {
        let action = ProjectCockpitAction;

        assert_eq!(action.name(), "Project Cockpit");
        assert_eq!(action.category(), "Project");
        assert!(
            action
                .usage()
                .expect("usage")
                .contains("pm activate --dry-run")
        );
        assert!(
            action
                .related()
                .expect("related")
                .contains("blocks list --failed")
        );
    }

    #[test]
    fn project_cockpit_steps_cover_expected_project_surface() {
        let commands: Vec<Vec<String>> = ProjectCockpitAction::steps()
            .iter()
            .map(ProjectCockpitAction::step_argv)
            .collect();

        assert_eq!(
            commands,
            vec![
                vec!["pm".to_string(), "status".to_string()],
                vec!["task".to_string(), "--list".to_string()],
                vec!["doctor".to_string(), "validate".to_string()],
                vec![
                    "pm".to_string(),
                    "activate".to_string(),
                    "--dry-run".to_string()
                ],
                vec![
                    "blocks".to_string(),
                    "list".to_string(),
                    "--limit".to_string(),
                    "5".to_string(),
                    "--failed".to_string()
                ],
            ]
        );
    }
}
