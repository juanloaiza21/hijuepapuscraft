use crate::config::Config;
use crate::docker::DockerCtl;
use crate::rcon::McRcon;
use poise::serenity_prelude::{ChannelId, Http};
use std::sync::Arc;
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

pub struct Damper {
    current: ServerState,
    pending: Option<(ServerState, u8)>,
}

impl Damper {
    pub fn new(initial: ServerState) -> Self {
        Self { current: initial, pending: None }
    }

    pub fn observe(&mut self, s: ServerState) -> Option<ServerState> {
        if s == self.current {
            self.pending = None;
            return None;
        }
        match self.pending {
            Some((p, n)) if p == s && n + 1 >= 2 => {
                self.current = s;
                self.pending = None;
                Some(s)
            }
            Some((p, n)) if p == s => {
                self.pending = Some((p, n + 1));
                None
            }
            _ => {
                self.pending = Some((s, 1));
                None
            }
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
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
    loop {
        tick.tick().await;

        let mc = docker.inspect("mc").await.ok().flatten();
        let rcon_ok = { rcon.lock().await.cmd("list").await.is_ok() };
        let state = classify(
            mc.as_ref().map(|s| s.running).unwrap_or(false),
            mc.as_ref().and_then(|s| s.health.as_deref()),
            rcon_ok,
        );
        if let Some(t) = damper.observe(state) {
            let msg = match t {
                ServerState::Up => ":green_circle: Server is up",
                ServerState::Starting => ":yellow_circle: Server is starting",
                ServerState::Unhealthy => ":orange_circle: Server is unhealthy (tick loop struggling), consider /restart",
                ServerState::Down => ":red_circle: Server is DOWN",
            };
            let _ = channel.say(&http, msg).await;
        }

        if let Ok(Some(b)) = docker.inspect("mc-backup").await {
            match backups.observe(b.running, b.exit_code) {
                Some(BackupEvent::Failed(c)) => {
                    let _ = channel
                        .say(&http, format!(":rotating_light: Backup FAILED with exit code {c}"))
                        .await;
                }
                Some(BackupEvent::Succeeded) => {
                    tracing::info!("backup finished ok");
                }
                None => {}
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
        assert_eq!(d.observe(ServerState::Down), None); // first sighting
        assert_eq!(d.observe(ServerState::Down), Some(ServerState::Down)); // confirmed
        assert_eq!(d.observe(ServerState::Down), None); // no repeat notifications
    }

    #[test]
    fn damper_resets_on_flap() {
        let mut d = Damper::new(ServerState::Up);
        assert_eq!(d.observe(ServerState::Down), None);
        assert_eq!(d.observe(ServerState::Up), None); // flap, no transition
        assert_eq!(d.observe(ServerState::Down), None); // counting restarts
        assert_eq!(d.observe(ServerState::Down), Some(ServerState::Down));
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
