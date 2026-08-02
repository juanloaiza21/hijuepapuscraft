use crate::chronicle::{self, SessionTracker};
use crate::config::Config;
use crate::docker::DockerCtl;
use crate::fortuna;
use crate::heraldo::Herald;
use crate::parse;
use crate::rcon::McRcon;
use poise::serenity_prelude::{ChannelId, Http};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerState {
    Up,
    Starting,
    Unhealthy,
    Down,
}

pub fn classify(running: bool, health: Option<&str>, rcon_ok: bool) -> ServerState {
    if !running {
        return ServerState::Down;
    }
    match health {
        Some("unhealthy") => ServerState::Unhealthy,
        Some("starting") => ServerState::Starting,
        Some("healthy") => ServerState::Up,
        _ if rcon_ok => ServerState::Up,
        _ => ServerState::Starting,
    }
}

/// How far back we look when counting flap resets for instability detection.
const FLAP_WINDOW: Duration = Duration::from_secs(5 * 60);
/// Resets within the window at or above this count mean the ínsula is
/// crash-looping, not just taking a planned quick restart.
const FLAP_THRESHOLD: usize = 4;
/// Once an instability signal fires, stay quiet about further flapping for
/// this long so the channel isn't spammed while the crash loop continues.
const INSTABILITY_COOLDOWN: Duration = Duration::from_secs(15 * 60);
/// Defensive cap on the reset history so it can never grow unbounded; the
/// window-based pruning already keeps it small in ordinary operation.
const MAX_TRACKED_RESETS: usize = 16;

/// What a single [`Damper::observe`] call produced: an optional confirmed
/// state transition, and whether this reading pushed the flap-reset count
/// over the instability threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DamperSignal {
    pub transition: Option<ServerState>,
    pub instability: bool,
}

pub struct Damper {
    current: ServerState,
    pending: Option<(ServerState, u8)>,
    /// Timestamps of "flap resets": a pending state got discarded, either
    /// because a different state pre-empted it or because the reading
    /// bounced back to `current` before confirming. Bounded by window
    /// pruning plus `MAX_TRACKED_RESETS` as a hard ceiling.
    flap_resets: VecDeque<Instant>,
    /// Set once an instability signal fires; further resets before this
    /// instant are tracked-but-silent.
    suppressed_until: Option<Instant>,
}

impl Damper {
    pub fn new(initial: ServerState) -> Self {
        Self { current: initial, pending: None, flap_resets: VecDeque::new(), suppressed_until: None }
    }

    /// Feed one poll reading. `now` drives both the ordinary damper (for
    /// nothing, currently) and the flap-instability window/cooldown, and is
    /// injected (rather than read internally) so tests can drive it with a
    /// fake clock, same style as `chronicle::SessionTracker::observe`.
    pub fn observe(&mut self, s: ServerState, now: Instant) -> DamperSignal {
        let mut reset = false;
        let transition = if s == self.current {
            // A reading matching current discards any pending transition.
            // If something was actually pending, that's a flap reset; if
            // pending was already empty this is just steady state.
            if self.pending.take().is_some() {
                reset = true;
            }
            None
        } else if let Some((p, n)) = self.pending {
            if p == s {
                if n + 1 >= 2 {
                    self.current = s;
                    self.pending = None;
                    Some(s)
                } else {
                    self.pending = Some((p, n + 1));
                    None
                }
            } else {
                // Pending state pre-empted by a *different* pending state:
                // also a flap reset.
                self.pending = Some((s, 1));
                reset = true;
                None
            }
        } else {
            // First sighting of a departure from current: not a reset,
            // nothing was pending to discard yet.
            self.pending = Some((s, 1));
            None
        };

        let instability = if reset { self.record_flap_reset(now) } else { false };
        DamperSignal { transition, instability }
    }

