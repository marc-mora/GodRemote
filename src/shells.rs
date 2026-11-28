//! Shells locales: cuáles hay en este equipo (PowerShell, cmd, WSL, bash…)
//! y cómo partir la orden que el usuario escriba para abrir una.

use std::path::{Path, PathBuf};

/// Una shell que se puede abrir en un terminal local.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellProfile {
    pub name: String,
    /// Programa y argumentos, como se escribirían en una orden.
    pub command: String,
}

impl ShellProfile {
    fn new(name: &str, command: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            command: command.into(),
        }
    }
}

/// Busca un programa en las carpetas del `PATH`.
fn in_path(program: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// Pone comillas a una ruta con espacios para usarla en una orden.
fn quoted(path: &Path) -> String {
    let text = path.display().to_string();
    if text.contains(' ') {
        format!("\"{text}\"")
    } else {
        text
    }
}

/// Shells disponibles en este equipo, la habitual primero.
pub fn detect() -> Vec<ShellProfile> {
    let mut found = vec![];
    if cfg!(windows) {
        if in_path("pwsh.exe").is_some() {
            found.push(ShellProfile::new("PowerShell 7", "pwsh.exe -NoLogo"));
        }
        found.push(ShellProfile::new(
            "Windows PowerShell",
            "powershell.exe -NoLogo",
        ));
        found.push(ShellProfile::new(
            crate::i18n::tr("Símbolo del sistema (cmd)"),
            "cmd.exe",
        ));
        if in_path("wsl.exe").is_some() {
            found.push(ShellProfile::new("WSL (Linux)", "wsl.exe"));
        }
        for base in ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"] {
            let Some(root) = std::env::var_os(base) else {
                continue;
            };
            let root = PathBuf::from(root);
            let bash = [
                root.join("Git/bin/bash.exe"),
                root.join("Programs/Git/bin/bash.exe"),
            ]
            .into_iter()
            .find(|p| p.is_file());
            if let Some(bash) = bash {
                found.push(ShellProfile::new(
                    "Git Bash",
                    format!("{} --login -i", quoted(&bash)),
                ));
                break;
            }
        }
    } else {
        // La del usuario y, después, las que el sistema declara.
        let mut paths: Vec<String> = std::env::var("SHELL").into_iter().collect();
        if let Ok(listed) = std::fs::read_to_string("/etc/shells") {
            paths.extend(
                listed
                    .lines()
                    .map(str::trim)
                    .filter(|l| l.starts_with('/'))
                    .map(String::from),
            );
        }
        paths.extend(["/bin/bash", "/bin/zsh", "/bin/sh"].map(String::from));
        let mut seen = vec![];
        for path in paths {
            let file = Path::new(&path);
            let Some(name) = file.file_name().map(|n| n.to_string_lossy().to_string()) else {
                continue;
            };
            // La misma shell suele estar en /bin y en /usr/bin.
            if !file.is_file() || seen.contains(&name) || name == "nologin" || name == "false" {
                continue;
            }
            seen.push(name.clone());
            found.push(ShellProfile::new(&name, quoted(file)));
        }
    }
    found
}

/// Parte una orden en programa y argumentos. Las comillas dobles agrupan
/// (para rutas con espacios); una ruta entera que exista como fichero se
/// toma como el programa aunque tenga espacios y no lleve comillas.
pub fn split_command(command: &str) -> Vec<String> {
    let command = command.trim();
    if command.is_empty() {
        return vec![];
    }
    if !command.contains('"') && Path::new(command).is_file() {
        return vec![command.to_string()];
    }
    let mut parts = vec![];
    let mut current = String::new();
    let mut in_quotes = false;
    let mut started = false;
    for c in command.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                started = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if started {
                    parts.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        parts.push(current);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_split_like_a_shell_would() {
        assert_eq!(split_command("  "), Vec::<String>::new());
        assert_eq!(split_command("pwsh.exe -NoLogo"), ["pwsh.exe", "-NoLogo"]);
        assert_eq!(
            split_command(r#""C:\Program Files\Git\bin\bash.exe" --login  -i"#),
            [r"C:\Program Files\Git\bin\bash.exe", "--login", "-i"]
        );
        assert_eq!(
            split_command(r#"wsl.exe -d "Ubuntu 24.04""#),
            ["wsl.exe", "-d", "Ubuntu 24.04"]
        );
        assert_eq!(split_command(r#"programa """#), ["programa", ""]);
        // Una ruta con espacios que existe es el programa entero.
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("mi shell");
        std::fs::write(&program, "").unwrap();
        let text = program.display().to_string();
        assert_eq!(split_command(&text), std::slice::from_ref(&text));
        assert_eq!(quoted(&program), format!("\"{text}\""));
    }

    #[test]
    fn this_machine_has_at_least_one_shell() {
        let shells = detect();
        assert!(!shells.is_empty());
        // Sin repetidos, y cada una con algo que ejecutar.
        for (i, shell) in shells.iter().enumerate() {
            assert!(!split_command(&shell.command).is_empty(), "{shell:?}");
            assert!(
                shells[..i].iter().all(|s| s.name != shell.name),
                "{shells:?}"
            );
        }
        if cfg!(windows) {
            assert!(shells.iter().any(|s| s.command == "cmd.exe"));
        }
    }
}
