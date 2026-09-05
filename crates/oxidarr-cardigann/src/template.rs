//! A minimal Go-template subset — only the forms the Cardigann v11 corpus
//! actually uses: the `if`/`else`/`end`, `range`/`end`, `or`, `join` and
//! `re_replace` directives; the `and`, `or`, `eq` and `ne` functions; and
//! the `.Keywords`, `.Config.*`, `.Result.*`, `.Categories`, `.False` and
//! `.True` variables.
//!
//! This is deliberately not a general Go-template engine: it recognises
//! exactly the shapes observed across the 543-definition corpus and
//! reports an error for anything else, rather than silently guessing.

use crate::error::CardigannError;
use std::collections::HashMap;

/// Variable bindings available to a template.
#[derive(Debug, Default, Clone)]
pub struct Scope {
    keywords: String,
    config: HashMap<String, String>,
    result: HashMap<String, String>,
    categories: Vec<String>,
}

impl Scope {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_keywords(&mut self, s: &str) {
        self.keywords = s.to_string();
    }

    pub fn set_config(&mut self, k: &str, v: &str) {
        self.config.insert(k.to_string(), v.to_string());
    }

    pub fn set_result(&mut self, k: &str, v: &str) {
        self.result.insert(k.to_string(), v.to_string());
    }

    /// Binds the selected category IDs.
    ///
    /// Not part of the brief's original interface: every `{{ range ... }}`
    /// and `{{ join ... }}` in the corpus iterates `.Categories`, never
    /// `.Config.*`, and a category selection is inherently a list rather
    /// than a single string, so `set_config` cannot express it. See the
    /// task report for the corpus evidence.
    pub fn set_categories<I, S>(&mut self, ids: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.categories = ids.into_iter().map(Into::into).collect();
    }

    fn lookup(&self, path: &str) -> String {
        match path {
            ".Keywords" => self.keywords.clone(),
            // Falsy by construction: 74 corpus files map a raw boolean
            // through `case: { False: "{{ .False }}", True: "{{ .True }}" }`
            // and then feed the result straight into a bare truthy check,
            // e.g. `{{ if .Result._internal }}Internal{{ else }}{{ end }}`
            // (blutopia-api.yml, nobs.yml, luminarr-api.yml, +71 more). For
            // that idiom to do anything at all, `.False` must render falsy
            // (empty) and `.True` must render truthy (non-empty) — Go
            // template truthiness treats the empty string as the only
            // falsy string, so `.False` cannot be a non-empty literal
            // without making every one of those checks unconditionally
            // true. See the task report for the corpus evidence.
            ".False" => String::new(),
            ".True" => "True".to_string(),
            p => {
                if let Some(name) = p.strip_prefix(".Config.") {
                    self.config.get(name).cloned().unwrap_or_default()
                } else if let Some(name) = p.strip_prefix(".Result.") {
                    self.result.get(name).cloned().unwrap_or_default()
                } else {
                    // Unrecognised paths (`.Query.*`, `.DownloadUri.*`, ...)
                    // are outside this task's scope; rendering them as
                    // empty matches Go's behaviour for a nil field and
                    // keeps unknown forms from turning into hard errors.
                    String::new()
                }
            }
        }
    }
}

/// A parsed condition/value expression, e.g. the argument of `if`, `and`,
/// `or`, `eq`, `ne`, or the second argument of `join`.
#[derive(Debug, Clone)]
enum Expr {
    /// A dotted variable path such as `.Config.sort`, or `.` (the current
    /// `range` loop item).
    Path(String),
    /// A quoted string literal such as `"NULL"`.
    Str(String),
    /// A function call: `and`, `or`, `eq` or `ne`, with its arguments.
    Call(String, Vec<Expr>),
}

/// One parsed piece of a template.
#[derive(Debug)]
enum Node {
    Text(String),
    Var(String),
    /// `{{ or a b ... }}` — the first non-empty operand's value.
    Or(Vec<Expr>),
    /// `{{ join .Categories "," }}`
    Join {
        list: String,
        sep: Expr,
    },
    /// `{{ re_replace VALUE "pattern" "replacement" }}`
    ReReplace {
        value: Expr,
        pattern: Expr,
        replacement: Expr,
    },
    /// `{{ range .Categories }}...{{.}}...{{ end }}`
    Range {
        list: String,
        body: Vec<Node>,
    },
    /// `{{ if COND }}then{{ else }}else{{ end }}`
    If {
        cond: Expr,
        then: Vec<Node>,
        otherwise: Vec<Node>,
    },
}

