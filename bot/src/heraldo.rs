//! El heraldo: parses vanilla server-log advancement/challenge/goal lines
//! (and, since the wheel started killing people, death lines too) and has
//! Don Quijote proclaim them in the notify channel.
//!
//! [`parse_deed_line`] and [`parse_death_line`] are pure and fixture-tested.
//! [`Herald`] wraps both with a single process-lifetime dedup set so the
//! same log line, possibly re-fetched across ticks because of tail overlap,
//! is never announced twice.

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

/// Death categories, following vanilla's own grouping closely enough that
/// each one maps to a family of related death messages rather than a single
/// exact string. `Other` is the catch-all for any matched death phrase that
/// doesn't fit a more specific bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeathCategory {
    SlainBy,
    Fall,
    FireOrLava,
    Drowning,
    Explosion,
    Lightning,
    Void,
    Starvation,
    Suffocation,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Death {
    pub player: String,
    pub category: DeathCategory,
    pub killer: Option<String>,
    /// The raw phrase matched after the player name (e.g. "fell from a high
    /// place"), kept only to spread the roast pick deterministically —
    /// never surfaced to players directly.
    phrase: String,
}

/// Shared charset for a killer name: covers both player usernames and
/// vanilla mob display names ("Zombie", "Cave Spider", "Wither Skeleton").
const KILLER_CHARS: &str = r"[A-Za-z0-9_' -]{1,32}";

/// One vanilla death-message shape: a regex matching the *whole* log line
/// (player name plus phrase) and the category it belongs to. Built once,
/// checked in order, first match wins.
fn death_patterns() -> &'static [(Regex, DeathCategory)] {
    static PATTERNS: OnceLock<Vec<(Regex, DeathCategory)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let line = |phrase: &str| {
            Regex::new(&format!(
                r"^\[\d{{2}}:\d{{2}}:\d{{2}}\] \[Server thread/INFO\]: (?P<player>[A-Za-z0-9_]{{1,16}}) {phrase}\.?$"
            ))
            .unwrap()
        };
        let killer = format!(r"(?P<killer>{KILLER_CHARS})");
        vec![
            // --- Slain by (melee/ranged/named kills) ------------------------
            (line(&format!("was slain by {killer}(?: using .+)?")), DeathCategory::SlainBy),
            (line(&format!("was shot by {killer}")), DeathCategory::SlainBy),
            (line(&format!("was fireballed by {killer}")), DeathCategory::SlainBy),
            (line(&format!("was pummeled by {killer}")), DeathCategory::SlainBy),
            (line(&format!("was killed by {killer}(?: using .+)?")), DeathCategory::SlainBy),
            (line(&format!("was stung to death by {killer}")), DeathCategory::SlainBy),
            // --- Fall ---------------------------------------------------------
            (line("fell from a high place"), DeathCategory::Fall),
            (line("hit the ground too hard"), DeathCategory::Fall),
            (line("fell off (?:a ladder|some scaffolding|scaffolding)"), DeathCategory::Fall),
            (line("fell while climbing"), DeathCategory::Fall),
            (
                line(&format!("fell too far and was finished by {killer}(?: using .+)?")),
                DeathCategory::Fall,
            ),
            (line(&format!("was doomed to fall by {killer}(?: using .+)?")), DeathCategory::Fall),
            // --- Fire / lava ----------------------------------------------
            (line("went up in flames"), DeathCategory::FireOrLava),
            (line("burned to death"), DeathCategory::FireOrLava),
            (
                line(&format!("tried to swim in lava(?: to escape {killer})?")),
                DeathCategory::FireOrLava,
            ),
            (
                line(&format!("walked into a fire whilst fighting {killer}")),
                DeathCategory::FireOrLava,
            ),
            (
                line(&format!("was burned to a crisp whilst fighting {killer}")),
                DeathCategory::FireOrLava,
            ),
            // --- Drowning -------------------------------------------------
            (
                line(&format!("drowned(?: whilst trying to escape {killer})?")),
                DeathCategory::Drowning,
            ),
            // --- Explosion --------------------------------------------------
            (line("blew up"), DeathCategory::Explosion),
            (line(&format!("was blown up by {killer}")), DeathCategory::Explosion),
            // --- Lightning --------------------------------------------------
            (line("was struck by lightning"), DeathCategory::Lightning),
            // --- Void -------------------------------------------------------
            (line("fell out of the world"), DeathCategory::Void),
            (
                line(&format!("didn't want to live in the same world as {killer}")),
                DeathCategory::Void,
            ),
            // --- Starvation -------------------------------------------------
            (line("starved to death"), DeathCategory::Starvation),
            // --- Suffocation ------------------------------------------------
            (line("suffocated in a wall"), DeathCategory::Suffocation),
            // --- Catch-all (Other) ------------------------------------------
            (line("was pricked to death"), DeathCategory::Other),
            (line("discovered floor was lava"), DeathCategory::Other),
            (line("was squashed by a falling anvil"), DeathCategory::Other),
            (line("was skewered by a falling stalactite"), DeathCategory::Other),
            (line("experienced kinetic energy"), DeathCategory::Other),
            (
                line(&format!("walked into a cactus while trying to escape {killer}")),
                DeathCategory::Other,
            ),
        ]
    })
}

