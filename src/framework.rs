//! Conservative detection of JS dev servers and argv/env alignment.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::net::IpAddr;
use std::path::Path;

/// Supported development servers that Nook can align without rewriting project files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Framework {
    Vite,
    Nuxt,
    Next,
    Nitro,
    Astro,
}

/// How the framework should be selected for a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameworkChoice {
    Auto,
    Disabled,
    Forced(Framework),
}

impl Framework {
    fn from_program(name: &str) -> Option<Self> {
        match name {
            "vite" => Some(Self::Vite),
            "nuxt" | "nuxi" => Some(Self::Nuxt),
            "next" => Some(Self::Next),
            "nitro" | "nitropack" => Some(Self::Nitro),
            "astro" => Some(Self::Astro),
            _ => None,
        }
    }

    /// Extra environment variables layered on top of `PORT` / `HOST` / `NOOK_URL`.
    pub(crate) fn environment(
        self,
        port: u16,
        bind_address: IpAddr,
        hostname: &str,
    ) -> Vec<(OsString, OsString)> {
        let port = port.to_string();
        let host = bind_address.to_string();
        let mut variables = Vec::new();
        match self {
            Self::Vite | Self::Nuxt | Self::Astro => {
                if let Some(value) = additional_vite_hosts(hostname) {
                    variables.push((
                        OsString::from("__VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS"),
                        value,
                    ));
                }
            }
            Self::Next | Self::Nitro => {}
        }
        match self {
            Self::Nuxt => {
                variables.push((OsString::from("NUXT_HOST"), OsString::from(&host)));
                variables.push((OsString::from("NUXT_PORT"), OsString::from(&port)));
            }
            Self::Nitro => {
                variables.push((OsString::from("NITRO_HOST"), OsString::from(&host)));
                variables.push((OsString::from("NITRO_PORT"), OsString::from(&port)));
            }
            Self::Next => {
                variables.push((OsString::from("HOSTNAME"), OsString::from(&host)));
            }
            Self::Vite | Self::Astro => {}
        }
        variables
    }

    /// Append host/port flags when they are not already present.
    pub(crate) fn inject_argv(
        self,
        mut argv: Vec<OsString>,
        port: u16,
        bind_address: IpAddr,
        hostname: &str,
        strict_port: bool,
    ) -> Vec<OsString> {
        let flags = self.flags(port, bind_address, hostname, strict_port);
        let missing: Vec<_> = flags
            .into_iter()
            .filter(|(names, _)| !has_option(&argv, names))
            .collect();
        if missing.is_empty() {
            return argv;
        }
        if let Some(index) = npm_exec_separator_index(&argv) {
            argv.insert(index, OsString::from("--"));
        } else if needs_package_script_separator(&argv) && !has_exact(&argv, "--") {
            argv.push(OsString::from("--"));
        }
        for (_, arguments) in missing {
            argv.extend(arguments);
        }
        argv
    }

    fn flags(
        self,
        port: u16,
        bind_address: IpAddr,
        hostname: &str,
        strict_port: bool,
    ) -> Vec<(&'static [&'static str], Vec<OsString>)> {
        let host = bind_address.to_string();
        let port = port.to_string();
        let mut flags = Vec::new();
        match self {
            Self::Vite => {
                flags.push((
                    &["--host"][..],
                    vec![OsString::from("--host"), OsString::from(&host)],
                ));
                flags.push((
                    &["--port"][..],
                    vec![OsString::from("--port"), OsString::from(&port)],
                ));
                if strict_port {
                    flags.push((
                        &["--strictPort", "--strict-port"][..],
                        vec![OsString::from("--strictPort")],
                    ));
                }
            }
            Self::Nuxt | Self::Nitro => {
                flags.push((
                    &["--host"][..],
                    vec![OsString::from("--host"), OsString::from(&host)],
                ));
                flags.push((
                    &["--port", "-p"][..],
                    vec![OsString::from("--port"), OsString::from(&port)],
                ));
            }
            Self::Next => {
                flags.push((
                    &["--hostname", "-H"][..],
                    vec![OsString::from("--hostname"), OsString::from(&host)],
                ));
                flags.push((
                    &["--port", "-p"][..],
                    vec![OsString::from("--port"), OsString::from(&port)],
                ));
            }
            Self::Astro => {
                flags.push((
                    &["--host"][..],
                    vec![OsString::from("--host"), OsString::from(&host)],
                ));
                flags.push((
                    &["--port"][..],
                    vec![OsString::from("--port"), OsString::from(&port)],
                ));
                flags.push((
                    &["--allowed-hosts"][..],
                    vec![OsString::from("--allowed-hosts"), OsString::from(hostname)],
                ));
            }
        }
        flags
    }
}