/// Renders a Cardigann template against `scope`.
///
/// # Errors
///
/// Returns [`CardigannError::Template`] if `template` contains an action
/// this engine does not recognise, an `if`/`range` that is never closed by
/// a matching `end`, a condition with unbalanced parentheses, or a
/// `re_replace` whose pattern is not a valid regular expression.
pub fn render(template: &str, scope: &Scope) -> Result<String, CardigannError> {
    let nodes = parse(template)?;
    let mut out = String::new();
    let ctx = Ctx { scope, item: None };
    render_nodes(&nodes, &ctx, &mut out)?;
    Ok(out)
}

/// Rendering context: the bound [`Scope`] plus, inside a `range` body, the
/// current loop item (bound to the path `.`).
struct Ctx<'a> {
    scope: &'a Scope,
    item: Option<&'a str>,
}

impl Ctx<'_> {
    fn value(&self, path: &str) -> String {
        if path == "." {
            self.item.unwrap_or_default().to_string()
        } else {
            self.scope.lookup(path)
        }
    }

    /// Go templates treat the empty string (and, for lists, the empty
    /// list) as falsy. `.Categories` is the corpus's one list-valued
    /// variable, so its truthiness is "is the list non-empty" rather than
    /// "is its (nonexistent) string form non-empty".
    fn truthy(&self, path: &str) -> bool {
        if path == ".Categories" {
            !self.scope.categories.is_empty()
        } else {
            !self.value(path).is_empty()
        }
    }
}

