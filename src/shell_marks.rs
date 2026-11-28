//! Lo que se sabe de la shell que hay al otro lado de un terminal: si está
//! ejecutando una orden o esperando otra.
//!
//! Con la integración de shell (los marcadores OSC 133 que emiten fish,
//! PowerShell en Windows Terminal, y bash o zsh con la integración de
//! kitty, WezTerm, iTerm2 o VS Code) se sabe con exactitud dónde acaba el
//! indicador y cuándo empieza y termina cada orden. Sin ella se deduce de
//! lo que se ve: se pulsó Intro, hubo salida y después silencio con el
//! cursor parado al final de algo que parece un indicador.

use std::time::{Duration, Instant};

/// Marcador de integración de shell (`OSC 133 ; letra`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    /// `A`: la shell empieza a escribir su indicador.
    PromptStart,
    /// `B`: fin del indicador; lo que sigue lo teclea el usuario.
    PromptEnd,
    /// `C`: la orden empieza a ejecutarse.
    CommandStart,
    /// `D`: la orden ha terminado, con su código de salida si lo dice.
    CommandEnd(Option<i32>),
}

/// Interpreta los parámetros de una secuencia OSC como marcador 133.
pub fn parse_osc133(params: &[&[u8]]) -> Option<Mark> {
    let [b"133", kind, rest @ ..] = params else {
        return None;
    };
    Some(match *kind {
        b"A" => Mark::PromptStart,
        b"B" => Mark::PromptEnd,
        b"C" => Mark::CommandStart,
        b"D" => Mark::CommandEnd(
            rest.first()
                .and_then(|code| std::str::from_utf8(code).ok())
                .and_then(|code| code.trim().parse::<i32>().ok()),
        ),
        _ => return None,
    })
}

/// Una orden ha terminado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finished {
    /// Lo que ha durado.
    pub duration: Duration,
    /// Código de salida, si la shell lo comunica.
    pub exit_code: Option<i32>,
    /// Lo dice la shell con sus marcadores; si no, se ha deducido.
    pub exact: bool,
}

/// Silencio tras la última salida antes de mirar si la orden ha terminado
/// (solo sin integración de shell).
pub const QUIET: Duration = Duration::from_millis(2500);

/// Seguimiento de las órdenes en marcha en un terminal.
#[derive(Debug)]
pub struct Activity {
    /// La shell emite marcadores OSC 133.
    integrated: bool,
    /// Hay una orden en marcha desde este momento.
    running: Option<Instant>,
    /// Última salida recibida desde que empezó la orden.
    last_output: Option<Instant>,
    /// Ya se miró el silencio que siguió a esa salida.
    quiet_checked: bool,
    /// Silencio que se espera (se acorta en las pruebas).
    pub quiet: Duration,
}

impl Default for Activity {
    fn default() -> Self {
        Self {
            integrated: false,
            running: None,
            last_output: None,
            quiet_checked: false,
            quiet: QUIET,
        }
    }
}

impl Activity {
    /// La shell usa marcadores OSC 133: lo que se sabe de ella es exacto.
    pub fn is_integrated(&self) -> bool {
        self.integrated
    }

    /// Hay una orden en marcha (con integración de shell es seguro; sin
    /// ella solo se sabe que se pulsó Intro y no ha vuelto el indicador).
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// Olvida lo que hubiera (la sesión se ha cerrado o se reconecta).
    pub fn reset(&mut self) {
        let quiet = self.quiet;
        *self = Self {
            quiet,
            ..Self::default()
        };
    }

    /// Ha llegado un marcador de la shell.
    pub fn mark(&mut self, mark: &Mark, at: Instant) -> Option<Finished> {
        self.integrated = true;
        match mark {
            Mark::PromptEnd => None,
            Mark::CommandStart => {
                self.running = Some(at);
                None
            }
            // Un indicador nuevo sin haber visto el final de la orden
            // (shells que solo marcan el indicador): terminó igualmente.
            Mark::PromptStart => self.finish(at, None, true),
            Mark::CommandEnd(code) => self.finish(at, *code, true),
        }
    }

    fn finish(&mut self, at: Instant, exit_code: Option<i32>, exact: bool) -> Option<Finished> {
        let since = self.running.take()?;
        self.last_output = None;
        Some(Finished {
            duration: at.saturating_duration_since(since),
            exit_code,
            exact,
        })
    }

    /// Se ha enviado un Intro a la shell.
    pub fn enter(&mut self, at: Instant) {
        if self.integrated {
            // Con una orden en marcha, el Intro es para ella. Si no, la
            // orden empieza ahora (y `C`, si llega, lo afina).
            if self.running.is_none() {
                self.running = Some(at);
            }
        } else {
            // Sin marcadores, cada Intro vuelve a empezar la cuenta: la
            // duración es la del último tramo sin intervención.
            self.running = Some(at);
            self.last_output = None;
            self.quiet_checked = false;
        }
    }

    /// Ha llegado salida del terminal.
    pub fn output(&mut self, at: Instant) {
        if self.running.is_some() && !self.integrated {
            self.last_output = Some(at);
            self.quiet_checked = false;
        }
    }

