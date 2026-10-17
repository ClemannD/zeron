//! Block-level markdown parsing over pulldown-cmark.
//!
//! Full parses build a [`BlockTree`] — a list of top-level blocks with their
//! byte ranges in the source. The streaming path ([`IncrementalParser`]) reparses
//! only from the last stable top-level block boundary: text before the start of
//! the last top-level block cannot be affected by an append, so each streamed
//! delta costs roughly O(delta + last block) instead of O(document).
//!
//! Soundness guard: link-reference definitions (`[label]: url`) have non-local
//! effects (a definition anywhere resolves references anywhere), so a source
//! containing one drops to full reparses. The parity unit tests stream corpora
//! through both paths and assert equality.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

// ---------------------------------------------------------------------------
// Tree model
// ---------------------------------------------------------------------------

/// Inline styling flags threaded through nested emphasis/links.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strikethrough: bool,
    /// Destination URL when inside a link.
    pub link: Option<String>,
    /// The label to show instead of `text` for a file link whose text is the
    /// path itself (an inline-code span) rather than a label the author
    /// wrote. Never set by the parser; `None` shows `text`. When set, copy
    /// and selection still map back to `text`.
    pub file_label: Option<String>,
    pub image: Option<InlineImage>,
    pub task: Option<TaskMarker>,
    /// Inline TeX math (`$…$`, `\(…\)`): `text` is the TeX source without delimiters.
    pub math: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskMarker {
    pub checked: bool,
    /// Byte range of `[ ]`, `[x]` or `[X]` in the original document.
    pub range: Range<usize>,
}

/// An image remains inline in the AST; hosts can opt in to visual media.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineImage {
    pub source: String,
    pub alt: String,
    pub title: String,
    pub link: Option<String>,
}

/// One run of identically-styled inline text.
#[derive(Debug, Clone, PartialEq)]
pub struct InlineRun {
    pub text: String,
    pub style: InlineStyle,
}

/// A markdown block. Containers nest.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Paragraph {
        runs: Vec<InlineRun>,
    },
    Heading {
        level: u8,
        runs: Vec<InlineRun>,
    },
    CodeBlock {
        language: Option<String>,
        code: String,
    },
    BlockQuote {
        children: Vec<Block>,
    },
    List {
        ordered_start: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    Table {
        header: Vec<Vec<InlineRun>>,
        rows: Vec<Vec<Vec<InlineRun>>>,
        /// Per-column GFM alignment (`:--`/`:-:`/`--:`); unspecified is Left.
        align: Vec<TableAlign>,
    },
    /// Display math (`$$…$$`, `\[…\]`) on its own: the TeX source without delimiters.
    Math {
        tex: String,
    },
    Rule,
}

/// GFM column alignment for a table (mdast `align`; `None` renders as Left).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TableAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// A top-level block plus its byte range in the source. The range start is the
/// stable-boundary anchor for incremental reparses.
#[derive(Debug, Clone, PartialEq)]
pub struct TopBlock {
    pub range: Range<usize>,
    pub block: Block,
}

/// The parse result: top-level blocks in document order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BlockTree {
    // Completed blocks are immutable. Canonical/display trees and old/new
    // transcript rows share their contents across streamed tail updates.
    pub blocks: Vec<Arc<TopBlock>>,
}

impl BlockTree {
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }
}

// ---------------------------------------------------------------------------
// Full parse
// ---------------------------------------------------------------------------

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH
}

/// Parse a whole source into a [`BlockTree`].
pub fn parse_full(source: &str) -> BlockTree {
    parse_at(source, 0)
}

fn parse_at(source: &str, offset: usize) -> BlockTree {
    parse_with(source, offset, options())
}

fn parse_with(source: &str, offset: usize, options: Options) -> BlockTree {
    // Pre-passes run in order, each over the previous one's output; `maps`
    // records their edits so ranges walk back through them in reverse.
    let mut text = Cow::Borrowed(source);
    let mut maps: Vec<OffsetMap> = Vec::new();
    if let Some((rewritten, map)) = rewrite_math_per_block(source, options) {
        text = Cow::Owned(rewritten);
        maps.push(map);
    }
    if let Some((rewritten, map)) = rewrite_absolute_destinations(&text) {
        text = Cow::Owned(rewritten);
        maps.push(map);
    }
    // Rewrites shift every following offset; ranges come from the rewritten
    // text and must point into the original.
    let original = |maps: &[OffsetMap], range: Range<usize>| {
        maps.iter()
            .rev()
            .fold(range, |range, map| map.original_range(range))
    };
    // pulldown pairs dollars across markup (`**$5** to **$10**`), so a span
    // `not_math` rejects can't just print its source: the emphasis, links
    // and code inside would stay literal. Escape its opening `$` and reparse
    // until every remaining span is math. An escape reshapes only the text
    // after it, and only a code span, autolink or raw HTML shows an escape's
    // backslash — so within a block, later spans are escaped in the same
    // round unless an earlier one's TeX could open one of those. Each round
    // escapes at least one dollar; the bound only matters for pathological
    // input, whose leftovers print as source.
    let mut rounds = 0;
    let events = loop {
        let events: Vec<(Event, Range<usize>)> =
            Parser::new_ext(&text, options).into_offset_iter().collect();
        let mut rejected = Vec::new();
        if rounds < 16 && text.contains('$') {
            let mut stalled = false;
            for (event, range) in &events {
                match event {
                    Event::Start(
                        Tag::Emphasis
                        | Tag::Strong
                        | Tag::Strikethrough
                        | Tag::Link { .. }
                        | Tag::Image { .. },
                    )
                    | Event::End(
                        TagEnd::Emphasis
                        | TagEnd::Strong
                        | TagEnd::Strikethrough
                        | TagEnd::Link
                        | TagEnd::Image,
                    ) => {}
                    Event::Start(_) | Event::End(_) => stalled = false,
                    Event::InlineMath(tex) if !stalled => {
                        let spelled = original(&maps, range.clone());
                        let next = source.as_bytes().get(spelled.end).copied();
                        if source[spelled].starts_with('$') && not_math(tex, next) {
                            rejected.push(range.start);
                            stalled = tex.contains(['`', '<']);
                        }
                    }
                    _ => {}
                }
            }
        }
        if rejected.is_empty() {
            break events;
        }
        let (escaped, map) = escape_dollars(&text, &rejected);
        text = Cow::Owned(escaped);
        maps.push(map);
        rounds += 1;
    };
    let events: Vec<(Event, Range<usize>)> = events
        .into_iter()
        .map(|(event, range)| {
            let range = original(&maps, range);
            (event, range.start + offset..range.end + offset)
        })
        .collect();
    let mut cur = Cursor {
        events: &events,
        ix: 0,
        source,
        offset,
    };
    let mut blocks = Vec::new();
    while let Some((event, range)) = cur.peek() {
        let range = range.clone();
        match event {
            Event::Rule => {
                cur.bump();
                blocks.push(Arc::new(TopBlock {
                    range,
                    block: Block::Rule,
                }));
            }
            Event::Start(_) => {
                for block in parse_started_block(&mut cur) {
                    blocks.push(Arc::new(TopBlock {
                        range: range.clone(),
                        block,
                    }));
                }
            }
            // Stray inline events at top level (shouldn't happen): skip.
            _ => cur.bump(),
        }
    }
    BlockTree { blocks }
}

/// Insert a backslash before the `$` at each (ascending) offset, so it reads
/// as a literal dollar.
fn escape_dollars(text: &str, at: &[usize]) -> (String, OffsetMap) {
    let mut out = String::with_capacity(text.len() + at.len());
    let mut map = OffsetMap::default();
    let mut copied = 0;
    for &at in at {
        out.push_str(&text[copied..at]);
        map.shift(out.len(), -1);
        out.push('\\');
        copied = at;
    }
    out.push_str(&text[copied..]);
    (out, map)
}

/// The edits a rewrite pass made, as `(position, shift)` pairs in rewritten
/// byte order: every rewritten offset past `position` maps back to the
/// original by adding `shift` (the running total of bytes removed minus bytes
/// inserted up to there). Each edit replaces a delimiter with the single byte
/// at `position`, so a range starting on it maps to the original delimiter's
/// start and a range ending after it maps past the original delimiter's end.
#[derive(Debug, Default)]
struct OffsetMap {
    edits: Vec<(usize, isize)>,
}

impl OffsetMap {
    /// Record `delta` original bytes more (or, negative, fewer) behind the
    /// rewritten byte at `position`.
    fn shift(&mut self, position: usize, delta: isize) {
        let total = self.total() + delta;
        self.edits.push((position, total));
    }

    fn total(&self) -> isize {
        self.edits.last().map_or(0, |&(_, total)| total)
    }

    fn original(&self, offset: usize) -> usize {
        match self.edits.partition_point(|&(at, _)| at < offset) {
            0 => offset,
            n => offset.saturating_add_signed(self.edits[n - 1].1),
        }
    }

    fn original_range(&self, range: Range<usize>) -> Range<usize> {
        self.original(range.start)..self.original(range.end)
    }
}

/// [`rewrite_math_delimiters`] over each top-level block on its own (each
/// starting at its line start), so no rewrite or skipped region reaches
/// across a block boundary: a tail reparse from any block start rewrites
/// that tail exactly as the full parse does.
fn rewrite_math_per_block(source: &str, options: Options) -> Option<(String, OffsetMap)> {
    if !source.contains("\\(") && !source.contains("\\[") {
        return None;
    }
    let mut starts = vec![0];
    let mut depth = 0usize;
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        let top = depth == 0;
        match event {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth -= 1,
            _ => {}
        }
        let start = source[..range.start].rfind('\n').map_or(0, |nl| nl + 1);
        if top && starts.last() != Some(&start) {
            starts.push(start);
        }
    }
    starts.push(source.len());
    let mut out = String::with_capacity(source.len());
    let mut map = OffsetMap::default();
    let mut changed = false;
    for block in starts.windows(2) {
        let block = &source[block[0]..block[1]];
        match rewrite_math_delimiters(block) {
            Some((rewritten, edits)) => {
                let (base, total) = (out.len(), map.total());
                map.edits.extend(
                    edits
                        .edits
                        .into_iter()
                        .map(|(at, shift)| (base + at, total + shift)),
                );
                out.push_str(&rewritten);
                changed = true;
            }
            None => out.push_str(block),
        }
    }
    changed.then_some((out, map))
}

/// Rewrite the LaTeX delimiters models favor — `\(…\)` inline, `\[…\]`
/// display — to the `$…$` / `$$…$$` pulldown-cmark understands. Code and
/// URLs keep their spelling (fences, indented lines, inline code spans, link
/// destinations, autolinks and reference definitions are copied verbatim),
/// an opener is rewritten only once its closer has arrived within the same
/// paragraph (so a still-streaming formula stays literal and the rewrite is
/// append-stable), and a backslash that is itself escaped (`\\(`) is a
/// literal backslash, not an opener. An opener glued to a word or a URL
/// (`arr\[0\]`, `f\(x\)`, `x_\(1\)`) is markdown escaping, not math, and an
/// empty pair (`\[ \]`) is an escaped task box. Inline formulas drop the
/// whitespace inside their delimiters, which `$…$` does not allow.
fn rewrite_math_delimiters(source: &str) -> Option<(String, OffsetMap)> {
    if !source.contains("\\(") && !source.contains("\\[") {
        return None;
    }
    let bytes = source.as_bytes();
    let mut rewritten: Option<String> = None;
    let mut map = OffsetMap::default();
    let mut copied = 0usize;
    let mut at = 0usize;
    // The last closer search that failed, per (closer, quoted, heading): it
    // proved there is no closer up to where it stopped. A later opener of the
    // same kind before that point would scan a suffix of the same bytes
    // (escape pairs align: every scan starts just after a `(` or `[`) and
    // fail too, so a paragraph full of unclosed openers stays linear.
    let mut misses: Vec<((u8, bool, bool), usize)> = Vec::new();
    let mut code_spans = CodeSpans::default();
    while at < bytes.len() {
        if at == 0 || bytes[at - 1] == b'\n' {
            if let Some(end) = fence_open(&source[at..])
                .map(|fence| skip_fenced_block(source, at, fence))
                .or_else(|| skip_container_fence(source, at))
            {
                skip(&mut rewritten, source, &mut copied, end);
                at = end;
                continue;
            }
            // Indented code, and a reference definition's destination.
            if line_is_indented_code(&source[at..]) || has_link_defs(line_of(source, at)) {
                let end = line_end(source, at);
                skip(&mut rewritten, source, &mut copied, end);
                at = end;
                continue;
            }
        }
        match bytes[at] {
            b'`' if !is_escaped(bytes, at) => {
                let end = code_spans.end(source, at);
                skip(&mut rewritten, source, &mut copied, end);
                at = end;
            }
            // Link destinations keep their spelling: `\(` there is an
            // escaped paren in a URL.
            b']' if bytes.get(at + 1) == Some(&b'(') && !is_escaped(bytes, at) => {
                let end = destination_close(bytes, at + 2).map_or(at + 2, |close| close + 1);
                skip(&mut rewritten, source, &mut copied, end);
                at = end;
            }
            b'<' if !is_escaped(bytes, at) => {
                let end = autolink_end(bytes, at).unwrap_or(at + 1);
                skip(&mut rewritten, source, &mut copied, end);
                at = end;
            }
            b'\\' if !is_escaped(bytes, at) && matches!(bytes.get(at + 1), Some(b'(' | b'[')) => {
                let display = bytes[at + 1] == b'[';
                // Glued to a word or a URL (`f\(x\)`, `x_\(1\)`): escaping.
                let glued = at > 0
                    && (bytes[at - 1].is_ascii_alphanumeric()
                        || b"_/.-:=?&#%~+@".contains(&bytes[at - 1]));
                let closer = if display { b']' } else { b')' };
                let (in_quote, one_line) = code_spans.lines.at(source, at);
                let key = (closer, in_quote, one_line);
                let known_miss = misses.iter().any(|&(k, stop)| k == key && at + 2 < stop);
                let close = if glued || known_miss {
                    None
                } else {
                    match math_closer(source, at + 2, closer, in_quote, one_line) {
                        Ok(close) => Some(close),
                        Err(stop) => {
                            misses.retain(|&(k, _)| k != key);
                            misses.push((key, stop));
                            None
                        }
                    }
                };
                let Some(close) = close.filter(|&close| !source[at + 2..close].trim().is_empty())
                else {
                    at += 2;
                    continue;
                };
                let out = rewritten.get_or_insert_with(|| {
                    let mut out = String::with_capacity(source.len());
                    out.push_str(&source[..copied]);
                    out
                });
                out.push_str(&source[copied..at]);
                if display {
                    // `\[` and `$$` are the same length: no offsets move.
                    out.push_str("$$");
                    out.push_str(&source[at + 2..close]);
                    out.push_str("$$");
                } else {
                    let body = &source[at + 2..close];
                    let inner = body.trim_start();
                    let lead = body.len() - inner.len();
                    let inner = inner.trim_end();
                    let trail = body.len() - lead - inner.len();
                    // Each `$` stands in for its two-byte delimiter plus the
                    // whitespace it swallowed.
                    map.shift(out.len(), 1 + lead as isize);
                    out.push('$');
                    out.push_str(inner);
                    map.shift(out.len(), 1 + trail as isize);
                    out.push('$');
                }
                copied = close + 2;
                at = close + 2;
            }
            _ => at += 1,
        }
    }
    let mut out = rewritten?;
    out.push_str(&source[copied..]);
    Some((out, map))
}

