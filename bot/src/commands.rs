use crate::config::Config;
use crate::docker::{self, DockerCtl, StartOutcome};
use crate::fortuna;
use crate::parse;
use crate::rcon::McRcon;
use poise::serenity_prelude::RoleId;
use std::sync::Arc;
use std::time::{Duration, Instant};
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

/// Renders the "not running" branch of `/status` (and the lifecycle
/// commands' honest-outcome replies): a wedged container must never again
/// read like a clean stop. Pure and unit-testable without a running server.
fn state_line(status: Option<&str>, exit_code: Option<i64>) -> String {
    if docker::is_wedged(status) {
        return wedge_explanation(status);
    }
    match status {
        Some("exited") => {
            let code = exit_code.unwrap_or(0);
            format!("Yace dormida la ínsula, detenida en buen orden (código de salida {code}). Ni un alma en sus dominios.")
        }
        Some(s) => format!("Yace la ínsula en estado '{s}', vuestra merced. Ni un alma en sus dominios."),
        None => "Yace dormida la ínsula, vuestra merced. Ni un alma en sus dominios.".to_string(),
    }
}

/// Shared Quijote-voiced explanation of a wedged container: podman's own
/// state, the fact that neither `/start` nor `/restart` can fix it, and
/// that the host watchdog will rebuild it on its own within minutes.
fn wedge_explanation(status: Option<&str>) -> String {
    format!(
        "Trabada yace la ínsula (podman la reporta en el estado '{}'), no por reposo sino por un mal \
         encantamiento en sus entrañas. Ni /start ni /restart pueden ya desatarla: el vigía del \
         castillo (host) la reconstruirá por su propia mano en pocos minutos, sin que nadie lo mande. \
         Paciencia, vuestra merced; si tarda en exceso, consulte el RUNBOOK.",
        status.unwrap_or("desconocido")
    )
}

