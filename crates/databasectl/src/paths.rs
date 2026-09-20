use crate::error::{Error, Result};
use std::path::PathBuf;

/// Returns the base directory for ClickHouse CLI (~/.dctl/)
pub fn base_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| {
        Error::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine home directory",
        ))
    })?;
    Ok(home.join(".dctl"))
}

/// Returns the custom server configs directory (~/.dctl/configs/)
pub fn configs_dir() -> Result<PathBuf> {
    Ok(base_dir()?.join("configs"))
}
