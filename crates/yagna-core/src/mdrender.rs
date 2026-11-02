//! The README renderer: markdown in, blocks out — the pure half of the
//! repository tab's reading surface.
//!
//! The renderer never fetches, never reads the disk and never sees a
//! URL. Images arrive as the `yagna-asset://<id>` destinations the
//! offline rewrite produced, and the renderer asks an [`AssetResolver`]
//! what each id is: a cached file, still pending, or missing. That is
//! the whole contract — the reader cannot become a browser.
//!
//! Raw HTML never renders: a tag is stripped to the text around it (an
//! `<img>` tag with an asset destination becomes an image, an autolink
//! becomes a link, everything else disappears). The parser runs under
//! size limits — the input is capped, the block count is capped, the
//! block and inline nesting depths are capped — so a hostile README
//! cannot cost more than a bounded amount of work.
//!
//! Items: `Resolved` — what an asset id is (file, pending, missing,
//! badge); `AssetResolver` — the question the renderer asks;
//! `Inline`/`Block`/`ListItem` — the rendered shapes; `render` — the
//! entry point; `Preview`/`preview` — the row preview's blocks and
//! badge count; `count_badges` — how many of the images are badges;
//! `visible_text` — the blocks as the text a reader sees.
//! Depends on: crate::markdown (the asset scheme, the tag scanner, the
//! parenthesis matcher).
//! Called by: the app's README view.

use std::path::PathBuf;

use crate::markdown::{
    ASSET_SCHEME, find_ascii_ci, html_attribute_span, html_tag_end, matching_paren,
};

/// The input cap: the store already caps a README at 256 KB, and the
/// renderer caps again so a caller cannot hand it more.
pub const MAX_INPUT_BYTES: usize = 256 * 1024;

/// The block cap: a README with thousands of blocks is truncated, not
/// rendered forever.
pub const MAX_BLOCKS: usize = 4096;

/// The nesting cap: a quote inside a quote inside a quote stops there.
pub const MAX_DEPTH: usize = 4;

/// The inline nesting cap: a link inside a link inside emphasis stops
/// there — deep bracket nesting degrades to text instead of recursing
/// one stack frame per bracket.
pub const MAX_INLINE_DEPTH: usize = 12;

/// What an asset id is, from the reader's point of view.
///
/// - `File` — the bytes are cached; this is the path they sit under.
/// - `Pending` — the asset is queued; the reader shows a placeholder.
/// - `Missing` — the asset is not there (a 404, or over the byte cap);
///   the reader shows a quiet marker.
/// - `Badge` — the asset is a CI/status badge; the reader counts it
///   instead of showing the picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    File(PathBuf),
    Pending,
    Missing,
    /// The asset is a badge image: a CI/status badge. The reader shows
    /// a count of them, never the picture.
    Badge,
}

/// The question the renderer asks about an asset id. The renderer never
/// implements it — the tab's resolver is backed by the asset cache.
pub trait AssetResolver {
    /// What the asset id is.
    fn resolve(&self, asset_id: &str) -> Resolved;
}

/// A resolver that knows nothing: every asset is pending. The default
/// for a caller that has no cache to ask.
pub struct BlindResolver;

impl AssetResolver for BlindResolver {
    fn resolve(&self, _asset_id: &str) -> Resolved {
        Resolved::Pending
    }
}

/// One piece of text inside a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    /// Plain text.
    Text(String),
    /// A code span.
    Code(String),
    /// Emphasised text.
    Emphasis(Vec<Inline>),
    /// Strongly emphasised text.
    Strong(Vec<Inline>),
    /// A link: its visible text (which may hold an image — a badge
    /// link is a link around an image) and its destination.
    Link { text: Vec<Inline>, url: String },
    /// An image: its alt text and what the asset id resolved to.
    Image { alt: String, asset: Resolved },
}

/// One block of the rendered README.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// A heading, its level 1 through 6 — ATX (`#`) and setext
    /// (a `=`/`-` underline) both land here.
    Heading { level: u8, inlines: Vec<Inline> },
    /// A paragraph.
    Paragraph(Vec<Inline>),
    /// A list: ordered or not, one item per entry.
    List { ordered: bool, items: Vec<ListItem> },
    /// A fenced code block, with its language when the fence named one.
    Code {
        language: Option<String>,
        code: String,
    },
    /// A pipe table: its header row and its body rows. The cells carry
    /// their inlines, so a link inside a cell is a link.
    Table {
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    /// A block quote: the blocks inside it.
    Quote(Vec<Block>),
    /// A horizontal rule.
    Rule,
}

/// One list item: its own inline runs, its GFM task marker when it
/// carries one (`None` for a plain item), and the blocks indented under
/// it — a nested list, most commonly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
    pub inlines: Vec<Inline>,
    pub task: Option<bool>,
    pub children: Vec<Block>,
}

/// Renders a README into blocks. The input is the *rewritten* body —
/// image destinations are asset ids — and the resolver answers for each
/// of them. The renderer is deterministic: the same body and the same
/// resolver give the same blocks.
pub fn render(markdown: &str, resolver: &dyn AssetResolver) -> Vec<Block> {
    render_at(markdown, resolver, 0)
}

/// The renderer with its depth counter: quotes nest, and the nesting
/// stops at [`MAX_DEPTH`].
fn render_at(markdown: &str, resolver: &dyn AssetResolver, depth: usize) -> Vec<Block> {
    if depth >= MAX_DEPTH {
        return Vec::new();
    }
    let text = cap_input(markdown);
    let mut reader = Reader::new(text);
    let mut blocks = Vec::new();
    while blocks.len() < MAX_BLOCKS {
        let Some(line) = reader.peek() else {
            break;
        };
        if line.trim().is_empty() {
            reader.bump();
            continue;
        }
        blocks.push(read_block(&mut reader, resolver, depth));
    }
    blocks
}

/// The input cap: the text is cut on a character boundary.
fn cap_input(markdown: &str) -> &str {
    if markdown.len() <= MAX_INPUT_BYTES {
        return markdown;
    }
    let mut cut = MAX_INPUT_BYTES;
    while cut > 0 && !markdown.is_char_boundary(cut) {
        cut -= 1;
    }
    &markdown[..cut]
}

/// The line reader: the cursor over the README's lines.
struct Reader<'a> {
    lines: Vec<&'a str>,
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            lines: text.lines().collect(),
            at: 0,
        }
    }

    /// The line the cursor is on.
    fn peek(&self) -> Option<&'a str> {
        self.lines.get(self.at).copied()
    }

    /// The line `ahead` lines past the cursor.
    fn peek_ahead(&self, ahead: usize) -> Option<&'a str> {
        self.lines.get(self.at + ahead).copied()
    }

    /// Takes the line the cursor is on.
    fn bump(&mut self) -> Option<&'a str> {
        let line = self.peek()?;
        self.at += 1;
        Some(line)
    }
}

