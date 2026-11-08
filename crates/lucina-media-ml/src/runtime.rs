//! ONNX Runtime, shared by every model of this crate (SigLIP2 for smart search, YuNet and SFace
//! for faces). `ort::init_from` may only run once per process, so the first model to load decides
//! which `libonnxruntime.so` is used; all of them point at the same file.

use std::path::Path;
use std::sync::OnceLock;

use ort::session::Session;

use crate::MlError;

pub(crate) fn unavailable(e: impl std::fmt::Display) -> MlError {
    MlError::Unavailable(e.to_string())
}

pub(crate) fn failed(e: impl std::fmt::Display) -> MlError {
    MlError::Failed(e.to_string())
}

/// ONNX Runtime from `lib`, once per process (it cannot be reloaded). A library that fails to
/// load leaves the models off instead of crashing the service.
pub(crate) fn init_runtime(lib: &Path) -> Result<(), MlError> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        let path = lib.display().to_string();
        std::panic::catch_unwind(|| ort::init_from(path).with_name("lucina-media-ml").commit())
            .map_err(|_| format!("could not load ONNX Runtime from {}", lib.display()))?
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
    .clone()
    .map_err(MlError::Unavailable)
}

/// A session on `path` with `threads` intra-op threads.
pub(crate) fn session(path: &Path, threads: usize) -> Result<Session, MlError> {
    std::panic::catch_unwind(|| {
        Session::builder()?
            .with_intra_threads(threads.max(1))?
            .with_inter_threads(1)?
            .commit_from_file(path)
    })
    .map_err(|_| MlError::Unavailable("ONNX Runtime panicked loading a model".into()))?
    .map_err(|e| MlError::Unavailable(format!("{}: {e}", path.display())))
}
