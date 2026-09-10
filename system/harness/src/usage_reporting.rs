//! Deterministic read models for the durable usage ledger.
//!
//! This module has no provider, file, or database side effects.  It deliberately
//! separates measured tokens from standard-rate credit comparisons and never
//! calls an estimate an invoice or an account debit.

use crate::usage_ledger::{Coverage, UsageRow};
use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, BTreeSet};

pub const AUDITED_RATE_VERSION: &str = "codex-standard-2026-09-10";
pub const AUDITED_RATE_SOURCE: &str =
    "https://learn.chatgpt.com/docs/pricing (retrieved 2026-09-10)";

/// A signed integer amount in micro-units.  For the audited card, one unit is
/// one millionth of a modeled ChatGPT credit equivalent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct MicroUnits(pub i128);

impl MicroUnits {
    pub fn whole_and_fraction(self) -> (i128, i128) {
        (self.0 / 1_000_000, self.0.abs() % 1_000_000)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EstimateUnit {
    CreditEquivalent,
    ApiUsd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenRates {
    /// Micro-units per million tokens.  This makes `tokens * rate` exact.
    pub fresh: i64,
    pub cached: i64,
    pub output: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateFact {
    pub model: &'static str,
    pub rates: TokenRates,
}

/// The audited source defines standard-rate credit equivalents, not API USD.
pub const AUDITED_CREDIT_RATES: &[RateFact] = &[
    RateFact {
        model: "gpt-6-astra",
        rates: TokenRates {
            fresh: 250,
            cached: 25,
            output: 1250,
        },
    },
    RateFact {
        model: "gpt-5.6-sol",
        rates: TokenRates {
            fresh: 100,
            cached: 10,
            output: 500,
        },
    },
    RateFact {
        model: "gpt-5.6-terra",
        rates: TokenRates {
            fresh: 50,
            cached: 5,
            output: 300,
        },
    },
    RateFact {
        model: "gpt-5.6-luna",
        rates: TokenRates {
            fresh: 5,
            cached: 1,
            output: 30,
        },
    },
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MeasuredTokens {
    pub responses: u64,
    pub input: i128,
    pub cached_input: i128,
    pub uncached_input: i128,
    pub output: i128,
}

impl MeasuredTokens {
    pub fn total(&self) -> i128 {
        self.input + self.output
    }
    fn add(&mut self, row: &UsageRow) {
        let input = row.input_tokens.unwrap() as i128;
        let cached = row.cached_input_tokens.unwrap() as i128;
        let output = row.output_tokens.unwrap() as i128;
        self.responses += 1;
        self.input += input;
        self.cached_input += cached;
        self.uncached_input += input - cached;
        self.output += output;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Estimate {
    pub unit: EstimateUnit,
    pub rate_version: &'static str,
    pub rate_source: &'static str,
    pub value: MicroUnits,
    pub incomplete: bool,
    pub labels: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contributor {
    pub key: String,
    pub measured: MeasuredTokens,
    pub response_ids: Vec<String>,
    pub credits: Estimate,
}

#[derive(Debug, PartialEq, Eq)]
pub struct UsageReport {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub measured: MeasuredTokens,
    pub modeled_credits: Estimate,
    /// Always unavailable for the audited credit-only rate facts.
    pub modeled_api_usd: Estimate,
    /// The ledger has no provider statement/debit interface.
    pub actual_billed_debited: Estimate,
    pub by_model: Vec<Contributor>,
    pub by_family: Vec<Contributor>,
    /// Child-response tokens divided by all measured response tokens, in
    /// millionths. Children remain leaf rows and are never added to the total.
    pub child_coordination_share_millionths: Option<u64>,
    pub child_coordination: Contributor,
    pub preceding_measured: MeasuredTokens,
    pub preceding_change_tokens: Option<i128>,
    pub incomplete_labels: BTreeSet<String>,
    pub coverage: Coverage,
}

pub fn report(
    rows: &[UsageRow],
    coverage: Coverage,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> UsageReport {
    let previous_start = start - (end - start);
    let mut current = Accumulator::default();
    let mut previous = Accumulator::default();
    for row in rows {
        match parse_time(row) {
            Some(time) if time >= start && time < end => current.add(row),
            Some(time) if time >= previous_start && time < start => previous.add(row),
            _ => {}
        }
    }
    let mut labels = current.labels.clone();
    if coverage.conflicts > 0 {
        labels.insert("conflicting_usage_records".into());
    }
    if coverage.quarantined > 0 {
        labels.insert("quarantined_records".into());
    }
    if coverage.pending_sources > 0 {
        labels.insert("source_backlog".into());
    }
    if coverage.stale_sources > 0 {
        labels.insert("stale_source".into());
    }
    let current_total = current.measured.total();
    let child_coordination = current.child_contributor();
    let child_coordination_share_millionths = if current_total > 0 {
        Some(((child_coordination.measured.total() * 1_000_000) / current_total) as u64)
    } else {
        None
    };
    UsageReport {
        start,
        end,
        measured: current.measured.clone(),
        modeled_credits: current.estimate(EstimateUnit::CreditEquivalent),
        modeled_api_usd: unavailable(
            EstimateUnit::ApiUsd,
            "api_usd_rate_not_defined_by_audited_facts",
        ),
        actual_billed_debited: unavailable(
            EstimateUnit::ApiUsd,
            "actual_billed_or_debited_unavailable",
        ),
        by_model: current.contributors(&current.models),
        by_family: current.contributors(&current.families),
        child_coordination_share_millionths,
        child_coordination,
        preceding_measured: previous.measured.clone(),
        preceding_change_tokens: if !previous.invalid_usage {
            Some(current_total - previous.measured.total())
        } else {
            None
        },
        incomplete_labels: labels,
        coverage,
    }
}

#[derive(Default)]
struct Bucket {
    measured: MeasuredTokens,
    rows: Vec<UsageRow>,
    labels: BTreeSet<String>,
}

#[derive(Default)]
struct Accumulator {
    measured: MeasuredTokens,
    rows: Vec<UsageRow>,
    labels: BTreeSet<String>,
    invalid_usage: bool,
    models: BTreeMap<String, Bucket>,
    families: BTreeMap<String, Bucket>,
    children: Bucket,
}

impl Accumulator {
    fn add(&mut self, row: &UsageRow) {
        if !complete(row) {
            self.labels.insert("missing_or_invalid_usage".into());
            self.invalid_usage = true;
            return;
        }
        self.measured.add(row);
        self.rows.push(row.clone());
        let model = row.model.clone().unwrap_or_else(|| "unknown".into());
        let family = row
            .root_task_family
            .clone()
            .unwrap_or_else(|| "unknown".into());
        add_bucket(self.models.entry(model).or_default(), row);
        add_bucket(self.families.entry(family).or_default(), row);
        if row.parent_response_id.is_some() {
            add_bucket(&mut self.children, row);
        }
        if row.model.is_none() {
            self.labels.insert("unknown_model".into());
        }
        // The frozen ledger has no speed field. These are standard-rate comparisons,
        // so a speed multiplier cannot be represented as an account estimate.
        self.labels
            .insert("speed_unavailable_standard_rate_only".into());
    }
    fn estimate(&self, unit: EstimateUnit) -> Estimate {
        estimate_rows(&self.rows, unit)
    }
    fn contributors(&self, buckets: &BTreeMap<String, Bucket>) -> Vec<Contributor> {
        let mut out: Vec<_> = buckets
            .iter()
            .map(|(key, bucket)| contributor(key, bucket))
            .collect();
        out.sort_by(|a, b| {
            b.measured
                .total()
                .cmp(&a.measured.total())
                .then_with(|| a.key.cmp(&b.key))
        });
        out
    }
    fn child_contributor(&self) -> Contributor {
        contributor("child_responses", &self.children)
    }
}

fn add_bucket(bucket: &mut Bucket, row: &UsageRow) {
    bucket.measured.add(row);
    bucket.rows.push(row.clone());
    if row.model.is_none() {
        bucket.labels.insert("unknown_model".into());
    }
}
fn contributor(key: &str, bucket: &Bucket) -> Contributor {
    let mut ids: Vec<_> = bucket
        .rows
        .iter()
        .map(|row| row.response_id.clone())
        .collect();
    ids.sort();
    Contributor {
        key: key.into(),
        measured: bucket.measured.clone(),
        response_ids: ids,
        credits: estimate_rows(&bucket.rows, EstimateUnit::CreditEquivalent),
    }
}
fn complete(row: &UsageRow) -> bool {
    matches!((row.input_tokens, row.cached_input_tokens, row.output_tokens), (Some(input), Some(cached), Some(output)) if input >= cached && cached >= 0 && output >= 0)
}
fn parse_time(row: &UsageRow) -> Option<DateTime<Utc>> {
    row.event_at.as_ref()?.parse::<DateTime<Utc>>().ok()
}
fn rate(model: &str) -> Option<TokenRates> {
    AUDITED_CREDIT_RATES
        .iter()
        .find(|r| r.model == model)
        .map(|r| r.rates)
}
fn estimate_rows(rows: &[UsageRow], unit: EstimateUnit) -> Estimate {
    if unit == EstimateUnit::ApiUsd {
        return unavailable(unit, "api_usd_rate_not_defined_by_audited_facts");
    }
    let mut value = MicroUnits(0);
    let mut labels = BTreeSet::new();
    for row in rows {
        let Some(model) = row.model.as_deref() else {
            labels.insert("unknown_model_rate".into());
            continue;
        };
        let Some(rates) = rate(model) else {
            labels.insert(format!("unknown_model_rate:{model}"));
            continue;
        };
        if !complete(row) {
            labels.insert("missing_or_invalid_usage".into());
            continue;
        }
        let input = row.input_tokens.unwrap() as i128;
        let cached = row.cached_input_tokens.unwrap() as i128;
        value.0 += (input - cached) * rates.fresh as i128
            + cached * rates.cached as i128
            + row.output_tokens.unwrap() as i128 * rates.output as i128;
    }
    // No speed appears in UsageRow. The audited facts support a standard-rate
    // comparison but not a speed-adjusted provider/account amount.
    labels.insert("speed_unavailable_standard_rate_only".into());
    Estimate {
        unit,
        rate_version: AUDITED_RATE_VERSION,
        rate_source: AUDITED_RATE_SOURCE,
        value,
        incomplete: !labels.is_empty(),
        labels,
    }
}
fn unavailable(unit: EstimateUnit, label: &str) -> Estimate {
    let mut labels = BTreeSet::new();
    labels.insert(label.into());
    Estimate {
        unit,
        rate_version: AUDITED_RATE_VERSION,
        rate_source: AUDITED_RATE_SOURCE,
        value: MicroUnits(0),
        incomplete: true,
        labels,
    }
}