    /// Sin integración de shell: tras un silencio, mira si el cursor ha
    /// quedado en algo que parece un indicador. `at_prompt` solo se
    /// consulta cuando toca.
    pub fn quiet_check(
        &mut self,
        now: Instant,
        at_prompt: impl FnOnce() -> bool,
    ) -> Option<Finished> {
        if self.integrated || self.quiet_checked {
            return None;
        }
        let last = self.last_output?;
        if now.saturating_duration_since(last) < self.quiet {
            return None;
        }
        self.quiet_checked = true;
        if !at_prompt() {
            return None;
        }
        // La orden acabó cuando dejó de escribir, no cuando se mira.
        self.finish(last, None, false)
    }

    /// Cuánto falta para la próxima comprobación de silencio, si hay
    /// alguna pendiente.
    pub fn next_check(&self, now: Instant) -> Option<Duration> {
        if self.integrated || self.quiet_checked || self.running.is_none() {
            return None;
        }
        let last = self.last_output?;
        Some(
            self.quiet
                .saturating_sub(now.saturating_duration_since(last)),
        )
    }
}

/// Finales habituales de un indicador o de una pregunta que espera
/// respuesta, con el espacio que suele separarlos de lo que se teclea.
const PROMPT_ENDINGS: [&str; 11] = [
    "$ ", "# ", "> ", "% ", "❯ ", "➜ ", "» ", "λ ", ": ", "? ", "] ",
];

/// El cursor está parado al final de algo que parece el indicador de una
/// shell o una pregunta: no hay nada detrás de él y lo de delante termina
/// como terminan los indicadores (`$ `, `# `, `> `…) o coincide con el
/// último indicador que se vio (`known`).
pub fn looks_like_prompt(before: &str, after: &str, known: Option<&str>) -> bool {
    if !after.trim().is_empty() || before.trim().is_empty() {
        return false;
    }
    if known.is_some_and(|known| !known.trim().is_empty() && known.trim_end() == before.trim_end())
    {
        return true;
    }
    // «C:\>» de cmd no deja espacio.
    PROMPT_ENDINGS.iter().any(|end| before.ends_with(end)) || before.ends_with('>')
}

