use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;
use xodus::tokens::TokenManager;

mod connection;
mod simple_context;

use xodus::ipc::{PROTO_MAGIC, XML_MAGIC};

#[tokio::main]
async fn main() {
    xodus::secrets::init_secrets().expect("Failed to init keychain");
    let tokens = Arc::new(TokenManager::with_keychain_and_memory());
    xodus::tokens::device::ensure_device_credentials(&reqwest::Client::new(), &tokens).await;
    let xodus::models::secrets::Token::Legacy(device_token) =
        tokens.get_device_sts_token().unwrap()
    else {
        panic!("Device token isnt legacy")
    };

    env_logger::init_from_env("XODUS_LOG");
    let cancellation = CancellationToken::new();
    // Shared with clients (xodus-cli, the wine-side runtime) via xodus::ipc so
    // both ends always agree on the endpoint.
    let socket_path = xodus::ipc::socket_path().expect("XDG_RUNTIME_DIR is not set");
    let trigger = cancellation.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Failure to handle ctrl_c");
        trigger.cancel();
    });
    {
        // A previous unclean shutdown leaves the socket file behind and makes
        // bind fail; remove it only after confirming nothing answers on it.
        if tokio::fs::try_exists(&socket_path).await.unwrap_or(false) {
            match xodus::ipc::ping(&socket_path, Duration::from_secs(2)).await {
                Ok(()) => {
                    eprintln!(
                        "another xodus-service is already running on {}",
                        socket_path.display()
                    );
                    return;
                }
                Err(err) => {
                    log::info!("removing stale socket {} ({err})", socket_path.display());
                    _ = tokio::fs::remove_file(&socket_path).await;
                }
            }
        }
        let listener = UnixListener::bind(&socket_path).expect("Unable to bind to socket");
        let mode = 0o600;
        let perms = Permissions::from_mode(mode);
        _ = tokio::fs::set_permissions(&socket_path, perms).await;
        loop {
            let accept = match tokio::select! {
                r = listener.accept() => r,
                _ = cancellation.cancelled() => break,
            } {
                Ok(accept) => accept,
                Err(err) => {
                    // Usually transient (EMFILE, aborted handshake) - keep serving.
                    log::error!("Failed to accept connection: {err}");
                    continue;
                }
            };

            let token = cancellation.clone();
            let device_token = device_token.clone();
            let tokens = tokens.clone();
            tokio::spawn(async move {
                connection::router::route(accept.0, token, device_token, tokens).await
            });
        }
    }

    _ = tokio::fs::remove_file(socket_path).await;
}
