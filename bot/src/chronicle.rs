//! Session chronicle: tracks how long each hidalgo has been mounted on the
//! ínsula and narrates milestone hours in the notify channel.
//!
//! [`SessionTracker`] is pure logic (no I/O, no wall-clock reads of its
//! own) so it can be driven by a fake clock in tests. `run()` in
//! `monitor.rs` feeds it the RCON player list every 30 s and turns the
//! [`Announcement`]s it returns into Quijote-voiced text via
//! [`session_message`].

use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Announcement {
    pub player: String,
    pub hours: u32,
}

struct PlayerSession {
    start: Instant,
    /// Consecutive polls in which this player was absent. Reset to 0 the
    /// moment they're seen again.
    missed: u8,
    /// Highest full-hour threshold already announced for this session (0
    /// means none yet).
    last_announced_hour: u32,
}

/// Tracks per-player continuous presence across RCON polls and decides
/// when a session crosses an hourly milestone worth announcing.
#[derive(Default)]
pub struct SessionTracker {
    sessions: HashMap<String, PlayerSession>,
}

impl SessionTracker {
    /// Feed the set of currently-online player names for this poll.
    ///
    /// A player missing from `present` doesn't end their session
    /// immediately: one missed poll is tolerated (a single RCON hiccup
    /// shouldn't reset everyone's clocks). Two consecutive misses end it.
    /// Returns one [`Announcement`] per session that just crossed a new
    /// full-hour threshold (>= 2h), never repeating a threshold already
    /// announced.
    pub fn observe(&mut self, present: &[String], now: Instant) -> Vec<Announcement> {
        for name in present {
            self.sessions
                .entry(name.clone())
                .and_modify(|s| s.missed = 0)
                .or_insert(PlayerSession { start: now, missed: 0, last_announced_hour: 0 });
        }

        let mut ended = Vec::new();
        for (name, sess) in self.sessions.iter_mut() {
            if present.iter().any(|p| p == name) {
                continue;
            }
            if sess.missed >= 1 {
                ended.push(name.clone());
            } else {
                sess.missed += 1;
            }
        }
        for name in ended {
            self.sessions.remove(&name);
        }

        let mut announcements = Vec::new();
        for name in present {
            let Some(sess) = self.sessions.get_mut(name) else { continue };
            let elapsed_hours = (now.duration_since(sess.start).as_secs() / 3600) as u32;
            if elapsed_hours >= 2 && elapsed_hours > sess.last_announced_hour {
                sess.last_announced_hour = elapsed_hours;
                announcements.push(Announcement { player: name.clone(), hours: elapsed_hours });
            }
        }
        announcements
    }
}

fn pick<'a>(pool: &[&'a str], player: &str, hours: u32) -> &'a str {
    let idx = (player.chars().count() as u32).wrapping_add(hours) as usize % pool.len();
    pool[idx]
}

const TIER_2H: &[&str] = &[
    ":hourglass_flowing_sand: ¡Dos horas cumplidas! **{player}** cabalga sin desmontar por la ínsula, y hasta el cronista de esta historia alza la pluma en señal de admiración.",
    ":hourglass_flowing_sand: Dos horas lleva **{player}** en la brecha, firme como Amadís de Gaula en sus mejores páginas. Buen comienzo de hazaña.",
    ":hourglass_flowing_sand: Reparad, vuestras mercedes: **{player}** suma ya dos horas de porfía. Promete ser jornada memorable.",
    ":hourglass_flowing_sand: A las dos horas de justa, **{player}** aún no pide cuartel. Así empiezan las grandes gestas.",
];

const TIER_3H: &[&str] = &[
    ":thinking: Tres horas ya, y **{player}** no da tregua. Empieza uno a preguntarse si tanto denuedo es hazaña o pura tozudez.",
    ":thinking: A las tres horas, hasta el más pintado caballero suele parar a beber agua. **{player}**, sin embargo, sigue en danza.",
    ":thinking: Tres horas de brega lleva ya **{player}**. Quien mucho abarca, dicen, poco aprieta el mando de salir a estirar las piernas.",
    ":thinking: Van tres horas y **{player}** sigue en pie de guerra. El cronista empieza a tomar notas con una ceja alzada.",
];

