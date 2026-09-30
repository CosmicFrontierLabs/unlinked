//! Shared lexical comment handling. Delimiters are recognized only outside
//! string literals by callers, and block delimiters must occupy their own line.
use crate::Error;

pub(crate) fn skip(chars: &[char], position: &mut usize, line: &mut usize) -> Result<(), Error> {
    let failure = |line, message: &str| Error {
        line,
        message: message.into(),
    };
    let start = *position;
    let end = chars[start..]
        .iter()
        .position(|c| *c == '\n')
        .map_or(chars.len(), |n| start + n);
    let marker = chars.get(start + 1).copied();
    if marker != Some('{') && marker != Some('}') {
        *position = end;
        return Ok(());
    }
    let line_start = chars[..start]
        .iter()
        .rposition(|c| *c == '\n')
        .map_or(0, |n| n + 1);
    if !chars[line_start..start].iter().all(|c| c.is_whitespace())
        || !chars[start + 2..end].iter().all(|c| c.is_whitespace())
    {
        return Err(failure(
            *line,
            "block comment delimiters must occupy a standalone line",
        ));
    }
    if marker == Some('}') {
        return Err(failure(*line, "unmatched block comment closing delimiter"));
    }
    let opening_line = *line;
    let mut depth = 1usize;
    *position = end;
    while *position < chars.len() {
        // The preceding scan always stops immediately before a newline.
        *position += 1;
        *line += 1;
        let start = *position;
        while *position < chars.len() && chars[*position] != '\n' {
            *position += 1;
        }
        let content = &chars[start..*position];
        let first = content
            .iter()
            .position(|c| !c.is_whitespace())
            .unwrap_or(content.len());
        let last = content
            .iter()
            .rposition(|c| !c.is_whitespace())
            .map_or(first, |n| n + 1);
        match &content[first..last] {
            ['%', '{'] => {
                depth += 1;
                if depth > 64 {
                    return Err(failure(*line, "block comment nesting exceeds 64 levels"));
                }
            }
            ['%', '}'] => {
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
    Err(failure(opening_line, "unclosed block comment"))
}
