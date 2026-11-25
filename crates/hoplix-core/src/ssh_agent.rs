use russh::client::{Handle, Handler};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::{AgentClient, AgentStream};

use crate::{Error, ErrorKind, Result};

// Constants

#[cfg(windows)]
const OPENSSH_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

// Types

type Agent = AgentClient<Box<dyn AgentStream + Send + Unpin + 'static>>;

// Agents

#[cfg(windows)]
async fn agents() -> Vec<Agent> {
    let mut agents = Vec::new();
    let mut pipes = Vec::new();
    if let Some(socket) = std::env::var_os("SSH_AUTH_SOCK").filter(|value| !value.is_empty()) {
        pipes.push(socket);
    }
    if !pipes
        .iter()
        .any(|pipe| pipe.eq_ignore_ascii_case(OPENSSH_PIPE))
    {
        pipes.push(OPENSSH_PIPE.into());
    }
    for pipe in pipes {
        if let Ok(agent) = AgentClient::connect_named_pipe(&pipe).await {
            agents.push(agent.dynamic());
        }
    }
    if let Ok(agent) = AgentClient::connect_pageant().await {
        agents.push(agent.dynamic());
    }
    agents
}

#[cfg(unix)]
async fn agents() -> Vec<Agent> {
    match AgentClient::connect_env().await {
        Ok(agent) => vec![agent.dynamic()],
        Err(_) => Vec::new(),
    }
}

// Authentication

pub(crate) async fn authenticate<H: Handler>(
    handle: &mut Handle<H>,
    user: &str,
    label: &str,
) -> Result<()> {
    let mut answered = 0;
    let mut offered = 0;
    for mut agent in agents().await {
        let Ok(identities) = agent.request_identities().await else {
            continue;
        };
        answered += 1;
        for identity in identities {
            offered += 1;
            let hash = if identity.public_key().algorithm().is_rsa() {
                handle
                    .best_supported_rsa_hash()
                    .await
                    .ok()
                    .flatten()
                    .flatten()
            } else {
                None
            };
            let result = match identity {
                AgentIdentity::PublicKey { key, .. } => {
                    handle
                        .authenticate_publickey_with(user, key, hash, &mut agent)
                        .await
                }
                AgentIdentity::Certificate { certificate, .. } => {
                    handle
                        .authenticate_certificate_with(user, certificate, hash, &mut agent)
                        .await
                }
            };
            match result {
                Ok(result) if result.success() => return Ok(()),
                _ if handle.is_closed() => {
                    return Err(Error::new(
                        ErrorKind::AuthenticationFailed,
                        format!(
                            "{label}: the server closed the connection after {offered} keys from the SSH agent"
                        ),
                    ));
                }
                _ => {}
            }
        }
    }
    let detail = if answered == 0 {
        format!("{label}: no SSH agent is running")
    } else if offered == 0 {
        format!("{label}: the SSH agent holds no keys")
    } else {
        format!("{label}: the server accepted none of the {offered} keys from the SSH agent")
    };
    Err(Error::new(ErrorKind::AuthenticationFailed, detail))
}
