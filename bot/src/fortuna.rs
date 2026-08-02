//! La Rueda de la Fortuna: at each session milestone the bot spins a
//! probabilistic wheel and turns the result into real in-game consequences
//! (RCON console commands) plus narration for both the server chat and the
//! Discord notify channel.
//!
//! Pure logic + tests live here; the only I/O (RCON execution, `channel.say`)
//! happens in the callers (`monitor.rs`'s announcement loop, `commands.rs`'s
//! `/fortuna`). Blessings stay modest (food/xp/short buffs, never diamonds);
//! curses may summon hostiles or even kill, but never touch blocks or
//! inventories (no tnt/creeper/setblock/fill/clear/kill anywhere in the
//! effect tables — enforced by tests, not just convention).
//!
//! Coheres with the Matcha Flavoured 1.03 datapack already running on the
//! ínsula: the jackpot blessing (`disco_de_oro`) gifts the vanilla music
//! disc carrying Matcha's own `main:golden` jukebox song, and nothing here
//! touches Matcha's recipes, loot tables or dimensions.

/// A source of pseudo-random `u32`s. Production code uses [`EntropyRolls`];
/// tests use fixed sequences via the blanket `FnMut() -> u32` impl below.
pub trait RollSource {
    fn roll(&mut self) -> u32;
}

impl<F: FnMut() -> u32> RollSource for F {
    fn roll(&mut self) -> u32 {
        self()
    }
}

/// SplitMix64, seeded from `SystemTime` nanoseconds XOR'd with a
/// `RandomState`-derived hash so that two instances created back-to-back
/// (even within the same nanosecond) still diverge. Deliberately not the
/// `rand` crate: this codebase stays RNG-dependency-free, and a party-game
/// wheel doesn't need a CSPRNG.
pub struct EntropyRolls {
    state: u64,
}

