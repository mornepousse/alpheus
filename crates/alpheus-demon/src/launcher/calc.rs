//! The launcher's calculator (small features 1): a pure evaluator, no state, no I/O.
//! A hand-written lexer, a recursive-descent parser carrying a depth counter, and
//! evaluation over exact `i128` values that fall back to `f64`.
//! `%` is the truncated remainder, as in Rust and C: it takes the sign of the left operand
//! (`-7 % 3` is `-1`).

use std::fmt;

#[cfg(test)]
mod tests;

/// Longest input (in characters) taken for an expression.
pub const MAX_INPUT: usize = 256;
/// Deepest nesting of parentheses and function calls.
pub const MAX_DEPTH: usize = 64;

/// A successful result: the decimal text, and the hex and binary forms when
/// the input was written in another base and the result is a non-negative integer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub text: String,
    pub bases: Option<(String, String)>,
}

impl Answer {
    /// `= 256`, or `= 256 · 0x100 · 0b100000000` with bases.
    pub fn shown(&self) -> String {
        match &self.bases {
            Some((hex, bin)) => format!("= {} · {hex} · {bin}", self.text),
            None => format!("= {}", self.text),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalcError {
    DivisionByZero,
    Overflow,
    Domain,
    Nesting,
}

impl fmt::Display for CalcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CalcError::DivisionByZero => "division by zero",
            CalcError::Overflow => "overflow",
            CalcError::Domain => "not a real number",
            CalcError::Nesting => "too deeply nested",
        })
    }
}

pub type Calc = Result<Answer, CalcError>;

/// A value: exact while every operation is exact and in range, else a float.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Value {
    Int(i128),
    Float(f64),
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(Value),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    LParen,
    RParen,
    Comma,
}

/// What the literals of an input looked like (for the bases).
struct Flags {
    all_int: bool,
    any_based: bool,
}

fn digit_of(c: char, radix: u32) -> bool {
    c == '_' || c.is_digit(radix)
}

/// Reads a number at `i`; `None` when it cannot be a number's value.
fn lex_number(chars: &[char], i: &mut usize, flags: &mut Flags) -> Option<Value> {
    let at = |k: usize| chars.get(k).copied();
    if at(*i) == Some('0') {
        let radix = match at(*i + 1) {
            Some('x' | 'X') => 16,
            Some('b' | 'B') => 2,
            Some('o' | 'O') => 8,
            _ => 0,
        };
        if radix != 0 && at(*i + 2).is_some_and(|c| c.is_digit(radix)) {
            let mut j = *i + 2;
            let mut digits = String::new();
            while let Some(c) = at(j).filter(|&c| digit_of(c, radix)) {
                if c != '_' {
                    digits.push(c);
                }
                j += 1;
            }
            *i = j;
            flags.any_based = true;
            return i128::from_str_radix(&digits, radix).ok().map(Value::Int);
        }
    }
    let mut text = String::new();
    while let Some(c) = at(*i).filter(|&c| digit_of(c, 10)) {
        if c != '_' {
            text.push(c);
        }
        *i += 1;
    }
    let mut float = false;
    if at(*i) == Some('.') && at(*i + 1).is_some_and(|c| c.is_ascii_digit()) {
        float = true;
        text.push('.');
        *i += 1;
        while let Some(c) = at(*i).filter(|&c| digit_of(c, 10)) {
            if c != '_' {
                text.push(c);
            }
            *i += 1;
        }
    }
    if matches!(at(*i), Some('e' | 'E')) {
        let sign = matches!(at(*i + 1), Some('+' | '-'));
        let first = *i + 1 + usize::from(sign);
        if at(first).is_some_and(|c| c.is_ascii_digit()) {
            float = true;
            text.push('e');
            if let Some(s) = at(*i + 1).filter(|_| sign) {
                text.push(s);
            }
            *i = first;
            while let Some(c) = at(*i).filter(char::is_ascii_digit) {
                text.push(c);
                *i += 1;
            }
        }
    }
    if !float {
        if let Ok(n) = text.parse::<i128>() {
            return Some(Value::Int(n));
        }
    }
    flags.all_int = false;
    text.parse::<f64>().ok().filter(|f| f.is_finite()).map(Value::Float)
}