/// The start of the first unescaped `\` + `closer` from `from` — or, as
/// `Err`, where the search stopped — unless the paragraph ends first: at a
/// blank line (math never crosses one), or at a line that opens another
/// block. The latter keeps an opener and its closer in one top-level block —
/// so the incremental tail reparse always sees both or neither — and keeps a
/// fence's body from ever being rewritten.
fn math_closer(
    source: &str,
    from: usize,
    closer: u8,
    in_quote: bool,
    one_line: bool,
) -> Result<usize, usize> {
    let bytes = source.as_bytes();
    let mut at = from;
    while at < bytes.len() {
        match bytes[at] {
            // Escape pairs (`\\`, `\{`) step over together, so the backslash
            // of a TeX line break never starts a closer.
            b'\\' if bytes.get(at + 1) == Some(&closer) => return Ok(at),
            b'\\' if bytes.get(at + 1) != Some(&b'\n') => at += 2,
            // A link destination's escaped parens aren't closers.
            b']' if bytes.get(at + 1) == Some(&b'(') => {
                at = destination_close(bytes, at + 2).map_or(at + 2, |close| close + 1);
            }
            b'\n' => {
                if one_line || paragraph_ends(source, at, in_quote) {
                    return Err(at);
                }
                at += 1;
            }
            _ => at += 1,
        }
    }
    Err(bytes.len())
}

/// Whether the paragraph holding the newline at `nl` ends there: the next
/// line opens a block, or is a table's header row (the line after it is a
/// delimiter row).
fn paragraph_ends(source: &str, nl: usize, in_quote: bool) -> bool {
    let next = &source[nl + 1..];
    opens_block(next, in_quote)
        || next
            .find('\n')
            .is_some_and(|end| delimiter_row(&next[end + 1..], in_quote))
}

/// Whether a line ends the paragraph above it: a blank line, or (after up to
/// three spaces) a fence, an ATX heading, a list item, a thematic break, a
/// setext underline, a table delimiter row, an HTML block, or a block quote —
/// unless the paragraph is itself quoted, where `>` continues it and the
/// rules apply to what follows the markers. Conservative: a false positive
/// only leaves a formula's delimiters literal.
fn opens_block(line: &str, in_quote: bool) -> bool {
    let line = &line[..line.find('\n').unwrap_or(line.len())];
    let trimmed = line.trim_start_matches(' ');
    if line.trim().is_empty() {
        return true;
    }
    if line.len() - trimmed.len() > 3 {
        return false;
    }
    let trimmed = if in_quote {
        trimmed.trim_start_matches(['>', ' '])
    } else {
        trimmed
    };
    let trimmed = trimmed.trim_end();
    let bytes = trimmed.as_bytes();
    let underline = |mark: u8| !bytes.is_empty() && bytes.iter().all(|&byte| byte == mark);
    let spaced = |at: usize| {
        bytes
            .get(at)
            .is_none_or(|&byte| byte == b' ' || byte == b'\t')
    };
    let hashes = bytes.iter().take_while(|&&byte| byte == b'#').count();
    let digits = bytes
        .iter()
        .take_while(|&&byte| byte.is_ascii_digit())
        .count();
    let thematic = |mark: u8| {
        bytes.iter().filter(|&&byte| byte == mark).count() >= 3
            && bytes.iter().all(|&byte| byte == mark || byte == b' ')
    };
    trimmed.is_empty()
        || fence_open(trimmed).is_some()
        || (1..=6).contains(&hashes) && spaced(hashes)
        || matches!(bytes.first(), Some(b'-' | b'*' | b'+')) && spaced(1)
        || (1..=9).contains(&digits)
            && matches!(bytes.get(digits), Some(b'.' | b')'))
            && spaced(digits + 1)
        || thematic(b'*')
        || thematic(b'_')
        || underline(b'-')
        || underline(b'=')
        || delimiter_row(trimmed, false)
        || bytes.first() == Some(&b'<')
            && bytes
                .get(1)
                .is_some_and(|&byte| byte.is_ascii_alphabetic() || b"/!?".contains(&byte))
        || (!in_quote && bytes.first() == Some(&b'>'))
}

/// A line that could be a table's delimiter row (`|---|:-:|`, `--|--`):
/// only pipes, dashes, colons and spaces, with at least one dash.
fn delimiter_row(line: &str, in_quote: bool) -> bool {
    let line = line[..line.find('\n').unwrap_or(line.len())].trim();
    let line = if in_quote {
        line.trim_start_matches(['>', ' '])
    } else {
        line
    };
    line.contains('-') && line.bytes().all(|byte| b"|-: \t".contains(&byte))
}

/// The line holding byte `at`, without its newline.
fn line_of(source: &str, at: usize) -> &str {
    let start = source[..at].rfind('\n').map_or(0, |nl| nl + 1);
    let end = source[at..].find('\n').map_or(source.len(), |nl| at + nl);
    &source[start..end]
}

/// The paragraph context of the line holding a forward scan's position:
/// whether it is quoted, and whether inline spans on it must close on it —
/// an ATX heading or a table row is one line. Cached per line, and the line
/// start found from the bytes scanned since the last call, so asking at every
/// opener of a long line stays linear.
#[derive(Default)]
struct LineContext {
    start: usize,
    scanned: usize,
    context: Option<(bool, bool)>,
}

impl LineContext {
    fn at(&mut self, source: &str, at: usize) -> (bool, bool) {
        if let Some(nl) = source[self.scanned..at].rfind('\n') {
            self.start = self.scanned + nl + 1;
            self.context = None;
        }
        self.scanned = at;
        *self.context.get_or_insert_with(|| {
            let line = source[self.start..].trim_start();
            let heading = line.starts_with('#') && opens_block(line, true);
            (line.starts_with('>'), heading || line.starts_with('|'))
        })
    }
}

/// Where inline code spans end, for a forward scan. A span closes on the
/// next backtick run of its opening length within its paragraph — code
/// spans never cross a block boundary — or the run stays literal. A failed
/// search is remembered per run length (with its line context): no later
/// run of that length before where it stopped can close either, since the
/// search would have closed on it, so a paragraph of unclosed runs stays
/// linear.
#[derive(Default)]
struct CodeSpans {
    lines: LineContext,
    misses: Vec<((usize, bool, bool), usize)>,
}

impl CodeSpans {
    /// The byte after the span opened by the backtick run at `at` — or after
    /// the run itself when it never closes.
    fn end(&mut self, source: &str, at: usize) -> usize {
        let bytes = source.as_bytes();
        let run = backtick_run(bytes, at);
        let (in_quote, one_line) = self.lines.at(source, at);
        let key = (run, in_quote, one_line);
        if self.misses.iter().any(|&(k, stop)| k == key && at < stop) {
            return at + run;
        }
        match closing_backtick_run(source, at + run, run, in_quote, one_line) {
            Ok(close) => close + run,
            Err(stop) => {
                self.misses.retain(|&(k, _)| k != key);
                self.misses.push((key, stop));
                at + run
            }
        }
    }
}

/// The end of an autolink (`<https://…>`, `<a@b.c>`) opening at `at`: no
/// spaces or angle brackets inside, and a scheme or an address.
fn autolink_end(bytes: &[u8], at: usize) -> Option<usize> {
    let len = bytes[at + 1..]
        .iter()
        .position(|&byte| byte == b'>' || byte == b'<' || byte.is_ascii_whitespace())?;
    let body = &bytes[at + 1..at + 1 + len];
    (bytes[at + 1 + len] == b'>' && body.iter().any(|&byte| byte == b':' || byte == b'@'))
        .then_some(at + len + 2)
}

/// Wrap inline link and image destinations that spell an absolute path with
/// literal spaces in pulldown-cmark's angle-bracket form: CommonMark ends a
/// destination at its first space, so such a link otherwise stays plain text.
/// Code keeps its spelling — a fenced block, an indented line or an inline
/// code span is copied verbatim — and a destination that is already bracketed
/// or whose closing `)` has not streamed in yet is left alone, which makes
/// the rewrite idempotent and safe while text is still arriving.
fn rewrite_absolute_destinations(source: &str) -> Option<(String, OffsetMap)> {
    if !source.contains("](") {
        return None;
    }
    let bytes = source.as_bytes();
    let mut rewritten: Option<String> = None;
    let mut insertions = OffsetMap::default();
    // Bytes before this are already in `rewritten`; skipped regions advance
    // it without copying, so the next splice stays contiguous.
    let mut copied = 0usize;
    let mut at = 0usize;
    let mut code_spans = CodeSpans::default();
    while at < bytes.len() {
        if at == 0 || bytes[at - 1] == b'\n' {
            if let Some(end) = fence_open(&source[at..])
                .map(|fence| skip_fenced_block(source, at, fence))
                .or_else(|| skip_container_fence(source, at))
            {
                skip(&mut rewritten, source, &mut copied, end);
                at = end;
                continue;
            }
            if line_is_indented_code(&source[at..]) {
                let end = line_end(source, at);
                skip(&mut rewritten, source, &mut copied, end);
                at = end;
                continue;
            }
        }
        match bytes[at] {
            b'`' if !is_escaped(bytes, at) => {
                let end = code_spans.end(source, at);
                skip(&mut rewritten, source, &mut copied, end);
                at = end;
            }
            b']' if bytes.get(at + 1) == Some(&b'(') && !is_escaped(bytes, at) => {
                let start = skip_destination_spaces(bytes, at + 2);
                let Some(close) = destination_close(bytes, start) else {
                    at += 2;
                    continue;
                };
                let destination = source[start..close].trim_end_matches(' ');
                if absolute_destination_with_spaces(destination) {
                    let out = match &mut rewritten {
                        Some(out) => out,
                        None => rewritten.insert({
                            // Nothing is copied until the first rewrite; the
                            // skipped code regions ahead of it come along with
                            // the prefix.
                            let mut out = String::with_capacity(source.len() + 8);
                            out.push_str(&source[..copied]);
                            out
                        }),
                    };
                    out.push_str(&source[copied..start]);
                    insertions.shift(out.len(), -1);
                    out.push('<');
                    out.push_str(destination);
                    insertions.shift(out.len(), -1);
                    out.push('>');
                    copied = start + destination.len();
                }
                at = close + 1;
            }
            _ => at += 1,
        }
    }
    let mut out = rewritten?;
    out.push_str(&source[copied..]);
    Some((out, insertions))
}

/// Advance the copy cursor over a region that keeps its spelling, appending
/// it to the rewritten text when a rewrite is already in progress.
fn skip(rewritten: &mut Option<String>, source: &str, copied: &mut usize, until: usize) {
    if let Some(out) = rewritten {
        out.push_str(&source[*copied..until]);
    }
    *copied = until;
}

