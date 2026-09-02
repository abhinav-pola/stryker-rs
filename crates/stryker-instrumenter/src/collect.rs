//! Read-only AST pass that collects mutation sites.
//!
//! Each site pairs a *placement* (where the runtime switch goes) with one or
//! more candidate mutants (sub-span + replacement text). All replacement text
//! is derived from the original source slices, never from codegen, so the
//! instrumented output preserves the file byte-for-byte outside mutated
//! regions.

use oxc_ast::ast::*;
use oxc_ast_visit::{Visit, walk};
use oxc_span::{GetSpan, Span};

use crate::directives::DirectiveIndex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Wrap the expression at this span in a switch ternary.
    Expression(Span),
    /// Replace the *contents* of the block at this span (braces excluded from
    /// inner_span) with an if/else switch.
    BlockBody { block: Span },
    /// A brace-less statement list (switch-case consequent): wrap in
    /// `if (...) {} else { ... }` without outer braces.
    Statements { span: Span },
}

impl Placement {
    pub fn span(&self) -> Span {
        match self {
            Placement::Expression(s) => *s,
            Placement::BlockBody { block } => *block,
            Placement::Statements { span } => *span,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub mutator: &'static str,
    /// Span being textually replaced (within the placement span).
    pub sub_span: Span,
    pub replacement: String,
    /// Reason when suppressed via `// Stryker disable`.
    pub ignored: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Site {
    pub placement: Placement,
    pub candidates: Vec<Candidate>,
}

pub struct Collector<'s> {
    source: &'s str,
    directives: &'s DirectiveIndex,
    excluded: &'s [String],
    /// Line number lookup for directive matching (1-based).
    line_of: Box<dyn Fn(u32) -> u32 + 's>,
    pub sites: Vec<Site>,
    /// Set when entering a constructor so its own body is not blanked
    /// (removing a derived-class `super()` call makes every run error).
    skip_block_once: bool,
    /// Spans whose true/false ConditionalExpression emission was already
    /// decided by an enclosing `&&`/`||` (operand rule: `&&` operands get
    /// only `true`, `||` operands only `false` — the other variant is
    /// equivalent to the parent's own true/false mutants).
    ce_decided: Vec<Span>,
    /// Loop test spans: `true` would make the loop infinite, so only the
    /// `false` mutant is emitted for expressions at these spans.
    no_true_spans: Vec<Span>,
    /// Span of the outermost member/call chain currently being descended.
    /// Expression mutants on the chain SPINE are placed here instead of at
    /// their own node: wrapping a mid-chain node in `(...)` would terminate
    /// an optional chain's short-circuit (`(a?.b).c` throws when `a` is
    /// nullish; `a?.b.c` doesn't). The mutated arm carries the whole chain.
    chain_root: Option<Span>,
}

impl<'s> Collector<'s> {
    pub fn new(
        source: &'s str,
        directives: &'s DirectiveIndex,
        excluded: &'s [String],
        line_of: impl Fn(u32) -> u32 + 's,
    ) -> Self {
        Self {
            source,
            directives,
            excluded,
            line_of: Box::new(line_of),
            sites: Vec::new(),
            skip_block_once: false,
            ce_decided: Vec::new(),
            no_true_spans: Vec::new(),
            chain_root: None,
        }
    }

    /// Enter a member/call chain node: returns the previous state and the
    /// placement-root guard. Off-spine subtrees (arguments, computed index
    /// expressions) must be visited with a cleared chain via `off_spine`.
    fn enter_chain(&mut self, span: Span) -> Option<Span> {
        let previous = self.chain_root;
        if previous.is_none() {
            self.chain_root = Some(span);
        }
        previous
    }

    fn exit_chain(&mut self, previous: Option<Span>) {
        self.chain_root = previous;
    }

    fn off_spine(&mut self, f: impl FnOnce(&mut Self)) {
        let saved = self.chain_root.take();
        f(self);
        self.chain_root = saved;
    }

    /// true/false ConditionalExpression pair, honoring loop-test suppression.
    fn add_bool_pair(&mut self, span: Span) {
        if !self.no_true_spans.contains(&span) {
            self.replace_expr("ConditionalExpression", span, "true".into());
        }
        self.replace_expr("ConditionalExpression", span, "false".into());
    }

    /// Restricted emission for a direct `&&`/`||` operand that is itself a
    /// boolean-shaped expression: `&&` operands only get `true`, `||`
    /// operands only get `false` (the other variant is equivalent to the
    /// parent's own mutant). Parens are transparent.
    fn decide_operand(&mut self, operand: &Expression<'_>, parent_op: LogicalOperator) {
        let inner = operand.get_inner_expression();
        let boolean_shaped = match inner {
            Expression::LogicalExpression(l) => {
                !matches!(l.operator, LogicalOperator::Coalesce)
            }
            Expression::BinaryExpression(b) => is_comparison(b.operator),
            _ => false,
        };
        let span = inner.span();
        if !boolean_shaped || self.ce_decided.contains(&span) {
            return;
        }
        self.ce_decided.push(span);
        match parent_op {
            LogicalOperator::And => {
                self.replace_expr("ConditionalExpression", span, "true".into());
            }
            LogicalOperator::Or => {
                self.replace_expr("ConditionalExpression", span, "false".into());
            }
            LogicalOperator::Coalesce => {}
        }
    }

    fn text(&self, span: Span) -> &'s str {
        &self.source[span.start as usize..span.end as usize]
    }

    /// Operand text for a rebuilt logical expression, parenthesized when the
    /// operand is itself a logical expression (associativity/mixing safety).
    fn parenthesized_operand(&self, operand: &Expression<'_>) -> String {
        let text = self.text(operand.span());
        if matches!(operand, Expression::LogicalExpression(_)) {
            format!("({text})")
        } else {
            text.to_string()
        }
    }

    fn add_expr(&mut self, mutator: &'static str, expr_span: Span, sub_span: Span, replacement: String) {
        // Hoist spine mutants to the enclosing chain root (see chain_root).
        let placement_span = match self.chain_root {
            Some(root) if root.start <= expr_span.start && root.end >= expr_span.end => root,
            _ => expr_span,
        };
        self.add(mutator, Placement::Expression(placement_span), sub_span, replacement);
    }

    /// Whole-expression replacement (sub span == placement span).
    fn replace_expr(&mut self, mutator: &'static str, span: Span, replacement: String) {
        self.add_expr(mutator, span, span, replacement);
    }

    fn add(&mut self, mutator: &'static str, placement: Placement, sub_span: Span, replacement: String) {
        // Identical replacement (e.g. `a < b` where operands render the same)
        // would be an equivalent mutant by construction.
        if self.text(sub_span) == replacement {
            return;
        }
        // Excluded mutators are still REPORTED as Ignored (stryker-js
        // behavior — keeps mutant counts comparable), just never placed.
        let ignored = if self.excluded.iter().any(|e| e == mutator) {
            Some(format!("Ignored because {mutator} is excluded (mutator.excludedMutations)"))
        } else {
            let line = (self.line_of)(sub_span.start);
            self.directives.disabled_reason(line, mutator).map(str::to_string)
        };
        let candidate = Candidate { mutator, sub_span, replacement, ignored };
        // Merge into an existing site with the same placement so co-located
        // mutants share one switch chain; drop exact duplicates (the same
        // mutant can be proposed from two visit paths, e.g. an if-test that
        // is also a comparison expression).
        if let Some(site) = self.sites.iter_mut().find(|s| s.placement == placement) {
            let duplicate = site.candidates.iter().any(|c| {
                c.mutator == candidate.mutator
                    && c.sub_span == candidate.sub_span
                    && c.replacement == candidate.replacement
            });
            if !duplicate {
                site.candidates.push(candidate);
            }
        } else {
            self.sites.push(Site { placement, candidates: vec![candidate] });
        }
    }
}

/// Level-1 regex mutations on the literal text `/pattern/flags` (mirrors
/// weapon-regex level 1, which stryker-js uses):
/// - strip a leading `^` / unescaped trailing `$` anchor
/// - flip EVERY character-class negation (`[abc]` ↔ `[^abc]`)
/// - flip every predefined class (`\d`↔`\D`, `\w`↔`\W`, `\s`↔`\S`)
/// - remove every `*` / `+` / `?` quantifier (with its lazy `?` if present)
fn regex_mutations(literal: &str) -> Vec<String> {
    let Some(end) = literal.rfind('/') else { return vec![] };
    let (body, flags) = literal.split_at(end);
    let Some(pattern) = body.strip_prefix('/') else { return vec![] };
    if pattern.is_empty() {
        return vec![];
    }
    let mut out: Vec<String> = Vec::new();
    let mut push = |new_pattern: String| {
        if new_pattern != pattern && !new_pattern.is_empty() {
            let candidate = format!("/{new_pattern}{flags}");
            if !out.contains(&candidate) {
                out.push(candidate);
            }
        }
    };

    if let Some(rest) = pattern.strip_prefix('^') {
        push(rest.to_string());
    }
    if let Some(rest) = pattern.strip_suffix('$') {
        let trailing_backslashes = rest.bytes().rev().take_while(|b| *b == b'\\').count();
        if trailing_backslashes % 2 == 0 {
            push(rest.to_string());
        }
    }

    let bytes = pattern.as_bytes();
    let mut in_class = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => {
                let c = bytes[i + 1];
                if matches!(c, b'd' | b'D' | b'w' | b'W' | b's' | b'S') {
                    let flipped = (c as char).to_ascii_uppercase();
                    let flipped =
                        if flipped as u8 == c { (c as char).to_ascii_lowercase() } else { flipped };
                    push(format!("{}{}{}", &pattern[..i + 1], flipped, &pattern[i + 2..]));
                }
                i += 2;
            }
            b'[' if !in_class => {
                in_class = true;
                if bytes.get(i + 1) == Some(&b'^') {
                    push(format!("{}{}", &pattern[..i + 1], &pattern[i + 2..]));
                    i += 2;
                } else {
                    push(format!("{}^{}", &pattern[..i + 1], &pattern[i + 1..]));
                    i += 1;
                }
            }
            b']' if in_class => {
                in_class = false;
                i += 1;
            }
            b'*' | b'+' | b'?' if !in_class => {
                // `?` directly after `(` is group syntax ((?:, (?=, ...), not
                // a quantifier.
                if bytes[i] == b'?' && i > 0 && bytes[i - 1] == b'(' {
                    i += 1;
                    continue;
                }
                // Remove the quantifier together with a lazy modifier.
                let mut cut_end = i + 1;
                if bytes[i] != b'?' && bytes.get(cut_end) == Some(&b'?') {
                    cut_end += 1;
                }
                push(format!("{}{}", &pattern[..i], &pattern[cut_end..]));
                i = cut_end;
            }
            _ => i += 1,
        }
    }
    out
}