/// Matches vanilla death-broadcast lines, e.g. `[12:34:56] [Server
/// thread/INFO]: Juan was slain by Zombie`. Same anti-spoof reasoning as
/// [`parse_deed_line`]: the player-name group is the real username charset,
/// so a chat line `<Juan> was slain by Zombie` never matches (the literal
/// `<` right after `INFO]: ` can't satisfy it), and join/leave/advancement
/// lines simply don't match any known death phrase.
pub fn parse_death_line(line: &str) -> Option<Death> {
    let line = line.trim();
    for (re, category) in death_patterns() {
        if let Some(cap) = re.captures(line) {
            let player = cap.name("player").unwrap().as_str().to_string();
            let killer = cap.name("killer").map(|m| m.as_str().to_string());
            let phrase = line[cap.name("player").unwrap().end()..].trim().to_string();
            return Some(Death { player, category: *category, killer, phrase });
        }
    }
    None
}

fn pick_death<'a>(pool: &[&'a str], death: &Death) -> &'a str {
    let key = format!("{}\u{0}{}", death.player, death.phrase);
    let idx = fnv1a(&key) as usize % pool.len();
    pool[idx]
}

const SLAIN_BY_POOL: &[&str] = &[
    ":skull_crossbones: Cayó **{player}**, atravesado por la mano de **{killer}**. Ni el mismísimo Cid habría podido evitar tan fiero desenlace.",
    ":skull_crossbones: **{player}** ha rendido el alma a manos de **{killer}**. Que descanse en el Olimpo de los hidalgos caídos, que allí ya no hay TPS que valga.",
    ":skull_crossbones: Triste sino el de **{player}**, vencido por **{killer}**. El cronista anota la hazaña ajena y calla, por piedad, la propia torpeza.",
    ":skull_crossbones: **{killer}** se alza sobre el cuerpo yaciente de **{player}**. Así se escriben las crónicas: con sangre, sudor y un respawn pendiente.",
];

const FALL_POOL: &[&str] = &[
    ":dizzy_face: **{player}** confundió el vuelo con la caída, y la caída ganó por goleada. Al suelo fue a dar, sin alas ni Pegaso que lo sostuviera.",
    ":dizzy_face: Cayó **{player}** desde las alturas, recordando —demasiado tarde— que Rocinante no tiene plumas. El suelo, paciente, lo aguardaba.",
    ":dizzy_face: **{player}** ha comprobado, a su costa, que la gravedad no respeta ni a caballeros andantes. Mal viaje el de bajada.",
    ":dizzy_face: De lo alto cayó **{player}**, y el batacazo quedará en la memoria de la ínsula como advertencia a los demás trepadores.",
];

const FIRE_OR_LAVA_POOL: &[&str] = &[
    ":fire: **{player}** confundió la lava con agua de baño, error que ni el manual del escudero más torpe recomienda cometer dos veces.",
    ":fire: Ardió **{player}** como antorcha de vigilia, prueba de que ni el fuego respeta a los caballeros errantes de esta ínsula.",
    ":fire: **{player}** ha aprendido, por las malas, que el fuego no negocia ni conoce de treguas. Cenizas quedan de tan ardiente lección.",
    ":fire: Las llamas reclamaron a **{player}**, que salió de este mundo más achicharrado que un tasajo olvidado al fuego de Sancho.",
];

