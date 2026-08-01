use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, PartialEq)]
pub struct Tps {
    pub last_5s: f64,
    pub last_10s: f64,
    pub last_1m: f64,
    pub last_5m: f64,
    pub last_15m: f64,
    pub catching_up: bool,
}

#[derive(Debug, PartialEq)]
pub struct PlayerList {
    pub online: u32,
    pub max: u32,
    pub names: Vec<String>,
}

fn strip_colors(raw: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new("§[0-9a-fk-or]").unwrap())
        .replace_all(raw, "")
        .into_owned()
}

pub fn parse_tps(raw: &str) -> Option<Tps> {
    let clean = strip_colors(raw);
    let mut after = clean.split("TPS from last 5s, 10s, 1m, 5m, 15m:").nth(1)?;

    // Trim leading whitespace
    after = after.trim_start();

    // Strip leading [⚡] marker if present
    if after.starts_with("[⚡]") {
        after = &after[4..]; // "[⚡]" is 4 bytes in UTF-8
        after = after.trim_start();
    }

    // Truncate at the next [⚡] marker to bound the search
    if let Some(pos) = after.find("[⚡]") {
        after = &after[..pos];
    }

    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(\*?)(\d+(?:\.\d+)?)").unwrap());
    let mut vals = Vec::with_capacity(5);
    let mut catching_up = false;
    for cap in re.captures_iter(after).take(5) {
        catching_up |= &cap[1] == "*";
        vals.push(cap[2].parse::<f64>().ok()?);
    }
    if vals.len() < 5 {
        return None;
    }
    Some(Tps {
        last_5s: vals[0],
        last_10s: vals[1],
        last_1m: vals[2],
        last_5m: vals[3],
        last_15m: vals[4],
        catching_up,
    })
}

pub fn parse_list(raw: &str) -> Option<PlayerList> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"There are (\d+) of a max of (\d+) players online:?\s*(.*)").unwrap()
    });
    let clean = strip_colors(raw);
    let cap = re.captures(&clean)?;
    let names = cap[3]
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Some(PlayerList {
        online: cap[1].parse().ok()?,
        max: cap[2].parse().ok()?,
        names,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reconstructed from spark's StatisticFormatter/HealthModule source.
    const SPARK_TPS_RCON: &str = "[⚡] TPS from last 5s, 10s, 1m, 5m, 15m: [⚡]  20.0, *20.0, 19.98, 19.87, 19.91[⚡] [⚡] Tick durations (min/med/95%ile/max ms) from last 10s, 1m: [⚡]  2.5/3.2/5.1/21.5;  2.4/3.4/6.8/45.2[⚡] [⚡] CPU usage from last 10s, 1m, 15m: [⚡]     12%, 15%, 14%  (system)[⚡]     8%, 9%, 9%  (process)";

    #[test]
    fn parses_spark_tps_line() {
        let tps = parse_tps(SPARK_TPS_RCON).unwrap();
        assert_eq!(tps.last_5s, 20.0);
        assert_eq!(tps.last_10s, 20.0);
        assert_eq!(tps.last_1m, 19.98);
        assert_eq!(tps.last_15m, 19.91);
        assert!(tps.catching_up); // the *20.0
    }

    #[test]
    fn tolerates_color_codes() {
        let painted = "[⚡] TPS from last 5s, 10s, 1m, 5m, 15m: §a20.0§r, §a19.99§r, 19.98, 19.87, 19.91";
        let tps = parse_tps(painted).unwrap();
        assert_eq!(tps.last_10s, 19.99);
        assert!(!tps.catching_up);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_tps("Unknown command").is_none());
    }

    #[test]
    fn rejects_truncated_tps_values() {
        let truncated = "[⚡] TPS from last 5s, 10s, 1m, 5m, 15m: [⚡]  20.0, 19.9[⚡] [⚡] Tick durations (min/med/95%ile/max ms) from last 10s, 1m: [⚡]  2.5/3.2/5.1/21.5;  2.4/3.4/6.8/45.2";
        assert!(parse_tps(truncated).is_none());
    }

    #[test]
    fn parses_player_list() {
        let raw = "There are 3 of a max of 8 players online: alice, bob, carl";
        let l = parse_list(raw).unwrap();
        assert_eq!((l.online, l.max), (3, 8));
        assert_eq!(l.names, vec!["alice", "bob", "carl"]);
    }

    #[test]
    fn parses_empty_list() {
        let l = parse_list("There are 0 of a max of 8 players online:").unwrap();
        assert_eq!(l.online, 0);
        assert!(l.names.is_empty());
    }
}
