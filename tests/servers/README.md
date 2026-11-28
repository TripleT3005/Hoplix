# Test servers

A Docker container with every server the ignored integration tests and manual tests of Hoplix need. It runs on Docker Desktop (Linux containers) or any other Docker host and is built from `debian:13` with Debian packages only: OpenSSH (SFTP, SCP, keyboard-interactive with TOTP through `libpam-google-authenticator`), vsftpd (FTP, FTPES, FTPS), Apache with `mod_dav` (WebDAV over HTTP and HTTPS) and moto (S3). A small shell script starts the daemons and stops the container when one of them exits.

## Usage

Run the helper from the repository root in Windows PowerShell:

```powershell
.\scripts\test-servers.ps1 start     # build the image if needed, create and start the container
.\scripts\test-servers.ps1 env       # set all HOPLIX_TEST_* variables in this session
cargo test -p hoplix-core --test sftp --test scp --test keyboard_interactive --test ftp --test restore -- --ignored --test-threads=1
.\scripts\test-servers.ps1 stop      # stop the container, it is kept for the next start
```

Further commands: `status`, `rebuild` (rebuild the image and recreate the container with the same credentials) and `reset` (remove the container and all generated credentials after a confirmation, then start again with new ones).

The FTP tests use plain FTP by default. For the TLS variants set the protocol and port after `env`:

```powershell
$env:HOPLIX_TEST_FTP_PROTOCOL = 'ftpes'                                  # explicit TLS on the FTP port
$env:HOPLIX_TEST_FTP_PROTOCOL = 'ftps'; $env:HOPLIX_TEST_FTP_PORT = $env:HOPLIX_TEST_FTPS_PORT   # implicit TLS
```

The image is `hoplix-test-servers:latest`, the container `hoplix-test-servers`, both labelled `project=hoplix`.

## Ports

All ports are published on `127.0.0.1` only and use the same number inside the container, so the jump host test can reach the target port from within the container.

| Port | Server |
| --- | --- |
| 2222 | SFTP and SCP, password or key (`demo`) |
| 2223 | SSH with key, then a one-time code by keyboard-interactive |
| 2224 | SSH with password and one-time code by keyboard-interactive |
| 2221 | FTP and FTPES (explicit TLS, optional) |
| 2990 | FTPS (implicit TLS) |
| 40000–40019 | FTP passive data connections (2221: 40000–40009, 2990: 40010–40019) |
| 2280 | WebDAV over HTTP: `/digest` (Digest authentication), `/basic` (Basic authentication) |
| 2443 | WebDAV over HTTPS, same folders |
| 2900 | S3 (moto) over HTTP with the bucket `demo`; use path-style addressing |

Windows reserves port ranges for Hyper-V and WinNAT, which can change after a restart. If `start` reports that a port is not available, `netsh interface ipv4 show excludedportrange protocol=tcp` shows the reserved ranges.

## Credentials

Nothing secret is part of the repository or the image. On the first start the container generates a random password for the user `demo`, two client keys (one protected by a random passphrase), the TOTP secret, an SSH host key, a self-signed certificate for `localhost` and `127.0.0.1` and random S3 keys. They are stored in `%USERPROFILE%\.hoplix-test-servers`, which is mounted into the container, and are reused on every later start and after `rebuild`. New ones are only generated when this folder is empty.

| File | Content |
| --- | --- |
| `test.env` | All `HOPLIX_TEST_*` variables as `NAME=value` lines, loaded by `env` |
| `id_ed25519_passphrase` | Client key for SFTP and SCP, protected by `HOPLIX_TEST_SFTP_PASSPHRASE` |
| `id_ed25519` | Client key without passphrase for the keyboard-interactive tests |
| `certificate.pem` | The self-signed server certificate of FTPS and WebDAV over HTTPS |
| `fingerprints.txt` | SHA-256 fingerprints of the certificate and of the SSH host key |
| `server` | Server side keys and secrets used by the container |

All of this is for local tests only; the servers are not reachable from other computers. moto accepts any S3 keys, so the generated ones are only there for clients that require some.