/// One block, read from the cursor. Every branch consumes at least one
/// line, so the reader always advances.
fn read_block(reader: &mut Reader<'_>, resolver: &dyn AssetResolver, depth: usize) -> Block {
    let line = reader.peek().unwrap_or_default().to_string();
    let trimmed = line.trim().to_string();
    if is_fence(&trimmed) {
        return read_code(reader);
    }
    if let Some(level) = heading_level(&trimmed) {
        reader.bump();
        let text = trimmed[(level as usize) + 1..].to_string();
        return Block::Heading {
            level,
            inlines: inlines(&text, resolver, 0),
        };
    }
    if is_rule(&trimmed) {
        reader.bump();
        return Block::Rule;
    }
    if trimmed.starts_with('>') {
        return read_quote(reader, resolver, depth);
    }
    if is_table_start(reader) {
        return read_table(reader, resolver);
    }
    if list_marker(&trimmed).is_some() {
        return read_list(reader, resolver, depth);
    }
    read_paragraph(reader, resolver)
}

/// The heading level of a line, when it is an ATX heading.
fn heading_level(trimmed: &str) -> Option<u8> {
    let hashes = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed[hashes..];
    if !rest.starts_with(' ') {
        return None;
    }
    Some(hashes as u8)
}

/// Whether a line opens or closes a fenced code block, and with which
/// marker.
fn is_fence(trimmed: &str) -> bool {
    fence_marker(trimmed).is_some()
}

/// The fence marker of a line: three or more backticks or tildes.
fn fence_marker(trimmed: &str) -> Option<String> {
    let marker = trimmed.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }
    let run = trimmed
        .chars()
        .take_while(|character| *character == marker)
        .count();
    (run >= 3).then(|| marker.to_string().repeat(run))
}

/// Whether a line is a horizontal rule: three or more of one marker on
/// their own.
fn is_rule(trimmed: &str) -> bool {
    let marker = match trimmed.chars().next() {
        Some(marker @ ('-' | '*' | '_')) => marker,
        _ => return false,
    };
    let rest = &trimmed[1..];
    rest.chars().all(|character| character == marker) && rest.len() >= 2
}

/// The list marker of a line: a bullet or a number, and how long the
/// marker is (the item's text starts after it).
struct ListMarker {
    ordered: bool,
    len: usize,
}

/// The list marker of a trimmed line, when it opens a list item.
fn list_marker(trimmed: &str) -> Option<ListMarker> {
    let bullet =
        trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ");
    if bullet {
        return Some(ListMarker {
            ordered: false,
            len: 2,
        });
    }
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 9 {
        return None;
    }
    let rest = &trimmed[digits..];
    if !rest.starts_with(". ") {
        return None;
    }
    Some(ListMarker {
        ordered: true,
        len: digits + 2,
    })
}

/// The code block: the fence's language and the lines up to the closing
/// fence.
fn read_code(reader: &mut Reader<'_>) -> Block {
    let opening = reader.bump().unwrap_or_default().trim().to_string();
    let fence = fence_marker(&opening).unwrap_or_else(|| "```".to_string());
    let language = opening.trim_start_matches(&fence).trim().to_string();
    let mut code = String::new();
    while let Some(line) = reader.peek() {
        let text = line.to_string();
        reader.bump();
        if text.trim().starts_with(&fence) {
            break;
        }
        code.push_str(&text);
        code.push('\n');
    }
    Block::Code {
        language: (!language.is_empty()).then_some(language),
        code,
    }
}

/// The quote: the lines behind the `>` markers, rendered as blocks.
fn read_quote(reader: &mut Reader<'_>, resolver: &dyn AssetResolver, depth: usize) -> Block {
    let mut inner = String::new();
    while let Some(line) = reader.peek() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('>') {
            break;
        }
        inner.push_str(trimmed[1..].trim_start());
        inner.push('\n');
        reader.bump();
    }
    Block::Quote(render_at(&inner, resolver, depth + 1))
}

/// Whether the cursor is on a table's first row: a row with a pipe, and
/// a separator row behind it.
fn is_table_start(reader: &Reader<'_>) -> bool {
    let Some(line) = reader.peek() else {
        return false;
    };
    if !line.contains('|') {
        return false;
    }
    let Some(next) = reader.lines.get(reader.at + 1) else {
        return false;
    };
    is_separator_row(next)
}

/// Whether a line is a table's separator row: pipes and dashes only.
fn is_separator_row(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.contains('-') || !trimmed.contains('|') {
        return false;
    }
    trimmed
        .chars()
        .all(|character| matches!(character, '|' | '-' | ':' | ' '))
}

/// The table: the header row, the separator row, and the body rows.
fn read_table(reader: &mut Reader<'_>, resolver: &dyn AssetResolver) -> Block {
    let header = split_row(reader.bump().unwrap_or_default(), resolver);
    reader.bump();
    let mut rows = Vec::new();
    while let Some(line) = reader.peek() {
        if !line.contains('|') {
            break;
        }
        rows.push(split_row(reader.bump().unwrap_or_default(), resolver));
    }
    Block::Table { header, rows }
}

/// One table row's cells: the pipes split it, the outer pipes are
/// dropped, and each cell's whitespace is trimmed. A cell is a run of
/// inlines, so a link inside a cell is a link.
fn split_row(line: &str, resolver: &dyn AssetResolver) -> Vec<Vec<Inline>> {
    let trimmed = line.trim().trim_start_matches('|').trim_end_matches('|');
    trimmed
        .split('|')
        .map(|cell| inlines(cell.trim(), resolver, 0))
        .collect()
}

/// The list: the items at one indent level. An item keeps its
/// continuation lines (indented, marker-free) folded into its text, and
/// the deeper lines render as its child blocks — that is how a nested
/// list stays nested.
fn read_list(reader: &mut Reader<'_>, resolver: &dyn AssetResolver, depth: usize) -> Block {
    let first_line = reader.peek().unwrap_or_default();
    let base = indent_of(first_line);
    let Some(first) = list_marker(first_line.trim()) else {
        return Block::List {
            ordered: false,
            items: Vec::new(),
        };
    };
    let content_column = base + first.len;
    let mut items = Vec::new();
    while starts_next_item(reader, base, first.ordered) {
        if reader.peek().is_some_and(|line| line.trim().is_empty()) {
            reader.bump();
        }
        items.push(read_item(reader, resolver, depth, base, content_column));
    }
    Block::List {
        ordered: first.ordered,
        items,
    }
}