/// Duración legible: «45 s», «2 min 5 s», «1 h 3 min».
pub fn human_duration(duration: Duration) -> String {
    let secs = duration.as_secs_f64().round() as u64;
    match secs {
        0..=59 => format!("{secs} s"),
        60..=3599 => match secs % 60 {
            0 => format!("{} min", secs / 60),
            rest => format!("{} min {rest} s", secs / 60),
        },
        _ => match (secs % 3600) / 60 {
            0 => format!("{} h", secs / 3600),
            minutes => format!("{} h {minutes} min", secs / 3600),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn osc_133_marks_are_parsed() {
        let parse = |text: &str| {
            let parts: Vec<&[u8]> = text.split(';').map(str::as_bytes).collect();
            parse_osc133(&parts)
        };
        assert_eq!(parse("133;A"), Some(Mark::PromptStart));
        assert_eq!(parse("133;A;k=s"), Some(Mark::PromptStart));
        assert_eq!(parse("133;B"), Some(Mark::PromptEnd));
        assert_eq!(parse("133;C"), Some(Mark::CommandStart));
        assert_eq!(parse("133;C;cmdline_url=ls"), Some(Mark::CommandStart));
        assert_eq!(parse("133;D"), Some(Mark::CommandEnd(None)));
        assert_eq!(parse("133;D;0"), Some(Mark::CommandEnd(Some(0))));
        assert_eq!(parse("133;D;130;aid=7"), Some(Mark::CommandEnd(Some(130))));
        // Códigos que no son números, letras desconocidas y otras secuencias.
        assert_eq!(parse("133;D;err=1"), Some(Mark::CommandEnd(None)));
        assert_eq!(
            parse("133;D;99999999999999999999"),
            Some(Mark::CommandEnd(None))
        );
        assert_eq!(parse("133;Z"), None);
        assert_eq!(parse("133"), None);
        assert_eq!(parse("7;file://host/tmp"), None);
        assert_eq!(parse("1337;A"), None);
    }

    #[test]
    fn integrated_shells_report_exact_durations() {
        let start = Instant::now();
        let mut shell = Activity::default();
        assert!(!shell.is_integrated());
        assert_eq!(shell.mark(&Mark::PromptStart, start), None);
        assert!(shell.is_integrated() && !shell.is_running());
        assert_eq!(shell.mark(&Mark::PromptEnd, start), None);
        // Intro: la orden empieza; `C` lo confirma un instante después.
        shell.enter(start + secs(1));
        assert!(shell.is_running());
        assert_eq!(shell.mark(&Mark::CommandStart, start + secs(2)), None);
        // Los Intro que se pulsen mientras corre son para la orden.
        shell.enter(start + secs(30));
        assert_eq!(
            shell.mark(&Mark::CommandEnd(Some(3)), start + secs(62)),
            Some(Finished {
                duration: secs(60),
                exit_code: Some(3),
                exact: true
            })
        );
        // El indicador que sigue no vuelve a darla por terminada.
        assert_eq!(shell.mark(&Mark::PromptStart, start + secs(62)), None);
        // Una shell que solo marca el indicador: Intro … indicador.
        shell.enter(start + secs(70));
        assert_eq!(
            shell.mark(&Mark::PromptStart, start + secs(100)),
            Some(Finished {
                duration: secs(30),
                exit_code: None,
                exact: true
            })
        );
        // Con marcadores, el silencio no cuenta.
        shell.enter(start + secs(110));
        shell.output(start + secs(111));
        assert_eq!(shell.next_check(start + secs(111)), None);
        assert_eq!(shell.quiet_check(start + secs(200), || true), None);
        assert!(shell.is_running());
        // Al reconectar se empieza de cero.
        shell.reset();
        assert!(!shell.is_integrated() && !shell.is_running());
    }

    #[test]
    fn without_marks_the_end_is_deduced_from_silence_at_a_prompt() {
        let start = Instant::now();
        let mut shell = Activity::default();
        // Salida sin haber pulsado Intro (el saludo): no hay orden.
        shell.output(start);
        assert_eq!(shell.next_check(start), None);
        assert_eq!(shell.quiet_check(start + secs(10), || true), None);

        shell.enter(start);
        assert!(shell.is_running());
        // Aún no ha habido salida: nada que mirar.
        assert_eq!(shell.next_check(start + secs(1)), None);
        shell.output(start + secs(1));
        assert_eq!(shell.next_check(start + secs(2)), Some(QUIET - secs(1)));
        // Demasiado pronto: sigue escribiendo.
        assert_eq!(shell.quiet_check(start + secs(2), || true), None);
        shell.output(start + secs(40));
        // Silencio, pero el cursor no está en un indicador (un `tail -f`).
        let mut asked = 0;
        assert_eq!(
            shell.quiet_check(start + secs(43), || {
                asked += 1;
                false
            }),
            None
        );
        // No se vuelve a mirar hasta que llegue más salida.
        assert_eq!(shell.next_check(start + secs(44)), None);
        assert_eq!(
            shell.quiet_check(start + secs(50), || {
                asked += 1;
                true
            }),
            None
        );
        assert_eq!(asked, 1);
        // Más salida y, ahora sí, el indicador: duró hasta la última salida.
        shell.output(start + secs(90));
        assert_eq!(
            shell.quiet_check(start + secs(93), || true),
            Some(Finished {
                duration: secs(90),
                exit_code: None,
                exact: false
            })
        );
        assert!(!shell.is_running());
        // Un Intro en mitad de la orden (una respuesta) reinicia la cuenta.
        shell.enter(start + secs(100));
        shell.output(start + secs(101));
        shell.enter(start + secs(130));
        shell.output(start + secs(170));
        assert_eq!(
            shell
                .quiet_check(start + secs(180), || true)
                .map(|f| f.duration),
            Some(secs(40))
        );
    }

    #[test]
    fn prompts_are_recognised() {
        for prompt in [
            "root@web:~# ",
            "ana@web:~/proyecto$ ",
            "web% ",
            "PS C:\\Users\\ana> ",
            "C:\\Users\\ana>",
            "❯ ",
            "mysql> ",
            ">>> ",
            "[sudo] password for ana: ",
            "¿Continuar? [s/N] ",
            "Overwrite (y/n)? ",
        ] {
            assert!(looks_like_prompt(prompt, "", None), "{prompt:?}");
        }
        for (before, after) in [
            ("", ""),
            ("   ", ""),
            // Barras de progreso y salida a medias.
            ("Descargando… 45%", ""),
            ("Receiving objects:  45% (4/9)", ""),
            ("[=====     ] 50", ""),
            ("compilando módulo", ""),
            // Hay texto detrás del cursor: se está editando algo.
            ("ana@web:~$ ", "ls -la"),
        ] {
            assert!(!looks_like_prompt(before, after, None), "{before:?}");
        }
        // El indicador que ya se vio vale aunque termine de otra forma.
        assert!(!looks_like_prompt("web ~ ⟩", "", None));
        assert!(looks_like_prompt("web ~ ⟩", "", Some("web ~ ⟩ ")));
        assert!(!looks_like_prompt("otra cosa", "", Some("web ~ ⟩ ")));
        assert!(!looks_like_prompt("", "", Some("")));
    }

    #[test]
    fn durations_read_well() {
        assert_eq!(human_duration(secs(0)), "0 s");
        assert_eq!(human_duration(secs(45)), "45 s");
        assert_eq!(human_duration(Duration::from_millis(5980)), "6 s");
        assert_eq!(human_duration(secs(120)), "2 min");
        assert_eq!(human_duration(secs(125)), "2 min 5 s");
        assert_eq!(human_duration(secs(3600)), "1 h");
        assert_eq!(human_duration(secs(3600 + 3 * 60 + 20)), "1 h 3 min");
    }
}
