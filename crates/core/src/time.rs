/// `1:02:03.456` or `02:03.456`.
pub fn fmt_precise(secs: f64) -> String {
    let secs = secs.max(0.0);
    let total_ms = (secs * 1000.0).round() as u64;
    let (h, m, s, ms) = split(total_ms);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{ms:03}")
    } else {
        format!("{m:02}:{s:02}.{ms:03}")
    }
}

/// `1:02:03` or `2:03`.
pub fn fmt_short(secs: f64) -> String {
    let total_ms = (secs.max(0.0) * 1000.0).round() as u64;
    let (h, m, s, _) = split(total_ms);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn split(total_ms: u64) -> (u64, u64, u64, u64) {
    let ms = total_ms % 1000;
    let s = total_ms / 1000;
    (s / 3600, (s / 60) % 60, s % 60, ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(fmt_precise(0.0), "00:00.000");
        assert_eq!(fmt_precise(3723.4567), "1:02:03.457");
        assert_eq!(fmt_short(125.0), "2:05");
        assert_eq!(fmt_short(3600.0), "1:00:00");
    }
}