/// Da cuenta del estado de la ínsula: hidalgos presentes, TPS, tiempo en pie y memoria.
#[poise::command(slash_command)]
pub async fn status(ctx: Ctx<'_>) -> Result<(), Error> {
    ctx.defer().await?;
    let d = ctx.data();
    let mc = d.docker.inspect("mc").await.ok().flatten();
    let running = mc.as_ref().map(|s| s.running).unwrap_or(false);
    if !running {
        let status = mc.as_ref().and_then(|s| s.status.as_deref());
        let exit_code = mc.as_ref().and_then(|s| s.exit_code);
        let icon = if docker::is_wedged(status) { ":skull:" } else { ":red_circle:" };
        ctx.say(format!(
            "{icon} **{}**\n{}",
            d.cfg.server_address,
            state_line(status, exit_code)
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
    let d = ctx.data();

    // Never fire /start blind: a wedged container answers podman's own
    // "must be in Created or Stopped state" 500 either way, and a bare
    // "start failed" would be exactly the lie the incident was about. Tell
    // the truth up front and don't even try.
    let pre = d.docker.inspect("mc").await?;
    let pre_status = pre.as_ref().and_then(|s| s.status.as_deref());
    if docker::is_wedged(pre_status) {
        ctx.say(wedge_explanation(pre_status)).await?;
        return Ok(());
    }

    match d.docker.start("mc").await? {
        StartOutcome::AlreadyRunning => {
            ctx.say("Ya galopa la ínsula, vuestra merced; no ha menester espuelas.").await?;
            return Ok(());
        }
        StartOutcome::Started => {}
    }

    // Report what actually happened, not just that the call returned Ok.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let post = d.docker.inspect("mc").await.ok().flatten();
    let msg = match post {
        Some(s) if s.running => "¡Ensillad a Rocinante! La ínsula despierta de su letargo.".to_string(),
        Some(s) => format!(
            "Se libró la orden de despertar, mas la ínsula aún no galopa (estado '{}'). Aguarde vuestra merced unos instantes y consulte /status.",
            s.status.as_deref().unwrap_or("desconocido")
        ),
        None => "Se libró la orden de despertar, mas ya no hallo rastro de la ínsula; acuda vuestra merced al castillo (host).".to_string(),
    };
    ctx.say(msg).await?;
    Ok(())
}

#[poise::command(slash_command)]
pub async fn stop(ctx: Ctx<'_>) -> Result<(), Error> {
    if !is_admin(ctx).await? {
        return Ok(());
    }
    ctx.defer().await?;
    let d = ctx.data();
    d.docker.stop("mc").await?;

    // podman can answer 204 to a stop that changes nothing on a wedged
    // container (verified in the incident: 12:37:17, no-op 204). Poll for
    // the real outcome instead of reporting success unconditionally.
    let deadline = Instant::now() + Duration::from_secs(150);
    let final_status = loop {
        let s = d.docker.inspect("mc").await.ok().flatten().and_then(|s| s.status);
        if s.as_deref() == Some("exited") || Instant::now() >= deadline {
            break s;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    };

    match final_status.as_deref() {
        Some("exited") => {
            ctx.say("La ínsula reposa por mandato vuestro. Dormirá hasta que un /start la despierte.").await?;
        }
        Some(s) if docker::is_wedged(Some(s)) => {
            ctx.say(format!(
                "El mandato de reposo se libró, mas la ínsula ha quedado TRABADA en el estado '{s}' en vez de \
                 dormir en paz. El vigía del castillo (host) la reconstruirá en pocos minutos y VOLVERÁ A \
                 LEVANTARSE por su propia mano — si vuestra merced desea que repose de veras, deberá librar \
                 /stop de nuevo una vez se alce."
            ))
            .await?;
        }
        other => {
            ctx.say(format!(
                "El mandato de reposo se libró, mas tras dos minutos y medio la ínsula aún no confirma su \
                 descanso (estado '{}'). Consulte vuestra merced /status.",
                other.unwrap_or("desconocido")
            ))
            .await?;
        }
    }
    Ok(())
}

#[poise::command(slash_command)]
pub async fn restart(ctx: Ctx<'_>) -> Result<(), Error> {
    if !is_admin(ctx).await? {
        return Ok(());
    }
    ctx.defer().await?;
    let d = ctx.data();

    let pre = d.docker.inspect("mc").await?;
    let pre_status = pre.as_ref().and_then(|s| s.status.as_deref());
    if docker::is_wedged(pre_status) {
        ctx.say(wedge_explanation(pre_status)).await?;
        return Ok(());
    }

    d.docker.restart_via_stop_start("mc").await?;
    ctx.say("Recomienza la justa: la ínsula se detuvo y se alzó de nuevo en buen orden.").await?;
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
/// Proclamations for knights admitted through the EasyWhitelist door, i.e.
/// those whose names Mojang has never heard because they never paid.
/// Chosen deterministically by name length so the same pauper gets the
/// same jab every time.
const POBRE_POOL: &[&str] = &[
    ":coin: ¡Regocijaos a medias! **{name}** entra en el padrón, mas por la puerta de servicio: \
     no le halla Mojang en sus registros, que jamás vio un maravedí suyo. Bienvenido sea el pobre.",
    ":coin: Armado caballero queda **{name}**, aunque conste en acta: su cuenta es tan falsa como \
     los gigantes de mi imaginación, y su bolsa más vacía que la despensa de Sancho.",
    ":coin: Admitido sea **{name}** por la vía de los menesterosos. Mojang no lo conoce, y con razón: \
     antes gastaría el hidalgo en yelmos de barbero que este en su propio juego.",
    ":coin: Pasa **{name}** al padrón sin pagar peaje, como quien entra a la venta por el corral. \
     Que nadie se lo eche en cara... salvo yo, que para eso soy el cronista.",
];

fn pobre_message(name: &str) -> String {
    let idx = name.chars().count() % POBRE_POOL.len();
    POBRE_POOL[idx].replace("{name}", name)
}

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

/// Vanilla `whitelist add/remove` resolves names through Mojang, so it
/// rejects the offline-account friends EasyAuth exists to welcome. When
/// the server answers "does not exist", retry the same operation through
/// EasyWhitelist, which keeps a name-based roster. Returns the response
/// worth showing the user.
fn easywhitelist_fallback(cmd: &str) -> Option<String> {
    let rest = cmd.strip_prefix("whitelist ")?;
    let (op, target) = rest.split_once(' ')?;
    if op != "add" && op != "remove" {
        return None;
    }
    Some(format!("easywhitelist {op} {target}"))
}

async fn whitelist_cmd(ctx: Ctx<'_>, cmd: &str) -> Result<(), Error> {
    let first = { ctx.data().rcon.lock().await.cmd(cmd).await };
    let mut via_pauper_door = false;
    let out = match first {
        Ok(out) => {
            if out.contains("does not exist") {
                match easywhitelist_fallback(cmd) {
                    Some(alt) => {
                        tracing::info!(%cmd, "mojang lookup failed, retrying via easywhitelist");
                        match ctx.data().rcon.lock().await.cmd(&alt).await {
                            Ok(second) => {
                                via_pauper_door = true;
                                second
                            }
                            Err(_) => out,
                        }
                    }
                    None => out,
                }
            } else {
                out
            }
        }
        Err(_) => {
            ctx.say("Duerme la ínsula; para tocar el padrón de caballeros ha de estar despierta.")
                .await?;
            return Ok(());
        }
    };
    let added_name = out
        .trim()
        .strip_prefix("Added ")
        .and_then(|s| s.strip_suffix(" to the whitelist"));
    match (via_pauper_door, added_name) {
        (true, Some(name)) => ctx.say(pobre_message(name)).await?,
        _ => ctx.say(quixotify_whitelist(&out)).await?,
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
    fn state_line_distinguishes_stopped_from_wedged() {
        use super::state_line;

        let stopped = state_line(Some("exited"), Some(0));
        assert!(stopped.contains("dormida"), "{stopped}");
        assert!(!stopped.contains("TRABADA") && !stopped.contains("Trabada"), "{stopped}");

        for wedged_status in ["stopping", "removing", "dead", "paused"] {
            let wedged = state_line(Some(wedged_status), None);
            assert!(wedged.contains("Trabada"), "{wedged}");
            assert!(wedged.contains(wedged_status), "podman's own state should be named: {wedged}");
            assert!(!wedged.contains("dormida"), "wedge must not read as a clean stop: {wedged}");
            assert!(wedged.contains("/start") && wedged.contains("/restart"));
        }

        let unknown = state_line(None, None);
        assert!(unknown.contains("dormida"), "{unknown}");
    }

    #[test]
    fn pauper_proclamations_name_the_pauper_and_vary() {
        use super::{pobre_message, POBRE_POOL};
        for name in ["Bispannus", "x", "CamRG121", "unnombrelargo16"] {
            let m = pobre_message(name);
            assert!(m.contains(name), "missing name: {m}");
            assert!(!m.contains("{name}"), "placeholder left: {m}");
        }
        assert!(POBRE_POOL.len() >= 3);
        let mut sorted: Vec<&str> = POBRE_POOL.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), POBRE_POOL.len(), "duplicate pauper lines");
        // different name lengths reach different lines
        assert_ne!(pobre_message("ab"), pobre_message("abc"));
    }

    #[test]
    fn falls_back_to_easywhitelist_for_add_and_remove() {
        use super::easywhitelist_fallback;
        assert_eq!(
            easywhitelist_fallback("whitelist add Bispannus").as_deref(),
            Some("easywhitelist add Bispannus")
        );
        assert_eq!(
            easywhitelist_fallback("whitelist remove Bispannus").as_deref(),
            Some("easywhitelist remove Bispannus")
        );
    }

    #[test]
    fn no_fallback_for_list_or_foreign_commands() {
        use super::easywhitelist_fallback;
        assert!(easywhitelist_fallback("whitelist list").is_none());
        assert!(easywhitelist_fallback("say hola").is_none());
        assert!(easywhitelist_fallback("whitelist").is_none());
    }

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
