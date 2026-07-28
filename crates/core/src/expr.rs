//! Address expressions: the per-class address bar.
//!
//! Grammar (ReClass-style), lowest to highest precedence:
//!
//! ```text
//! expr   := term  (('+' | '-') term)*
//! term   := unary (('*' | '/') unary)*
//! unary  := '[' expr ']'        // pointer-sized dereference
//!         | '<' module '>'      // module load base
//!         | '(' expr ')'
//!         | number              // 0x.. hex or decimal
//! ```
//!
//! Examples: `<module.so> + 0x10`, `[0xADDR]`, `[<module> + 0x10] + 0x20`.

use crate::backend::{MemError, MemoryBackend};

/// A parsed address expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddrExpr {
    /// Literal value.
    Num(u64),
    /// Load base of a module, by basename.
    Module(String),
    /// Pointer-sized dereference of the inner expression.
    Deref(Box<AddrExpr>),
    /// Binary integer operation.
    Bin(BinOp, Box<AddrExpr>, Box<AddrExpr>),
}

/// Binary operators (integer arithmetic, wrapping for `+ - *`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    /// Addition.
    Add,
    /// Subtraction.
    Sub,
    /// Multiplication.
    Mul,
    /// Integer division.
    Div,
}

/// Parse or evaluation error.
#[derive(Debug, thiserror::Error)]
pub enum ExprError {
    /// Syntax error (with a message and byte position).
    #[error("parse error at {pos}: {msg}")]
    Parse {
        /// Human-readable reason.
        msg: String,
        /// Byte offset into the input.
        pos: usize,
    },
    /// A `<module>` could not be resolved by the backend.
    #[error("module '{0}' not found")]
    ModuleNotFound(String),
    /// Integer division by zero.
    #[error("division by zero")]
    DivByZero,
    /// A dereference read failed.
    #[error("dereference failed: {0}")]
    Read(#[from] MemError),
}

impl AddrExpr {
    /// Parse an address expression.
    pub fn parse(input: &str) -> Result<AddrExpr, ExprError> {
        let mut p = Parser {
            src: input.as_bytes(),
            pos: 0,
            depth: 0,
        };
        let e = p.expr()?;
        p.skip_ws();
        if p.pos != p.src.len() {
            return Err(p.err("unexpected trailing input"));
        }
        Ok(e)
    }

    /// Resolve to a final address, dereferencing through `backend`.
    pub fn eval(&self, backend: &dyn MemoryBackend) -> Result<u64, ExprError> {
        match self {
            AddrExpr::Num(n) => Ok(*n),
            AddrExpr::Module(name) => backend
                .module_base(name)
                .ok_or_else(|| ExprError::ModuleNotFound(name.clone())),
            AddrExpr::Deref(inner) => {
                let addr = inner.eval(backend)?;
                let mut buf = [0u8; 8];
                backend.read(addr, &mut buf)?;
                Ok(u64::from_le_bytes(buf))
            }
            AddrExpr::Bin(op, a, b) => {
                let l = a.eval(backend)?;
                let r = b.eval(backend)?;
                Ok(match op {
                    BinOp::Add => l.wrapping_add(r),
                    BinOp::Sub => l.wrapping_sub(r),
                    BinOp::Mul => l.wrapping_mul(r),
                    BinOp::Div => {
                        if r == 0 {
                            return Err(ExprError::DivByZero);
                        }
                        l / r
                    }
                })
            }
        }
    }

    /// Convenience: parse then evaluate.
    pub fn resolve(input: &str, backend: &dyn MemoryBackend) -> Result<u64, ExprError> {
        AddrExpr::parse(input)?.eval(backend)
    }
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    depth: usize,
}

impl Parser<'_> {
    /// Hard cap on bracket/paren nesting; bounds recursion on adversarial input.
    const MAX_DEPTH: usize = 64;

