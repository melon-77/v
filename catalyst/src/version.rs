//! Version strings and dependency matching, exactly as apk-tools 3.0.8
//! (`version.c`, `package.c`; Part F).
//!
//! A version is `digits{.digits}[letter]{_suffix[digits]}[~hash][-rN]`.
//! Comparison is token by token; results are the apk match bits
//! [`LESS`], [`EQUAL`], [`GREATER`].

use std::cmp::Ordering;

pub const UNKNOWN: u8 = 0;
pub const EQUAL: u8 = 0x01;
pub const LESS: u8 = 0x02;
pub const GREATER: u8 = 0x04;
pub const FUZZY: u8 = 0x08;
pub const CONFLICT: u8 = 0x10;
/// `EQUAL | LESS | GREATER`: any version.
pub const ANY: u8 = 0x07;
/// `LESS | GREATER`: the checksum form `><`.
pub const CHECKSUM: u8 = 0x06;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Tok {
    InitialDigit = 0,
    Digit = 1,
    Letter = 2,
    Suffix = 3,
    SuffixNo = 4,
    CommitHash = 5,
    RevisionNo = 6,
    End = 7,
    Invalid = 8,
}

// suffix values (version.c:39-56); below NONE means pre-release
const SUFFIX_INVALID: u8 = 0;
const SUFFIX_NONE: u8 = 5;

fn suffix_value(s: &[u8]) -> u8 {
    let (val, lit): (u8, &[u8]) = match s.first() {
        None => return SUFFIX_INVALID,
        Some(b'a') => (1, b"alpha"),
        Some(b'b') => (2, b"beta"),
        Some(b'c') => (6, b"cvs"),
        Some(b'g') => (8, b"git"),
        Some(b'h') => (9, b"hg"),
        Some(b'p') if s.len() > 1 => (3, b"pre"),
        Some(b'p') => (10, b"p"),
        Some(b'r') => (4, b"rc"),
        Some(b's') => (7, b"svn"),
        Some(_) => return SUFFIX_INVALID,
    };
    if s == lit {
        val
    } else {
        SUFFIX_INVALID
    }
}

struct Tokens<'a> {
    b: &'a [u8],
    pos: usize,
    tok: Tok,
    suffix: u8,
    number: u64,
    value: &'a [u8],
}

impl<'a> Tokens<'a> {
    /// `token_first`.
    fn first(b: &'a [u8]) -> Self {
        let mut t = Tokens { b, pos: 0, tok: Tok::InitialDigit, suffix: 0, number: 0, value: &[] };
        t.parse_digits();
        t
    }

    /// `token_parse_digits` with `apk_blob_pull_uint(10)`: wraps on
    /// overflow, stops at the first non-digit.
    fn parse_digits(&mut self) {
        let start = self.pos;
        let mut n: u64 = 0;
        while self.pos < self.b.len() && self.b[self.pos].is_ascii_digit() {
            n = n.wrapping_mul(10).wrapping_add((self.b[self.pos] - b'0') as u64);
            self.pos += 1;
        }
        self.number = n;
        self.value = &self.b[start..self.pos];
        if self.value.is_empty() {
            self.tok = Tok::Invalid;
        }
    }