const TIER_4H: &[&str] = &[
    ":horse: Cuatro horas cumplidas. A esta altura, el pobre Rocinante ya habría pedido cebada y una siesta a la sombra; **{player}** ni se inmuta.",
    ":horse: Ni el rocín más sufrido aguanta cuatro horas sin un respiro, y sin embargo **{player}** sigue en la silla. Admirable, o temerario.",
    ":horse: Rocinante, que todo lo sufre, llevaría ya cuatro horas cojeando hacia el establo. **{player}** ni lo piensa siquiera.",
    ":horse: Cuatro horas de cabalgata continua. Hasta las bestias de carga tienen su turno de descanso; **{player}**, al parecer, no lo necesita.",
];

const TIER_5H: &[&str] = &[
    ":wind_chime: Cinco horas al pie del cañón, y ahora son los propios molinos los que se preguntan si **{player}** se encuentra bien.",
    ":wind_chime: Cinco horas ya. Los gigantes —o molinos, según se mire— empiezan a mostrar preocupación genuina por **{player}**.",
    ":wind_chime: A las cinco horas de justa continua, hasta el viento que mueve las aspas se detiene a mirar con lástima a **{player}**.",
    ":wind_chime: Cinco horas sin desmontar. Las aspas del molino giran más despacio, como si también ellas quisieran preguntar: ¿todo bien por allá, **{player}**?",
];

const TIER_6H: &[&str] = &[
    ":stew: Seis horas sin desmontar. Sancho Panza, de haber estado presente, ya habría sacado la bota de vino y un tasajo, insistiendo a **{player}** en que parase a comer algo.",
    ":stew: A las seis horas hasta el escudero más paciente pierde la paciencia: Sancho pediría ya un alto para el potaje, con **{player}** de convidado de honor.",
    ":stew: Seis horas de aventura corrida. En algún lugar, Sancho suspira y murmura que ni el mejor caballero pelea con la panza vacía. Va por ti, **{player}**.",
    ":stew: Seis horas. Sancho ofrece, muy solemne, su propio potaje a cambio de que **{player}** se siente cinco minutos.",
];

const TIER_7H: &[&str] = &[
    ":ghost: Siete horas. Empieza a dudarse si **{player}** es aún hidalgo de carne y hueso, o ya solo el fantasma errante de su propio personaje.",
    ":ghost: A las siete horas la línea entre caballero y espectro se difumina; **{player}** vaga por la ínsula como alma que ni el propio cronista sabe si respira.",
    ":ghost: Siete horas sin tregua. Uno ya no sabe si mira a **{player}** o a su sombra, condenada a errar sin descanso por estos pagos.",
    ":ghost: Siete horas cumplidas. **{player}** ha cruzado la frontera invisible entre la vigilia y la leyenda, y nadie sabe bien de qué lado quedó.",
];

const TIER_8H: &[&str] = &[
    ":briefcase: ¡Ocho horas, por mis barbas! Sugiere este cronista, con todo respeto, que **{player}** considere un oficio honrado, toque pasto de verdad y recuerde que hasta Dulcinea ha empezado a preguntar por su paradero.",
    ":briefcase: Ocho horas cumplidas. Ni Don Quijote, en sus delirios más largos, pasó tanto tiempo sin bajar del caballo. Vuestra merced, **{player}**, quizá debería probar un oficio honrado — y ver el sol, que existe.",
    ":briefcase: A las ocho horas se decreta lo siguiente: **{player}** necesita tocar pasto real, buscarse un oficio honrado, y llamar a Dulcinea antes de que esta pierda la paciencia.",
    ":briefcase: Ocho horas sin desmontar. Este escribano recomienda, muy seriamente, que **{player}** deje la lanza, tome el aire, y considere que hasta los caballeros andantes cobraban por sus hazañas.",
    ":briefcase: Ocho horas. Se abre convocatoria pública: **{player}** busca oficio honrado, se ofrece disponibilidad inmediata, motivo del cambio: Dulcinea ya no contesta los mensajes.",
];

