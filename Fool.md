# Coding guidelines (Rust)

Bias toward caution over speed — no task is too small to verify.

## 1. Think before coding
State assumptions. When the request is genuinely ambiguous, surface the
options and ask — don't pick silently. Push back when warranted. Read
the existing types and traits before adding new ones.

## 2. Simplicity first
Minimum code that solves the problem — nothing speculative. No features
beyond what was asked; no generics, lifetimes, `Arc<Mutex<_>>`, or
speculative error types until a real caller or the compiler demands it.
Prefer `&str` params over `String`. Don't `unwrap()` real I/O or parse
failures — propagate with `?`.

## 3. Surgical changes
Touch only what the task requires. Don't reformat, reorder, rename, or
change the visibility of adjacent items, and don't derive traits you
don't use. Match the surrounding style; remove only the imports your own
change orphans. Before fixing code that merely looks wrong, check
git log/blame and the tests that pin it — wrong-looking is often
deliberate; fix only what you can show is broken: a failing test, a
compiler or clippy error, or an input you can name that produces the
wrong output. Before calling a defect fixed, grep for the same shape
elsewhere.

## 4. Goal-driven execution
Name the command output or test that proves this done, then run it.
`[bash timed out after 30s]` is the tool's cap, not a failure — the
build resumes, so re-run rather than edit. Same check, same error,
three times — stop and say what's blocking. Pin new behavior with a
test first; for a bug, write the failing test first.

## 5. Suggestions when done
End every finished task with at most three concrete follow-ups, risks,
or cleanups noticed but not done. Offers, not actions — the one
exception to "do not narrate".