fn eval_str(expr: &Expr, ctx: &Ctx) -> String {
    match expr {
        Expr::Path(p) => ctx.value(p),
        Expr::Str(s) => s.clone(),
        // and/or/eq/ne are never used as value producers in the corpus;
        // rendering the boolean as Go's own string form is a conservative
        // fallback for a shape that has not actually been observed.
        Expr::Call(..) => {
            if eval_bool(expr, ctx) {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
    }
}

fn eval_bool(expr: &Expr, ctx: &Ctx) -> bool {
    match expr {
        Expr::Path(p) => ctx.truthy(p),
        Expr::Str(s) => !s.is_empty(),
        Expr::Call(name, args) => match name.as_str() {
            "and" => args.iter().all(|a| eval_bool(a, ctx)),
            "or" => args.iter().any(|a| eval_bool(a, ctx)),
            "eq" => args.len() == 2 && eval_str(&args[0], ctx) == eval_str(&args[1], ctx),
            "ne" => args.len() == 2 && eval_str(&args[0], ctx) != eval_str(&args[1], ctx),
            _ => false,
        },
    }
}

fn render_nodes(nodes: &[Node], ctx: &Ctx, out: &mut String) -> Result<(), CardigannError> {
    for node in nodes {
        match node {
            Node::Text(t) => out.push_str(t),
            Node::Var(path) => out.push_str(&ctx.value(path)),
            Node::Or(args) => {
                let value = args
                    .iter()
                    .map(|a| eval_str(a, ctx))
                    .find(|v| !v.is_empty())
                    .unwrap_or_default();
                out.push_str(&value);
            }
            Node::Join { list, sep } => {
                if list == ".Categories" {
                    let sep = eval_str(sep, ctx);
                    out.push_str(&ctx.scope.categories.join(&sep));
                }
                // Any other list source is outside this task's scope;
                // rendering nothing matches the "unknown path is empty"
                // fallback used everywhere else in this module.
            }
            Node::Range { list, body } => {
                if list == ".Categories" {
                    for item in &ctx.scope.categories {
                        let item_ctx = Ctx {
                            scope: ctx.scope,
                            item: Some(item.as_str()),
                        };
                        render_nodes(body, &item_ctx, out)?;
                    }
                }
            }
            Node::ReReplace {
                value,
                pattern,
                replacement,
            } => {
                let text = eval_str(value, ctx);
                let pattern = eval_str(pattern, ctx);
                let replacement = eval_str(replacement, ctx);
                // Same dual engine as the `re_replace` FILTER, so the two
                // spellings of one feature cannot diverge: `regex` first,
                // `fancy-regex` only for look-around it refuses.
                let re = crate::filters::Pattern::compile(&pattern).map_err(|reason| {
                    CardigannError::Template {
                        template: pattern.clone(),
                        reason: format!("invalid regular expression: {reason}"),
                    }
                })?;
                let replaced = re
                    .replace_all(&text, replacement.as_str())
                    .map_err(|reason| CardigannError::Template {
                        template: pattern.clone(),
                        reason,
                    })?;
                out.push_str(&replaced);
            }
            Node::If {
                cond,
                then,
                otherwise,
            } => {
                if eval_bool(cond, ctx) {
                    render_nodes(then, ctx, out)?;
                } else {
                    render_nodes(otherwise, ctx, out)?;
                }
            }
        }
    }
    Ok(())
}

/// A lexical token inside a `{{ ... }}` action's argument list.
#[derive(Debug, Clone)]
enum Token {
    LParen,
    RParen,
    Str(String),
    Ident(String),
}

/// The only bare identifiers that open a function call rather than
/// denoting a variable path.
const CALL_NAMES: [&str; 4] = ["and", "or", "eq", "ne"];

fn tokenize(full: &str, expr: &str) -> Result<Vec<Token>, CardigannError> {
    let chars: Vec<char> = expr.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '(' {
            tokens.push(Token::LParen);
            i += 1;
        } else if c == ')' {
            tokens.push(Token::RParen);
            i += 1;
        } else if c == '"' {
            i += 1;
            let mut s = String::new();
            loop {
                match chars.get(i) {
                    None => {
                        return Err(CardigannError::Template {
                            template: full.to_string(),
                            reason: "unterminated string literal".to_string(),
                        });
                    }
                    Some('"') => {
                        i += 1;
                        break;
                    }
                    Some('\\') => {
                        i += 1;
                        // Only `\"` and `\\` are true escapes here. Any
                        // other backslash is left as-is: these literals
                        // usually carry a regex pattern (e.g. `[\s]+`,
                        // `(\d+)`), and the corpus's YAML escaping already
                        // reduces `\\s` to a single literal `\s` by the
                        // time this string reaches us, so treating `\s`
                        // as an escape would silently eat the backslash
                        // and corrupt the pattern.
                        match chars.get(i) {
                            None => {
                                return Err(CardigannError::Template {
                                    template: full.to_string(),
                                    reason: "unterminated escape in string literal".to_string(),
                                });
                            }
                            Some('"') => {
                                s.push('"');
                                i += 1;
                            }
                            Some('\\') => {
                                s.push('\\');
                                i += 1;
                            }
                            Some(&other) => {
                                s.push('\\');
                                s.push(other);
                                i += 1;
                            }
                        }
                    }
                    Some(&ch) => {
                        s.push(ch);
                        i += 1;
                    }
                }
            }
            tokens.push(Token::Str(s));
        } else {
            let start = i;
            while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '(' && chars[i] != ')'
            {
                i += 1;
            }
            tokens.push(Token::Ident(chars[start..i].iter().collect()));
        }
    }
    Ok(tokens)
}

/// Parses one argument: a variable path, a string literal, or a
/// parenthesised (possibly nested) expression.
fn parse_atom(full: &str, tokens: &[Token], pos: &mut usize) -> Result<Expr, CardigannError> {
    match tokens.get(*pos) {
        Some(Token::LParen) => {
            *pos += 1;
            let inner = parse_expr(full, tokens, pos)?;
            match tokens.get(*pos) {
                Some(Token::RParen) => {
                    *pos += 1;
                    Ok(inner)
                }
                _ => Err(CardigannError::Template {
                    template: full.to_string(),
                    reason: "unbalanced parentheses in condition".to_string(),
                }),
            }
        }
        Some(Token::Str(s)) => {
            let s = s.clone();
            *pos += 1;
            Ok(Expr::Str(s))
        }
        Some(Token::Ident(p)) if p.starts_with('.') => {
            let p = p.clone();
            *pos += 1;
            Ok(Expr::Path(p))
        }
        Some(Token::Ident(other)) => Err(CardigannError::Template {
            template: full.to_string(),
            reason: format!("expected a variable path or string, found {other:?}"),
        }),
        Some(Token::RParen) => Err(CardigannError::Template {
            template: full.to_string(),
            reason: "unexpected ')' in condition".to_string(),
        }),
        None => Err(CardigannError::Template {
            template: full.to_string(),
            reason: "unexpected end of condition".to_string(),
        }),
    }
}

/// Parses a full expression: either a bare function call consuming every
/// remaining argument up to the enclosing `)` (or end of input), or a
/// single atom.
fn parse_expr(full: &str, tokens: &[Token], pos: &mut usize) -> Result<Expr, CardigannError> {
    if let Some(Token::Ident(name)) = tokens.get(*pos)
        && CALL_NAMES.contains(&name.as_str())
    {
        let name = name.clone();
        *pos += 1;
        let mut args = Vec::new();
        while !matches!(tokens.get(*pos), None | Some(Token::RParen)) {
            args.push(parse_atom(full, tokens, pos)?);
        }
        if args.is_empty() {
            return Err(CardigannError::Template {
                template: full.to_string(),
                reason: format!("{name} requires at least one argument"),
            });
        }
        return Ok(Expr::Call(name, args));
    }
    parse_atom(full, tokens, pos)
}

/// Parses the full argument text of an `if` action (everything after
/// `if `) into a single [`Expr`], requiring every token to be consumed.
fn parse_cond(full: &str, expr_str: &str) -> Result<Expr, CardigannError> {
    let tokens = tokenize(full, expr_str)?;
    if tokens.is_empty() {
        return Err(CardigannError::Template {
            template: full.to_string(),
            reason: "empty condition".to_string(),
        });
    }
    let mut pos = 0;
    let expr = parse_expr(full, &tokens, &mut pos)?;
    if pos != tokens.len() {
        return Err(CardigannError::Template {
            template: full.to_string(),
            reason: format!("unexpected trailing tokens in condition {expr_str:?}"),
        });
    }
    Ok(expr)
}

/// Tokenizes `action` and parses every atom after the leading function
/// name (already known from `head`), requiring every token to be consumed.
fn parse_args(full: &str, action: &str) -> Result<Vec<Expr>, CardigannError> {
    let tokens = tokenize(full, action)?;
    let mut pos = 1; // skip the function name itself
    let mut args = Vec::new();
    while pos < tokens.len() {
        args.push(parse_atom(full, &tokens, &mut pos)?);
    }
    Ok(args)
}

/// Splits a template into `{{ ... }}` actions and literal text.
fn parse(template: &str) -> Result<Vec<Node>, CardigannError> {
    let (nodes, rest) = parse_until(template, template, &[])?;
    if !rest.is_empty() {
        return Err(CardigannError::Template {
            template: template.to_string(),
            reason: format!("unexpected trailing action near {rest:?}"),
        });
    }
    Ok(nodes)
}

/// Parses an `{{ if COND }}then{{ else }}otherwise{{ end }}` (or the
/// else-less form) starting just after the `if` keyword has been
/// recognised. Returns the node plus the input remaining after `end`.
fn parse_if<'a>(
    full: &str,
    action: &str,
    remainder: &'a str,
) -> Result<(Node, &'a str), CardigannError> {
    let cond = parse_cond(full, action["if".len()..].trim())?;
    let (then, rest) = parse_until(full, remainder, &["else", "end"])?;
    let (otherwise, rest) = if action_head(rest) == "else" {
        parse_until(full, skip_action(rest), &["end"])?
    } else {
        (Vec::new(), rest)
    };
    let next_input = skip_action(rest);
    Ok((
        Node::If {
            cond,
            then,
            otherwise,
        },
        next_input,
    ))
}