const DROWNING_POOL: &[&str] = &[
    ":ocean: **{player}** se ahogó como quien confunde el océano con una alberca de aldea. Ni la armadura más ligera flota sola.",
    ":ocean: Las aguas cerraron sobre **{player}**, que aprendió tarde que ni los caballeros andantes tienen branquias.",
    ":ocean: **{player}** se hundió sin gloria ni bandera, tragado por unas aguas tan traicioneras como cualquier mar de leyenda.",
    ":ocean: El agua venció a **{player}** en su ley más antigua: quien no respira, no cabalga. Descanse en las profundidades.",
];

const EXPLOSION_POOL: &[&str] = &[
    ":boom: **{player}** voló en mil pedazos, cortesía de **{killer}**. Ni el yelmo de Mambrino sobrevive tal estruendo.",
    ":boom: Un estallido, un grito, y de **{player}** solo quedó el eco. **{killer}** se cobra su fogosa venganza.",
    ":boom: **{player}** aprendió que **{killer}** no negocia: solo detona. Que sirva de escarmiento a los demás caballeros pirotécnicos.",
    ":boom: La pólvora —o lo que fuere— de **{killer}** dejó a **{player}** esparcido por media ínsula. Menudo estruendo para la crónica de hoy.",
];

/// Every variant here nods at la Rueda de la Fortuna's own `rayo` severe
/// curse (see `fortuna.rs`) — an organic lightning death is too good a
/// coincidence for the cronista to let pass without the wink.
const LIGHTNING_POOL: &[&str] = &[
    ":zap: ¡Un rayo parte los cielos y a **{player}**! El cronista jura reconocer en ello la mano traviesa de la Rueda de la Fortuna y su temida maldición del rayo.",
    ":zap: **{player}** ha sido fulminado como si la propia Rueda de la Fortuna hubiese decretado su severa maldición del rayo fuera de horario. Cosas de la fortuna, que no respeta calendario.",
    ":zap: Cayó el rayo sobre **{player}**, y este cronista no puede sino pensar que la Rueda de la Fortuna anda repartiendo su maldición del rayo sin previo aviso.",
    ":zap: ¡Zeus mismo, o la Rueda de la Fortuna en su vertiente más cruel, ha fulminado a **{player}**! Que conste: esta casa ya avisaba de tal maldición.",
];

const VOID_POOL: &[&str] = &[
    ":hole: **{player}** se despeñó fuera de los límites mismos del mundo conocido, como quien cae del mapa de un cartógrafo distraído.",
    ":hole: El vacío reclamó a **{player}**, que descubrió —a las malas— que la ínsula sí tiene bordes, y son definitivos.",
    ":hole: **{player}** cayó más allá de donde la brújula señala, perdido en el confín mismo de la creación.",
    ":hole: De **{player}** solo queda el recuerdo: se fue directo al vacío, sin escala ni paracaídas.",
];

const STARVATION_POOL: &[&str] = &[
    ":bread: **{player}** murió de hambre, tragedia que ni Sancho con su bota de vino pudo prevenir a tiempo.",
    ":bread: El estómago de **{player}** dio su último aviso, y nadie acudió con pan. Que esta crónica sirva de recordatorio: comed, vuestras mercedes.",
    ":bread: **{player}** se dejó morir de inanición, más ocupado en la aventura que en la despensa. Error de principiantes.",
    ":bread: Ni un mendrugo halló **{player}** a tiempo. El hambre, paciente, cobró su pieza.",
];

const SUFFOCATION_POOL: &[&str] = &[
    ":bricks: **{player}** quedó atrapado dentro de un muro, aplastado por su propia torpeza arquitectónica.",
    ":bricks: La piedra no perdona: **{player}** se asfixió dentro de un muro, víctima de la construcción menos afortunada de la ínsula.",
    ":bricks: **{player}** confundió un bloque con aire respirable. El resultado, por desgracia, fue el esperado.",
    ":bricks: Sepultado en vida dentro de un muro quedó **{player}**, lección dura sobre mirar antes de colocar bloques.",
];

