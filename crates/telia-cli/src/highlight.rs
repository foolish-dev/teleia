// Syntax highlighting via syntect. Loads default syntaxes + a dark theme
// lazily on first use, then converts each highlighted span into a ratatui
// `Line` with RGB colors so it renders in the existing log Paragraph.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::path::Path;
use std::sync::LazyLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SynStyle, Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

static THEME: LazyLock<Theme> = LazyLock::new(|| {
    let ts = ThemeSet::load_defaults();
    // base16-ocean.dark sits closest to the Tokyo Night palette of the rest
    // of the TUI; close enough that the contrast reads cleanly.
    ts.themes
        .get("base16-ocean.dark")
        .cloned()
        .unwrap_or_else(|| ts.themes.values().next().cloned().expect("no themes"))
});

/// Highlight `code` for `hint`. `hint` may be a file extension ("rs"),
/// a markdown fence token ("rust"), or empty. Returns one Line per source
/// line, each indented with two spaces so it matches the existing tool-
/// output indent.
pub fn highlight(code: &str, hint: &str, indent: &str) -> Vec<Line<'static>> {
    let syntax = pick_syntax(hint);
    let mut h = HighlightLines::new(syntax, &THEME);
    let mut out = Vec::new();

    for line in code.lines() {
        let spans = match h.highlight_line(line, &SYNTAX_SET) {
            Ok(s) => s,
            Err(_) => {
                // On highlight failure, fall back to dim text so the user
                // still sees the content.
                out.push(Line::from(Span::styled(
                    format!("{indent}{line}"),
                    Style::default().fg(Color::Rgb(86, 95, 137)),
                )));
                continue;
            }
        };

        let mut rendered: Vec<Span<'static>> = Vec::with_capacity(spans.len() + 1);
        rendered.push(Span::raw(indent.to_string()));
        for (style, text) in spans {
            rendered.push(Span::styled(text.to_string(), to_ratatui_style(style)));
        }
        out.push(Line::from(rendered));
    }

    out
}

/// Look up a syntax by extension, then by token (markdown fence hint), then
/// finally falling back to plain text.
fn pick_syntax(hint: &str) -> &'static SyntaxReference {
    let set: &SyntaxSet = &SYNTAX_SET;
    set.find_syntax_by_extension(hint)
        .or_else(|| set.find_syntax_by_token(hint))
        .unwrap_or_else(|| set.find_syntax_plain_text())
}

fn to_ratatui_style(style: SynStyle) -> Style {
    let mut s = Style::default().fg(Color::Rgb(
        style.foreground.r,
        style.foreground.g,
        style.foreground.b,
    ));
    if style.font_style.contains(FontStyle::BOLD) {
        s = s.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        s = s.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        s = s.add_modifier(Modifier::UNDERLINED);
    }
    s
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

    #[test]
    fn highlight_returns_one_line_per_source_line() {
        let lines = highlight("fn main() {}\nfn other() {}", "rs", "  ");
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
