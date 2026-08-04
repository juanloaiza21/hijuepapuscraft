use anyhow::{bail, Context as _};
use bollard::Docker;
use std::time::{Duration, Instant};

pub const ALLOWED: [&str; 2] = ["mc", "mc-backup"];

pub fn guard(name: &str) -> anyhow::Result<()> {
    if ALLOWED.contains(&name) {
        Ok(())
    } else {
        bail!("container {name:?} is not managed by this bot")
    }
}

#[derive(Debug, PartialEq)]
pub enum StartOutcome {
    Started,
    AlreadyRunning,
}

#[derive(Debug, Clone)]
pub struct ContainerStatus {
    pub running: bool,
    pub health: Option<String>,
    pub started_at: Option<String>,
    pub exit_code: Option<i64>,
    /// Raw `State.Status` from podman's docker-compat inspect: "created",
    /// "running", "paused", "restarting", "removing", "exited", "dead", or
    /// podman 4.9's "stopping" (a libpod-only value the upstream docker API
    /// never emits, but which podman's compat handler passes through
    /// verbatim — see `is_wedged`). This is the one field that lets the
    /// bot tell a deliberate stop from a container wedged mid-lifecycle.
    pub status: Option<String>,
}

/// Statuses podman's docker-compat inspect can report for a container that
/// is stuck mid-lifecycle rather than cleanly stopped or running: podman
/// 4.9.3 pins a decapitated container (conmon dead, no exit file) in
/// "stopping" forever, and "removing"/"dead"/"paused" are the other states
/// from which neither `/start` nor `/restart` can recover the container —
/// only a host-side `podman rm -f` + recreate can.
pub fn is_wedged(status: Option<&str>) -> bool {
    matches!(status, Some("stopping") | Some("removing") | Some("dead") | Some("paused"))
}

/// Renders a bollard error for a human, keeping podman's own explanation
/// instead of discarding it. `DockerResponseServerError` is podman's own
/// compat-API response body (e.g. "container ... must be in Created or
/// Stopped state to be started: container state improper"); everything
/// else falls back to the error's own Display. Pure and unit-testable
/// without a running server.
fn describe(op: &str, e: &bollard::errors::Error) -> String {
    match e {
        bollard::errors::Error::DockerResponseServerError { status_code, message } => {
            format!("{op} failed (HTTP {status_code}): {message}")
        }
        other => format!("{op} failed: {other}"),
    }
}

#[derive(Clone)]
pub struct DockerCtl {
    docker: Docker,
    /// Second handle, identical to `docker` except for a 180s read/write
    /// timeout instead of 30s. `stop`/`restart_via_stop_start` are the
    /// only callers: a `--stop-timeout 120` container can legitimately
    /// take just under two minutes to answer a stop, and the 30s handle
    /// would report failure while the stop was still proceeding. Scoped
    /// deliberately narrow — if a hung socket-proxy ever stalls this
    /// handle, only lifecycle commands stall, not the 30s monitor loop.
    slow: Docker,
}