impl EntropyRolls {
    pub fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        use std::hash::{BuildHasher, Hasher};
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(nanos);
        Self { state: nanos ^ hasher.finish() }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

impl Default for EntropyRolls {
    fn default() -> Self {
        Self::new()
    }
}

impl RollSource for EntropyRolls {
    fn roll(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Blessing,
    MinorCurse,
    MediumCurse,
    SevereCurse,
}

/// `[blessing, minor, medium, severe]` out of 100 for a given session hour.
/// Hours are clamped to `2..=8`: nothing crosses the wheel below 2h, and 8h+
/// all share the terminally-online table.
fn weights_for(hours: u32) -> [u32; 4] {
    match hours.clamp(2, 8) {
        2 => [70, 20, 8, 2],
        3 => [55, 25, 15, 5],
        4 => [40, 30, 20, 10],
        5 => [30, 30, 25, 15],
        6 => [20, 30, 30, 20],
        7 => [12, 28, 32, 28],
        _ => [5, 20, 35, 40], // 8+
    }
}

/// Map one roll onto a category given cumulative weights (out of 100).
/// Boundaries are `[0, w0)` blessing, `[w0, w0+w1)` minor, etc. — the last
/// bucket (severe) also catches any stray remainder so this never panics
/// regardless of roll value.
fn category_for_roll(weights: [u32; 4], roll: u32) -> Category {
    let r = roll % 100;
    let mut acc = 0u32;
    for (i, w) in weights.iter().enumerate() {
        acc += w;
        if r < acc {
            return match i {
                0 => Category::Blessing,
                1 => Category::MinorCurse,
                2 => Category::MediumCurse,
                _ => Category::SevereCurse,
            };
        }
    }
    Category::SevereCurse
}

struct EffectDef {
    id: &'static str,
    category: Category,
    /// Console command templates with `{player}` placeholders, executed in
    /// order. Every entry starts with `give `, `effect give `, `xp add `, or
    /// `execute at ` — enforced by a test sweep, along with a forbidden-
    /// substring check (no tnt/creeper/setblock/fill/clear/kill).
    commands: &'static [&'static str],
    /// Third-person-singular flavor clause, spliced into narration as
    /// `{player} {deed}`. No placeholders of its own.
    deed: &'static str,
}

/// All 23 effect table entries: 11 blessings, 4 minor curses, 4 medium
/// curses, 4 severe curses.
const EFFECTS: &[EffectDef] = &[
    // --- Blessings (11) ---------------------------------------------------
    EffectDef {
        id: "pan",
        category: Category::Blessing,
        commands: &["give {player} minecraft:bread 3"],
        deed: "recibe tres panes recién horneados, cortesía de la fortuna",
    },
    EffectDef {
        id: "carne",
        category: Category::Blessing,
        commands: &["give {player} minecraft:cooked_beef 2"],
        deed: "recibe dos filetes de carne asada, banquete de improviso",
    },
    EffectDef {
        id: "zanahoria_dorada",
        category: Category::Blessing,
        commands: &["give {player} minecraft:golden_carrot 2"],
        deed: "recibe dos zanahorias doradas, manjar reservado a los favoritos del destino",
    },
    EffectDef {
        id: "xp_menor",
        category: Category::Blessing,
        commands: &["xp add {player} 2 levels"],
        deed: "es agraciado con dos niveles de experiencia",
    },
    EffectDef {
        id: "xp_mayor",
        category: Category::Blessing,
        commands: &["xp add {player} 5 levels"],
        deed: "es agraciado con cinco niveles de experiencia, generosidad de la rueda",
    },
    EffectDef {
        id: "velocidad",
        category: Category::Blessing,
        commands: &["effect give {player} minecraft:speed 120"],
        deed: "siente el viento a favor: velocidad por dos minutos",
    },
    EffectDef {
        id: "prisa",
        category: Category::Blessing,
        commands: &["effect give {player} minecraft:haste 120"],
        deed: "encuentra sus manos veloces como las de un herrero, por dos minutos",
    },
    EffectDef {
        id: "regeneracion",
        category: Category::Blessing,
        commands: &["effect give {player} minecraft:regeneration 30"],
        deed: "siente sanar sus heridas con una bendición pasajera",
    },
    EffectDef {
        id: "antorchas",
        category: Category::Blessing,
        commands: &["give {player} minecraft:torch 16"],
        deed: "recibe dieciséis antorchas para no andar a ciegas por la ínsula",
    },
    EffectDef {
        id: "flechas",
        category: Category::Blessing,
        commands: &["give {player} minecraft:arrow 16"],
        deed: "recibe dieciséis flechas para su carcaj",
    },
    EffectDef {
        id: "disco_de_oro",
        category: Category::Blessing,
        commands: &[
            "give {player} minecraft:music_disc_cat[minecraft:jukebox_playable={song:\"main:golden\"}]",
        ],
        deed: "gana el Disco Dorado de Ciren, tesoro sonoro del recetario de bendiciones de Matcha",
    },
    // --- Minor curses (4) ---------------------------------------------------
    EffectDef {
        id: "hambre",
        category: Category::MinorCurse,
        commands: &["effect give {player} minecraft:hunger 30"],
        deed: "siente morderle el hambre por medio minuto",
    },
    EffectDef {
        id: "lentitud",
        category: Category::MinorCurse,
        commands: &["effect give {player} minecraft:slowness 30"],
        deed: "nota las piernas de plomo por medio minuto",
    },
    EffectDef {
        id: "ceguera",
        category: Category::MinorCurse,
        commands: &["effect give {player} minecraft:blindness 20"],
        deed: "es cegado veinte segundos, castigo leve de la rueda",
    },
    EffectDef {
        id: "zombi",
        category: Category::MinorCurse,
        commands: &["execute at {player} run summon minecraft:zombie ~ ~ ~"],
        deed: "atrae a un zombi solitario que sale a su encuentro",
    },
    // --- Medium curses (4) ---------------------------------------------------
    EffectDef {
        id: "veneno",
        category: Category::MediumCurse,
        commands: &["effect give {player} minecraft:poison 15"],
        deed: "es envenenado por quince segundos",
    },
    EffectDef {
        id: "fatiga",
        category: Category::MediumCurse,
        commands: &["effect give {player} minecraft:mining_fatigue 60"],
        deed: "sufre fatiga en las manos por un minuto entero: picar se vuelve tormento",
    },
    EffectDef {
        id: "cuadrilla",
        category: Category::MediumCurse,
        commands: &[
            "execute at {player} run summon minecraft:zombie ~ ~ ~",
            "execute at {player} run summon minecraft:zombie ~ ~ ~",
            "execute at {player} run summon minecraft:skeleton ~ ~ ~",
        ],
        deed: "es visitado por una cuadrilla de dos zombis y un esqueleto",
    },
    EffectDef {
        id: "levitacion",
        category: Category::MediumCurse,
        commands: &["effect give {player} minecraft:levitation 3 0"],
        deed: "es alzado tres segundos por una levitación traviesa",
    },
    // --- Severe curses (4) ---------------------------------------------------
    EffectDef {
        id: "marchitez",
        category: Category::SevereCurse,
        commands: &["effect give {player} minecraft:wither 10 1"],
        deed: "es marchitado diez segundos, maldición amarga",
    },
    EffectDef {
        id: "rayo",
        category: Category::SevereCurse,
        commands: &["execute at {player} run summon minecraft:lightning_bolt ~ ~ ~"],
        deed: "es alcanzado por un rayo caído del cielo mismo",
    },
    EffectDef {
        id: "jauria",
        category: Category::SevereCurse,
        commands: &[
            "execute at {player} run summon minecraft:zombie ~ ~ ~",
            "execute at {player} run summon minecraft:zombie ~ ~ ~",
            "execute at {player} run summon minecraft:skeleton ~ ~ ~",
            "execute at {player} run summon minecraft:skeleton ~ ~ ~",
            "execute at {player} run summon minecraft:spider ~ ~ ~",
        ],
        deed: "es rodeado por una jauría de dos zombis, dos esqueletos y una araña",
    },
    EffectDef {
        id: "ascension",
        category: Category::SevereCurse,
        commands: &["effect give {player} minecraft:levitation 5 3"],
        deed: "es lanzado a los cielos cinco segundos por la más severa de las levitaciones (unos 22 bloques de caída)",
    },
];

const BLESSING_POOL: &[&str] = &[
    ":four_leaf_clover: ¡Gira la Rueda de la Fortuna y sonríe a **{player}**! {deed}. Bendición donde las llaman, según el propio recetario de Matcha.",
    ":four_leaf_clover: La Rueda de la Fortuna se detiene en buen signo: **{player}** {deed}. Hasta la fortuna tiene sus días generosos.",
    ":four_leaf_clover: ¡Fortuna favorece a los osados! **{player}** {deed}, bendición que ni el propio recetario de bendiciones de Matcha desdeñaría.",
    ":four_leaf_clover: La rueda gira y **{player}** {deed}. Que Dulcinea tome nota: hoy el destino fue generoso.",
];

const MINOR_CURSE_POOL: &[&str] = &[
    ":cloud: La Rueda de la Fortuna tuerce el gesto: **{player}** {deed}. Nada que un buen caldo no cure.",
    ":cloud: Pequeño tropiezo del destino: **{player}** {deed}. La rueda también gasta bromas leves.",
    ":cloud: La fortuna, traviesa, dispone que **{player}** {deed}. Cosa de nada, ya lo dijo el refrán.",
    ":cloud: Gira la rueda y roza apenas: **{player}** {deed}. Molestia menor, hazaña intacta.",
];

const MEDIUM_CURSE_POOL: &[&str] = &[
    ":warning: La Rueda de la Fortuna aprieta más fuerte: **{player}** {deed}. Empieza la cosa a doler de veras.",
    ":warning: El destino cobra su peaje: **{player}** {deed}. Sancho recomendaría prudencia a partir de ahora.",
    ":warning: La rueda gira torcida: **{player}** {deed}. Ya no es broma, vuestra merced.",
    ":warning: Golpe de mediana fortuna: **{player}** {deed}. La ínsula toma nota del castigo.",
];

const SEVERE_CURSE_POOL: &[&str] = &[
    ":skull: ¡La Rueda de la Fortuna se cobra su tributo más cruel! **{player}** {deed}. Que no se diga que la fortuna no avisa.",
    ":skull: La rueda ha hablado, y sin piedad: **{player}** {deed}. Ni el propio cronista puede mirar sin estremecerse.",
    ":skull: Tiembla la ínsula: **{player}** {deed}. La severa fortuna no conoce de méritos, solo de suerte.",
    ":skull: La rueda gira hacia el abismo: **{player}** {deed}. Que sirva de escarmiento a los demás hidalgos.",
];

/// Single plain-text template sent to the server chat via rcon `say`. Not
/// Discord markdown (Minecraft chat won't render it), just the plain deed.
const GAME_MSG_TEMPLATE: &str = "La Rueda de la Fortuna gira para {player}: {deed}.";

fn pool_for(category: Category) -> &'static [&'static str] {
    match category {
        Category::Blessing => BLESSING_POOL,
        Category::MinorCurse => MINOR_CURSE_POOL,
        Category::MediumCurse => MEDIUM_CURSE_POOL,
        Category::SevereCurse => SEVERE_CURSE_POOL,
    }
}

