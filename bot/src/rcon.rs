use anyhow::Context as _;
use mc_query::rcon::RconClient;

pub struct McRcon {
    host: String,
    port: u16,
    password: String,
    conn: Option<RconClient>,
}

impl McRcon {
    pub fn new(host: String, port: u16, password: String) -> Self {
        Self { host, port, password, conn: None }
    }

    async fn ensure(&mut self) -> anyhow::Result<()> {
        if self.conn.is_none() {
            let mut c = RconClient::new(&self.host, self.port)
                .await
                .context("rcon connect")?;
            c.authenticate(&self.password).await.context("rcon auth")?;
            self.conn = Some(c);
        }
        Ok(())
    }

    /// One transparent reconnect: a dead persistent connection (server
    /// restarted) looks identical to a down server on the first error.
    pub async fn cmd(&mut self, cmd: &str) -> anyhow::Result<String> {
        self.ensure().await?;
        match self.conn.as_mut().unwrap().run_command(cmd).await {
            Ok(out) => Ok(out),
            Err(_) => {
                self.conn = None;
                self.ensure().await?;
                self.conn
                    .as_mut()
                    .unwrap()
                    .run_command(cmd)
                    .await
                    .context("rcon command failed after reconnect")
            }
        }
    }
}
