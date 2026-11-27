use super::ShellProxy;
use crossterm::style::Stylize;
use dsh_types::{Context, ExitStatus};
use std::io::Write;
use sysinfo::System;

pub fn description() -> &'static str {
    "Show integrated dashboard (System, Git, GitHub)"
}

pub fn command(ctx: &Context, _argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let mut sys = match System::new_all() {
        Ok(system) => system,
        Err(err) => {
            let _ = ctx.write_stderr(&format!("dashboard: {err}"));
            return ExitStatus::ExitedWith(1);
        }
    };
    // Refresh twice for CPU usage accuracy on some systems, but once is usually okay for a quick look.
    sys.refresh_all();

    let cwd = proxy.get_current_dir().unwrap_or_default();
    let branch = proxy.get_git_branch().unwrap_or_else(|| "none".to_string());
    let (reviews, mentions, others) = proxy.get_github_status();
    let job_count = proxy.get_job_count();

    let cpu_usage = sys.global_cpu_usage();
    let mem_total = sys.total_memory() as f64 / 1024.0 / 1024.0 / 1024.0;
    let mem_used = sys.used_memory() as f64 / 1024.0 / 1024.0 / 1024.0;
    let mem_percent = (mem_used / mem_total) * 100.0;

    let width = 60;
    let border = "═".repeat(width);

    let mut out = Vec::new();

    writeln!(out, "╔{}╗", border).ok();
    writeln!(
        out,
        "║ {:^width$} ║",
        "🐕 doge-shell dashboard".bold().yellow(),
        width = width
    )
    .ok();
    writeln!(out, "╠{}╣", border).ok();

    // Project Info
    let project_info = super::project::find_project_by_path(&cwd).ok().flatten();
    let title = if let Some(ref p) = project_info {
        format!("🚀 Project: {}", p.name)
    } else {
        "🚀 Project Context".to_string()
    };

    writeln!(out, "║ {:<width$} ║", title.cyan().bold(), width = width).ok();
    writeln!(
        out,
        "║   Dir:    {:<width$} ║",
        cwd.display().to_string().dim(),
        width = width - 10
    )
    .ok();
    writeln!(
        out,
        "║   Branch: {:<width$} ║",
        format!("🐾 {}", branch).green(),
        width = width - 10
    )
    .ok();
    writeln!(
        out,
        "║   Jobs:   {:<width$} ║",
        job_count,
        width = width - 10
    )
    .ok();

    writeln!(out, "╠{}╣", border).ok();

    // GitHub Status
    writeln!(
        out,
        "║ {:<width$} ║",
        "🐙 GitHub Notifications".magenta().bold(),
        width = width
    )
    .ok();
    let gh_summary = format!(
        "🔍 {} Reviews  🔔 {} Mentions  📬 {} Others",
        reviews.to_string().cyan(),
        mentions.to_string().yellow(),
        others.to_string().white().dim()
    );
    writeln!(out, "║   {:<width$} ║", gh_summary, width = width + 28).ok(); // Adjusted for ANSI codes

    writeln!(out, "╠{}╣", border).ok();

    // System Resources
    writeln!(
        out,
        "║ {:<width$} ║",
        "💻 System Resources".blue().bold(),
        width = width
    )
    .ok();

    let cpu_bar = make_bar(cpu_usage as usize, 20);
    writeln!(
        out,
        "║   CPU:    [{}] {:>5.1}% {:<width$} ║",
        cpu_bar.red(),
        cpu_usage,
        "",
        width = width - 36
    )
    .ok();

    let mem_bar = make_bar(mem_percent as usize, 20);
    writeln!(
        out,
        "║   Memory: [{}] {:>5.1}% ({:.1}/{:.1} GB) {:<width$} ║",
        mem_bar.green(),
        mem_percent,
        mem_used,
        mem_total,
        "",
        width = width - 48
    )
    .ok();

    writeln!(out, "╚{}╝", border).ok();

    let _ = ctx.write_stdout(String::from_utf8_lossy(&out).as_ref());

    ExitStatus::ExitedWith(0)
}

fn make_bar(percent: usize, width: usize) -> String {
    let filled = (percent * width) / 100;
    let mut bar = String::new();
    for i in 0..width {
        if i < filled {
            bar.push('|');
        } else {
            bar.push(' ');
        }
    }
    bar
}