const OTHER_DEATH_POOL: &[&str] = &[
    ":scroll: **{player}** ha encontrado la muerte por vías que ni el propio cronista sabe versar con precisión. Que baste decir: cayó.",
    ":scroll: Se apagó la vela de **{player}**, por causas que esta crónica registra mas no alcanza a explicar del todo.",
    ":scroll: **{player}** ha perecido de forma tan singular que ni Cide Hamete Benengeli, el sabio historiador, sabría cómo narrarla.",
    ":scroll: Fin de la aventura para **{player}**, por un percance que esta crónica archiva bajo el rótulo de \"cosas que pasan en la ínsula\".",
];

fn death_pool(category: DeathCategory) -> &'static [&'static str] {
    match category {
        DeathCategory::SlainBy => SLAIN_BY_POOL,
        DeathCategory::Fall => FALL_POOL,
        DeathCategory::FireOrLava => FIRE_OR_LAVA_POOL,
        DeathCategory::Drowning => DROWNING_POOL,
        DeathCategory::Explosion => EXPLOSION_POOL,
        DeathCategory::Lightning => LIGHTNING_POOL,
        DeathCategory::Void => VOID_POOL,
        DeathCategory::Starvation => STARVATION_POOL,
        DeathCategory::Suffocation => SUFFOCATION_POOL,
        DeathCategory::Other => OTHER_DEATH_POOL,
    }
}

/// Weave a Quijote-voiced roast for `death`. Selection within the category
/// pool is deterministic (hashed on player+phrase), same style as
/// [`narrate`]. Pools for categories where a killer can't always be
/// identified (`Fall`, `FireOrLava`, ...) never reference `{killer}`;
/// `SlainBy` and `Explosion` always do, and both are only ever matched by
/// patterns that require a killer capture, so it's always present there.
pub fn roast(death: &Death) -> String {
    let pool = death_pool(death.category);
    let template = pick_death(pool, death);
    let mut msg = template.replace("{player}", &death.player);
    if let Some(k) = &death.killer {
        msg = msg.replace("{killer}", k);
    }
    msg
}

