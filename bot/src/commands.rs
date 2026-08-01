use crate::config::Config;
use crate::docker::{DockerCtl, StartOutcome};
use crate::parse;
use crate::rcon::McRcon;
use poise::serenity_prelude::RoleId;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct Data {
    pub cfg: Config,
    pub rcon: Arc<Mutex<McRcon>>,
    pub docker: DockerCtl,
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Ctx<'a> = poise::Context<'a, Data, Error>;

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![status(), start(), stop(), restart(), say(), whitelist(), backup()]
}

async fn is_admin(ctx: Ctx<'_>) -> Result<bool, Error> {
    let role = RoleId::new(ctx.data().cfg.admin_role_id);
    let ok = ctx
        .author_member()
        .await
        .map(|m| m.roles.contains(&role))
        .unwrap_or(false);
    if !ok {
        ctx.send(
            poise::CreateReply::default()
                .content("You need the admin role for that.")
                .ephemeral(true),
        )
        .await?;
    }
    Ok(ok)
}

/// Server status: players, TPS, uptime, memory.
#[poise::command(slash_command)]
pub async fn status(ctx: Ctx<'_>) -> Result<(), Error> {
    ctx.defer().await?;
    let d = ctx.data();
    let mc = d.docker.inspect("mc").await.ok().flatten();
    let running = mc.as_ref().map(|s| s.running).unwrap_or(false);
    if !running {
        ctx.say(format!(
            ":red_circle: **{}** is offline.",
            d.cfg.server_address
        ))
        .await?;
        return Ok(());
    }
    let (list, tps) = {
        let mut r = d.rcon.lock().await;
        (r.cmd("list").await.ok(), r.cmd("spark tps").await.ok())
    };
    let players = list.as_deref().and_then(parse::parse_list);
    let tps = tps.as_deref().and_then(parse::parse_tps);
    let mem = d.docker.stats_mem("mc").await.ok().flatten();

    let mut out = format!(":green_circle: **{}**\n", d.cfg.server_address);
    match players {
        Some(p) => {
            out += &format!("Players: {}/{}", p.online, p.max);
            if !p.names.is_empty() {
                out += &format!(" ({})", p.names.join(", "));
            }
            out += "\n";
        }
        None => out += "Players: RCON not answering yet\n",
    }
    if let Some(t) = tps {
        out += &format!(
            "TPS (1m/5m/15m): {:.1} / {:.1} / {:.1}{}\n",
            t.last_1m,
            t.last_5m,
            t.last_15m,
            if t.catching_up { " (catching up)" } else { "" }
        );
    }
    if let Some((used, limit)) = mem {
        out += &format!(
            "Memory: {:.1} / {:.1} GiB\n",
            used as f64 / 1e9 * 0.931,
            limit as f64 / 1e9 * 0.931
        );
    }
    if let Some(st) = mc.and_then(|s| s.started_at) {
        out += &format!("Container started: {st}\n");
    }
    ctx.say(out).await?;
    Ok(())
}

#[poise::command(slash_command)]
pub async fn start(ctx: Ctx<'_>) -> Result<(), Error> {
    if !is_admin(ctx).await? {
        return Ok(());
    }
    ctx.defer().await?;
    match ctx.data().docker.start("mc").await? {
        StartOutcome::Started => ctx.say("Starting the server.").await?,
        StartOutcome::AlreadyRunning => ctx.say("Already running.").await?,
    };
    Ok(())
}

#[poise::command(slash_command)]
pub async fn stop(ctx: Ctx<'_>) -> Result<(), Error> {
    if !is_admin(ctx).await? {
        return Ok(());
    }
    ctx.defer().await?;
    ctx.data().docker.stop("mc").await?;
    ctx.say("Server stopped. It stays stopped until /start.").await?;
    Ok(())
}

#[poise::command(slash_command)]
pub async fn restart(ctx: Ctx<'_>) -> Result<(), Error> {
    if !is_admin(ctx).await? {
        return Ok(());
    }
    ctx.defer().await?;
    ctx.data().docker.restart("mc").await?;
    ctx.say("Restarting.").await?;
    Ok(())
}

