//! `csformat-core` — a Roslyn/ReSharper-compatible formatter for modern C#.
//!
//! The pipeline is deliberately simple and lossless:
//!
//! 1. [`lexer`] turns the source into tokens plus the whitespace gaps between them.
//! 2. [`syntax`] annotates tokens with tree-sitter context (generic angles, brace roles,
//!    paren roles, colons, operators). Parse errors are tolerated.
//! 3. [`config`] resolves `.editorconfig` (Roslyn and ReSharper keys) and `.DotSettings`.
//! 4. [`format`] decides newlines, indentation and spacing, then re-emits the source by
//!    inserting/removing whitespace only. Token text is never rewritten.

pub mod config;
pub mod format;
pub mod lexer;
pub mod syntax;
pub mod verify;

pub use config::FormatOptions;
pub use format::{FormatResult, format_source};
pub use lexer::{Gap, LexError, Token, TokenKind};
pub use verify::{VerifyReport, verify_source};
