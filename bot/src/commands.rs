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
                .content("Alto ahí. Solo los caballeros de la orden de El quijote pueden blandir tal comando.")
                .ephemeral(true),
        )
        .await?;
    }
    Ok(ok)
}

/// Da cuenta del estado de la ínsula: hidalgos presentes, TPS, tiempo en pie y memoria.
#[poise::command(slash_command)]
pub async fn status(ctx: Ctx<'_>) -> Result<(), Error> {
    ctx.defer().await?;
    let d = ctx.data();
    let mc = d.docker.inspect("mc").await.ok().flatten();
    let running = mc.as_ref().map(|s| s.running).unwrap_or(false);
    if !running {
        ctx.say(format!(
            "Yace dormida la ínsula de **{}**, vuestra merced. Ni un alma en sus dominios.",
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
            out += &format!("Hidalgos presentes: {}/{}", p.online, p.max);
            if !p.names.is_empty() {
                out += &format!(" ({})", p.names.join(", "));
            }
            out += "\n";
        }
        None => out += "Hidalgos presentes: el RCON aún no responde, paciencia vuestra merced\n",
    }
    if let Some(t) = tps {
        out += &format!(
            "TPS (1m/5m/15m): {:.1} / {:.1} / {:.1}{}\n",
            t.last_1m,
            t.last_5m,
            t.last_15m,
            if t.catching_up { " (recobrando el resuello)" } else { "" }
        );
    }
    if let Some((used, limit)) = mem {
        out += &format!(
            "Memoria (bálsamo de Fierabrás consumido): {:.1} / {:.1} GiB\n",
            used as f64 / 1e9 * 0.931,
            limit as f64 / 1e9 * 0.931
        );
    }
    if let Some(st) = mc.and_then(|s| s.started_at) {
        out += &format!("En pie desde: {st}\n");
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
        StartOutcome::Started => {
            ctx.say("¡Ensillad a Rocinante! La ínsula despierta de su letargo.").await?
        }
        StartOutcome::AlreadyRunning => {
            ctx.say("Ya galopa la ínsula, vuestra merced; no ha menester espuelas.").await?
        }
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
    ctx.say("La ínsula reposa por mandato vuestro. Dormirá hasta que un /start la despierte.").await?;
    Ok(())
}

#[poise::command(slash_command)]
pub async fn restart(ctx: Ctx<'_>) -> Result<(), Error> {
    if !is_admin(ctx).await? {
        return Ok(());
    }
    ctx.defer().await?;
    ctx.data().docker.restart("mc").await?;
    ctx.say("Recomienza la justa: la ínsula se reinicia.").await?;
    Ok(())
}

/// Envía un pregón vuestro al chat de la ínsula.
#[poise::command(slash_command)]
pub async fn say(
    ctx: Ctx<'_>,
    #[description = "El pregón a voces"] message: String,
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
        Ok(_) => ctx.say(format!("Pregonado a los cuatro vientos: {message}")).await?,
        Err(_) => {
            ctx.say("Duerme la ínsula; vuestro pregón se pierde en el silencio.").await?
        }
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
    #[description = "El nombre del futuro caballero de Minecraft"] name: String,
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
    #[description = "El nombre del escudero a desterrar de la ínsula"] name: String,
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
        Ok(out) => {
            ctx.say(if out.is_empty() {
                "Hecho está, vuestra merced.".into()
            } else {
                out
            })
            .await?
        }
        Err(_) => {
            ctx.say("Duerme la ínsula; para tocar el padrón de caballeros ha de estar despierta.")
                .await?
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
            ctx.say("Ya se labra un respaldo, vuestra merced; no hay dos encomiendas a la vez.")
                .await?;
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
                        ":white_check_mark: La encomienda de respaldo se ha cumplido con honor"
                    } else {
                        ":rotating_light: ¡La encomienda de respaldo ha FRACASADO!"
                    };
                    ctx.say(format!("{verdict} (código de salida {code})\n```\n{logs}\n```"))
                        .await?;
                    return Ok(());
                }
            }
            Ok(None) => {
                ctx.say(
                    "El contenedor del respaldo se ha desvanecido como castillo encantado (¿fue recreado a mitad de faena?); acuda vuestra merced al castillo (host).",
                )
                .await?;
                return Ok(());
            }
            Err(_) => {
                consecutive_errors += 1;
                if consecutive_errors >= 5 {
                    ctx.say(
                        "Se ha perdido el contacto con la API de Docker mientras vigilaba el respaldo; acuda vuestra merced al castillo (host).",
                    )
                    .await?;
                    return Ok(());
                }
            }
        }
    }
    ctx.say("El respaldo aún se afana tras diez minutos; acuda vuestra merced al castillo (host) a inquirir.").await?;
    Ok(())
}
