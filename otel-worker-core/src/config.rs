use anyhow::{Context, Result};
use std::env;
use std::fs::DirBuilder;
use std::path::PathBuf;
use tracing::{error, trace};

/// The directory name that is used for the otel-worker working directory. This
/// will contain the database, temporary files, etc.
pub const WORKING_DIRECTORY_NAME: &str = ".otel-worker";

/// The file name that is used to identify the database within otel-worker
/// working directory.
pub const DATABASE_FILENAME: &str = "data.db";

/// Find the otel-worker directory in the current directory or any parent
/// directories. This returns [`None`] if no otel-worker directory is found.
///
/// Any directory that is named `.otel-worker` is considered the otel-worker
/// directory.
pub fn find_otel_worker_dir() -> Option<PathBuf> {
    let Ok(cwd) = env::current_dir() else {
        error!("Failed to get current directory");
        return None;
    };

    let mut dir = Some(cwd);
    while let Some(inner_dir) = dir {
        let otel_worker_dir = inner_dir.join(WORKING_DIRECTORY_NAME);

        if otel_worker_dir.is_dir() {
            return Some(otel_worker_dir);
        }

        dir = inner_dir.parent().map(Into::into);
    }

    None
}

/// Ensure that the otel-worker directory exists and is initialized. It will
/// return a path to the otel-worker directory.
///
/// If override_path is provided than that path is used as the otel-worker
/// directory. It won't delete anything in that directory.
///
/// If no [`override_path`] is provided, it will search for the otel-worker
/// directory using the algorithm described in [`find_otel_worker_dir`]. If no
/// directory is found, then a '.otel-worker' directory is created in the
/// current directory.
pub fn initialize_otel_worker_dir(override_path: &Option<PathBuf>) -> Result<PathBuf> {
    trace!("Initializing otel-worker directory");
    let path = match override_path {
        Some(path) => {
            trace!(otel_worker_directory = ?path, "Using override path for otel-worker directory");
            path.to_path_buf()
        }
        None => match find_otel_worker_dir() {
            Some(path) => {
                trace!(otel_worker_directory = ?path, "Found otel-worker directory in a parent directory");
                return Ok(path);
            }
            None => {
                let path = env::current_dir()?.join(WORKING_DIRECTORY_NAME);
                trace!(otel_worker_directory = ?path, "No otel-worker directory found, using the current directory");
                path
            }
        },
    };

    DirBuilder::new()
        .recursive(true)
        .create(&path)
        .with_context(|| format!("Failed to create otel-worker working directory: {:?}", path))?;

    Ok(path)
}