/// A completed spin: the effect that landed, the console commands to run
/// (already spliced with `player`, in execution order), and the two
/// narrations (Discord-formatted, and plain for in-game `say`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spin {
    pub effect_id: &'static str,
    pub category: Category,
    pub commands: Vec<String>,
    pub discord_msg: String,
    pub game_msg: String,
}

fn build_spin(player: &str, effect: &'static EffectDef, variant_roll: u32) -> Spin {
    let commands: Vec<String> =
        effect.commands.iter().map(|c| c.replace("{player}", player)).collect();
    let pool = pool_for(effect.category);
    let template = pool[(variant_roll as usize) % pool.len()];
    let discord_msg =
        template.replace("{player}", player).replace("{deed}", effect.deed);
    let game_msg =
        GAME_MSG_TEMPLATE.replace("{player}", player).replace("{deed}", effect.deed);
    Spin { effect_id: effect.id, category: effect.category, commands, discord_msg, game_msg }
}

/// Validates a Minecraft username against the real client charset
/// (`[A-Za-z0-9_]{1,16}`), rejecting selectors (`@a`, `@p`, ...) and spaces
/// so nothing hostile ever reaches command splicing.
pub fn valid_player_name(name: &str) -> bool {
    let len = name.chars().count();
    (1..=16).contains(&len) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Spin the wheel for `player` at `hours` into their session. Consumes
/// exactly 3 rolls from `rng`: category, effect within category, narration
/// variant. Callers are expected to have already checked
/// [`valid_player_name`] before calling this (both `monitor.rs` and
/// `commands.rs` do); `spin` itself just does the splicing.
pub fn spin(player: &str, hours: u32, rng: &mut impl RollSource) -> Spin {
    let weights = weights_for(hours);
    let category = category_for_roll(weights, rng.roll());
    let candidates: Vec<&'static EffectDef> =
        EFFECTS.iter().filter(|e| e.category == category).collect();
    let effect = candidates[(rng.roll() as usize) % candidates.len()];
    build_spin(player, effect, rng.roll())
}

/// Force a specific effect id (bypassing hours/weights entirely) — used by
/// `/fortuna suerte:<id>` so every table entry is live-verifiable against
/// the running server. Consumes 1 roll (narration variant only). `None` for
/// an unknown id.
pub fn spin_forced(player: &str, effect_id: &str, rng: &mut impl RollSource) -> Option<Spin> {
    let effect = EFFECTS.iter().find(|e| e.id == effect_id)?;
    Some(build_spin(player, effect, rng.roll()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn fixed(seq: Vec<u32>) -> impl FnMut() -> u32 {
        let mut it = seq.into_iter();
        move || it.next().unwrap_or(0)
    }

    // 1. Weights sum to 100 for hours 0..=100.
    #[test]
    fn weights_always_sum_to_100() {
        for h in 0..=100u32 {
            let w = weights_for(h);
            assert_eq!(w.iter().sum::<u32>(), 100, "hours={h} weights={w:?}");
        }
    }

    // 2. Total-curse and severe monotonicity across 2..=8.
    #[test]
    fn curse_and_severe_weight_are_monotonically_nondecreasing() {
        let mut prev_curse = 0u32;
        let mut prev_severe = 0u32;
        for h in 2..=8u32 {
            let w = weights_for(h);
            let curse = w[1] + w[2] + w[3];
            let severe = w[3];
            assert!(curse >= prev_curse, "curse weight dropped at hour {h}: {w:?}");
            assert!(severe >= prev_severe, "severe weight dropped at hour {h}: {w:?}");
            prev_curse = curse;
            prev_severe = severe;
        }
    }

    // 3. Blessing >= 60 at h<=2, <= 5 at h>=8.
    #[test]
    fn blessing_weight_bounds_at_the_extremes() {
        for h in 0..=2u32 {
            assert!(weights_for(h)[0] >= 60, "hour {h}: {:?}", weights_for(h));
        }
        for h in 8..=100u32 {
            assert!(weights_for(h)[0] <= 5, "hour {h}: {:?}", weights_for(h));
        }
    }

    // 4. Category boundary rolls at h=2, plus u32::MAX.
    #[test]
    fn category_boundaries_at_two_hours() {
        let w = weights_for(2); // [70, 20, 8, 2]
        assert_eq!(category_for_roll(w, 69), Category::Blessing);
        assert_eq!(category_for_roll(w, 70), Category::MinorCurse);
        assert_eq!(category_for_roll(w, 89), Category::MinorCurse);
        assert_eq!(category_for_roll(w, 90), Category::MediumCurse);
        assert_eq!(category_for_roll(w, 97), Category::MediumCurse);
        assert_eq!(category_for_roll(w, 98), Category::SevereCurse);
        // Doesn't panic on an extreme roll, and resolves to something.
        let _ = category_for_roll(w, u32::MAX);
    }

    // 5. spin never panics across hours 0..=100 with adversarial roll
    // streams, and leaves no {player}/{deed} residue.
    #[test]
    fn spin_never_panics_and_leaves_no_placeholder_residue() {
        let streams: Vec<Vec<u32>> = vec![
            vec![0, 0, 0],
            vec![u32::MAX, u32::MAX, u32::MAX],
            vec![1, u32::MAX, 0],
            vec![99, 3, 2],
            vec![u32::MAX / 2, 17, 255],
        ];
        for h in 0..=100u32 {
            for s in &streams {
                let mut rng = fixed(s.clone());
                let spin = spin("Juan", h, &mut rng);
                assert!(!spin.discord_msg.contains("{player}"));
                assert!(!spin.discord_msg.contains("{deed}"));
                assert!(!spin.game_msg.contains("{player}"));
                assert!(!spin.game_msg.contains("{deed}"));
                for c in &spin.commands {
                    assert!(!c.contains("{player}"));
                }
            }
        }
    }

    // 6. Determinism with fixed rolls.
    #[test]
    fn spin_is_deterministic_given_fixed_rolls() {
        let mut rng1 = fixed(vec![10, 1, 2]);
        let mut rng2 = fixed(vec![10, 1, 2]);
        let a = spin("Juan", 4, &mut rng1);
        let b = spin("Juan", 4, &mut rng2);
        assert_eq!(a, b);
    }

    // 7. Golden command strings per effect id via spin_forced (player Juan).
    #[test]
    fn golden_command_strings_per_effect_id() {
        let cases: &[(&str, &[&str])] = &[
            ("pan", &["give Juan minecraft:bread 3"]),
            ("carne", &["give Juan minecraft:cooked_beef 2"]),
            ("zanahoria_dorada", &["give Juan minecraft:golden_carrot 2"]),
            ("xp_menor", &["xp add Juan 2 levels"]),
            ("xp_mayor", &["xp add Juan 5 levels"]),
            ("velocidad", &["effect give Juan minecraft:speed 120"]),
            ("prisa", &["effect give Juan minecraft:haste 120"]),
            ("regeneracion", &["effect give Juan minecraft:regeneration 30"]),
            ("antorchas", &["give Juan minecraft:torch 16"]),
            ("flechas", &["give Juan minecraft:arrow 16"]),
            (
                "disco_de_oro",
                &["give Juan minecraft:music_disc_cat[minecraft:jukebox_playable={song:\"main:golden\"}]"],
            ),
            ("hambre", &["effect give Juan minecraft:hunger 30"]),
            ("lentitud", &["effect give Juan minecraft:slowness 30"]),
            ("ceguera", &["effect give Juan minecraft:blindness 20"]),
            ("zombi", &["execute at Juan run summon minecraft:zombie ~ ~ ~"]),
            ("veneno", &["effect give Juan minecraft:poison 15"]),
            ("fatiga", &["effect give Juan minecraft:mining_fatigue 60"]),
            (
                "cuadrilla",
                &[
                    "execute at Juan run summon minecraft:zombie ~ ~ ~",
                    "execute at Juan run summon minecraft:zombie ~ ~ ~",
                    "execute at Juan run summon minecraft:skeleton ~ ~ ~",
                ],
            ),
            ("levitacion", &["effect give Juan minecraft:levitation 3 0"]),
            ("marchitez", &["effect give Juan minecraft:wither 10 1"]),
            ("rayo", &["execute at Juan run summon minecraft:lightning_bolt ~ ~ ~"]),
            (
                "jauria",
                &[
                    "execute at Juan run summon minecraft:zombie ~ ~ ~",
                    "execute at Juan run summon minecraft:zombie ~ ~ ~",
                    "execute at Juan run summon minecraft:skeleton ~ ~ ~",
                    "execute at Juan run summon minecraft:skeleton ~ ~ ~",
                    "execute at Juan run summon minecraft:spider ~ ~ ~",
                ],
            ),
            ("ascension", &["effect give Juan minecraft:levitation 5 3"]),
        ];
        assert_eq!(cases.len(), 23, "golden test must cover all 23 effects");
        for (id, expected) in cases {
            let mut rng = fixed(vec![0]);
            let spin = spin_forced("Juan", id, &mut rng).unwrap_or_else(|| panic!("unknown id {id}"));
            assert_eq!(&spin.commands, expected, "effect {id}");
            assert_eq!(spin.effect_id, *id);
        }
    }

    // 8. Forbidden-substring sweep.
    #[test]
    fn no_effect_touches_blocks_inventory_or_world_destructively() {
        let forbidden = ["tnt", "creeper", "setblock", "fill", "clear", "kill"];
        for e in EFFECTS {
            for c in e.commands {
                let lower = c.to_lowercase();
                for f in forbidden {
                    assert!(!lower.contains(f), "effect {} command {:?} contains forbidden substring {f}", e.id, c);
                }
                assert!(
                    c.starts_with("give ")
                        || c.starts_with("effect give ")
                        || c.starts_with("xp add ")
                        || c.starts_with("execute at "),
                    "effect {} command {:?} has an unexpected prefix",
                    e.id,
                    c
                );
            }
        }
    }

    // 9. Pools non-empty/unique, ids unique.
    #[test]
    fn narration_pools_are_nonempty_and_unique() {
        for pool in [BLESSING_POOL, MINOR_CURSE_POOL, MEDIUM_CURSE_POOL, SEVERE_CURSE_POOL] {
            assert!(pool.len() >= 4, "pool too small: {pool:?}");
            assert!(pool.iter().all(|s| !s.is_empty()));
            let unique: HashSet<_> = pool.iter().collect();
            assert_eq!(unique.len(), pool.len(), "duplicate entries in pool: {pool:?}");
        }
    }

    #[test]
    fn effect_ids_are_unique_and_cover_all_categories() {
        assert_eq!(EFFECTS.len(), 23);
        let ids: HashSet<&str> = EFFECTS.iter().map(|e| e.id).collect();
        assert_eq!(ids.len(), 23, "duplicate effect ids");
        assert_eq!(EFFECTS.iter().filter(|e| e.category == Category::Blessing).count(), 11);
        assert_eq!(EFFECTS.iter().filter(|e| e.category == Category::MinorCurse).count(), 4);
        assert_eq!(EFFECTS.iter().filter(|e| e.category == Category::MediumCurse).count(), 4);
        assert_eq!(EFFECTS.iter().filter(|e| e.category == Category::SevereCurse).count(), 4);
    }

    // 10. valid_player_name accept/reject matrix.
    #[test]
    fn valid_player_name_accept_reject_matrix() {
        assert!(valid_player_name("Juan"));
        assert!(valid_player_name("a"));
        assert!(valid_player_name("CamRG121"));
        assert!(valid_player_name("a_b_c_1234567_9")); // 16 chars
        assert!(!valid_player_name(""));
        assert!(!valid_player_name("this_name_is_seventeen")); // > 16
        assert!(!valid_player_name("@a"));
        assert!(!valid_player_name("@p[distance=..5]"));
        assert!(!valid_player_name("Juan Perez")); // space
        assert!(!valid_player_name("Juan;kill"));
        assert!(!valid_player_name("Ju\u{e1}n")); // accented, not ASCII alnum
    }

    // 11. spin_forced unknown id -> None.
    #[test]
    fn spin_forced_unknown_id_is_none() {
        let mut rng = fixed(vec![0]);
        assert!(spin_forced("Juan", "no_existe", &mut rng).is_none());
    }

    // 12. Two EntropyRolls instances diverge.
    #[test]
    fn two_entropy_rolls_instances_diverge() {
        let mut a = EntropyRolls::new();
        let mut b = EntropyRolls::new();
        let seq_a: Vec<u32> = (0..4).map(|_| a.roll()).collect();
        let seq_b: Vec<u32> = (0..4).map(|_| b.roll()).collect();
        assert_ne!(seq_a, seq_b, "two independently-seeded instances produced identical streams");
    }

    #[test]
    fn category_for_roll_never_panics_across_all_weight_tables() {
        for h in 0..=100u32 {
            let w = weights_for(h);
            for r in [0u32, 1, 50, 98, 99, 100, 1000, u32::MAX] {
                let _ = category_for_roll(w, r);
            }
        }
    }
}
