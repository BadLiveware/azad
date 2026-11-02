use serde_json::{Number, Value};

const NEGATIVE_ZERO_TEXT: &str = "-0";
const NAN_TEXT: &str = "NaN";
const INFINITY_TEXT: &str = "Infinity";
const NEGATIVE_INFINITY_TEXT: &str = "-Infinity";
const OBJECT_TEXT: &str = "Object";
const ARRAY_TEXT: &str = "Array";
const SIGNIFICANT_DIGITS: usize = 15;
const SMALLEST_FIXED_EXPONENT: i32 = -4;
const LARGEST_INTEGER_AS_I64: f64 = 9_007_199_254_740_992.0;
const HEX_PREFIX: &str = "0x";
const OCTAL_PREFIX: &str = "0o";

/// GitHub's conversion of a value to text: null -> "", true/false, numbers without a trailing ".0", objects and
/// arrays as "Object" and "Array".
pub fn to_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number_to_text(number.as_f64().unwrap_or(f64::NAN)),
        Value::String(text) => text.clone(),
        Value::Array(_) => ARRAY_TEXT.to_string(),
        Value::Object(_) => OBJECT_TEXT.to_string(),
    }
}

/// GitHub's truthiness: false for null, false, 0, -0, NaN and "", true otherwise.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// A JSON number for a finite float; whole numbers stay integers, so `toJSON(1)` prints `1`.
pub(crate) fn number_value(number: f64) -> Value {
    let is_whole = number.fract() == 0.0 && number.abs() < LARGEST_INTEGER_AS_I64;
    match is_whole {
        true => Value::Number(Number::from(number as i64)),
        false => Number::from_f64(number).map_or(Value::Null, Value::Number),
    }
}

/// .NET's `G15` format, which GitHub uses.
pub(crate) fn number_to_text(number: f64) -> String {
    if number.is_nan() {
        return NAN_TEXT.to_string();
    }
    if number.is_infinite() {
        return match number > 0.0 {
            true => INFINITY_TEXT.to_string(),
            false => NEGATIVE_INFINITY_TEXT.to_string(),
        };
    }
    if number == 0.0 {
        return match number.is_sign_negative() {
            true => NEGATIVE_ZERO_TEXT.to_string(),
            false => "0".to_string(),
        };
    }
    let sign = match number < 0.0 {
        true => "-",
        false => "",
    };
    let (digits, exponent) = significant_digits(number.abs());
    let body = match !(SMALLEST_FIXED_EXPONENT..SIGNIFICANT_DIGITS as i32).contains(&exponent) {
        true => scientific(&digits, exponent),
        false => fixed(&digits, exponent),
    };
    format!("{sign}{body}")
}

fn significant_digits(positive: f64) -> (String, i32) {
    let formatted = format!("{:.*e}", SIGNIFICANT_DIGITS - 1, positive);
    let (mantissa, exponent) = formatted.split_once('e').unwrap_or((&formatted, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let trimmed = digits.trim_end_matches('0');
    let kept = match trimmed.is_empty() {
        true => "0",
        false => trimmed,
    };
    (kept.to_string(), exponent.parse().unwrap_or(0))
}

fn scientific(digits: &str, exponent: i32) -> String {
    let (first, rest) = digits.split_at(1);
    let point_and_rest = match rest.is_empty() {
        true => String::new(),
        false => format!(".{rest}"),
    };
    let exponent_sign = match exponent < 0 {
        true => '-',
        false => '+',
    };
    format!("{first}{point_and_rest}E{exponent_sign}{:02}", exponent.abs())
}

fn fixed(digits: &str, exponent: i32) -> String {
    if exponent < 0 {
        let zeros = "0".repeat((-exponent - 1) as usize);
        return format!("0.{zeros}{digits}");
    }
    let integer_length = exponent as usize + 1;
    if digits.len() <= integer_length {
        let zeros = "0".repeat(integer_length - digits.len());
        return format!("{digits}{zeros}");
    }
    let (integer, fraction) = digits.split_at(integer_length);
    format!("{integer}.{fraction}")
}

/// GitHub's number conversion: null -> 0, true -> 1, false -> 0, a string -> its number or NaN, array or object -> NaN.
pub(crate) fn to_number(value: &Value) -> f64 {
    match value {
        Value::Null => 0.0,
        Value::Bool(flag) => f64::from(u8::from(*flag)),
        Value::Number(number) => number.as_f64().unwrap_or(f64::NAN),
        Value::String(text) => string_to_number(text),
        Value::Array(_) | Value::Object(_) => f64::NAN,
    }
}

pub(crate) fn string_to_number(text: &str) -> f64 {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return 0.0;
    }
    parse_number_text(trimmed).unwrap_or(f64::NAN)
}

/// Parses the number forms of GitHub: decimal, exponent, `0x` hexadecimal, `0o` octal, `Infinity`, with a sign.
pub(crate) fn parse_number_text(text: &str) -> Option<f64> {
    let (negative, unsigned) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let magnitude = parse_unsigned(unsigned)?;
    match negative {
        true => Some(-magnitude),
        false => Some(magnitude),
    }
}

fn parse_unsigned(text: &str) -> Option<f64> {
    if text == INFINITY_TEXT {
        return Some(f64::INFINITY);
    }
    if let Some(digits) = text.strip_prefix(HEX_PREFIX) {
        return parse_radix(digits, 16);
    }
    if let Some(digits) = text.strip_prefix(OCTAL_PREFIX) {
        return parse_radix(digits, 8);
    }
    let first = text.chars().next()?;
    let starts_like_number = first.is_ascii_digit() || first == '.';
    let only_number_characters = text.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'));
    if !starts_like_number || !only_number_characters {
        return None;
    }
    text.parse::<f64>().ok()
}

fn parse_radix(digits: &str, radix: u32) -> Option<f64> {
    u64::from_str_radix(digits, radix).ok().map(|n| n as f64)
}

/// GitHub's `==`: same types compare directly; other types compare as numbers; arrays and objects never match.
pub(crate) fn loosely_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::String(a), Value::String(b)) => a.to_lowercase() == b.to_lowercase(),
        _ => to_number(left) == to_number(right),
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Relation {
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

/// GitHub's `<`, `<=`, `>` and `>=`: two strings compare case-insensitively; other types compare as numbers.
pub(crate) fn relation_holds(left: &Value, right: &Value, relation: Relation) -> bool {
    let ordering = match (left, right) {
        (Value::String(a), Value::String(b)) => Some(a.to_uppercase().cmp(&b.to_uppercase())),
        _ => to_number(left).partial_cmp(&to_number(right)),
    };
    let Some(ordering) = ordering else {
        return false;
    };
    match relation {
        Relation::Less => ordering.is_lt(),
        Relation::LessOrEqual => ordering.is_le(),
        Relation::Greater => ordering.is_gt(),
        Relation::GreaterOrEqual => ordering.is_ge(),
    }
}