impl FrameworkChoice {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "vite" => Ok(Self::Forced(Framework::Vite)),
            "nuxt" => Ok(Self::Forced(Framework::Nuxt)),
            "next" => Ok(Self::Forced(Framework::Next)),
            "nitro" => Ok(Self::Forced(Framework::Nitro)),
            "astro" => Ok(Self::Forced(Framework::Astro)),
            "none" => Ok(Self::Disabled),
            _ => Err(format!(
                "unknown framework `{value}`; expected vite, nuxt, next, nitro, astro, or none"
            )),
        }
    }

    pub(crate) fn resolve(self, argv: &[OsString], directory: &Path) -> Option<Framework> {
        match self {
            Self::Disabled => None,
            Self::Forced(framework) => Some(framework),
            Self::Auto => detect(argv, directory),
        }
    }
}

fn additional_vite_hosts(hostname: &str) -> Option<OsString> {
    match env::var_os("__VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS") {
        Some(existing) => Some(merge_allowed_hosts(&existing, hostname)?),
        None => Some(OsString::from(hostname)),
    }
}

fn merge_allowed_hosts(existing: &OsStr, hostname: &str) -> Option<OsString> {
    let existing = existing.to_str()?;
    let trimmed = existing.trim();
    if trimmed.is_empty() {
        return Some(OsString::from(hostname));
    }
    if trimmed.split(',').any(|host| host.trim() == hostname) {
        Some(OsString::from(existing))
    } else {
        Some(OsString::from(format!("{existing},{hostname}")))
    }
}

fn detect(argv: &[OsString], directory: &Path) -> Option<Framework> {
    if let Some(framework) = detect_from_argv(argv) {
        return Some(framework);
    }
    detect_from_package_script(argv, directory)
}

fn detect_from_argv(argv: &[OsString]) -> Option<Framework> {
    let program = executable_name(argv.first()?)?;
    if let Some(framework) = Framework::from_program(&program) {
        return Some(framework);
    }
    Framework::from_program(&wrapper_package(argv)?)
}

fn wrapper_package(argv: &[OsString]) -> Option<String> {
    let program = executable_name(argv.first()?)?;
    let rest = argv.get(1..).unwrap_or(&[]);
    match program.as_str() {
        "npx" | "bunx" | "pnpx" => first_operand(rest),
        "npm" | "pnpm" | "yarn" | "bun" => {
            let (subcommand, remaining) = split_first_operand(rest)?;
            match subcommand.as_str() {
                "exec" | "dlx" | "x" => first_operand(remaining),
                _ => None,
            }
        }
        _ => None,
    }
}

fn first_operand(arguments: &[OsString]) -> Option<String> {
    split_first_operand(arguments).map(|(name, _)| name)
}

fn split_first_operand(arguments: &[OsString]) -> Option<(String, &[OsString])> {
    let index = next_operand_index(arguments)?;
    let name = executable_name(&arguments[index])?;
    Some((name, &arguments[index + 1..]))
}

fn next_operand_index(arguments: &[OsString]) -> Option<usize> {
    let mut skip_value = false;
    let mut after_separator = false;
    for (index, argument) in arguments.iter().enumerate() {
        let Some(value) = argument.to_str() else {
            continue;
        };
        if skip_value {
            skip_value = false;
            continue;
        }
        if !after_separator {
            if value == "--" {
                after_separator = true;
                continue;
            }
            if value.starts_with("--package=") {
                continue;
            }
            if value.starts_with('-') {
                if matches!(value, "--package" | "-p" | "-c" | "--call") {
                    skip_value = true;
                }
                continue;
            }
        }
        return Some(index);
    }
    None
}

fn npm_exec_separator_index(argv: &[OsString]) -> Option<usize> {
    let program = executable_name(argv.first()?)?;
    if program != "npm" || has_exact(argv, "--") {
        return None;
    }
    let exec_index = next_operand_index(&argv[1..])? + 1;
    let subcommand = executable_name(&argv[exec_index])?;
    if !matches!(subcommand.as_str(), "exec" | "x") {
        return None;
    }
    next_operand_index(&argv[exec_index + 1..]).map(|index| index + exec_index + 1)
}

