use anyhow::{bail, Context as _};

#[derive(Clone, Debug)]
pub struct Config {
    pub discord_token: String,
    pub guild_id: u64,
    pub admin_role_id: u64,
    /// Role allowed to run `/whitelist remove`. When unset, that command
    /// falls back to `admin_role_id` like every other admin command.
    pub remover_role_id: Option<u64>,
    pub notify_channel_id: u64,
    pub docker_api_url: String,
    pub rcon_addr: String,
    pub rcon_password: String,
    pub server_address: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let req = |k: &str| get(k).with_context(|| format!("missing env var {k}"));
        let id = |k: &str| -> anyhow::Result<u64> {
            req(k)?.parse().with_context(|| format!("{k} must be a numeric Discord id"))
        };
        // Missing or set-but-empty -> None; present and non-numeric is
        // still a named error, same as the required ids above.
        let opt_id = |k: &str| -> anyhow::Result<Option<u64>> {
            match get(k) {
                None => Ok(None),
                Some(v) if v.trim().is_empty() => Ok(None),
                Some(v) => {
                    v.parse().map(Some).with_context(|| format!("{k} must be a numeric Discord id"))
                }
            }
        };
        Ok(Self {
            discord_token: req("DISCORD_TOKEN")?,
            guild_id: id("DISCORD_GUILD_ID")?,
            admin_role_id: id("DISCORD_ADMIN_ROLE_ID")?,
            remover_role_id: opt_id("DISCORD_REMOVER_ROLE_ID")?,
            notify_channel_id: id("DISCORD_NOTIFY_CHANNEL_ID")?,
            docker_api_url: get("DOCKER_API_URL").unwrap_or_else(|| "http://socket-proxy:2375".into()),
            rcon_addr: get("RCON_ADDR").unwrap_or_else(|| "mc:25575".into()),
            rcon_password: req("RCON_PASSWORD")?,
            server_address: get("SERVER_ADDRESS").unwrap_or_else(|| "unknown".into()),
        })
    }

    pub fn rcon_host_port(&self) -> anyhow::Result<(String, u16)> {
        match self.rcon_addr.rsplit_once(':') {
            Some((h, p)) => Ok((h.to_string(), p.parse().context("RCON_ADDR port")?)),
            None => bail!("RCON_ADDR must be host:port"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        let m: HashMap<&str, &str> = pairs.iter().copied().collect();
        move |k| m.get(k).map(|v| v.to_string())
    }

    #[test]
    fn parses_complete_env() {
        let cfg = Config::from_lookup(env(&[
            ("DISCORD_TOKEN", "t"),
            ("DISCORD_GUILD_ID", "123"),
            ("DISCORD_ADMIN_ROLE_ID", "456"),
            ("DISCORD_NOTIFY_CHANNEL_ID", "789"),
            ("DOCKER_API_URL", "http://socket-proxy:2375"),
            ("RCON_ADDR", "mc:25575"),
            ("SERVER_ADDRESS", "mc.hijuepapus.pro"),
            ("RCON_PASSWORD", "pw"),
        ]))
        .unwrap();
        assert_eq!(cfg.guild_id, 123);
        assert_eq!(cfg.rcon_host_port().unwrap(), ("mc".to_string(), 25575));
    }

    #[test]
    fn missing_var_is_a_named_error() {
        let err = Config::from_lookup(env(&[])).unwrap_err();
        assert!(err.to_string().contains("DISCORD_TOKEN"));
    }

    #[test]
    fn bad_id_is_a_named_error() {
        let err = Config::from_lookup(env(&[
            ("DISCORD_TOKEN", "t"),
            ("DISCORD_GUILD_ID", "notanumber"),
        ]))
        .unwrap_err();
        assert!(err.to_string().contains("DISCORD_GUILD_ID"));
    }

    #[test]
    fn remover_role_id_is_none_when_absent() {
        let cfg = Config::from_lookup(env(&[
            ("DISCORD_TOKEN", "t"),
            ("DISCORD_GUILD_ID", "123"),
            ("DISCORD_ADMIN_ROLE_ID", "456"),
            ("DISCORD_NOTIFY_CHANNEL_ID", "789"),
            ("RCON_PASSWORD", "pw"),
        ]))
        .unwrap();
        assert_eq!(cfg.remover_role_id, None);
    }

    #[test]
    fn remover_role_id_parses_when_present() {
        let cfg = Config::from_lookup(env(&[
            ("DISCORD_TOKEN", "t"),
            ("DISCORD_GUILD_ID", "123"),
            ("DISCORD_ADMIN_ROLE_ID", "456"),
            ("DISCORD_NOTIFY_CHANNEL_ID", "789"),
            ("RCON_PASSWORD", "pw"),
            ("DISCORD_REMOVER_ROLE_ID", "999"),
        ]))
        .unwrap();
        assert_eq!(cfg.remover_role_id, Some(999));
    }

    #[test]
    fn remover_role_id_bad_value_is_a_named_error() {
        let err = Config::from_lookup(env(&[
            ("DISCORD_TOKEN", "t"),
            ("DISCORD_GUILD_ID", "123"),
            ("DISCORD_ADMIN_ROLE_ID", "456"),
            ("DISCORD_NOTIFY_CHANNEL_ID", "789"),
            ("RCON_PASSWORD", "pw"),
            ("DISCORD_REMOVER_ROLE_ID", "notanumber"),
        ]))
        .unwrap_err();
        assert!(err.to_string().contains("DISCORD_REMOVER_ROLE_ID"));
    }
}
