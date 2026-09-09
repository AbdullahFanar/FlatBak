//! Thin wrapper around the `flatpak` command line tool.
//!
//! FlatBak drives the CLI rather than linking libflatpak: the CLI is stable,
//! already handles authorisation via polkit for system installations, and works
//! unchanged when FlatBak itself runs inside a sandbox.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::error::{Cancelled, FlatpakUnavailable};
use crate::flatpak::model::{InstalledApp, Installation, Remote};
use crate::util::{parse_size, Cancel};

/// How to invoke `flatpak`.
///
/// When FlatBak runs inside a sandbox or container it must reach the *host's*
/// Flatpak installation, not the one inside the container (which typically has
/// none). The command prefix is resolved once at startup.
#[derive(Debug, Clone)]
pub struct Flatpak {
    prefix: Vec<String>,
}

impl Default for Flatpak {
    fn default() -> Self {
        Self::detect()
    }
}

impl Flatpak {
    /// Works out how to reach a usable `flatpak`.
    ///
    /// `FLATBAK_FLATPAK_CMD` overrides the detection entirely and is split on
    /// whitespace, so `FLATBAK_FLATPAK_CMD="sudo flatpak"` works.
    pub fn detect() -> Self {
        if let Ok(custom) = std::env::var("FLATBAK_FLATPAK_CMD") {
            let prefix: Vec<String> = custom.split_whitespace().map(str::to_owned).collect();
            if !prefix.is_empty() {
                return Self { prefix };
            }
        }

        // Running as a Flatpak ourselves: escape the sandbox via the portal.
        if Path::new("/.flatpak-info").exists() {
            return Self {
                prefix: vec![
                    "flatpak-spawn".to_owned(),
                    "--host".to_owned(),
                    "flatpak".to_owned(),
                ],
            };
        }

        // Running inside a toolbox/distrobox container: it shares the user's
        // home directory but has its own /var, so the host's flatpak is the one
        // that knows about the user's applications. This is checked before
        // falling back to a plain `flatpak`, because a container may well have
        // the command installed over an empty installation of its own.
        if in_container() {
            if which("distrobox-host-exec").is_some() {
                return Self {
                    prefix: vec!["distrobox-host-exec".to_owned(), "flatpak".to_owned()],
                };
            }
            if which("flatpak-spawn").is_some() {
                return Self {
                    prefix: vec![
                        "flatpak-spawn".to_owned(),
                        "--host".to_owned(),
                        "flatpak".to_owned(),
                    ],
                };
            }
        }

        Self {
            prefix: vec!["flatpak".to_owned()],
        }
    }

    /// The command prefix, for display in diagnostics.
    pub fn command_line(&self) -> String {
        self.prefix.join(" ")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.prefix[0]);
        command.args(&self.prefix[1..]);
        command.args(args);
        // Keep flatpak's output stable and parseable regardless of the user's
        // locale, and stop it from trying to draw progress bars.
        command.env("LC_ALL", "C");
        command.env("FLATPAK_FANCY_OUTPUT", "0");
        command.stdin(Stdio::null());
        command
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let output = self
            .command(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    anyhow!(FlatpakUnavailable::NotFound)
                } else {
                    anyhow!(FlatpakUnavailable::Failed(error.to_string()))
                }
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let detail = if stderr.is_empty() {
                format!("flatpak {} exited with {}", args.join(" "), output.status)
            } else {
                stderr
            };
            return Err(anyhow!(detail));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Returns the version string reported by `flatpak --version`.
    ///
    /// This doubles as the availability probe run at startup.
    pub fn version(&self) -> Result<String> {
        let raw = self.run(&["--version"])?;
        Ok(raw
            .trim()
            .strip_prefix("Flatpak ")
            .unwrap_or(raw.trim())
            .to_owned())
    }