    fn span(&mut self, pred: impl Fn(u8) -> bool) -> &'a [u8] {
        let start = self.pos;
        while self.pos < self.b.len() && pred(self.b[self.pos]) {
            self.pos += 1;
        }
        &self.b[start..self.pos]
    }

    /// `token_next`.
    fn next(&mut self) {
        if self.pos >= self.b.len() {
            self.tok = Tok::End;
            return;
        }
        let c = self.b[self.pos];
        match c {
            b'a'..=b'z' => {
                if self.tok > Tok::Digit {
                    self.tok = Tok::Invalid;
                    return;
                }
                self.value = &self.b[self.pos..self.pos + 1];
                self.tok = Tok::Letter;
                self.pos += 1;
            }
            b'.' | b'0'..=b'9' => {
                if c == b'.' {
                    if self.tok > Tok::Digit {
                        self.tok = Tok::Invalid;
                        return;
                    }
                    self.pos += 1;
                }
                self.tok = match self.tok {
                    Tok::InitialDigit | Tok::Digit => Tok::Digit,
                    Tok::Suffix => Tok::SuffixNo,
                    _ => {
                        self.tok = Tok::Invalid;
                        return;
                    }
                };
                self.parse_digits();
            }
            b'_' => {
                if self.tok > Tok::SuffixNo {
                    self.tok = Tok::Invalid;
                    return;
                }
                self.pos += 1;
                self.value = self.span(|c| c.is_ascii_lowercase());
                self.suffix = suffix_value(self.value);
                self.tok = if self.suffix == SUFFIX_INVALID { Tok::Invalid } else { Tok::Suffix };
            }
            b'~' => {
                if self.tok >= Tok::CommitHash {
                    self.tok = Tok::Invalid;
                    return;
                }
                self.pos += 1;
                self.value = self.span(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c));
                self.tok = if self.value.is_empty() { Tok::Invalid } else { Tok::CommitHash };
            }
            b'-' => {
                if self.tok >= Tok::RevisionNo || !self.b[self.pos..].starts_with(b"-r") {
                    self.tok = Tok::Invalid;
                    return;
                }
                self.pos += 2;
                self.tok = Tok::RevisionNo;
                self.parse_digits();
            }
            _ => self.tok = Tok::Invalid,
        }
    }
}

/// `apk_blob_sort`: bytewise over the common length, then shorter first.
fn blob_sort(a: &[u8], b: &[u8]) -> Ordering {
    a.cmp(b)
}

fn ord_bits(o: Ordering) -> u8 {
    match o {
        Ordering::Less => LESS,
        Ordering::Equal => EQUAL,
        Ordering::Greater => GREATER,
    }
}

/// `token_cmp`; both tokens have the same kind, below End.
fn token_cmp(a: &Tokens, b: &Tokens) -> u8 {
    match a.tok {
        Tok::Digit if a.value[0] == b'0' || b.value[0] == b'0' => ord_bits(blob_sort(a.value, b.value)),
        Tok::Digit | Tok::InitialDigit | Tok::SuffixNo | Tok::RevisionNo => ord_bits(a.number.cmp(&b.number)),
        Tok::Letter => ord_bits(a.value[0].cmp(&b.value[0])),
        Tok::Suffix => ord_bits(a.suffix.cmp(&b.suffix)),
        _ => ord_bits(blob_sort(a.value, b.value)),
    }
}

/// `apk_version_validate`: the whole string is a valid version.
pub fn validate(v: &str) -> bool {
    let mut t = Tokens::first(v.as_bytes());
    while t.tok < Tok::End {
        t.next();
    }
    t.tok == Tok::End
}

/// `apk_version_compare_fuzzy` on two present versions. Returns LESS,
/// EQUAL or GREATER (comparing `a` to `b`). With `fuzzy`, `a` is EQUAL
/// when `b`'s tokens are a prefix of `a`'s.
pub fn compare_fuzzy(a: &str, b: &str, fuzzy: bool) -> u8 {
    let mut ta = Tokens::first(a.as_bytes());
    let mut tb = Tokens::first(b.as_bytes());
    while ta.tok == tb.tok && ta.tok < Tok::End {
        let r = token_cmp(&ta, &tb);
        if r != EQUAL {
            return r;
        }
        ta.next();
        tb.next();
    }
    if ta.tok == tb.tok {
        return EQUAL;
    }
    if tb.tok == Tok::End && fuzzy {
        return EQUAL;
    }
    if ta.tok == Tok::Suffix && ta.suffix < SUFFIX_NONE {
        return LESS;
    }
    if tb.tok == Tok::Suffix && tb.suffix < SUFFIX_NONE {
        return GREATER;
    }
    if ta.tok > tb.tok {
        return LESS;
    }
    if tb.tok > ta.tok {
        return GREATER;
    }
    EQUAL
}

/// `apk_version_compare`: LESS, EQUAL or GREATER.
pub fn compare(a: &str, b: &str) -> u8 {
    compare_fuzzy(a, b, false)
}

