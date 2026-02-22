//! Floating point precision handling
//!
//! Avoids ugly values like 13.000000001 by rounding to appropriate precision.

/// Determine appropriate decimal places from scale factor
///
/// # Examples
/// - scale 1.0 → 0 decimal places (integers)
/// - scale 0.1 → 1 decimal place
/// - scale 0.01 → 2 decimal places
/// - scale 0.25 → 2 decimal places (1/4 needs 2 places)
/// - scale 0.001 → 3 decimal places
pub fn precision_from_scale(scale: f64) -> u8 {
    if scale <= 0.0 {
        return 4; // Safe default
    }

    let abs_scale = scale.abs();

    // Count decimal places by finding how many digits after the decimal point
    // are needed to represent the scale exactly
    if abs_scale >= 1.0 {
        return 0;
    }

    // For values like 0.25, 0.125, etc., we need to count actual decimals needed
    // Scale by 10 until we get an integer (or close enough)
    let mut temp = abs_scale;
    let mut precision = 0u8;

    while precision < 6 {
        if (temp - temp.round()).abs() < 1e-9 {
            break;
        }
        temp *= 10.0;
        precision += 1;
    }

    precision
}

/// Round a value to the specified number of decimal places
pub fn round_to_precision(value: f64, precision: u8) -> f64 {
    if precision == 0 {
        value.round()
    } else {
        let factor = 10_f64.powi(precision as i32);
        (value * factor).round() / factor
    }
}

/// Round a value based on the scale factor used to produce it
///
/// This is the main function to use for display values.
pub fn round_for_scale(value: f64, scale: f64) -> f64 {
    let precision = precision_from_scale(scale);
    round_to_precision(value, precision)
}

/// Format a value as a clean JSON number
///
/// Ensures we don't get ugly representations like 1.4000000000000001
pub fn to_json_number(value: f64, scale: f64) -> serde_json::Value {
    let rounded = round_for_scale(value, scale);

    // Check if it's effectively an integer
    if (rounded - rounded.round()).abs() < f64::EPSILON {
        let int_val = rounded.round() as i64;
        // Use integer representation if it fits cleanly
        if (int_val as f64 - rounded).abs() < f64::EPSILON {
            return serde_json::json!(int_val);
        }
    }

    serde_json::json!(rounded)
}

/// Format an array of values as clean JSON numbers
pub fn to_json_array(values: &[f64], scale: f64) -> serde_json::Value {
    serde_json::Value::Array(values.iter().map(|&v| to_json_number(v, scale)).collect())
}

/// Format a 2D matrix of values as clean JSON
pub fn to_json_matrix(values: &[Vec<f64>], scale: f64) -> serde_json::Value {
    serde_json::Value::Array(values.iter().map(|row| to_json_array(row, scale)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_precision_from_scale() {
        assert_eq!(precision_from_scale(1.0), 0);
        assert_eq!(precision_from_scale(10.0), 0);
        assert_eq!(precision_from_scale(0.5), 1);
        assert_eq!(precision_from_scale(0.1), 1);
        assert_eq!(precision_from_scale(0.25), 2);
        assert_eq!(precision_from_scale(0.01), 2);
        assert_eq!(precision_from_scale(0.001), 3);
        assert_eq!(precision_from_scale(0.0001), 4);
    }

    #[test]
    fn test_round_to_precision() {
        assert_eq!(round_to_precision(1.234567, 0), 1.0);
        assert_eq!(round_to_precision(1.234567, 1), 1.2);
        assert_eq!(round_to_precision(1.234567, 2), 1.23);
        assert_eq!(round_to_precision(1.234567, 3), 1.235);
    }

    #[test]
    fn test_round_for_scale() {
        // scale 0.01 → 2 decimal places
        assert_eq!(round_for_scale(1.4000000000001, 0.01), 1.4);
        assert_eq!(round_for_scale(13.000000000001, 0.01), 13.0);

        // scale 0.1 → 1 decimal place
        assert_eq!(round_for_scale(1.45000001, 0.1), 1.5);

        // scale 1.0 → integer
        assert_eq!(round_for_scale(92.0000001, 1.0), 92.0);
    }

    #[test]
    fn test_to_json_number() {
        // Integer values should come out clean
        let v = to_json_number(92.0, 1.0);
        assert_eq!(v, serde_json::json!(92));

        // Fractional values should be rounded
        let v = to_json_number(1.4000000001, 0.01);
        assert_eq!(v, serde_json::json!(1.4));

        // Verify no ugly decimals
        let v = to_json_number(140.0 * 0.01, 0.01);
        assert_eq!(v, serde_json::json!(1.4));
    }

    #[test]
    fn test_to_json_array() {
        let values = vec![1.0, 1.1, 1.2, 1.3];
        let actual: Vec<f64> = values.iter().map(|&v| v * 100.0 * 0.01).collect();
        let json = to_json_array(&actual, 0.01);
        // Note: 1.0 comes out as integer 1, not 1.0
        assert_eq!(json, serde_json::json!([1, 1.1, 1.2, 1.3]));
    }
}
