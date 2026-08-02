use anyhow::Context as _;
use mc_query::rcon::RconClient;

/// Folds Spanish (and general Latin-1) text down to ASCII for the RCON
/// wire. In-game text loses its accents; Discord messages keep theirs.
/// Anything still non-ASCII after folding is dropped rather than risking
/// a rejected payload.
pub fn ascii_fold(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            'Á' | 'À' | 'Ä' | 'Â' => 'A',
            'É' | 'È' | 'Ë' | 'Ê' => 'E',
            'Í' | 'Ì' | 'Ï' | 'Î' => 'I',
            'Ó' | 'Ò' | 'Ö' | 'Ô' => 'O',
            'Ú' | 'Ù' | 'Ü' | 'Û' => 'U',
            'Ñ' => 'N',
            'ç' => 'c',
            'Ç' => 'C',
            '¡' | '¿' => ' ',
            '«' | '»' => '"',
            '—' | '–' => '-',
            '…' => '.',
            other => other,
        })
        .filter(|c| c.is_ascii())
        .collect()
}

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
    ///
    /// The payload is ASCII-folded first: mc_query rejects non-ASCII with
    /// "non-ascii payload", so an accented Spanish `say` would otherwise
    /// fail while the plain-ASCII commands beside it succeed.
    pub async fn cmd(&mut self, cmd: &str) -> anyhow::Result<String> {
        let cmd = &ascii_fold(cmd);
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

#[cfg(test)]
mod tests {
    use super::ascii_fold;

    #[test]
    fn folds_spanish_accents_and_punctuation() {
        assert_eq!(
            ascii_fold("¡La ínsula tiembla! Un veneno de encantador le corre por las venas."),
            " La insula tiembla! Un veneno de encantador le corre por las venas."
        );
        assert_eq!(ascii_fold("¿Dónde está el Quijote?"), " Donde esta el Quijote?");
        assert_eq!(ascii_fold("ñoño ÑOÑO"), "nono NONO");
    }

    #[test]
    fn plain_ascii_passes_through_unchanged() {
        let cmd = "execute at Juan run summon minecraft:zombie ~ ~ ~";
        assert_eq!(ascii_fold(cmd), cmd);
        assert_eq!(ascii_fold("give Juan minecraft:bread 3"), "give Juan minecraft:bread 3");
    }

    #[test]
    fn output_is_always_ascii() {
        for s in [
            "emoji: ⚡🏰 y acentos áéíóú",
            "巨大な文字",
            ":skull: ¡jauría de dos zombis!",
        ] {
            assert!(ascii_fold(s).is_ascii(), "not ascii: {s}");
        }
    }

    #[test]
    fn drops_unmappable_glyphs_but_keeps_the_sentence() {
        let out = ascii_fold("La rueda ⚡ ha girado para Juan");
        assert!(out.is_ascii());
        assert!(out.contains("La rueda"));
        assert!(out.contains("ha girado para Juan"));
    }
}
