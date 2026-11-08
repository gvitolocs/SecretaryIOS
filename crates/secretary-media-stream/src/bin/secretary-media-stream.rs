//! Provisional media service: folders on disk served with the player routes
//! until `lucina-media` mounts `secretary_media_stream::router` itself.
//!
//! `/v1/media/s/...` is protected by its play tokens. Everything else trusts
//! the caller, so bind it where only the authenticating proxy (p1's
//! secretary-server) can reach it, and/or set `--upstream-key-file`: the
//! proxy then sends the key in `X-Media-Upstream-Key`.
use axum::{
    extract::Request,
    http::StatusCode,
    middleware::{self, Next},
    response::Response,
};
use clap::Parser;
use secretary_media_stream::{
    dir::{self, DirSource, Root},
    router, StreamConfig,
};
use std::{path::PathBuf, sync::Arc};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8830")]
    listen: String,
    /// `Label=/path`, repeatable. The label is the album name.
    #[arg(long = "root", required = true)]
    roots: Vec<String>,
    #[arg(long)]
    cache: PathBuf,
    /// Resume positions (JSON).
    #[arg(long)]
    progress: PathBuf,
    /// Play-token HMAC key; created (32 random bytes, mode 0600) if missing.
    #[arg(long)]
    secret_file: PathBuf,
    /// Shared key the proxy must send for the non-token routes.
    #[arg(long)]
    upstream_key_file: Option<PathBuf>,
    #[arg(long, default_value_t = 2)]
    max_jobs: usize,
}

fn load_secret(path: &PathBuf) -> anyhow_lite::Result<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) if bytes.len() >= 32 => Ok(bytes),
        Ok(_) => Err(format!("{}: shorter than 32 bytes", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut key = vec![0u8; 32];
            getrandom::fill(&mut key).map_err(|e| e.to_string())?;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).map_err(|e| e.to_string())?;
            f.write_all(&key).map_err(|e| e.to_string())?;
            Ok(key)
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

mod anyhow_lite {
    pub type Result<T> = std::result::Result<T, String>;
}

async fn require_upstream_key(key: Arc<String>, req: Request, next: Next) -> Result<Response, StatusCode> {
    if req.uri().path().starts_with("/v1/media/s/") {
        return Ok(next.run(req).await);
    }
    let given = req.headers().get("x-media-upstream-key").and_then(|v| v.to_str().ok()).unwrap_or("");
    // Lengths are public; compare the bytes without an early exit.
    let ok = given.len() == key.len() && given.bytes().zip(key.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0;
    if ok {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

#[tokio::main]
async fn main() -> Result<(), String> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let args = Args::parse();
    let roots = args
        .roots
        .iter()
        .map(|r| {
            let (label, path) = r.split_once('=').ok_or_else(|| format!("--root {r}: expected Label=/path"))?;
            Ok(Root { label: label.to_string(), path: PathBuf::from(path) })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut cfg = StreamConfig::new(load_secret(&args.secret_file)?, args.cache, args.progress);
    cfg.max_jobs = args.max_jobs;
    let src = Arc::new(DirSource::new(roots));
    let mut app = router(Arc::clone(&src), cfg.clone())?.merge(dir::items_router(src, cfg));
    if let Some(path) = &args.upstream_key_file {
        let key = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?.trim().to_string();
        if key.len() < 16 {
            return Err("upstream key shorter than 16 characters".into());
        }
        let key = Arc::new(key);
        app = app.layer(middleware::from_fn(move |req, next| require_upstream_key(Arc::clone(&key), req, next)));
    }
    let listener = tokio::net::TcpListener::bind(&args.listen).await.map_err(|e| format!("{}: {e}", args.listen))?;
    tracing::info!("media stream listening on {}", args.listen);
    axum::serve(listener, app).await.map_err(|e| e.to_string())
}
