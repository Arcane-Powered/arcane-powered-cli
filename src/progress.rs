use std::io::IsTerminal;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

/// Returns true when interactive progress bars should be rendered.
pub fn is_interactive(json_mode: bool) -> bool {
    if json_mode {
        return false;
    }
    if std::env::var("CI")
        .map(|v| !v.is_empty() && v != "0" && v != "false")
        .unwrap_or(false)
    {
        return false;
    }
    std::io::stderr().is_terminal()
}

pub fn make_bytes_bar(total: u64, prefix: &str, interactive: bool) -> ProgressBar {
    if !interactive {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::with_draw_target(Some(total), ProgressDrawTarget::stderr());
    let style = ProgressStyle::with_template(
        "  {prefix:.cyan.bold} [{bar:32.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, ETA {eta})",
    )
    .unwrap_or_else(|_| ProgressStyle::default_bar())
    .progress_chars("=>-");
    pb.set_style(style);
    pb.set_prefix(prefix.to_string());
    pb
}

pub fn make_count_bar(total: u64, prefix: &str, interactive: bool) -> ProgressBar {
    if !interactive {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::with_draw_target(Some(total), ProgressDrawTarget::stderr());
    let style = ProgressStyle::with_template(
        "  {prefix:.cyan.bold} [{bar:32.cyan/blue}] {pos}/{len} ({eta})",
    )
    .unwrap_or_else(|_| ProgressStyle::default_bar())
    .progress_chars("=>-");
    pb.set_style(style);
    pb.set_prefix(prefix.to_string());
    pb
}

pub fn make_spinner(msg: &str, interactive: bool) -> ProgressBar {
    if !interactive {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr());
    let style = ProgressStyle::with_template("  {spinner:.cyan} {msg}")
        .unwrap_or_else(|_| ProgressStyle::default_spinner());
    pb.set_style(style);
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(std::time::Duration::from_millis(120));
    pb
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    const UNITS: &[&str] = &["KB", "MB", "GB", "TB"];
    let mut v = bytes as f64 / 1000.0;
    let mut u = 0usize;
    while v >= 1000.0 && u + 1 < UNITS.len() {
        v /= 1000.0;
        u += 1;
    }
    format!("{v:.1} {}", UNITS[u])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes_cleanly() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1500), "1.5 KB");
        assert_eq!(format_bytes(2_500_000), "2.5 MB");
        assert_eq!(format_bytes(150_000_000_000), "150.0 GB");
    }
}
