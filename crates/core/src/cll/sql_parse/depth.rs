// ---------------------------------------------------------------------------
// Pre-parse depth bound — flat chains must not overflow the walks
// ---------------------------------------------------------------------------
//
// `sqlparser`'s own recursion budget only watches NESTED constructs
// (default 50 deep): the flat chains it parses iteratively —
// `a and b and …`, `x || y || …`, `select … union all select …` — build
// left-deep trees whose spine is unbounded. Every depth-sensitive pass
// that follows (scope resolution, the relation scan, expression
// rendering, the AST's own drop) recurses along that spine, and on the
// smallest stacks the binary runs on — ~1 MiB on Windows mains and
// wasm32, ~2 MiB on test threads — a few thousand terms overflow.
//
// Rather than rewriting every walker, the scan below estimates the
// depth a text could parse to BEFORE the AST exists. Per parenthesis
// level it counts operator units — punctuation runs and operator
// keywords — in each "segment" (a run cut by a clause keyword, `,`,
// `;`, or a set operator), plus the set operators themselves; `(`
// opens a fresh level that `)` folds back. The peak
// `level + segment + set` upper-bounds real tree depth: every
// depth-adding construct (an infix/prefix/postfix op, a nested paren,
// a set-op arm) contributes at least one counted unit while operands
// contribute none — so over-limit text is handed back exactly like a
// parse error and lands on the name-match / `unparsed` path.
//
// Counts are approximations by design and only err toward depth: a
// `between`'s `and`, a `cast`'s `as`, or a call's `f(` paren may count
// a unit the tree doesn't spend, but nothing that ADDS depth escapes
// the count.

use sqlparser::ast::Statement;
use sqlparser::parser::Parser;

use super::dialect::SqlDialect;
use super::jinja::{
    block_comment_end, dollar_quote_close, dollar_quote_open, line_comment_end, q_quote_end,
    quoted_end, word_byte, Lex,
};

/// The deepest a text may parse and still be traversed — sized for the
/// smallest stack the shipped binary meets. Measured on this codebase:
/// a release build overflows a 1 MiB stack near ~1.9k expression terms
/// (~600 B a resolver frame), so 512 keeps roughly 3× headroom in
/// release while real models spend a few dozen at most. Debug frames
/// are far larger, so debug builds on a 1 MiB stack still overflow
/// lower — no bound that keeps normal SQL parsing can fully cover
/// that, and the estimate's over-count is already margin.
const MAX_DEPTH: usize = 512;

/// Keywords that open a fresh segment — clause and statement
/// boundaries, `CASE` delimiters, alias/constraint markers. Each one is
/// a hard keyword that can never appear inside an expression, so a
/// false entry only ever splits a segment (mildly under-counting what
/// follows); a word that CAN sit inside one — `if`, `left`, `set`,
/// `key`, `partition`, `first`… — is deliberately absent.
const SEGMENT_END: &[&[u8]] = &[
    b"action",
    b"alter",
    b"always",
    b"analyze",
    b"apply",
    b"as",
    b"attach",
    b"auto_increment",
    b"begin",
    b"by",
    b"cache",
    b"call",
    b"cascade",
    b"checkpoint",
    b"close",
    b"cluster",
    b"comment",
    b"commit",
    b"committed",
    b"concurrently",
    b"connect",
    b"constraint",
    b"copy",
    b"create",
    b"cycle",
    b"database",
    b"deallocate",
    b"declare",
    b"default",
    b"deferrable",
    b"deferred",
    b"delete",
    b"describe",
    b"detach",
    b"discard",
    b"distribute",
    b"do",
    b"drop",
    b"else",
    b"end",
    b"exec",
    b"execute",
    b"explain",
    b"export",
    b"fetch",
    b"for",
    b"foreign",
    b"format",
    b"from",
    b"generated",
    b"grant",
    b"group",
    b"having",
    b"hold",
    b"identity",
    b"immediate",
    b"import",
    b"increment",
    b"index",
    b"initially",
    b"insert",
    b"install",
    b"into",
    b"isolation",
    b"iterate",
    b"join",
    b"kill",
    b"lateral",
    b"leave",
    b"limit",
    b"listen",
    b"load",
    b"lock",
    b"loop",
    b"materialized",
    b"maxvalue",
    b"merge",
    b"minvalue",
    b"no",
    b"notify",
    b"nowait",
    b"offset",
    b"on",
    b"only",
    b"open",
    b"optimize",
    b"order",
    b"output",
    b"pragma",
    b"prepare",
    b"prewhere",
    b"primary",
    b"procedure",
    b"qualify",
    b"read",
    b"references",
    b"refresh",
    b"reindex",
    b"release",
    b"rename",
    b"repeat",
    b"restart",
    b"restrict",
    b"return",
    b"returning",
    b"revoke",
    b"role",
    b"rollback",
    b"sample",
    b"savepoint",
    b"schema",
    b"select",
    b"sequence",
    b"settings",
    b"share",
    b"show",
    b"siblings",
    b"skip",
    b"sort",
    b"start",
    b"straight_join",
    b"table",
    b"tablesample",
    b"temp",
    b"temporary",
    b"then",
    b"top",
    b"transaction",
    b"trigger",
    b"truncate",
    b"unlisten",
    b"unlogged",
    b"unpivot",
    b"until",
    b"update",
    b"use",
    b"user",
    b"using",
    b"vacuum",
    b"values",
    b"view",
    b"wait",
    b"watch",
    b"when",
    b"where",
    b"while",
    b"window",
    b"with",
    b"work",
    b"write",
];