fn detect_from_package_script(argv: &[OsString], directory: &Path) -> Option<Framework> {
    let script = package_script_name(argv)?;
    let contents = fs::read_to_string(directory.join("package.json")).ok()?;
    let package: serde_json::Value = serde_json::from_str(&contents).ok()?;
    let command = package.get("scripts")?.get(&script)?.as_str()?;
    detect_from_script(command)
}

fn detect_from_script(command: &str) -> Option<Framework> {
    if has_shell_control(command) {
        return None;
    }
    let tokens: Vec<OsString> = command
        .split_whitespace()
        .filter(|token| !is_env_assignment(token))
        .map(OsString::from)
        .collect();
    detect_from_argv(&tokens)
}

fn has_shell_control(command: &str) -> bool {
    command.contains("&&")
        || command.contains("||")
        || command.contains("$(")
        || command.contains('|')
        || command.contains(';')
        || command.contains('`')
        || command.contains('&')
}

fn is_env_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn package_script_name(argv: &[OsString]) -> Option<String> {
    let program = executable_name(argv.first()?)?;
    if !matches!(program.as_str(), "npm" | "pnpm" | "yarn" | "bun") {
        return None;
    }
    let mut saw_run = false;
    for argument in argv.iter().skip(1) {
        let Some(value) = argument.to_str() else {
            continue;
        };
        if value == "--" {
            break;
        }
        if value == "run" || value == "run-script" {
            saw_run = true;
            continue;
        }
        if !saw_run || value.starts_with('-') {
            continue;
        }
        return Some(value.to_owned());
    }
    None
}

fn needs_package_script_separator(argv: &[OsString]) -> bool {
    let Some(program) = argv.first().and_then(|argument| executable_name(argument)) else {
        return false;
    };
    matches!(program.as_str(), "npm") && package_script_name(argv).is_some()
}

fn executable_name(argument: &OsStr) -> Option<String> {
    let name = Path::new(argument).file_name()?.to_str()?;
    let lower = name.to_ascii_lowercase();
    Some(
        lower
            .strip_suffix(".exe")
            .or_else(|| lower.strip_suffix(".cmd"))
            .or_else(|| lower.strip_suffix(".bat"))
            .or_else(|| lower.strip_suffix(".ps1"))
            .unwrap_or(&lower)
            .to_owned(),
    )
}

fn has_option(argv: &[OsString], names: &[&str]) -> bool {
    names.iter().any(|name| has_named_option(argv, name))
}

fn has_named_option(argv: &[OsString], name: &str) -> bool {
    let prefix = format!("{name}=");
    argv.iter().any(|argument| {
        argument
            .to_str()
            .is_some_and(|value| value == name || value.starts_with(&prefix))
    })
}

fn has_exact(argv: &[OsString], value: &str) -> bool {
    argv.iter()
        .any(|argument| argument.to_str().is_some_and(|current| current == value))
}

