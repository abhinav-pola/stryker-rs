//! Single-file HTML report embedding mutation-testing-elements (pinned
//! 3.9.0, vendored in js/mutation-testing-elements/).
//!
//! The report JSON is assigned via property binding (`app.report = ...`)
//! so the file works from file://. Every `<` in the JSON is emitted as
//! `\u003c` so embedded source code cannot close the script tag.

use crate::schema::MutationTestResult;

const ELEMENTS_BUNDLE: &str =
    include_str!("../../../js/mutation-testing-elements/mutation-test-elements.js");

pub fn render(report: &MutationTestResult) -> anyhow::Result<String> {
    let json = serde_json::to_string(report)?;
    let escaped = json.replace('<', "\\u003c");
    Ok(format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Mutation test report</title>
  <script>{ELEMENTS_BUNDLE}</script>
</head>
<body>
  <mutation-test-report-app title-postfix="stryker-rs">
    Your browser doesn't support custom elements.
  </mutation-test-report-app>
  <script>
    const app = document.querySelector('mutation-test-report-app');
    app.report = {escaped};
    function updateTheme() {{
      document.body.style.backgroundColor = app.themeBackgroundColor;
    }}
    app.addEventListener('theme-changed', updateTheme);
    updateTheme();
  </script>
</body>
</html>
"#
    ))
}

pub fn write(report: &MutationTestResult, path: &camino::Utf8Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, render(report)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::*;
    use std::collections::BTreeMap;

    #[test]
    fn escapes_script_breakout() {
        let mut files = BTreeMap::new();
        files.insert(
            "src/evil.ts".to_string(),
            FileResult {
                language: "typescript".into(),
                source: "const x = \"</script><script>alert(1)</script>\";".into(),
                mutants: vec![],
            },
        );
        let report = MutationTestResult {
            schema_version: SCHEMA_VERSION.into(),
            thresholds: SchemaThresholds { high: 80.0, low: 60.0 },
            project_root: None,
            files,
            test_files: None,
            framework: None,
            config: None,
            performance: None,
        };
        let html = render(&report).unwrap();
        // The only literal `</script>` closers must be ours (bundle + binding).
        assert!(!html.contains("</script><script>alert"));
        assert!(html.contains("\\u003c/script>"));
    }
}
