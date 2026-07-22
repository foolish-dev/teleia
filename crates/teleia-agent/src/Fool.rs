//! Behavioral guidelines drawn from the fool CLAUDE.md,
//! rewritten for a Rust workspace. The agent appends [`GUIDELINES`] to
//! its system prompt so the model inherits the same four-principle
//! discipline — but with examples that fit `cargo`, lifetimes, traits,
//! and the borrow checker rather than generic LLM-coding advice.
//!
//! Sourced from `Fool.md` at the workspace root so the same file
//! is shared between the agent prompt and the human-facing reference
//! at `~/.claude/Karpathy.md`. Edit the .md and rebuild — both stay
//! in sync.

/// The four principles, formatted for direct inclusion in a system
/// prompt. Self-contained — safe to concatenate after the base prompt
/// with a single blank line.
pub const GUIDELINES: &str = include_str!("../../../Fool.md");
