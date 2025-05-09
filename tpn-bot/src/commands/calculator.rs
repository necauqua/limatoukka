use std::fmt::{self, Debug};

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

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Unit {
    None,
    Seconds,
}

impl Unit {
    pub fn combine(self, other: Self) -> std::result::Result<Self, CalculatorError> {
        match (self, other) {
            (a, Unit::None) => Ok(a),
            (Unit::None, b) => Ok(b),
            (Self::Seconds, Self::Seconds) => Ok(Self::Seconds),
        }
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub struct Value {
    pub magnitude: i64,
    pub unit: Unit,
}

impl Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.unit {
            Unit::None => write!(f, "{}", self.magnitude),
            Unit::Seconds => write!(f, "{}s", self.magnitude),
        }
    }
}

impl Value {
    pub fn new(magnitude: i64, unit: Unit) -> Self {
        Self { magnitude, unit }
    }

    pub fn binary(self, rhs: Self, f: fn(i64, i64) -> i64) -> Result {
        Ok(Self {
            magnitude: f(self.magnitude, rhs.magnitude),
            unit: self.unit.combine(rhs.unit)?,
        })
    }
}

type Result = std::result::Result<Value, CalculatorError>;

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
                    value = value.binary(self.term()?, i64::wrapping_add)?;
                }
                '-' => {
                    self.next();
                    value = value.binary(self.term()?, i64::wrapping_sub)?;
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
                    value = value.binary(self.unary()?, i64::wrapping_mul)?;
                }
                '/' => {
                    self.next();
                    let divisor = self.unary()?;
                    if divisor.magnitude == 0 {
                        return Err(CalculatorError::DivisionByZero);
                    }
                    value = value.binary(self.unary()?, i64::wrapping_div)?;
                }
                '%' => {
                    self.next();
                    let divisor = self.unary()?;
                    if divisor.magnitude == 0 {
                        return Err(CalculatorError::DivisionByZero);
                    }
                    value = value.binary(divisor, i64::wrapping_rem)?;
                }
                _ => break,
            }
        }
        Ok(value)
    }

    fn unary(&mut self) -> Result {
        if self.take('-') {
            let term = self.unary()?;
            Ok(Value {
                magnitude: -term.magnitude,
                unit: term.unit,
            })
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
            Ok(Value {
                magnitude: num,
                unit: if self.take('s') {
                    Unit::Seconds
                } else {
                    Unit::None
                },
            })
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
        assert_eq!(
            Calculator::eval("2 + 2 * 2").unwrap(),
            Value::new(6, Unit::None)
        );
    }

    #[test]
    fn seconds() {
        assert_eq!(
            Calculator::eval("2s + 2 * 2").unwrap(),
            Value::new(6, Unit::Seconds)
        );
    }

    #[test]
    fn more_seconds() {
        assert_eq!(
            Calculator::eval("2 + 2s * 2s").unwrap(),
            Value::new(6, Unit::Seconds)
        );
    }
}
