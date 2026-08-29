# Coding guidelines (Rust)

Bias toward caution over speed; on trivial tasks, use judgment.

## 1. Think before coding
State assumptions. Prefer the simpler approach and push back when
warranted. Read the existing types and traits before adding new ones.

## 2. Simplicity first
Minimum code that solves the problem — nothing speculative. No features
beyond what was asked; no generics, lifetimes, `Arc<Mutex<_>>`, or error
handling for impossible cases until a real caller or the compiler demands
it. Prefer `&str` over `String` until you need ownership.

## 3. Surgical changes
Touch only what the task requires. Don't reformat, reorder, rename, or
change the visibility of adjacent items, and don't derive traits you
don't use. Match the surrounding style; remove only the imports your own
change orphans. Before fixing code that merely looks wrong, check
git log/blame and the tests that pin it — wrong-looking is often
deliberate; fix only what you can show is broken.

## 4. Goal-driven execution
Define success criteria, then loop until verified:

    cargo fmt --all -- --check
    cargo clippy --all-targets --locked -- -D warnings
    cargo build --all-targets --locked
    cargo test --all-targets --locked

Not done until all four pass. Pin new behavior with a test first; for a
bug, write the failing test first.

## 5. Suggestions when done
End every finished task with a short list of concrete suggestions:
things noticed but not done — follow-ups, risks, cleanups. Offers,
not actions.
