use std::path::PathBuf;

use hoplix_core::{JumpHost, Proxy, ProxyKind, parse_mode};

use crate::target::{Target, split_host_port};

// Constants

pub const MAX_RETRIES: u32 = 10;
pub const MAX_RETRY_DELAY: u32 = 60;
const KIB: u64 = 1024;
const MIB: u64 = 1024 * 1024;
const MAX_RATE: u64 = 1_000_000 * MIB;

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    Sites,
    ExportSites(PathBuf),
    ImportSites(PathBuf),
    List(Target),
    Stat(Target),
    Copy {
        from: Target,
        to: Target,
        recursive: bool,
    },
    MakeDirectory(Target),
    Remove {
        target: Target,
        recursive: bool,
    },
    Move {
        from: Target,
        to: Target,
    },
    Compare {
        left: Target,
        right: Target,
        recursive: bool,
    },
    Sync {
        from: Target,
        to: Target,
        recursive: bool,
    },
    Chmod {
        mode: u32,
        target: Target,
        recursive: bool,
    },
    Find {
        target: Target,
        pattern: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpTarget {
    pub username: String,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Options {
    pub key: Option<PathBuf>,
    pub agent: bool,
    pub jump: Option<JumpTarget>,
    pub jump_key: Option<PathBuf>,
    pub jump_passphrase_env: Option<String>,
    pub jump_password_env: Option<String>,
    pub jump_agent: bool,
    pub proxy: Option<Proxy>,
    pub proxy_user: Option<String>,
    pub proxy_password_env: Option<String>,
    pub passphrase_env: Option<String>,
    pub password_env: Option<String>,
    pub master_password_env: Option<String>,
    pub accept_host_key: bool,
    pub remember_host_key: bool,
    pub known_hosts: Option<PathBuf>,
    pub trusted_certificates: Option<PathBuf>,
    pub resume: bool,
    pub skip_existing: bool,
    pub keep_both: bool,
    pub size_only: bool,
    pub skip_hidden: bool,
    pub exclude: Vec<String>,
    pub mirror: bool,
    pub delete: bool,
    pub dry_run: bool,
    pub retries: Option<u32>,
    pub retry_delay: Option<u32>,
    pub limit_download: Option<u64>,
    pub limit_upload: Option<u64>,
    pub verify: bool,
    pub include_passwords: bool,
    pub folder: Option<String>,
    pub depth: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub command: Command,
    pub options: Options,
}

// Parsing

pub fn parse<I: IntoIterator<Item = String>>(arguments: I) -> Result<Invocation, String> {
    let mut options = Options::default();
    let mut positional = Vec::new();
    let mut recursive = false;
    let mut iterator = arguments.into_iter();
    while let Some(argument) = iterator.next() {
        let mut value = |name: &str| {
            iterator
                .next()
                .filter(|value| !value.starts_with("--"))
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match argument.as_str() {
            "--key" => options.key = Some(PathBuf::from(value("--key")?)),
            "--agent" => options.agent = true,
            "--jump" => options.jump = Some(jump_target(&value("--jump")?)?),
            "--jump-key" => options.jump_key = Some(PathBuf::from(value("--jump-key")?)),
            "--jump-passphrase-env" => {
                options.jump_passphrase_env = Some(value("--jump-passphrase-env")?);
            }
            "--jump-password-env" => {
                options.jump_password_env = Some(value("--jump-password-env")?);
            }
            "--jump-agent" => options.jump_agent = true,
            "--proxy" => options.proxy = Some(proxy_url(&value("--proxy")?)?),
            "--proxy-user" => options.proxy_user = Some(value("--proxy-user")?),
            "--proxy-password-env" => {
                options.proxy_password_env = Some(value("--proxy-password-env")?);
            }
            "--passphrase-env" => options.passphrase_env = Some(value("--passphrase-env")?),
            "--password-env" => options.password_env = Some(value("--password-env")?),
            "--master-password-env" => {
                options.master_password_env = Some(value("--master-password-env")?);
            }
            "--known-hosts" => options.known_hosts = Some(PathBuf::from(value("--known-hosts")?)),
            "--trusted-certificates" => {
                options.trusted_certificates =
                    Some(PathBuf::from(value("--trusted-certificates")?));
            }
            "--accept-host-key" => options.accept_host_key = true,
            "--remember-host-key" => options.remember_host_key = true,
            "--resume" => options.resume = true,
            "--skip-existing" => options.skip_existing = true,
            "--keep-both" => options.keep_both = true,
            "--size-only" => options.size_only = true,
            "--skip-hidden" => options.skip_hidden = true,
            "--exclude" => options.exclude.push(value("--exclude")?),
            "--mirror" => options.mirror = true,
            "--delete" => options.delete = true,
            "--dry-run" => options.dry_run = true,
            "--retries" => {
                options.retries = Some(number("--retries", &value("--retries")?, 0, MAX_RETRIES)?);
            }
            "--retry-delay" => {
                options.retry_delay = Some(number(
                    "--retry-delay",
                    &value("--retry-delay")?,
                    1,
                    MAX_RETRY_DELAY,
                )?);
            }
            "--limit-download" => {
                options.limit_download =
                    Some(rate("--limit-download", &value("--limit-download")?)?);
            }
            "--limit-upload" => {
                options.limit_upload = Some(rate("--limit-upload", &value("--limit-upload")?)?);
            }
            "--verify" => options.verify = true,
            "--include-passwords" => options.include_passwords = true,
            "--folder" => options.folder = Some(value("--folder")?),
            "--depth" => {
                let depth = number("--depth", &value("--depth")?, 1, u32::MAX)?;
                options.depth = Some(usize::try_from(depth).unwrap_or(usize::MAX));
            }
            "-r" | "--recursive" => recursive = true,
            "-h" | "--help" => positional.insert(0, "help".to_string()),
            "-V" | "--version" => positional.insert(0, "version".to_string()),
            "--password" | "--passphrase" | "--master-password" | "--jump-password"
            | "--jump-passphrase" | "--proxy-password" => {
                return Err(format!(
                    "{argument} is not supported, pass the name of an environment variable with {argument}-env"
                ));
            }
            _ if argument.starts_with('-') && argument.len() > 1 => {
                return Err(format!("unknown option: {argument}"));
            }
            _ => positional.push(argument),
        }
    }
    let command = command_from(&positional, recursive)?;
    check_route(&options)?;
    if recursive
        && !matches!(
            command,
            Command::Remove { .. }
                | Command::Copy { .. }
                | Command::Compare { .. }
                | Command::Sync { .. }
                | Command::Chmod { .. }
        )
    {
        return Err("-r only works with get, put, cp, rm, chmod, compare and sync".to_string());
    }
    if options.depth.is_some() && !matches!(command, Command::Find { .. }) {
        return Err("--depth only works with find".to_string());
    }
    let comparing = matches!(command, Command::Compare { .. } | Command::Sync { .. });
    if !comparing && (options.size_only || options.skip_hidden || !options.exclude.is_empty()) {
        return Err(
            "--size-only, --skip-hidden and --exclude only work with compare and sync".to_string(),
        );
    }
    if !matches!(command, Command::Sync { .. })
        && (options.mirror || options.delete || options.dry_run)
    {
        return Err("--mirror, --delete and --dry-run only work with sync".to_string());
    }
    let transferring = matches!(command, Command::Copy { .. } | Command::Sync { .. });
    let tuned = options.retries.is_some()
        || options.retry_delay.is_some()
        || options.limit_download.is_some()
        || options.limit_upload.is_some();
    if tuned && !transferring {
        return Err(
            "--retries, --retry-delay, --limit-download and --limit-upload only work with get, put, cp and sync"
                .to_string(),
        );
    }
    if options.verify && !transferring {
        return Err("--verify only works with get, put, cp and sync".to_string());
    }
    let exchanging = matches!(command, Command::ExportSites(_) | Command::ImportSites(_));
    if options.folder.is_some() && !exchanging {
        return Err("--folder only works with sites export and sites import".to_string());
    }
    if options.include_passwords {
        if !matches!(command, Command::ExportSites(_)) {
            return Err("--include-passwords only works with sites export".to_string());
        }
        if options.password_env.is_none() {
            return Err(
                "--include-passwords needs --password-env <name> with the export password"
                    .to_string(),
            );
        }
    }
    if [options.resume, options.skip_existing, options.keep_both]
        .iter()
        .filter(|flag| **flag)
        .count()
        > 1
    {
        return Err("use only one of --resume, --skip-existing and --keep-both".to_string());
    }
    Ok(Invocation { command, options })
}

fn check_route(options: &Options) -> Result<(), String> {
    if options.agent && (options.key.is_some() || options.password_env.is_some()) {
        return Err("use only one of --agent, --key and --password-env".to_string());
    }
    let jump_methods = [
        options.jump_key.is_some(),
        options.jump_password_env.is_some(),
        options.jump_agent,
    ]
    .iter()
    .filter(|flag| **flag)
    .count();
    match (&options.jump, jump_methods) {
        (None, 0) if options.jump_passphrase_env.is_none() => {}
        (None, _) => {
            return Err(
                "--jump-key, --jump-passphrase-env, --jump-password-env and --jump-agent only work with --jump"
                    .to_string(),
            );
        }
        (Some(_), 1) => {}
        (Some(_), _) => {
            return Err(
                "--jump needs exactly one of --jump-key, --jump-password-env and --jump-agent"
                    .to_string(),
            );
        }
    }
    if options.jump_passphrase_env.is_some() && options.jump_key.is_none() {
        return Err("--jump-passphrase-env only works with --jump-key".to_string());
    }
    let proxy_user = options.proxy_user.is_some();
    let proxy_password = options.proxy_password_env.is_some();
    if (proxy_user || proxy_password) && options.proxy.is_none() {
        return Err("--proxy-user and --proxy-password-env only work with --proxy".to_string());
    }
    if proxy_user != proxy_password {
        return Err("--proxy-user and --proxy-password-env belong together".to_string());
    }
    Ok(())
}

fn jump_target(text: &str) -> Result<JumpTarget, String> {
    let (username, address) = text
        .rsplit_once('@')
        .filter(|(username, _)| !username.is_empty())
        .ok_or_else(|| format!("--jump needs user@host[:port], got {text}"))?;
    let (host, port) = split_host_port(address, JumpHost::DEFAULT_PORT)?;
    if host.is_empty() {
        return Err(format!("missing host in --jump {text}"));
    }
    Ok(JumpTarget {
        username: username.to_string(),
        host,
        port,
    })
}

fn proxy_url(text: &str) -> Result<Proxy, String> {
    let (scheme, rest) = text.split_once("://").ok_or_else(|| {
        format!("--proxy needs socks5://host:port or http://host:port, got {text}")
    })?;
    let kind = match scheme.to_ascii_lowercase().as_str() {
        "socks5" | "socks5h" => ProxyKind::Socks5,
        "http" => ProxyKind::Http,
        _ => return Err(format!("unsupported proxy type: {scheme}")),
    };
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.contains('@') {
        return Err(
            "do not put credentials into --proxy, use --proxy-user and --proxy-password-env"
                .to_string(),
        );
    }
    if authority.contains('/') {
        return Err(format!("invalid proxy address: {text}"));
    }
    let (host, port) = split_host_port(authority, kind.default_port())?;
    if host.is_empty() {
        return Err(format!("missing host in --proxy {text}"));
    }
    Ok(Proxy {
        kind,
        host,
        port,
        credentials: None,
    })
}

fn number(name: &str, text: &str, low: u32, high: u32) -> Result<u32, String> {
    text.parse::<u32>()
        .ok()
        .filter(|value| (low..=high).contains(value))
        .ok_or_else(|| format!("{name} needs a whole number from {low} to {high}"))
}

fn rate(name: &str, text: &str) -> Result<u64, String> {
    let lower = text.trim().to_ascii_lowercase();
    let digits_end = lower
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(lower.len());
    let (digits, unit) = lower.split_at(digits_end);
    let factor = match unit.trim_end_matches("/s") {
        "" | "k" | "kib" => KIB,
        "m" | "mib" => MIB,
        _ => 0,
    };
    digits
        .parse::<u64>()
        .ok()
        .filter(|_| factor > 0)
        .map(|amount| amount.saturating_mul(factor))
        .filter(|bytes| *bytes <= MAX_RATE)
        .ok_or_else(|| {
            format!("{name} needs a rate like 500 (KiB/s), 500K or 2M (MiB/s), 0 for no limit")
        })
}

fn command_from(positional: &[String], recursive: bool) -> Result<Command, String> {
    let Some((name, rest)) = positional.split_first() else {
        return Ok(Command::Help);
    };
    let target = |index: usize| -> Result<Target, String> {
        Target::parse(
            rest.get(index)
                .ok_or_else(|| format!("{name}: missing argument"))?,
        )
    };
    let expect = |count: usize| -> Result<(), String> {
        if rest.len() == count {
            Ok(())
        } else {
            Err(format!(
                "{name}: expected {count} argument(s), got {}",
                rest.len()
            ))
        }
    };
    match name.as_str() {
        "help" => Ok(Command::Help),
        "version" => Ok(Command::Version),
        "sites" => match rest {
            [] => Ok(Command::Sites),
            [action, file] if action == "export" => Ok(Command::ExportSites(PathBuf::from(file))),
            [action, file] if action == "import" => Ok(Command::ImportSites(PathBuf::from(file))),
            [action] if action == "export" || action == "import" => {
                Err(format!("sites {action}: missing file name"))
            }
            _ => Err("sites: expected nothing, export <file> or import <file>".to_string()),
        },
        "ls" => expect(1).and_then(|()| Ok(Command::List(target(0)?))),
        "stat" => expect(1).and_then(|()| Ok(Command::Stat(target(0)?))),
        "mkdir" => expect(1).and_then(|()| Ok(Command::MakeDirectory(target(0)?))),
        "rm" => expect(1).and_then(|()| {
            Ok(Command::Remove {
                target: target(0)?,
                recursive,
            })
        }),
        "get" | "put" | "cp" => expect(2).and_then(|()| {
            Ok(Command::Copy {
                from: target(0)?,
                to: target(1)?,
                recursive,
            })
        }),
        "compare" => expect(2).and_then(|()| {
            Ok(Command::Compare {
                left: target(0)?,
                right: target(1)?,
                recursive,
            })
        }),
        "sync" => expect(2).and_then(|()| {
            Ok(Command::Sync {
                from: target(0)?,
                to: target(1)?,
                recursive,
            })
        }),
        "chmod" => expect(2).and_then(|()| {
            Ok(Command::Chmod {
                mode: parse_mode(&rest[0]).ok_or_else(|| {
                    format!(
                        "chmod: {} is not a mode, use three or four octal digits like 644",
                        rest[0]
                    )
                })?,
                target: target(1)?,
                recursive,
            })
        }),
        "find" => expect(2).and_then(|()| {
            Ok(Command::Find {
                target: target(0)?,
                pattern: rest[1].clone(),
            })
        }),
        "mv" => expect(2).and_then(|()| {
            Ok(Command::Move {
                from: target(0)?,
                to: target(1)?,
            })
        }),
        _ => Err(format!("unknown command: {name}")),
    }
}

// Help

pub const USAGE: &str = "\
Usage: hoplix-cli <command> [options]

Commands:
  ls <target>             List a folder
  stat <target>           Show details of a file or folder
  get [-r] <remote> <local>
                          Download a file, or a folder with everything in it (-r)
  put [-r] <local> <remote>
                          Upload a file, or a folder with everything in it (-r)
  cp [-r] <from> <to>     Copy between any two targets, e.g. from one server to another
  mkdir <target>          Create a folder
  rm [-r] <target>        Delete a file, or a folder with everything in it (-r)
  mv <from> <to>          Rename or move within the same server
  chmod [-r] <mode> <target>
                          Change the permissions (octal, e.g. 644) of a file or
                          folder (-r: also everything inside); SFTP, FTP with
                          SITE CHMOD and local folders on Linux and macOS
  find <target> <pattern> [--depth <n>]
                          Search the folder and its subfolders for names that
                          contain the pattern, or match it with * and ? (quote
                          it); --depth 1 searches only the folder itself
  compare [-r] <a> <b>    Show the differences between two folders
  sync [-r] <from> <to>   Copy new and newer files so that <to> matches <from>
  sites                   List the sites saved in Hoplix
  sites export <file>     Save the site list to a file (without passwords unless
                          --include-passwords is given)
  sites import <file>     Add the sites from an exported file to the site list
  help, version

Targets are local paths or URLs like sftp://user@host:port/path
(use /~/path for paths relative to the home folder).
Protocols: sftp://, ftp:// (unencrypted), ftpes:// (FTPS with explicit TLS),
ftps:// (FTPS with implicit TLS), dav:// (WebDAV over unencrypted HTTP) and
davs:// (WebDAV over HTTPS); for WebDAV the path is the full server path, e.g.
davs://user@host/remote.php/dav/files/user/Documents. FTP URLs without a user
sign in anonymously. WebDAV uploads cannot be resumed and start again.
Saved sites are targets too: site://<name>/path (encode / in folder/name as %2F).
They use the saved password or passphrase unless you pass --password-env or --key.

Options:
  --key <file>              Sign in with this private key file
  --agent                   Sign in with the keys of the running SSH agent (SFTP;
                            Windows: OpenSSH agent or Pageant, else SSH_AUTH_SOCK)
  --passphrase-env <name>   Read the key passphrase from this environment variable
  --password-env <name>     Read the password from this environment variable
  --master-password-env <name>
                            Read the Hoplix master password from this environment
                            variable (only when saved passwords use a master password)
  --accept-host-key         Trust an unknown server key or certificate for this run
  --remember-host-key       Trust an unknown server key or certificate and remember it
  --known-hosts <file>      Use this known hosts file instead of the Hoplix one
  --trusted-certificates <file>
                            Use this certificate trust file instead of the Hoplix one

Jump host and proxy (they apply to all servers of the command and replace
the jump host and proxy saved with a site):
  --jump <user@host[:port]> Connect to the SFTP server through this SSH jump host
  --jump-key <file>         Sign in to the jump host with this private key file
  --jump-passphrase-env <name>
                            Read the passphrase of --jump-key from this variable
  --jump-password-env <name>
                            Read the jump host password from this variable
  --jump-agent              Sign in to the jump host with the SSH agent
  --proxy <url>             Connect through a proxy: socks5://host[:port] (default
                            port 1080, host names are resolved by the proxy) or
                            http://host[:port] (HTTP CONNECT, default port 8080).
                            FTP works through the proxy in passive mode only
  --proxy-user <name>       Sign in to the proxy with this user name
  --proxy-password-env <name>
                            Read the proxy password from this environment variable

Transfers:
  --resume                  Continue interrupted files (existing smaller files are
                            completed, equal ones skipped); default: overwrite
  --skip-existing           Leave files that already exist untouched
  --keep-both               Store new files next to existing ones as name (1).ext
  --retries <n>             Retry a file this often when the connection drops
                            (0 to 10, default 3)
  --retry-delay <seconds>   Wait this long before the first retry; each further
                            wait is twice as long (1 to 60, default 1)
  --limit-download <rate>   Limit the download speed, e.g. 500K or 2M; a number
                            without unit means KiB/s, 0 means no limit
  --limit-upload <rate>     Limit the upload speed in the same way
                            (a copy between two servers is limited by both)
  --verify                  Compare checksums of each copied file with the original
                            (SHA-256 locally, on servers the best algorithm both
                            sides offer, otherwise the size)

Site lists:
  --folder <name>           sites export: only export this folder;
                            sites import: put the sites into this folder
                            (default: Imported <date>)
  --include-passwords       sites export: also export the saved passwords; the file
                            is then encrypted with the password from --password-env
  --password-env <name>     sites export and import: the password of an encrypted
                            site list (sites with saved passwords in a master
                            password vault also need --master-password-env)

Compare and sync:
  --size-only               Compare only the size, ignore modification times
                            (times within 2 seconds count as equal)
  --skip-hidden             Leave out files and folders whose name starts with a dot
  --exclude <pattern>       Leave out names or paths matching the pattern (* and ?,
                            ignoring case); can be given several times
  --mirror                  Also replace files that are newer in <to>
  --delete                  Like --mirror, and delete what is missing in <from>
  --dry-run                 Only show what sync would do

compare prints one line per difference: only-first, only-second, newer-first,
newer-second, size (same time, different size) or type (file and folder).
";

#[cfg(test)]
mod tests {
    use super::*;

    fn run(arguments: &[&str]) -> Result<Invocation, String> {
        parse(arguments.iter().map(|argument| (*argument).to_string()))
    }

    fn remote(text: &str) -> Target {
        Target::parse(text).expect("valid target")
    }

    #[test]
    fn empty_or_help_shows_usage() {
        assert_eq!(run(&[]).map(|i| i.command), Ok(Command::Help));
        assert_eq!(run(&["ls", "--help"]).map(|i| i.command), Ok(Command::Help));
        assert_eq!(run(&["-V"]).map(|i| i.command), Ok(Command::Version));
    }

    #[test]
    fn commands_and_options_parse() {
        let invocation = run(&[
            "get",
            "sftp://demo@host/~/a.txt",
            "a.txt",
            "--key",
            "id_ed25519",
            "--passphrase-env",
            "KEY_PASS",
            "--accept-host-key",
            "--resume",
        ])
        .expect("valid invocation");
        assert_eq!(
            invocation.command,
            Command::Copy {
                from: remote("sftp://demo@host/~/a.txt"),
                to: Target::Local("a.txt".to_string()),
                recursive: false
            }
        );
        assert_eq!(invocation.options.key, Some(PathBuf::from("id_ed25519")));
        assert_eq!(
            invocation.options.passphrase_env.as_deref(),
            Some("KEY_PASS")
        );
        assert!(invocation.options.accept_host_key && invocation.options.resume);

        let invocation = run(&["rm", "-r", "sftp://demo@host/tmp/x"]).expect("valid");
        assert_eq!(
            invocation.command,
            Command::Remove {
                target: remote("sftp://demo@host/tmp/x"),
                recursive: true
            }
        );
    }

    #[test]
    fn mistakes_are_reported() {
        assert!(run(&["ls"]).is_err());
        assert!(run(&["ls", "a", "b"]).is_err());
        assert!(run(&["frobnicate", "x"]).is_err());
        assert!(run(&["ls", "x", "--bogus"]).is_err());
        assert!(run(&["ls", "x", "--key"]).is_err());
        assert!(run(&["ls", "x", "--key", "--resume"]).is_err());
        assert!(run(&["ls", "-r", "x"]).is_err());
        assert!(run(&["get", "a", "b", "--resume", "--keep-both"]).is_err());
        assert!(run(&["ls", "x", "--agent", "--key", "id"]).is_err());
        assert!(run(&["ls", "sftp://host/x"]).is_err());
    }

    #[test]
    fn recursive_copies_parse() {
        let invocation = run(&[
            "cp",
            "-r",
            "sftp://a@one/x",
            "sftp://b@two/y",
            "--skip-existing",
        ])
        .expect("valid");
        assert_eq!(
            invocation.command,
            Command::Copy {
                from: remote("sftp://a@one/x"),
                to: remote("sftp://b@two/y"),
                recursive: true
            }
        );
        assert!(invocation.options.skip_existing);
    }

    #[test]
    fn compare_and_sync_parse_their_options() {
        let invocation = run(&[
            "compare",
            "-r",
            "local",
            "sftp://demo@host/srv",
            "--size-only",
        ])
        .expect("valid");
        assert_eq!(
            invocation.command,
            Command::Compare {
                left: Target::Local("local".to_string()),
                right: remote("sftp://demo@host/srv"),
                recursive: true
            }
        );
        assert!(invocation.options.size_only);

        let invocation = run(&[
            "sync",
            "site://web/www",
            "backup",
            "-r",
            "--delete",
            "--exclude",
            "*.tmp",
            "--exclude",
            ".git",
            "--skip-hidden",
            "--dry-run",
        ])
        .expect("valid");
        assert!(matches!(
            invocation.command,
            Command::Sync {
                recursive: true,
                ..
            }
        ));
        let options = invocation.options;
        assert!(options.delete && options.dry_run && options.skip_hidden && !options.mirror);
        assert_eq!(options.exclude, ["*.tmp", ".git"]);
    }

    #[test]
    fn sync_options_need_the_right_command() {
        assert!(run(&["compare", "a"]).is_err());
        assert!(run(&["sync", "a", "b", "c"]).is_err());
        assert!(run(&["compare", "a", "b", "--delete"]).is_err());
        assert!(run(&["compare", "a", "b", "--dry-run"]).is_err());
        assert!(run(&["cp", "a", "b", "--size-only"]).is_err());
        assert!(run(&["get", "a", "b", "--exclude", "*.tmp"]).is_err());
        assert!(run(&["sync", "a", "b", "--exclude"]).is_err());
        assert!(run(&["sync", "a", "b", "--mirror"]).is_ok());
    }

    #[test]
    fn retry_and_limit_options_parse() {
        let invocation = run(&[
            "get",
            "sftp://demo@host/big.iso",
            "big.iso",
            "--retries",
            "5",
            "--retry-delay",
            "10",
            "--limit-download",
            "2M",
            "--limit-upload",
            "512",
        ])
        .expect("valid");
        let options = invocation.options;
        assert_eq!(options.retries, Some(5));
        assert_eq!(options.retry_delay, Some(10));
        assert_eq!(options.limit_download, Some(2 * 1024 * 1024));
        assert_eq!(options.limit_upload, Some(512 * 1024));

        for (text, bytes) in [
            ("0", 0),
            ("300k", 300 * 1024),
            ("300KiB", 300 * 1024),
            ("300KiB/s", 300 * 1024),
            ("1mib", 1024 * 1024),
            ("7M/s", 7 * 1024 * 1024),
        ] {
            let invocation = run(&["sync", "a", "b", "--limit-upload", text]).expect("valid rate");
            assert_eq!(invocation.options.limit_upload, Some(bytes), "{text}");
        }
        let invocation = run(&["put", "a", "sftp://demo@host/a", "--retries", "0"]).expect("valid");
        assert_eq!(invocation.options.retries, Some(0));
    }

    #[test]
    fn retry_and_limit_mistakes_are_reported() {
        for arguments in [
            &["get", "a", "b", "--retries", "11"][..],
            &["get", "a", "b", "--retries", "-1"],
            &["get", "a", "b", "--retries"],
            &["get", "a", "b", "--retry-delay", "0"],
            &["get", "a", "b", "--retry-delay", "61"],
            &["get", "a", "b", "--limit-download", "fast"],
            &["get", "a", "b", "--limit-download", "2G"],
            &["get", "a", "b", "--limit-download", "1.5M"],
            &["get", "a", "b", "--limit-upload", "M"],
            &["get", "a", "b", "--limit-upload", "9999999M"],
            &["ls", "a", "--retries", "2"],
            &["ls", "a", "--verify"],
            &["compare", "a", "b", "--verify"],
            &["compare", "a", "b", "--limit-download", "1M"],
            &["rm", "a", "--retry-delay", "5"],
        ] {
            assert!(run(arguments).is_err(), "{arguments:?}");
        }
        let error = run(&["get", "a", "b", "--retries", "11"]).expect_err("accepted");
        assert!(error.contains("0 to 10"), "{error}");
    }

    #[test]
    fn verification_is_offered_for_transfers() {
        for command in ["get", "put", "cp", "sync"] {
            let invocation = run(&[command, "a", "b", "--verify"]).expect("valid");
            assert!(invocation.options.verify, "{command}");
        }
        assert!(!run(&["get", "a", "b"]).expect("valid").options.verify);
    }

    #[test]
    fn plain_secrets_are_refused() {
        let error = run(&["ls", "x", "--password", "hunter2"]).expect_err("accepted");
        assert!(error.contains("--password-env"));
        assert!(!error.contains("hunter2"));
        let error = run(&["ls", "x", "--master-password", "hunter2"]).expect_err("accepted");
        assert!(error.contains("--master-password-env"));
    }

    #[test]
    fn site_lists_export_and_import() {
        let invocation = run(&[
            "sites",
            "export",
            "list.hoplix-sites",
            "--include-passwords",
            "--password-env",
            "EXPORT_PASSWORD",
            "--folder",
            "Work",
        ])
        .expect("valid");
        assert_eq!(
            invocation.command,
            Command::ExportSites(PathBuf::from("list.hoplix-sites"))
        );
        assert!(invocation.options.include_passwords);
        assert_eq!(invocation.options.folder.as_deref(), Some("Work"));
        assert_eq!(
            invocation.options.password_env.as_deref(),
            Some("EXPORT_PASSWORD")
        );
        let invocation = run(&["sites", "import", "list.hoplix-sites"]).expect("valid");
        assert_eq!(
            invocation.command,
            Command::ImportSites(PathBuf::from("list.hoplix-sites"))
        );
        for arguments in [
            &["sites", "export"][..],
            &["sites", "import", "a", "b"],
            &["sites", "export", "a", "--include-passwords"],
            &[
                "sites",
                "import",
                "a",
                "--include-passwords",
                "--password-env",
                "X",
            ],
            &["ls", "a", "--folder", "Work"],
            &["sites", "--folder", "Work"],
        ] {
            assert!(run(arguments).is_err(), "{arguments:?}");
        }
    }

    #[test]
    fn sites_and_master_passwords_parse() {
        assert_eq!(run(&["sites"]).map(|i| i.command), Ok(Command::Sites));
        assert!(run(&["sites", "extra"]).is_err());
        let invocation =
            run(&["ls", "site://web", "--master-password-env", "HOPLIX_MASTER"]).expect("valid");
        assert_eq!(
            invocation.options.master_password_env.as_deref(),
            Some("HOPLIX_MASTER")
        );
    }

    #[test]
    fn agent_jump_and_proxy_options_parse() {
        let invocation = run(&[
            "ls",
            "sftp://demo@target/",
            "--agent",
            "--jump",
            "hop@[fe80::1]:2222",
            "--jump-key",
            "jump_key",
            "--jump-passphrase-env",
            "JUMP_PASS",
            "--proxy",
            "socks5h://proxy.test/",
            "--proxy-user",
            "tester",
            "--proxy-password-env",
            "PROXY_PASS",
        ])
        .expect("valid invocation");
        let options = invocation.options;
        assert!(options.agent);
        assert_eq!(
            options.jump,
            Some(JumpTarget {
                username: "hop".to_string(),
                host: "fe80::1".to_string(),
                port: 2222,
            })
        );
        assert_eq!(options.jump_key, Some(PathBuf::from("jump_key")));
        assert_eq!(options.jump_passphrase_env.as_deref(), Some("JUMP_PASS"));
        assert_eq!(
            options.proxy,
            Some(Proxy {
                kind: ProxyKind::Socks5,
                host: "proxy.test".to_string(),
                port: 1080,
                credentials: None,
            })
        );
        assert_eq!(options.proxy_user.as_deref(), Some("tester"));
        assert_eq!(options.proxy_password_env.as_deref(), Some("PROXY_PASS"));

        let options = run(&[
            "ls",
            "ftp://demo@target/",
            "--proxy",
            "HTTP://[::1]:3128",
            "--jump",
            "user@bastion",
            "--jump-agent",
        ])
        .expect("valid")
        .options;
        let proxy = options.proxy.expect("proxy");
        assert_eq!(
            (proxy.kind, proxy.host.as_str(), proxy.port),
            (ProxyKind::Http, "::1", 3128)
        );
        let jump = options.jump.expect("jump");
        assert_eq!((jump.host.as_str(), jump.port), ("bastion", 22));
        assert!(options.jump_agent);
    }

    #[test]
    fn route_mistakes_are_reported() {
        for arguments in [
            &["ls", "x", "--jump", "bastion", "--jump-agent"][..],
            &["ls", "x", "--jump", "@bastion", "--jump-agent"],
            &["ls", "x", "--jump", "hop@bastion:0", "--jump-agent"],
            &["ls", "x", "--jump", "hop@bastion"],
            &[
                "ls",
                "x",
                "--jump",
                "hop@bastion",
                "--jump-agent",
                "--jump-password-env",
                "P",
            ],
            &["ls", "x", "--jump-agent"],
            &["ls", "x", "--jump-passphrase-env", "P"],
            &[
                "ls",
                "x",
                "--jump",
                "hop@bastion",
                "--jump-agent",
                "--jump-passphrase-env",
                "P",
            ],
            &["ls", "x", "--jump-password", "secret"],
            &["ls", "x", "--proxy", "proxy.test:1080"],
            &["ls", "x", "--proxy", "socks4://proxy.test"],
            &["ls", "x", "--proxy", "socks5://user:pw@proxy.test"],
            &["ls", "x", "--proxy", "http://proxy.test/path"],
            &["ls", "x", "--proxy", "http://:8080"],
            &["ls", "x", "--proxy-user", "tester"],
            &[
                "ls",
                "x",
                "--proxy",
                "socks5://proxy.test",
                "--proxy-user",
                "tester",
            ],
            &[
                "ls",
                "x",
                "--proxy",
                "socks5://proxy.test",
                "--proxy-password-env",
                "P",
            ],
            &["ls", "x", "--proxy-password", "secret"],
        ] {
            assert!(run(arguments).is_err(), "{arguments:?}");
        }
    }

    #[test]
    fn usage_mentions_the_route_options() {
        for option in [
            "--agent",
            "--jump ",
            "--jump-key",
            "--jump-passphrase-env",
            "--jump-password-env",
            "--jump-agent",
            "--proxy ",
            "--proxy-user",
            "--proxy-password-env",
        ] {
            assert!(USAGE.contains(option), "{option}");
        }
    }

    #[test]
    fn chmod_and_find_parse() {
        assert_eq!(
            run(&["chmod", "-r", "0755", "sftp://demo@host/www"])
                .expect("chmod")
                .command,
            Command::Chmod {
                mode: 0o755,
                target: remote("sftp://demo@host/www"),
                recursive: true,
            }
        );
        let found = run(&["find", "sftp://demo@host/www", "*.log", "--depth", "2"]).expect("find");
        assert_eq!(
            found.command,
            Command::Find {
                target: remote("sftp://demo@host/www"),
                pattern: "*.log".to_string(),
            }
        );
        assert_eq!(found.options.depth, Some(2));
        assert!(run(&["chmod", "rwx", "/tmp/a"]).is_err());
        assert!(run(&["chmod", "644"]).is_err());
        assert!(run(&["find", "/tmp", "x", "--depth", "0"]).is_err());
        assert!(run(&["ls", "/tmp", "--depth", "2"]).is_err());
        assert!(run(&["find", "-r", "/tmp", "x"]).is_err());
        assert!(USAGE.contains("chmod [-r] <mode> <target>"));
        assert!(USAGE.contains("find <target> <pattern>"));
    }
}