/// Whether a destination needs the angle-bracket rewrite: an absolute path
/// with at least one literal space that stays inside the absolute-path
/// envelope the link classifier accepts. `file://` destinations are exempt
/// from the file-name dot rule, exactly as they are when classified.
fn absolute_destination_with_spaces(destination: &str) -> bool {
    if !destination.contains(' ') {
        return false;
    }
    let path = match destination.strip_prefix("file://") {
        Some(rest) => {
            // Only an empty host keeps this a file path; `file://localhost`
            // and friends are ordinary URLs.
            let host_end = rest.find('/').unwrap_or(rest.len());
            if !rest[..host_end].is_empty() {
                return false;
            }
            &rest[host_end..]
        }
        None if destination.starts_with('/') => destination,
        None => return false,
    };
    if path.len() <= 1
        || path.starts_with("//")
        || path.ends_with('/')
        || destination.contains(['\\', '?'])
        || destination.chars().any(char::is_control)
        || destination
            .split('/')
            .any(|part| matches!(part, "." | ".."))
    {
        return false;
    }
    let file_name = path.rsplit('/').next().unwrap_or(path);
    destination.starts_with("file://") || file_name.contains('.')
}

/// A fence opener: up to three leading spaces, then at least three backticks
/// or tildes. Returns the fence byte and its run length.
fn fence_open(line: &str) -> Option<(u8, usize)> {
    let bytes = line.as_bytes();
    let indent = bytes.iter().take_while(|&&byte| byte == b' ').count();
    if indent > 3 {
        return None;
    }
    let fence = *bytes.get(indent)?;
    if !matches!(fence, b'`' | b'~') {
        return None;
    }
    let len = bytes[indent..].iter().take_while(|&&b| b == fence).count();
    // A backtick fence's info string can't hold a backtick.
    let info = &line[indent + len..line.find('\n').unwrap_or(line.len())];
    (len >= 3 && !(fence == b'`' && info.contains('`'))).then_some((fence, len))
}

/// The byte after a fenced block's closing line (or the end of the source for
/// a still-open fence, whose body is literal by definition).
fn skip_fenced_block(source: &str, at: usize, (fence, len): (u8, usize)) -> usize {
    let bytes = source.as_bytes();
    let mut cursor = line_end(source, at);
    while cursor < bytes.len() {
        let end = line_end(source, cursor);
        let line = &bytes[cursor..end];
        let indent = line.iter().take_while(|&&byte| byte == b' ').count();
        if indent <= 3 && closes_fence(&line[indent..], (fence, len)) {
            return end;
        }
        cursor = end;
    }
    bytes.len()
}

/// Whether a line (from its first non-space byte) closes a fence.
fn closes_fence(line: &[u8], (fence, len): (u8, usize)) -> bool {
    let run = line.iter().take_while(|&&b| b == fence).count();
    run >= len
        && line[run..]
            .iter()
            .all(|&byte| byte == b' ' || byte == b'\n' || byte == b'\r')
}

/// A fence opened inside a block quote or a list item (`> ````, `- ````):
/// the byte after its closing line, or the start of the first line that
/// leaves its container (unquoted, or indented less than the item's
/// content) — the skip never reaches past the container's end.
fn skip_container_fence(source: &str, at: usize) -> Option<usize> {
    let line = &source[at..line_end(source, at)];
    let (width, quoted) = container_prefix(line)?;
    let fence = fence_open(&line[width..])?;
    let mut cursor = line_end(source, at);
    while cursor < source.len() {
        let end = line_end(source, cursor);
        let next = &source[cursor..end];
        let body = if quoted {
            let trimmed = next.trim_start_matches(' ');
            if !trimmed.starts_with('>') {
                return Some(cursor);
            }
            trimmed.trim_start_matches(['>', ' '])
        } else if next.trim().is_empty() {
            cursor = end;
            continue;
        } else if next.bytes().take_while(|&byte| byte == b' ').count() < width {
            return Some(cursor);
        } else {
            next.trim_start_matches(' ')
        };
        if closes_fence(body.as_bytes(), fence) {
            return Some(end);
        }
        cursor = end;
    }
    Some(source.len())
}

/// The width of a line's block-quote and list-item markers, and whether it
/// is quoted; `None` without any.
fn container_prefix(line: &str) -> Option<(usize, bool)> {
    let bytes = line.as_bytes();
    let mut at = 0;
    let mut quoted = false;
    loop {
        let indent = bytes[at..].iter().take_while(|&&byte| byte == b' ').count();
        if indent > 3 {
            break;
        }
        let marker = at + indent;
        let digits = bytes[marker..]
            .iter()
            .take_while(|&&byte| byte.is_ascii_digit())
            .count();
        let len = match bytes.get(marker) {
            Some(b'>') => {
                quoted = true;
                1
            }
            Some(b'-' | b'*' | b'+') => 1,
            Some(b'0'..=b'9')
                if digits <= 9 && matches!(bytes.get(marker + digits), Some(b'.' | b')')) =>
            {
                digits + 1
            }
            _ => break,
        };
        let spaces = bytes[marker + len..]
            .iter()
            .take_while(|&&byte| byte == b' ')
            .count();
        if bytes[marker] != b'>' && spaces == 0 {
            break;
        }
        at = marker + len + spaces;
    }
    (at > 0).then_some((at, quoted))
}

/// Whether a line opens indented code: four leading spaces or a tab. The
/// rewrite only needs the conservative direction — a line it treats as code
/// just keeps today's plain-text rendering.
fn line_is_indented_code(line: &str) -> bool {
    line.starts_with('\t') || line.as_bytes().iter().take_while(|&&b| b == b' ').count() >= 4
}

fn line_end(source: &str, at: usize) -> usize {
    source.as_bytes()[at..]
        .iter()
        .position(|&byte| byte == b'\n')
        .map(|run| at + run + 1)
        .unwrap_or(source.len())
}

/// An odd number of backslashes before `at` escapes the byte there.
fn is_escaped(bytes: &[u8], at: usize) -> bool {
    bytes[..at]
        .iter()
        .rev()
        .take_while(|&&byte| byte == b'\\')
        .count()
        % 2
        == 1
}

fn backtick_run(bytes: &[u8], at: usize) -> usize {
    bytes[at..].iter().take_while(|&&byte| byte == b'`').count()
}

/// The start of the next backtick run as long as `run` in the paragraph —
/// or, as `Err`, where the search stopped (see [`math_closer`]).
fn closing_backtick_run(
    source: &str,
    from: usize,
    run: usize,
    in_quote: bool,
    one_line: bool,
) -> Result<usize, usize> {
    let bytes = source.as_bytes();
    let mut at = from;
    while at < bytes.len() {
        match bytes[at] {
            b'`' => {
                let len = backtick_run(bytes, at);
                if len == run {
                    return Ok(at);
                }
                at += len;
            }
            b'\n' if one_line || paragraph_ends(source, at, in_quote) => return Err(at),
            _ => at += 1,
        }
    }
    Err(bytes.len())
}

fn skip_destination_spaces(bytes: &[u8], at: usize) -> usize {
    let mut at = at;
    while bytes.get(at) == Some(&b' ') {
        at += 1;
    }
    at
}

/// The first unescaped `)` at paren depth zero — the destination's end. `)`
/// inside balanced parentheses belongs to the path, an escaped `)` does not
/// close, and a control character (a newline mid-destination, a tab) leaves
/// the link alone rather than guessing at its shape.
fn destination_close(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut at = start;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => at += 2,
            b'(' => {
                depth += 1;
                at += 1;
            }
            b')' if depth == 0 => return Some(at),
            b')' => {
                depth -= 1;
                at += 1;
            }
            byte if byte.is_ascii_control() => return None,
            _ => at += 1,
        }
    }
    None
}

struct Cursor<'a, 'e> {
    events: &'a [(Event<'e>, Range<usize>)],
    ix: usize,
    /// The original (unrewritten) source the event ranges point into, and
    /// the document offset of its first byte.
    source: &'a str,
    offset: usize,
}

impl<'a, 'e> Cursor<'a, 'e> {
    fn peek(&self) -> Option<&(Event<'e>, Range<usize>)> {
        self.events.get(self.ix)
    }

    fn peek_event(&self) -> Option<&Event<'e>> {
        self.peek().map(|(e, _)| e)
    }

    fn bump(&mut self) {
        self.ix += 1;
    }

    /// The original source spelling of a document range, plus the byte after
    /// it (if any).
    fn spelling(&self, range: &Range<usize>) -> (&'a str, Option<u8>) {
        let (start, end) = (range.start - self.offset, range.end - self.offset);
        (
            &self.source[start..end],
            self.source.as_bytes().get(end).copied(),
        )
    }

    fn next_event(&mut self) -> Option<Event<'e>> {
        let event = self.events.get(self.ix).map(|(e, _)| e.clone());
        if event.is_some() {
            self.ix += 1;
        }
        event
    }
}

fn is_block_tag(tag: &Tag) -> bool {
    matches!(
        tag,
        Tag::Paragraph
            | Tag::Heading { .. }
            | Tag::CodeBlock(_)
            | Tag::BlockQuote(_)
            | Tag::List(_)
            | Tag::Item
            | Tag::Table(_)
            | Tag::HtmlBlock
            | Tag::FootnoteDefinition(_)
    )
}

/// Consume a `Start(tag)` and everything through its matching `End`, producing
/// block(s). Unknown containers are transparent (children splice in).
fn parse_started_block(cur: &mut Cursor) -> Vec<Block> {
    let Some(Event::Start(tag)) = cur.next_event() else {
        return Vec::new();
    };
    match tag {
        Tag::Paragraph => parse_paragraph(cur),
        Tag::Heading { level, .. } => vec![Block::Heading {
            level: heading_level(level),
            runs: parse_inline_container(cur, &InlineStyle::default()),
        }],
        Tag::CodeBlock(kind) => {
            let language = match kind {
                CodeBlockKind::Fenced(info) => {
                    let lang = info.split_whitespace().next().unwrap_or("");
                    if lang.is_empty() {
                        None
                    } else {
                        Some(lang.to_string())
                    }
                }
                CodeBlockKind::Indented => None,
            };
            let mut code = String::new();
            loop {
                match cur.next_event() {
                    Some(Event::Text(t)) => code.push_str(&t),
                    Some(Event::End(_)) | None => break,
                    Some(_) => {}
                }
            }
            // Fenced blocks carry a trailing newline; render per-line without it.
            if code.ends_with('\n') {
                code.pop();
            }
            vec![Block::CodeBlock { language, code }]
        }
        Tag::BlockQuote(_) => vec![Block::BlockQuote {
            children: parse_block_sequence(cur),
        }],
        Tag::List(ordered_start) => {
            let mut items = Vec::new();
            loop {
                match cur.peek_event() {
                    Some(Event::Start(Tag::Item)) => {
                        cur.bump();
                        items.push(parse_block_sequence(cur));
                    }
                    Some(Event::End(_)) | None => {
                        cur.bump();
                        break;
                    }
                    Some(_) => cur.bump(),
                }
            }
            vec![Block::List {
                ordered_start,
                items,
            }]
        }
        Tag::Table(align) => {
            let align = align
                .iter()
                .map(|a| match a {
                    Alignment::Center => TableAlign::Center,
                    Alignment::Right => TableAlign::Right,
                    Alignment::None | Alignment::Left => TableAlign::Left,
                })
                .collect();
            vec![parse_table(cur, align)]
        }
        Tag::HtmlBlock => {
            // Render raw HTML blocks as plain text (zeron's markdown does the same).
            let mut text = String::new();
            loop {
                match cur.next_event() {
                    Some(Event::Html(t)) | Some(Event::Text(t)) => text.push_str(&t),
                    Some(Event::End(_)) | None => break,
                    Some(_) => {}
                }
            }
            let text = text.trim_end_matches('\n').to_string();
            if text.is_empty() {
                Vec::new()
            } else {
                vec![Block::Paragraph {
                    runs: vec![InlineRun {
                        text,
                        style: InlineStyle::default(),
                    }],
                }]
            }
        }
        // Transparent containers (footnote definitions when enabled, etc.).
        _ => parse_block_sequence(cur),
    }
}

/// Parse a block sequence until the container's `End` (consumed). Bare inline
/// events (tight list items) accumulate into an implicit paragraph.
fn parse_block_sequence(cur: &mut Cursor) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut inline_acc: Vec<InlineRun> = Vec::new();
    // Text after a display formula drops its leading break (see
    // `take_display_math`).
    let mut after_math = false;
    while let Some(event) = cur.peek_event() {
        match event {
            Event::End(_) => {
                cur.bump();
                break;
            }
            Event::Start(tag) if is_block_tag(tag) => {
                flush_paragraph(&mut out, &mut inline_acc, std::mem::take(&mut after_math));
                out.extend(parse_started_block(cur));
            }
            Event::Rule => {
                flush_paragraph(&mut out, &mut inline_acc, std::mem::take(&mut after_math));
                cur.bump();
                out.push(Block::Rule);
            }
            _ => {
                if !take_display_math(cur, &mut out, &mut inline_acc, &mut after_math) {
                    parse_inline_event(cur, &mut inline_acc, &InlineStyle::default());
                }
            }
        }
    }
    flush_paragraph(&mut out, &mut inline_acc, after_math);
    out
}