fn lex(input: &str) -> Option<(Vec<Tok>, Flags)> {
    let chars: Vec<char> = input.chars().collect();
    let mut flags = Flags { all_int: true, any_based: false };
    let mut toks: Vec<Tok> = Vec::new();
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            toks.push(Tok::Num(lex_number(&chars, &mut i, &mut flags)?));
        } else if c.is_alphabetic() {
            // `x` between two operands is a multiplication: `2x3`, `2 x 3`.
            let after_operand = matches!(toks.last(), Some(Tok::Num(_) | Tok::RParen));
            let next_is_letter = chars.get(i + 1).is_some_and(|n| n.is_alphabetic());
            if matches!(c, 'x' | 'X') && after_operand && !next_is_letter {
                toks.push(Tok::Star);
                i += 1;
                continue;
            }
            let mut name = String::new();
            while let Some(&n) = chars.get(i).filter(|n| n.is_alphanumeric() || **n == '_') {
                name.push(n.to_ascii_lowercase());
                i += 1;
            }
            toks.push(Tok::Ident(name));
        } else {
            toks.push(match c {
                '+' => Tok::Plus,
                '-' => Tok::Minus,
                '*' | '×' => Tok::Star,
                '/' | '÷' => Tok::Slash,
                '%' => Tok::Percent,
                '^' => Tok::Caret,
                '(' => Tok::LParen,
                ')' => Tok::RParen,
                ',' => Tok::Comma,
                _ => return None,
            });
            i += 1;
        }
    }
    Some((toks, flags))
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Func {
    Sqrt,
    Cbrt,
    Abs,
    Ln,
    Log,
    Log2,
    Exp,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Round,
    Floor,
    Ceil,
    Min,
    Max,
}

