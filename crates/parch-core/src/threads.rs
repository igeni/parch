//! Thread budget: by default the engine uses three quarters of the available cores.

/// Number of cores available to this process (respects affinity and cgroup quotas where std does).
pub fn available_cores() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// Default worker count: `max(1, floor(cores * 3 / 4))`.
pub fn default_threads() -> usize {
    threads_for(available_cores())
}

pub fn threads_for(cores: usize) -> usize {
    (cores * 3 / 4).max(1)
}

/// Parses `N` (absolute) or `P%` (share of available cores).
pub fn parse_threads(s: &str) -> Result<usize, String> {
    let s = s.trim();
    if let Some(p) = s.strip_suffix('%') {
        let p: f64 = p.trim().parse().map_err(|_| format!("invalid percentage: {s}"))?;
        if !(p > 0.0 && p <= 100.0) {
            return Err(format!("percentage must be in (0, 100]: {s}"));
        }
        Ok(((available_cores() as f64 * p / 100.0).floor() as usize).max(1))
    } else {
        match s.parse::<usize>() {
            Ok(0) | Err(_) => Err(format!("invalid thread count: {s}")),
            Ok(n) => Ok(n),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_quarters_with_floor_and_minimum() {
        assert_eq!(threads_for(1), 1);
        assert_eq!(threads_for(2), 1);
        assert_eq!(threads_for(4), 3);
        assert_eq!(threads_for(8), 6);
        assert_eq!(threads_for(10), 7);
        assert_eq!(threads_for(16), 12);
    }

    #[test]
    fn parses_counts_and_percentages() {
        assert_eq!(parse_threads("5"), Ok(5));
        assert!(parse_threads("0").is_err());
        assert!(parse_threads("x").is_err());
        assert!(parse_threads("0%").is_err());
        assert!(parse_threads("101%").is_err());
        assert_eq!(parse_threads("100%"), Ok(available_cores()));
        assert_eq!(parse_threads("75%"), Ok(default_threads()));
    }
}
