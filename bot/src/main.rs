mod changelog;
mod chronicle;
mod commands;
mod config;
mod docker;
mod fortuna;
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

/// Directory holding the "last announced build" marker file. Overridable
/// via `BOT_STATE_DIR` (tests use a tempdir); defaults to `/data`, backed
/// in production by the `bot-state` podman volume (see
/// `containers/quadlet/bot.container`).
fn state_dir() -> String {
    std::env::var("BOT_STATE_DIR").unwrap_or_else(|_| "/data".to_string())
}

/// Reads the last-announced build sha from the state file, tolerating a
/// missing file (first-ever run) as "nothing announced yet". IO failures
/// are logged and swallowed — a hiccup here must never block or fail
/// startup, and the caller treats them the same as "nothing announced".
fn read_last_announced_build() -> Option<String> {
    let dir = state_dir();
    let state_path = std::path::Path::new(&dir).join("last_announced_build");
    match std::fs::read_to_string(&state_path) {
        Ok(s) => Some(s.trim().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!("changelog: failed to read state file {}: {e:#}", state_path.display());
            None
        }
    }
}

/// Reads the last-announced build sha, and if the currently running build
/// is new, posts a Quijote-voiced changelog to the notify channel and
/// persists the new sha so the next restart of *this* build stays quiet.
///
/// All file IO failures are logged and swallowed — a hiccup here must
/// never block or fail startup.
async fn announce_changelog_if_new(http: &Arc<serenity::Http>, cfg: &Config) {
    let dir = state_dir();
    let state_path = std::path::Path::new(&dir).join("last_announced_build");
    let current_sha = env!("GIT_SHA");
    let last_announced = read_last_announced_build();

    if !changelog::should_announce(current_sha, last_announced.as_deref()) {
        tracing::info!("changelog: build {current_sha} already announced, staying quiet");
        return;
    }

    let build = changelog::parse_build(current_sha, env!("GIT_LOG"));
    let msg = changelog::render(&build, last_announced.as_deref());
    let channel = serenity::ChannelId::new(cfg.notify_channel_id);
    if let Err(e) = channel.say(http, msg).await {
        tracing::warn!("changelog: failed to post changelog announcement: {e:#}");
    }

    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("changelog: failed to create state dir {dir}: {e:#}");
    }
    match std::fs::write(&state_path, current_sha) {
        Ok(()) => tracing::info!("changelog: announced build {current_sha}"),
        Err(e) => tracing::warn!(
            "changelog: failed to persist last announced build to {}: {e:#}",
            state_path.display()
        ),
    }
}

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
    // Same "is this a genuinely new build" decision `announce_changelog_if_new`
    // makes below (and will re-derive independently there, before writing
    // the state file — no race, since nothing writes it in between); computed
    // here too so `monitor::run`'s deploy-round wheel spin can key off it
    // without threading the changelog module's side effects through it.
    let announce_deploy_round =
        changelog::should_announce(env!("GIT_SHA"), read_last_announced_build().as_deref());
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
                    announce_deploy_round,
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

                // Don Quijote proclaims once per genuinely new build; a
                // restart of the same build (crash loop, unattended-
                // upgrade reboot, config reload) stays silent. Never
                // allowed to block or fail startup.
                announce_changelog_if_new(&ctx.http, &cfg).await;

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