/// The same with absent versions (raw ADB blobs): both absent are EQUAL,
/// one absent gives `EQUAL | LESS | GREATER` (§F1.5).
pub fn compare_opt(a: Option<&str>, b: Option<&str>, fuzzy: bool) -> u8 {
    match (a, b) {
        (Some(a), Some(b)) => compare_fuzzy(a, b, fuzzy),
        (None, None) => EQUAL,
        _ => ANY,
    }
}

/// [`compare`] as an [`Ordering`], for sorting.
pub fn cmp(a: &str, b: &str) -> Ordering {
    match compare(a, b) {
        LESS => Ordering::Less,
        GREATER => Ordering::Greater,
        _ => Ordering::Equal,
    }
}

/// `apk_version_match`: does the candidate version `a` satisfy `op b`?
/// `op` may include FUZZY and CONFLICT (which inverts the result).
pub fn version_match(a: &str, op: u8, b: &str) -> bool {
    let mut ok = (op & ANY) == ANY || (compare_fuzzy(a, b, op & FUZZY != 0) & op) != 0;
    if op & CONFLICT != 0 {
        ok = !ok;
    }
    ok
}

/// `apk_version_op_string` (§F2.3).
pub fn op_string(op: u8) -> &'static str {
    match op & !CONFLICT {
        LESS => "<",
        x if x == LESS | EQUAL => "<=",
        x if x == LESS | EQUAL | FUZZY => "<~",
        x if x == EQUAL | FUZZY || x == FUZZY => "~",
        EQUAL => "=",
        x if x == GREATER | EQUAL => ">=",
        x if x == GREATER | EQUAL | FUZZY => ">~",
        GREATER => ">",
        CHECKSUM => "><",
        ANY => "",
        _ => "?",
    }
}

/// `apk_version_result_mask_blob` (§F2.2): `<`=2, `>`=4, `=`=1, `~`=9,
/// OR-ed in any order; any other byte gives 0.
pub fn op_from_str(s: &str) -> u8 {
    let mut r = 0;
    for c in s.bytes() {
        r |= match c {
            b'<' => LESS,
            b'>' => GREATER,
            b'=' => EQUAL,
            b'~' => FUZZY | EQUAL,
            _ => return 0,
        };
    }
    r
}

fn is_comparer(c: u8) -> bool {
    matches!(c, b'<' | b'=' | b'>' | b'~')
}

