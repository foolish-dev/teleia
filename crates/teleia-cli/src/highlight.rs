// Syntax highlighting via syntect. We parse with `ParseState` so we
// see the SCOPE STACK at each token instead of pre-resolved theme
// colours — then map common scope categories to the active TUI
// theme's palette so highlighted code reads as one piece with the
// surrounding chat. Falls back gracefully if syntect doesn't have a
// syntax for the requested language.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::path::Path;
use std::sync::LazyLock;
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

/// Colour palette consumed by [`highlight`]. The TUI builds one of
/// these from its active theme so `/theme dracula` changes code
/// colours alongside the rest of the UI.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub keyword: Color,
    pub string: Color,
    pub number: Color,
    pub function: Color,
    pub type_: Color,
    pub comment: Color,
    pub punctuation: Color,
    pub fg: Color,
}

/// Highlight `code` for `hint`. `hint` may be a file extension ("rs"),
/// a markdown fence token ("rust"), or empty. Returns one Line per
/// source line, each indented with `indent` so it matches the
/// surrounding chat layout.
pub fn highlight(code: &str, hint: &str, indent: &str, palette: Palette) -> Vec<Line<'static>> {
    let set: &SyntaxSet = &SYNTAX_SET;
    let syntax = pick_syntax(hint);
    let mut parser = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut out = Vec::new();

    for line in code.lines() {
        let ops = match parser.parse_line(line, set) {
            Ok(o) => o,
            Err(_) => {
                out.push(Line::from(Span::styled(
                    format!("{indent}{line}"),
                    Style::default().fg(palette.fg),
                )));
                continue;
            }
        };

        let mut spans: Vec<Span<'static>> = Vec::with_capacity(ops.len() + 1);
        spans.push(Span::raw(indent.to_string()));
        let mut cursor = 0usize;
        for (offset, op) in ops {
            if offset > cursor {
                let segment = &line[cursor..offset];
                spans.push(Span::styled(
                    segment.to_string(),
                    style_for(&stack, palette),
                ));
                cursor = offset;
            }
            let _ = stack.apply(&op);
        }
        if cursor < line.len() {
            let segment = &line[cursor..];
            spans.push(Span::styled(
                segment.to_string(),
                style_for(&stack, palette),
            ));
        }
        out.push(Line::from(spans));
    }

    out
}

/// Map the current scope stack to a ratatui style using `palette`.
/// Walks the scope tokens from deepest to outermost and picks the
/// first match — so `comment.line.double-slash.rust` resolves to the
/// comment colour even though it ends in `.rust`.
fn style_for(stack: &ScopeStack, p: Palette) -> Style {
    let scope_text = stack
        .as_slice()
        .iter()
        .map(|s| s.build_string())
        .collect::<Vec<_>>()
        .join(" ");

    let (color, bold, italic) = classify(&scope_text, p);
    let mut s = Style::default().fg(color);
    if bold {
        s = s.add_modifier(Modifier::BOLD);
    }
    if italic {
        s = s.add_modifier(Modifier::ITALIC);
    }
    s
}

fn classify(scope: &str, p: Palette) -> (Color, bool, bool) {
    // Order matters: more specific matches first. Returns
    // (color, bold, italic).
    if scope.contains("comment") {
        (p.comment, false, true)
    } else if scope.contains("string") {
        (p.string, false, false)
    } else if scope.contains("constant.numeric")
        || scope.contains("constant.language")
        || scope.contains("constant.character")
    {
        (p.number, false, false)
    } else if scope.contains("entity.name.function") || scope.contains("support.function") {
        (p.function, true, false)
    } else if scope.contains("entity.name.type")
        || scope.contains("storage.type")
        || scope.contains("support.type")
        || scope.contains("support.class")
    {
        (p.type_, false, false)
    } else if scope.contains("keyword") || scope.contains("storage.modifier") {
        (p.keyword, true, false)
    } else if scope.contains("variable.parameter") || scope.contains("variable.other.member") {
        (p.fg, false, false)
    } else if scope.contains("punctuation") {
        (p.punctuation, false, false)
    } else if scope.contains("entity.other.attribute-name") {
        (p.keyword, false, false)
    } else {
        (p.fg, false, false)
    }
}

/// Look up a syntax by extension, then by token (markdown fence hint),
/// then finally falling back to plain text.
fn pick_syntax(hint: &str) -> &'static SyntaxReference {
    let set: &SyntaxSet = &SYNTAX_SET;
    set.find_syntax_by_extension(hint)
        .or_else(|| set.find_syntax_by_token(hint))
        .unwrap_or_else(|| set.find_syntax_plain_text())
}

/// Parse the JSON args of a `read` tool call and return the file extension,
/// e.g. `read({"path":"foo.rs"})` → `Some("rs")`.
pub fn extension_from_read_args(args: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    let path = v.get("path")?.as_str()?;
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_palette() -> Palette {
        Palette {
            keyword: Color::Rgb(187, 154, 247),
            string: Color::Rgb(158, 206, 106),
            number: Color::Rgb(224, 175, 104),
            function: Color::Rgb(122, 162, 247),
            type_: Color::Rgb(125, 207, 255),
            comment: Color::Rgb(86, 95, 137),
            punctuation: Color::Rgb(86, 95, 137),
            fg: Color::Rgb(192, 202, 245),
        }
    }

    #[test]
    fn highlight_returns_one_line_per_source_line() {
        let lines = highlight("fn main() {}\nfn other() {}", "rs", "  ", test_palette());
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn extension_from_args_handles_typical_read_call() {
        assert_eq!(
            extension_from_read_args(r#"{"path":"src/lib.rs"}"#).as_deref(),
            Some("rs")
        );
        assert_eq!(
            extension_from_read_args(r#"{"path":"Cargo.toml"}"#).as_deref(),
            Some("toml")
        );
        assert_eq!(
            extension_from_read_args(r#"{"path":"Makefile"}"#),
            None,
            "no extension yields None"
        );
    }

    #[test]
    fn extension_from_args_returns_none_on_malformed_json() {
        assert_eq!(extension_from_read_args("not json"), None);
        assert_eq!(extension_from_read_args(r#"{"no-path":"x"}"#), None);
    }
}
