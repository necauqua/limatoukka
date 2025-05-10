use thiserror::Error;

pub struct Calculator<'a> {
    chars: std::str::Chars<'a>,
    curr: Option<char>,
}

#[derive(Debug, Error)]
pub enum CalculatorError {
    #[error("expected end of math expr")]
    ExpectedEnd,
    #[error("expected a number")]
    ExpectedNumber,
    #[error("division by zero")]
    DivisionByZero,
    #[error("missing ')'")]
    MissingClosingParen,
}

type Result = std::result::Result<i64, CalculatorError>;

impl<'a> Calculator<'a> {
    pub fn eval(expr: &'a str) -> Result {
        let mut c = Calculator {
            chars: expr.chars(),
            curr: None,
        };
        c.next();
        let result = c.expr()?;
        if c.curr.is_some() {
            Err(CalculatorError::ExpectedEnd)
        } else {
            Ok(result)
        }
    }

    fn next(&mut self) {
        self.curr = self.chars.next();
        while self.curr.is_some_and(|c| c.is_whitespace()) {
            self.curr = self.chars.next();
        }
    }

    fn take(&mut self, expected: char) -> bool {
        if self.curr == Some(expected) {
            self.next();
            true
        } else {
            false
        }
    }

    fn expr(&mut self) -> Result {
        let mut value = self.term()?;
        while let Some(op) = self.curr {
            match op {
                '+' => {
                    self.next();
                    value = value.wrapping_add(self.term()?);
                }
                '-' => {
                    self.next();
                    value = value.wrapping_sub(self.term()?);
                }
                _ => break,
            }
        }
        Ok(value)
    }

    fn term(&mut self) -> Result {
        let mut value = self.unary()?;
        while let Some(op) = self.curr {
            match op {
                '*' => {
                    self.next();
                    value = value.wrapping_mul(self.unary()?);
                }
                '/' => {
                    self.next();
                    let divisor = self.unary()?;
                    if divisor == 0 {
                        return Err(CalculatorError::DivisionByZero);
                    }
                    value = value.wrapping_div(divisor);
                }
                '%' => {
                    self.next();
                    let divisor = self.unary()?;
                    if divisor == 0 {
                        return Err(CalculatorError::DivisionByZero);
                    }
                    value = value.wrapping_rem(divisor);
                }
                _ => break,
            }
        }
        Ok(value)
    }

    fn unary(&mut self) -> Result {
        if self.take('-') {
            Ok(-self.unary()?)
        } else {
            self.primary()
        }
    }

    fn primary(&mut self) -> Result {
        if self.take('(') {
            let val = self.expr()?;
            if !self.take(')') {
                return Err(CalculatorError::MissingClosingParen);
            }
            Ok(val)
        } else {
            self.value()
        }
    }

    fn value(&mut self) -> Result {
        let mut num = 0i64;
        let mut found = false;
        while let Some(c) = self.curr {
            if let Some(d) = c.to_digit(10) {
                num = num * 10 + d as i64;
                self.next();
                found = true;
            } else {
                break;
            }
        }
        if found {
            let k = self.take('s') || self.take('S') || self.take('k') || self.take('K');
            Ok(if k { num * 1000 } else { num })
        } else {
            Err(CalculatorError::ExpectedNumber)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unitless() {
        assert_eq!(Calculator::eval("2 + 2 * 2").unwrap(), 6);
    }

    #[test]
    fn seconds() {
        assert_eq!(Calculator::eval("2s + 2 * 2").unwrap(), 2004);
    }

    #[test]
    fn more_seconds() {
        assert_eq!(Calculator::eval("2 + 2s * 2s").unwrap(), 4000002);
    }
}
