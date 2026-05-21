// Shared tool dispatcher used by all 5 polyglot impls. CLI:
//   teleia-tools-bin defs        — print OpenAI-format tool definitions JSON
//   teleia-tools-bin run NAME    — read JSON args on stdin, dispatch, write
//                                  tool output to stdout. Errors are
//                                  formatted as "error: <msg>" in stdout
//                                  (always exits 0 so wrappers don't have to
//                                  distinguish dispatch errors from non-zero
//                                  tool exit codes — the bash tool already
//                                  encodes those in its own output).

use anyhow::Result;
use std::io::{self, Read, Write};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("defs") => {
            let defs = teleia_tools::definitions();
            let json = serde_json::to_string(&defs)?;
            println!("{json}");
        }
        Some("run") => {
            let name = match args.get(2) {
                Some(n) => n.clone(),
                None => {
                    eprintln!("usage: teleia-tools-bin run NAME (args via stdin)");
                    std::process::exit(2);
                }
            };
            let mut input = String::new();
            io::stdin().read_to_string(&mut input)?;
            let output = match teleia_tools::dispatch(&name, &input).await {
                Ok(o) => o,
                Err(e) => format!("error: {e}"),
            };
            io::stdout().write_all(output.as_bytes())?;
        }
        _ => {
            eprintln!("usage: teleia-tools-bin defs | run NAME");
            std::process::exit(2);
        }
    }
    Ok(())
}