/// `+`/`-` on strings is concatenation; mutating it to arithmetic yields NaN
/// noise, so skip when either operand is literally a string.
fn is_stringy(expr: &Expression<'_>) -> bool {
    match expr {
        Expression::StringLiteral(_) | Expression::TemplateLiteral(_) => true,
        Expression::BinaryExpression(b) => is_stringy(&b.left) || is_stringy(&b.right),
        _ => false,
    }
}

fn is_comparison(op: BinaryOperator) -> bool {
    use BinaryOperator::*;
    matches!(
        op,
        Equality | Inequality | StrictEquality | StrictInequality
            | LessThan | LessEqualThan | GreaterThan | GreaterEqualThan
    )
}

fn binary_operator_replacements(op: BinaryOperator) -> (&'static str, &'static [&'static str]) {
    use BinaryOperator::*;
    match op {
        Equality => ("EqualityOperator", &["!="]),
        Inequality => ("EqualityOperator", &["=="]),
        StrictEquality => ("EqualityOperator", &["!=="]),
        StrictInequality => ("EqualityOperator", &["==="]),
        LessThan => ("EqualityOperator", &["<=", ">="]),
        LessEqualThan => ("EqualityOperator", &["<", ">"]),
        GreaterThan => ("EqualityOperator", &[">=", "<="]),
        GreaterEqualThan => ("EqualityOperator", &[">", "<"]),
        Addition => ("ArithmeticOperator", &["-"]),
        Subtraction => ("ArithmeticOperator", &["+"]),
        Multiplication => ("ArithmeticOperator", &["/"]),
        Division => ("ArithmeticOperator", &["*"]),
        Remainder => ("ArithmeticOperator", &["*"]),
        _ => ("", &[]),
    }
}

const METHOD_SWAPS: &[(&str, &str)] = &[
    ("endsWith", "startsWith"),
    ("startsWith", "endsWith"),
    ("toUpperCase", "toLowerCase"),
    ("toLowerCase", "toUpperCase"),
    ("toLocaleUpperCase", "toLocaleLowerCase"),
    ("toLocaleLowerCase", "toLocaleUpperCase"),
    ("trimStart", "trimEnd"),
    ("trimEnd", "trimStart"),
    ("some", "every"),
    ("every", "some"),
    ("min", "max"),
    ("max", "min"),
];

/// Calls removed entirely: `a.filter(f)` → `a`.
const METHOD_REMOVALS: &[&str] = &[
    "trim", "substr", "substring", "sort", "reverse", "filter", "slice", "charAt",
];

impl<'a> Visit<'a> for Collector<'_> {
    // ---- skip type-space and other non-mutable regions ----

    fn visit_ts_type(&mut self, _t: &TSType<'a>) {
        // Never descend into type space.
    }

    fn visit_ts_type_annotation(&mut self, _a: &TSTypeAnnotation<'a>) {}

    fn visit_ts_type_parameter_declaration(&mut self, _d: &TSTypeParameterDeclaration<'a>) {}

    fn visit_ts_type_parameter_instantiation(&mut self, _i: &TSTypeParameterInstantiation<'a>) {}

    fn visit_ts_interface_declaration(&mut self, _d: &TSInterfaceDeclaration<'a>) {}

    fn visit_ts_type_alias_declaration(&mut self, _d: &TSTypeAliasDeclaration<'a>) {}

    fn visit_ts_enum_declaration(&mut self, _d: &TSEnumDeclaration<'a>) {
        // Enum initializers are const-ish contexts; skip entirely (v1).
    }

    fn visit_import_declaration(&mut self, _d: &ImportDeclaration<'a>) {}

    fn visit_export_all_declaration(&mut self, _d: &ExportAllDeclaration<'a>) {}

    fn visit_export_from_declaration(&mut self, _d: &ExportFromDeclaration<'a>) {
        // `export { x } from "./mod"` — the source string must stay literal.
    }

    fn visit_ts_external_module_declaration(&mut self, _d: &TSExternalModuleDeclaration<'a>) {
        // `declare module "zod" { ... }` — ambient; nothing runs.
    }

    fn visit_import_expression(&mut self, expr: &ImportExpression<'a>) {
        // Dynamic `import("...")`: the specifier must stay literal (bundlers
        // resolve it at build time). Only walk the options argument.
        if let Some(options) = &expr.options {
            self.visit_expression(options);
        }
    }

    fn visit_directive(&mut self, _d: &Directive<'a>) {
        // "use strict" prologues must stay literal.
    }

    fn visit_decorator(&mut self, _d: &Decorator<'a>) {}

    fn visit_ts_as_expression(&mut self, expr: &TSAsExpression<'a>) {
        if expr.type_annotation.is_const_type_reference() {
            return; // `x as const`: literal-typed, stryker-js skips entirely
        }
        walk::walk_ts_as_expression(self, expr);
    }

    fn visit_object_property(&mut self, prop: &ObjectProperty<'a>) {
        // Non-computed keys ({"key": v}) are not expressions to mutate.
        if prop.computed {
            self.visit_property_key(&prop.key);
        }
        self.visit_expression(&prop.value);
    }

    fn visit_binding_property(&mut self, prop: &BindingProperty<'a>) {
        // Destructuring pattern keys ({ "data-testid": x }) are bindings,
        // not expressions — only computed keys and the value can be mutated
        // (the value may carry a default expression).
        if prop.computed {
            self.visit_property_key(&prop.key);
        }
        self.visit_binding_pattern(&prop.value);
    }

    fn visit_jsx_attribute(&mut self, attr: &JSXAttribute<'a>) {
        // A plain string attribute value (attr="x") cannot host a ternary —
        // that needs an expression container. Only walk container values.
        if let Some(JSXAttributeValue::ExpressionContainer(container)) = &attr.value {
            self.visit_jsx_expression(&container.expression);
        }
    }

    fn visit_tagged_template_expression(&mut self, expr: &TaggedTemplateExpression<'a>) {
        // Splicing a ternary at the template span would turn `tag\`x\`` into
        // a call. Walk only the interpolated expressions.
        self.visit_expression(&expr.tag);
        for e in &expr.quasi.expressions {
            self.visit_expression(e);
        }
    }

    // ---- mutators keyed on owning node ----

    fn visit_if_statement(&mut self, stmt: &IfStatement<'a>) {
        let test_span = stmt.test.span();
        self.add_expr("ConditionalExpression", test_span, test_span, "true".into());
        self.add_expr("ConditionalExpression", test_span, test_span, "false".into());
        walk::walk_if_statement(self, stmt);
    }

    fn visit_while_statement(&mut self, stmt: &WhileStatement<'a>) {
        let test_span = stmt.test.span();
        self.no_true_spans.push(test_span);
        self.add_expr("ConditionalExpression", test_span, test_span, "false".into());
        walk::walk_while_statement(self, stmt);
    }

    fn visit_do_while_statement(&mut self, stmt: &DoWhileStatement<'a>) {
        let test_span = stmt.test.span();
        self.no_true_spans.push(test_span);
        self.add_expr("ConditionalExpression", test_span, test_span, "false".into());
        walk::walk_do_while_statement(self, stmt);
    }

    fn visit_for_statement(&mut self, stmt: &ForStatement<'a>) {
        if let Some(test) = &stmt.test {
            let test_span = test.span();
            self.no_true_spans.push(test_span);
            self.add_expr("ConditionalExpression", test_span, test_span, "false".into());
        }
        walk::walk_for_statement(self, stmt);
    }

    fn visit_binary_expression(&mut self, expr: &BinaryExpression<'a>) {
        let (mutator, replacements) = binary_operator_replacements(expr.operator);
        let skip_stringy = matches!(
            expr.operator,
            BinaryOperator::Addition | BinaryOperator::Subtraction
        ) && (is_stringy(&expr.left) || is_stringy(&expr.right));
        if !replacements.is_empty() && !skip_stringy {
            let left = self.text(expr.left.span());
            let right = self.text(expr.right.span());
            for op in replacements {
                self.replace_expr(mutator, expr.span, format!("{left} {op} {right}"));
            }
        }
        // A comparison is a boolean expression: also offer true/false
        // (stryker-js ConditionalExpression). Direct `&&`/`||` operands were
        // already emitted (restricted) by the parent's visit.
        if is_comparison(expr.operator) && !self.ce_decided.contains(&expr.span) {
            self.add_bool_pair(expr.span);
        }
        walk::walk_binary_expression(self, expr);
    }

    fn visit_logical_expression(&mut self, expr: &LogicalExpression<'a>) {
        let replacement_op = match expr.operator {
            LogicalOperator::And => Some("||"),
            LogicalOperator::Or => Some("&&"),
            LogicalOperator::Coalesce => Some("&&"),
        };
        if let Some(op) = replacement_op {
            // Compound logical operands must be parenthesized: `a ?? b ?? c`
            // with the outer operator swapped would otherwise reconstruct as
            // `a ?? b && c`, which is a SyntaxError (`??` cannot mix with
            // `&&`/`||` unparenthesized). Matches stryker-js codegen output.
            let left = self.parenthesized_operand(&expr.left);
            let right = self.parenthesized_operand(&expr.right);
            self.replace_expr("LogicalOperator", expr.span, format!("{left} {op} {right}"));
        }
        let boolean_op = !matches!(expr.operator, LogicalOperator::Coalesce);
        if boolean_op {
            if !self.ce_decided.contains(&expr.span) {
                self.add_bool_pair(expr.span);
            }
            self.decide_operand(&expr.left, expr.operator);
            self.decide_operand(&expr.right, expr.operator);
        }
        walk::walk_logical_expression(self, expr);
    }

    fn visit_switch_case(&mut self, case: &SwitchCase<'a>) {
        // Make the case body unreachable (stryker-js switch-case placer).
        // The mutant is reported at the `case` keyword with the bare label
        // as replacement, exactly like stryker-js.
        if let (Some(first), Some(last)) = (case.consequent.first(), case.consequent.last()) {
            let statements = Span::new(first.span().start, last.span().end);
            let label = self.text(Span::new(case.span.start, first.span().start));
            self.add(
                "ConditionalExpression",
                Placement::Statements { span: statements },
                Span::new(case.span.start, last.span().end),
                label.trim_end().to_string(),
            );
        }
        walk::walk_switch_case(self, case);
    }

    fn visit_boolean_literal(&mut self, lit: &BooleanLiteral) {
        let replacement = if lit.value { "false" } else { "true" };
        self.replace_expr("BooleanLiteral", lit.span, replacement.into());
    }

    fn visit_string_literal(&mut self, lit: &StringLiteral<'a>) {
        let replacement =
            if lit.value.is_empty() { "\"Stryker was here!\"" } else { "\"\"" };
        self.replace_expr("StringLiteral", lit.span, replacement.into());
    }

    fn visit_template_literal(&mut self, lit: &TemplateLiteral<'a>) {
        let is_empty = lit.expressions.is_empty()
            && lit.quasis.iter().all(|q| q.value.raw.is_empty());
        let replacement = if is_empty { "`Stryker was here!`" } else { "``" };
        self.replace_expr("StringLiteral", lit.span, replacement.into());
        walk::walk_template_literal(self, lit);
    }

    fn visit_object_expression(&mut self, expr: &ObjectExpression<'a>) {
        if !expr.properties.is_empty() {
            self.replace_expr("ObjectLiteral", expr.span, "{}".into());
        }
        walk::walk_object_expression(self, expr);
    }

    fn visit_reg_exp_literal(&mut self, lit: &RegExpLiteral<'a>) {
        let text = self.text(lit.span);
        for mutated in regex_mutations(text) {
            self.replace_expr("Regex", lit.span, mutated);
        }
    }

    fn visit_unary_expression(&mut self, expr: &UnaryExpression<'a>) {
        match expr.operator {
            UnaryOperator::LogicalNot => {
                // `!x` → `x` (BooleanLiteral mutator in stryker-js terms).
                let arg = self.text(expr.argument.span());
                self.replace_expr("BooleanLiteral", expr.span, arg.to_string());
            }
            UnaryOperator::UnaryNegation => {
                let arg = self.text(expr.argument.span());
                self.replace_expr("UnaryOperator", expr.span, format!("+{arg}"));
            }
            UnaryOperator::UnaryPlus => {
                let arg = self.text(expr.argument.span());
                self.replace_expr("UnaryOperator", expr.span, format!("-{arg}"));
            }
            UnaryOperator::BitwiseNot => {
                let arg = self.text(expr.argument.span());
                self.replace_expr("UnaryOperator", expr.span, arg.to_string());
            }
            _ => {}
        }
        walk::walk_unary_expression(self, expr);
    }

    fn visit_update_expression(&mut self, expr: &UpdateExpression<'a>) {
        let (from, to) = match expr.operator {
            UpdateOperator::Increment => ("++", "--"),
            UpdateOperator::Decrement => ("--", "++"),
        };
        let text = self.text(expr.span);
        // Replace only the operator token to keep prefix/postfix shape.
        let replaced = if expr.prefix {
            text.replacen(from, to, 1)
        } else {
            match text.rfind(from) {
                Some(idx) => {
                    let mut s = text.to_string();
                    s.replace_range(idx..idx + from.len(), to);
                    s
                }
                None => return,
            }
        };
        self.replace_expr("UpdateOperator", expr.span, replaced);
        walk::walk_update_expression(self, expr);
    }

    fn visit_assignment_expression(&mut self, expr: &AssignmentExpression<'a>) {
        use AssignmentOperator::*;
        let replacement_op = match expr.operator {
            Addition => {
                if is_stringy(&expr.right) { None } else { Some("-=") }
            }
            Subtraction => Some("+="),
            Multiplication => Some("/="),
            Division => Some("*="),
            Remainder => Some("*="),
            ShiftLeft => Some(">>="),
            ShiftRight => Some("<<="),
            BitwiseAnd => Some("|="),
            BitwiseOR => Some("&="),
            LogicalAnd => Some("||="),
            LogicalOr => Some("&&="),
            LogicalNullish => Some("&&="),
            _ => None,
        };
        if let Some(op) = replacement_op {
            let left = self.text(expr.left.span());
            let right = self.text(expr.right.span());
            self.replace_expr("AssignmentOperator", expr.span, format!("{left} {op} {right}"));
        }
        walk::walk_assignment_expression(self, expr);
    }

    fn visit_array_expression(&mut self, expr: &ArrayExpression<'a>) {
        if expr.elements.is_empty() {
            self.replace_expr("ArrayDeclaration", expr.span, "[\"Stryker was here\"]".into());
        } else {
            self.replace_expr("ArrayDeclaration", expr.span, "[]".into());
        }
        walk::walk_array_expression(self, expr);
    }

    fn visit_arrow_function_expression(&mut self, expr: &ArrowFunctionExpression<'a>) {
        // Whole-arrow replacement, matching stryker-js: `(a) => a + 1`
        // becomes `() => undefined` (only for concise bodies).
        if let Some(body_expr) = expr.get_expression() {
            if self.text(body_expr.span()) != "undefined" {
                self.add_expr("ArrowFunction", expr.span, expr.span, "() => undefined".into());
            }
        }
        walk::walk_arrow_function_expression(self, expr);
    }

    fn visit_call_expression(&mut self, expr: &CallExpression<'a>) {
        // `require("...")` specifiers stay literal, like import sources;
        // `Symbol("desc")` descriptions are identity-bearing (stryker-js
        // skips them too).
        if let Expression::Identifier(ident) = &expr.callee {
            if ident.name == "require" || ident.name == "Symbol" {
                return;
            }
        }
        let chain = self.enter_chain(expr.span);
        // OptionalChaining: `f?.(x)` -> `f(x)`.
        if expr.optional {
            let callee_end = expr.callee.span().end as usize;
            let call_text = self.text(expr.span);
            let rel = &call_text[callee_end - expr.span.start as usize..];
            if let Some(q) = rel.find("?.") {
                let offset = callee_end - expr.span.start as usize + q;
                let mut replaced = call_text.to_string();
                replaced.replace_range(offset..offset + 2, "");
                self.replace_expr("OptionalChaining", expr.span, replaced);
            }
        }
        // MethodExpression: swap or strip well-known method calls (optional
        // chains included — the swap splices only the method name, and the
        // removal text is the callee's object, valid in both forms).
        if let Some(member) = expr.callee.as_member_expression() {
            if let MemberExpression::StaticMemberExpression(static_member) = member {
                let name = static_member.property.name.as_str();
                if let Some((_, to)) = METHOD_SWAPS.iter().find(|(from, _)| *from == name) {
                    let call_text = self.text(expr.span);
                    let prop = static_member.property.span;
                    let start = (prop.start - expr.span.start) as usize;
                    let end = (prop.end - expr.span.start) as usize;
                    let mut replaced = call_text.to_string();
                    replaced.replace_range(start..end, to);
                    self.replace_expr("MethodExpression", expr.span, replaced);
                } else if METHOD_REMOVALS.contains(&name) {
                    let object = self.text(static_member.object.span());
                    self.replace_expr("MethodExpression", expr.span, object.to_string());
                }
            }
        }
        // Callee stays on the spine; arguments and type args do not.
        self.visit_expression(&expr.callee);
        self.off_spine(|c| {
            for argument in &expr.arguments {
                c.visit_argument(argument);
            }
        });
        self.exit_chain(chain);
    }

    fn visit_static_member_expression(&mut self, expr: &StaticMemberExpression<'a>) {
        let chain = self.enter_chain(expr.span);
        if expr.optional {
            // `a?.b` → `a.b`
            let object = self.text(expr.object.span());
            let property = expr.property.name.as_str();
            self.replace_expr("OptionalChaining", expr.span, format!("{object}.{property}"));
        }
        self.visit_expression(&expr.object);
        self.exit_chain(chain);
    }

    fn visit_computed_member_expression(&mut self, expr: &ComputedMemberExpression<'a>) {
        let chain = self.enter_chain(expr.span);
        if expr.optional {
            // `a?.[i]` → `a[i]`
            let object = self.text(expr.object.span());
            let index = self.text(expr.expression.span());
            self.replace_expr("OptionalChaining", expr.span, format!("{object}[{index}]"));
        }
        self.visit_expression(&expr.object);
        self.off_spine(|c| c.visit_expression(&expr.expression));
        self.exit_chain(chain);
    }

    fn visit_method_definition(&mut self, def: &MethodDefinition<'a>) {
        // Blanking a constructor that calls super() breaks every run;
        // super()-free constructors are fair game (stryker-js parity).
        if def.kind == MethodDefinitionKind::Constructor {
            if let Some(body) = &def.value.body {
                if self.text(body.span).contains("super(") {
                    self.skip_block_once = true;
                }
            }
        }
        walk::walk_method_definition(self, def);
        self.skip_block_once = false;
    }

    fn visit_function_body(&mut self, body: &FunctionBody<'a>) {
        // BlockStatement mutator: empty out non-empty function bodies.
        // Guards: arrow expression bodies have no braces (handled by
        // ArrowFunction); bodies with directive prologues ("use strict")
        // can't be wrapped in an else-block; constructor bodies are skipped
        // (super() removal). oxc keeps directives separate from statements.
        let skip = std::mem::replace(&mut self.skip_block_once, false);
        if !skip
            && !body.statements.is_empty()
            && body.directives.is_empty()
            && self.text(body.span).starts_with('{')
        {
            self.add(
                "BlockStatement",
                Placement::BlockBody { block: body.span },
                body.span,
                "{}".into(),
            );
        }
        walk::walk_function_body(self, body);
    }

    fn visit_block_statement(&mut self, block: &BlockStatement<'a>) {
        if !block.body.is_empty() {
            self.add(
                "BlockStatement",
                Placement::BlockBody { block: block.span },
                block.span,
                "{}".into(),
            );
        }
        walk::walk_block_statement(self, block);
    }
}