/// Keywords that build a SET operation's left spine — counted
/// separately from segment ops (and they close the segment before
/// them).
const SET_OP: &[&[u8]] = &[b"except", b"intersect", b"minus", b"union"];

/// Words that add expression depth like a punctuation operator —
/// infix, prefix, and postfix operators spelled as words. An unlisted
/// infix keyword under-counts only if a chain can run through it, so
/// the list stays generous: an entry that turns out to be an operand
/// merely over-counts toward depth.
const OP_WORD: &[&[u8]] = &[
    b"all", b"and", b"any", b"at", b"between", b"case", b"collate", b"div", b"escape", b"exists",
    b"filter", b"glob", b"ilike", b"in", b"is", b"isnull", b"like", b"match", b"member", b"mod",
    b"not", b"notnull", b"or", b"over", b"prior", b"regexp", b"rlike", b"similar", b"some",
    b"sounds", b"within", b"xor",
];

/// Punctuation bytes that glue into one operator run (`->>`, `::`,
/// `||`…). Delimiters, quote bytes, and comment starters are absent —
/// each dispatches on its own match arm before this one.
fn op_byte(c: u8) -> bool {
    matches!(
        c,
        b'=' | b'<'
            | b'>'
            | b'!'
            | b'+'
            | b'*'
            | b'%'
            | b'|'
            | b'&'
            | b'^'
            | b'~'
            | b'?'
            | b':'
            | b'@'
            | b'.'
    )
}

/// Is the word `w` (any case) in `list` (all lowercase)?
fn word_in(w: &[u8], list: &[&[u8]]) -> bool {
    list.iter().any(|k| k.eq_ignore_ascii_case(w))
}

/// One parenthesis level's running counts.
#[derive(Default)]
struct Level {
    /// Operator units in the open segment.
    seg: usize,
    /// The largest segment finished at this level.
    peak: usize,
    /// Set operators seen at this level.
    set: usize,
}

impl Level {
    /// Close the open segment into `peak`.
    fn cut(&mut self) {
        self.peak = self.peak.max(self.seg);
        self.seg = 0;
    }
}

/// `Parser::parse_sql` behind the depth bound — text too deep to walk
/// comes back `None` like a parse error, so the caller degrades per
/// model instead of aborting the process.
pub(super) fn parse_statements(dialect: SqlDialect, sql: &str) -> Option<Vec<Statement>> {
    if too_deep(sql, dialect) {
        return None;
    }
    Parser::parse_sql(&*dialect.sqlparser(), sql).ok()
}