    fn err(&self, msg: &str) -> ExprError {
        ExprError::Parse {
            msg: msg.to_string(),
            pos: self.pos,
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.src.len() && self.src[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.src.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn expr(&mut self) -> Result<AddrExpr, ExprError> {
        self.depth += 1;
        if self.depth > Self::MAX_DEPTH {
            return Err(self.err("expression nested too deep"));
        }
        let mut left = self.term()?;
        loop {
            match self.peek() {
                Some(b'+') => {
                    self.bump();
                    let right = self.term()?;
                    left = AddrExpr::Bin(BinOp::Add, Box::new(left), Box::new(right));
                }
                Some(b'-') => {
                    self.bump();
                    let right = self.term()?;
                    left = AddrExpr::Bin(BinOp::Sub, Box::new(left), Box::new(right));
                }
                _ => break,
            }
        }
        self.depth -= 1;
        Ok(left)
    }

    fn term(&mut self) -> Result<AddrExpr, ExprError> {
        let mut left = self.unary()?;
        loop {
            match self.peek() {
                Some(b'*') => {
                    self.bump();
                    let right = self.unary()?;
                    left = AddrExpr::Bin(BinOp::Mul, Box::new(left), Box::new(right));
                }
                Some(b'/') => {
                    self.bump();
                    let right = self.unary()?;
                    left = AddrExpr::Bin(BinOp::Div, Box::new(left), Box::new(right));
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<AddrExpr, ExprError> {
        match self.peek() {
            Some(b'[') => {
                self.bump();
                let inner = self.expr()?;
                match self.bump() {
                    Some(b']') => Ok(AddrExpr::Deref(Box::new(inner))),
                    _ => Err(self.err("expected ']'")),
                }
            }
            Some(b'(') => {
                self.bump();
                let inner = self.expr()?;
                match self.bump() {
                    Some(b')') => Ok(inner),
                    _ => Err(self.err("expected ')'")),
                }
            }
            Some(b'<') => {
                self.bump();
                let start = self.pos;
                while self.pos < self.src.len() && self.src[self.pos] != b'>' {
                    self.pos += 1;
                }
                if self.pos >= self.src.len() {
                    return Err(self.err("unterminated module name (expected '>')"));
                }
                let name = std::str::from_utf8(&self.src[start..self.pos])
                    .map_err(|_| self.err("module name is not valid UTF-8"))?
                    .trim()
                    .to_string();
                self.pos += 1; // consume '>'
                if name.is_empty() {
                    return Err(self.err("empty module name"));
                }
                Ok(AddrExpr::Module(name))
            }
            Some(c) if c.is_ascii_digit() => self.number(),
            Some(_) => Err(self.err("unexpected character")),
            None => Err(self.err("unexpected end of input")),
        }
    }

    fn number(&mut self) -> Result<AddrExpr, ExprError> {
        self.skip_ws();
        let start = self.pos;
        let (radix, digit_start) =
            if self.src[self.pos..].starts_with(b"0x") || self.src[self.pos..].starts_with(b"0X") {
                (16, start + 2)
            } else {
                (10, start)
            };
        self.pos = digit_start;
        while self.pos < self.src.len() && (self.src[self.pos] as char).is_digit(radix) {
            self.pos += 1;
        }
        if self.pos == digit_start {
            return Err(ExprError::Parse {
                msg: "expected number".to_string(),
                pos: start,
            });
        }
        let text = std::str::from_utf8(&self.src[digit_start..self.pos])
            .expect("digit bytes are always valid UTF-8");
        u64::from_str_radix(text, radix)
            .map(AddrExpr::Num)
            .map_err(|_| ExprError::Parse {
                msg: "number out of range".to_string(),
                pos: start,
            })
    }
}

#[cfg(all(test, feature = "mock"))]
mod tests {
    use super::*;
    use crate::backend::MockBackend;

    #[test]
    fn parse_literals_hex_decimal() {
        assert_eq!(AddrExpr::parse("0x10").unwrap(), AddrExpr::Num(0x10));
        assert_eq!(AddrExpr::parse("16").unwrap(), AddrExpr::Num(16));
    }

    #[test]
    fn module_plus_offset() {
        let m = MockBackend::new();
        m.put_module("module.so", 0x5000);
        assert_eq!(AddrExpr::resolve("<module.so> + 0x10", &m).unwrap(), 0x5010);
    }

    #[test]
    fn deref_literal() {
        let m = MockBackend::new();
        // at 0xADDR=0x1000 store pointer 0x4242
        m.put(0x1000, 0x4242u64.to_le_bytes().to_vec());
        assert_eq!(AddrExpr::resolve("[0x1000]", &m).unwrap(), 0x4242);
    }

    #[test]
    fn nested_deref_module() {
        let m = MockBackend::new();
        m.put_module("module", 0x1000);
        // [<module> + 0x10] -> read at 0x1010
        m.put(0x1010, 0x9000u64.to_le_bytes().to_vec());
        // result: 0x9000 + 0x20
        assert_eq!(
            AddrExpr::resolve("[<module> + 0x10] + 0x20", &m).unwrap(),
            0x9020
        );
    }

    #[test]
    fn operators_and_precedence() {
        let m = MockBackend::new();
        assert_eq!(AddrExpr::resolve("2 + 3 * 4", &m).unwrap(), 14);
        assert_eq!(AddrExpr::resolve("(2 + 3) * 4", &m).unwrap(), 20);
        assert_eq!(AddrExpr::resolve("0x100 - 0x10", &m).unwrap(), 0xF0);
        assert_eq!(AddrExpr::resolve("0x100 / 0x10", &m).unwrap(), 0x10);
    }

    #[test]
    fn div_by_zero_and_missing_module() {
        let m = MockBackend::new();
        assert!(matches!(
            AddrExpr::resolve("1 / 0", &m),
            Err(ExprError::DivByZero)
        ));
        assert!(matches!(
            AddrExpr::resolve("<nope>", &m),
            Err(ExprError::ModuleNotFound(_))
        ));
    }

    #[test]
    fn parse_errors() {
        assert!(matches!(
            AddrExpr::parse("[0x10"),
            Err(ExprError::Parse { .. })
        ));
        assert!(matches!(
            AddrExpr::parse("<mod"),
            Err(ExprError::Parse { .. })
        ));
        assert!(matches!(
            AddrExpr::parse("0x10 0x20"),
            Err(ExprError::Parse { .. })
        ));
        assert!(matches!(AddrExpr::parse(""), Err(ExprError::Parse { .. })));
    }

    #[test]
    fn deeply_nested_is_rejected() {
        // Hundreds of '[' would overflow a naive recursive-descent parser; the
        // depth cap turns it into a clean parse error well before that.
        let deep = format!("{}0x1000{}", "[".repeat(200), "]".repeat(200));
        assert!(matches!(
            AddrExpr::parse(&deep),
            Err(ExprError::Parse { .. })
        ));
        // reasonable nesting still parses
        assert!(AddrExpr::parse("[[[0x10]]]").is_ok());
    }

    /// Deterministic xorshift64*, so a failure is reproducible from the seed
    /// printed in the assertion rather than needing a fuzz corpus.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    #[test]
    fn arbitrary_input_never_panics_or_hangs() {
        // The parser is the only hand-written one in the crate and it takes
        // user input on every keystroke of the address bar, so the contract
        // that matters is "always returns" — not that anything parses.
        const ALPHABET: &[u8] = b"[]<>()+-*/0123456789abcdefxX. \t\0\xff";
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        let mut ok = 0usize;
        for case in 0..20_000 {
            let len = rng.below(24) as usize;
            let s: String = (0..len)
                .map(|_| ALPHABET[rng.below(ALPHABET.len() as u64) as usize] as char)
                .collect();
            // A panic here fails the test by unwinding; the assertion exists so
            // a silent change to "always Err" is visible too.
            if AddrExpr::parse(&s).is_ok() {
                ok += 1;
            }
            assert!(case < 20_000);
        }
        assert!(ok > 0, "the generator never produced a valid expression");
    }

    /// Build a random expression string alongside the value it must evaluate to.
    ///
    /// Deref is excluded: its value depends on target memory, and the point
    /// here is that parsing and arithmetic agree.
    fn gen_expr(rng: &mut Rng, depth: u32) -> (String, u64) {
        if depth == 0 || rng.below(3) == 0 {
            let v = rng.next() % 0x1_0000;
            return if rng.below(2) == 0 {
                (format!("0x{v:X}"), v)
            } else {
                (format!("{v}"), v)
            };
        }
        let (ls, lv) = gen_expr(rng, depth - 1);
        let (rs, rv) = gen_expr(rng, depth - 1);
        match rng.below(4) {
            0 => (format!("({ls} + {rs})"), lv.wrapping_add(rv)),
            1 => (format!("({ls} - {rs})"), lv.wrapping_sub(rv)),
            2 => (format!("({ls} * {rs})"), lv.wrapping_mul(rv)),
            _ if rv != 0 => (format!("({ls} / {rs})"), lv / rv),
            _ => (format!("({ls} + {rs})"), lv.wrapping_add(rv)),
        }
    }

    #[test]
    fn generated_expressions_parse_and_evaluate_to_the_expected_value() {
        let backend = crate::backend::MockBackend::new();
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for i in 0..2_000 {
            let (src, expected) = gen_expr(&mut rng, 4);
            let got = AddrExpr::resolve(&src, &backend)
                .unwrap_or_else(|e| panic!("case {i}: {src} failed: {e}"));
            assert_eq!(got, expected, "case {i}: {src}");
        }
    }

    #[test]
    fn precedence_holds_without_parentheses() {
        // The generator always parenthesizes, so precedence needs its own case.
        let b = crate::backend::MockBackend::new();
        for (src, want) in [
            ("2 + 3 * 4", 14u64),
            ("2 * 3 + 4", 10),
            ("20 / 4 - 1", 4),
            ("1 - 2 + 3", 2),
            ("0x10 + 0x10 * 2", 0x30),
        ] {
            assert_eq!(AddrExpr::resolve(src, &b).unwrap(), want, "{src}");
        }
    }

    #[test]
    fn whitespace_is_insignificant_between_tokens() {
        let b = crate::backend::MockBackend::new();
        let dense = AddrExpr::resolve("0x10+0x20*2", &b).unwrap();
        let loose = AddrExpr::resolve("  0x10\t+\n0x20 *  2 ", &b).unwrap();
        assert_eq!(dense, loose);
    }
}
