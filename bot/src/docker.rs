use anyhow::{bail, Context as _};
use bollard::Docker;

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
}

#[derive(Clone)]
pub struct DockerCtl {
    docker: Docker,
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
        Ok(Self { docker })
    }

    pub async fn start(&self, name: &str) -> anyhow::Result<StartOutcome> {
        guard(name)?;
        match self.docker.start_container(name, None).await {
            Ok(()) => Ok(StartOutcome::Started),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => Ok(StartOutcome::AlreadyRunning),
            Err(e) => Err(e).context("start failed"),
        }
    }

    pub async fn stop(&self, name: &str) -> anyhow::Result<()> {
        guard(name)?;
        self.docker.stop_container(name, None).await.context("stop failed")
    }

    pub async fn restart(&self, name: &str) -> anyhow::Result<()> {
        guard(name)?;
        self.docker.restart_container(name, None).await.context("restart failed")
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
                }))
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(None),
            Err(e) => Err(e).context("inspect failed"),
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
}