#[cfg(test)]
mod tests {
    use super::{Framework, FrameworkChoice, detect, executable_name, merge_allowed_hosts};
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::net::{IpAddr, Ipv4Addr};
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn nowhere() -> &'static Path {
        Path::new("/nook-framework-no-package")
    }

    fn argv(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn bind() -> IpAddr {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    }

    fn temporary_directory() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("nook-framework-{}-{}", std::process::id(), unique));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn detects_framework_binaries_from_argv() {
        assert_eq!(detect(&argv(&["vite"]), nowhere()), Some(Framework::Vite));
        assert_eq!(
            detect(&argv(&["npx", "--yes", "nuxt", "dev"]), nowhere()),
            Some(Framework::Nuxt)
        );
        assert_eq!(
            detect(&argv(&["bunx", "next", "dev"]), nowhere()),
            Some(Framework::Next)
        );
        assert_eq!(
            detect(&argv(&["pnpm", "exec", "vite"]), nowhere()),
            Some(Framework::Vite)
        );
        assert_eq!(
            detect(&argv(&["bun", "x", "astro"]), nowhere()),
            Some(Framework::Astro)
        );
        assert_eq!(
            detect(&argv(&["astro", "dev"]), nowhere()),
            Some(Framework::Astro)
        );
        assert_eq!(
            detect(&argv(&["nitro", "dev"]), nowhere()),
            Some(Framework::Nitro)
        );
        assert_eq!(
            detect(&argv(&["node_modules/.bin/nuxi"]), nowhere()),
            Some(Framework::Nuxt)
        );
    }

    #[test]
    fn prefers_specific_frameworks_over_vite() {
        assert_eq!(
            detect(&argv(&["nuxt", "dev", "--", "vite"]), nowhere()),
            Some(Framework::Nuxt)
        );
    }

    #[test]
    fn does_not_infer_from_unrelated_commands() {
        assert_eq!(
            detect(&argv(&["bun", "--watch", "src/server.ts"]), nowhere()),
            None
        );
        assert_eq!(
            detect(&argv(&["python3", "app.py", "next"]), nowhere()),
            None
        );
        assert_eq!(detect(&argv(&["npm", "run", "next"]), nowhere()), None);
        assert_eq!(
            detect(
                &argv(&["npx", "--package=vite", "--", "node", "server.js"]),
                nowhere()
            ),
            None
        );
        assert_eq!(
            detect(&argv(&["npx", "--package=vite", "vite"]), nowhere()),
            Some(Framework::Vite)
        );
    }

    #[test]
    fn detects_package_manager_run_scripts() {
        let directory = temporary_directory();
        fs::write(
            directory.join("package.json"),
            r#"{"scripts":{"dev":"nuxt dev","front":"vite"}}"#,
        )
        .unwrap();
        assert_eq!(
            detect(&argv(&["bun", "run", "dev"]), &directory),
            Some(Framework::Nuxt)
        );
        assert_eq!(
            detect(&argv(&["npm", "run", "--silent", "front"]), &directory),
            Some(Framework::Vite)
        );
        assert_eq!(detect(&argv(&["pnpm", "dev"]), &directory), None);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_compound_package_scripts() {
        let directory = temporary_directory();
        fs::write(
            directory.join("package.json"),
            r#"{"scripts":{
                "dev":"vite && node server.js",
                "echo":"echo vite",
                "prod":"NODE_ENV=production nuxt dev",
                "wrapped":"npx vite"
            }}"#,
        )
        .unwrap();
        assert_eq!(detect(&argv(&["npm", "run", "dev"]), &directory), None);
        assert_eq!(detect(&argv(&["npm", "run", "echo"]), &directory), None);
        assert_eq!(
            detect(&argv(&["npm", "run", "prod"]), &directory),
            Some(Framework::Nuxt)
        );
        assert_eq!(
            detect(&argv(&["npm", "run", "wrapped"]), &directory),
            Some(Framework::Vite)
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn disabled_choice_skips_detection() {
        let directory = temporary_directory();
        fs::write(
            directory.join("package.json"),
            r#"{"scripts":{"dev":"vite"}}"#,
        )
        .unwrap();
        assert_eq!(
            FrameworkChoice::Disabled.resolve(&argv(&["bun", "run", "dev"]), &directory),
            None
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn injects_missing_flags_and_skips_duplicates() {
        let injected =
            Framework::Vite.inject_argv(argv(&["vite"]), 5173, bind(), "app.localhost", true);
        assert_eq!(
            injected,
            argv(&[
                "vite",
                "--host",
                "127.0.0.1",
                "--port",
                "5173",
                "--strictPort"
            ])
        );
        let already = Framework::Vite.inject_argv(
            argv(&["vite", "--host", "0.0.0.0", "--port=4000"]),
            5173,
            bind(),
            "app.localhost",
            true,
        );
        assert_eq!(
            already,
            argv(&["vite", "--host", "0.0.0.0", "--port=4000", "--strictPort"])
        );
    }

    #[test]
    #[cfg(unix)]
    fn package_script_name_skips_non_utf8_arguments() {
        use std::os::unix::ffi::OsStringExt;

        let mut command = argv(&["npm", "run"]);
        command.push(OsString::from_vec(vec![0xff]));
        command.push(OsString::from("dev"));
        let injected = Framework::Vite.inject_argv(command, 5173, bind(), "app.localhost", false);
        assert_eq!(injected[4], "--");
        assert_eq!(injected[5], "--host");
    }

    #[test]
    fn npm_exec_inserts_a_separator_before_the_package() {
        assert_eq!(
            Framework::Vite.inject_argv(
                argv(&["npm", "exec", "vite"]),
                5173,
                bind(),
                "app.localhost",
                false
            ),
            argv(&[
                "npm",
                "exec",
                "--",
                "vite",
                "--host",
                "127.0.0.1",
                "--port",
                "5173"
            ])
        );
        assert_eq!(
            Framework::Vite.inject_argv(
                argv(&["npm", "x", "--", "vite"]),
                5173,
                bind(),
                "app.localhost",
                false
            ),
            argv(&[
                "npm",
                "x",
                "--",
                "vite",
                "--host",
                "127.0.0.1",
                "--port",
                "5173"
            ])
        );
    }

    #[test]
    fn npm_run_inserts_a_separator_before_flags() {
        let injected = Framework::Nuxt.inject_argv(
            argv(&["npm", "run", "dev"]),
            3000,
            bind(),
            "app.localhost",
            false,
        );
        assert_eq!(
            injected,
            argv(&[
                "npm",
                "run",
                "dev",
                "--",
                "--host",
                "127.0.0.1",
                "--port",
                "3000"
            ])
        );
    }

    #[test]
    fn pnpm_and_yarn_scripts_receive_flags_without_a_separator() {
        assert_eq!(
            Framework::Vite.inject_argv(
                argv(&["pnpm", "run", "dev"]),
                5173,
                bind(),
                "app.localhost",
                false
            ),
            argv(&[
                "pnpm",
                "run",
                "dev",
                "--host",
                "127.0.0.1",
                "--port",
                "5173"
            ])
        );
        assert_eq!(
            Framework::Vite.inject_argv(
                argv(&["yarn", "run", "dev"]),
                5173,
                bind(),
                "app.localhost",
                false
            ),
            argv(&[
                "yarn",
                "run",
                "dev",
                "--host",
                "127.0.0.1",
                "--port",
                "5173"
            ])
        );
        assert_eq!(
            Framework::Nuxt.inject_argv(
                argv(&["bun", "run", "dev"]),
                3000,
                bind(),
                "app.localhost",
                false
            ),
            argv(&["bun", "run", "dev", "--host", "127.0.0.1", "--port", "3000"])
        );
    }

    #[test]
    fn next_and_astro_use_their_cli_flags() {
        assert_eq!(
            Framework::Next.inject_argv(
                argv(&["next", "dev"]),
                3000,
                bind(),
                "app.localhost",
                false
            ),
            argv(&["next", "dev", "--hostname", "127.0.0.1", "--port", "3000"])
        );
        assert_eq!(
            Framework::Astro.inject_argv(
                argv(&["astro", "dev"]),
                4321,
                bind(),
                "docs.localhost",
                false
            ),
            argv(&[
                "astro",
                "dev",
                "--host",
                "127.0.0.1",
                "--port",
                "4321",
                "--allowed-hosts",
                "docs.localhost"
            ])
        );
    }

    #[test]
    fn merge_allowed_hosts_appends_without_duplicating() {
        assert_eq!(
            merge_allowed_hosts(OsStr::new("staging.example.com"), "app.localhost")
                .unwrap()
                .to_str()
                .unwrap(),
            "staging.example.com,app.localhost"
        );
        assert_eq!(
            merge_allowed_hosts(OsStr::new("app.localhost,other.localhost"), "app.localhost")
                .unwrap()
                .to_str()
                .unwrap(),
            "app.localhost,other.localhost"
        );
        assert_eq!(
            merge_allowed_hosts(OsStr::new("  "), "app.localhost")
                .unwrap()
                .to_str()
                .unwrap(),
            "app.localhost"
        );
    }

    #[test]
    fn environment_covers_vite_nuxt_and_next() {
        let vite = Framework::Vite.environment(5173, bind(), "app.localhost");
        assert!(vite.iter().any(|(key, value)| {
            key == "__VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS" && value == "app.localhost"
        }));
        let nuxt = Framework::Nuxt.environment(3000, bind(), "app.localhost");
        assert!(
            nuxt.iter()
                .any(|(key, value)| key == "NUXT_PORT" && value == "3000")
        );
        let next = Framework::Next.environment(3000, bind(), "app.localhost");
        assert!(
            next.iter()
                .any(|(key, value)| key == "HOSTNAME" && value == "127.0.0.1")
        );
    }

    #[test]
    fn parses_known_framework_names() {
        assert_eq!(
            FrameworkChoice::parse("astro").unwrap(),
            FrameworkChoice::Forced(Framework::Astro)
        );
        assert_eq!(
            FrameworkChoice::parse("none").unwrap(),
            FrameworkChoice::Disabled
        );
        assert!(FrameworkChoice::parse("angular").is_err());
    }

    #[test]
    fn strips_windows_executable_suffixes() {
        assert_eq!(
            executable_name(OsStr::new("vite.CMD")).as_deref(),
            Some("vite")
        );
        assert_eq!(
            executable_name(OsStr::new("next.exe")).as_deref(),
            Some("next")
        );
    }
}
