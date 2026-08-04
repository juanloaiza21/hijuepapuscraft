use crate::chronicle::{self, SessionTracker};
use crate::config::Config;
use crate::docker::{self, DockerCtl};
use crate::fortuna;
use crate::heraldo::Herald;
use crate::parse;
use crate::rcon::McRcon;
use poise::serenity_prelude::{
    ChannelId, CreateAllowedMentions, CreateMessage, Http, RoleId,
};
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
    /// Stuck mid-lifecycle per podman's own `State.Status` (stopping,
    /// removing, dead, paused) — distinct from `Down` because neither
    /// `/start` nor `/restart` can recover it; only the host watchdog's
    /// `podman rm -f` + recreate can. See `docker::is_wedged`.
    Wedged,
}

/// `status` is podman's raw `State.Status` (see `docker::ContainerStatus`).
/// It wins over every other signal — including a stale/misleading
/// `running` flag — because it is the one field that names a wedge
/// instead of letting it masquerade as a clean stop or a healthy server.
pub fn classify(status: Option<&str>, running: bool, health: Option<&str>, rcon_ok: bool) -> ServerState {
    if docker::is_wedged(status) {
        return ServerState::Wedged;
    }
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

/// How long a confirmed non-Up state must persist before the Damper pages
/// again: 5, 15, 30, then 60 minutes. This is what turns the Damper from
/// edge-triggered (one message, ever, per outage — the incident's exact
/// failure mode) into duration-aware: it fires again and again for as long
/// as the ínsula stays down or wedged, instead of going silent forever
/// after the first transition.
const ESCALATION_LADDER: [Duration; 4] = [
    Duration::from_secs(5 * 60),
    Duration::from_secs(15 * 60),
    Duration::from_secs(30 * 60),
    Duration::from_secs(60 * 60),
];
/// Once the ladder's last rung (60 min) has fired, keep paging at this
/// cadence forever, so a wedge that outlives a full day still gets an
/// hourly nudge instead of relapsing into silence.
const ESCALATION_REPEAT: Duration = Duration::from_secs(60 * 60);

/// How long a state must have persisted for the `escalations_sent`-th
/// escalation (0-indexed) to fire.
fn escalation_threshold(escalations_sent: usize) -> Duration {
    match ESCALATION_LADDER.get(escalations_sent) {
        Some(&d) => d,
        None => {
            let extra = (escalations_sent - ESCALATION_LADDER.len() + 1) as u32;
            *ESCALATION_LADDER.last().expect("ladder is non-empty") + ESCALATION_REPEAT * extra
        }
    }
}

/// One escalation: the ínsula has been stuck in `state` (never `Up`) for
/// at least `elapsed`, and this is the next rung of `ESCALATION_LADDER` to
/// fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Escalation {
    pub state: ServerState,
    pub elapsed: Duration,
}

/// What a single [`Damper::observe`] call produced: an optional confirmed
/// state transition, whether this reading pushed the flap-reset count over
/// the instability threshold, and an optional escalation if the current
/// (non-Up) state has just crossed the next rung of the ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DamperSignal {
    pub transition: Option<ServerState>,
    pub instability: bool,
    pub escalation: Option<Escalation>,
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
    /// Wall-clock instant `current` was last confirmed to change, seeded
    /// lazily on the first `observe()` call rather than read at
    /// construction time — same reasoning as
    /// `chronicle::PlayerSession::start` — so `Damper::new` stays a pure,
    /// clockless constructor and every timestamp still comes from an
    /// injected `now`.
    state_since: Option<Instant>,
    /// How many rungs of `ESCALATION_LADDER` (+ repeats) have already
    /// fired for the current unbroken non-Up stretch. Reset to 0 whenever
    /// `current` transitions back to `Up`.
    escalations_sent: usize,
}

impl Damper {
    pub fn new(initial: ServerState) -> Self {
        Self {
            current: initial,
            pending: None,
            flap_resets: VecDeque::new(),
            suppressed_until: None,
            state_since: None,
            escalations_sent: 0,
        }
    }