/// Process-lifetime dedup wrapper: feed it raw (possibly multi-line, tail-
/// overlapping) server log text each tick and get back narrations only for
/// deed and death lines not seen before in this process. Missing history
/// from before a bot restart is acceptable; re-announcing a line already
/// narrated is not, so the dedup set is keyed on the exact raw line.
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
                continue;
            }
            if let Some(death) = parse_death_line(line) {
                if self.seen.insert(line.to_string()) {
                    out.push(roast(&death));
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

    // --- Death eulogies -----------------------------------------------------

    #[test]
    fn parses_slain_by_with_killer() {
        let d = parse_death_line("[12:34:56] [Server thread/INFO]: Juan was slain by Zombie").unwrap();
        assert_eq!(d.player, "Juan");
        assert_eq!(d.category, DeathCategory::SlainBy);
        assert_eq!(d.killer.as_deref(), Some("Zombie"));
    }

    #[test]
    fn parses_shot_and_fireballed_as_slain_by() {
        let d = parse_death_line("[12:34:56] [Server thread/INFO]: Juan was shot by Skeleton").unwrap();
        assert_eq!(d.category, DeathCategory::SlainBy);
        assert_eq!(d.killer.as_deref(), Some("Skeleton"));

        let d = parse_death_line("[12:34:56] [Server thread/INFO]: Ana was fireballed by Ghast").unwrap();
        assert_eq!(d.category, DeathCategory::SlainBy);
        assert_eq!(d.killer.as_deref(), Some("Ghast"));
    }

    #[test]
    fn parses_slain_by_with_weapon_suffix_still_extracts_bare_killer() {
        let d = parse_death_line(
            "[12:34:56] [Server thread/INFO]: Juan was slain by Bob using [Diamond Sword]",
        )
        .unwrap();
        assert_eq!(d.category, DeathCategory::SlainBy);
        assert_eq!(d.killer.as_deref(), Some("Bob"));
    }

    #[test]
    fn parses_fall_variants_with_and_without_killer() {
        for phrase in [
            "fell from a high place",
            "hit the ground too hard",
            "fell off a ladder",
            "fell off some scaffolding",
            "fell while climbing",
        ] {
            let line = format!("[12:34:56] [Server thread/INFO]: Juan {phrase}");
            let d = parse_death_line(&line).unwrap_or_else(|| panic!("didn't match: {line}"));
            assert_eq!(d.category, DeathCategory::Fall, "phrase: {phrase}");
            assert_eq!(d.player, "Juan");
        }

        let d = parse_death_line(
            "[12:34:56] [Server thread/INFO]: Juan was doomed to fall by Ana",
        )
        .unwrap();
        assert_eq!(d.category, DeathCategory::Fall);
        assert_eq!(d.killer.as_deref(), Some("Ana"));
    }

    #[test]
    fn parses_fire_and_lava_variants() {
        for phrase in ["went up in flames", "burned to death", "tried to swim in lava"] {
            let line = format!("[12:34:56] [Server thread/INFO]: Juan {phrase}");
            let d = parse_death_line(&line).unwrap_or_else(|| panic!("didn't match: {line}"));
            assert_eq!(d.category, DeathCategory::FireOrLava, "phrase: {phrase}");
        }
    }

    #[test]
    fn parses_drowning_with_and_without_escapee() {
        let d = parse_death_line("[12:34:56] [Server thread/INFO]: Juan drowned").unwrap();
        assert_eq!(d.category, DeathCategory::Drowning);
        assert_eq!(d.killer, None);

        let d = parse_death_line(
            "[12:34:56] [Server thread/INFO]: Juan drowned whilst trying to escape Drowned",
        )
        .unwrap();
        assert_eq!(d.category, DeathCategory::Drowning);
        assert_eq!(d.killer.as_deref(), Some("Drowned"));
    }

    #[test]
    fn parses_explosion_with_and_without_killer() {
        let d = parse_death_line("[12:34:56] [Server thread/INFO]: Juan blew up").unwrap();
        assert_eq!(d.category, DeathCategory::Explosion);
        assert_eq!(d.killer, None);

        let d = parse_death_line(
            "[12:34:56] [Server thread/INFO]: Juan was blown up by Creeper",
        )
        .unwrap();
        assert_eq!(d.category, DeathCategory::Explosion);
        assert_eq!(d.killer.as_deref(), Some("Creeper"));
    }

    #[test]
    fn parses_lightning() {
        let d = parse_death_line("[12:34:56] [Server thread/INFO]: Juan was struck by lightning").unwrap();
        assert_eq!(d.category, DeathCategory::Lightning);
        assert_eq!(d.killer, None);
    }

    #[test]
    fn parses_void_with_and_without_pusher() {
        let d = parse_death_line("[12:34:56] [Server thread/INFO]: Juan fell out of the world").unwrap();
        assert_eq!(d.category, DeathCategory::Void);
        assert_eq!(d.killer, None);

        let d = parse_death_line(
            "[12:34:56] [Server thread/INFO]: Juan didn't want to live in the same world as Ana",
        )
        .unwrap();
        assert_eq!(d.category, DeathCategory::Void);
        assert_eq!(d.killer.as_deref(), Some("Ana"));
    }

    #[test]
    fn parses_starvation_and_suffocation() {
        let d = parse_death_line("[12:34:56] [Server thread/INFO]: Juan starved to death").unwrap();
        assert_eq!(d.category, DeathCategory::Starvation);

        let d = parse_death_line("[12:34:56] [Server thread/INFO]: Juan suffocated in a wall").unwrap();
        assert_eq!(d.category, DeathCategory::Suffocation);
    }

    #[test]
    fn parses_catch_all_other_deaths() {
        let d = parse_death_line("[12:34:56] [Server thread/INFO]: Juan was pricked to death").unwrap();
        assert_eq!(d.category, DeathCategory::Other);

        let d = parse_death_line("[12:34:56] [Server thread/INFO]: Juan discovered floor was lava").unwrap();
        assert_eq!(d.category, DeathCategory::Other);
    }

    #[test]
    fn rejects_chat_line_impersonating_a_death() {
        // Same anti-spoof reasoning as the advancement parser: a player
        // literally typing "was slain by Zombie" in chat is rendered with
        // angle brackets and must never be mistaken for a real death.
        assert!(parse_death_line(
            "[12:34:56] [Server thread/INFO]: <Juan> was slain by Zombie"
        )
        .is_none());
    }

    #[test]
    fn rejects_join_leave_and_advancement_lines_as_deaths() {
        assert!(parse_death_line("[12:34:56] [Server thread/INFO]: Juan joined the game").is_none());
        assert!(parse_death_line("[12:34:56] [Server thread/INFO]: Juan left the game").is_none());
        assert!(parse_death_line(
            "[12:34:56] [Server thread/INFO]: Juan has made the advancement [Stone Age]"
        )
        .is_none());
    }

    #[test]
    fn death_pools_are_nonempty_and_unique() {
        let pools: [&[&str]; 10] = [
            SLAIN_BY_POOL,
            FALL_POOL,
            FIRE_OR_LAVA_POOL,
            DROWNING_POOL,
            EXPLOSION_POOL,
            LIGHTNING_POOL,
            VOID_POOL,
            STARVATION_POOL,
            SUFFOCATION_POOL,
            OTHER_DEATH_POOL,
        ];
        for pool in pools {
            assert!(pool.len() >= 3, "pool too small: {pool:?}");
            assert!(pool.iter().all(|s| !s.is_empty()));
            let unique: HashSet<_> = pool.iter().collect();
            assert_eq!(unique.len(), pool.len(), "duplicate entries: {pool:?}");
        }
    }

    #[test]
    fn lightning_roasts_all_nod_to_la_rueda_de_la_fortuna() {
        for tpl in LIGHTNING_POOL {
            assert!(
                tpl.to_lowercase().contains("rueda de la fortuna"),
                "lightning roast missing the wheel nod: {tpl}"
            );
        }
    }

    #[test]
    fn roast_interpolates_and_never_panics() {
        let deaths = [
            Death {
                player: "Juan".into(),
                category: DeathCategory::SlainBy,
                killer: Some("Zombie".into()),
                phrase: "was slain by Zombie".into(),
            },
            Death { player: "Ana".into(), category: DeathCategory::Fall, killer: None, phrase: "fell from a high place".into() },
            Death { player: "Bob".into(), category: DeathCategory::Lightning, killer: None, phrase: "was struck by lightning".into() },
            Death { player: "".into(), category: DeathCategory::Other, killer: None, phrase: "".into() },
        ];
        for d in deaths {
            let msg = roast(&d);
            assert!(!msg.contains("{player}") && !msg.contains("{killer}"));
            if !d.player.is_empty() {
                assert!(msg.contains(&d.player));
            }
            if let Some(k) = &d.killer {
                assert!(msg.contains(k));
            }
        }
    }

    #[test]
    fn herald_detects_and_dedups_death_lines_across_two_process_calls() {
        let mut h = Herald::new();
        let log = "[12:34:56] [Server thread/INFO]: Juan was slain by Zombie\n[12:35:00] [Server thread/INFO]: Juan joined the game\n[12:36:00] [Server thread/INFO]: Ana fell from a high place\n";
        let first = h.process(log);
        assert_eq!(first.len(), 2);

        // Overlapping tail: same two death lines, plus one new one.
        let overlapping = "[12:34:56] [Server thread/INFO]: Juan was slain by Zombie\n[12:36:00] [Server thread/INFO]: Ana fell from a high place\n[12:37:00] [Server thread/INFO]: Bob was struck by lightning\n";
        let second = h.process(overlapping);
        assert_eq!(second.len(), 1);
        assert!(second[0].contains("Bob"));
    }

    #[test]
    fn herald_process_mixes_deeds_and_deaths_in_one_call() {
        let mut h = Herald::new();
        let log = "[12:34:56] [Server thread/INFO]: Juan has made the advancement [Stone Age]\n[12:35:00] [Server thread/INFO]: Ana was slain by Zombie\n";
        let out = h.process(log);
        assert_eq!(out.len(), 2);
    }
}
