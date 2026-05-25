# Coding guidelines (Rust)

Bias toward caution over speed. For trivial tasks, use judgment.

## 1. Think before coding
Don't assume. Don't hide confusion. Surface tradeoffs.
- State assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them — don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- Read the existing types and traits before introducing new ones.

## 2. Simplicity first
Minimum code that solves the problem. Nothing speculative.
- No features beyond what was asked.
- No generic parameters, lifetimes, or trait impls until a second caller exists.
- Prefer `&str` over `String` until ownership is actually required.
- No `Arc<Mutex<T>>` until the borrow checker actually disagrees with you.
- No error handling for impossible scenarios — `?` at boundaries is enough.

## 3. Surgical changes
Touch only what you must. Clean up only your own mess.
- Don't `#[derive(...)]` traits you don't use.
- Don't change `pub` / private visibility unless asked.
- Don't reformat, re-order, or rename adjacent items.
- If your changes orphan an import or a `use` line, remove that. Leave the rest.
- Match the existing style — `anyhow::Result` vs custom errors, `tokio` vs blocking, etc.

## 4. Goal-driven execution
Define success criteria. Loop until verified. The verification loop for
this workspace is:

    cargo fmt --all -- --check
    cargo clippy --all-targets -- -D warnings
    cargo build --all-targets --locked
    cargo test --all-targets --locked

A change isn't done until all four pass. For new behavior, add a test
that pins it before claiming the work is finished; for a bug fix, add a
test that reproduces the bug first.
