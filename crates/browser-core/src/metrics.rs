use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct MetricsSnapshot {
    pub startup: Duration,
    pub rss_bytes: u64,
    pub webkit_rss_bytes: u64,
    pub cpu_seconds: f64,
}

pub fn snapshot(started: Instant) -> MetricsSnapshot {
    MetricsSnapshot {
        startup: started.elapsed(),
        rss_bytes: process_rss_bytes(std::process::id()),
        webkit_rss_bytes: webkit_rss_bytes(),
        cpu_seconds: sample_cpu_seconds(),
    }
}

pub fn sample_cpu_seconds() -> f64 {
    let out = Command::new("ps")
        .args(["-o", "time=", "-p", &std::process::id().to_string()])
        .output();
    let Ok(out) = out else {
        return 0.0;
    };
    parse_ps_time(&String::from_utf8_lossy(&out.stdout))
}

fn parse_ps_time(value: &str) -> f64 {
    let value = value.trim();
    let mut parts: Vec<&str> = value.split(':').collect();
    if parts.len() == 2 {
        parts.insert(0, "0");
    }
    if parts.len() != 3 {
        return 0.0;
    }
    let hours: f64 = parts[0].parse().unwrap_or(0.0);
    let minutes: f64 = parts[1].parse().unwrap_or(0.0);
    let seconds: f64 = parts[2].parse().unwrap_or(0.0);
    hours * 3600.0 + minutes * 60.0 + seconds
}

fn process_rss_bytes(pid: u32) -> u64 {
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output();
    let Ok(out) = out else {
        return 0;
    };
    let kb: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0);
    kb * 1024
}

fn webkit_rss_bytes() -> u64 {
    let out = Command::new("ps").args(["-ax", "-o", "rss=,comm="]).output();
    let Ok(out) = out else {
        return 0;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("webkit") || lower.contains("networking")
        })
        .filter_map(|line| line.split_whitespace().next()?.parse::<u64>().ok())
        .sum::<u64>()
        * 1024
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ps_cpu_time() {
        assert_eq!(parse_ps_time("0:01.50"), 1.5);
        assert_eq!(parse_ps_time("1:02:03"), 3723.0);
    }
}
