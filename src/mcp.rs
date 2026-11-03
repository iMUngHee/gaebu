//! stdio MCP server (Decision 16): JSON-RPC 2.0, protocol 2025-06-18.
//! stdout carries JSON-RPC only; logs go to stderr. Root directives are not
//! exposed here.

use crate::pager::Pager;
use crate::store::Store;
use std::io::{BufRead, Write};

pub const PROTOCOL_VERSION: &str = "2025-06-18";

pub fn serve(
    stdin: &mut dyn BufRead,
    stdout: &mut dyn Write,
    store: &Store,
    pager: &dyn Pager,
) -> std::io::Result<()> {
    let _ = (stdin, stdout, store, pager);
    todo!("bepo: mcp")
}
