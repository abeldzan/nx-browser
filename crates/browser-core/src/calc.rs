/// Evaluates simple arithmetic (`+ - * / % ^`, parentheses, decimals) for the command palette.
/// `None` unless the input is a whole expression with at least one operator.
pub fn calc(input: &str) -> Option<f64> {
    // The parser recurses per nesting level; a pasted wall of `(` would overflow the stack.
    if input.len() > 512 {
        return None;
    }
    let tokens = input.chars().filter(|c| !c.is_whitespace()).collect::<Vec<_>>();
    if !tokens.iter().any(|c| "+-*/%^".contains(*c)) {
        return None;
    }
    let mut parser = Parser { tokens, at: 0 };
    let value = parser.sum()?;
    (parser.at == parser.tokens.len() && value.is_finite()).then_some(value)
}

struct Parser {
    tokens: Vec<char>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.tokens.get(self.at).copied()
    }

    fn sum(&mut self) -> Option<f64> {
        let mut value = self.product()?;
        while let Some(op @ ('+' | '-')) = self.peek() {
            self.at += 1;
            let rhs = self.product()?;
            value = if op == '+' { value + rhs } else { value - rhs };
        }
        Some(value)
    }

    fn product(&mut self) -> Option<f64> {
        let mut value = self.unary()?;
        while let Some(op @ ('*' | '/' | '%')) = self.peek() {
            self.at += 1;
            let rhs = self.unary()?;
            value = match op {
                '*' => value * rhs,
                _ if rhs == 0.0 => return None,
                '/' => value / rhs,
                _ => value % rhs,
            };
        }
        Some(value)
    }

    /// Unary minus binds looser than `^`: `-2^2` is -4.
    fn unary(&mut self) -> Option<f64> {
        if self.peek() == Some('-') {
            self.at += 1;
            return Some(-self.unary()?);
        }
        self.power()
    }

    /// Right-associative: `2^3^2` is 2^9.
    fn power(&mut self) -> Option<f64> {
        let base = self.atom()?;
        if self.peek() == Some('^') {
            self.at += 1;
            return Some(base.powf(self.unary()?));
        }
        Some(base)
    }

    fn atom(&mut self) -> Option<f64> {
        if self.peek() == Some('(') {
            self.at += 1;
            let value = self.sum()?;
            (self.peek() == Some(')')).then(|| self.at += 1)?;
            return Some(value);
        }
        let start = self.at;
        while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '.') {
            self.at += 1;
        }
        self.tokens[start..self.at].iter().collect::<String>().parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::calc;

    #[test]
    fn evaluates_arithmetic() {
        assert_eq!(calc("12*(3+4)/2"), Some(42.0));
        assert_eq!(calc("2 + 3 * 4"), Some(14.0));
        assert_eq!(calc("2^3^2"), Some(512.0));
        assert_eq!(calc("-2^2"), Some(-4.0));
        assert_eq!(calc("10 % 4 - 0.5"), Some(1.5));
    }

    #[test]
    fn rejects_non_math() {
        assert_eq!(calc("42"), None);
        assert_eq!(calc("1/0"), None);
        assert_eq!(calc("rust-lang"), None);
        assert_eq!(calc(&"(".repeat(200_000)), None);
        assert_eq!(calc("(1+2"), None);
        assert_eq!(calc("1+"), None);
    }
}