/// `apk_dep_parse` (§F3.1): `[!]name[op version]` to (name, op, version).
/// Without an operator, op is ANY (plus CONFLICT) and the version absent.
/// `None` when an operator is not followed by a version.
pub fn parse_dep(spec: &str) -> Option<(&str, u8, Option<&str>)> {
    let mut op = 0u8;
    let mut s = spec;
    if let Some(rest) = s.strip_prefix('!') {
        op |= CONFLICT;
        s = rest;
    }
    match s.bytes().position(is_comparer) {
        Some(i) => {
            let name = &s[..i];
            let rest = &s[i..];
            let j = rest.bytes().position(|c| !is_comparer(c))?;
            op |= op_from_str(&rest[..j]);
            if op & !CONFLICT == 0 {
                return None;
            }
            Some((name, op, Some(&rest[j..])))
        }
        None => Some((s, op | ANY, None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(a: &str, b: &str) -> u8 {
        compare(a, b)
    }

    #[test]
    fn worked_results_f1_6() {
        assert_eq!(c("1.2", "1.2.3"), LESS);
        assert_eq!(c("1.2.3_rc1", "1.2.3"), LESS);
        assert_eq!(c("1.2.3_alpha", "1.2.3_beta"), LESS);
        assert_eq!(c("1.2.3_beta", "1.2.3_pre"), LESS);
        assert_eq!(c("1.2.3_pre", "1.2.3_rc"), LESS);
        assert_eq!(c("1.2.3", "1.2.3_p1"), LESS);
        assert_eq!(c("1.2.3_p1", "1.2.3-r1"), GREATER);
        assert_eq!(c("1.2.3", "1.2.3-r0"), LESS);
        assert_eq!(c("1.0_alpha", "1.0_alpha1"), LESS);
        assert_eq!(c("1.0_alpha", "1.0_alpha0"), LESS);
        assert_eq!(c("1.0_rc1", "1.0_rc"), GREATER);
        assert_eq!(c("1.2", "1.2a"), LESS);
        assert_eq!(c("1.2a", "1.2.1"), LESS);
        assert_eq!(c("1.0_p1", "1.0a"), LESS);
        assert_eq!(c("1.10", "1.9"), GREATER);
        assert_eq!(c("1.02", "1.1"), LESS);
        assert_eq!(c("1.010", "1.09"), LESS);
        assert_eq!(c("1.0", "1.00"), LESS);
        assert_eq!(c("01", "1"), EQUAL);
        assert_eq!(c("1.0~abc", "1.0"), GREATER);
        assert_eq!(c("1.0~abc", "1.0-r1"), GREATER);
        assert_eq!(c("1.0~abc", "1.0~abd"), LESS);
        // symmetric checks
        assert_eq!(c("1.2.3", "1.2"), GREATER);
        assert_eq!(c("1.2.3", "1.2.3_rc1"), GREATER);
        assert_eq!(c("1.2.3-r1", "1.2.3-r1"), EQUAL);
        assert_eq!(c("2.44-r1", "2.44-r10"), LESS);
    }

    #[test]
    fn invalid_and_reader_notes_f1_5() {
        assert_eq!(c("1.2X", "1.2Y"), EQUAL);
        assert_eq!(c("1.2X", "1.2"), LESS);
        assert_eq!(c("", "abc"), EQUAL);
        assert_eq!(c("", "1"), LESS); // "" is INVALID (8) > INITIAL_DIGIT (0)
        assert_eq!(compare_opt(None, None, false), EQUAL);
        assert_eq!(compare_opt(None, Some("1"), false), ANY);
        // wrapping u64 numbers
        assert_eq!(c("18446744073709551616", "0"), EQUAL);
        assert_eq!(c("1.18446744073709551617", "1.1"), EQUAL);
    }

    #[test]
    fn validate_f1_3() {
        for v in ["1", "1.2.3", "1a", "1.2a", "1.0_rc", "1.0_alpha1_p2", "1.0_alpha_beta2", "1.0~abc",
                  "1.0-r1", "1.2_rc1~ab-r5", "0", "2.44-r1", "20240318-r1"] {
            assert!(validate(v), "{v} should be valid");
        }
        for v in ["", "a", "1.", "1..2", "1.a", "1.2ab", "1.2a.3", "1.2a3", "1.0-r", "1.0-x1", "1.0-r1a",
                  "1.0_pa", "1.0_r", "1.0_Rc", "1.0_", "1.0~", "1.0~ABC", "1.0+1", "1.0-r1-r2",
                  "1.0_rc1.2", "1.0~ab~cd", "v1"] {
            assert!(!validate(v), "{v} should be invalid");
        }
    }

    #[test]
    fn fuzzy_f1_7() {
        let m = |a: &str| version_match(a, FUZZY | EQUAL, "1.2");
        for a in ["1.2", "1.2.3", "1.2a", "1.2_rc1", "1.2-r5", "1.2~ab", "1.2X"] {
            assert!(m(a), "~1.2 should match {a}");
        }
        for a in ["1.20", "1.3", "1.1"] {
            assert!(!m(a), "~1.2 should not match {a}");
        }
    }

    #[test]
    fn ops_f2() {
        assert_eq!(op_from_str("<="), 3);
        assert_eq!(op_from_str("=<"), 3);
        assert_eq!(op_from_str("~"), 9);
        assert_eq!(op_from_str("=~"), 9);
        assert_eq!(op_from_str("~="), 9);
        assert_eq!(op_from_str("<>"), 6);
        assert_eq!(op_from_str("><"), 6);
        assert_eq!(op_from_str("<=>"), 7);
        assert_eq!(op_from_str("<x"), 0);
        for (m, s) in [(2, "<"), (3, "<="), (11, "<~"), (9, "~"), (8, "~"), (1, "="), (5, ">="),
                       (13, ">~"), (4, ">"), (6, "><"), (7, ""), (0, "?"), (12, "?")] {
            assert_eq!(op_string(m), s, "mask {m}");
            assert_eq!(op_string(m | CONFLICT), s);
        }
        // match semantics (§F2.4)
        assert!(version_match("1.0", ANY, "9"));
        assert!(!version_match("1.0", FUZZY, "1.0"));
        assert!(version_match("1.0", CONFLICT, "1.0"));
        assert!(version_match("2.0", GREATER | EQUAL, "1.0"));
        assert!(!version_match("2.0", CONFLICT | GREATER | EQUAL, "1.0"));
        assert!(!version_match("1.0", CONFLICT | ANY, ""));
    }

    #[test]
    fn parse_f3() {
        assert_eq!(parse_dep("foo"), Some(("foo", 7, None)));
        assert_eq!(parse_dep("so:libc.so.6"), Some(("so:libc.so.6", 7, None)));
        assert_eq!(parse_dep("foo=1.2"), Some(("foo", 1, Some("1.2"))));
        assert_eq!(parse_dep("foo<1.2"), Some(("foo", 2, Some("1.2"))));
        assert_eq!(parse_dep("foo<=1.2"), Some(("foo", 3, Some("1.2"))));
        assert_eq!(parse_dep("foo>1.2"), Some(("foo", 4, Some("1.2"))));
        assert_eq!(parse_dep("foo>=1.2"), Some(("foo", 5, Some("1.2"))));
        assert_eq!(parse_dep("foo><Q1abc"), Some(("foo", 6, Some("Q1abc"))));
        assert_eq!(parse_dep("foo~1.2"), Some(("foo", 9, Some("1.2"))));
        assert_eq!(parse_dep("foo=~1.2"), Some(("foo", 9, Some("1.2"))));
        assert_eq!(parse_dep("foo<~1.2"), Some(("foo", 11, Some("1.2"))));
        assert_eq!(parse_dep("foo>~1.2"), Some(("foo", 13, Some("1.2"))));
        assert_eq!(parse_dep("!foo"), Some(("foo", 23, None)));
        assert_eq!(parse_dep("!foo=1.2"), Some(("foo", 17, Some("1.2"))));
        assert_eq!(parse_dep("!foo<1.2"), Some(("foo", 18, Some("1.2"))));
        assert_eq!(parse_dep("!foo>=1.2"), Some(("foo", 21, Some("1.2"))));
        assert_eq!(parse_dep("foo<=>1"), Some(("foo", 7, Some("1"))));
        assert_eq!(parse_dep("=1.0"), Some(("", 1, Some("1.0"))));
        assert_eq!(parse_dep("!!foo"), Some(("!foo", 23, None)));
        assert_eq!(parse_dep("foo="), None);
        assert_eq!(parse_dep("foo>="), None);
    }

    #[test]
    fn providers_f4() {
        // provides so:libfoo.so.1=0
        assert!(version_match("0", ANY, ""));
        assert!(version_match("0", GREATER | EQUAL, "0"));
        // provides pc:foo=1.2
        assert!(version_match("1.2", GREATER | EQUAL, "1.0"));
        assert!(version_match("1.2", FUZZY | EQUAL, "1"));
        assert!(!version_match("1.2", LESS, "1"));
        // §F4.4: an unversioned provider ("") against versioned deps
        assert!(version_match("", ANY, ""));
        assert!(version_match("", LESS, "1"));
        assert!(version_match("", LESS | EQUAL, "1"));
        assert!(version_match("", LESS | EQUAL | FUZZY, "1"));
        assert!(!version_match("", EQUAL, "1"));
        assert!(!version_match("", GREATER, "1"));
        assert!(!version_match("", GREATER | EQUAL, "1"));
        assert!(!version_match("", FUZZY | EQUAL, "1"));
        // §F3.8: !foo><Q1... conflicts with every normally versioned provider
        assert!(!version_match("1.0", CONFLICT | CHECKSUM, "Q1abc"));
    }

    #[test]
    fn sorting() {
        let mut v = vec!["1.10", "1.9", "1.9_rc1", "1.9-r1", "1.9a", "1.9.1"];
        v.sort_by(|a, b| cmp(a, b));
        assert_eq!(v, vec!["1.9_rc1", "1.9", "1.9-r1", "1.9a", "1.9.1", "1.10"]);
    }
}
