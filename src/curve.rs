// Copyright (c) 2026 Pegasus Heavy Industries LLC
// Licensed under the MIT License

//! Fan curve definitions and interpolation.
//!
//! A curve maps temperature readings to PWM duty values (0-255).
//! Points are linearly interpolated between defined thresholds.

use serde::{Deserialize, Serialize};

/// A single point on a fan curve.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct CurvePoint {
    /// Temperature in degrees Celsius
    pub temp_c: f64,
    /// PWM duty value (0-255)
    pub pwm: u8,
}

/// A named fan curve with an ordered list of temperature-to-PWM points.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FanCurve {
    /// Unique name for this curve
    pub name: String,
    /// Points sorted by ascending temperature.
    /// Must have at least 2 points.
    pub points: Vec<CurvePoint>,
}

impl FanCurve {
    /// Create a new fan curve. Points are sorted by temperature automatically.
    pub fn new(name: String, mut points: Vec<CurvePoint>) -> Self {
        points.sort_by(|a, b| a.temp_c.partial_cmp(&b.temp_c).unwrap());
        Self { name, points }
    }

    /// Interpolate the PWM value for a given temperature.
    ///
    /// - Below the lowest point: returns the lowest point's PWM
    /// - Above the highest point: returns the highest point's PWM
    /// - Between two points: linear interpolation
    pub fn interpolate(&self, temp_c: f64) -> u8 {
        if self.points.is_empty() {
            return 0;
        }
        if self.points.len() == 1 || temp_c <= self.points[0].temp_c {
            return self.points[0].pwm;
        }

        let last = &self.points[self.points.len() - 1];
        if temp_c >= last.temp_c {
            return last.pwm;
        }

        // Find the two surrounding points
        for window in self.points.windows(2) {
            let lo = &window[0];
            let hi = &window[1];

            if temp_c >= lo.temp_c && temp_c <= hi.temp_c {
                let range_t = hi.temp_c - lo.temp_c;
                if range_t == 0.0 {
                    return lo.pwm;
                }
                let frac = (temp_c - lo.temp_c) / range_t;
                let pwm_f = lo.pwm as f64 + frac * (hi.pwm as f64 - lo.pwm as f64);
                return pwm_f.round().clamp(0.0, 255.0) as u8;
            }
        }

        last.pwm
    }

    /// Validate the curve has at least 2 points and PWM values are in range.
    pub fn validate(&self) -> Result<(), String> {
        if self.points.len() < 2 {
            return Err("Curve must have at least 2 points".to_string());
        }
        for (i, p) in self.points.iter().enumerate() {
            if i > 0 && p.temp_c <= self.points[i - 1].temp_c {
                return Err(format!(
                    "Points must have strictly increasing temperatures (point {i})"
                ));
            }
        }
        Ok(())
    }
}

/// A default "silent" curve: low speed until 50C, ramp up to full at 90C.
pub fn default_silent_curve() -> FanCurve {
    FanCurve::new(
        "silent".to_string(),
        vec![
            CurvePoint { temp_c: 30.0, pwm: 0 },
            CurvePoint { temp_c: 50.0, pwm: 64 },
            CurvePoint { temp_c: 70.0, pwm: 153 },
            CurvePoint { temp_c: 80.0, pwm: 204 },
            CurvePoint { temp_c: 90.0, pwm: 255 },
        ],
    )
}

/// A default "performance" curve: always some airflow, aggressive ramp.
pub fn default_performance_curve() -> FanCurve {
    FanCurve::new(
        "performance".to_string(),
        vec![
            CurvePoint { temp_c: 30.0, pwm: 64 },
            CurvePoint { temp_c: 50.0, pwm: 128 },
            CurvePoint { temp_c: 65.0, pwm: 204 },
            CurvePoint { temp_c: 75.0, pwm: 255 },
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interpolation_below_range() {
        let curve = default_silent_curve();
        assert_eq!(curve.interpolate(10.0), 0);
    }

    #[test]
    fn test_interpolation_above_range() {
        let curve = default_silent_curve();
        assert_eq!(curve.interpolate(100.0), 255);
    }

    #[test]
    fn test_interpolation_exact_point() {
        let curve = default_silent_curve();
        assert_eq!(curve.interpolate(50.0), 64);
    }

    #[test]
    fn test_interpolation_midpoint() {
        let curve = FanCurve::new(
            "test".to_string(),
            vec![
                CurvePoint { temp_c: 0.0, pwm: 0 },
                CurvePoint { temp_c: 100.0, pwm: 200 },
            ],
        );
        assert_eq!(curve.interpolate(50.0), 100);
    }

    #[test]
    fn test_validation_too_few_points() {
        let curve = FanCurve::new(
            "bad".to_string(),
            vec![CurvePoint { temp_c: 50.0, pwm: 128 }],
        );
        assert!(curve.validate().is_err());
    }
}
