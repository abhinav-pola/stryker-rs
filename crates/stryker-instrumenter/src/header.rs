//! The runtime header prepended to every instrumented file.
//!
//! A close port of stryker-js's instrumentation helpers (same `_9fa48` name
//! suffix, same env activation contract) so anyone reading sandbox output
//! recognizes the shape. Self-rewriting functions keep the hot path cheap.
//!
//! Activation: `__STRYKER_ACTIVE_MUTANT__` env var, read once at first load.
//! Hit limit: `__STRYKER_HIT_LIMIT__` env var; exceeding it throws, which the
//! runner maps to Timeout (infinite-loop detection).

/// `namespace` is the global object property, default `__stryker__`.
pub fn header(namespace: &str, ts_nocheck: bool) -> String {
    let nocheck = if ts_nocheck { "// @ts-nocheck\n" } else { "" };
    format!(
        r#"{nocheck}/* stryker-rs instrumentation header */
function stryNS_9fa48() {{
  var g = typeof globalThis === "object" && globalThis || typeof global === "object" && global || this;
  var ns = g.{namespace} || (g.{namespace} = {{}});
  var env = typeof process === "object" && process && process.env || {{}};
  if (ns.activeMutant === undefined && env.__STRYKER_ACTIVE_MUTANT__) {{
    ns.activeMutant = env.__STRYKER_ACTIVE_MUTANT__;
  }}
  if (ns.hitLimit === undefined && env.__STRYKER_HIT_LIMIT__) {{
    ns.hitLimit = parseInt(env.__STRYKER_HIT_LIMIT__, 10);
    ns.hitCount = 0;
  }}
  function retrieveNS() {{ return ns; }}
  stryNS_9fa48 = retrieveNS;
  return retrieveNS();
}}
stryNS_9fa48();
function stryCov_9fa48() {{
  var ns = stryNS_9fa48();
  var cov = ns.mutantCoverage || (ns.mutantCoverage = {{ static: {{}}, perTest: {{}} }});
  function cover() {{
    var c = cov.static;
    if (ns.currentTestId !== undefined) {{
      c = cov.perTest[ns.currentTestId] = cov.perTest[ns.currentTestId] || {{}};
    }}
    var a = arguments;
    for (var i = 0; i < a.length; i++) {{
      c[a[i]] = (c[a[i]] || 0) + 1;
    }}
  }}
  stryCov_9fa48 = cover;
  cover.apply(null, arguments);
}}
function stryMutAct_9fa48(id) {{
  var ns = stryNS_9fa48();
  function isActive(id) {{
    if (ns.activeMutant === id) {{
      if (ns.hitCount !== undefined && ++ns.hitCount > ns.hitLimit) {{
        throw new Error("Stryker: Hit limit reached (" + ns.hitCount + ")");
      }}
      return true;
    }}
    return false;
  }}
  stryMutAct_9fa48 = isActive;
  return isActive(id);
}}
"#
    )
}

/// Marker present in every instrumented file; `stryker restore` greps for it
/// as a second-pass safety net.
pub const HEADER_MARKER: &str = "/* stryker-rs instrumentation header */";