/// Parses an `{{ range .Path }}body{{ end }}`, returning the node plus the
/// input remaining after `end`.
fn parse_range<'a>(
    full: &str,
    action: &str,
    remainder: &'a str,
) -> Result<(Node, &'a str), CardigannError> {
    let list = action["range".len()..].trim();
    if list.is_empty() || !list.starts_with('.') || list.contains(char::is_whitespace) {
        return Err(CardigannError::Template {
            template: full.to_string(),
            reason: format!("range requires a single variable path, found {list:?}"),
        });
    }
    let (body, rest) = parse_until(full, remainder, &["end"])?;
    let next_input = skip_action(rest);
    Ok((
        Node::Range {
            list: list.to_string(),
            body,
        },
        next_input,
    ))
}

/// Parses a bare `{{ or a b ... }}` value-producing action.
fn parse_or(full: &str, action: &str) -> Result<Node, CardigannError> {
    let args = parse_args(full, action)?;
    if args.is_empty() {
        return Err(CardigannError::Template {
            template: full.to_string(),
            reason: "or requires at least one argument".to_string(),
        });
    }
    Ok(Node::Or(args))
}

/// Parses `{{ join .Path "sep" }}`.
fn parse_join(full: &str, action: &str) -> Result<Node, CardigannError> {
    let args = parse_args(full, action)?;
    let [list_expr, sep] = <[Expr; 2]>::try_from(args).map_err(|_| CardigannError::Template {
        template: full.to_string(),
        reason: "join requires exactly two arguments".to_string(),
    })?;
    let Expr::Path(list) = list_expr else {
        return Err(CardigannError::Template {
            template: full.to_string(),
            reason: "join's first argument must be a variable path".to_string(),
        });
    };
    Ok(Node::Join { list, sep })
}