/// Whether the cursor opens the list's next item: a marker of the same
/// kind at the list's own indent. A blank line between items counts
/// when the line after it is one of ours (the loose-list case).
fn starts_next_item(reader: &Reader<'_>, base: usize, ordered: bool) -> bool {
    let Some(line) = reader.peek() else {
        return false;
    };
    if line.trim().is_empty() {
        let Some(next) = reader.peek_ahead(1) else {
            return false;
        };
        return list_matches(next, base, ordered);
    }
    list_matches(line, base, ordered)
}

/// Whether one concrete line is an item of this list.
fn list_matches(line: &str, base: usize, ordered: bool) -> bool {
    if indent_of(line) != base {
        return false;
    }
    let Some(marker) = list_marker(line.trim()) else {
        return false;
    };
    marker.ordered == ordered
}

/// One list item: the first line's text behind its marker (and its task
/// marker when it carries one), then the lines behind it — continuation
/// text folds into the item, deeper lines collect as child blocks.
fn read_item(
    reader: &mut Reader<'_>,
    resolver: &dyn AssetResolver,
    depth: usize,
    base: usize,
    content_column: usize,
) -> ListItem {
    let first = reader.bump().unwrap_or_default();
    let marker_len = marker_len_of(first.trim());
    let head = first[(base + marker_len).min(first.len())..]
        .trim()
        .to_string();
    let (task, text) = task_marker(&head);
    let mut text = text.to_string();
    let mut child_lines: Vec<String> = Vec::new();
    while let Some(line) = reader.peek() {
        if line.trim().is_empty() && item_continues(reader, content_column) {
            reader.bump();
            continue;
        }
        if indent_of(line) < content_column || line.trim().is_empty() {
            break;
        }
        reader.bump();
        absorb_item_line(line, content_column, &mut text, &mut child_lines);
    }
    let children = render_children(&child_lines, resolver, depth);
    ListItem {
        inlines: inlines(&text, resolver, 0),
        task,
        children,
    }
}

/// The length of the list marker at the head of a line, when there is
/// one; zero otherwise.
fn marker_len_of(trimmed: &str) -> usize {
    list_marker(trimmed).map_or(0, |marker| marker.len)
}

/// Whether a blank line is inside the item: the next line stays
/// indented to the item's content.
fn item_continues(reader: &Reader<'_>, content_column: usize) -> bool {
    let Some(next) = reader.peek_ahead(1) else {
        return false;
    };
    !next.trim().is_empty() && indent_of(next) >= content_column
}

/// One indented line behind an item's first: a marker opens (or
/// continues) the child block lines; any other line is continuation
/// text while no child block has started, child text after.
fn absorb_item_line(
    line: &str,
    content_column: usize,
    text: &mut String,
    child_lines: &mut Vec<String>,
) {
    let dedented = dedent(line, content_column);
    if !child_lines.is_empty() || list_marker(line.trim()).is_some() {
        child_lines.push(dedented);
        return;
    }
    let joined = dedented.trim();
    if !joined.is_empty() {
        text.push(' ');
        text.push_str(joined);
    }
}

/// A line with up to `column` columns of leading whitespace removed.
fn dedent(line: &str, column: usize) -> String {
    let indent = indent_of(line);
    line[indent.min(column)..].to_string()
}

/// The item's child blocks: the collected lines re-rendered one level
/// down; the depth cap empties them, so nesting cannot run away.
fn render_children(
    child_lines: &[String],
    resolver: &dyn AssetResolver,
    depth: usize,
) -> Vec<Block> {
    if child_lines.is_empty() {
        return Vec::new();
    }
    render_at(&child_lines.join("\n"), resolver, depth + 1)
}

/// The GFM task marker at the head of an item's text, when there is
/// one: `[ ] ` or `[x] `, and whether it is checked.
fn task_marker(text: &str) -> (Option<bool>, &str) {
    if let Some(rest) = text.strip_prefix("[ ] ") {
        return (Some(false), rest);
    }
    if let Some(rest) = text.strip_prefix("[x] ") {
        return (Some(true), rest);
    }
    if let Some(rest) = text.strip_prefix("[X] ") {
        return (Some(true), rest);
    }
    (None, text)
}

/// A line's leading-whitespace width in bytes (spaces and tabs each
/// count as one column).
fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The paragraph: the lines up to the next block, joined with spaces. A
/// `=`/`-` underline behind accumulated text is a setext heading — the
/// paragraph becomes the heading's text instead.
fn read_paragraph(reader: &mut Reader<'_>, resolver: &dyn AssetResolver) -> Block {
    let mut text = String::new();
    while let Some(line) = reader.peek() {
        let trimmed = line.trim();
        if !text.is_empty()
            && let Some(level) = setext_level(trimmed)
        {
            reader.bump();
            return Block::Heading {
                level,
                inlines: inlines(&text, resolver, 0),
            };
        }
        if trimmed.is_empty() || starts_block(line) {
            break;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(trimmed);
        reader.bump();
    }
    Block::Paragraph(inlines(&text, resolver, 0))
}

/// The heading level of a setext underline: a run of `=` (level 1) or
/// `-` (level 2) and nothing else.
fn setext_level(trimmed: &str) -> Option<u8> {
    let marker = trimmed.chars().next()?;
    if marker != '=' && marker != '-' {
        return None;
    }
    let all_of_one = !trimmed.is_empty() && trimmed.chars().all(|character| character == marker);
    if !all_of_one {
        return None;
    }
    Some(if marker == '=' { 1 } else { 2 })
}

/// Whether a line opens another kind of block — a paragraph stops there.
fn starts_block(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }
    if is_fence(trimmed) || is_rule(trimmed) {
        return true;
    }
    if heading_level(trimmed).is_some() {
        return true;
    }
    if trimmed.starts_with('>') {
        return true;
    }
    list_marker(trimmed).is_some()
}

/// One parsed inline run: the inline (when there is one) and how many
/// bytes of the source it took.
struct InlineStep {
    inline: Option<Inline>,
    len: usize,
}

/// The inline run of one line of text. The depth caps the recursion —
/// a link inside a link inside emphasis is rare, and bracket nesting a
/// thousand deep is hostile, not prose.
fn inlines(text: &str, resolver: &dyn AssetResolver, depth: usize) -> Vec<Inline> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < text.len() {
        let rest = &text[at..];
        let Some(step) = inline_at(rest, resolver, depth) else {
            let len = failed_len(rest);
            out.push(Inline::Text(rest[..len].to_string()));
            at += len;
            continue;
        };
        if let Some(inline) = step.inline {
            out.push(inline);
        }
        at += step.len.max(1);
    }
    out
}

