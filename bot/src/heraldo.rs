//! El heraldo: parses vanilla server-log advancement/challenge/goal lines
//! and has Don Quijote proclaim them in the notify channel.
//!
//! [`parse_deed_line`] is pure and fixture-tested. [`Herald`] wraps it with
//! a process-lifetime dedup set so the same log line, possibly re-fetched
//! across ticks because of tail overlap, is never announced twice.

use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeedKind {
    Advancement,
    Challenge,
    Goal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deed {
    pub player: String,
    pub kind: DeedKind,
    pub name: String,
}

/// Matches vanilla log lines of the three advancement-family forms, e.g.
/// `[12:34:56] [Server thread/INFO]: Juan has made the advancement [Stone Age]`.
///
/// Player names are constrained to the real Minecraft username charset
/// (`[A-Za-z0-9_]`, 1-16 chars) specifically so that chat lines impersonating
/// this format — `[12:34:56] [Server thread/INFO]: <Juan> has made the
/// advancement [fake]` — never match: the angle brackets around a chat
/// author aren't valid username characters.
fn deed_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^\[\d{2}:\d{2}:\d{2}\] \[Server thread/INFO\]: ([A-Za-z0-9_]{1,16}) has (made the advancement|completed the challenge|reached the goal) \[(.+)\]$",
        )
        .unwrap()
    })
}

pub fn parse_deed_line(line: &str) -> Option<Deed> {
    let cap = deed_regex().captures(line.trim())?;
    let kind = match &cap[2] {
        "made the advancement" => DeedKind::Advancement,
        "completed the challenge" => DeedKind::Challenge,
        "reached the goal" => DeedKind::Goal,
        _ => return None,
    };
    Some(Deed { player: cap[1].to_string(), kind, name: cap[3].to_string() })
}

/// FNV-1a, used purely as a cheap deterministic (no RNG, no per-process
/// seed) way to spread player+achievement pairs across a narration pool.
fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

fn pick<'a>(pool: &[&'a str], deed: &Deed) -> &'a str {
    let key = format!("{}\u{0}{}", deed.player, deed.name);
    let idx = fnv1a(&key) as usize % pool.len();
    pool[idx]
}

const ADVANCEMENT_POOL: &[&str] = &[
    ":scroll: ¡Asiento en el gran libro de hazañas! **{player}** ha alcanzado el logro de *{name}*, paso firme en su andanza por la ínsula.",
    ":scroll: Cuenta esta crónica que **{player}** ha logrado *{name}*. Pequeña hazaña, sí, mas toda gran gesta se compone de estas piedras.",
    ":scroll: **{player}** suma a su haber el logro de *{name}*. El cronista toma nota y sonríe complacido.",
    ":scroll: Nuevo capítulo en la vida de **{player}**: ha conquistado *{name}*. Adelante, buen hidalgo.",
];

const GOAL_POOL: &[&str] = &[
    ":dart: **{player}** ha coronado la meta de *{name}*, como quien al fin alcanza el molino tras larga cabalgata.",
    ":dart: Meta cumplida: **{player}** ha alcanzado *{name}* tras porfiada persecución. Bien haya tal tesón.",
    ":dart: **{player}** planta su bandera sobre *{name}*. Otra cumbre menos que conquistar en esta ínsula.",
    ":dart: Se ha cumplido el objetivo de *{name}* por mano de **{player}**. El horizonte ya no parece tan lejano.",
];

const CHALLENGE_POOL: &[&str] = &[
    ":crossed_swords: ¡Por mi fe que ha sido proeza homérica! **{player}** ha vencido el desafío de *{name}*, hazaña que el mismísimo Rocinante celebraría con relincho de gloria.",
    ":crossed_swords: ¡Cantad, musas, la gesta de **{player}**! Ha sometido el desafío de *{name}* como quien desfacía entuertos por doquier. Ni los gigantes de otrora osarían interponerse.",
    ":crossed_swords: Escrito quede en letras de oro: **{player}** ha derrotado el reto de *{name}*. Ni el yelmo de Mambrino brilla tanto como esta hazaña.",
    ":crossed_swords: ¡Alabado sea el brazo de **{player}**! El desafío de *{name}* yace vencido a sus pies, proeza digna de figurar junto a las de Amadís de Gaula.",
    ":crossed_swords: Que se toquen las trompetas: **{player}** ha superado el desafío de *{name}*. Ni la Ínsula Barataria vio jamás gobernador tan bizarro.",
];

/// Weave a short chivalric proclamation for `deed`. Challenges get the
/// most epic register, goals a solid one, plain advancements a modest but
/// warm nod. Selection within each pool is deterministic (hashed on
/// player+achievement) so back-to-back deeds don't read as copy-paste.
pub fn narrate(deed: &Deed) -> String {
    let pool = match deed.kind {
        DeedKind::Advancement => ADVANCEMENT_POOL,
        DeedKind::Goal => GOAL_POOL,
        DeedKind::Challenge => CHALLENGE_POOL,
    };
    pick(pool, deed).replace("{player}", &deed.player).replace("{name}", &deed.name)
}

