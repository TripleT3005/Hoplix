use std::path::PathBuf;

use crate::target::Target;

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    Sites,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Options {
    pub key: Option<PathBuf>,
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
            "-r" | "--recursive" => recursive = true,
            "-h" | "--help" => positional.insert(0, "help".to_string()),
            "-V" | "--version" => positional.insert(0, "version".to_string()),
            "--password" | "--passphrase" | "--master-password" => {
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
    if recursive
        && !matches!(
            command,
            Command::Remove { .. }
                | Command::Copy { .. }
                | Command::Compare { .. }
                | Command::Sync { .. }
        )
    {
        return Err("-r only works with get, put, cp, rm, compare and sync".to_string());
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
        "sites" => expect(0).map(|()| Command::Sites),
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
  compare [-r] <a> <b>    Show the differences between two folders
  sync [-r] <from> <to>   Copy new and newer files so that <to> matches <from>
  sites                   List the sites saved in Hoplix
  help, version

Targets are local paths or URLs like sftp://user@host:port/path
(use /~/path for paths relative to the home folder).
Protocols: sftp://, ftp:// (unencrypted), ftpes:// (FTPS with explicit TLS)
and ftps:// (FTPS with implicit TLS). FTP URLs without a user sign in anonymously.
Saved sites are targets too: site://<name>/path (encode / in folder/name as %2F).
They use the saved password or passphrase unless you pass --password-env or --key.

Options:
  --key <file>              Sign in with this private key file
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
  --resume                  Continue interrupted files (existing smaller files are
                            completed, equal ones skipped); default: overwrite
  --skip-existing           Leave files that already exist untouched
  --keep-both               Store new files next to existing ones as name (1).ext

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
    fn plain_secrets_are_refused() {
        let error = run(&["ls", "x", "--password", "hunter2"]).expect_err("accepted");
        assert!(error.contains("--password-env"));
        assert!(!error.contains("hunter2"));
        let error = run(&["ls", "x", "--master-password", "hunter2"]).expect_err("accepted");
        assert!(error.contains("--master-password-env"));
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
}
