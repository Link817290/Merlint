/// Format a token count as a human-readable string (e.g. "1.5M", "23.4K", "512").
pub fn format_tokens(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{}", n)
    }
}

/// Format a token count from u64 (convenience wrapper).
pub fn format_tokens_u64(n: u64) -> String {
    format_tokens(n as i64)
}

/// Format a USD cost with appropriate precision.
pub fn format_cost(usd: f64) -> String {
    if usd < 0.01 {
        format!("${:.4}", usd)
    } else if usd < 1.0 {
        format!("${:.3}", usd)
    } else {
        format!("${:.2}", usd)
    }
}

/// Format a large integer with K/M suffixes.
pub fn format_num(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{}", n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_tokens() {
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(1_500), "1.5K");
        assert_eq!(format_tokens(2_500_000), "2.5M");
        assert_eq!(format_tokens(0), "0");
    }

    #[test]
    fn test_format_cost() {
        assert_eq!(format_cost(0.001), "$0.0010");
        assert_eq!(format_cost(0.123), "$0.123");
        assert_eq!(format_cost(5.5), "$5.50");
    }

    #[test]
    fn test_format_num() {
        assert_eq!(format_num(42), "42");
        assert_eq!(format_num(1_200), "1.2K");
        assert_eq!(format_num(3_000_000), "3.0M");
    }
}