/// Relay a message to in-game chat.
#[poise::command(slash_command)]
pub async fn say(
    ctx: Ctx<'_>,
    #[description = "Message"] message: String,
) -> Result<(), Error> {
    ctx.defer().await?;
    match ctx
        .data()
        .rcon
        .lock()
        .await
        .cmd(&format!("say {message}"))
        .await
    {
        Ok(_) => ctx.say(format!("Sent: {message}")).await?,
        Err(_) => ctx.say("Server is offline, nothing sent.").await?,
    };
    Ok(())
}

#[poise::command(slash_command, subcommands("wl_add", "wl_remove", "wl_list"))]
pub async fn whitelist(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command, rename = "add")]
pub async fn wl_add(
    ctx: Ctx<'_>,
    #[description = "Minecraft username"] name: String,
) -> Result<(), Error> {
    if !is_admin(ctx).await? {
        return Ok(());
    }
    ctx.defer().await?;
    whitelist_cmd(ctx, &format!("whitelist add {name}")).await
}

#[poise::command(slash_command, rename = "remove")]
pub async fn wl_remove(
    ctx: Ctx<'_>,
    #[description = "Minecraft username"] name: String,
) -> Result<(), Error> {
    if !is_admin(ctx).await? {
        return Ok(());
    }
    ctx.defer().await?;
    whitelist_cmd(ctx, &format!("whitelist remove {name}")).await
}

#[poise::command(slash_command, rename = "list")]
pub async fn wl_list(ctx: Ctx<'_>) -> Result<(), Error> {
    ctx.defer().await?;
    whitelist_cmd(ctx, "whitelist list").await
}

async fn whitelist_cmd(ctx: Ctx<'_>, cmd: &str) -> Result<(), Error> {
    match ctx.data().rcon.lock().await.cmd(cmd).await {
        Ok(out) => ctx.say(if out.is_empty() { "Done.".into() } else { out }).await?,
        Err(_) => {
            ctx.say("Server is offline; whitelist changes need it up.").await?
        }
    };
    Ok(())
}

#[poise::command(slash_command, subcommands("backup_now"))]
pub async fn backup(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command, rename = "now")]
pub async fn backup_now(ctx: Ctx<'_>) -> Result<(), Error> {
    if !is_admin(ctx).await? {
        return Ok(());
    }
    ctx.defer().await?;
    let d = ctx.data();
    match d.docker.start("mc-backup").await? {
        StartOutcome::AlreadyRunning => {
            ctx.say("A backup is already running.").await?;
            return Ok(());
        }
        StartOutcome::Started => {}
    }
    // Poll to completion (max 10 min), then report honestly.
    let mut consecutive_errors = 0u32;
    for _ in 0..300 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        match d.docker.inspect("mc-backup").await {
            Ok(Some(s)) => {
                consecutive_errors = 0;
                if !s.running {
                    let logs = d.docker.logs_tail("mc-backup", 5).await.unwrap_or_default();
                    let code = s.exit_code.unwrap_or(-1);
                    let verdict = if code == 0 {
                        ":white_check_mark: Backup finished"
                    } else {
                        ":rotating_light: Backup FAILED"
                    };
                    ctx.say(format!("{verdict} (exit {code})\n```\n{logs}\n```"))
                        .await?;
                    return Ok(());
                }
            }
            Ok(None) => {
                ctx.say(
                    "Backup container no longer exists (was it recreated mid-run?), check the host.",
                )
                .await?;
                return Ok(());
            }
            Err(_) => {
                consecutive_errors += 1;
                if consecutive_errors >= 5 {
                    ctx.say(
                        "Lost contact with the Docker API while polling the backup, check the host.",
                    )
                    .await?;
                    return Ok(());
                }
            }
        }
    }
    ctx.say("Backup still running after 10 minutes, check the host.").await?;
    Ok(())
}
