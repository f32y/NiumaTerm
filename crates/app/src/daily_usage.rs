use serde::Deserialize;
use serde_json::from_slice;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TokenCounts {
    #[serde(default)]
    pub(crate) input_tokens: u64,
    #[serde(default)]
    pub(crate) output_tokens: u64,
    #[serde(default)]
    pub(crate) cache_creation_tokens: u64,
    #[serde(default)]
    pub(crate) cache_read_tokens: u64,
    #[serde(default)]
    pub(crate) total_tokens: u64,
}

impl TokenCounts {
    pub(crate) fn total(self) -> u64 {
        if self.total_tokens > 0 {
            self.total_tokens
        } else {
            self.input_tokens
                .saturating_add(self.output_tokens)
                .saturating_add(self.cache_creation_tokens)
                .saturating_add(self.cache_read_tokens)
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelTokenUsage {
    pub(crate) model_name: String,
    #[serde(flatten)]
    pub(crate) counts: TokenCounts,
    #[serde(default, rename = "cost")]
    pub(crate) price_usd: f64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DailyTokenUsage {
    #[serde(alias = "period")]
    pub(crate) date: String,
    #[serde(flatten)]
    pub(crate) counts: TokenCounts,
    #[serde(default, rename = "totalCost")]
    pub(crate) price_usd: f64,
    #[serde(default)]
    pub(crate) model_breakdowns: Vec<ModelTokenUsage>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReportTotals {
    #[serde(flatten)]
    counts: TokenCounts,
    #[serde(default, rename = "totalCost")]
    price_usd: f64,
}

#[derive(Deserialize)]
struct CcusageReport {
    #[serde(default)]
    daily: Vec<DailyTokenUsage>,
    #[serde(default)]
    totals: ReportTotals,
}

pub(crate) fn parse_usage(bytes: &[u8], date: &str) -> Result<DailyTokenUsage, String> {
    let mut report: CcusageReport =
        from_slice(bytes).map_err(|err| format!("ccusage output is not valid JSON: {err}"))?;

    if let Some(usage) = report.daily.drain(..).find(|usage| usage.date == date) {
        return Ok(usage);
    }

    Ok(DailyTokenUsage {
        date: date.to_string(),
        counts: report.totals.counts,
        price_usd: report.totals.price_usd,
        model_breakdowns: Vec::new(),
    })
}
