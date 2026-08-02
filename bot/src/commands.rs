use crate::config::Config;
use crate::docker::{DockerCtl, StartOutcome};
use crate::fortuna;
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
    vec![status(), start(), stop(), restart(), say(), whitelist(), backup(), fortuna(), help()]
}

/// El pergamino de los poderes de este caballero.
#[poise::command(slash_command)]
pub async fn help(ctx: Ctx<'_>) -> Result<(), Error> {
    ctx.say(concat!(
        "**El pergamino de Don Quijote del nether** \u{1F4DC}\n\n",
        "*Para todo villano o caballero:*\n",
        "`/status` \u{2014} el estado de la \u{ED}nsula: almas presentes, TPS, memoria\n",
        "`/whitelist add <nombre>` \u{2014} armar caballero a un nuevo jugador\n",
        "`/whitelist list` \u{2014} leer el rollo de los caballeros\n",
        "`/say <mensaje>` \u{2014} hablar al mundo con la voz del servidor\n",
        "`/help` \u{2014} este pergamino\n\n",
        "*Solo para la orden de El quijote:*\n",
        "`/start` `/stop` `/restart` \u{2014} despertar, dormir o reiniciar la \u{ED}nsula\n",
        "`/backup now` \u{2014} encomendar los mundos al arca de respaldo\n",
        "`/fortuna jugador:<nombre> horas:<n> suerte:<id>` \u{2014} girar a voluntad la Rueda de la Fortuna sobre un caballero\n\n",
        "*Solo para los Lud\u{F3}patas Antisionistas:*\n",
        "`/whitelist remove <nombre>` \u{2014} desterrar a un caballero\n\n",
        "*Y sin que nadie lo mande, este hidalgo proclama:* ca\u{ED}das y resurrecciones ",
        "de la \u{ED}nsula, fracasos del arca, cr\u{F3}nicas de sesiones largas, las ",
        "haza\u{F1}as de cada caballero, los giros de la Rueda de la Fortuna y las ",
        "eleg\u{ED}as de quienes cayeron en el intento."
    ))
    .await?;
    Ok(())
}

async fn has_role(ctx: Ctx<'_>, role_id: u64) -> bool {
    let role = RoleId::new(role_id);
    ctx.author_member()
        .await
        .map(|m| m.roles.contains(&role))
        .unwrap_or(false)
}