    /// Feed one poll reading. `now` drives the ordinary damper, the
    /// flap-instability window/cooldown, and the escalation ladder, and is
    /// injected (rather than read internally) so tests can drive it with a
    /// fake clock, same style as `chronicle::SessionTracker::observe`.
    pub fn observe(&mut self, s: ServerState, now: Instant) -> DamperSignal {
        // Seed state_since on the very first observation ever, so a
        // Damper constructed already-non-Up (e.g. `Damper::new(Down)` at
        // boot) still starts its escalation clock somewhere sane. Any
        // later confirmed transition overwrites this below.
        self.state_since.get_or_insert(now);

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
                    self.state_since = Some(now);
                    if s == ServerState::Up {
                        self.escalations_sent = 0;
                    }
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

        let escalation = if self.current != ServerState::Up {
            let since = self.state_since.expect("seeded above");
            let elapsed = now.saturating_duration_since(since);
            let threshold = escalation_threshold(self.escalations_sent);
            if elapsed >= threshold {
                self.escalations_sent += 1;
                Some(Escalation { state: self.current, elapsed })
            } else {
                None
            }
        } else {
            None
        };

        DamperSignal { transition, instability, escalation }
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

/// Whether a crossed half-hour milestone is worth a chronicle roast
/// message. The wheel spins on every milestone `SessionTracker::observe`
/// emits, but the chronicle keeps its old, coarser cadence: whole hours
/// only (`half_hours` even), starting at the 2h floor (`half_hours >= 4`).
/// So 0:30/1:00/1:30 spin silently, 2:00 is the first chronicle-eligible
/// one, 2:30 spins silently, 3:00 is chronicle-eligible again, etc.
fn chronicle_due(half_hours: u32) -> bool {
    half_hours % 2 == 0 && half_hours >= 4
}

/// Milestone hours to use when spinning the deploy-round wheel (see
/// `run`'s first-tick handling) for a player who's already online at
/// startup. Prefers the tracker's own record of their current session if
/// one exists, falling back to the 2h floor otherwise. In practice the
/// fallback is what always fires: `SessionTracker` state lives only in
/// memory, so a freshly restarted process never has session history for
/// anyone yet — this function exists so that stays true by construction
/// rather than by accident if that ever changes.
fn deploy_round_hours(current_session_hours: Option<u32>) -> u32 {
    current_session_hours.unwrap_or(2)
}

/// Spin the wheel for `player` at `hours` and carry out the result: RCON
/// `say` + effect commands, then narrate in the notify channel. Shared by
/// the ordinary per-milestone spin and the deploy-round catch-up spin so
/// both go through identical execution. RCON failures here are logged and
/// swallowed, never propagated — a hiccup spinning the wheel must never
/// take down the monitor loop. No-ops for names `fortuna::valid_player_name`
/// rejects.
async fn spin_wheel_for(
    channel: ChannelId,
    http: &Http,
    rcon: &Arc<Mutex<McRcon>>,
    fortune_rng: &mut fortuna::EntropyRolls,
    player: &str,
    hours: u32,
) {
    if !fortuna::valid_player_name(player) {
        return;
    }
    let spin = fortuna::spin(player, hours, fortune_rng);
    tracing::info!(
        "la rueda de la fortuna: {} @ {}h -> {} ({:?})",
        player,
        hours,
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
    let _ = channel.say(http, spin.discord_msg).await;
}

/// 30 s loop. Owns its own damper and watch; sends notifications to the
/// configured channel. Spawned from main after the Discord client is
/// ready. `announce_deploy_round` mirrors the changelog decision main.rs
/// already made (`changelog::should_announce`): when true, this is a
/// genuinely new build, and the very first tick spins the wheel once for
/// every player already online so a deploy never leaves the ínsula
/// waiting for its next milestone. It stays false — and the deploy round
/// stays silent — on ordinary restarts of the same build.
pub async fn run(
    http: Arc<Http>,
    cfg: Config,
    docker: DockerCtl,
    rcon: Arc<Mutex<McRcon>>,
    announce_deploy_round: bool,
) {
    let channel = ChannelId::new(cfg.notify_channel_id);
    let mut damper = Damper::new(ServerState::Down);
    let mut backups = BackupWatch::default();
    let mut sessions = SessionTracker::default();
    let mut herald = Herald::new();
    let mut fortune_rng = fortuna::EntropyRolls::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
    let mut first_tick = true;
    loop {
        tick.tick().await;

        let mc = docker.inspect("mc").await.ok().flatten();
        let list_result = { rcon.lock().await.cmd("list").await };
        let rcon_ok = list_result.is_ok();
        let status = mc.as_ref().and_then(|s| s.status.clone());
        let state = classify(
            status.as_deref(),
            mc.as_ref().map(|s| s.running).unwrap_or(false),
            mc.as_ref().and_then(|s| s.health.as_deref()),
            rcon_ok,
        );
        let signal = damper.observe(state, Instant::now());
        if let Some(t) = signal.transition {
            let msg: String = match t {
                ServerState::Up => ":green_circle: ¡En pie está la ínsula! Presta para recibir a sus caballeros.".into(),
                ServerState::Starting => ":yellow_circle: La ínsula despereza sus engranajes; aguarde vuestra merced, que ya despierta.".into(),
                ServerState::Unhealthy => ":orange_circle: La ínsula sufre de melancolía en sus engranajes (el tick loop flaquea). Considere vuestra merced un /restart.".into(),
                ServerState::Down => ":red_circle: ¡Ha caído la ínsula! Los follones y malandrines han triunfado... por ahora.".into(),
                ServerState::Wedged => format!(
                    ":skull: ¡La ínsula ha quedado TRABADA! Podman la reporta en el estado '{}': ni /start ni /restart pueden ya desatascarla — es un mal encantamiento en sus entrañas, no un reposo. El vigía del castillo (host) la reconstruirá en pocos minutos.",
                    status.as_deref().unwrap_or("desconocido")
                ),
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
        if let Some(esc) = signal.escalation {
            let mins = esc.elapsed.as_secs() / 60;
            let role_ping = format!("<@&{}>", cfg.admin_role_id);
            let body = match esc.state {
                ServerState::Wedged => format!(
                    "{role_ping} :rotating_light: La ínsula sigue TRABADA hace {mins} minutos (podman: estado '{}'). Ni /start ni /restart la remedian; el vigía del castillo obrará, mas acuda vuestra merced si tarda.",
                    status.as_deref().unwrap_or("desconocido")
                ),
                _ => format!(
                    "{role_ping} :rotating_light: La ínsula lleva {mins} minutos sin levantarse ({esc_state:?}). Acuda alguien del consejo a mirar los logs; este hidalgo no callará mientras dure la caída.",
                    esc_state = esc.state
                ),
            };
            let reply = CreateMessage::new()
                .content(body)
                .allowed_mentions(CreateAllowedMentions::new().roles(vec![RoleId::new(cfg.admin_role_id)]));
            if let Err(e) = channel.send_message(&http, reply).await {
                tracing::warn!("failed to post escalation: {e:#}");
            }
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
        // tracker, which now surfaces one milestone per crossed HALF hour
        // (0:30, 1:00, 1:30, ...). The wheel (La Rueda de la Fortuna) spins
        // on every one of those; Don Quijote's chronicle roast only fires
        // on the whole-hour milestones from 2h onward, per `chronicle_due`.
        // An unparseable or failed `list` reads as nobody present, which is
        // fine: SessionTracker tolerates absences up to its 5-minute grace
        // window before ending a session.
        let present = list_result
            .as_deref()
            .ok()
            .and_then(parse::parse_list)
            .map(|p| p.names)
            .unwrap_or_default();

        // Deploy round: only on this process's very first tick, and only
        // when main.rs already decided this build is genuinely new (never
        // on a same-build crash/patch restart). Spins once for every
        // player already online so a fresh deploy doesn't leave anyone
        // waiting out a fresh half hour before the wheel notices them.
        if first_tick && announce_deploy_round && !present.is_empty() {
            let _ = channel
                .say(
                    &http,
                    ":ferris_wheel: Nueva singladura del código, y la Rueda quiere estrenarla: gira una vez por cada caballero presente.",
                )
                .await;
        }

        let anns = sessions.observe(&present, Instant::now());

        if first_tick && announce_deploy_round && !present.is_empty() {
            for player in &present {
                let hours = deploy_round_hours(sessions.current_hours(player, Instant::now()));
                spin_wheel_for(channel, &http, &rcon, &mut fortune_rng, player, hours).await;
            }
        }
        first_tick = false;

        for ann in anns {
            if chronicle_due(ann.half_hours) {
                let _ = channel.say(&http, chronicle::session_message(&ann.player, ann.hours)).await;
            }

            // La Rueda de la Fortuna: spin for EVERY milestone (each half
            // hour of continuous session), and turn the result into real
            // RCON consequences.
            spin_wheel_for(channel, &http, &rcon, &mut fortune_rng, &ann.player, ann.hours).await;
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
    fn chronicle_due_only_whole_hours_from_two_up() {
        // Half-hour milestones (odd, or below the 2h floor) never fire the
        // chronicle, only the wheel.
        for hh in [0u32, 1, 2, 3] {
            assert!(!chronicle_due(hh), "half_hours={hh} should not be chronicle-due");
        }
        // 2:00, 2:30 (wheel-only), 3:00, 3:30 (wheel-only), 4:00...
        assert!(chronicle_due(4)); // 2:00
        assert!(!chronicle_due(5)); // 2:30
        assert!(chronicle_due(6)); // 3:00
        assert!(!chronicle_due(7)); // 3:30
        assert!(chronicle_due(8)); // 4:00
    }

    #[test]
    fn deploy_round_hours_uses_session_when_known_else_floor() {
        assert_eq!(deploy_round_hours(Some(5)), 5);
        assert_eq!(deploy_round_hours(Some(0)), 0);
        assert_eq!(deploy_round_hours(None), 2);
    }

    #[test]
    fn classify_maps_observations() {
        assert_eq!(classify(None, false, None, false), ServerState::Down);
        assert_eq!(classify(None, true, Some("starting"), false), ServerState::Starting);
        assert_eq!(classify(None, true, Some("healthy"), true), ServerState::Up);
        assert_eq!(classify(None, true, Some("unhealthy"), true), ServerState::Unhealthy);
        // running, no healthcheck info, rcon answers: that's up
        assert_eq!(classify(None, true, None, true), ServerState::Up);
        // running but rcon dead and no health: still starting, not down
        assert_eq!(classify(None, true, None, false), ServerState::Starting);
    }

    #[test]
    fn wedged_wins_over_running_flag() {
        // podman's own status pins the container as stuck mid-lifecycle
        // even when other signals disagree — including a `running: true`
        // that can briefly coexist with `status: "stopping"`.
        assert_eq!(classify(Some("stopping"), false, Some("healthy"), false), ServerState::Wedged);
        assert_eq!(classify(Some("stopping"), true, Some("healthy"), true), ServerState::Wedged);
        assert_eq!(classify(Some("removing"), true, None, true), ServerState::Wedged);
        assert_eq!(classify(Some("dead"), false, None, false), ServerState::Wedged);
        assert_eq!(classify(Some("paused"), true, Some("healthy"), true), ServerState::Wedged);
    }

    #[test]
    fn classify_unknown_status_falls_through_to_old_behavior() {
        // Any status that isn't one of the wedge values must not change
        // the pre-existing classification logic at all.
        assert_eq!(classify(Some("running"), true, Some("healthy"), true), ServerState::Up);
        assert_eq!(classify(Some("restarting"), false, None, false), ServerState::Down);
        assert_eq!(classify(Some("created"), false, None, false), ServerState::Down);
        assert_eq!(classify(None, true, Some("starting"), false), ServerState::Starting);
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
    fn no_escalation_while_up() {
        let mut d = Damper::new(ServerState::Up);
        let t0 = Instant::now();
        for i in 0..20u64 {
            let sig = d.observe(ServerState::Up, t0 + Duration::from_secs(i * 600));
            assert_eq!(sig.escalation, None, "Up must never escalate, tick {i}");
        }
    }

    #[test]
    fn escalates_after_five_minutes_down() {
        let mut d = Damper::new(ServerState::Up);
        let t0 = Instant::now();
        // Confirm the transition to Down (two consecutive readings).
        assert_eq!(d.observe(ServerState::Down, t0).transition, None);
        let confirmed = d.observe(ServerState::Down, t0 + Duration::from_secs(30));
        assert_eq!(confirmed.transition, Some(ServerState::Down));
        assert_eq!(confirmed.escalation, None, "no escalation on the transition tick itself");

        let just_under = t0 + Duration::from_secs(30) + Duration::from_secs(299);
        assert_eq!(d.observe(ServerState::Down, just_under).escalation, None);

        let at_five_minutes = t0 + Duration::from_secs(30) + Duration::from_secs(300);
        let sig = d.observe(ServerState::Down, at_five_minutes);
        assert_eq!(
            sig.escalation,
            Some(Escalation { state: ServerState::Down, elapsed: Duration::from_secs(300) })
        );
    }

    #[test]
    fn escalation_ladder_backs_off_5_15_30_60_then_hourly() {
        let mut d = Damper::new(ServerState::Down);
        let t0 = Instant::now();
        // Damper starts already-Down (as monitor::run constructs it);
        // this first observe seeds state_since at t0.
        assert_eq!(d.observe(ServerState::Down, t0).transition, None);

        let ladder_minutes = [5u64, 15, 30, 60, 120, 180, 240];
        for m in ladder_minutes {
            let sig = d.observe(ServerState::Down, t0 + Duration::from_secs(m * 60));
            assert_eq!(
                sig.escalation.map(|e| e.elapsed),
                Some(Duration::from_secs(m * 60)),
                "expected an escalation exactly at minute {m}"
            );
        }
    }

    #[test]
    fn escalation_counter_resets_after_recovery() {
        let mut d = Damper::new(ServerState::Up);
        let t0 = Instant::now();
        d.observe(ServerState::Down, t0);
        d.observe(ServerState::Down, t0 + Duration::from_secs(30));
        let sig = d.observe(ServerState::Down, t0 + Duration::from_secs(330));
        assert!(sig.escalation.is_some(), "first rung should have fired");

        // Recover: two confirmed Up readings clear escalations_sent.
        d.observe(ServerState::Up, t0 + Duration::from_secs(400));
        let recovered = d.observe(ServerState::Up, t0 + Duration::from_secs(430));
        assert_eq!(recovered.transition, Some(ServerState::Up));

        // Go down again: the ladder must restart at 5 minutes from THIS
        // transition, not continue from wherever the old counter left off.
        let t1 = t0 + Duration::from_secs(1000);
        d.observe(ServerState::Down, t1);
        let confirmed = d.observe(ServerState::Down, t1 + Duration::from_secs(30));
        assert_eq!(confirmed.transition, Some(ServerState::Down));

        let just_under = t1 + Duration::from_secs(30) + Duration::from_secs(299);
        assert_eq!(d.observe(ServerState::Down, just_under).escalation, None);

        let at_five_minutes = t1 + Duration::from_secs(30) + Duration::from_secs(300);
        assert!(d.observe(ServerState::Down, at_five_minutes).escalation.is_some());
    }

    #[test]
    fn wedged_and_down_both_escalate() {
        let mut d = Damper::new(ServerState::Wedged);
        let t0 = Instant::now();
        d.observe(ServerState::Wedged, t0); // seeds state_since
        let sig = d.observe(ServerState::Wedged, t0 + Duration::from_secs(300));
        assert_eq!(
            sig.escalation,
            Some(Escalation { state: ServerState::Wedged, elapsed: Duration::from_secs(300) })
        );
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