    /// Record a flap reset at `now`, prune stale entries outside the
    /// window, and decide whether this tips the reset count over the
    /// instability threshold. Returns `true` at most once per cooldown
    /// period.
    fn record_flap_reset(&mut self, now: Instant) -> bool {
        while let Some(&front) = self.flap_resets.front() {
            if now.saturating_duration_since(front) > FLAP_WINDOW {
                self.flap_resets.pop_front();
            } else {
                break;
            }
        }

        if let Some(until) = self.suppressed_until {
            if now < until {
                // Still cooling down from a previous signal: track nothing
                // new, stay silent.
                return false;
            }
            self.suppressed_until = None;
        }

        self.flap_resets.push_back(now);
        if self.flap_resets.len() > MAX_TRACKED_RESETS {
            self.flap_resets.pop_front();
        }

        if self.flap_resets.len() >= FLAP_THRESHOLD {
            self.suppressed_until = Some(now + INSTABILITY_COOLDOWN);
            self.flap_resets.clear();
            true
        } else {
            false
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum BackupEvent {
    Succeeded,
    Failed(i64),
}

#[derive(Default)]
pub struct BackupWatch {
    was_running: bool,
}

impl BackupWatch {
    pub fn observe(&mut self, running: bool, exit_code: Option<i64>) -> Option<BackupEvent> {
        let ev = if self.was_running && !running {
            match exit_code {
                Some(0) => Some(BackupEvent::Succeeded),
                Some(c) => Some(BackupEvent::Failed(c)),
                None => None,
            }
        } else {
            None
        };
        self.was_running = running;
        ev
    }
}

/// 30 s loop. Owns its own damper and watch; sends notifications to the
/// configured channel. Spawned from main after the Discord client is ready.
pub async fn run(
    http: Arc<Http>,
    cfg: Config,
    docker: DockerCtl,
    rcon: Arc<Mutex<McRcon>>,
) {
    let channel = ChannelId::new(cfg.notify_channel_id);
    let mut damper = Damper::new(ServerState::Down);
    let mut backups = BackupWatch::default();
    let mut sessions = SessionTracker::default();
    let mut herald = Herald::new();
    let mut fortune_rng = fortuna::EntropyRolls::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
    loop {
        tick.tick().await;

        let mc = docker.inspect("mc").await.ok().flatten();
        let list_result = { rcon.lock().await.cmd("list").await };
        let rcon_ok = list_result.is_ok();
        let state = classify(
            mc.as_ref().map(|s| s.running).unwrap_or(false),
            mc.as_ref().and_then(|s| s.health.as_deref()),
            rcon_ok,
        );
        let signal = damper.observe(state, Instant::now());
        if let Some(t) = signal.transition {
            let msg = match t {
                ServerState::Up => ":green_circle: ¡En pie está la ínsula! Presta para recibir a sus caballeros.",
                ServerState::Starting => ":yellow_circle: La ínsula despereza sus engranajes; aguarde vuestra merced, que ya despierta.",
                ServerState::Unhealthy => ":orange_circle: La ínsula sufre de melancolía en sus engranajes (el tick loop flaquea). Considere vuestra merced un /restart.",
                ServerState::Down => ":red_circle: ¡Ha caído la ínsula! Los follones y malandrines han triunfado... por ahora.",
            };
            let _ = channel.say(&http, msg).await;
        }
        if signal.instability {
            let _ = channel
                .say(
                    &http,
                    ":warning: ¡La ínsula tiembla, vuestra merced! Cae y se alza sin cesar, como aspa de molino en tormenta. Un mal presagio: acuda alguien del consejo a mirar los logs.",
                )
                .await;
        }

        if let Ok(Some(b)) = docker.inspect("mc-backup").await {
            match backups.observe(b.running, b.exit_code) {
                Some(BackupEvent::Failed(c)) => {
                    let _ = channel
                        .say(
                            &http,
                            format!(":rotating_light: ¡La encomienda de respaldo ha FRACASADO con código {c}! Acuda vuestra merced presto."),
                        )
                        .await;
                }
                Some(BackupEvent::Succeeded) => {
                    tracing::info!("backup finished ok");
                }
                None => {}
            }
        }

        // Session chronicle: feed the RCON player list into the presence
        // tracker and narrate any milestone hours crossed this tick. An
        // unparseable or failed `list` reads as nobody present, which is
        // fine: SessionTracker tolerates absences up to its 5-minute grace
        // window before ending a session.
        let present = list_result
            .as_deref()
            .ok()
            .and_then(parse::parse_list)
            .map(|p| p.names)
            .unwrap_or_default();
        for ann in sessions.observe(&present, Instant::now()) {
            let _ = channel.say(&http, chronicle::session_message(&ann.player, ann.hours)).await;

            // La Rueda de la Fortuna: spin for this milestone and turn the
            // result into real RCON consequences. RCON failures here are
            // logged and swallowed, never propagated — a hiccup spinning
            // the wheel must never take down the monitor loop.
            if fortuna::valid_player_name(&ann.player) {
                let spin = fortuna::spin(&ann.player, ann.hours, &mut fortune_rng);
                tracing::info!(
                    "la rueda de la fortuna: {} @ {}h -> {} ({:?})",
                    ann.player,
                    ann.hours,
                    spin.effect_id,
                    spin.category
                );
                {
                    let mut r = rcon.lock().await;
                    if let Err(e) = r.cmd(&format!("say {}", spin.game_msg)).await {
                        tracing::warn!("fortuna: rcon say failed: {e:#}");
                    }
                    for cmd in &spin.commands {
                        if let Err(e) = r.cmd(cmd).await {
                            tracing::warn!("fortuna: rcon command {cmd:?} failed: {e:#}");
                        }
                    }
                }
                let _ = channel.say(&http, spin.discord_msg).await;
            }
        }

        // Deeds ledger: tail mc's log for freshly-earned advancements,
        // challenges and goals and have Don Quijote proclaim them. Herald
        // dedups internally so tail overlap across ticks never
        // double-announces.
        if let Ok(raw_logs) = docker.logs_tail("mc", 100).await {
            for msg in herald.process(&raw_logs) {
                let _ = channel.say(&http, msg).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_maps_observations() {
        assert_eq!(classify(false, None, false), ServerState::Down);
        assert_eq!(classify(true, Some("starting"), false), ServerState::Starting);
        assert_eq!(classify(true, Some("healthy"), true), ServerState::Up);
        assert_eq!(classify(true, Some("unhealthy"), true), ServerState::Unhealthy);
        // running, no healthcheck info, rcon answers: that's up
        assert_eq!(classify(true, None, true), ServerState::Up);
        // running but rcon dead and no health: still starting, not down
        assert_eq!(classify(true, None, false), ServerState::Starting);
    }

    #[test]
    fn damper_requires_two_consecutive_readings() {
        let mut d = Damper::new(ServerState::Up);
        let t0 = Instant::now();
        assert_eq!(d.observe(ServerState::Down, t0).transition, None); // first sighting
        assert_eq!(d.observe(ServerState::Down, t0).transition, Some(ServerState::Down)); // confirmed
        assert_eq!(d.observe(ServerState::Down, t0).transition, None); // no repeat notifications
    }

    #[test]
    fn damper_resets_on_flap() {
        let mut d = Damper::new(ServerState::Up);
        let t0 = Instant::now();
        assert_eq!(d.observe(ServerState::Down, t0).transition, None);
        assert_eq!(d.observe(ServerState::Up, t0).transition, None); // flap, no transition
        assert_eq!(d.observe(ServerState::Down, t0).transition, None); // counting restarts
        assert_eq!(d.observe(ServerState::Down, t0).transition, Some(ServerState::Down));
    }

    /// Drives an alternating Down/Up flip-flop against a damper whose
    /// current state is Up: every "Up" reading returns to current and
    /// discards the pending "Down", i.e. one flap reset per pair. This is
    /// the crash-loop shape from the bug report: no two consecutive
    /// readings ever match, so the plain transition damper never fires,
    /// but the flap-reset count climbs steadily.
    fn flap(d: &mut Damper, t0: Instant, count: u64) -> Vec<DamperSignal> {
        (0..count)
            .map(|i| {
                let s = if i % 2 == 0 { ServerState::Down } else { ServerState::Up };
                d.observe(s, t0 + Duration::from_secs(30 * i))
            })
            .collect()
    }

    #[test]
    fn instability_fires_once_after_four_resets_within_window() {
        let mut d = Damper::new(ServerState::Up);
        let t0 = Instant::now();
        let signals = flap(&mut d, t0, 8); // resets at i=1,3,5,7 -> 30s,90s,150s,210s
        assert!(signals.iter().all(|s| s.transition.is_none()));
        let fired = signals.iter().filter(|s| s.instability).count();
        assert_eq!(fired, 1, "expected exactly one instability signal, got {signals:?}");
        assert!(signals.last().unwrap().instability, "the 4th reset should be the one that fires");
    }

    #[test]
    fn three_resets_do_not_trigger_instability() {
        let mut d = Damper::new(ServerState::Up);
        let t0 = Instant::now();
        let signals = flap(&mut d, t0, 6); // resets at i=1,3,5 -> only 3
        assert!(signals.iter().all(|s| !s.instability));
    }

    #[test]
    fn resets_spread_beyond_five_minutes_do_not_trigger_instability() {
        let mut d = Damper::new(ServerState::Up);
        let t0 = Instant::now();
        // Four resets, but 200s apart: each new one prunes anything older
        // than the 5-minute window before counting, so it never reaches 4.
        let offsets = [0u64, 200, 400, 600, 800];
        let mut signals = Vec::new();
        for (i, &off) in offsets.iter().enumerate() {
            let now = t0 + Duration::from_secs(off);
            signals.push(d.observe(ServerState::Down, now));
            if i + 1 < offsets.len() {
                let next_now = t0 + Duration::from_secs(off + 1);
                signals.push(d.observe(ServerState::Up, next_now));
            }
        }
        assert!(signals.iter().all(|s| !s.instability), "{signals:?}");
    }

    #[test]
    fn cooldown_suppresses_further_signals_for_fifteen_minutes() {
        let mut d = Damper::new(ServerState::Up);
        let t0 = Instant::now();
        let first = flap(&mut d, t0, 8); // fires once at i=7 (t0+210s)
        assert_eq!(first.iter().filter(|s| s.instability).count(), 1);

        // Keep flapping hard, well inside the 15-minute cooldown: no
        // second signal, however many more resets pile up.
        let more: Vec<DamperSignal> = (8..24u64)
            .map(|i| {
                let s = if i % 2 == 0 { ServerState::Down } else { ServerState::Up };
                d.observe(s, t0 + Duration::from_secs(30 * i))
            })
            .collect();
        assert!(more.iter().all(|s| !s.instability), "cooldown should hold: {more:?}");
    }

    #[test]
    fn planned_quick_restart_stays_silent() {
        // Up -> one Down reading -> Up again: no transition (never
        // confirmed), and only a single flap reset, well under the
        // instability threshold.
        let mut d = Damper::new(ServerState::Up);
        let t0 = Instant::now();
        let sig1 = d.observe(ServerState::Down, t0);
        assert_eq!(sig1.transition, None);
        assert!(!sig1.instability);
        let sig2 = d.observe(ServerState::Up, t0 + Duration::from_secs(30));
        assert_eq!(sig2.transition, None);
        assert!(!sig2.instability);
    }

    #[test]
    fn backup_watch_fires_once_per_run_end() {
        let mut w = BackupWatch::default();
        assert_eq!(w.observe(true, None), None); // started
        assert_eq!(w.observe(false, Some(1)), Some(BackupEvent::Failed(1)));
        assert_eq!(w.observe(false, Some(1)), None); // no repeat
        assert_eq!(w.observe(true, None), None);
        assert_eq!(w.observe(false, Some(0)), Some(BackupEvent::Succeeded));
    }
}
