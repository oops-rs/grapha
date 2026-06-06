use std::time::{Duration, Instant};

use indicatif::{ProgressBar, ProgressStyle};

/// A progress bar for known-length operations.
pub fn bar(len: u64, msg: &str) -> ProgressBar {
    let pb = ProgressBar::new(len);
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg} [{bar:25.cyan/dim}] {pos}/{len}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
            .progress_chars("━╸─"),
    );
    pb.set_message(msg.to_string());
    pb
}

/// Print a completed step with elapsed time.
pub fn done(msg: &str, start: Instant) {
    done_elapsed(msg, start.elapsed());
}

/// Print a completed step with a precomputed elapsed duration.
pub fn done_elapsed(msg: &str, elapsed: Duration) {
    eprintln!("  \x1b[32m✓\x1b[0m {} ({})", msg, format_elapsed(elapsed));
}

/// Print a summary line.
pub fn summary(msg: &str) {
    eprintln!("\x1b[1m{}\x1b[0m", msg);
}

fn format_elapsed(elapsed: Duration) -> String {
    if elapsed.as_secs() >= 1 {
        format!("{:.1}s", elapsed.as_secs_f64())
    } else if elapsed.as_millis() >= 1 {
        format!("{:.1}ms", elapsed.as_secs_f64() * 1_000.0)
    } else {
        format!("{}µs", elapsed.as_micros())
    }
}

#[cfg(test)]
mod tests {
    use super::format_elapsed;
    use std::time::Duration;

    #[test]
    fn formats_microseconds() {
        assert_eq!(format_elapsed(Duration::from_micros(823)), "823µs");
    }

    #[test]
    fn formats_fractional_milliseconds() {
        assert_eq!(format_elapsed(Duration::from_micros(1_250)), "1.2ms");
    }

    #[test]
    fn formats_seconds() {
        assert_eq!(format_elapsed(Duration::from_millis(1_250)), "1.2s");
    }
}
