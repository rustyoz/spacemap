use std::io::BufReader;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use spacemap_core::{
    read_msg, socket_path, write_msg, Engine, Request, Response, SnapshotSource,
};

#[derive(Parser, Debug)]
#[command(name = "spacemapd", about = "Scan disks and keep a live usage map")]
struct Args {
    /// Unix socket path (default: $XDG_RUNTIME_DIR/spacemap.sock)
    #[arg(long)]
    socket: Option<PathBuf>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "spacemap_core=info,spacemapd=info".into()),
        )
        .init();

    let args = Args::parse();
    let sock = args.socket.unwrap_or_else(socket_path);
    if let Some(dir) = sock.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::remove_file(&sock);

    let engine = Engine::new(SnapshotSource::Daemon)?;
    let runner = Arc::clone(&engine);
    std::thread::Builder::new()
        .name("spacemap-engine".into())
        .spawn(move || runner.run())
        .expect("engine thread");

    {
        let engine = Arc::clone(&engine);
        let sock = sock.clone();
        ctrlc::set_handler(move || {
            engine.stop();
            let _ = std::fs::remove_file(&sock);
            std::process::exit(0);
        })?;
    }

    let listener = UnixListener::bind(&sock)?;
    // Allow the user session to connect.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600));
    }
    tracing::info!(path = %sock.display(), "spacemapd listening");

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let engine = Arc::clone(&engine);
        std::thread::spawn(move || {
            if let Err(err) = serve_client(stream, engine) {
                tracing::debug!(error = %err, "client gone");
            }
        });
    }
    engine.stop();
    let _ = std::fs::remove_file(&sock);
    Ok(())
}

fn serve_client(stream: std::os::unix::net::UnixStream, engine: Arc<Engine>) -> Result<()> {
    stream.set_nonblocking(false)?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    loop {
        let req: Request = read_msg(&mut reader)?;
        let resp = match req {
            Request::Ping => Response::Pong,
            Request::Snapshot {
                min_fraction,
                focus,
                open,
            } => Response::Snapshot(engine.snapshot(min_fraction, &focus, &open)),
            Request::Rescan { mount } => {
                engine.request_rescan(mount.map(PathBuf::from));
                Response::Ok
            }
            Request::Expand { path } => {
                engine.request_expand(PathBuf::from(path));
                Response::Ok
            }
        };
        write_msg(&mut writer, &resp)?;
    }
}
