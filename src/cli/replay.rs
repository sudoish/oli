//! Provider-free transcript replay command.

use std::path::Path;

use crate::error::Result;

/// Compare a replay fixture and print the report as pretty JSON.
pub fn run(fixture: &Path) -> Result<()> {
    let report = crate::replay::compare_path(fixture)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
