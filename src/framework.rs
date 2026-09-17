//! Align Vite, Nuxt, and Astro when the child argv *is* the framework CLI.
//!
//! `bun run` / `npm run` only inherit `PORT` / `HOST` / `NOOK_URL`. Put the
//! raw CLI in the script (`nook-run -- vite`, `nook-run -- nuxt dev`).
//! Elysia (`bun --watch src/server.ts`) is not a frontend framework.

use std::env;
use std::ffi::{OsStr, OsString};
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
        match strip_package_version(name) {
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

    /// Append host/port flags on the framework CLI only, not on `bun run`.
    pub(crate) fn inject_argv(
        self,
        mut argv: Vec<OsString>,
        port: u16,
        bind_address: IpAddr,
        hostname: &str,
        strict_port: bool,
    ) -> Vec<OsString> {
        let Some(index) = framework_executable_index(&argv) else {
            return argv;
        };
        if !serving_invocation(self, &argv[index + 1..]) {
            return argv;
        }
        if let Some(separator) = npm_exec_separator_index(&argv) {
            argv.insert(separator, OsString::from("--"));
            let package = separator + 1;
            if let Some(offset) = argv
                .iter()
                .skip(package + 1)
                .position(|argument| argument == "--")
            {
                argv.remove(package + 1 + offset);
            }
        }
        let index = framework_executable_index(&argv).unwrap_or(index);
        let start = index + 1;
        let end = args_end(&argv, start);
        let flags = self.flags(port, bind_address, hostname, strict_port);
        let missing: Vec<_> = flags
            .into_iter()
            .filter(|(names, arguments)| {
                if is_port_option(names) || is_host_option(names) {
                    return !overwrite_option(&mut argv[start..end], names, arguments.last());
                }
                !has_option(&argv[start..end], names)
            })
            .collect();
        let insert_at = args_end(&argv, start);
        let mut inserted = 0;
        for (_, arguments) in missing {
            for argument in arguments {
                argv.insert(insert_at + inserted, argument);
                inserted += 1;
            }
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
                    &["--port", "-p"][..],
                    vec![OsString::from("--port"), OsString::from(&port)],
                ));
                if strict_port {
                    flags.push((
                        &["--strictPort", "--strict-port"][..],
                        vec![OsString::from("--strictPort")],
                    ));
                }
            }
            Self::Nuxt => {
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
            Self::Nitro => {
                flags.push((
                    &["--host"][..],
                    vec![OsString::from("--host"), OsString::from(&host)],
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
                    &["--port", "-p"][..],
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

    pub(crate) fn resolve(self, argv: &[OsString]) -> Option<Framework> {
        match self {
            Self::Disabled => None,
            Self::Forced(framework) => Some(framework),
            Self::Auto => detect(argv),
        }
    }
}

fn additional_vite_hosts(hostname: &str) -> Option<OsString> {
    vite_hosts_override(
        env::var_os("__VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS").as_deref(),
        hostname,
    )
}

fn vite_hosts_override(existing: Option<&OsStr>, hostname: &str) -> Option<OsString> {
    match existing {
        Some(value) if !value.is_empty() => None,
        _ => Some(OsString::from(hostname)),
    }
}

fn detect(argv: &[OsString]) -> Option<Framework> {
    let index = framework_executable_index(argv)?;
    let name = executable_name(&argv[index])?;
    let framework = Framework::from_program(&name)?;
    serving_invocation(framework, &argv[index + 1..]).then_some(framework)
}

fn serving_invocation(framework: Framework, arguments: &[OsString]) -> bool {
    match first_subcommand(arguments).as_deref() {
        None => matches!(framework, Framework::Vite),
        Some("dev" | "start" | "preview" | "serve") => true,
        Some("build" | "optimize" | "check" | "lint" | "generate") => false,
        Some(_) => matches!(framework, Framework::Vite),
    }
}

fn first_subcommand(arguments: &[OsString]) -> Option<String> {
    let mut skip_value = false;
    for argument in arguments {
        let Some(value) = argument.to_str() else {
            continue;
        };
        if skip_value {
            skip_value = false;
            continue;
        }
        if value == "--" {
            break;
        }
        if value.starts_with('-') {
            if !value.contains('=') && flag_takes_value(value) {
                skip_value = true;
            }
            continue;
        }
        return Some(value.to_owned());
    }
    None
}

fn flag_takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "--port"
            | "-p"
            | "--host"
            | "--hostname"
            | "-H"
            | "--allowed-hosts"
            | "--mode"
            | "--config"
            | "-c"
            | "--filter"
            | "--package"
            | "--dir"
            | "--cwd"
            | "--prefix"
            | "--call"
    )
}

fn wrapper_flag_takes_value(program: &str, flag: &str) -> bool {
    match program {
        "npx" | "npm" => flag_takes_value(flag) || matches!(flag, "-w" | "--workspace" | "--call"),
        "pnpm" | "pnpx" => {
            flag_takes_value(flag) || matches!(flag, "--dir" | "-C" | "--filter" | "--package")
        }
        "yarn" | "bun" | "bunx" => {
            matches!(flag, "--cwd" | "--package" | "-p" | "--filter" | "-F")
        }
        _ => flag_takes_value(flag),
    }
}

fn framework_executable_index(argv: &[OsString]) -> Option<usize> {
    let program = executable_name(argv.first()?)?;
    if Framework::from_program(&program).is_some() {
        return Some(0);
    }
    let rest = argv.get(1..)?;
    match program.as_str() {
        "npx" | "bunx" | "pnpx" => next_operand_index(&program, rest).map(|index| index + 1),
        "npm" | "pnpm" | "yarn" | "bun" => {
            let exec_index = next_operand_index(&program, rest)? + 1;
            let subcommand = executable_name(&argv[exec_index])?;
            match subcommand.as_str() {
                "exec" | "dlx" | "x" => next_operand_index(&program, &argv[exec_index + 1..])
                    .map(|index| index + exec_index + 1),
                _ => None,
            }
        }
        _ => None,
    }
}

fn next_operand_index(program: &str, arguments: &[OsString]) -> Option<usize> {
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
            if value.starts_with("--package=")
                || value.starts_with("--workspace=")
                || value.starts_with("--dir=")
                || value.starts_with("--cwd=")
                || value.starts_with("--filter=")
            {
                continue;
            }
            if value.starts_with('-') {
                if wrapper_flag_takes_value(program, value) {
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
    if program != "npm" {
        return None;
    }
    let exec_index = next_operand_index("npm", &argv[1..])? + 1;
    let subcommand = executable_name(&argv[exec_index])?;
    if !matches!(subcommand.as_str(), "exec" | "x") {
        return None;
    }
    let package_index = next_operand_index("npm", &argv[exec_index + 1..])? + exec_index + 1;
    if argv[..package_index]
        .iter()
        .any(|argument| argument == "--")
    {
        return None;
    }
    Some(package_index)
}

fn args_end(argv: &[OsString], start: usize) -> usize {
    argv.get(start..)
        .and_then(|arguments| arguments.iter().position(|argument| argument == "--"))
        .map_or(argv.len(), |offset| start + offset)
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

fn strip_package_version(name: &str) -> &str {
    match name.rfind('@') {
        Some(index) if index > 0 => &name[..index],
        _ => name,
    }
}

fn is_port_option(names: &[&str]) -> bool {
    names.iter().any(|name| *name == "--port" || *name == "-p")
}

fn is_host_option(names: &[&str]) -> bool {
    names
        .iter()
        .any(|name| *name == "--host" || *name == "--hostname" || *name == "-H")
}

fn overwrite_option(argv: &mut [OsString], names: &[&str], value: Option<&OsString>) -> bool {
    let Some(value) = value else {
        return false;
    };
    for index in 0..argv.len() {
        let Some(current) = argv[index].to_str() else {
            continue;
        };
        for name in names {
            if current == *name {
                if index + 1 < argv.len()
                    && !argv[index + 1]
                        .to_str()
                        .is_some_and(|next| next.starts_with('-'))
                {
                    argv[index + 1] = value.clone();
                    return true;
                }
                return false;
            }
            let prefix = format!("{name}=");
            if current.starts_with(&prefix) {
                argv[index] = OsString::from(format!("{name}={}", value.to_string_lossy()));
                return true;
            }
        }
    }
    false
}

fn has_option(argv: &[OsString], names: &[&str]) -> bool {
    names.iter().any(|name| {
        let prefix = format!("{name}=");
        argv.iter().any(|argument| {
            argument
                .to_str()
                .is_some_and(|value| value == *name || value.starts_with(&prefix))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::{Framework, FrameworkChoice, detect, executable_name, vite_hosts_override};
    use std::ffi::{OsStr, OsString};
    use std::net::{IpAddr, Ipv4Addr};

    fn argv(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn bind() -> IpAddr {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    }

    #[test]
    fn detects_framework_binaries_and_bunx() {
        assert_eq!(detect(&argv(&["vite"])), Some(Framework::Vite));
        assert_eq!(
            detect(&argv(&["vite", "./frontend"])),
            Some(Framework::Vite)
        );
        assert_eq!(
            detect(&argv(&["bunx", "nuxt", "dev"])),
            Some(Framework::Nuxt)
        );
        assert_eq!(
            detect(&argv(&["bun", "x", "astro", "dev"])),
            Some(Framework::Astro)
        );
        assert_eq!(
            detect(&argv(&["npx", "vite@latest"])),
            Some(Framework::Vite)
        );
        assert_eq!(
            detect(&argv(&["node_modules/.bin/nuxi", "dev"])),
            Some(Framework::Nuxt)
        );
        assert_eq!(detect(&argv(&["astro", "dev"])), Some(Framework::Astro));
        assert_eq!(detect(&argv(&["vite", "build"])), None);
        assert_eq!(detect(&argv(&["nuxt", "build"])), None);
        assert_eq!(
            FrameworkChoice::Forced(Framework::Next).resolve(&argv(&["next", "build"])),
            Some(Framework::Next)
        );
        assert_eq!(
            Framework::Next.inject_argv(
                argv(&["next", "build"]),
                3000,
                bind(),
                "app.localhost",
                false
            ),
            argv(&["next", "build"])
        );
    }

    #[test]
    fn ignores_bun_run_and_elysia() {
        assert_eq!(detect(&argv(&["bun", "run", "dev"])), None);
        assert_eq!(detect(&argv(&["bun", "--watch", "src/server.ts"])), None);
        assert_eq!(detect(&argv(&["python3", "app.py", "next"])), None);
        assert_eq!(detect(&argv(&["npm", "run", "dev"])), None);
    }

    #[test]
    fn disabled_choice_skips_detection() {
        assert_eq!(FrameworkChoice::Disabled.resolve(&argv(&["vite"])), None);
    }

    #[test]
    fn injects_on_the_framework_cli_only() {
        assert_eq!(
            Framework::Vite.inject_argv(argv(&["vite"]), 5173, bind(), "app.localhost", true),
            argv(&[
                "vite",
                "--host",
                "127.0.0.1",
                "--port",
                "5173",
                "--strictPort"
            ])
        );
        assert_eq!(
            Framework::Vite.inject_argv(
                argv(&["bun", "run", "dev"]),
                5173,
                bind(),
                "app.localhost",
                false
            ),
            argv(&["bun", "run", "dev"])
        );
        assert_eq!(
            Framework::Vite.inject_argv(
                argv(&["vite", "--host", "192.168.1.20", "--port", "4000"]),
                5173,
                bind(),
                "app.localhost",
                false
            ),
            argv(&["vite", "--host", "127.0.0.1", "--port", "5173"])
        );
        assert_eq!(
            Framework::Vite.inject_argv(
                argv(&["bunx", "vite", "--port", "4000"]),
                5173,
                bind(),
                "app.localhost",
                false
            ),
            argv(&["bunx", "vite", "--port", "5173", "--host", "127.0.0.1"])
        );
        assert_eq!(
            Framework::Nuxt.inject_argv(
                argv(&["nuxt", "dev"]),
                3000,
                bind(),
                "app.localhost",
                false
            ),
            argv(&["nuxt", "dev", "--host", "127.0.0.1", "--port", "3000"])
        );
        assert_eq!(
            Framework::Vite.inject_argv(
                argv(&["npm", "exec", "vite", "--", "--mode", "test"]),
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
                "--mode",
                "test",
                "--host",
                "127.0.0.1",
                "--port",
                "5173"
            ])
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
    fn npx_package_flag_is_not_rewritten_as_a_port() {
        assert_eq!(
            Framework::Vite.inject_argv(
                argv(&["npx", "-p", "vite", "vite"]),
                5173,
                bind(),
                "app.localhost",
                false
            ),
            argv(&[
                "npx",
                "-p",
                "vite",
                "vite",
                "--host",
                "127.0.0.1",
                "--port",
                "5173"
            ])
        );
    }

    #[test]
    fn environment_covers_vite_and_nuxt() {
        let vite = Framework::Vite.environment(5173, bind(), "app.localhost");
        assert!(vite.iter().any(|(key, value)| {
            key == "__VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS" && value == "app.localhost"
        }));
        let nuxt = Framework::Nuxt.environment(3000, bind(), "app.localhost");
        assert!(
            nuxt.iter()
                .any(|(key, value)| key == "NUXT_PORT" && value == "3000")
        );
        assert!(
            vite_hosts_override(Some(OsStr::new("staging.example.com")), "app.localhost").is_none()
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