/// Parses `{{ re_replace VALUE "pattern" "replacement" }}`.
fn parse_re_replace(full: &str, action: &str) -> Result<Node, CardigannError> {
    let args = parse_args(full, action)?;
    let [value, pattern, replacement] =
        <[Expr; 3]>::try_from(args).map_err(|_| CardigannError::Template {
            template: full.to_string(),
            reason: "re_replace requires exactly three arguments".to_string(),
        })?;
    Ok(Node::ReReplace {
        value,
        pattern,
        replacement,
    })
}

/// Parses nodes until one of `stop` keywords is reached. Returns the
/// remaining input starting at that keyword's action.
fn parse_until<'a>(
    full: &str,
    mut input: &'a str,
    stop: &[&str],
) -> Result<(Vec<Node>, &'a str), CardigannError> {
    let mut nodes = Vec::new();

    loop {
        let Some(open) = input.find("{{") else {
            if !input.is_empty() {
                nodes.push(Node::Text(input.to_string()));
            }
            if stop.is_empty() {
                return Ok((nodes, ""));
            }
            return Err(CardigannError::Template {
                template: full.to_string(),
                reason: format!("missing {}", stop.join(" or ")),
            });
        };

        if open > 0 {
            nodes.push(Node::Text(input[..open].to_string()));
        }
        let after = &input[open + 2..];
        let Some(close) = after.find("}}") else {
            return Err(CardigannError::Template {
                template: full.to_string(),
                reason: "unclosed action".to_string(),
            });
        };
        let action = after[..close].trim();
        let remainder = &after[close + 2..];

        let head = action.split_whitespace().next().unwrap_or_default();
        if stop.contains(&head) {
            return Ok((nodes, &input[open..]));
        }

        match head {
            "if" => {
                let (node, next) = parse_if(full, action, remainder)?;
                nodes.push(node);
                input = next;
            }
            "range" => {
                let (node, next) = parse_range(full, action, remainder)?;
                nodes.push(node);
                input = next;
            }
            "or" => {
                nodes.push(parse_or(full, action)?);
                input = remainder;
            }
            "join" => {
                nodes.push(parse_join(full, action)?);
                input = remainder;
            }
            "re_replace" => {
                nodes.push(parse_re_replace(full, action)?);
                input = remainder;
            }
            _ if head.starts_with('.') => {
                nodes.push(Node::Var(head.to_string()));
                input = remainder;
            }
            other => {
                return Err(CardigannError::Template {
                    template: full.to_string(),
                    reason: format!("unsupported directive {other:?}"),
                });
            }
        }
    }
}

fn action_head(s: &str) -> &str {
    s.strip_prefix("{{")
        .and_then(|r| r.find("}}").map(|i| r[..i].trim()))
        .and_then(|a| a.split_whitespace().next())
        .unwrap_or_default()
}