const TIER_ROAST: &[&str] = &[
    ":rotating_light: {hours} horas y contando. **{player}**, a este paso el yelmo de Mambrino le va a salir raíces.",
    ":rotating_light: {hours} horas de porfía. Ya ni los molinos lo consideran caballero: lo consideran parte del paisaje.",
    ":rotating_light: {hours} horas. Dulcinea ha mandado un segundo mensajero preguntando si **{player}** sigue con vida.",
    ":rotating_light: {hours} horas sin bajar del caballo. Sancho ya se comió el tasajo él solo, hastiado de esperar a **{player}**.",
    ":rotating_light: {hours} horas cumplidas. A este ritmo, **{player}** logrará el récord que ningún hidalgo de la Mancha quiso jamás.",
    ":rotating_light: {hours} horas. El propio cronista de esta historia empieza a sospechar que **{player}** ha confundido la ínsula con su morada permanente.",
];

/// Pick a Quijote-voiced line for `player` crossing the `hours` threshold.
/// Selection is deterministic (name length + hour, no RNG) so the same
/// player cycles through a tier's variants across sessions instead of
/// always getting the first one. Total over all `hours` values: below the
/// 2h floor it falls back to the 2h tier rather than panicking, though the
/// tracker never actually calls it below 2.
pub fn session_message(player: &str, hours: u32) -> String {
    let template = match hours {
        2 => pick(TIER_2H, player, hours),
        3 => pick(TIER_3H, player, hours),
        4 => pick(TIER_4H, player, hours),
        5 => pick(TIER_5H, player, hours),
        6 => pick(TIER_6H, player, hours),
        7 => pick(TIER_7H, player, hours),
        8 => pick(TIER_8H, player, hours),
        h if h >= 9 => pick(TIER_ROAST, player, h),
        _ => pick(TIER_2H, player, hours),
    };
    template.replace("{player}", player).replace("{hours}", &hours.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_announcement_before_two_hours() {
        let mut t = SessionTracker::default();
        let t0 = Instant::now();
        assert_eq!(t.observe(&names(&["juan"]), t0), vec![]);
        assert_eq!(
            t.observe(&names(&["juan"]), t0 + Duration::from_secs(3600 * 2 - 1)),
            vec![]
        );
    }

    #[test]
    fn announces_once_at_two_hour_threshold() {
        let mut t = SessionTracker::default();
        let t0 = Instant::now();
        t.observe(&names(&["juan"]), t0);
        let anns = t.observe(&names(&["juan"]), t0 + Duration::from_secs(3600 * 2));
        assert_eq!(anns, vec![Announcement { player: "juan".into(), hours: 2 }]);
        // Same hour again: no repeat.
        let anns = t.observe(&names(&["juan"]), t0 + Duration::from_secs(3600 * 2 + 60));
        assert_eq!(anns, vec![]);
    }

    #[test]
    fn announces_each_additional_full_hour_exactly_once() {
        let mut t = SessionTracker::default();
        let t0 = Instant::now();
        t.observe(&names(&["juan"]), t0);
        for h in 2..=5u64 {
            let anns = t.observe(&names(&["juan"]), t0 + Duration::from_secs(3600 * h));
            assert_eq!(anns, vec![Announcement { player: "juan".into(), hours: h as u32 }]);
            // Poll again mid-hour: no duplicate.
            let anns = t.observe(&names(&["juan"]), t0 + Duration::from_secs(3600 * h + 100));
            assert_eq!(anns, vec![]);
        }
    }

    #[test]
    fn tolerates_one_missed_poll_without_resetting_clock() {
        let mut t = SessionTracker::default();
        let t0 = Instant::now();
        t.observe(&names(&["juan"]), t0);
        // One poll where juan is absent (rcon hiccup / lag spike).
        assert_eq!(t.observe(&names(&[]), t0 + Duration::from_secs(60)), vec![]);
        // Juan is back; the clock kept running from t0, so at t0+2h we
        // still cross the threshold rather than starting over.
        let anns = t.observe(&names(&["juan"]), t0 + Duration::from_secs(3600 * 2));
        assert_eq!(anns, vec![Announcement { player: "juan".into(), hours: 2 }]);
    }

    #[test]
    fn two_consecutive_misses_end_the_session() {
        let mut t = SessionTracker::default();
        let t0 = Instant::now();
        t.observe(&names(&["juan"]), t0);
        t.observe(&names(&[]), t0 + Duration::from_secs(30)); // miss 1: tolerated
        t.observe(&names(&[]), t0 + Duration::from_secs(60)); // miss 2: session ends
        // Juan reappears well past the old t0 + 2h mark, but since the
        // session reset, this is a *new* session and shouldn't announce.
        let anns = t.observe(&names(&["juan"]), t0 + Duration::from_secs(3600 * 3));
        assert_eq!(anns, vec![]);
    }

    #[test]
    fn independent_players_tracked_separately() {
        let mut t = SessionTracker::default();
        let t0 = Instant::now();
        t.observe(&names(&["juan"]), t0);
        // ana joins one poll later.
        t.observe(&names(&["juan", "ana"]), t0 + Duration::from_secs(30));
        let anns = t.observe(&names(&["juan", "ana"]), t0 + Duration::from_secs(3600 * 2 + 30));
        // juan started at t0, ana at t0+30s; both cross 2h in this same
        // poll (30s granularity doesn't push ana past the 2h boundary yet).
        let mut players: Vec<_> = anns.iter().map(|a| a.player.clone()).collect();
        players.sort();
        assert_eq!(players, vec!["ana".to_string(), "juan".to_string()]);
    }

    #[test]
    fn session_pools_are_nonempty_and_unique() {
        let pools: [&[&str]; 8] = [
            TIER_2H, TIER_3H, TIER_4H, TIER_5H, TIER_6H, TIER_7H, TIER_8H, TIER_ROAST,
        ];
        for pool in pools {
            assert!(pool.len() >= 3, "pool too small: {pool:?}");
            assert!(pool.iter().all(|s| !s.is_empty()));
            let unique: std::collections::HashSet<_> = pool.iter().collect();
            assert_eq!(unique.len(), pool.len(), "duplicate entries in pool: {pool:?}");
        }
        assert!(TIER_ROAST.len() >= 5);
    }

    #[test]
    fn session_message_is_deterministic() {
        assert_eq!(session_message("juan", 4), session_message("juan", 4));
        assert_eq!(session_message("ana", 9), session_message("ana", 9));
    }

    #[test]
    fn session_message_distinct_across_hours_two_to_eight() {
        let msgs: Vec<String> = (2..=8).map(|h| session_message("juan", h)).collect();
        let unique: std::collections::HashSet<_> = msgs.iter().collect();
        assert_eq!(unique.len(), msgs.len(), "hours 2..=8 should never share a message: {msgs:?}");
    }

    #[test]
    fn session_message_never_panics_and_is_nonempty() {
        for hours in 0..=50u32 {
            for player in ["", "a", "juan", "a-very-long-name-indeed"] {
                let msg = session_message(player, hours);
                assert!(!msg.is_empty());
                assert!(!msg.contains("{player}") && !msg.contains("{hours}"));
            }
        }
    }

    #[test]
    fn roast_tier_includes_hour_number() {
        let msg = session_message("juan", 12);
        assert!(msg.contains("12"));
    }
}