/// The length a construct that failed to parse occupies as text: a
/// bracket-shaped one is taken **whole** — degrading it one character
/// at a time would re-run the matcher over the same nesting for every
/// position, and a hostile bracket wall would cost cubic time (and
/// worse through the capped recursion) where this costs one pass.
/// Anything else degrades as plain text runs.
fn failed_len(rest: &str) -> usize {
    if rest.starts_with('[') {
        return matching_bracket(rest).map_or(1, |close| close + 1);
    }
    if let Some(bracket) = rest.strip_prefix("![") {
        return matching_bracket(bracket).map_or(2, |close| close + 3);
    }
    plain_run(rest)
}

/// The inline construct at the start of the text, when there is one.
/// Past the inline depth cap there is none: the construct degrades to
/// plain text instead of another recursion level.
fn inline_at(rest: &str, resolver: &dyn AssetResolver, depth: usize) -> Option<InlineStep> {
    if depth >= MAX_INLINE_DEPTH {
        return None;
    }
    if rest.starts_with("![") {
        return image_at(rest, resolver);
    }
    if rest.starts_with('<') {
        return html_at(rest, resolver);
    }
    if rest.starts_with('[') {
        return link_at(rest, resolver, depth);
    }
    if rest.starts_with('`') {
        return code_at(rest);
    }
    if rest.starts_with("**") {
        return strong_at(rest, resolver, depth);
    }
    if rest.starts_with('*') {
        return emphasis_at(rest, resolver, depth);
    }
    None
}

/// The image: `![alt](destination)`. An asset destination is resolved
/// through the resolver; any other destination is missing — the reader
/// never fetches a URL.
fn image_at(rest: &str, resolver: &dyn AssetResolver) -> Option<InlineStep> {
    let close = rest.find("](")?;
    let alt = rest[2..close].to_string();
    let after = &rest[close + 1..];
    let end = matching_paren(after)?;
    let dest = after[1..end].trim().to_string();
    let asset = resolve_asset(&dest, resolver);
    Some(InlineStep {
        inline: Some(Inline::Image { alt, asset }),
        len: close + 1 + end + 1,
    })
}

/// The link: `[text](url)`. An asset destination is not a link — it is
/// the text alone.
fn link_at(rest: &str, resolver: &dyn AssetResolver, depth: usize) -> Option<InlineStep> {
    let close = matching_bracket(rest)?;
    let text = inlines(&rest[1..close], resolver, depth + 1);
    let after = &rest[close + 1..];
    if !after.starts_with('(') {
        return None;
    }
    let end = matching_paren(after)?;
    let url = after[1..end].trim().to_string();
    let len = close + 2 + end + 1;
    if url.starts_with(ASSET_SCHEME) {
        return Some(InlineStep {
            inline: Some(Inline::Strong(text)),
            len,
        });
    }
    Some(InlineStep {
        inline: Some(Inline::Link { text, url }),
        len,
    })
}