/// Process-lifetime dedup wrapper: feed it raw (possibly multi-line, tail-
/// overlapping) server log text each tick and get back narrations only for
/// deed lines not seen before in this process. Missing history from before
/// a bot restart is acceptable; re-announcing a line already narrated is
/// not, so the dedup set is keyed on the exact raw line.
#[derive(Default)]
pub struct Herald {
    seen: HashSet<String>,
}

impl Herald {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn process(&mut self, log_text: &str) -> Vec<String> {
        let mut out = Vec::new();
        for line in log_text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(deed) = parse_deed_line(line) {
                if self.seen.insert(line.to_string()) {
                    out.push(narrate(&deed));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_advancement_line() {
        let d = parse_deed_line("[12:34:56] [Server thread/INFO]: Juan has made the advancement [Stone Age]").unwrap();
        assert_eq!(d, Deed { player: "Juan".into(), kind: DeedKind::Advancement, name: "Stone Age".into() });
    }

    #[test]
    fn parses_challenge_line() {
        let d = parse_deed_line("[12:34:56] [Server thread/INFO]: Juan has completed the challenge [Cover Me in Debris]").unwrap();
        assert_eq!(d, Deed { player: "Juan".into(), kind: DeedKind::Challenge, name: "Cover Me in Debris".into() });
    }

    #[test]
    fn parses_goal_line() {
        let d = parse_deed_line("[12:34:56] [Server thread/INFO]: Juan has reached the goal [Sky's the Limit]").unwrap();
        assert_eq!(d, Deed { player: "Juan".into(), kind: DeedKind::Goal, name: "Sky's the Limit".into() });
    }

    #[test]
    fn rejects_chat_line_impersonating_an_advancement() {
        // Real player chat is rendered with angle brackets around the
        // author; a player literally typing "Juan has made the
        // advancement [fake]" in chat must never be mistaken for the real
        // server-generated line.
        assert!(parse_deed_line(
            "[12:34:56] [Server thread/INFO]: <Juan> has made the advancement [fake]"
        )
        .is_none());
    }

    #[test]
    fn rejects_unrelated_log_lines() {
        assert!(parse_deed_line("[12:34:56] [Server thread/INFO]: Juan joined the game").is_none());
        assert!(parse_deed_line("Done (12.345s)! For help, type \"help\"").is_none());
    }

    #[test]
    fn narration_pools_are_nonempty_and_unique() {
        for pool in [ADVANCEMENT_POOL, GOAL_POOL, CHALLENGE_POOL] {
            assert!(pool.len() >= 4, "pool too small: {pool:?}");
            assert!(pool.iter().all(|s| !s.is_empty()));
            let unique: HashSet<_> = pool.iter().collect();
            assert_eq!(unique.len(), pool.len(), "duplicate entries: {pool:?}");
        }
        assert!(CHALLENGE_POOL.len() >= 4);
    }

    #[test]
    fn narrate_interpolates_player_and_name_and_never_panics() {
        let deeds = [
            Deed { player: "Juan".into(), kind: DeedKind::Advancement, name: "Stone Age".into() },
            Deed { player: "Ana".into(), kind: DeedKind::Goal, name: "Sky's the Limit".into() },
            Deed { player: "Bob".into(), kind: DeedKind::Challenge, name: "Cover Me in Debris".into() },
            Deed { player: "".into(), kind: DeedKind::Challenge, name: "".into() },
        ];
        for d in deeds {
            let msg = narrate(&d);
            assert!(!msg.contains("{player}") && !msg.contains("{name}"));
            if !d.player.is_empty() {
                assert!(msg.contains(&d.player));
            }
        }
    }

    #[test]
    fn narrate_selection_varies_across_inputs() {
        // Sanity check the hash-based picker doesn't degenerate to always
        // choosing the same template.
        let names = ["Stone Age", "Cover Me in Debris", "Sky's the Limit", "Diamonds!", "We Need to Go Deeper"];
        let msgs: HashSet<String> = names
            .iter()
            .map(|n| narrate(&Deed { player: "Juan".into(), kind: DeedKind::Advancement, name: (*n).into() }))
            .collect();
        assert!(msgs.len() > 1, "expected variety, got a single repeated template");
    }

    #[test]
    fn herald_never_reannounces_the_same_line_twice() {
        let mut h = Herald::new();
        let log = "[12:34:56] [Server thread/INFO]: Juan has made the advancement [Stone Age]\n[12:35:00] [Server thread/INFO]: Juan joined the game\n[12:36:00] [Server thread/INFO]: Ana has reached the goal [Sky's the Limit]\n";
        let first = h.process(log);
        assert_eq!(first.len(), 2);

        // Next tick re-fetches an overlapping tail (same two deed lines
        // plus one genuinely new one).
        let overlapping = "[12:34:56] [Server thread/INFO]: Juan has made the advancement [Stone Age]\n[12:36:00] [Server thread/INFO]: Ana has reached the goal [Sky's the Limit]\n[12:37:00] [Server thread/INFO]: Juan has completed the challenge [Cover Me in Debris]\n";
        let second = h.process(overlapping);
        assert_eq!(second.len(), 1);
        assert!(second[0].contains("Juan"));
    }
}