impl DockerCtl {
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        // Podman 4.9 compat API tops out around 1.41; bollard's default is
        // newer, so negotiate down or every call 400s.
        let docker = Docker::connect_with_http(url, 30, bollard::API_DEFAULT_VERSION)
            .context("bad DOCKER_API_URL")?
            .negotiate_version()
            .await
            .context("API version negotiation against socket-proxy failed")?;
        let slow = Docker::connect_with_http(url, 180, &docker.client_version())
            .context("bad DOCKER_API_URL (slow handle)")?;
        Ok(Self { docker, slow })
    }

    pub async fn start(&self, name: &str) -> anyhow::Result<StartOutcome> {
        guard(name)?;
        match self.docker.start_container(name, None).await {
            Ok(()) => Ok(StartOutcome::Started),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => Ok(StartOutcome::AlreadyRunning),
            Err(e) => Err(anyhow::anyhow!(describe("start", &e))),
        }
    }

    /// Stops the container on the 180s handle. A `stopping` result after
    /// this returns Ok is a real possibility (podman 4.9's absorbing
    /// state on a decapitated container) — callers that care about the
    /// distinction must poll `inspect` afterward; this method only
    /// reports whether the stop *request* succeeded.
    pub async fn stop(&self, name: &str) -> anyhow::Result<()> {
        guard(name)?;
        self.slow
            .stop_container(name, None)
            .await
            .map_err(|e| anyhow::anyhow!(describe("stop", &e)))
    }

    /// Restarts by stopping and waiting for the container to actually
    /// settle into `exited`/`created` before starting it again, instead of
    /// calling podman's compat `/restart` endpoint. That endpoint is the
    /// primitive stage 2 of the wedge incident rode in on: `restartWithTimeout`
    /// only re-inits from a settled state, so restarting a container that
    /// hasn't finished stopping calls `crun start` on a half-torn-down
    /// payload and wedges it. Bails with the observed status (never blindly
    /// starts on top of an unsettled state) if it does not settle within
    /// 150s.
    pub async fn restart_via_stop_start(&self, name: &str) -> anyhow::Result<()> {
        guard(name)?;
        self.slow
            .stop_container(name, None)
            .await
            .map_err(|e| anyhow::anyhow!(describe("restart", &e)))?;

        let deadline = Instant::now() + Duration::from_secs(150);
        loop {
            let status = self.inspect(name).await?.and_then(|s| s.status);
            if matches!(status.as_deref(), Some("exited") | Some("created")) {
                break;
            }
            if Instant::now() >= deadline {
                bail!(
                    "restart failed: container did not settle within 150s after stop (status: {})",
                    status.as_deref().unwrap_or("unknown")
                );
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }

        self.start(name).await.map(|_| ())
    }

    pub async fn inspect(&self, name: &str) -> anyhow::Result<Option<ContainerStatus>> {
        guard(name)?;
        match self.docker.inspect_container(name, None).await {
            Ok(c) => {
                let state = c.state.unwrap_or_default();
                Ok(Some(ContainerStatus {
                    running: state.running.unwrap_or(false),
                    health: state.health.and_then(|h| h.status).map(|s| s.to_string()),
                    started_at: state.started_at,
                    exit_code: state.exit_code,
                    status: state.status.map(|s| s.to_string()),
                }))
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(None),
            Err(e) => Err(anyhow::anyhow!(describe("inspect", &e))),
        }
    }

    pub async fn stats_mem(&self, name: &str) -> anyhow::Result<Option<(u64, u64)>> {
        guard(name)?;
        use bollard::query_parameters::StatsOptions;
        use futures_util::StreamExt as _;
        let mut s = self.docker.stats(
            name,
            Some(StatsOptions { stream: false, one_shot: true }),
        );
        match s.next().await {
            Some(Ok(st)) => {
                let mem = st.memory_stats.unwrap_or_default();
                Ok(mem.usage.zip(mem.limit))
            }
            _ => Ok(None),
        }
    }

    pub async fn logs_tail(&self, name: &str, lines: usize) -> anyhow::Result<String> {
        guard(name)?;
        use bollard::query_parameters::LogsOptions;
        use futures_util::StreamExt as _;
        let mut out = String::new();
        let mut stream = self.docker.logs(
            name,
            Some(LogsOptions {
                stdout: true,
                stderr: true,
                tail: lines.to_string(),
                ..Default::default()
            }),
        );
        while let Some(Ok(chunk)) = stream.next().await {
            out.push_str(&chunk.to_string());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_allows_only_the_two_managed_containers() {
        assert!(guard("mc").is_ok());
        assert!(guard("mc-backup").is_ok());
        assert!(guard("bot").is_err());
        assert!(guard("socket-proxy").is_err());
        assert!(guard("mc; rm -rf /").is_err());
    }

    #[test]
    fn is_wedged_covers_stopping_removing_dead_paused() {
        for s in ["stopping", "removing", "dead", "paused"] {
            assert!(is_wedged(Some(s)), "{s} should be wedged");
        }
        for s in ["running", "exited", "created", "restarting"] {
            assert!(!is_wedged(Some(s)), "{s} should not be wedged");
        }
        assert!(!is_wedged(None));
    }

    #[test]
    fn describe_keeps_podman_message() {
        let e = bollard::errors::Error::DockerResponseServerError {
            status_code: 500,
            message: "container ... must be in Created or Stopped state to be started: container state improper".to_string(),
        };
        let out = describe("start", &e);
        assert!(
            out.contains(
                "container ... must be in Created or Stopped state to be started: container state improper"
            ),
            "podman's message was dropped: {out}"
        );
        assert!(out.contains("500"), "status code was dropped: {out}");
    }

    #[test]
    fn describe_labels_the_operation() {
        let e = bollard::errors::Error::DockerResponseServerError {
            status_code: 409,
            message: "conflict".to_string(),
        };
        assert!(describe("stop", &e).starts_with("stop failed"));
        assert!(describe("restart", &e).starts_with("restart failed"));
        assert!(describe("start", &e).starts_with("start failed"));
    }
}
