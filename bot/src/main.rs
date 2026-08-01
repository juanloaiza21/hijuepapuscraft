mod commands;
mod config;
mod docker;
mod monitor;
mod parse;
mod rcon;

use commands::Data;
use config::Config;
use poise::serenity_prelude as serenity;
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = Config::from_env()?;
    let (rcon_host, rcon_port) = cfg.rcon_host_port()?;

    // The proxy may come up after us; retry instead of crash-looping fast.
    let docker = {
        let mut attempt = 0u32;
        loop {
            match docker::DockerCtl::connect(&cfg.docker_api_url).await {
                Ok(d) => break d,
                Err(e) if attempt < 10 => {
                    attempt += 1;
                    tracing::warn!("docker API not ready ({e:#}), retry {attempt}/10");
                    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
                }
                Err(e) => return Err(e),
            }
        }
    };

    let rcon = Arc::new(Mutex::new(rcon::McRcon::new(
        rcon_host,
        rcon_port,
        cfg.rcon_password.clone(),
    )));

    let intents = serenity::GatewayIntents::non_privileged();
    let guild = serenity::GuildId::new(cfg.guild_id);
    let monitor_cfg = cfg.clone();
    let monitor_docker = docker.clone();
    let monitor_rcon = rcon.clone();
    // Grab the token before `cfg` moves into the setup closure below;
    // avoids re-reading env vars a second time to get it back out.
    let token = cfg.discord_token.clone();

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: commands::commands(),
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_in_guild(
                    ctx,
                    &framework.options().commands,
                    guild,
                )
                .await?;
                tokio::spawn(monitor::run(
                    ctx.http.clone(),
                    monitor_cfg,
                    monitor_docker,
                    monitor_rcon,
                ));
                tracing::info!("commands registered, monitor running");
                Ok(Data { cfg, rcon, docker })
            })
        })
        .build();

    let mut client = serenity::ClientBuilder::new(&token, intents)
        .framework(framework)
        .await?;
    client.start().await?;
    Ok(())
}
