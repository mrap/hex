//! Deterministic read models for the durable usage ledger.
//!
//! This module has no provider, file, or database side effects.  It deliberately
//! separates measured tokens from standard-rate credit comparisons and never
//! calls an estimate an invoice or an account debit.

use crate::usage_ledger::{Coverage, UsageRow};
use crate::usage_ledger::UsageSummaryGroup;
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
    /// Provider-reported cache writes. This is separate from input.
    pub cache_write_input: i128,
    pub output: i128,
    /// Provider-reported reasoning. This is a subset of output, never additive.
    pub reasoning_output: i128,
    /// Provider-reported total, available only when every included row supplied it.
    pub provider_total: Option<i128>,
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
        if let Some(cache_write) = row.cache_write_input_tokens {
            self.cache_write_input += cache_write as i128;
        }
        self.output += output;
        if let Some(reasoning) = row.reasoning_output_tokens {
            self.reasoning_output += reasoning as i128;
        }
        self.provider_total = match (self.responses, row.total_tokens, self.provider_total) {
            (1, Some(total), _) => Some(total as i128),
            (_, Some(total), Some(sum)) => Some(sum + total as i128),
            _ => None,
        };
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CreditComponents {
    pub fresh: MicroUnits,
    pub cached: MicroUnits,
    pub output: MicroUnits,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contributor {
    pub key: String,
    pub measured: MeasuredTokens,
    pub response_ids: Vec<String>,
    pub credits: Estimate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContributorDimension {
    Model,
    Family,
    Child,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContributorDetail {
    pub dimension: ContributorDimension,
    pub key: String,
    pub total_matches: usize,
    pub offset: usize,
    pub response_ids: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct UsageReport {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub measured: MeasuredTokens,
    pub modeled_credits: Estimate,
    pub modeled_credit_components: CreditComponents,
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
    report_inner(rows, coverage, start, end, true)
}

/// Produces the default bounded summary. Contributor IDs remain available via
/// [`contributor_detail`], so the summary never serializes an unbounded ID list.
pub fn report_summary(
    rows: &[UsageRow],
    coverage: Coverage,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> UsageReport {
    let mut accumulator = ReportAccumulator::new(coverage, start, end, false);
    accumulator.extend(rows);
    accumulator.finish()
}

/// Incrementally builds a summary from bounded ledger pages. Rows outside the
/// current or immediately preceding half-open windows are ignored, so callers
/// never need to retain an unbounded result set.
pub struct ReportAccumulator {
    coverage: Coverage,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    previous_start: DateTime<Utc>,
    include_ids: bool,
    current: Accumulator,
    previous: Accumulator,
}

impl ReportAccumulator {
    pub fn new(
        coverage: Coverage,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        include_ids: bool,
    ) -> Self {
        Self {
            coverage,
            start,
            end,
            previous_start: start - (end - start),
            include_ids,
            current: Accumulator::default(),
            previous: Accumulator::default(),
        }
    }

    pub fn extend(&mut self, rows: &[UsageRow]) {
        for row in rows {
            match parse_time(row) {
                Some(time) if time >= self.start && time < self.end => {
                    self.current.add(row, self.include_ids)
                }
                Some(time) if time >= self.previous_start && time < self.start => {
                    self.previous.add(row, false)
                }
                _ => {}
            }
        }
    }
    pub fn extend_summary_groups(&mut self, groups: &[UsageSummaryGroup], preceding: bool) {
        for group in groups {
            let target = if preceding { &mut self.previous } else { &mut self.current };
            target.add_group(group, self.include_ids);
        }
    }

    /// Summary callers pass `include_ids = false`, so page rows do not remain
    /// owned after aggregation. This exposes the retained optional detail IDs
    /// for a deterministic bounded-memory regression test.
    pub fn retained_response_ids(&self) -> usize {
        let ids = |accumulator: &Accumulator| {
            accumulator.models.values().map(|bucket| bucket.response_ids.len()).sum::<usize>()
                + accumulator.families.values().map(|bucket| bucket.response_ids.len()).sum::<usize>()
                + accumulator.children.response_ids.len()
        };
        ids(&self.current) + ids(&self.previous)
    }

    pub fn finish(self) -> UsageReport {
        report_from_accumulators(
            self.coverage,
            self.start,
            self.end,
            self.current,
            self.previous,
        )
    }
}

fn report_inner(
    rows: &[UsageRow],
    coverage: Coverage,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    include_ids: bool,
) -> UsageReport {
    let mut accumulator = ReportAccumulator::new(coverage, start, end, include_ids);
    accumulator.extend(rows);
    accumulator.finish()
}

fn report_from_accumulators(
    coverage: Coverage,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    current: Accumulator,
    previous: Accumulator,
) -> UsageReport {
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
        modeled_credit_components: current.credits.components.clone(),
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
    credits: CreditAccumulator,
    response_ids: Vec<String>,
    labels: BTreeSet<String>,
}

#[derive(Default)]
struct Accumulator {
    measured: MeasuredTokens,
    credits: CreditAccumulator,
    labels: BTreeSet<String>,
    invalid_usage: bool,
    models: BTreeMap<String, Bucket>,
    families: BTreeMap<String, Bucket>,
    children: Bucket,
}

impl Accumulator {
    fn add_group(&mut self, g: &UsageSummaryGroup, include_ids: bool) {
        if g.invalid > 0 { self.labels.insert("missing_or_invalid_usage".into()); self.invalid_usage = true; }
        if g.complete == 0 { return; }
        let add = |m: &mut MeasuredTokens| { let first=m.responses==0; m.responses+=g.complete as u64; m.input+=g.input as i128; m.cached_input+=g.cached as i128; m.uncached_input+=(g.input-g.cached) as i128; m.output+=g.output as i128; m.cache_write_input+=g.cache_write as i128; m.reasoning_output+=g.reasoning as i128; m.provider_total=if g.missing_provider_total>0 {None} else if first {Some(g.provider_total as i128)} else {m.provider_total.map(|v|v+g.provider_total as i128)}; };
        add(&mut self.measured); self.credits.add_summary(g);
        let model=g.model.clone().unwrap_or_else(||"unknown".into()); let family=g.family.clone().unwrap_or_else(||"unknown".into());
        add_bucket_group(self.models.entry(model).or_default(),g,include_ids); add_bucket_group(self.families.entry(family).or_default(),g,include_ids); if g.child { add_bucket_group(&mut self.children,g,include_ids); }
        if g.model.is_none(){self.labels.insert("unknown_model".into());} if g.missing_cache_write>0 {self.labels.insert("missing_cache_write_input_tokens".into());} if g.missing_reasoning>0 {self.labels.insert("missing_reasoning_output_tokens".into());} if g.missing_provider_total>0 {self.labels.insert("missing_provider_total_tokens".into());} self.labels.insert("speed_unavailable_standard_rate_only".into());
    }
    fn add(&mut self, row: &UsageRow, include_ids: bool) {
        if !complete(row) {
            self.labels.insert("missing_or_invalid_usage".into());
            self.invalid_usage = true;
            return;
        }
        self.measured.add(row);
        self.credits.add(row);
        let model = row.model.clone().unwrap_or_else(|| "unknown".into());
        let family = row
            .root_task_family
            .clone()
            .unwrap_or_else(|| "unknown".into());
        add_bucket(self.models.entry(model).or_default(), row, include_ids);
        add_bucket(self.families.entry(family).or_default(), row, include_ids);
        if row.parent_response_id.is_some() {
            add_bucket(&mut self.children, row, include_ids);
        }
        if row.model.is_none() {
            self.labels.insert("unknown_model".into());
        }
        if row.cache_write_input_tokens.is_none() {
            self.labels
                .insert("missing_cache_write_input_tokens".into());
        }
        if row.reasoning_output_tokens.is_none() {
            self.labels.insert("missing_reasoning_output_tokens".into());
        }
        if row.total_tokens.is_none() {
            self.labels.insert("missing_provider_total_tokens".into());
        }
        // The frozen ledger has no speed field. These are standard-rate comparisons,
        // so a speed multiplier cannot be represented as an account estimate.
        self.labels
            .insert("speed_unavailable_standard_rate_only".into());
    }
    fn estimate(&self, unit: EstimateUnit) -> Estimate {
        self.credits.estimate(unit)
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
fn add_bucket_group(bucket:&mut Bucket,g:&UsageSummaryGroup,_:bool){ if g.complete==0{return}; let first=bucket.measured.responses==0; bucket.measured.responses+=g.complete as u64; bucket.measured.input+=g.input as i128; bucket.measured.cached_input+=g.cached as i128; bucket.measured.uncached_input+=(g.input-g.cached) as i128; bucket.measured.output+=g.output as i128; bucket.measured.cache_write_input+=g.cache_write as i128; bucket.measured.reasoning_output+=g.reasoning as i128; bucket.measured.provider_total=if g.missing_provider_total>0{None}else if first{Some(g.provider_total as i128)}else{bucket.measured.provider_total.map(|v|v+g.provider_total as i128)}; bucket.credits.add_summary(g); }

fn add_bucket(bucket: &mut Bucket, row: &UsageRow, include_ids: bool) {
    bucket.measured.add(row);
    bucket.credits.add(row);
    if include_ids {
        bucket.response_ids.push(row.response_id.clone());
    }
    if row.model.is_none() {
        bucket.labels.insert("unknown_model".into());
    }
}

/// Returns one stable, bounded page of IDs for a current-window contributor.
/// The caller supplies an explicit dimension and key, so model and family names
/// cannot be confused. Child detail uses the fixed `child_responses` key.
pub fn contributor_detail(
    rows: &[UsageRow],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    dimension: ContributorDimension,
    key: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<ContributorDetail, &'static str> {
    let key = match dimension {
        ContributorDimension::Child => "child_responses",
        ContributorDimension::Model | ContributorDimension::Family => {
            key.ok_or("detail key is required")?
        }
    };
    let mut ids = rows
        .iter()
        .filter(|row| {
            matches!(parse_time(row), Some(time) if time >= start && time < end)
                && complete(row)
                && match dimension {
                    ContributorDimension::Model => row.model.as_deref().unwrap_or("unknown") == key,
                    ContributorDimension::Family => {
                        row.root_task_family.as_deref().unwrap_or("unknown") == key
                    }
                    ContributorDimension::Child => row.parent_response_id.is_some(),
                }
        })
        .map(|row| row.response_id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    let total_matches = ids.len();
    Ok(ContributorDetail {
        dimension,
        key: key.into(),
        total_matches,
        offset,
        response_ids: ids.into_iter().skip(offset).take(limit).collect(),
    })
}
fn contributor(key: &str, bucket: &Bucket) -> Contributor {
    let mut ids = bucket.response_ids.clone();
    ids.sort();
    Contributor {
        key: key.into(),
        measured: bucket.measured.clone(),
        response_ids: ids,
        credits: bucket.credits.estimate(EstimateUnit::CreditEquivalent),
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
#[derive(Default)]
struct CreditAccumulator {
    components: CreditComponents,
    labels: BTreeSet<String>,
}

impl CreditAccumulator {
    fn add_summary(&mut self,g:&UsageSummaryGroup){ let Some(model)=g.model.as_deref() else {self.labels.insert("unknown_model_rate".into());return}; let Some(r)=rate(model) else {self.labels.insert(format!("unknown_model_rate:{model}"));return}; self.components.fresh.0+=(g.input-g.cached) as i128*r.fresh as i128; self.components.cached.0+=g.cached as i128*r.cached as i128; self.components.output.0+=g.output as i128*r.output as i128; if g.invalid>0 {self.labels.insert("missing_or_invalid_usage".into());} }
    fn add(&mut self, row: &UsageRow) {
        let Some(model) = row.model.as_deref() else {
            self.labels.insert("unknown_model_rate".into());
            return;
        };
        let Some(rates) = rate(model) else {
            self.labels.insert(format!("unknown_model_rate:{model}"));
            return;
        };
        if !complete(row) {
            self.labels.insert("missing_or_invalid_usage".into());
            return;
        }
        let input = row.input_tokens.unwrap() as i128;
        let cached = row.cached_input_tokens.unwrap() as i128;
        let output = row.output_tokens.unwrap() as i128;
        self.components.fresh.0 += (input - cached) * rates.fresh as i128;
        self.components.cached.0 += cached * rates.cached as i128;
        self.components.output.0 += output * rates.output as i128;
    }

    fn estimate(&self, unit: EstimateUnit) -> Estimate {
        if unit == EstimateUnit::ApiUsd {
            return unavailable(unit, "api_usd_rate_not_defined_by_audited_facts");
        }
        let mut labels = self.labels.clone();
        // No speed appears in UsageRow. The audited facts support a standard-rate
        // comparison but not a speed-adjusted provider/account amount.
        labels.insert("speed_unavailable_standard_rate_only".into());
        Estimate {
            unit,
            rate_version: AUDITED_RATE_VERSION,
            rate_source: AUDITED_RATE_SOURCE,
            value: MicroUnits(
                self.components.fresh.0 + self.components.cached.0 + self.components.output.0,
            ),
            incomplete: !labels.is_empty(),
            labels,
        }
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
