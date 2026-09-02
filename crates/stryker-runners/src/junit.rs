//! Parser for bun's `--reporter=junit` output.
//!
//! Shape (probed on bun 1.3.14, see js/bun-preload.ts):
//! - nested `<testsuite>` per describe, one outer testsuite per file
//!   (its `name` equals its `file` attribute);
//! - every testcase appears in DOCUMENT ORDER, executed ones without a
//!   `<skipped>` child — that order is the ordinal-correlation contract;
//! - `<failure>` children carry a type but no message text;
//! - `classname` is double-escaped and innermost-first, so describe paths
//!   are reconstructed from testsuite nesting instead.

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

#[derive(Debug, Clone)]
pub struct JunitCase {
    pub file: Option<String>,
    /// Describe path, outermost first.
    pub describes: Vec<String>,
    pub name: String,
    pub time_ms: f64,
    pub failed: bool,
    pub skipped: bool,
}

impl JunitCase {
    /// Stable test id: `<file> > <describe> > ... > <name>`.
    pub fn test_id(&self) -> String {
        let mut parts: Vec<&str> = Vec::with_capacity(self.describes.len() + 2);
        if let Some(file) = &self.file {
            parts.push(file);
        }
        parts.extend(self.describes.iter().map(String::as_str));
        parts.push(&self.name);
        parts.join(" > ")
    }
}

pub fn parse_junit(xml: &str) -> anyhow::Result<Vec<JunitCase>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut cases: Vec<JunitCase> = Vec::new();
    // (name, is_file_suite): file-level suites are not describes.
    let mut suite_stack: Vec<(String, bool)> = Vec::new();
    let mut current: Option<JunitCase> = None;

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => match e.name().as_ref() {
                b"testsuite" => suite_stack.push(read_suite(&e)?),
                b"testcase" => current = Some(read_case(&e, &suite_stack)?),
                b"failure" | b"error" => mark(&mut current, |c| c.failed = true),
                b"skipped" => mark(&mut current, |c| c.skipped = true),
                _ => {}
            },
            Event::Empty(e) => match e.name().as_ref() {
                b"testsuite" => {} // self-closing suite has no testcases
                b"testcase" => cases.push(read_case(&e, &suite_stack)?),
                b"failure" | b"error" => mark(&mut current, |c| c.failed = true),
                b"skipped" => mark(&mut current, |c| c.skipped = true),
                _ => {}
            },
            Event::End(e) => match e.name().as_ref() {
                b"testsuite" => {
                    suite_stack.pop();
                }
                b"testcase" => {
                    if let Some(case) = current.take() {
                        cases.push(case);
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(cases)
}

fn mark(current: &mut Option<JunitCase>, f: impl FnOnce(&mut JunitCase)) {
    if let Some(case) = current.as_mut() {
        f(case);
    }
}

fn read_suite(e: &BytesStart<'_>) -> anyhow::Result<(String, bool)> {
    let mut name = String::new();
    let mut file: Option<String> = None;
    for attr in e.attributes().flatten() {
        match attr.key.as_ref() {
            b"name" => name = attr.unescape_value()?.into_owned(),
            b"file" => file = Some(attr.unescape_value()?.into_owned()),
            _ => {}
        }
    }
    let is_file_suite = file.as_deref() == Some(name.as_str());
    Ok((name, is_file_suite))
}

fn read_case(e: &BytesStart<'_>, suite_stack: &[(String, bool)]) -> anyhow::Result<JunitCase> {
    let mut name = String::new();
    let mut file: Option<String> = None;
    let mut time_ms = 0.0f64;
    for attr in e.attributes().flatten() {
        match attr.key.as_ref() {
            b"name" => name = attr.unescape_value()?.into_owned(),
            b"file" => file = Some(attr.unescape_value()?.into_owned()),
            b"time" => time_ms = attr.unescape_value()?.parse::<f64>().unwrap_or(0.0) * 1000.0,
            _ => {}
        }
    }
    let describes: Vec<String> = suite_stack
        .iter()
        .filter(|(_, is_file)| !is_file)
        .map(|(n, _)| n.clone())
        .collect();
    Ok(JunitCase { file, describes, name, time_ms, failed: false, skipped: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="bun test" tests="7" failures="2" skipped="2" time="0.013">
  <testsuite name="fail.test.ts" file="fail.test.ts" tests="2">
    <testcase name="boom" classname="" time="0.000165" file="fail.test.ts" line="2" assertions="0">
      <failure type="AssertionError" />
    </testcase>
    <testcase name="ok" classname="" time="0.0001" file="fail.test.ts" line="3" assertions="1" />
  </testsuite>
  <testsuite name="mixed.test.ts" file="mixed.test.ts" tests="5">
    <testsuite name="suite" file="mixed.test.ts" line="3" tests="4">
      <testcase name="alpha" classname="suite" time="0.000012" file="mixed.test.ts" line="4" assertions="1" />
      <testcase name="beta" classname="suite" time="0" file="mixed.test.ts" line="5" assertions="0">
        <skipped />
      </testcase>
    </testsuite>
    <testcase name="epsilon" classname="" time="0.000004" file="mixed.test.ts" line="9" assertions="1" />
  </testsuite>
</testsuites>"#;

    #[test]
    fn parses_nested_structure() {
        let cases = parse_junit(SAMPLE).unwrap();
        assert_eq!(cases.len(), 5);
        assert_eq!(cases[0].test_id(), "fail.test.ts > boom");
        assert!(cases[0].failed);
        assert!(!cases[1].failed);
        assert_eq!(cases[2].test_id(), "mixed.test.ts > suite > alpha");
        assert!(cases[3].skipped);
        assert_eq!(cases[4].test_id(), "mixed.test.ts > epsilon");
        assert!((cases[2].time_ms - 0.012).abs() < 1e-9);
        // Executed cases in document order (the ordinal contract).
        let executed: Vec<String> =
            cases.iter().filter(|c| !c.skipped).map(|c| c.test_id()).collect();
        assert_eq!(
            executed,
            vec![
                "fail.test.ts > boom",
                "fail.test.ts > ok",
                "mixed.test.ts > suite > alpha",
                "mixed.test.ts > epsilon"
            ]
        );
    }
}