fn flush_paragraph(out: &mut Vec<Block>, acc: &mut Vec<InlineRun>, after_math: bool) {
    if after_math {
        trim_runs_start(acc);
    }
    if !acc.is_empty() {
        out.push(Block::Paragraph {
            runs: merge_runs(std::mem::take(acc)),
        });
    }
}

/// A paragraph's inline content, split around display math: `$$…$$` on its
/// own is a [`Block::Math`], and one mid-paragraph ends the text before it
/// and starts a new paragraph after it. Display math nested in emphasis or a
/// link stays an inline math run. All the pieces share the paragraph's range.
fn parse_paragraph(cur: &mut Cursor) -> Vec<Block> {
    let mut out = Vec::new();
    let mut runs = Vec::new();
    let mut after_math = false;
    while let Some(event) = cur.peek_event() {
        if matches!(event, Event::End(_)) {
            cur.bump();
            break;
        }
        if !take_display_math(cur, &mut out, &mut runs, &mut after_math) {
            parse_inline_event(cur, &mut runs, &InlineStyle::default());
        }
    }
    if after_math {
        trim_runs_start(&mut runs);
    }
    if !runs.is_empty() || out.is_empty() {
        out.push(Block::Paragraph {
            runs: autolink_runs(merge_runs(runs)),
        });
    }
    out
}

/// Consume a display-math event at paragraph level: flush the text before
/// it (minus the soft break or space leading up to the formula) and push the
/// formula as its own block. `false` (nothing consumed) for any other event.
fn take_display_math(
    cur: &mut Cursor,
    out: &mut Vec<Block>,
    acc: &mut Vec<InlineRun>,
    after_math: &mut bool,
) -> bool {
    let Some(Event::DisplayMath(tex)) = cur.peek_event() else {
        return false;
    };
    let tex = tex.trim().to_string();
    cur.bump();
    trim_runs_end(acc);
    if *after_math {
        trim_runs_start(acc);
    }
    if !acc.is_empty() {
        out.push(Block::Paragraph {
            runs: autolink_runs(merge_runs(std::mem::take(acc))),
        });
    }
    out.push(Block::Math { tex });
    *after_math = true;
    true
}

/// Plain text whose edge whitespace is layout, not content: code, math,
/// images and task markers keep their spelling.
fn is_plain(run: &InlineRun) -> bool {
    !run.style.code && !run.style.math && run.style.image.is_none() && run.style.task.is_none()
}

fn trim_runs_end(runs: &mut Vec<InlineRun>) {
    while let Some(last) = runs.last_mut().filter(|run| is_plain(run)) {
        last.text.truncate(last.text.trim_end().len());
        if !last.text.is_empty() {
            break;
        }
        runs.pop();
    }
}

fn trim_runs_start(runs: &mut Vec<InlineRun>) {
    let mut drop = 0;
    for run in runs.iter_mut() {
        if !is_plain(run) {
            break;
        }
        let trimmed = run.text.len() - run.text.trim_start().len();
        run.text.drain(..trimmed);
        if !run.text.is_empty() {
            break;
        }
        drop += 1;
    }
    runs.drain(..drop);
}

fn parse_table(cur: &mut Cursor, align: Vec<TableAlign>) -> Block {
    let mut header = Vec::new();
    let mut rows = Vec::new();
    loop {
        match cur.peek_event() {
            Some(Event::Start(Tag::TableHead)) => {
                cur.bump();
                header = parse_table_cells(cur);
            }
            Some(Event::Start(Tag::TableRow)) => {
                cur.bump();
                rows.push(parse_table_cells(cur));
            }
            Some(Event::End(_)) | None => {
                cur.bump();
                break;
            }
            Some(_) => cur.bump(),
        }
    }
    Block::Table {
        header,
        rows,
        align,
    }
}

fn parse_table_cells(cur: &mut Cursor) -> Vec<Vec<InlineRun>> {
    let mut cells = Vec::new();
    loop {
        match cur.peek_event() {
            Some(Event::Start(Tag::TableCell)) => {
                cur.bump();
                cells.push(parse_inline_container(cur, &InlineStyle::default()));
            }
            Some(Event::End(_)) | None => {
                cur.bump();
                break;
            }
            Some(_) => cur.bump(),
        }
    }
    cells
}

/// Parse inline events until the container's `End` (consumed).
fn parse_inline_container(cur: &mut Cursor, style: &InlineStyle) -> Vec<InlineRun> {
    let mut runs = Vec::new();
    while let Some(event) = cur.peek_event() {
        if matches!(event, Event::End(_)) {
            cur.bump();
            break;
        }
        parse_inline_event(cur, &mut runs, style);
    }
    // Autolink AFTER merging: pulldown splits Text events at would-be
    // emphasis chars ("…/Foo_(bar)" arrives as three events), so scanning
    // per-event would truncate URLs at every underscore.
    autolink_runs(merge_runs(runs))
}

fn parse_inline_event(cur: &mut Cursor, runs: &mut Vec<InlineRun>, style: &InlineStyle) {
    let range = cur.peek().map(|(_, range)| range.clone());
    let Some(event) = cur.next_event() else {
        return;
    };
    let push = |runs: &mut Vec<InlineRun>, text: String, style: InlineStyle| {
        if !text.is_empty() {
            runs.push(InlineRun { text, style });
        }
    };
    match event {
        Event::Text(t) => push(runs, t.into_string(), style.clone()),
        Event::Code(t) => {
            let mut s = style.clone();
            s.code = true;
            push(runs, t.into_string(), s);
        }
        Event::InlineMath(tex) => {
            let range = range.unwrap();
            let (spelling, next) = cur.spelling(&range);
            if spelling.starts_with('$') && not_math(&tex, next) {
                push(runs, spelling.to_string(), style.clone());
            } else {
                let mut s = style.clone();
                s.math = true;
                push(runs, tex.trim().to_string(), s);
            }
        }
        // Display math reaching here sits in a heading, a table cell or a
        // styled span — places that hold runs, so it renders inline.
        Event::DisplayMath(tex) => {
            let mut s = style.clone();
            s.math = true;
            push(runs, tex.trim().to_string(), s);
        }
        Event::SoftBreak => push(runs, " ".into(), style.clone()),
        Event::HardBreak => push(runs, "\n".into(), style.clone()),
        Event::Html(t) | Event::InlineHtml(t) => push(runs, t.into_string(), style.clone()),
        Event::TaskListMarker(done) => {
            let mut style = style.clone();
            style.task = Some(TaskMarker {
                checked: done,
                range: range.unwrap(),
            });
            push(
                runs,
                if done { "[x] ".into() } else { "[ ] ".into() },
                style,
            );
        }
        Event::FootnoteReference(t) => push(runs, format!("[{t}]"), style.clone()),
        Event::Start(tag) => {
            let mut inner = style.clone();
            match tag {
                Tag::Emphasis => inner.italic = true,
                Tag::Strong => inner.bold = true,
                Tag::Strikethrough => inner.strikethrough = true,
                Tag::Image {
                    dest_url, title, ..
                } => {
                    let alt: String = parse_inline_container(cur, style)
                        .iter()
                        .map(|run| run.text.as_str())
                        .collect();
                    inner.image = Some(InlineImage {
                        source: dest_url.to_string(),
                        alt: alt.clone(),
                        title: title.to_string(),
                        link: style.link.clone(),
                    });
                    inner.link = Some(dest_url.into_string());
                    runs.push(InlineRun {
                        text: alt,
                        style: inner,
                    });
                    return;
                }
                Tag::Link { dest_url, .. } => {
                    inner.link = Some(dest_url.into_string());
                }
                _ => {}
            }
            runs.extend(parse_inline_container(cur, &inner));
        }
        // `End` is consumed by the container loop; anything else is ignored.
        _ => {}
    }
}

/// Whether a `$…$` span (`tex` between the dollars, `next` the byte after
/// the closer) is really prose that happens to hold two dollar signs:
/// - Pandoc's currency rule: a closer glued to a digit is a price (`$5-$10`);
/// - a span ending in `/`, `:` or `@` joins shell variables
///   (`$HOME/$USER`, `PATH=$PATH:$HOME`, `$USER@$HOST`) — no formula ends so;
/// - an all-caps identifier whose closer runs into another identifier is a
///   shell variable (`$FOO$BAR`), while `$AB$` or `$n$th` stay math;
/// - TeX has no use for a backtick: one inside belongs to a code span the
///   dollars straddle (`$HOME and `$PATH``).
fn not_math(tex: &str, next: Option<u8>) -> bool {
    let shell_name = tex.len() > 1
        && tex
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && tex.as_bytes()[0].is_ascii_uppercase();
    next.is_some_and(|b| b.is_ascii_digit())
        || tex.ends_with(['/', ':', '@'])
        || tex.contains('`')
        || (shell_name && next.is_some_and(|b| b.is_ascii_uppercase() || b == b'_'))
}

/// Promote bare `http(s)://` URLs into link runs — GFM's autolink extension,
/// which pulldown-cmark has no option for (agents paste naked PR/issue URLs
/// constantly; user report: the link isn't clickable). Runs already inside a
/// link, code span or formula pass through untouched. Idempotent, so nested containers
/// re-applying it on their merged output is harmless.
fn autolink_runs(runs: Vec<InlineRun>) -> Vec<InlineRun> {
    let mut out = Vec::with_capacity(runs.len());
    for run in runs {
        if run.style.link.is_some() || run.style.code || run.style.math {
            out.push(run);
        } else {
            push_text_autolinked(&mut out, &run.text, &run.style);
        }
    }
    out
}

fn push_text_autolinked(runs: &mut Vec<InlineRun>, text: &str, style: &InlineStyle) {
    let push = |runs: &mut Vec<InlineRun>, text: &str, style: InlineStyle| {
        if !text.is_empty() {
            runs.push(InlineRun {
                text: text.to_string(),
                style,
            });
        }
    };
    let mut rest = text;
    while let Some(at) = find_url_start(rest) {
        let from = &rest[at..];
        let scheme = if from.starts_with("https://") {
            "https://".len()
        } else {
            "http://".len()
        };
        let len = bare_url_len(from);
        if len <= scheme {
            // A scheme with nothing after it stays text (don't re-find it).
            push(runs, &rest[..at + scheme], style.clone());
            rest = &from[scheme..];
            continue;
        }
        push(runs, &rest[..at], style.clone());
        let mut linked = style.clone();
        linked.link = Some(from[..len].to_string());
        push(runs, &from[..len], linked);
        rest = &from[len..];
    }
    push(runs, rest, style.clone());
}

