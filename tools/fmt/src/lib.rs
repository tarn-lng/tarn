//! Conservative canonical whitespace over the real Tarn lexer and parser.
//! Physical line breaks remain in place: the parser owns their meaning.
mod roles;

use std::collections::HashSet;
use tarn_diagnostics::{Diagnostic, Severity, SourceMap};
use tarn_lexer::{Token, TokenKind as K};

#[derive(Debug)]
pub enum FormatError {
    Syntax(Vec<Diagnostic>),
    Invariant,
}

/// Format a complete UTF-8 source buffer without resolving imports or types.
/// Literals and comment text are copied verbatim. No recovered syntax is printed.
pub fn format(source: &str) -> Result<String, FormatError> {
    let mut map = SourceMap::new();
    let file = map.add("<format>", source);
    let parsed = tarn_parser::parse_file(file, map.file(file));
    if parsed
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        return Err(FormatError::Syntax(parsed.diagnostics));
    }
    let lexed = tarn_lexer::lex(file, map.file(file));
    let tokens: Vec<_> = lexed
        .tokens
        .iter()
        .filter(|t| !matches!(t.kind, K::Newline | K::Eof))
        .collect();
    let mut binary = HashSet::new();
    roles::binary_operators(&parsed.module, &tokens, &mut binary);
    let mut output = String::new();
    let mut depth: usize = 0;
    let mut token_index = 0;
    let mut comment_index = 0;
    let mut blank = false;
    let mut previous_line_binary = false;
    for (line_index, _) in source.split('\n').enumerate() {
        let line_number = line_index as u32 + 1;
        let start = token_index;
        while token_index < tokens.len() && tokens[token_index].line == line_number {
            token_index += 1;
        }
        let line_tokens = &tokens[start..token_index];
        let comment = lexed
            .comments
            .get(comment_index)
            .filter(|comment| map.file(file).line_col(comment.span.start).line == line_number);
        if comment.is_some() {
            comment_index += 1;
        }
        if line_tokens.is_empty() && comment.is_none() {
            if !output.is_empty() {
                blank = true;
            }
            continue;
        }
        if blank {
            output.push('\n');
            blank = false;
        }
        let closing: usize = line_tokens
            .iter()
            .take_while(|t| closes(t, &binary) > 0)
            .map(|t| closes(t, &binary))
            .sum();
        let continued =
            previous_line_binary || line_tokens.first().is_some_and(|t| t.kind == K::Dot);
        // Delimiters already account for multiline argument/list indentation.
        let indent = depth.saturating_sub(closing) + usize::from(continued && closing == 0);
        output.push_str(&"    ".repeat(indent));
        for (index, token) in line_tokens.iter().enumerate() {
            if index > 0 && space(line_tokens[index - 1], token, &binary) {
                output.push(' ');
            }
            output.push_str(map.snippet(token.span));
            if matches!(token.kind, K::LBrace | K::LParen | K::LBracket)
                || (token.kind == K::Lt && !binary.contains(&token.span.start))
            {
                depth += 1;
            }
            depth = depth.saturating_sub(closes(token, &binary));
        }
        if let Some(comment) = comment {
            if !line_tokens.is_empty() {
                output.push_str("  ");
            }
            // CR belongs to a CRLF separator, not comment content.
            output.push_str(map.snippet(comment.span).trim_end_matches('\r'));
        }
        output.push('\n');
        if let Some(last) = line_tokens.last() {
            previous_line_binary = binary.contains(&last.span.start)
                || matches!(
                    last.kind,
                    K::Eq
                        | K::ColonEq
                        | K::FatArrow
                        | K::Dot
                        | K::Amp
                        | K::AmpAmp
                        | K::Bang
                        | K::Minus
                );
        }
    }
    // Before returning edits, prove token spelling/order/newline boundaries and AST.
    let formatted_file = map.add("<formatted>", output.clone());
    let formatted = tarn_parser::parse_file(formatted_file, map.file(formatted_file));
    let after = tarn_lexer::lex(formatted_file, map.file(formatted_file));
    let signature = |ts: &[Token]| {
        let mut kinds: Vec<_> = ts
            .iter()
            .filter(|t| t.kind != K::Eof)
            .map(|t| t.kind.clone())
            .collect();
        if kinds.last() == Some(&K::Newline) {
            kinds.pop();
        }
        kinds
    };
    let spelling = |ts: &[Token]| {
        ts.iter()
            .filter(|t| !matches!(t.kind, K::Newline | K::Eof))
            .map(|t| map.snippet(t.span))
            .collect::<Vec<_>>()
    };
    let comments_before: Vec<_> = lexed.comments.iter().map(|c| (&c.text, c.doc)).collect();
    let comments_after: Vec<_> = after.comments.iter().map(|c| (&c.text, c.doc)).collect();
    if !formatted.diagnostics.is_empty()
        || spelling(&lexed.tokens) != spelling(&after.tokens)
        || comments_before != comments_after
        || signature(&lexed.tokens) != signature(&after.tokens)
        || tarn_ast::dump_module(&parsed.module) != tarn_ast::dump_module(&formatted.module)
    {
        return Err(FormatError::Invariant);
    }
    Ok(output)
}

fn closes(token: &Token, binary: &HashSet<u32>) -> usize {
    match token.kind {
        K::RBrace | K::RParen | K::RBracket => 1,
        K::Gt if !binary.contains(&token.span.start) => 1,
        K::Shr if !binary.contains(&token.span.start) => 2,
        _ => 0,
    }
}

fn space(left: &Token, right: &Token, binary: &HashSet<u32>) -> bool {
    let (a, b) = (&left.kind, &right.kind);
    // Never fuse distinct punctuation tokens (notably nested refs and generics).
    if matches!(
        (a, b),
        (K::Amp, K::Amp) | (K::Gt, K::Gt) | (K::Gt, K::Shr) | (K::Colon, K::Eq)
    ) {
        return true;
    }
    // `#[attribute]`: the `#` hugs its bracket (ADR 0101, P6).
    if matches!((a, b), (K::Hash, K::LBracket)) {
        return false;
    }
    if binary.contains(&left.span.start) || binary.contains(&right.span.start) {
        return true;
    }
    if matches!(
        b,
        K::Comma
            | K::Colon
            | K::RParen
            | K::RBracket
            | K::Dot
            | K::DotDot
            | K::DotDotEq
            | K::Lt
            | K::Gt
            | K::Shr
    ) {
        return false;
    }
    if matches!(
        a,
        K::LParen | K::LBracket | K::Dot | K::DotDot | K::DotDotEq | K::Lt
    ) {
        return false;
    }
    if matches!(a, K::Amp | K::AmpAmp | K::Star | K::Minus | K::Bang) {
        return false;
    }
    if matches!(b, K::LParen) {
        return matches!(
            a,
            K::If
                | K::For
                | K::Match
                | K::Return
                | K::Try
                | K::Await
                | K::Eq
                | K::ColonEq
                | K::FatArrow
        );
    }
    if matches!(b, K::LBracket) {
        return !matches!(a, K::Ident(_) | K::RParen | K::RBracket);
    }
    if matches!(a, K::RBracket) && matches!(b, K::Ident(_)) {
        return false;
    }
    if matches!((a, b), (K::LBrace, K::RBrace)) {
        return false;
    }
    true
}
