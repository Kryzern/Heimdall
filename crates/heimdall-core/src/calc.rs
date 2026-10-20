//! Inline calculator: a small recursive-descent evaluator.
//!
//! `+ - * / % ^`, parentheses, implicit multiplication (`2pi`, `3(4+1)`),
//! `sqrt sin cos tan asin acos atan ln log log2 abs floor ceil round exp`,
//! constants `pi e tau`.

pub fn eval(input: &str) -> Option<f64> {
    let src: String = input
        .trim()
        .trim_start_matches('=')
        .chars()
        .map(|c| match c {
            '×' => '*',
            '÷' => '/',
            '−' => '-',
            ',' => '_',
            c => c,
        })
        .filter(|&c| c != '_')
        .collect();
    if !looks_like_math(&src) {
        return None;
    }
    let mut p = Parser { s: src.as_bytes(), i: 0 };
    let v = p.expr()?;
    p.ws();
    (p.i == p.s.len() && v.is_finite()).then_some(v)
}

/// Only answer when there is something to compute: an operator or a function.
fn looks_like_math(s: &str) -> bool {
    let has_digit = s.bytes().any(|b| b.is_ascii_digit());
    let has_op = s.trim_start_matches(['-', '+']).bytes().any(|b| b"+-*/%^(".contains(&b));
    let has_fn = ["sqrt", "sin", "cos", "tan", "ln", "log", "abs", "exp", "pi", "floor", "ceil", "round"]
        .iter()
        .any(|f| s.contains(f));
    (has_digit && (has_op || has_fn)) || (has_fn && s.contains('('))
}

pub fn format(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{:.10}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.len() > 18 {
        format!("{:.6e}", v)
    } else {
        s.to_string()
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        loop {
            if self.eat(b'+') {
                v += self.term()?;
            } else if self.eat(b'-') {
                v -= self.term()?;
            } else {
                return Some(v);
            }
        }
    }

    fn term(&mut self) -> Option<f64> {
        let mut v = self.unary()?;
        loop {
            if self.eat(b'*') {
                v *= self.unary()?;
            } else if self.eat(b'/') {
                v /= self.unary()?;
            } else if self.eat(b'%') {
                v %= self.unary()?;
            } else if matches!(self.peek(), Some(b'(' | b'a'..=b'z' | b'0'..=b'9' | b'.')) {
                // implicit multiplication: 2pi, 3(4+1), (1+2)(3+4)
                v *= self.unary()?;
            } else {
                return Some(v);
            }
        }
    }

    fn unary(&mut self) -> Option<f64> {
        if self.eat(b'-') {
            return Some(-self.unary()?);
        }
        if self.eat(b'+') {
            return self.unary();
        }
        self.power()
    }

    fn power(&mut self) -> Option<f64> {
        let base = self.atom()?;
        if self.eat(b'^') {
            // right associative, binds tighter than unary minus on the left
            let exp = self.unary()?;
            return Some(base.powf(exp));
        }
        Some(base)
    }

    fn atom(&mut self) -> Option<f64> {
        match self.peek()? {
            b'(' => {
                self.i += 1;
                let v = self.expr()?;
                self.eat(b')'); // tolerate a missing close paren while typing
                Some(v)
            }
            b'0'..=b'9' | b'.' => self.number(),
            b'a'..=b'z' | b'A'..=b'Z' => {
                let start = self.i;
                while self.i < self.s.len() && self.s[self.i].is_ascii_alphanumeric() {
                    self.i += 1;
                }
                let word = std::str::from_utf8(&self.s[start..self.i]).ok()?.to_ascii_lowercase();
                match word.as_str() {
                    "pi" => return Some(std::f64::consts::PI),
                    "e" => return Some(std::f64::consts::E),
                    "tau" => return Some(std::f64::consts::TAU),
                    _ => {}
                }
                let arg = self.power()?;
                Some(match word.as_str() {
                    "sqrt" => arg.sqrt(),
                    "sin" => arg.sin(),
                    "cos" => arg.cos(),
                    "tan" => arg.tan(),
                    "asin" => arg.asin(),
                    "acos" => arg.acos(),
                    "atan" => arg.atan(),
                    "ln" => arg.ln(),
                    "log" => arg.log10(),
                    "log2" => arg.log2(),
                    "abs" => arg.abs(),
                    "floor" => arg.floor(),
                    "ceil" => arg.ceil(),
                    "round" => arg.round(),
                    "exp" => arg.exp(),
                    _ => return None,
                })
            }
            _ => None,
        }
    }

    fn number(&mut self) -> Option<f64> {
        let start = self.i;
        if self.s[self.i..].starts_with(b"0x") {
            self.i += 2;
            let hs = self.i;
            while self.i < self.s.len() && self.s[self.i].is_ascii_hexdigit() {
                self.i += 1;
            }
            let hex = std::str::from_utf8(&self.s[hs..self.i]).ok()?;
            return u64::from_str_radix(hex, 16).ok().map(|v| v as f64);
        }
        while self.i < self.s.len() && (self.s[self.i].is_ascii_digit() || self.s[self.i] == b'.') {
            self.i += 1;
        }
        if self.i < self.s.len() && self.s[self.i] == b'e' {
            let save = self.i;
            self.i += 1;
            if self.i < self.s.len() && matches!(self.s[self.i], b'+' | b'-') {
                self.i += 1;
            }
            if self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                    self.i += 1;
                }
            } else {
                self.i = save;
            }
        }
        std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic() {
        assert_eq!(eval("2^10 * 3"), Some(3072.0));
        assert_eq!(eval("(1+2)(3+4)"), Some(21.0));
        assert_eq!(eval("-2^2"), Some(-4.0));
        assert_eq!(eval("10 % 4"), Some(2.0));
        assert_eq!(eval("1,000 * 2"), Some(2000.0));
        assert_eq!(eval("0xff + 1"), Some(256.0));
        assert!((eval("2pi").unwrap() - std::f64::consts::TAU).abs() < 1e-12);
        assert_eq!(eval("sqrt(16)"), Some(4.0));
    }

    #[test]
    fn ignores_plain_text() {
        assert_eq!(eval("chrome"), None);
        assert_eq!(eval("42"), None);
        assert_eq!(eval("report-2024"), None);
        assert_eq!(eval("1/0"), None);
    }

    #[test]
    fn formatting() {
        assert_eq!(format(3072.0), "3072");
        assert_eq!(format(0.1 + 0.2), "0.3");
    }
}