/// First viable `http(s)://` occurrence: not glued to a preceding
/// alphanumeric (`foohttps://…` stays text, per GFM's boundary rule).
fn find_url_start(text: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(rel) = text[from..].find("http") {
        let at = from + rel;
        let after = &text[at..];
        let is_scheme = after.starts_with("http://") || after.starts_with("https://");
        let boundary = text[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        if is_scheme && boundary {
            return Some(at);
        }
        from = at + "http".len();
    }
    None
}

/// Byte length of the bare URL at the start of `text`: run to whitespace (or
/// a delimiter that never appears in pasted URLs), then trim the trailing
/// punctuation GFM excludes — a closing paren only stays when an opener
/// inside the URL balances it ("…/Foo_(bar))" keeps one, sheds one).
fn bare_url_len(text: &str) -> usize {
    let end = text
        .char_indices()
        .find(|(_, c)| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`'))
        .map_or(text.len(), |(i, _)| i);
    let mut url = &text[..end];
    while let Some(last) = url.chars().next_back() {
        let trim = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '*' | '_' | '~' => true,
            ')' => url.matches('(').count() < url.matches(')').count(),
            _ => false,
        };
        if !trim {
            break;
        }
        url = &url[..url.len() - last.len_utf8()];
    }
    url.len()
}

/// Merge adjacent identically-styled runs (keeps run counts small and makes the
/// tree canonical for equality tests). Images and math are atoms: two formulas
/// side by side stay two formulas.
fn merge_runs(runs: Vec<InlineRun>) -> Vec<InlineRun> {
    let mut out: Vec<InlineRun> = Vec::with_capacity(runs.len());
    for run in runs {
        match out.last_mut() {
            Some(last)
                if last.style == run.style && run.style.image.is_none() && !run.style.math =>
            {
                last.text.push_str(&run.text)
            }
            _ => out.push(run),
        }
    }
    out
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

// ---------------------------------------------------------------------------
// Incremental parse
// ---------------------------------------------------------------------------

/// Streaming parser: appends reparse only from the last stable top-level block
/// boundary (snapped back to a line start so indentation context survives).
#[derive(Debug, Default)]
pub struct IncrementalParser {
    source: String,
    tree: BlockTree,
    /// Display-only replacement for the last top-level block when its source
    /// has hanging inline markers ([`crate::mend`]): `None` means the display
    /// tree is exactly [`Self::tree`]. Never fed back into the incremental
    /// state — the canonical tree stays parity-exact with `parse_full`.
    display_tail: Option<Vec<Arc<TopBlock>>>,
    /// Index of the first canonical block the display tail replaces: the last
    /// block and any siblings sharing its range (a paragraph split around
    /// display math reparses as a unit).
    display_from: usize,
    /// Link-reference definitions act at a distance — full reparses only.
    full_only: bool,
    /// Bytes fed through `parse_full` by the most recent `set_text`/`append`/
    /// `reset` — instrumentation proving per-append work is O(tail), not
    /// O(total). 0 for a no-op set_text.
    last_parse_bytes: usize,
    /// Number of leading top-level blocks guaranteed untouched by the most
    /// recent update (render caches for these blocks stay valid).
    stable_prefix_blocks: usize,
}

impl IncrementalParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn tree(&self) -> &BlockTree {
        &self.tree
    }

    /// The tree to render while streaming: the canonical tree with the last
    /// block swapped for its mended parse when inline markers hang (an
    /// unclosed `**bold`, a half-streamed `[link](url…`). Same shape and cost
    /// as `tree().clone()` — the stable prefix shares its blocks; only a
    /// hanging tail adds one O(tail) reparse, done at append time.
    pub fn display_tree(&self) -> BlockTree {
        let Some(tail) = &self.display_tail else {
            return self.tree.clone();
        };
        let stable = &self.tree.blocks[..self.display_from];
        let mut blocks = Vec::with_capacity(stable.len() + tail.len());
        blocks.extend_from_slice(stable);
        blocks.extend_from_slice(tail);
        BlockTree { blocks }
    }

    /// Bytes actually reparsed by the last update (see field docs).
    pub fn last_parse_bytes(&self) -> usize {
        self.last_parse_bytes
    }

    /// Leading top-level blocks left untouched by the last update.
    pub fn stable_prefix_blocks(&self) -> usize {
        self.stable_prefix_blocks
    }

    /// Set the source: appends take the incremental path, anything else resets.
    pub fn set_text(&mut self, text: &str) {
        if text.len() >= self.source.len() && text.starts_with(self.source.as_str()) {
            let delta = &text[self.source.len()..];
            if delta.is_empty() {
                self.last_parse_bytes = 0;
                self.stable_prefix_blocks = self.tree.blocks.len();
                return;
            }
            self.append(delta);
        } else {
            self.reset(text);
        }
    }

    pub fn reset(&mut self, text: &str) {
        self.source = text.to_string();
        self.full_only = has_link_defs(text);
        self.tree = parse_full(text);
        self.last_parse_bytes = text.len();
        self.stable_prefix_blocks = 0;
        self.remend();
    }

    /// Append streamed text, reparsing from the last stable boundary.
    pub fn append(&mut self, delta: &str) {
        if delta.is_empty() {
            self.last_parse_bytes = 0;
            self.stable_prefix_blocks = self.tree.blocks.len();
            return;
        }
        // The delta may complete a line begun earlier — rescan from that line's
        // start when checking for definitions.
        let scan_from = self.source.rfind('\n').map(|i| i + 1).unwrap_or(0);
        self.source.push_str(delta);
        if !self.full_only && has_link_defs(&self.source[scan_from..]) {
            self.full_only = true;
        }
        if self.full_only {
            self.tree = parse_full(&self.source);
            self.last_parse_bytes = self.source.len();
            self.stable_prefix_blocks = 0;
            self.remend();
            return;
        }

        // Stable boundary: start of the SECOND-to-last top-level block, snapped
        // back to its line start (keeps indented-code / fenced-indent context
        // intact). Reparsing the last two blocks — not just the last — covers
        // continuation merges: a trailing paragraph like `3` can become `3.`
        // and fuse into the preceding loose list. Merges cannot cascade
        // further back (a block's separation from its predecessor is decided
        // by its own already-streamed leading bytes), so two blocks suffice;
        // the parity tests stream corpora to hold this invariant.
        // Blocks split from one source block (a paragraph around display
        // math) share its range and count as one here.
        let last_start = self.tree.blocks.last().map_or(0, |b| b.range.start);
        let boundary = self
            .tree
            .blocks
            .iter()
            .rev()
            .map(|b| b.range.start)
            .find(|&start| start < last_start)
            .unwrap_or(0);
        let boundary = self.source[..boundary]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);

        let tail = parse_at(&self.source[boundary..], boundary);
        self.last_parse_bytes = self.source.len() - boundary;
        self.tree.blocks.retain(|b| b.range.start < boundary);
        self.stable_prefix_blocks = self.tree.blocks.len();
        for top in tail.blocks {
            self.tree.blocks.push(top);
        }
        self.remend();
    }

    /// Recompute the display tail: mend hanging inline markers in the last
    /// top-level block (only place they can hang — a blank line settles a
    /// block, and CommonMark keeps unclosed markers literal across it) and
    /// reparse just that block's source. `close_hanging` is one O(last block)
    /// scan and returns `None` when nothing hangs, so the extra parse happens
    /// only while a marker is actually open.
    fn remend(&mut self) {
        self.display_tail = None;
        let Some(last) = self.tree.blocks.last() else {
            return;
        };
        // Code blocks render an unclosed fence verbatim (already stable);
        // rules, tables and a paragraph ending in display math have no inline
        // tail to mend.
        if matches!(
            last.block,
            Block::CodeBlock { .. } | Block::Rule | Block::Table { .. } | Block::Math { .. }
        ) {
            return;
        }
        let start = last.range.start;
        let Some(mended) = crate::mend::close_hanging(&self.source[start..]) else {
            return;
        };
        // Count toward the O(tail) instrumentation — this is real parse work,
        // in the same bound as the reparse that produced the block.
        self.last_parse_bytes += mended.len();
        let mut tail = parse_at(&mended, start).blocks;
        for top in &mut tail {
            let top = Arc::make_mut(top);
            // Display ranges point back into the unmended source; synthetic
            // closers at the end clamp away.
            top.range.end = top.range.end.min(self.source.len());
        }
        self.display_from = self
            .tree
            .blocks
            .iter()
            .rposition(|b| b.range.start != start)
            .map_or(0, |i| i + 1);
        self.display_tail = Some(tail);
    }
}

/// Conservative detector for link-reference-definition lines
/// (`[label]: destination`, up to 3 leading spaces).
fn has_link_defs(text: &str) -> bool {
    text.lines().any(|line| {
        let trimmed = line.trim_start();
        line.len() - trimmed.len() <= 3 && trimmed.starts_with('[') && trimmed.contains("]:")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_ranges_survive_nested_lists_unicode_crlf_and_streaming() {
        let source = "Título 🦀\r\n\r\nIntro\r\n\r\n- [ ] repetida\r\n  - [X] repetida\r\n\r\n> - [x] citada\r\n\r\n```md\r\n- [ ] literal\r\n```\r\n";
        fn collect(block: &Block, tasks: &mut Vec<TaskMarker>) {
            match block {
                Block::Paragraph { runs } => {
                    tasks.extend(runs.iter().filter_map(|run| run.style.task.clone()))
                }
                Block::List { items, .. } => items
                    .iter()
                    .flatten()
                    .for_each(|block| collect(block, tasks)),
                Block::BlockQuote { children } => {
                    children.iter().for_each(|block| collect(block, tasks))
                }
                _ => {}
            }
        }
        let tree = parse_full(source);
        let mut tasks = Vec::new();
        for top in &tree.blocks {
            collect(&top.block, &mut tasks);
        }
        assert_eq!(tasks.len(), 3);
        assert_eq!(
            tasks
                .iter()
                .map(|task| &source[task.range.clone()])
                .collect::<Vec<_>>(),
            ["[ ]", "[X]", "[x]"]
        );
        let mut stream = IncrementalParser::new();
        for ch in source.chars() {
            stream.append(&ch.to_string());
            assert_eq!(stream.tree(), &parse_full(stream.source()));
        }
    }

    #[test]
    fn display_snapshots_share_stable_blocks_without_mutating_old_frames() {
        let mut parser = IncrementalParser::new();
        parser.set_text("first **bold** paragraph\n\nsecond paragraph\n\nlast **open");
        let before = parser.display_tree();
        let frozen = format!("{before:?}");
        assert!(Arc::ptr_eq(&before.blocks[0], &parser.tree().blocks[0]));
        parser.append(" tail**\n\nnext paragraph");
        let after = parser.display_tree();
        assert!(Arc::ptr_eq(&before.blocks[0], &after.blocks[0]));
        assert_eq!(
            format!("{before:?}"),
            frozen,
            "old paint snapshot is immutable"
        );
        assert_eq!(parser.tree(), &parse_full(parser.source()));
        parser.reset("replacement");
        assert_eq!(
            format!("{before:?}"),
            frozen,
            "reset cannot alter an old frame"
        );
        assert_eq!(parser.tree(), &parse_full("replacement"));
    }

    fn stream(chunks: usize, text: &str) -> IncrementalParser {
        let mut p = IncrementalParser::new();
        let bytes = text.as_bytes();
        let mut start = 0;
        while start < bytes.len() {
            let mut end = (start + chunks).min(bytes.len());
            while end < bytes.len() && !text.is_char_boundary(end) {
                end += 1;
            }
            p.append(&text[start..end]);
            start = end;
        }
        p
    }

    const CORPORA: &[&str] = &[
        "# Title\n\nHello **bold** and *italic* and `code` and ~~gone~~.\n",
        "Paragraph one\nlazy continuation\n\nParagraph two with a [link](https://x.dev).\n",
        "- item one\n- item two\n  - nested a\n  - nested b\n- item three\n\ntail\n",
        "1. first\n2. second\n\n   loose paragraph in item\n\n3. third\n",
        "```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n\nafter code\n",
        "intro\n\n```\nunclosed fence streaming",
        "> quoted line\n> more quote\n>\n> - a list in a quote\n\nplain\n",
        "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n\ndone\n",
        "setext candidate\n===\n\nnext para\n---\n",
        "***\n\ntext between rules\n\n---\n",
        "- [x] done task\n- [ ] open task\n",
        "    indented code line one\n    line two\n\npara\n",
        "see [it's here](/tmp/2026/Some Folder/it's here.txt) and `[raw](/tmp/a b.md)`\n",
        "text\n\n```\n[x](/tmp/a b.md)\n```\n\nafter [y](file:///tmp/c d.md)\n",
        "para with <span>inline html</span> inside\n\n<div>\nblock html\n</div>\n",
        "###### deep heading\n\n#### h4\n",
        "Euler: $e^{i\\pi} + 1 = 0$ and $a_1 * b_2$.\n\n$$\n\\sum_{k=1}^n k = \\frac{n(n+1)}{2}\n$$\n\nafter\n",
        "Before\n$$x^2$$\nmiddle **bold** $$y$$ tail\n\n- item $$z$$\n- $a$\n\n> $$q$$\n",
        "Prose \\(a^2 + b^2\\) here.\n\n\\[\n\\int_0^1 f(x)\\,dx\n\\]\n\nand `\\(code\\)` then \\[ e = mc^2 \\] end\n",
        "costs $5-$10 or between $5 and $10, but $x$ is math\n\n# Heading $$h$$\n\n| m |\n|---|\n| $$c$$ |\n",
        "# T \\(\nx \\) y\npara \\[\n- item \\]\n\n> \\[\n> q_1\n> \\]\n\ntext\n```\n\\(raw\\)\n```\n",
    ];

    #[test]
    fn incremental_matches_full_on_streamed_corpora() {
        for (ci, corpus) in CORPORA.iter().enumerate() {
            let full = parse_full(corpus);
            for chunk in [1usize, 2, 3, 7, 16, 64] {
                let inc = stream(chunk, corpus);
                assert_eq!(
                    inc.tree(),
                    &full,
                    "corpus {ci} diverged at chunk size {chunk}:\n{corpus}"
                );
            }
        }
    }

    /// Random documents over the characters the pre-passes and math rules
    /// key on, streamed in every chunk size: the canonical tree must equal a
    /// one-shot parse, and the display tree a one-shot `set_text`'s.
    #[test]
    fn incremental_matches_full_on_random_documents() {
        const PIECES: &[&str] = &[
            "\\", "(", ")", "[", "]", "$", "$$", "`", "```", "-", "=", "|", "#", ">", "*", "_",
            "\n", "\n\n", " ", " ", "é", "漢", "x", "y", "1", ":", "/", "**", "\\(", "\\)", "\\[",
            "\\]", "|---|", "](u)", "~~", "\n> ", "\n- ", "\n1. ", "\n    ", "\t", "<a:b>", "<p>",
            "$5", "$HOME", "@", "'", ":-:",
        ];
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut failures = Vec::new();
        for case in 0..8000 {
            let len = 8 + (next() % 56) as usize;
            let doc: String = (0..len)
                .map(|_| PIECES[(next() % PIECES.len() as u64) as usize])
                .collect();
            let full = parse_full(&doc);
            let mut settled = IncrementalParser::new();
            settled.set_text(&doc);
            let chunk = 1 + case % 17;
            let streamed = stream(chunk, &doc);
            if streamed.tree() != &full || streamed.display_tree() != settled.display_tree() {
                failures.push((chunk, doc));
            }
        }
        assert!(
            failures.is_empty(),
            "{} diverged: {failures:#?}",
            failures.len()
        );
    }

    #[test]
    fn appends_keep_committed_blocks_identical() {
        // Streaming stability invariant: blocks before the reparse boundary
        // (everything but the last two source blocks — pieces of a paragraph
        // split around display math share a range and count as one) must be
        // reused as-is across appends — same index, same value — so
        // row/element keys never re-mount and earlier blocks can never
        // visibly reflow.
        for corpus in CORPORA {
            let mut p = IncrementalParser::new();
            let mut prev = p.tree().clone();
            let bytes = corpus.as_bytes();
            let mut start = 0;
            while start < bytes.len() {
                let mut end = (start + 3).min(bytes.len());
                while end < bytes.len() && !corpus.is_char_boundary(end) {
                    end += 1;
                }
                p.append(&corpus[start..end]);
                start = end;

                let cur = p.tree();
                let mut starts: Vec<usize> = prev.blocks.iter().map(|b| b.range.start).collect();
                starts.dedup();
                let committed = match starts.len() {
                    0..=2 => 0,
                    n => prev
                        .blocks
                        .partition_point(|b| b.range.start < starts[n - 2]),
                };
                assert!(
                    cur.blocks.len() >= committed,
                    "committed blocks disappeared:\n{corpus}"
                );
                for i in 0..committed {
                    assert_eq!(
                        cur.blocks[i], prev.blocks[i],
                        "block {i} changed across an append:\n{corpus}"
                    );
                }
                prev = cur.clone();
            }
        }
    }

    #[test]
    fn incremental_matches_full_with_link_definitions() {
        // Definitions act at a distance → parser falls back to full reparses,
        // so parity must still hold.
        let corpus = "See [docs] for more.\n\nMore text.\n\n[docs]: https://example.com\n";
        let full = parse_full(corpus);
        for chunk in [1usize, 3, 9] {
            assert_eq!(stream(chunk, corpus).tree(), &full, "chunk {chunk}");
        }
        // The reference actually resolved into a link.
        let has_link = full.blocks.iter().any(|b| match &b.block {
            Block::Paragraph { runs } => runs.iter().any(|r| r.style.link.is_some()),
            _ => false,
        });
        assert!(has_link, "expected [docs] to resolve to a link");
    }

    #[test]
    fn set_text_appends_or_resets() {
        let mut p = IncrementalParser::new();
        p.set_text("hello");
        p.set_text("hello world");
        assert_eq!(p.tree(), &parse_full("hello world"));
        // Non-append rewrites reset cleanly.
        p.set_text("different");
        assert_eq!(p.tree(), &parse_full("different"));
        assert_eq!(p.source(), "different");
    }

    #[test]
    fn block_structure_basics() {
        let tree = parse_full("## Head\n\npara **b _bi_** text\n\n```ts\nlet x = 1;\n```\n");
        assert_eq!(tree.len(), 3);
        match &tree.blocks[0].block {
            Block::Heading { level, runs } => {
                assert_eq!(*level, 2);
                assert_eq!(runs[0].text, "Head");
            }
            other => panic!("unexpected {other:?}"),
        }
        match &tree.blocks[1].block {
            Block::Paragraph { runs } => {
                assert_eq!(runs.len(), 4); // "para ", "b ", "bi" (bold+italic), " text"
                assert!(runs[1].style.bold && !runs[1].style.italic);
                assert!(runs[2].style.bold && runs[2].style.italic);
            }
            other => panic!("unexpected {other:?}"),
        }
        match &tree.blocks[2].block {
            Block::CodeBlock { language, code } => {
                assert_eq!(language.as_deref(), Some("ts"));
                assert_eq!(code, "let x = 1;");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn nested_lists_and_tight_items() {
        let tree = parse_full("- a\n  - a1\n  - a2\n- b\n");
        let Block::List {
            ordered_start,
            items,
        } = &tree.blocks[0].block
        else {
            panic!("expected list");
        };
        assert_eq!(*ordered_start, None);
        assert_eq!(items.len(), 2);
        // Tight item text became an implicit paragraph, nested list follows.
        assert!(matches!(items[0][0], Block::Paragraph { .. }));
        assert!(matches!(items[0][1], Block::List { .. }));
    }

    #[test]
    fn tables_parse_header_and_rows() {
        let tree = parse_full("| a | b |\n|---|---|\n| 1 | 2 |\n");
        let Block::Table {
            header,
            rows,
            align,
        } = &tree.blocks[0].block
        else {
            panic!("expected table");
        };
        assert_eq!(header.len(), 2);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][1][0].text, "2");
        assert_eq!(align, &vec![TableAlign::Left, TableAlign::Left]);
    }

    #[test]
    fn tables_parse_column_alignment() {
        let tree = parse_full("| a | b | c |\n|:--|:-:|--:|\n| 1 | 2 | 3 |\n");
        let Block::Table { align, .. } = &tree.blocks[0].block else {
            panic!("expected table");
        };
        assert_eq!(
            align,
            &vec![TableAlign::Left, TableAlign::Center, TableAlign::Right]
        );
    }

    #[test]
    fn links_carry_urls() {
        let tree = parse_full("go to [zed](https://zed.dev) now\n");
        let Block::Paragraph { runs } = &tree.blocks[0].block else {
            panic!()
        };
        let link = runs
            .iter()
            .find(|r| r.style.link.is_some())
            .expect("link run");
        assert_eq!(link.text, "zed");
        assert_eq!(link.style.link.as_deref(), Some("https://zed.dev"));
    }

    /// The paragraph's single link run: (text, url).
    fn only_link(source: &str) -> Option<(String, String)> {
        let tree = parse_full(source);
        let Block::Paragraph { runs } = &tree.blocks[0].block else {
            panic!()
        };
        let links: Vec<_> = runs
            .iter()
            .filter_map(|r| Some((r.text.clone(), r.style.link.clone()?)))
            .collect();
        assert!(links.len() <= 1, "expected at most one link: {links:?}");
        links.into_iter().next()
    }

    /// Bare URLs autolink (the GFM extension pulldown-cmark lacks): the URL
    /// becomes a clickable run, trailing sentence punctuation stays text.
    #[test]
    fn bare_urls_autolink() {
        assert_eq!(
            only_link("PR is updated: https://github.com/zeronsh/comet/pull/31\n"),
            Some((
                "https://github.com/zeronsh/comet/pull/31".into(),
                "https://github.com/zeronsh/comet/pull/31".into()
            ))
        );
        assert_eq!(
            only_link("see https://x.dev/a, then rest.\n").map(|l| l.1),
            Some("https://x.dev/a".into())
        );
        // A wrapping paren is shed; one balanced by an opener in the path stays.
        assert_eq!(
            only_link("(docs: https://x.dev/Foo_(bar))\n").map(|l| l.1),
            Some("https://x.dev/Foo_(bar)".into())
        );
        // Bold text still autolinks, and the run keeps the emphasis.
        let tree = parse_full("**see https://x.dev now**\n");
        let Block::Paragraph { runs } = &tree.blocks[0].block else {
            panic!()
        };
        let link = runs.iter().find(|r| r.style.link.is_some()).unwrap();
        assert!(link.style.bold);
        assert_eq!(link.style.link.as_deref(), Some("https://x.dev"));
    }

    /// Non-links stay text: glued schemes, bare schemes, code spans, and the
    /// destination text of a real markdown link.
    #[test]
    fn autolink_leaves_non_urls_alone() {
        assert_eq!(only_link("foohttps://x.dev is glued\n"), None);
        assert_eq!(only_link("the https:// scheme alone\n"), None);
        assert_eq!(only_link("`https://x.dev` in code\n"), None);
        // A markdown link whose TEXT is a URL keeps the written destination.
        assert_eq!(
            only_link("[https://shown.dev](https://real.dev)\n"),
            Some(("https://shown.dev".into(), "https://real.dev".into()))
        );
    }

    #[test]
    fn top_level_ranges_are_stable_anchors() {
        let src = "first\n\nsecond\n\nthird";
        let tree = parse_full(src);
        assert_eq!(tree.len(), 3);
        assert!(
            tree.blocks
                .windows(2)
                .all(|w| w[0].range.start < w[1].range.start)
        );
        assert_eq!(&src[tree.blocks[1].range.clone()], "second\n");
    }

    /// Concatenated visible text of a block (what the user reads).
    fn flat(block: &Block) -> String {
        fn walk(b: &Block, out: &mut String) {
            match b {
                Block::Paragraph { runs } | Block::Heading { runs, .. } => {
                    for r in runs {
                        out.push_str(&r.text);
                    }
                }
                Block::BlockQuote { children } => children.iter().for_each(|c| walk(c, out)),
                Block::List { items, .. } => items.iter().flatten().for_each(|c| walk(c, out)),
                _ => {}
            }
        }
        let mut s = String::new();
        walk(block, &mut s);
        s
    }

    #[test]
    fn display_tree_styles_hanging_bold_immediately() {
        let mut p = IncrementalParser::new();
        p.set_text("intro **bo");
        let display = p.display_tree();
        let Block::Paragraph { runs } = &display.blocks[0].block else {
            panic!("expected paragraph");
        };
        let bold: Vec<_> = runs.iter().filter(|r| r.style.bold).collect();
        assert_eq!(bold.len(), 1);
        assert_eq!(bold[0].text, "bo");
        assert!(!flat(&display.blocks[0].block).contains("**"));
        // The canonical tree stays honest: literal markers until truly closed.
        assert!(flat(&p.tree().blocks[0].block).contains("**"));
    }

    #[test]
    fn display_tree_converges_to_canonical_when_balanced() {
        let corpus = "a **b** *c* `d` [e](https://x.dev) ~~f~~";
        let mut p = IncrementalParser::new();
        p.set_text(corpus);
        assert_eq!(p.display_tree(), *p.tree());
        assert_eq!(p.tree(), &parse_full(corpus));
    }

    #[test]
    fn display_tree_never_leaks_streaming_urls() {
        let full = "read [docs](https://example.com/long/path) now";
        let mut p = IncrementalParser::new();
        for i in 1..=full.len() {
            if !full.is_char_boundary(i) {
                continue;
            }
            p.set_text(&full[..i]);
            let text = flat(&p.display_tree().blocks[0].block);
            assert!(!text.contains("http"), "url leaked at {i}: {text:?}");
        }
        // Mid-URL the link text carries the pending sentinel destination.
        let mut p = IncrementalParser::new();
        p.set_text("read [docs](https://exa");
        let Block::Paragraph { runs } = &p.display_tree().blocks[0].block else {
            panic!("expected paragraph");
        };
        let link = runs
            .iter()
            .find(|r| r.style.link.is_some())
            .expect("link run");
        assert_eq!(link.text, "docs");
        assert_eq!(
            link.style.link.as_deref(),
            Some(crate::mend::PENDING_LINK_URL)
        );
    }

    #[test]
    fn display_tree_leaves_code_blocks_alone() {
        let mut p = IncrementalParser::new();
        p.set_text("intro\n\n```\nunclosed **fence");
        assert_eq!(p.display_tree(), *p.tree());
    }

    #[test]
    fn display_tree_suppresses_setext_flicker() {
        // "para" + "\n-" parses as an H2 for exactly one chunk before the
        // list item's text arrives; the display tree keeps it a paragraph.
        let mut p = IncrementalParser::new();
        p.set_text("para\n-");
        let display = p.display_tree();
        assert!(
            matches!(
                display.blocks.last().unwrap().block,
                Block::Paragraph { .. }
            ),
            "expected paragraph, got {display:?}"
        );
    }

    #[test]
    fn display_tree_prefix_matches_canonical_across_streams() {
        // Mending swaps only the last block: everything before it must be
        // byte-identical to the canonical tree so render caches and row keys
        // survive.
        for corpus in CORPORA {
            let mut p = IncrementalParser::new();
            let bytes = corpus.as_bytes();
            let mut start = 0;
            while start < bytes.len() {
                let mut end = (start + 3).min(bytes.len());
                while end < bytes.len() && !corpus.is_char_boundary(end) {
                    end += 1;
                }
                p.append(&corpus[start..end]);
                start = end;

                let display = p.display_tree();
                let canonical = p.tree();
                for i in 0..canonical.blocks.len().saturating_sub(1) {
                    assert_eq!(
                        display.blocks[i], canonical.blocks[i],
                        "display prefix diverged:\n{corpus}"
                    );
                }
            }
        }
    }

    #[test]
    fn empty_and_whitespace_sources() {
        assert!(parse_full("").is_empty());
        assert!(parse_full("\n\n  \n").is_empty());
        let mut p = IncrementalParser::new();
        p.append("");
        assert!(p.tree().is_empty());
    }
}

#[cfg(test)]
mod closing_quote_blocks {
    use super::*;

    const STORY: &str = "\"How do we negotiate with machines that won't speak?\" someone asked.\n\nYuki almost laughed. \"You don't. You listen to the silence. And you finally understand what it means to be powerless.\"";

    #[test]
    fn full_parse_keeps_trailing_quote_in_block() {
        let tree = parse_full(STORY);
        for b in &tree.blocks {
            eprintln!("block {:?} => {:?}", b.range, &STORY[b.range.clone()]);
        }
        assert_eq!(tree.blocks.len(), 2, "two paragraphs expected");
        let last = &tree.blocks[1];
        assert!(STORY[last.range.clone()].ends_with("powerless.\""));
    }

    #[test]
    fn streamed_boundary_at_quote_adds_no_block() {
        // Stream with a commit boundary exactly between `powerless.` and `"`.
        let split = STORY.len() - 1;
        let mut p = IncrementalParser::new();
        p.set_text(&STORY[..split]);
        p.set_text(STORY);
        let tree = p.tree();
        for b in &tree.blocks {
            eprintln!("block {:?} => {:?}", b.range, &STORY[b.range.clone()]);
        }
        assert_eq!(tree.blocks.len(), 2, "streamed split must not add blocks");
        assert!(STORY[tree.blocks[1].range.clone()].ends_with("powerless.\""));
    }

    #[test]
    fn streamed_small_chunks_match_full_parse() {
        let mut p = IncrementalParser::new();
        let mut fed = String::new();
        for chunk in STORY.as_bytes().chunks(7) {
            fed.push_str(std::str::from_utf8(chunk).unwrap());
            p.set_text(&fed);
        }
        let full = parse_full(STORY);
        assert_eq!(p.tree().blocks.len(), full.blocks.len());
        for (a, b) in p.tree().blocks.iter().zip(full.blocks.iter()) {
            assert_eq!(a.range, b.range);
        }
    }
}

#[cfg(test)]
mod destination_rewrite_tests {
    use super::*;

    fn paragraph(source: &str) -> Vec<InlineRun> {
        let tree = parse_full(source);
        let Some(TopBlock {
            block: Block::Paragraph { runs },
            ..
        }) = tree.blocks.first().map(|top| top.as_ref())
        else {
            panic!("expected a paragraph for {source:?}");
        };
        runs.clone()
    }

    fn link_of(runs: &[InlineRun]) -> Option<&str> {
        runs.iter().find_map(|run| run.style.link.as_deref())
    }

    fn text(runs: &[InlineRun]) -> String {
        runs.iter().map(|run| run.text.as_str()).collect()
    }

    #[test]
    fn absolute_destinations_with_spaces_become_links() {
        let runs = paragraph("see [it's here](/tmp/2026/Some Folder/it's here.txt) now");
        assert_eq!(text(&runs), "see it's here now");
        assert_eq!(link_of(&runs), Some("/tmp/2026/Some Folder/it's here.txt"));

        let runs = paragraph("[x](file:///tmp/a b/c.md)");
        assert_eq!(link_of(&runs), Some("file:///tmp/a b/c.md"));

        let runs = paragraph("shot ![two](/tmp/pics/two words.png) done");
        let image = runs.iter().find_map(|run| run.style.image.as_ref());
        assert_eq!(
            image.map(|image| image.source.as_str()),
            Some("/tmp/pics/two words.png")
        );
    }

    #[test]
    fn non_absolute_or_unsafe_destinations_keep_their_text() {
        for source in [
            // Relative destinations with spaces stay plain text.
            "[x](Some Folder/a.md)",
            // The file-name dot rule applies to plain absolute paths.
            "[x](/usr/bin/some thing)",
            // `file://localhost` is an ordinary URL, not a host path.
            "[x](file://localhost/tmp/a b.md)",
            "[x](/tmp/a b/c/)",
            "[x](/tmp/a?b c.md)",
            "[x](/tmp/../a b.md)",
        ] {
            let runs = paragraph(source);
            assert_eq!(link_of(&runs), None, "{source}");
            assert!(text(&runs).contains('('), "{source} should stay literal");
        }
    }

    #[test]
    fn already_bracketed_destinations_are_left_alone() {
        let runs = paragraph("see [it](/tmp/Some Folder/it's here.txt) now");
        assert_eq!(text(&runs), "see it now");
        assert_eq!(link_of(&runs), Some("/tmp/Some Folder/it's here.txt"));
    }

    #[test]
    fn code_keeps_its_spelling() {
        let tree = parse_full("```\n[x](/tmp/a b.md)\n```\n");
        let Block::CodeBlock { code, .. } = &tree.blocks[0].block else {
            panic!("fenced code");
        };
        assert_eq!(code.trim_end(), "[x](/tmp/a b.md)");

        let tree = parse_full("    [x](/tmp/a b.md)\n");
        let Block::CodeBlock { code, .. } = &tree.blocks[0].block else {
            panic!("indented code");
        };
        assert_eq!(code.trim_end(), "[x](/tmp/a b.md)");

        let runs = paragraph("`[x](/tmp/a b.md)`");
        assert!(runs.iter().all(|run| run.style.link.is_none()));
        assert_eq!(text(&runs), "[x](/tmp/a b.md)");
    }

    #[test]
    fn unclosed_destinations_wait_for_their_closing_paren() {
        let mut parser = IncrementalParser::new();
        parser.append("see [x](/tmp/a b");
        assert!(link_of(&paragraph(parser.source())).is_none());
        parser.append(".md) now");
        assert_eq!(link_of(&paragraph(parser.source())), Some("/tmp/a b.md"));
        assert_eq!(parser.tree(), &parse_full(parser.source()));
    }

    #[test]
    fn rewritten_ranges_map_back_to_the_original_source() {
        let source = "intro [x](/tmp/a b.md) tail\n\nsecond paragraph\n";
        let tree = parse_full(source);
        assert_eq!(tree.blocks.len(), 2);
        assert_eq!(
            &source[tree.blocks[0].range.clone()],
            "intro [x](/tmp/a b.md) tail\n"
        );
        assert_eq!(&source[tree.blocks[1].range.clone()], "second paragraph\n");
    }

    #[test]
    fn rewrite_is_idempotent_on_its_own_output() {
        let source = "see [it](/tmp/Some Folder/it's here.txt) and [web](https://x.dev/a b)";
        let once = rewrite_absolute_destinations(source).expect("rewrite");
        assert!(once.0.contains("](</tmp/Some Folder/it's here.txt>)"));
        assert!(once.0.contains("https://x.dev/a b"));
        assert!(
            rewrite_absolute_destinations(&once.0).is_none(),
            "second pass must find nothing to rewrite"
        );
    }
}

#[cfg(test)]
mod math_tests {
    use super::*;

    fn blocks(source: &str) -> Vec<Block> {
        parse_full(source)
            .blocks
            .iter()
            .map(|top| top.block.clone())
            .collect()
    }

    fn para(source: &str) -> Vec<InlineRun> {
        match blocks(source).as_slice() {
            [Block::Paragraph { runs }] => runs.clone(),
            other => panic!("expected one paragraph for {source:?}: {other:?}"),
        }
    }

    fn math(text: &str) -> InlineRun {
        InlineRun {
            text: text.into(),
            style: InlineStyle {
                math: true,
                ..InlineStyle::default()
            },
        }
    }

    fn plain(text: &str) -> InlineRun {
        InlineRun {
            text: text.into(),
            style: InlineStyle::default(),
        }
    }

    fn display(tex: &str) -> Block {
        Block::Math { tex: tex.into() }
    }

    fn paragraph(runs: Vec<InlineRun>) -> Block {
        Block::Paragraph { runs }
    }

    #[test]
    fn inline_math_is_an_atomic_run() {
        assert_eq!(para("a $x^2$ b"), [plain("a "), math("x^2"), plain(" b")]);
        // Neighbouring formulas never merge; their TeX is not autolinked
        // and its markers are not emphasis.
        assert_eq!(para("$a$$b$"), [math("a"), math("b")]);
        assert_eq!(
            para("$\\text{https://x.dev}$"),
            [math("\\text{https://x.dev}")]
        );
        assert_eq!(para("$a_1 * b_2$"), [math("a_1 * b_2")]);
        // Inline math keeps the surrounding style.
        let runs = para("**bold $x$**");
        assert!(runs[1].style.math && runs[1].style.bold);
    }

    #[test]
    fn display_math_alone_is_a_block() {
        assert_eq!(blocks("$$x$$"), [display("x")]);
        assert_eq!(blocks("$$\n\\frac{a}{b}\n$$\n"), [display("\\frac{a}{b}")]);
    }

    #[test]
    fn display_math_mid_paragraph_splits_it() {
        assert_eq!(
            blocks("before\n$$x$$\nafter **b**"),
            [
                paragraph(vec![plain("before")]),
                display("x"),
                paragraph(vec![
                    plain("after "),
                    InlineRun {
                        text: "b".into(),
                        style: InlineStyle {
                            bold: true,
                            ..InlineStyle::default()
                        }
                    }
                ]),
            ]
        );
        assert_eq!(
            blocks("lead $$x$$ $$y$$ tail"),
            [
                paragraph(vec![plain("lead")]),
                display("x"),
                display("y"),
                paragraph(vec![plain("tail")]),
            ]
        );
        assert_eq!(
            blocks("$$x$$ trailing"),
            [display("x"), paragraph(vec![plain("trailing")])]
        );
        // Every piece shares the source paragraph's range.
        let source = "intro\n\nbefore $$x$$ after\n";
        let tree = parse_full(source);
        assert_eq!(tree.len(), 4);
        for top in &tree.blocks[1..] {
            assert_eq!(&source[top.range.clone()], "before $$x$$ after\n");
        }
    }

    #[test]
    fn display_math_splits_inside_lists_and_quotes() {
        let Block::List { items, .. } = &blocks("- see $$x$$ ok\n- $$y$$\n")[0] else {
            panic!("list");
        };
        assert_eq!(
            items[0],
            [
                paragraph(vec![plain("see")]),
                display("x"),
                paragraph(vec![plain("ok")])
            ]
        );
        assert_eq!(items[1], [display("y")]);
        let Block::BlockQuote { children } = &blocks("> q\n> $$z$$\n")[0] else {
            panic!("quote");
        };
        assert_eq!(children, &[paragraph(vec![plain("q")]), display("z")]);
    }

    #[test]
    fn display_math_stays_inline_in_headings_tables_and_spans() {
        let Block::Heading { runs, .. } = &blocks("# Sum $$x$$")[0] else {
            panic!("heading");
        };
        assert_eq!(runs, &[plain("Sum "), math("x")]);
        let Block::Table { rows, .. } = &blocks("| m |\n|---|\n| $$c$$ |\n")[0] else {
            panic!("table");
        };
        assert_eq!(rows[0][0], [math("c")]);
        let runs = para("*a $$x$$ b*");
        assert!(runs.iter().any(|run| run.style.math && run.style.italic));
    }

    #[test]
    fn currency_stays_text() {
        assert_eq!(para("costs $5-$10 total"), [plain("costs $5-$10 total")]);
        assert_eq!(para("between $5 and $10"), [plain("between $5 and $10")]);
        assert_eq!(para("$x$5"), [plain("$x$5")]);
        // A formula followed by a non-digit is still math.
        assert_eq!(para("$5$ apples"), [math("5"), plain(" apples")]);
    }

    #[test]
    fn shell_variables_stay_text() {
        assert_eq!(para("Set $HOME/$USER now"), [plain("Set $HOME/$USER now")]);
        assert_eq!(para("echo $FOO$BAR"), [plain("echo $FOO$BAR")]);
        // Capital letters alone are still geometry, and suffixes still attach.
        assert_eq!(
            para("triangle $ABC$ here"),
            [plain("triangle "), math("ABC"), plain(" here")]
        );
        assert_eq!(
            para("the $n$th one"),
            [plain("the "), math("n"), plain("th one")]
        );
        // Joined with `:` or `@`, as in shell lines.
        assert_eq!(
            para("PATH=$PATH:$HOME/bin"),
            [plain("PATH=$PATH:$HOME/bin")]
        );
        assert_eq!(para("'$USER@$HOST'"), [plain("'$USER@$HOST'")]);
        assert_eq!(
            para("so $x$ holds"),
            [plain("so "), math("x"), plain(" holds")]
        );
    }

    /// Dollars that aren't math parse exactly as they did without math
    /// support: a rejected span's markup still applies.
    #[test]
    fn rejected_dollar_spans_keep_their_markdown() {
        let without_math = |source: &str| parse_with(source, 0, options() - Options::ENABLE_MATH);
        for source in [
            "**$5** to **$10**",
            "from $5 to [$10](u)",
            "costs $5 (`$4` with code)",
            "costs $5-$10 total, between $5 and $10",
            "$5 *a* $6 *b* $7 *c* $8",
            "pay $5, get _$10_ back and ~~$2~~",
            "PATH=$PATH:$HOME/bin and `$HOME`",
            "'$USER@$HOST' **now**",
            "> quoted $5 **b** $6\n",
            "- $1 [x](u) $2\n- $3 *i* $4\n",
            "| a | b |\n|---|---|\n| $5 **x** | $6 |\n",
            "# $5 *off* $9\n",
        ] {
            assert_eq!(parse_full(source), without_math(source), "{source:?}");
        }
        // Mixed with real math, each span is judged on its own.
        let runs = para("**$5** and $x$ then **$10**");
        assert!(runs.iter().any(|run| run.style.bold && run.text == "$5"));
        assert!(runs.contains(&math("x")));
        assert!(runs.iter().any(|run| run.style.bold && run.text == "$10"));
        // Ranges still point at the original source.
        let source = "**$5** to **$10**\n\nnext\n";
        let tree = parse_full(source);
        assert_eq!(&source[tree.blocks[0].range.clone()], "**$5** to **$10**\n");
        assert_eq!(&source[tree.blocks[1].range.clone()], "next\n");
    }

    #[test]
    fn latex_delimiters_become_math() {
        assert_eq!(
            para("so \\(a^2 + b^2\\) holds"),
            [plain("so "), math("a^2 + b^2"), plain(" holds")]
        );
        // Whitespace inside `\( … \)` is dropped (`$ x $` would not parse).
        assert_eq!(para("\\( x \\)"), [math("x")]);
        // A rewritten closer before a digit is not a price.
        assert_eq!(para("\\(x\\)2"), [math("x"), plain("2")]);
        assert_eq!(blocks("\\[ e = mc^2 \\]"), [display("e = mc^2")]);
        assert_eq!(
            blocks("Thus\n\\[\n\\int_0^1 f\\,dx \\\\\ny\n\\]\nso"),
            [
                paragraph(vec![plain("Thus")]),
                display("\\int_0^1 f\\,dx \\\\\ny"),
                paragraph(vec![plain("so")]),
            ]
        );
    }

    #[test]
    fn unclosed_latex_openers_scan_in_linear_time() {
        // 20k openers with no closer in their paragraph: each would rescan it.
        // Likewise a long line of backtick runs, each asking for its line's
        // context.
        let mut source = "\\(".repeat(20_000);
        source.push_str(&"`a` ``b ".repeat(10_000));
        source.push_str(" tail\n\nand \\(y\\) after");
        let started = std::time::Instant::now();
        let (out, _) = rewrite_math_delimiters(&source).unwrap();
        assert!(
            started.elapsed().as_millis() < 500,
            "{:?}",
            started.elapsed()
        );
        // The openers stay literal; the next paragraph's pair still closes.
        assert!(out.starts_with("\\(\\(\\("));
        assert!(
            out.ends_with(" tail\n\nand $y$ after"),
            "{}",
            &out[out.len() - 30..]
        );
    }

    #[test]
    fn latex_delimiters_need_a_closer_in_the_same_paragraph() {
        // Unclosed (still streaming): markdown escapes, so `(` / `[` show.
        assert_eq!(para("so \\(a^2"), [plain("so (a^2")]);
        assert_eq!(
            blocks("\\[\nx\n\nmore \\]"),
            [
                paragraph(vec![plain("[ x")]),
                paragraph(vec![plain("more ]")])
            ]
        );
        // A closer past the start of another block doesn't pair either — not
        // into a fence's body, not across a heading or list item.
        let tree = blocks("a \\[\n```\n\\]\n```\n");
        assert_eq!(tree[0], paragraph(vec![plain("a [")]));
        assert_eq!(
            tree[1],
            Block::CodeBlock {
                language: None,
                code: "\\]".into()
            }
        );
        assert_eq!(blocks("# h \\(\nx \\)")[1], paragraph(vec![plain("x )")]));
        assert_eq!(blocks("a \\(\n- x \\)").len(), 2);
        // Quoted paragraphs continue over their `>` lines.
        let Block::BlockQuote { children } = &blocks("> \\[\n> x\n> \\]\n")[0] else {
            panic!("quote");
        };
        assert_eq!(children, &[display("x")]);
    }

    #[test]
    fn latex_delimiters_leave_escapes_and_code_alone() {
        // `\\(` is a literal backslash then `(`.
        assert_eq!(para("a \\\\(b\\\\) c"), [plain("a \\(b\\) c")]);
        // Escaped brackets glued to a word, and escaped task boxes.
        assert_eq!(para("arr\\[0\\] and f\\(x\\)"), [plain("arr[0] and f(x)")]);
        assert_eq!(para("\\[ \\] todo"), [plain("[ ] todo")]);
        // Code spans, fences and indented code keep their spelling.
        let runs = para("`\\(x\\)` and \\(y\\)");
        assert_eq!(runs[0].text, "\\(x\\)");
        assert!(runs[0].style.code);
        assert_eq!(runs[2], math("y"));
        let Block::CodeBlock { code, .. } = &blocks("```\n\\[x\\]\n```\n")[0] else {
            panic!("fence");
        };
        assert_eq!(code, "\\[x\\]");
        let Block::CodeBlock { code, .. } = &blocks("    \\(x\\)\n")[0] else {
            panic!("indented");
        };
        assert_eq!(code, "\\(x\\)");
    }

    /// Random prose where every `$` is a price (and no backslash can form a
    /// LaTeX delimiter): whatever spans pulldown pairs, the result must match
    /// a parse without math.
    #[test]
    fn random_prices_parse_as_without_math() {
        const PIECES: &[&str] = &[
            "$5", "$10", "$1.50", " ", " ", "a", "b c", "**", "*", "_", "~~", "`", "``", "[",
            "](u)", "]", "(", ")", "\\*", "\n", "\n\n", "\n- ", "\n> ", "|", "é", "-", ":",
        ];
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..3000 {
            let len = 4 + (next() % 40) as usize;
            let source: String = (0..len)
                .map(|_| PIECES[(next() % PIECES.len() as u64) as usize])
                .collect();
            assert_eq!(
                parse_full(&source),
                parse_with(&source, 0, options() - Options::ENABLE_MATH),
                "{source:?}"
            );
        }
    }

    #[test]
    fn latex_delimiters_never_rewrite_code_blocks() {
        // A stray backtick run can't pair past its paragraph into a fence.
        let tree = blocks("Wrap it in ```latex like this:\n\n```latex\n\\(x\\) and \\[y\\]\n```\n");
        assert_eq!(
            tree[1],
            Block::CodeBlock {
                language: Some("latex".into()),
                code: "\\(x\\) and \\[y\\]".into()
            }
        );
        // Fences in a quote or a list item keep their spelling too.
        let Block::BlockQuote { children } = &blocks("> ```\n> \\(x\\)\n> ```\n")[0] else {
            panic!("quote");
        };
        assert_eq!(
            children[0],
            Block::CodeBlock {
                language: None,
                code: "\\(x\\)".into()
            }
        );
        let Block::List { items, .. } = &blocks("- ```\n  \\[y\\]\n  ```\n")[0] else {
            panic!("list");
        };
        assert_eq!(
            items[0][0],
            Block::CodeBlock {
                language: None,
                code: "\\[y\\]".into()
            }
        );
    }

    #[test]
    fn latex_delimiters_skip_link_destinations() {
        let runs = para("[w](https://en.wikipedia.org/wiki/Foo_\\(bar\\))");
        assert_eq!(
            runs[0].style.link.as_deref(),
            Some("https://en.wikipedia.org/wiki/Foo_(bar)")
        );
        assert_eq!(runs[0].text, "w");
        // Escapes glued to URL and identifier characters stay escapes.
        assert_eq!(para("x_\\(1\\) and a/\\(b\\)"), [plain("x_(1) and a/(b)")]);
        // Autolinks and reference definitions keep their spelling.
        let runs = para("<https://x.dev/\\(a\\)> \\(y\\)");
        assert_eq!(runs[0].style.link.as_deref(), Some("https://x.dev/\\(a\\)"));
        assert_eq!(runs.last(), Some(&math("y")));
        // An opener before the link doesn't pair with a paren in its URL.
        let runs = para("\\(x [w](https://x.dev/Foo_\\(bar\\))");
        assert_eq!(runs[0], plain("(x "));
        assert_eq!(
            runs[1].style.link.as_deref(),
            Some("https://x.dev/Foo_(bar)")
        );
        let runs = para("[w]\n\n[w]: https://x.dev/\\(a\\)\n");
        assert_eq!(runs[0].style.link.as_deref(), Some("https://x.dev/(a)"));
    }

    #[test]
    fn latex_delimiters_stop_at_setext_underlines_and_tables() {
        // An underline ends the paragraph: no formula spans it.
        let tree = blocks("a \\(x\n---\ny\\)\n");
        assert_eq!(
            tree[0],
            Block::Heading {
                level: 2,
                runs: vec![plain("a (x")]
            }
        );
        assert_eq!(tree[1], paragraph(vec![plain("y)")]));
        let tree = blocks("a \\(x\n===\ny\\)\n");
        assert!(matches!(tree[0], Block::Heading { level: 1, .. }));
        // A table's header row starts a new block.
        let tree = blocks("p \\(x\n| a |\n|:-:|\n| y\\) |\n");
        assert_eq!(tree[0], paragraph(vec![plain("p (x")]));
        assert!(matches!(tree[1], Block::Table { .. }));
    }

    /// Minimized streaming divergences (some from review, the rest from the
    /// randomized parity test): stray backticks, setext underlines, escaped
    /// newlines, fences in quotes and list items, HTML blocks.
    #[test]
    fn streamed_regressions_match_full_parse() {
        for source in [
            "a ` b\n\nc\n\n\\(x\\) `y`\n",
            "a \\(x\n---\ny\\)\n\nz\n\nw\n",
            "a \\(x\n|---|\ny\\)\n\nz\n\nw\n",
            "Wrap it in ```latex like this:\n\n```latex\n\\(x\\) and \\[y\\]\n```\n",
            "\\(\\\n\n=\\)\n\n](",
            "```\\`\n>\n\\[y\\]",
            "```\\`\n\n\\[*\\]\n\n漢|",
            ">```\\[\\\n\\]\n\n)*",
            ">```\n\\[>\\]```\n> ",
            "\\(`\n    \n    \\)\n-]",
            "1. <p>\\('\n`\\)\n- ``",
            "1. # \\[漢\n\\]\n> ",
            "><p>\\(\n    x\n\\)",
            "<p\n```\n\n>\n\\(|\\)",
            "- \\[\n    <p\n\\]\n\né\n",
        ] {
            let full = parse_full(source);
            for chunk in 1..=17 {
                let mut parser = IncrementalParser::new();
                let mut start = 0;
                while start < source.len() {
                    let mut end = (start + chunk).min(source.len());
                    while !source.is_char_boundary(end) {
                        end += 1;
                    }
                    parser.append(&source[start..end]);
                    start = end;
                }
                assert_eq!(parser.tree(), &full, "chunk {chunk}: {source:?}");
            }
        }
    }

    #[test]
    fn rewritten_ranges_map_back_to_the_original_source() {
        let source = "a \\( x \\) [l](/tmp/a b.md)\n\n\\[y\\] tail\n";
        let tree = parse_full(source);
        assert_eq!(
            &source[tree.blocks[0].range.clone()],
            "a \\( x \\) [l](/tmp/a b.md)\n"
        );
        assert_eq!(&source[tree.blocks[1].range.clone()], "\\[y\\] tail\n");
        let Block::Paragraph { runs } = &tree.blocks[0].block else {
            panic!("paragraph");
        };
        assert_eq!(runs[1], math("x"));
        assert_eq!(
            runs.iter().find_map(|run| run.style.link.as_deref()),
            Some("/tmp/a b.md")
        );
        // Composed offsets: the event ranges themselves land on the
        // original delimiters.
        let (text, map) = rewrite_math_delimiters(source).unwrap();
        assert_eq!(&text[..6], "a $x$ ");
        assert_eq!(map.original_range(2..5), 2..9);
        assert_eq!(&source[2..9], "\\( x \\)");
    }

    #[test]
    fn streamed_math_matches_full_parse() {
        let mut parser = IncrementalParser::new();
        parser.set_text("text $$x");
        assert_eq!(parser.display_tree(), *parser.tree());
        parser.append("$$ more **b");
        assert_eq!(parser.tree(), &parse_full(parser.source()));
        // The display tail replaces the whole split paragraph exactly once.
        let shown = parser.display_tree();
        assert_eq!(shown.len(), 3);
        assert_eq!(shown.blocks[1].block, display("x"));
        let Block::Paragraph { runs } = &shown.blocks[2].block else {
            panic!("paragraph");
        };
        assert!(runs.iter().any(|run| run.style.bold && run.text == "b"));
    }
}

#[cfg(test)]
mod image_model_tests {
    use super::*;
    #[test]
    fn images_preserve_alt_title_position_links_and_empty_alt() {
        let tree =
            parse_full("Before [![**alt**](a.png \"Title\")](next.md) after ![](b.png) ![](b.png)");
        let Block::Paragraph { runs } = &tree.blocks[0].block else {
            panic!("paragraph");
        };
        let images: Vec<_> = runs.iter().filter_map(|r| r.style.image.as_ref()).collect();
        assert_eq!(images.len(), 3);
        assert_eq!(images[0].alt, "alt");
        assert_eq!(images[0].title, "Title");
        assert_eq!(images[0].link.as_deref(), Some("next.md"));
        assert_eq!(images[1].source, "b.png");
        assert!(images[1].alt.is_empty());
        assert_eq!(runs.first().unwrap().text, "Before ");
    }
}