fn func(name: &str) -> Option<Func> {
    Some(match name {
        "sqrt" => Func::Sqrt,
        "cbrt" => Func::Cbrt,
        "abs" => Func::Abs,
        "ln" => Func::Ln,
        "log" => Func::Log,
        "log2" => Func::Log2,
        "exp" => Func::Exp,
        "sin" => Func::Sin,
        "cos" => Func::Cos,
        "tan" => Func::Tan,
        "asin" => Func::Asin,
        "acos" => Func::Acos,
        "atan" => Func::Atan,
        "round" => Func::Round,
        "floor" => Func::Floor,
        "ceil" => Func::Ceil,
        "min" => Func::Min,
        "max" => Func::Max,
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
}

#[derive(Debug)]
enum Node {
    Num(Value),
    Neg(Box<Node>),
    Bin(Op, Box<Node>, Box<Node>),
    /// `a % of b`: `a * b / 100`.
    PercentOf(Box<Node>, Box<Node>),
    Call(Func, Vec<Node>),
}

#[derive(Debug)]
enum ParseError {
    Syntax,
    Nesting,
}

struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn eat(&mut self, t: &Tok) -> bool {
        let hit = self.peek() == Some(t);
        self.pos += usize::from(hit);
        hit
    }

    fn expr(&mut self, depth: usize) -> Result<Node, ParseError> {
        let mut left = self.term(depth)?;
        loop {
            let op = match self.peek() {
                Some(Tok::Plus) => Op::Add,
                Some(Tok::Minus) => Op::Sub,
                _ => return Ok(left),
            };
            self.pos += 1;
            left = Node::Bin(op, Box::new(left), Box::new(self.term(depth)?));
        }
    }

    fn term(&mut self, depth: usize) -> Result<Node, ParseError> {
        let mut left = self.unary(depth)?;
        loop {
            match self.peek() {
                Some(Tok::Star) => {
                    self.pos += 1;
                    left = Node::Bin(Op::Mul, Box::new(left), Box::new(self.unary(depth)?));
                }
                Some(Tok::Slash) => {
                    self.pos += 1;
                    left = Node::Bin(Op::Div, Box::new(left), Box::new(self.unary(depth)?));
                }
                Some(Tok::Percent) => {
                    self.pos += 1;
                    if matches!(self.peek(), Some(Tok::Ident(w)) if w == "of") {
                        self.pos += 1;
                        left = Node::PercentOf(Box::new(left), Box::new(self.unary(depth)?));
                    } else {
                        left = Node::Bin(Op::Rem, Box::new(left), Box::new(self.unary(depth)?));
                    }
                }
                _ => return Ok(left),
            }
        }
    }

    fn unary(&mut self, depth: usize) -> Result<Node, ParseError> {
        if self.eat(&Tok::Minus) {
            return Ok(Node::Neg(Box::new(self.unary(depth)?)));
        }
        if self.eat(&Tok::Plus) {
            return self.unary(depth);
        }
        self.power(depth)
    }

    /// `^` is right-associative and binds tighter than a unary minus on its left.
    fn power(&mut self, depth: usize) -> Result<Node, ParseError> {
        let base = self.primary(depth)?;
        if self.eat(&Tok::Caret) {
            return Ok(Node::Bin(Op::Pow, Box::new(base), Box::new(self.unary(depth)?)));
        }
        Ok(base)
    }

    fn primary(&mut self, depth: usize) -> Result<Node, ParseError> {
        let tok = self.peek().cloned().ok_or(ParseError::Syntax)?;
        self.pos += 1;
        match tok {
            Tok::Num(v) => Ok(Node::Num(v)),
            Tok::LParen => {
                let inner = self.enter(depth)?;
                let e = self.expr(inner)?;
                if self.eat(&Tok::RParen) { Ok(e) } else { Err(ParseError::Syntax) }
            }
            Tok::Ident(name) => {
                if let Some(c) = match name.as_str() {
                    "pi" => Some(std::f64::consts::PI),
                    "e" => Some(std::f64::consts::E),
                    "tau" => Some(std::f64::consts::TAU),
                    _ => None,
                } {
                    return Ok(Node::Num(Value::Float(c)));
                }
                let f = func(&name).ok_or(ParseError::Syntax)?;
                if !self.eat(&Tok::LParen) {
                    return Err(ParseError::Syntax);
                }
                let inner = self.enter(depth)?;
                let mut args = vec![self.expr(inner)?];
                while self.eat(&Tok::Comma) {
                    args.push(self.expr(inner)?);
                }
                if !self.eat(&Tok::RParen) {
                    return Err(ParseError::Syntax);
                }
                let variadic = matches!(f, Func::Min | Func::Max);
                if args.len() != 1 && !variadic {
                    return Err(ParseError::Syntax);
                }
                Ok(Node::Call(f, args))
            }
            _ => Err(ParseError::Syntax),
        }
    }

    fn enter(&self, depth: usize) -> Result<usize, ParseError> {
        if depth + 1 > MAX_DEPTH { Err(ParseError::Nesting) } else { Ok(depth + 1) }
    }
}

fn to_f(v: Value) -> f64 {
    match v {
        Value::Int(n) => n as f64,
        Value::Float(f) => f,
    }
}

/// A float result, refused when it is not a finite number.
fn fin(f: f64) -> Result<Value, CalcError> {
    if f.is_nan() {
        Err(CalcError::Domain)
    } else if f.is_infinite() {
        Err(CalcError::Overflow)
    } else {
        Ok(Value::Float(f))
    }
}

fn arith(op: Op, a: Value, b: Value) -> Result<Value, CalcError> {
    if let (Value::Int(x), Value::Int(y)) = (a, b) {
        let exact = match op {
            Op::Add => x.checked_add(y),
            Op::Sub => x.checked_sub(y),
            Op::Mul => x.checked_mul(y),
            Op::Div => {
                if y == 0 {
                    return Err(CalcError::DivisionByZero);
                }
                x.checked_rem(y).filter(|&r| r == 0).and_then(|_| x.checked_div(y))
            }
            Op::Rem => {
                if y == 0 {
                    return Err(CalcError::DivisionByZero);
                }
                match x.checked_rem(y) {
                    Some(r) => Some(r),
                    None => return Err(CalcError::Overflow),
                }
            }
            Op::Pow => u32::try_from(y).ok().and_then(|e| x.checked_pow(e)),
        };
        if let Some(n) = exact {
            return Ok(Value::Int(n));
        }
    }
    let (x, y) = (to_f(a), to_f(b));
    match op {
        Op::Add => fin(x + y),
        Op::Sub => fin(x - y),
        Op::Mul => fin(x * y),
        Op::Div | Op::Rem if y == 0.0 => Err(CalcError::DivisionByZero),
        Op::Div => fin(x / y),
        Op::Rem => fin(x % y),
        Op::Pow => fin(x.powf(y)),
    }
}

fn call(f: Func, args: &[Value]) -> Result<Value, CalcError> {
    if matches!(f, Func::Min | Func::Max) {
        let mut best = *args.first().ok_or(CalcError::Domain)?;
        for &v in args.iter().skip(1) {
            let take = match (best, v) {
                (Value::Int(a), Value::Int(b)) => (f == Func::Min && b < a) || (f == Func::Max && b > a),
                _ => (f == Func::Min && to_f(v) < to_f(best)) || (f == Func::Max && to_f(v) > to_f(best)),
            };
            if take {
                best = v;
            }
        }
        return Ok(best);
    }
    let v = *args.first().ok_or(CalcError::Domain)?;
    if let Value::Int(n) = v {
        match f {
            Func::Round | Func::Floor | Func::Ceil => return Ok(v),
            Func::Abs => {
                if let Some(a) = n.checked_abs() {
                    return Ok(Value::Int(a));
                }
            }
            _ => {}
        }
    }
    let x = to_f(v);
    match f {
        Func::Sqrt if x < 0.0 => Err(CalcError::Domain),
        Func::Ln | Func::Log | Func::Log2 if x <= 0.0 => Err(CalcError::Domain),
        Func::Sqrt => fin(x.sqrt()),
        Func::Cbrt => fin(x.cbrt()),
        Func::Abs => fin(x.abs()),
        Func::Ln => fin(x.ln()),
        Func::Log => fin(x.log10()),
        Func::Log2 => fin(x.log2()),
        Func::Exp => fin(x.exp()),
        Func::Sin => fin(x.sin()),
        Func::Cos => fin(x.cos()),
        Func::Tan => fin(x.tan()),
        Func::Asin => fin(x.asin()),
        Func::Acos => fin(x.acos()),
        Func::Atan => fin(x.atan()),
        Func::Round => fin(x.round()),
        Func::Floor => fin(x.floor()),
        Func::Ceil => fin(x.ceil()),
        Func::Min | Func::Max => Err(CalcError::Domain),
    }
}

fn eval(n: &Node) -> Result<Value, CalcError> {
    match n {
        Node::Num(v) => Ok(*v),
        Node::Neg(a) => match eval(a)? {
            Value::Int(x) => match x.checked_neg() {
                Some(r) => Ok(Value::Int(r)),
                None => Ok(Value::Float(-(x as f64))),
            },
            Value::Float(f) => Ok(Value::Float(-f)),
        },
        Node::Bin(op, a, b) => {
            let (x, y) = (eval(a)?, eval(b)?);
            arith(*op, x, y)
        }
        Node::PercentOf(a, b) => {
            let (x, y) = (eval(a)?, eval(b)?);
            arith(Op::Div, arith(Op::Mul, x, y)?, Value::Int(100))
        }
        Node::Call(f, args) => {
            let vals = args.iter().map(eval).collect::<Result<Vec<_>, _>>()?;
            call(*f, &vals)
        }
    }
}

/// `None`: not an expression (empty, too long, or it does not lex or parse).
pub fn evaluate(input: &str) -> Option<Calc> {
    let input = input.trim();
    if input.is_empty() || input.chars().count() > MAX_INPUT {
        return None;
    }
    let (toks, flags) = lex(input)?;
    let mut parser = Parser { toks: &toks, pos: 0 };
    let node = match parser.expr(0) {
        Ok(n) if parser.pos == toks.len() => n,
        Err(ParseError::Nesting) => return Some(Err(CalcError::Nesting)),
        _ => return None,
    };
    Some(eval(&node).and_then(|v| answer(v, &flags)))
}

fn answer(v: Value, flags: &Flags) -> Calc {
    match v {
        Value::Int(n) => {
            let bases = (flags.all_int && flags.any_based && n >= 0).then(|| (format!("{n:#x}"), format!("{n:#b}")));
            Ok(Answer { text: n.to_string(), bases })
        }
        Value::Float(f) => fin(f).map(|_| Answer { text: format_number(f), bases: None }),
    }
}

/// Whether a query typed in the launcher's main list should show the calculator
/// row at all: an ASCII digit, not a plain decimal number, and either more than
/// one word or symbol, or a single token that is itself a number (`0xff`, `1e3`).
/// So `e`, `pi`, `2048`, `7zip`, `1password` and `mp3` never do.
pub fn looks_like_math(input: &str) -> bool {
    let input = input.trim();
    if !input.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    let plain = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '_');
    // A sign or a balanced pair of parentheses around a plain number is still plain.
    let core = input
        .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '+' | '-' | '('))
        .trim_end_matches(|c: char| c.is_whitespace() || c == ')');
    let balanced = input.matches('(').count() == input.matches(')').count();
    let plain_number = balanced
        && match core.split_once('.') {
            Some((a, b)) => plain(a) && plain(b),
            None => plain(core),
        };
    if plain_number {
        return false;
    }
    let single_token = input.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.');
    !single_token || evaluate(input).is_some()
}

/// 12 significant digits, trailing zeros trimmed; scientific notation from a
/// decimal exponent of 15 up or below -9; `-0` is `0`.
pub(crate) fn format_number(x: f64) -> String {
    if x == 0.0 {
        return "0".into();
    }
    if !x.is_finite() {
        return if x.is_nan() { "nan".into() } else { "inf".into() };
    }
    let sci = format!("{:.11e}", x.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let sign = if x < 0.0 { "-" } else { "" };
    if !(-9..15).contains(&exp) {
        let (head, tail) = digits.split_at(1);
        let dot = if tail.is_empty() { "" } else { "." };
        return format!("{sign}{head}{dot}{tail}e{exp}");
    }
    if exp < 0 {
        return format!("{sign}0.{}{digits}", "0".repeat(exp.unsigned_abs() as usize - 1));
    }
    let int_len = exp as usize + 1;
    if digits.len() <= int_len {
        format!("{sign}{digits}{}", "0".repeat(int_len - digits.len()))
    } else {
        let (int, frac) = digits.split_at(int_len);
        format!("{sign}{int}.{frac}")
    }
}