/// Does `sql` exceed [`MAX_DEPTH`] — the lexical estimate described at
/// the top of the file?
fn too_deep(sql: &str, dialect: SqlDialect) -> bool {
    // Every counted unit costs at least a byte — short text can't trip.
    if sql.len() < MAX_DEPTH {
        return false;
    }
    let lex = Lex::new(dialect);
    let bracket_ident = dialect.sqlparser().is_delimited_identifier_start('[');
    let b = sql.as_bytes();
    let mut levels = vec![Level::default()];
    // The peak `level + segment + set` seen — folded at `)` and EOF.
    let mut bound = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        let l = levels.len() - 1;
        match b[i] {
            // Comments and literals carry no operators — the same
            // dialect-derived ends the render scan trusts.
            b'-' if lex.dash_comment_at(b, i) => i = line_comment_end(b, i),
            b'/' if lex.slash_comment_at(b, i) => i = line_comment_end(b, i),
            b'/' if b.get(i + 1) == Some(&b'*') => i = block_comment_end(b, i, lex.nested_comments),
            b'#' if lex.hash_comment_at(b, i) => i = line_comment_end(b, i),
            b'\'' if lex.q_quote_at(b, i) => i = q_quote_end(b, i),
            q @ (b'\'' | b'"' | b'`') => i = quoted_end(b, i, q, lex.escape_for(b, i)),
            b'[' if bracket_ident => i = quoted_end(b, i, b']', false),
            b'$' if lex.dollar_quote => match dollar_quote_open(b, i) {
                Some(dlen) => {
                    let from = i + dlen;
                    i = dollar_quote_close(b, from, &b[i..from]).map_or(b.len(), |c| c + dlen);
                }
                // `$1`/identifier `$` — the word arm absorbs it.
                None => i += 1,
            },
            b'(' => {
                levels.push(Level::default());
                i += 1;
            }
            b')' => {
                levels[l].cut();
                bound = bound.max(l + levels[l].peak + levels[l].set);
                if l > 0 {
                    levels.pop();
                }
                i += 1;
            }
            // Non-ASCII is operand material — part of a UTF-8
            // identifier or literal the quote arms didn't claim — and
            // never an operator byte.
            c if !c.is_ascii() => i += 1,
            b',' => {
                levels[l].cut();
                i += 1;
            }
            // `;` ends the statement: the segment AND the set chain.
            b';' => {
                levels[l].cut();
                levels[l].set = 0;
                i += 1;
            }
            c if word_byte(c) => {
                let mut j = i + 1;
                while j < b.len() && word_byte(b[j]) {
                    j += 1;
                }
                let w = &b[i..j];
                let lv = &mut levels[l];
                if word_in(w, SET_OP) {
                    lv.cut();
                    lv.set += 1;
                } else if word_in(w, SEGMENT_END) {
                    lv.cut();
                } else if word_in(w, OP_WORD) {
                    lv.seg += 1;
                }
                i = j;
            }
            // A run of punctuation is one operator — except a run of
            // `.`s alone, which is member access and adds no depth.
            c if !c.is_ascii_whitespace() => {
                let mut j = i + 1;
                while j < b.len() && op_byte(b[j]) {
                    j += 1;
                }
                if b[i..j].iter().any(|&c| c != b'.') {
                    levels[l].seg += 1;
                }
                i = j;
            }
            _ => i += 1,
        }
        let lv = levels.last().unwrap();
        bound = bound.max(levels.len() - 1 + lv.seg.max(lv.peak) + lv.set);
        if bound > MAX_DEPTH {
            return true;
        }
    }
    bound > MAX_DEPTH
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deep(sql: &str) -> bool {
        too_deep(sql, SqlDialect::Generic)
    }

    #[test]
    fn flat_and_chain_trips() {
        let sql = format!("select a from t where {}", vec!["1"; 900].join(" and "));
        assert!(deep(&sql));
    }

    #[test]
    fn flat_concat_and_union_chains_trip() {
        let cat = format!("select {}", vec!["a"; 600].join(" || "));
        let uni = vec!["select 1"; 600].join(" union all ");
        assert!(deep(&cat));
        assert!(deep(&uni));
    }

    #[test]
    fn ordinary_sql_stays_under() {
        assert!(!deep(
            "select a, b.c, count(*) as n from sch.t join u on t.id = u.id \
             where a between 1 and 2 and b like 'x%' group by a, b.c \
             having count(*) > 10 order by n desc limit 5"
        ));
    }

    #[test]
    fn operators_inside_literals_and_comments_do_not_count() {
        // ~600 `and`s hidden in a string and a line comment — none of it
        // is expression depth.
        let filler = " and ".repeat(600);
        let sql = format!("select '{filler}' as s -- {filler}\nfrom t /* {filler} */ where a = 1");
        assert!(!deep(&sql));
    }

    #[test]
    fn non_ascii_operands_do_not_count() {
        // Unquoted UTF-8 identifiers are operands, not operators — each
        // multibyte char must not read as a punctuation run.
        let sql = format!("select {}, 名前 from テーブル", "日本語".repeat(200));
        assert!(!deep(&sql));
    }

    #[test]
    fn deep_parentheses_trip() {
        let sql = format!("select {}1{}", "(".repeat(600), ")".repeat(600));
        assert!(deep(&sql));
    }

    #[test]
    fn unrelated_clauses_do_not_add_up() {
        // A long IN list: commas reset the segment, so ~900 list items
        // read as ~900 separate single-element segments, not one chain.
        let sql = format!("select a from t where a in ({})", vec!["1"; 900].join(","));
        assert!(!deep(&sql));
    }
}
