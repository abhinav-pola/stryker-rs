pub mod clear_text;
pub mod html;
pub mod report;
pub mod schema;

use camino::Utf8Path;

/// Write the JSON report, creating parent directories.
pub fn write_json_report(
    report: &schema::MutationTestResult,
    path: &Utf8Path,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string(report)?)?;
    Ok(())
}
