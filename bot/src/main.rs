mod chronicle;
mod commands;
mod config;
mod docker;
mod heraldo;
mod monitor;
mod parse;
mod rcon;

use commands::Data;
use config::Config;
use poise::serenity_prelude as serenity;
use std::sync::Arc;
use tokio::signal::unix::{signal, SignalKind};
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
                // Spawn the monitor before registering commands so status
                // alerting keeps working even if registration below fails.
                tokio::spawn(monitor::run(
                    ctx.http.clone(),
                    monitor_cfg,
                    monitor_docker,
                    monitor_rcon,
                ));
                if let Err(e) = poise::builtins::register_in_guild(
                    ctx,
                    &framework.options().commands,
                    guild,
                )
                .await
                {
                    // Deliberate: propagating this error would leave
                    // Framework::user_data() awaiting forever with no
                    // user_data ever set, so every command hangs while the
                    // process itself looks alive to systemd. Exiting makes
                    // the unit's Restart=always relaunch and retry a
                    // transient Discord failure instead of zombifying.
                    tracing::error!("slash command registration failed: {e:#}");
                    std::process::exit(1);
                }
                tracing::info!("commands registered, monitor running");
                Ok(Data { cfg, rcon, docker })
            })
        })
        .build();

    let mut client = serenity::ClientBuilder::new(&token, intents)
        .framework(framework)
        .await?;

    // We're PID1 in the container; podman sends SIGTERM on stop and waits
    // 10s before SIGKILL. Without a handler the async runtime never sees
    // the signal, so every restart used to burn the full timeout and exit
    // 137. Race the client against both signals and exit 0 promptly.
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    tokio::select! {
        result = client.start() => result?,
        _ = sigterm.recv() => {
            tracing::info!("SIGTERM recibido; la ínsula se despide y cierra sus puertas con dignidad.");
        }
        _ = sigint.recv() => {
            tracing::info!("SIGINT recibido; la ínsula se despide y cierra sus puertas con dignidad.");
        }
    }
    Ok(())
}