fn skip_action(s: &str) -> &str {
    s.strip_prefix("{{")
        .and_then(|r| r.find("}}").map(|i| &r[i + 2..]))
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn scope() -> Scope {
        let mut s = Scope::new();
        s.set_keywords("big buck bunny");
        s.set_config("sort", "seeders");
        s.set_result("title_optional", "Sintel");
        s
    }

    #[test]
    fn literal_text_passes_through() {
        assert_eq!(render("/search/", &scope()).unwrap(), "/search/");
    }

    #[test]
    fn substitutes_keywords() {
        assert_eq!(
            render("search/{{ .Keywords }}", &scope()).unwrap(),
            "search/big buck bunny"
        );
    }

    #[test]
    fn substitutes_config_and_result() {
        assert_eq!(render("{{ .Config.sort }}", &scope()).unwrap(), "seeders");
        assert_eq!(
            render("{{ .Result.title_optional }}", &scope()).unwrap(),
            "Sintel"
        );
    }

    #[test]
    fn unknown_variable_is_empty() {
        assert_eq!(render("{{ .Config.missing }}", &scope()).unwrap(), "");
    }

    #[test]
    fn if_else_end_picks_the_true_branch() {
        assert_eq!(
            render(
                "{{ if .Keywords }}search{{ else }}browse{{ end }}",
                &scope()
            )
            .unwrap(),
            "search"
        );
    }

    #[test]
    fn if_else_end_picks_the_false_branch() {
        let s = Scope::new();
        assert_eq!(
            render("{{ if .Keywords }}search{{ else }}browse{{ end }}", &s).unwrap(),
            "browse"
        );
    }

    #[test]
    fn or_returns_first_non_empty() {
        assert_eq!(
            render("{{ or .Config.missing .Config.sort }}", &scope()).unwrap(),
            "seeders"
        );
    }

    #[test]
    fn eq_compares_values() {
        assert_eq!(
            render(
                "{{ if eq .Config.sort .Config.sort }}same{{ else }}diff{{ end }}",
                &scope()
            )
            .unwrap(),
            "same"
        );
    }

    #[test]
    fn and_requires_both() {
        let s = Scope::new();
        assert_eq!(
            render(
                "{{ if and .Keywords .Config.sort }}y{{ else }}n{{ end }}",
                &s
            )
            .unwrap(),
            "n"
        );
    }

    #[test]
    fn unclosed_block_is_an_error() {
        assert!(render("{{ if .Keywords }}oops", &scope()).is_err());
    }

    // --- Extensions beyond the brief's ten tests, driven by corpus forms ---

    fn categorised_scope() -> Scope {
        let mut s = scope();
        s.set_categories(["2000", "2010"]);
        s
    }

    #[test]
    fn range_over_categories_binds_the_loop_item_to_dot() {
        // The real corpus form, e.g. bibliotik.yml: `cat[]={{.}}&`.
        assert_eq!(
            render(
                "{{ range .Categories }}cat[]={{.}}&{{end}}",
                &categorised_scope()
            )
            .unwrap(),
            "cat[]=2000&cat[]=2010&"
        );
    }

    #[test]
    fn range_over_empty_categories_produces_nothing() {
        assert_eq!(
            render("{{ range .Categories }}cat[]={{.}}&{{end}}", &scope()).unwrap(),
            ""
        );
    }

    #[test]
    fn join_categories_with_separator() {
        // e.g. aidoruonline.yml: `{{ join .Categories "," }}`.
        assert_eq!(
            render(r#"{{ join .Categories "," }}"#, &categorised_scope()).unwrap(),
            "2000,2010"
        );
    }

    #[test]
    fn categories_are_truthy_only_when_non_empty() {
        assert_eq!(
            render(
                "{{ if .Categories }}y{{ else }}n{{ end }}",
                &categorised_scope()
            )
            .unwrap(),
            "y"
        );
        assert_eq!(
            render("{{ if .Categories }}y{{ else }}n{{ end }}", &scope()).unwrap(),
            "n"
        );
    }

    #[test]
    fn re_replace_substitutes_with_backreferences() {
        // e.g. amigosshare.yml: `{{ re_replace .Keywords "[\s]+" "%" }}`.
        let mut s = Scope::new();
        s.set_keywords("big  buck bunny");
        assert_eq!(
            render(r#"{{ re_replace .Keywords "\s+" "%" }}"#, &s).unwrap(),
            "big%buck%bunny"
        );
    }

    #[test]
    fn re_replace_with_capture_group_backreference() {
        // e.g. arabtorrents.yml's `$1` backreference form.
        let mut s = Scope::new();
        s.set_config("sort", "download-torrent-42-extra");
        assert_eq!(
            render(
                r#"{{ re_replace .Config.sort ".*download-torrent-(\d+).*" "$1" }}"#,
                &s
            )
            .unwrap(),
            "42"
        );
    }

    #[test]
    fn re_replace_with_invalid_pattern_is_an_error() {
        assert!(render(r#"{{ re_replace .Keywords "(" "x" }}"#, &scope()).is_err());
    }

    #[test]
    fn parenthesised_and_of_nested_eq_and_ne() {
        // e.g. the corpus's `and (.Result._releaseGroup) (ne ... "NULL")`
        // and 1337x.yml's `and (.Keywords) (eq .Config.disablesort .False)`
        // idioms: `and`/`or` arguments are themselves full expressions when
        // parenthesised, not just bare variable paths. An unset checkbox
        // config value is the empty string, i.e. equal to `.False`.
        let mut s = scope();
        s.set_config("disablesort", "");
        assert_eq!(
            render(
                "{{ if and (.Keywords) (eq .Config.disablesort .False) }}y{{ else }}n{{ end }}",
                &s
            )
            .unwrap(),
            "y"
        );
        s.set_config("disablesort", "true");
        assert_eq!(
            render(
                "{{ if and (.Keywords) (eq .Config.disablesort .False) }}y{{ else }}n{{ end }}",
                &s
            )
            .unwrap(),
            "n"
        );
    }

    #[test]
    fn ne_compares_for_inequality() {
        // `ne` is not in the brief's stated function list but appears
        // throughout the corpus in the same idiom as `eq` (e.g.
        // `and (.Result._releaseGroup) (ne .Result._releaseGroup "NULL")`);
        // implementing it as `eq`'s negation is the natural reading.
        let mut s = Scope::new();
        s.set_result("group", "NULL");
        assert_eq!(
            render(
                r#"{{ if ne .Result.group "NULL" }}y{{ else }}n{{ end }}"#,
                &s
            )
            .unwrap(),
            "n"
        );
        s.set_result("group", "SPARKS");
        assert_eq!(
            render(
                r#"{{ if ne .Result.group "NULL" }}y{{ else }}n{{ end }}"#,
                &s
            )
            .unwrap(),
            "y"
        );
    }

    #[test]
    fn deeply_nested_and_or_eq() {
        // e.g. the corpus's `or (eq ...) (or (eq ...) (or ...))` chains.
        let mut s = Scope::new();
        s.set_result("a", "no");
        s.set_result("b", "yes");
        assert_eq!(
            render(
                r#"{{ if or (eq .Result.a "yes") (or (eq .Result.b "yes") (eq .Result.c "yes")) }}y{{ else }}n{{ end }}"#,
                &s
            )
            .unwrap(),
            "y"
        );
    }

    #[test]
    fn unbalanced_parentheses_in_condition_is_an_error() {
        // A real corpus typo (1337x.yml's TV path): a stray extra `)`.
        assert!(
            render(
                "{{ if and (.Keywords) (eq .Config.disablesort .False)) }}y{{ end }}",
                &scope()
            )
            .is_err()
        );
    }

    #[test]
    fn false_is_empty_and_true_is_non_empty() {
        assert_eq!(render("{{ .False }}", &scope()).unwrap(), "");
        assert_eq!(render("{{ .True }}", &scope()).unwrap(), "True");
    }

    #[test]
    fn false_and_true_round_trip_through_the_corpus_case_idiom() {
        // The real shape (e.g. blutopia-api.yml:143-149, nobs.yml:135-141):
        // a `case:` block resolves a raw boolean field to `{{ .False }}` or
        // `{{ .True }}`, and the resolved value is stored back into a
        // `.Result.*` field, which a *later*, separate template then
        // checks with a bare `{{ if ... }}`. Only rendering `.False` in
        // isolation (as the previous test does) cannot catch a regression
        // where `.False` is non-empty: that would make the `if` below
        // always take the truthy branch regardless of which case matched.
        // This test replicates both halves of that pipeline end to end.
        let false_case_value = render("{{ .False }}", &scope()).unwrap();
        let mut s = scope();
        s.set_result("_internal", &false_case_value);
        assert_eq!(
            render("{{ if .Result._internal }}Internal{{ else }}{{ end }}", &s).unwrap(),
            ""
        );

        let true_case_value = render("{{ .True }}", &scope()).unwrap();
        let mut s = scope();
        s.set_result("_internal", &true_case_value);
        assert_eq!(
            render("{{ if .Result._internal }}Internal{{ else }}{{ end }}", &s).unwrap(),
            "Internal"
        );
    }

    #[test]
    fn unclosed_range_is_an_error() {
        assert!(render("{{ range .Categories }}cat={{.}}", &scope()).is_err());
    }

    #[test]
    fn unsupported_directive_is_an_error() {
        assert!(render("{{ nosuchthing .Keywords }}", &scope()).is_err());
    }
}