    /// Lists installed applications across every installation.
    ///
    /// Runtimes are excluded: they are reinstalled automatically as
    /// dependencies of the applications that need them.
    pub fn list_apps(&self) -> Result<Vec<InstalledApp>> {
        // `:f` suppresses ellipsization so long values survive intact.
        const COLUMNS: &str = "application:f,name:f,version:f,branch:f,arch:f,\
                               origin:f,installation:f,ref:f,active:f,size:f";
        let raw = self
            .run(&["list", "--app", &format!("--columns={COLUMNS}")])
            .context("Could not list installed Flatpak applications")?;

        let mut apps = Vec::new();
        for line in raw.lines() {
            let line = line.trim_end_matches('\r');
            if line.trim().is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() < 10 {
                // Unexpected shape; skip the row rather than mis-attributing
                // fields to the wrong column.
                continue;
            }
            let clean = |value: &str| {
                let value = value.trim();
                if value == "-" {
                    String::new()
                } else {
                    value.to_owned()
                }
            };
            let id = clean(fields[0]);
            if id.is_empty() {
                continue;
            }
            apps.push(InstalledApp {
                id,
                name: clean(fields[1]),
                version: clean(fields[2]),
                branch: clean(fields[3]),
                arch: clean(fields[4]),
                origin: clean(fields[5]),
                installation: Installation::parse(fields[6]),
                reference: clean(fields[7]),
                commit: clean(fields[8]),
                installed_size: parse_size(fields[9]),
            });
        }
        apps.sort_by(|a, b| {
            a.display_name()
                .to_lowercase()
                .cmp(&b.display_name().to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(apps)
    }

    /// Lists configured remotes across every installation.
    pub fn list_remotes(&self) -> Result<Vec<Remote>> {
        const COLUMNS: &str = "name:f,title:f,url:f,options:f";
        let raw = self
            .run(&["remotes", &format!("--columns={COLUMNS}")])
            .context("Could not list Flatpak remotes")?;

        let mut remotes = Vec::new();
        for line in raw.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() < 4 {
                continue;
            }
            let options: Vec<&str> = fields[3].split(',').map(str::trim).collect();
            let installation = if options.contains(&"user") {
                Installation::User
            } else {
                Installation::System
            };
            remotes.push(Remote {
                name: fields[0].trim().to_owned(),
                title: fields[1].trim().to_owned(),
                url: fields[2].trim().to_owned(),
                installation,
                disabled: options.contains(&"disabled"),
            });
        }
        Ok(remotes)
    }

    /// Adds a remote, so a restore can proceed when the original is missing.
    pub fn add_remote(
        &self,
        name: &str,
        url: &str,
        installation: &Installation,
    ) -> Result<()> {
        let scope = installation.cli_args();
        let mut args: Vec<&str> = vec!["remote-add", "--if-not-exists"];
        for flag in &scope {
            args.push(flag);
        }
        args.push(name);
        args.push(url);
        self.run(&args)
            .with_context(|| format!("Could not add the remote \u{201c}{name}\u{201d}"))?;
        Ok(())
    }

    /// Installs one application, streaming flatpak's own output to `on_output`.
    ///
    /// `--or-update` makes this idempotent: restoring onto a system that already
    /// has some of the applications updates them instead of failing with
    /// "already installed", so a restore can safely be run twice.
    ///
    /// Returns `Err(Cancelled)` if `cancel` is tripped; the child process is
    /// killed so a partial download does not keep running in the background.
    pub fn install(
        &self,
        remote: &str,
        reference: &str,
        installation: &Installation,
        cancel: &Cancel,
        mut on_output: impl FnMut(&str),
    ) -> Result<()> {
        let scope = installation.cli_args();
        let mut args: Vec<&str> = vec!["install", "--noninteractive", "--or-update"];
        for flag in &scope {
            args.push(flag);
        }
        args.push(remote);
        args.push(reference);

        let mut child = self
            .command(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    anyhow!(FlatpakUnavailable::NotFound)
                } else {
                    anyhow!(FlatpakUnavailable::Failed(error.to_string()))
                }
            })?;

        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        // Collect stderr on a helper thread so a chatty flatpak cannot fill the
        // pipe buffer and deadlock while we are reading stdout.
        let stderr_thread = std::thread::spawn(move || {
            let mut collected = Vec::new();
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                collected.push(line);
            }
            collected
        });

        // A watchdog kills the child promptly on cancellation instead of waiting
        // for flatpak to emit its next line of output.
        let child = Arc::new(Mutex::new(child));
        let watchdog = {
            let child = Arc::clone(&child);
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                while !cancel.is_cancelled() {
                    // `try_wait` reaps nothing but tells us when to stop polling.
                    match child.lock().expect("child mutex").try_wait() {
                        Ok(Some(_)) | Err(_) => return,
                        Ok(None) => {}
                    }
                    std::thread::sleep(Duration::from_millis(150));
                }
                let _ = child.lock().expect("child mutex").kill();
            })
        };

        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let line = line.trim().to_owned();
            if !line.is_empty() {
                on_output(&line);
            }
        }

        let status = child.lock().expect("child mutex").wait()?;
        let stderr_lines = stderr_thread.join().unwrap_or_default();
        let _ = watchdog.join();

        if cancel.is_cancelled() {
            return Err(anyhow!(Cancelled));
        }
        if !status.success() {
            let message = stderr_lines
                .iter()
                .rev()
                .find(|line| !line.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| format!("flatpak install exited with {status}"));
            return Err(anyhow!(message));
        }
        Ok(())
    }
}

/// True when this process appears to be inside a container.
///
/// The markers differ by runtime: podman and toolbox drop `/run/.containerenv`,
/// Docker drops `/.dockerenv`, and distrobox exports `CONTAINER_ID` regardless
/// of which of the two is underneath it. Any one of them is enough.
fn in_container() -> bool {
    ["/run/.containerenv", "/run/.toolboxenv", "/.dockerenv"]
        .iter()
        .any(|marker| Path::new(marker).exists())
        || std::env::var_os("CONTAINER_ID").is_some()
        || std::env::var_os("DISTROBOX_ENTER_PATH").is_some()
        || std::env::var_os("container").is_some()
}

/// Minimal `which`, to avoid a dependency for two call sites.
fn which(program: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}
