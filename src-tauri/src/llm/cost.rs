//! Cost estimates from token usage and the user-editable price table (§6.4).
//! Code does the arithmetic, never the model (principle 5).

use serde::{Deserialize, Serialize};

use super::Usage;

/// US dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Price {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

/// `None` when there is no price or the response did not report usage.
pub fn estimate(usage: &Usage, price: Option<Price>) -> Option<f64> {
    let price = price?;
    let input = usage.input_tokens? as f64;
    let output = usage.output_tokens? as f64;
    Some((input * price.input_per_mtok + output * price.output_per_mtok) / 1_000_000.0)
}

/// True once spending has reached the cap. No cap means no limit.
pub fn cap_reached(spent_usd: f64, cap_usd: Option<f64>) -> bool {
    cap_usd.is_some_and(|cap| spent_usd >= cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_from_usage_and_price() {
        let usage = Usage {
            input_tokens: Some(2_000),
            output_tokens: Some(500),
        };
        let price = Price {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
        };
        let cost = estimate(&usage, Some(price)).unwrap();
        assert!((cost - 0.0135).abs() < 1e-12);
    }

    #[test]
    fn unknown_price_or_usage_gives_no_estimate() {
        let usage = Usage {
            input_tokens: Some(10),
            output_tokens: None,
        };
        assert_eq!(
            estimate(
                &usage,
                Some(Price {
                    input_per_mtok: 1.0,
                    output_per_mtok: 1.0
                })
            ),
            None
        );
        assert_eq!(
            estimate(
                &Usage {
                    input_tokens: Some(1),
                    output_tokens: Some(1)
                },
                None
            ),
            None
        );
    }

    #[test]
    fn cap() {
        assert!(!cap_reached(49.99, Some(50.0)));
        assert!(cap_reached(50.0, Some(50.0)));
        assert!(!cap_reached(1e9, None));
    }
}
