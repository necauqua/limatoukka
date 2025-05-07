use thiserror::Error;

pub struct Calculator<'a> {
    chars: std::str::Chars<'a>,
    curr: Option<char>,
}

#[derive(Debug, Error)]
pub enum CalculatorError {
    #[error("Expected end of expression")]
    ExpectedEnd,
    #[error("Expected a number")]
    ExpectedNumber,
    #[error("Division by zero")]
    DivisionByZero,
    #[error("Missing ')'")]
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
                    value += self.term()?;
                }
                '-' => {
                    self.next();
                    value -= self.term()?;
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
                    value *= self.unary()?;
                }
                '/' => {
                    self.next();
                    let divisor = self.unary()?;
                    if divisor == 0 {
                        return Err(CalculatorError::DivisionByZero);
                    }
                    value /= divisor;
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
            self.number()
        }
    }

    fn number(&mut self) -> Result {
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
            Ok(num)
        } else {
            Err(CalculatorError::ExpectedNumber)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test() {
        assert_eq!(Calculator::eval("2 + 2 * 2").unwrap(), 6);
    }
}