/// The index of the bracket that closes the one at the head of the
/// text: a link's text may hold an image, and the image's own brackets
/// have to balance.
fn matching_bracket(text: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (index, character) in text.char_indices() {
        match character {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// The code span: the text between two runs of the same number of
/// backticks (`` ``a ` b`` `` is one span containing a backtick). One
/// leading and one trailing space are stripped when both are present —
/// the CommonMark rule that lets a span start or end with a space.
fn code_at(rest: &str) -> Option<InlineStep> {
    let ticks = rest
        .chars()
        .take_while(|character| *character == '`')
        .count();
    let close = find_tick_run(rest, ticks)?;
    let mut code = rest[ticks..close].to_string();
    if code.starts_with(' ') && code.ends_with(' ') && code.len() >= 2 {
        code = code[1..code.len() - 1].to_string();
    }
    Some(InlineStep {
        inline: Some(Inline::Code(code)),
        len: close + ticks,
    })
}

/// Where the closing run of exactly `ticks` backticks starts: a longer
/// run does not close the span, so the search steps past one.
fn find_tick_run(rest: &str, ticks: usize) -> Option<usize> {
    let needle = "`".repeat(ticks);
    let mut search = ticks;
    while let Some(found) = rest[search..].find(&needle) {
        let at = search + found;
        let longer_run = rest[at + ticks..].starts_with('`');
        if !longer_run {
            return Some(at);
        }
        search = at + ticks;
    }
    None
}

/// The strong run: the text between the `**` markers.
fn strong_at(rest: &str, resolver: &dyn AssetResolver, depth: usize) -> Option<InlineStep> {
    let end = rest[2..].find("**")? + 2;
    Some(InlineStep {
        inline: Some(Inline::Strong(inlines(&rest[2..end], resolver, depth + 1))),
        len: end + 2,
    })
}

/// The emphasised run: the text between the `*` markers.
fn emphasis_at(rest: &str, resolver: &dyn AssetResolver, depth: usize) -> Option<InlineStep> {
    let end = rest[1..].find('*')? + 1;
    Some(InlineStep {
        inline: Some(Inline::Emphasis(inlines(
            &rest[1..end],
            resolver,
            depth + 1,
        ))),
        len: end + 1,
    })
}

/// The raw HTML: an `<img>` tag with an asset destination becomes an
/// image; an autolink (`<https://…>`) becomes a link; every other tag
/// is stripped, and the text around it stays.
fn html_at(rest: &str, resolver: &dyn AssetResolver) -> Option<InlineStep> {
    if rest.starts_with("<!--") {
        let end = rest.find("-->").map(|at| at + 3)?;
        return Some(InlineStep {
            inline: None,
            len: end,
        });
    }
    let end = html_tag_end(rest, 0)?;
    if let Some(url) = autolink_of(&rest[..end]) {
        return Some(InlineStep {
            inline: Some(Inline::Link {
                text: vec![Inline::Text(url.clone())],
                url,
            }),
            len: end,
        });
    }
    let tag = &rest[..end];
    let lower = tag.to_ascii_lowercase();
    if lower.starts_with("<script") || lower.starts_with("<style") {
        // The body of a script or a style block is code, not prose: it
        // is dropped whole, not rendered as text.
        let closing = if lower.starts_with("<script") {
            "</script"
        } else {
            "</style"
        };
        let after = find_ascii_ci(rest, closing)? + closing.len();
        let end = rest[after..].find('>').map(|at| after + at + 1)?;
        return Some(InlineStep {
            inline: None,
            len: end,
        });
    }
    if !lower.starts_with("<img") {
        return Some(InlineStep {
            inline: None,
            len: end,
        });
    }
    let body = tag.trim_start_matches('<').trim_end_matches('>');
    let Some((start, stop)) = html_attribute_span(body, "src") else {
        return Some(InlineStep {
            inline: None,
            len: end,
        });
    };
    let dest = body[start..stop].to_string();
    let asset = resolve_asset(&dest, resolver);
    Some(InlineStep {
        inline: Some(Inline::Image {
            alt: String::new(),
            asset,
        }),
        len: end,
    })
}

/// The address of an autolink tag — `<scheme:rest>` with no whitespace
/// — when the scheme is one a reader may open. Any other tag-shaped
/// text is not an autolink.
fn autolink_of(tag: &str) -> Option<String> {
    let body = tag.strip_prefix('<')?.strip_suffix('>')?;
    if body.is_empty() || body.chars().any(|c| c.is_whitespace() || c == '<') {
        return None;
    }
    let (scheme, _) = body.split_once(':')?;
    let openable = matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "mailto"
    );
    openable.then(|| body.to_string())
}

/// What an asset destination resolves to. A destination that is not an
/// asset id is missing: the reader never fetches a URL.
fn resolve_asset(dest: &str, resolver: &dyn AssetResolver) -> Resolved {
    let Some(id) = dest.strip_prefix(ASSET_SCHEME) else {
        return Resolved::Missing;
    };
    resolver.resolve(id)
}

/// The length of the plain text run at the head of the text: up to the
/// next special construct. A special construct that did not parse is
/// taken as one character of text.
fn plain_run(text: &str) -> usize {
    let mut end = 0usize;
    for (index, character) in text.char_indices() {
        if starts_construct(text, index, character) {
            break;
        }
        end = index + character.len_utf8();
    }
    end.max(1)
}

/// Whether the character at the index opens a construct.
fn starts_construct(text: &str, index: usize, character: char) -> bool {
    match character {
        '[' | '`' | '<' | '*' => true,
        '!' => text[index + 1..].starts_with('['),
        _ => false,
    }
}

/// How many of the images across the blocks are badges. A README's
/// badge wall is noise rather than content, so the reader counts the
/// badges instead of showing their pictures.
pub fn count_badges(blocks: &[Block]) -> usize {
    blocks.iter().map(count_badges_in_block).sum()
}

/// One block's badges: its inlines, its list items, its table cells or
/// the blocks inside its quote.
fn count_badges_in_block(block: &Block) -> usize {
    match block {
        Block::Heading { inlines, .. } | Block::Paragraph(inlines) => {
            count_badges_in_inlines(inlines)
        }
        Block::List { items, .. } => items.iter().map(count_badges_in_item).sum(),
        Block::Table { header, rows } => count_badges_in_table(header, rows),
        Block::Quote(inner) => count_badges(inner),
        Block::Code { .. } | Block::Rule => 0,
    }
}

/// One list item's badges: its own inlines and the blocks under it.
fn count_badges_in_item(item: &ListItem) -> usize {
    count_badges_in_inlines(&item.inlines) + count_badges(&item.children)
}

/// One table's badges: its header cells and its body rows.
fn count_badges_in_table(header: &[Vec<Inline>], rows: &[Vec<Vec<Inline>>]) -> usize {
    let head: usize = header
        .iter()
        .map(|cell| count_badges_in_inlines(cell))
        .sum();
    let body: usize = rows.iter().map(|row| count_badges_in_row(row)).sum();
    head + body
}

/// One table row's badges: the badges in its cells.
fn count_badges_in_row(row: &[Vec<Inline>]) -> usize {
    row.iter().map(|cell| count_badges_in_inlines(cell)).sum()
}

/// One run of inlines' badges: the images that resolved to a badge, and
/// the ones inside a link, an emphasis or a strong run.
fn count_badges_in_inlines(inlines: &[Inline]) -> usize {
    inlines.iter().map(count_badges_in_inline).sum()
}

/// One inline's badges: its own image's badge, or the badges it holds.
fn count_badges_in_inline(inline: &Inline) -> usize {
    match inline {
        Inline::Image { asset, .. } => usize::from(matches!(asset, Resolved::Badge)),
        Inline::Emphasis(inner) | Inline::Strong(inner) => count_badges_in_inlines(inner),
        Inline::Link { text, .. } => count_badges_in_inlines(text),
        Inline::Text(_) | Inline::Code(_) => 0,
    }
}

/// The preview of a README: the blocks an expanded row paints, and the
/// badges that were collapsed into a count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    /// The blocks the reader sees — the badge images are out of them.
    pub blocks: Vec<Block>,
    /// How many badge images the count line announces.
    pub badges: usize,
}

/// Renders a README for an expanded row: the badge images are
/// collapsed into a count. A wall of CI badges says nothing the reader
/// can act on, and a row has no room for a wall of pictures — the
/// count says what the wall held.
pub fn preview(markdown: &str, resolver: &dyn AssetResolver) -> Preview {
    let blocks = render(markdown, resolver);
    let badges = count_badges(&blocks);
    let blocks = collapse_badges(blocks);
    Preview { blocks, badges }
}

/// The blocks without their badge images: a badge is counted, never
/// drawn. A block that held nothing but badges is gone — there is
/// nothing left to paint.
fn collapse_badges(blocks: Vec<Block>) -> Vec<Block> {
    blocks.into_iter().filter_map(collapse_block).collect()
}

/// One block without its badge images.
fn collapse_block(block: Block) -> Option<Block> {
    match block {
        Block::Heading { level, inlines } => {
            let inlines = collapse_inlines(inlines);
            (!inlines.is_empty()).then_some(Block::Heading { level, inlines })
        }
        Block::Paragraph(inlines) => {
            let inlines = collapse_inlines(inlines);
            (!inlines.is_empty()).then_some(Block::Paragraph(inlines))
        }
        Block::List { ordered, items } => Some(Block::List {
            ordered,
            items: items.into_iter().filter_map(collapse_item).collect(),
        }),
        Block::Code { language, code } => Some(Block::Code { language, code }),
        Block::Table { header, rows } => Some(Block::Table {
            header: header.into_iter().map(collapse_inlines).collect(),
            rows: rows
                .into_iter()
                .map(|row| row.into_iter().map(collapse_inlines).collect())
                .collect(),
        }),
        Block::Quote(inner) => Some(Block::Quote(collapse_badges(inner))),
        Block::Rule => Some(Block::Rule),
    }
}

/// One list item without its badge images: an item that held nothing
/// but badges is gone.
fn collapse_item(item: ListItem) -> Option<ListItem> {
    let inlines = collapse_inlines(item.inlines);
    if inlines.is_empty() && item.children.is_empty() {
        return None;
    }
    Some(ListItem {
        inlines,
        task: item.task,
        children: item.children,
    })
}

/// One run of inlines without its badge images.
fn collapse_inlines(inlines: Vec<Inline>) -> Vec<Inline> {
    inlines.into_iter().filter_map(collapse_inline).collect()
}

/// One inline without its badge images: a badge goes, and a link or a
/// run that held nothing else goes with it.
fn collapse_inline(inline: Inline) -> Option<Inline> {
    match inline {
        Inline::Image { alt, asset } => match asset {
            Resolved::Badge => None,
            asset => Some(Inline::Image { alt, asset }),
        },
        Inline::Text(text) => Some(Inline::Text(text)),
        Inline::Code(code) => Some(Inline::Code(code)),
        Inline::Emphasis(inner) => {
            let inner = collapse_inlines(inner);
            (!inner.is_empty()).then_some(Inline::Emphasis(inner))
        }
        Inline::Strong(inner) => {
            let inner = collapse_inlines(inner);
            (!inner.is_empty()).then_some(Inline::Strong(inner))
        }
        Inline::Link { text, url } => {
            let text = collapse_inlines(text);
            (!text.is_empty()).then_some(Inline::Link { text, url })
        }
    }
}

/// The blocks as the text a reader sees: the words, the code, a link's
/// own text, an image's alt — never a URL, never a marker. The preview
/// is read, not looked at: this is what it says.
pub fn visible_text(blocks: &[Block]) -> String {
    blocks.iter().map(block_text).collect::<Vec<_>>().join("\n")
}

/// One block's text.
fn block_text(block: &Block) -> String {
    match block {
        Block::Heading { inlines, .. } | Block::Paragraph(inlines) => inline_text(inlines),
        Block::List { items, .. } => items
            .iter()
            .map(|item| inline_text(&item.inlines))
            .collect::<Vec<_>>()
            .join("\n"),
        Block::Code { code, .. } => code.clone(),
        Block::Table { header, rows } => table_text(header, rows),
        Block::Quote(inner) => visible_text(inner),
        Block::Rule => String::new(),
    }
}

/// One table's text: the header row, then the body rows.
fn table_text(header: &[Vec<Inline>], rows: &[Vec<Vec<Inline>>]) -> String {
    let head = header
        .iter()
        .map(|cell| inline_text(cell))
        .collect::<Vec<_>>()
        .join(" ");
    let body = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|cell| inline_text(cell))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{head}\n{body}")
}