async fn is_admin(ctx: Ctx<'_>) -> Result<bool, Error> {
    let ok = has_role(ctx, ctx.data().cfg.admin_role_id).await;
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

/// Gate for `/whitelist remove`: banishing a knight from the roster is
/// graver business than the rest of the admin commands, so it answers to
/// its own role when one is configured, falling back to the ordinary
/// admin gate otherwise.
async fn is_remover(ctx: Ctx<'_>) -> Result<bool, Error> {
    let Some(role_id) = ctx.data().cfg.remover_role_id else {
        return is_admin(ctx).await;
    };
    let ok = has_role(ctx, role_id).await;
    if !ok {
        ctx.send(
            poise::CreateReply::default()
                .content("Alto ahí. Solo los caballeros de la orden de los Ludópatas Antisionistas tienen licencia para desterrar a un hidalgo de la ínsula.")
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
    // Knighting is open to every villager; only banishment stays gated.
    ctx.defer().await?;
    whitelist_cmd(ctx, &format!("whitelist add {name}")).await
}

#[poise::command(slash_command, rename = "remove")]
pub async fn wl_remove(
    ctx: Ctx<'_>,
    #[description = "El nombre del escudero a desterrar de la ínsula"] name: String,
) -> Result<(), Error> {
    if !is_remover(ctx).await? {
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

/// Translates vanilla whitelist RCON responses into the hidalgo's tongue.
/// The raw server text is matched by its stable English substrings; anything
/// unrecognized is wrapped rather than echoed bare, so operators still see it.
fn quixotify_whitelist(raw: &str) -> String {
    let r = raw.trim();
    if let Some(name) = r.strip_prefix("Added ").and_then(|s| s.strip_suffix(" to the whitelist")) {
        return format!(
            ":crossed_swords: ¡Regocijaos! **{name}** ha sido armado caballero de la ínsula. \
             Que su pico sea recto y su lana abundante."
        );
    }
    if let Some(name) = r.strip_prefix("Removed ").and_then(|s| s.strip_suffix(" from the whitelist")) {
        return format!(
            ":scroll: Con pesar lo proclamo: **{name}** ha sido desterrado del padrón. \
             Que los molinos le sean leves en su exilio."
        );
    }
    if r.contains("already whitelisted") {
        return "Sosegaos, vuestra merced: ese caballero ya figura en el padrón desde antaño.".into();
    }
    if r.contains("not whitelisted") {
        return "No hallo a tal caballero en el padrón; nadie puede ser desterrado de donde nunca moró.".into();
    }
    if r.contains("does not exist") {
        return "Por más que escudriño los reinos de Mojang, tal nombre no existe. ¿Errata de vuestra pluma, quizá?".into();
    }
    if let Some(rest) = r.split("whitelisted player(s):").nth(1) {
        let names = rest.trim();
        return format!(":scroll: **El rollo de los caballeros de la ínsula:** {names}");
    }
    if r.contains("no whitelisted players") || r.is_empty() {
        return "El padrón yace virgen, sin caballero alguno. Triste soledad la de esta ínsula.".into();
    }
    format!("Responde la ínsula con palabras que este hidalgo no alcanza a versar: *{r}*")
}

async fn whitelist_cmd(ctx: Ctx<'_>, cmd: &str) -> Result<(), Error> {
    match ctx.data().rcon.lock().await.cmd(cmd).await {
        Ok(out) => ctx.say(quixotify_whitelist(&out)).await?,
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

/// Gira la Rueda de la Fortuna sobre un caballero, fuera de su milestone natural.
#[poise::command(slash_command)]
pub async fn fortuna(
    ctx: Ctx<'_>,
    #[description = "El nombre del hidalgo sobre quien gira la rueda"] jugador: String,
    #[description = "Horas de sesión a ponderar (0-100); ignoradas si se fuerza `suerte`"]
    #[min = 0_u32]
    #[max = 100_u32]
    horas: u32,
    #[description = "Fuerza un efecto concreto por su id (p. ej. pan, zombi, veneno, rayo, disco_de_oro)"]
    suerte: Option<String>,
) -> Result<(), Error> {
    if !is_admin(ctx).await? {
        return Ok(());
    }
    ctx.defer().await?;

    if !fortuna::valid_player_name(&jugador) {
        ctx.say(
            "Ese nombre no calza con la usanza de los hidalgos de Minecraft (letras, dígitos y guion bajo, hasta 16 caracteres). Rehúso invocar la rueda con tal nombre.",
        )
        .await?;
        return Ok(());
    }

    let mut rng = fortuna::EntropyRolls::new();
    let spin = match &suerte {
        Some(effect_id) => match fortuna::spin_forced(&jugador, effect_id, &mut rng) {
            Some(s) => s,
            None => {
                ctx.say(format!(
                    "No hallo tal suerte en el sino de esta rueda: `{effect_id}` no es un efecto conocido."
                ))
                .await?;
                return Ok(());
            }
        },
        None => fortuna::spin(&jugador, horas, &mut rng),
    };

    tracing::info!(
        "/fortuna: {} @ {}h -> {} ({:?})",
        jugador,
        horas,
        spin.effect_id,
        spin.category
    );

    let mut any_failed = false;
    {
        let mut r = ctx.data().rcon.lock().await;
        if let Err(e) = r.cmd(&format!("say {}", spin.game_msg)).await {
            tracing::warn!("fortuna: rcon say failed: {e:#}");
            any_failed = true;
        }
        for cmd in &spin.commands {
            if let Err(e) = r.cmd(cmd).await {
                tracing::warn!("fortuna: rcon command {cmd:?} failed: {e:#}");
                any_failed = true;
            }
        }
    }

    let mut reply = spin.discord_msg.clone();
    if let Some(effect_id) = &suerte {
        reply += &format!("\n*(suerte: {effect_id})*");
    }
    if any_failed {
        reply += "\n:warning: Alguna orden de RCON no obtuvo respuesta; quizá la ínsula duerme o anda a medio despertar.";
    }
    ctx.say(reply).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::quixotify_whitelist;

    #[test]
    fn knights_added_players_by_name() {
        let m = quixotify_whitelist("Added CamRG121 to the whitelist");
        assert!(m.contains("CamRG121") && m.contains("caballero"));
    }

    #[test]
    fn banishes_removed_players_by_name() {
        let m = quixotify_whitelist("Removed CamRG121 from the whitelist");
        assert!(m.contains("CamRG121") && m.contains("desterrado"));
    }

    #[test]
    fn classifies_the_known_refusals() {
        assert!(quixotify_whitelist("Player is already whitelisted").contains("antaño"));
        assert!(quixotify_whitelist("Player is not whitelisted").contains("nunca moró"));
        assert!(quixotify_whitelist("That player does not exist").contains("Mojang"));
    }

    #[test]
    fn renders_the_roll_with_names() {
        let m = quixotify_whitelist("There are 2 whitelisted player(s): alice, bob");
        assert!(m.contains("rollo") && m.contains("alice, bob"));
    }

    #[test]
    fn empty_roll_and_unknown_responses_stay_informative() {
        assert!(quixotify_whitelist("There are no whitelisted players").contains("virgen"));
        let unknown = quixotify_whitelist("Some new server response");
        assert!(unknown.contains("Some new server response"));
    }
}
