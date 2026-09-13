//! Rendering the prompt line: `print_preprompt` (module output plus the GitHub notification badge) and the right-aligned status/duration/clock (`print_right_prompt`).
use super::*;

impl Prompt {
    pub fn print_preprompt<W: Write>(&mut self, out: &mut W) {
        write!(out, "{}", "\r".reset()).ok();

        // 1. Prepare Context
        // Check git status validity first (legacy logic adapted)

        let has_git = self.under_git();
        let mut status_to_display = self.get_git_status_cached();
        let real_branch = self.get_head_branch();

        // Branch verification logic (ported from original)
        if has_git && let Some(ref real) = real_branch {
            let mut cache_invalid = false;
            if let Some(ref status) = status_to_display {
                if status.branch != *real {
                    cache_invalid = true;
                }
            } else {
                cache_invalid = true;
            }

            if cache_invalid {
                if status_to_display.is_none() {
                    // Create a temporary status with the real branch name if we have nothing
                    let mut new_status = GitStatus::new();
                    new_status.branch = real.clone();
                    status_to_display = Some(new_status);
                } else if let Some(s) = status_to_display.as_mut() {
                    // Update branch name in the existing stale status to avoid confusion
                    s.branch = real.clone();
                }

                // Force invalidation of cache so async task picks it up
                // We keep the local `status_to_display` for this frame, but clear global cache
                // so next frames/background task know to fetch.
                self.git_status_cache = None;
                self.needs_git_check = true;
            }
        }

        let context = PromptContext {
            current_dir: &self.current_dir,
            project_root: self.project_root.as_deref(),
            git_root: self.current_git_root.as_deref(),
            git_status: status_to_display.as_ref(),
            has_rust_project: self.project_types.has_cargo_toml,
            has_node_project: self.project_types.has_package_json,
            has_python_project: self.project_types.has_python_project,
            has_go_project: self.project_types.has_go_mod,
            rust_version: self.rust_version_cache.as_deref(),
            rust_source: self.rust_runtime_source.as_deref(),
            node_version: self.node_version_cache.as_deref(),
            node_source: self.node_runtime_source.as_deref(),
            python_version: self.python_version_cache.as_deref(),
            python_source: self.python_runtime_source.as_deref(),
            go_version: self.go_version_cache.as_deref(),
            go_source: self.go_runtime_source.as_deref(),
            k8s_context: self.k8s_context_cache.as_deref(),
            k8s_namespace: self.k8s_namespace_cache.as_deref(),
            aws_profile: self.aws_profile_cache.as_deref(),
            docker_context: self.docker_context_cache.as_deref(),
            last_exit_status: self.last_exit_status,
            last_duration: self.last_duration,
        };

        // 2. Render Modules
        let mut prompt_content = String::new();

        for module in &self.modules {
            if let Some(content) = module.render(&context) {
                prompt_content.push_str(&content);
            }
        }

        // 3. GitHub Status (Internal Legacy - could be modularized later)
        // Display GitHub notifications if available and under git
        // 3. GitHub Status
        if has_git && let Some(status_lock) = &self.github_status {
            let status = status_lock.read();
            if status.total() > 0 {
                let mut notify_display = format!(" [ {} ", self.github_icon.as_str().white());

                if status.review_count > 0 {
                    notify_display.push_str(&format!(
                        "{} {} ",
                        "🔍".cyan().bold(),
                        status.review_count.to_string().cyan().bold()
                    ));
                }

                if status.mention_count > 0 {
                    notify_display.push_str(&format!(
                        "{} {} ",
                        "🔔".yellow(),
                        status.mention_count.to_string().yellow()
                    ));
                }

                if status.other_count > 0 {
                    notify_display.push_str(&format!(
                        "{} {} ",
                        "📬".dim(),
                        status.other_count.to_string().dim()
                    ));
                }

                // Remove trailing space before closing bracket if needed, but the loop adds one.
                // Let's just push bracket and handle trimming.
                let trimmed = notify_display.trim_end();
                let final_display = format!("{}]", trimmed);

                prompt_content.push_str(&final_display);
            } else if status.has_error {
                let notify_display = format!(
                    " [ {} {} ]",
                    self.github_icon.as_str().red(),
                    "ERROR".red().bold()
                );
                prompt_content.push_str(&notify_display);
            }
        }

        write!(out, "{}", prompt_content).ok();
    }

    pub fn print_right_prompt<W: Write>(
        &self,
        out: &mut W,
        cols: usize,
        last_status: i32,
        last_duration: Option<Duration>,
    ) {
        // Keep existing logic
        let time_str = chrono::Local::now().format("%H:%M:%S").to_string();

        let status_str = if last_status != 0 {
            format!("{} {} ", "✘".red().bold(), last_status.to_string().red())
        } else {
            String::new()
        };

        let duration_str = if let Some(d) = last_duration {
            if d.as_secs() >= 2 {
                let secs = d.as_secs();
                if secs < 60 {
                    format!("{}s ", secs)
                } else {
                    format!("{}m{}s ", secs / 60, secs % 60)
                }
                .yellow()
                .to_string()
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        let right_prompt = format!("{}{}{}", status_str, duration_str, time_str.as_str().dim());
        let right_width = crate::input::display_width(&right_prompt);

        if cols > right_width + 1 {
            let start_col = cols - right_width - 1;
            queue!(
                out,
                cursor::MoveToColumn(start_col as u16),
                crossterm::style::Print(right_prompt),
                cursor::MoveToColumn(0)
            )
            .ok();
        }
    }
}