/// One run of inlines' text.
fn inline_text(inlines: &[Inline]) -> String {
    inlines.iter().map(inline_text_of).collect()
}

/// One inline's text: its own words, the words it holds, a link's own
/// text, an image's alt.
fn inline_text_of(inline: &Inline) -> String {
    match inline {
        Inline::Text(text) | Inline::Code(text) => text.clone(),
        Inline::Emphasis(inner) | Inline::Strong(inner) => inline_text(inner),
        Inline::Link { text, .. } => inline_text(text),
        Inline::Image { alt, .. } => alt.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A resolver with a fixed answer per asset id.
    struct Fixed {
        answers: HashMap<String, Resolved>,
    }

    impl Fixed {
        fn new() -> Self {
            Self {
                answers: HashMap::new(),
            }
        }

        fn with(mut self, id: &str, answer: Resolved) -> Self {
            self.answers.insert(id.to_string(), answer);
            self
        }
    }

    impl AssetResolver for Fixed {
        fn resolve(&self, asset_id: &str) -> Resolved {
            self.answers
                .get(asset_id)
                .cloned()
                .unwrap_or(Resolved::Missing)
        }
    }

    #[test]
    fn headings_paragraphs_and_lists_render() {
        let md = "# Title\n\nA paragraph.\n\n- one\n- two\n\n1. first\n2. second\n";
        let blocks = render(md, &BlindResolver);
        assert_eq!(blocks.len(), 4);
        assert!(matches!(blocks[0], Block::Heading { level: 1, .. }));
        assert_eq!(
            inline_text(match &blocks[1] {
                Block::Paragraph(inlines) => inlines,
                _ => &[],
            }),
            "A paragraph."
        );
        let Block::List { ordered, items } = &blocks[2] else {
            panic!("a bullet list");
        };
        assert!(!ordered);
        assert_eq!(items.len(), 2);
        assert_eq!(inline_text(&items[0].inlines), "one");
        let Block::List { ordered, items } = &blocks[3] else {
            panic!("an ordered list");
        };
        assert!(ordered);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn code_blocks_keep_their_language_and_their_lines() {
        let md = "```rust\nlet x = 1;\nlet y = 2;\n```\n";
        let blocks = render(md, &BlindResolver);
        let Block::Code { language, code } = &blocks[0] else {
            panic!("a code block");
        };
        assert_eq!(language.as_deref(), Some("rust"));
        assert_eq!(code, "let x = 1;\nlet y = 2;\n");
    }

    #[test]
    fn tables_render_their_header_and_their_rows() {
        let md = "| name | value |\n| --- | ---: |\n| one | 1 |\n| two | 2 |\n";
        let blocks = render(md, &BlindResolver);
        let Block::Table { header, rows } = &blocks[0] else {
            panic!("a table");
        };
        assert_eq!(header.len(), 2);
        assert_eq!(inline_text(&header[0]), "name");
        assert_eq!(inline_text(&header[1]), "value");
        assert_eq!(rows.len(), 2);
        assert_eq!(inline_text(&rows[1][0]), "two");
        assert_eq!(inline_text(&rows[1][1]), "2");
    }

    #[test]
    fn a_badge_link_is_a_link_around_an_image() {
        let md = "[![Build Status](https://example.org/badge.svg)](https://example.org/ci)
";
        let blocks = render(md, &BlindResolver);
        let Block::Paragraph(inlines) = &blocks[0] else {
            panic!("a paragraph");
        };
        let Some(Inline::Link { text, url }) = inlines.first() else {
            panic!("a link: {inlines:?}");
        };
        assert_eq!(url, "https://example.org/ci", "the link's own address");
        let Some(Inline::Image { alt, .. }) = text.first() else {
            panic!("the badge is an image: {text:?}");
        };
        assert_eq!(alt, "Build Status");
    }

    #[test]
    fn a_table_cell_carries_its_link() {
        let md = "| name | link |
| --- | --- |
| thing | [docs](https://example.org/docs) |
";
        let blocks = render(md, &BlindResolver);
        let Block::Table { rows, .. } = &blocks[0] else {
            panic!("a table");
        };
        let cell = &rows[0][1];
        let Some(Inline::Link { text, url }) = cell.first() else {
            panic!("a link in the cell: {cell:?}");
        };
        assert_eq!(inline_text(text), "docs");
        assert_eq!(url, "https://example.org/docs");
    }

    #[test]
    fn a_script_body_never_renders() {
        let md = "Before

<script>console.log('nope');</script>

After
";
        let blocks = render(md, &BlindResolver);
        let text: String = blocks
            .iter()
            .map(|block| match block {
                Block::Paragraph(inlines) => inline_text(inlines),
                _ => String::new(),
            })
            .collect();
        assert!(!text.contains("console"), "the script is gone: {text}");
        assert!(text.contains("Before") && text.contains("After"));
    }

    #[test]
    fn raw_html_is_stripped_to_text() {
        let md = "Before <b>bold</b> after.\n";
        let blocks = render(md, &BlindResolver);
        let Block::Paragraph(inlines) = &blocks[0] else {
            panic!("a paragraph");
        };
        assert_eq!(inline_text(inlines), "Before bold after.");
        assert!(
            !format!("{inlines:?}").contains("<b>"),
            "no raw tag survives: {inlines:?}"
        );
    }

    #[test]
    fn images_resolve_through_the_resolver_in_all_three_states() {
        let md = "![shot](yagna-asset://abc)\n\n![wait](yagna-asset://def)\n\n![gone](yagna-asset://ghi)\n";
        let resolver = Fixed::new()
            .with("abc", Resolved::File(PathBuf::from("/cache/abc.img")))
            .with("def", Resolved::Pending);
        let blocks = render(md, &resolver);
        let assets: Vec<Resolved> = blocks
            .iter()
            .filter_map(|block| match block {
                Block::Paragraph(inlines) => inlines.iter().find_map(|inline| match inline {
                    Inline::Image { asset, .. } => Some(asset.clone()),
                    _ => None,
                }),
                _ => None,
            })
            .collect();
        assert_eq!(
            assets,
            [
                Resolved::File(PathBuf::from("/cache/abc.img")),
                Resolved::Pending,
                Resolved::Missing,
            ]
        );
    }

    #[test]
    fn an_image_destination_that_is_not_an_asset_is_missing() {
        let blocks = render("![shot](https://example.org/a.png)\n", &BlindResolver);
        let Block::Paragraph(inlines) = &blocks[0] else {
            panic!("a paragraph");
        };
        let Some(Inline::Image { asset, .. }) = inlines.first() else {
            panic!("an image");
        };
        assert_eq!(*asset, Resolved::Missing);
    }

    #[test]
    fn an_html_image_tag_becomes_an_image() {
        let md = "<img align=\"center\" src=\"yagna-asset://abc\">\n";
        let resolver = Fixed::new().with("abc", Resolved::Pending);
        let blocks = render(md, &resolver);
        let Block::Paragraph(inlines) = &blocks[0] else {
            panic!("a paragraph");
        };
        let Some(Inline::Image { asset, .. }) = inlines.first() else {
            panic!("an image: {inlines:?}");
        };
        assert_eq!(*asset, Resolved::Pending);
    }

    #[test]
    fn links_keep_their_text_and_their_url() {
        let blocks = render(
            "See [the docs](https://example.org/docs).\n",
            &BlindResolver,
        );
        let Block::Paragraph(inlines) = &blocks[0] else {
            panic!("a paragraph");
        };
        let Some(Inline::Link { text, url }) = inlines
            .iter()
            .find(|inline| matches!(inline, Inline::Link { .. }))
        else {
            panic!("a link: {inlines:?}");
        };
        assert_eq!(inline_text(text), "the docs");
        assert_eq!(url, "https://example.org/docs");
    }

    #[test]
    fn quotes_nest_and_the_nesting_stops() {
        let md = "> quoted\n> lines\n";
        let blocks = render(md, &BlindResolver);
        let Block::Quote(inner) = &blocks[0] else {
            panic!("a quote");
        };
        assert_eq!(inner.len(), 1);
        assert_eq!(
            inline_text(match &inner[0] {
                Block::Paragraph(inlines) => inlines,
                _ => &[],
            }),
            "quoted lines"
        );
    }

    #[test]
    fn emphasis_and_strong_run() {
        let blocks = render("**bold** and *soft* and `code`\n", &BlindResolver);
        let Block::Paragraph(inlines) = &blocks[0] else {
            panic!("a paragraph");
        };
        assert!(matches!(inlines.first(), Some(Inline::Strong(_))));
        assert!(
            inlines
                .iter()
                .any(|inline| matches!(inline, Inline::Emphasis(_)))
        );
        assert!(
            inlines
                .iter()
                .any(|inline| matches!(inline, Inline::Code(_)))
        );
    }

    #[test]
    fn the_input_is_capped_and_the_block_count_is_bounded() {
        let huge = "x".repeat(MAX_INPUT_BYTES + 100);
        let blocks = render(&huge, &BlindResolver);
        assert_eq!(blocks.len(), 1, "the cap cuts the text, not the parser");

        let many = "# h\n".repeat(MAX_BLOCKS + 100);
        let blocks = render(&many, &BlindResolver);
        assert_eq!(blocks.len(), MAX_BLOCKS);
    }

    #[test]
    fn an_empty_readme_renders_nothing() {
        assert!(render("", &BlindResolver).is_empty());
        assert!(render("\n\n   \n", &BlindResolver).is_empty());
    }

    #[test]
    fn setext_headings_render_as_headings() {
        let md = "Title\n=====\n\nSubject\n---\n";
        let blocks = render(md, &BlindResolver);
        assert!(
            matches!(&blocks[0], Block::Heading { level: 1, inlines } if inline_text(inlines) == "Title"),
            "the = underline is an H1: {:?}",
            blocks[0]
        );
        assert!(
            matches!(&blocks[1], Block::Heading { level: 2, inlines } if inline_text(inlines) == "Subject"),
            "the - underline is an H2: {:?}",
            blocks[1]
        );
    }

    #[test]
    fn a_rule_stays_a_rule_and_a_lone_underline_is_not_a_heading() {
        let md = "Paragraph.\n\n---\n";
        let blocks = render(md, &BlindResolver);
        assert!(matches!(blocks[0], Block::Paragraph(_)));
        assert!(matches!(blocks[1], Block::Rule));
    }

    #[test]
    fn autolinks_render_as_links_not_vanish() {
        let md = "Homepage: <https://example.org> and <mailto:ops@example.org> end.\n";
        let blocks = render(md, &BlindResolver);
        let Block::Paragraph(inlines) = &blocks[0] else {
            panic!("a paragraph");
        };
        let text = inline_text(inlines);
        assert!(
            text.contains("https://example.org") && text.contains("ops@example.org"),
            "the addresses stay visible: {text}"
        );
        let links: Vec<&String> = inlines
            .iter()
            .filter_map(|inline| match inline {
                Inline::Link { url, .. } => Some(url),
                _ => None,
            })
            .collect();
        assert_eq!(links.len(), 2, "both autolinks are links: {inlines:?}");
    }

    #[test]
    fn double_backtick_code_spans_keep_their_inner_backtick() {
        let md = "Use ``a ` b`` here.\n";
        let blocks = render(md, &BlindResolver);
        let Block::Paragraph(inlines) = &blocks[0] else {
            panic!("a paragraph");
        };
        let codes: Vec<&String> = inlines
            .iter()
            .filter_map(|inline| match inline {
                Inline::Code(code) => Some(code),
                _ => None,
            })
            .collect();
        assert_eq!(codes, [&"a ` b".to_string()], "one code span: {inlines:?}");
    }

    #[test]
    fn nested_lists_stay_nested_and_continuation_lines_fold_in() {
        let md = "- parent one\n  continued text\n  - child a\n  - child b\n- parent two\n";
        let blocks = render(md, &BlindResolver);
        let Block::List { items, .. } = &blocks[0] else {
            panic!("a list: {blocks:?}");
        };
        assert_eq!(items.len(), 2, "two parents: {items:?}");
        assert_eq!(
            inline_text(&items[0].inlines),
            "parent one continued text",
            "the indented line folds into the item"
        );
        let child_list = items[0]
            .children
            .iter()
            .find_map(|block| match block {
                Block::List { items, .. } => Some(items),
                _ => None,
            })
            .expect("a nested list");
        assert_eq!(child_list.len(), 2);
        assert_eq!(inline_text(&child_list[0].inlines), "child a");
        assert!(
            items[1].children.is_empty(),
            "the second parent has no children"
        );
    }

    #[test]
    fn task_list_items_carry_their_marker() {
        let md = "- [ ] todo\n- [x] done\n- plain\n";
        let blocks = render(md, &BlindResolver);
        let Block::List { items, .. } = &blocks[0] else {
            panic!("a list");
        };
        assert_eq!(items[0].task, Some(false));
        assert_eq!(items[1].task, Some(true));
        assert_eq!(items[2].task, None);
        assert_eq!(inline_text(&items[0].inlines), "todo");
    }

    #[test]
    fn deep_bracket_nesting_degrades_to_text_instead_of_recursing() {
        let depth = 5_000;
        let mut hostile = String::new();
        for _ in 0..depth {
            hostile.push('[');
        }
        for _ in 0..depth {
            hostile.push(']');
        }
        let blocks = render(&hostile, &BlindResolver);
        let Block::Paragraph(inlines) = &blocks[0] else {
            panic!("a paragraph");
        };
        assert!(
            !inlines.is_empty(),
            "the hostile run degrades to plain text, not a stack overflow"
        );
    }

    #[test]
    fn a_quoted_gt_inside_an_img_tag_does_not_cut_the_tag() {
        let md = "<img src=\"a>b\">\n";
        let blocks = render(md, &BlindResolver);
        let Block::Paragraph(inlines) = &blocks[0] else {
            panic!("a paragraph");
        };
        assert!(
            !format!("{inlines:?}").contains("b\">"),
            "the tag ends at its real close, not the quoted gt: {inlines:?}"
        );
    }

    /// A resolver that answers one asset id as a badge and the rest as
    /// ordinary cached files.
    fn badge_resolver() -> Fixed {
        Fixed::new()
            .with("badge", Resolved::Badge)
            .with("shot", Resolved::File(PathBuf::from("/cache/shot.img")))
    }

    #[test]
    fn only_the_badge_images_are_counted() {
        let md = "![build](yagna-asset://badge)\n\n![shot](yagna-asset://shot)\n\n![gone](yagna-asset://nope)\n";
        let blocks = render(md, &badge_resolver());
        assert_eq!(count_badges(&blocks), 1, "one badge among the images");
    }

    #[test]
    fn a_badge_inside_a_list_item_counts() {
        let md = "- parent\n  - ![build](yagna-asset://badge)\n";
        let blocks = render(md, &badge_resolver());
        assert_eq!(count_badges(&blocks), 1, "the nested item's badge");
    }

    #[test]
    fn a_badge_inside_a_table_cell_counts() {
        let md = "| name | badge |\n| --- | --- |\n| thing | ![build](yagna-asset://badge) |\n";
        let blocks = render(md, &badge_resolver());
        assert_eq!(count_badges(&blocks), 1, "the cell's badge");
    }

    #[test]
    fn a_badge_inside_a_quote_and_a_link_counts() {
        let md = "> ![build](yagna-asset://badge)\n\n[![build](yagna-asset://badge)](https://example.org/ci)\n";
        let blocks = render(md, &badge_resolver());
        assert_eq!(count_badges(&blocks), 2, "the quote's and the link's");
    }

    #[test]
    fn a_readme_without_badges_counts_none() {
        let md = "# Title\n\nA paragraph with [a link](https://example.org).\n\n```rust\nlet x = 1;\n```\n";
        let blocks = render(md, &badge_resolver());
        assert_eq!(count_badges(&blocks), 0);
    }

    #[test]
    fn the_preview_counts_the_badges_and_drops_them() {
        let md = "[![build](yagna-asset://badge)](https://example.org/ci)\n\n![shot](yagna-asset://shot)\n";
        let preview = preview(md, &badge_resolver());
        assert_eq!(preview.badges, 1, "the badge is counted");
        assert!(
            !format!("{:?}", preview.blocks).contains("Badge"),
            "no badge is drawn: {:?}",
            preview.blocks
        );
        assert!(
            visible_text(&preview.blocks).contains("shot"),
            "the other image is still there: {}",
            visible_text(&preview.blocks)
        );
    }

    #[test]
    fn the_visible_text_holds_no_link_syntax() {
        let md = "See [the docs](https://example.org/docs) and `code`.\n";
        let blocks = render(md, &BlindResolver);
        let text = visible_text(&blocks);
        assert!(!text.contains("]("), "no raw link syntax: {text}");
        assert!(
            text.contains("the docs"),
            "the link's text is what shows: {text}"
        );
    }
}
