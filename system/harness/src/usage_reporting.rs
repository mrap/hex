//! Deterministic read models for the durable usage ledger.
//!
//! This module has no provider, file, or database side effects.  It deliberately
//! separates measured tokens from standard-rate credit comparisons and never
//! calls an estimate an invoice or an account debit.

use crate::usage_ledger::UsageSummaryGroup;
use crate::usage_ledger::{Coverage, UsageRow};
use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, BTreeSet};

pub const AUDITED_RATE_VERSION: &str = "codex-standard-2026-09-10";
pub const AUDITED_RATE_SOURCE: &str =
    "https://learn.chatgpt.com/docs/pricing (retrieved 2026-09-10)";

/// Provenance for the Claude rate table below. Anthropic API list prices,
/// USD per million tokens. Surfaced as a `claude_rate_version:<value>`
/// label on `modeled_credits` (never as the top-level `rate_version`,
/// which stays the audited Codex card) whenever a Claude row is priced.
pub const CLAUDE_RATE_VERSION: &str = "anthropic-api-2026-06-24";
pub const CLAUDE_RATE_SOURCE: &str = "https://www.anthropic.com/pricing (retrieved 2026-06-24)";

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

/// Per-token USD-equivalent rates for a Claude model, expressed as integer
/// **cents** (1/100 USD) per million tokens. Anthropic's published API list
/// prices carry two decimal places (e.g. $0.25/MTok), too fine for the
/// Codex card's whole-units-per-MTok scale (`TokenRates`), so this table
/// uses one extra digit of precision and the credit math divides by 100
/// (rather than the implicit divide-by-nothing `tokens * rate` the Codex
/// card uses) to land back on the same micro-unit (1e-6 USD) granularity:
/// `micro_usd = tokens * cents_per_million / 100`. For realistic token
/// counts the integer-division remainder is under one micro-dollar per
/// bucket, which is negligible for a modeled estimate (never an invoice).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaudeTokenRates {
    pub fresh: i64,
    pub cached: i64,
    pub cache_write: i64,
    pub output: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaudeRateFact {
    pub model: &'static str,
    pub rates: ClaudeTokenRates,
}

/// Anthropic API list prices as of 2026-06-24 (see [`CLAUDE_RATE_SOURCE`]).
/// Claude Code usage is billed under a subscription, not the API — this
/// table models a USD-equivalent using the API list price as the closest
/// available public reference, not an actual bill. Every row priced from
/// this table carries the `subscription_usage_priced_at_api_list` and
/// `api_usd_equivalent` labels (see `CreditAccumulator`).
pub const CLAUDE_API_RATES: &[ClaudeRateFact] = &[
    ClaudeRateFact {
        model: "claude-fable-5-1",
        rates: ClaudeTokenRates {
            fresh: 1_000,
            cached: 25,
            cache_write: 1_250,
            output: 5_000,
        },
    },
    ClaudeRateFact {
        model: "claude-opus-5",
        rates: ClaudeTokenRates {
            fresh: 500,
            cached: 50,
            cache_write: 625,
            output: 2_500,
        },
    },
    ClaudeRateFact {
        model: "claude-sonnet-5",
        rates: ClaudeTokenRates {
            fresh: 200,
            cached: 20,
            cache_write: 250,
            output: 1_000,
        },
    },
    ClaudeRateFact {
        model: "claude-haiku-4-5",
        rates: ClaudeTokenRates {
            fresh: 100,
            cached: 10,
            cache_write: 125,
            output: 500,
        },
    },
    ClaudeRateFact {
        model: "claude-haiku-4-5-20251001",
        rates: ClaudeTokenRates {
            fresh: 100,
            cached: 10,
            cache_write: 125,
            output: 500,
        },
    },
    // Previous generation still seen in the ledger (nightly agent-infra runs
    // on Sonnet 4.6; Opus 4.6/4.7/4.8 share Opus 5's price). Anthropic API
    // list, 2026-06-24.
    ClaudeRateFact {
        model: "claude-sonnet-4-6",
        rates: ClaudeTokenRates {
            fresh: 300,
            cached: 30,
            cache_write: 375,
            output: 1_500,
        },
    },
    ClaudeRateFact {
        model: "claude-opus-4-8",
        rates: ClaudeTokenRates {
            fresh: 500,
            cached: 50,
            cache_write: 625,
            output: 2_500,
        },
    },
    ClaudeRateFact {
        model: "claude-opus-4-7",
        rates: ClaudeTokenRates {
            fresh: 500,
            cached: 50,
            cache_write: 625,
            output: 2_500,
        },
    },
    ClaudeRateFact {
        model: "claude-opus-4-6",
        rates: ClaudeTokenRates {
            fresh: 500,
            cached: 50,
            cache_write: 625,
            output: 2_500,
        },
    },
    ClaudeRateFact {
        model: "claude-sonnet-4-5",
        rates: ClaudeTokenRates {
            fresh: 300,
            cached: 30,
            cache_write: 375,
            output: 1_500,
        },
    },
];

/// Providers whose Claude-priced rows are billed by subscription rather than
/// pay-per-token API usage — gates the `subscription_usage_priced_at_api_list`
/// label.
const CLAUDE_SUBSCRIPTION_PROVIDERS: &[&str] = &["claude-code", "claude_code", "claude-cli"];

/// Exact match first, then longest-prefix match, so a dated variant
/// (`claude-sonnet-5-20260615`) prices off its family's row even when not
/// listed explicitly. Longest-prefix (not first-match) keeps the result
/// independent of table order.
fn claude_rate(model: &str) -> Option<ClaudeTokenRates> {
    // Normalize gateway spellings to Anthropic ids: OpenRouter writes
    // `anthropic/claude-haiku-4.5`; the vendor prefix and dotted version
    // carry no pricing information.
    let normalized = model.rsplit('/').next().unwrap_or(model).replace('.', "-");
    let model = normalized.as_str();
    if let Some(fact) = CLAUDE_API_RATES.iter().find(|r| r.model == model) {
        return Some(fact.rates);
    }
    CLAUDE_API_RATES
        .iter()
        .filter(|r| model.starts_with(r.model))
        .max_by_key(|r| r.model.len())
        .map(|r| r.rates)
}

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
    /// Only ever non-zero for Claude-priced rows; the Codex card has no
    /// cache-write rate, so Codex cache-write tokens stay unpriced (as
    /// before this field existed).
    pub cache_write: MicroUnits,
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
    /// Keyed on `UsageRow::provider` (e.g. `codex`, `claude-code`).
    pub by_provider: Vec<Contributor>,
    /// Keyed on `UsageRow::account_scope` (e.g. `boi`, `harness`). Note this
    /// is coarser than `by_provider`: distinct providers that share a scope
    /// label (`claude_code/boi` and `codex/boi` both use `boi`) fold into
    /// one `by_source` entry.
    pub by_source: Vec<Contributor>,
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
            let target = if preceding {
                &mut self.previous
            } else {
                &mut self.current
            };
            target.add_group(group, self.include_ids);
        }
    }

    /// Summary callers pass `include_ids = false`, so page rows do not remain
    /// owned after aggregation. This exposes the retained optional detail IDs
    /// for a deterministic bounded-memory regression test.
    pub fn retained_response_ids(&self) -> usize {
        let ids = |accumulator: &Accumulator| {
            accumulator
                .models
                .values()
                .map(|bucket| bucket.response_ids.len())
                .sum::<usize>()
                + accumulator
                    .families
                    .values()
                    .map(|bucket| bucket.response_ids.len())
                    .sum::<usize>()
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
        by_provider: current.contributors(&current.providers),
        by_source: current.contributors(&current.sources),
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
    providers: BTreeMap<String, Bucket>,
    sources: BTreeMap<String, Bucket>,
    children: Bucket,
}

impl Accumulator {
    fn add_group(&mut self, g: &UsageSummaryGroup, include_ids: bool) {
        if g.invalid > 0 {
            self.labels.insert("missing_or_invalid_usage".into());
            self.invalid_usage = true;
        }
        if g.complete == 0 {
            return;
        }
        let add = |m: &mut MeasuredTokens| {
            let first = m.responses == 0;
            m.responses += g.complete as u64;
            m.input += g.input as i128;
            m.cached_input += g.cached as i128;
            m.uncached_input += (g.input - g.cached) as i128;
            m.output += g.output as i128;
            m.cache_write_input += g.cache_write as i128;
            m.reasoning_output += g.reasoning as i128;
            m.provider_total = if g.missing_provider_total > 0 {
                None
            } else if first {
                Some(g.provider_total as i128)
            } else {
                m.provider_total.map(|v| v + g.provider_total as i128)
            };
        };
        add(&mut self.measured);
        self.credits.add_summary(g);
        let model = g.model.clone().unwrap_or_else(|| "unknown".into());
        let family = g.family.clone().unwrap_or_else(|| "unknown".into());
        add_bucket_group(self.models.entry(model).or_default(), g, include_ids);
        add_bucket_group(self.families.entry(family).or_default(), g, include_ids);
        if g.child {
            add_bucket_group(&mut self.children, g, include_ids);
        }
        add_bucket_group(
            self.providers.entry(g.provider.clone()).or_default(),
            g,
            include_ids,
        );
        add_bucket_group(
            self.sources.entry(g.account_scope.clone()).or_default(),
            g,
            include_ids,
        );
        if g.model.is_none() {
            self.labels.insert("unknown_model".into());
        }
        if g.missing_cache_write > 0 {
            self.labels
                .insert("missing_cache_write_input_tokens".into());
        }
        if g.missing_reasoning > 0 {
            self.labels.insert("missing_reasoning_output_tokens".into());
        }
        if g.missing_provider_total > 0 {
            self.labels.insert("missing_provider_total_tokens".into());
        }
        self.labels
            .insert("speed_unavailable_standard_rate_only".into());
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
        add_bucket(
            self.providers.entry(row.provider.clone()).or_default(),
            row,
            include_ids,
        );
        add_bucket(
            self.sources.entry(row.account_scope.clone()).or_default(),
            row,
            include_ids,
        );
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
fn add_bucket_group(bucket: &mut Bucket, g: &UsageSummaryGroup, _: bool) {
    if g.complete == 0 {
        return;
    };
    let first = bucket.measured.responses == 0;
    bucket.measured.responses += g.complete as u64;
    bucket.measured.input += g.input as i128;
    bucket.measured.cached_input += g.cached as i128;
    bucket.measured.uncached_input += (g.input - g.cached) as i128;
    bucket.measured.output += g.output as i128;
    bucket.measured.cache_write_input += g.cache_write as i128;
    bucket.measured.reasoning_output += g.reasoning as i128;
    bucket.measured.provider_total = if g.missing_provider_total > 0 {
        None
    } else if first {
        Some(g.provider_total as i128)
    } else {
        bucket
            .measured
            .provider_total
            .map(|v| v + g.provider_total as i128)
    };
    bucket.credits.add_summary(g);
}

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

/// Which rate card priced a model, so the caller can apply each card's own
/// unit semantics (Codex: whole units per MTok, no cache-write price;
/// Claude: cents per MTok, `/100` back to micro-units, cache-write priced).
enum PricedBy {
    Codex(TokenRates),
    Claude(ClaudeTokenRates),
}

/// Codex card first (exact match only, unchanged), then the Claude card
/// (exact then longest-prefix, see [`claude_rate`]).
fn rate_for(model: &str) -> Option<PricedBy> {
    if let Some(r) = rate(model) {
        return Some(PricedBy::Codex(r));
    }
    claude_rate(model).map(PricedBy::Claude)
}

#[derive(Default)]
struct CreditAccumulator {
    components: CreditComponents,
    labels: BTreeSet<String>,
}

impl CreditAccumulator {
    fn add_summary(&mut self, g: &UsageSummaryGroup) {
        let Some(model) = g.model.as_deref() else {
            self.labels.insert("unknown_model_rate".into());
            return;
        };
        let Some(priced) = rate_for(model) else {
            self.labels.insert(format!("unknown_model_rate:{model}"));
            return;
        };
        match priced {
            PricedBy::Codex(r) => {
                self.components.fresh.0 += (g.input - g.cached) as i128 * r.fresh as i128;
                self.components.cached.0 += g.cached as i128 * r.cached as i128;
                self.components.output.0 += g.output as i128 * r.output as i128;
            }
            PricedBy::Claude(r) => {
                self.components.fresh.0 += (g.input - g.cached) as i128 * r.fresh as i128 / 100;
                self.components.cached.0 += g.cached as i128 * r.cached as i128 / 100;
                self.components.cache_write.0 +=
                    g.cache_write as i128 * r.cache_write as i128 / 100;
                self.components.output.0 += g.output as i128 * r.output as i128 / 100;
                self.label_claude_subscription(&g.provider);
            }
        }
        if g.invalid > 0 {
            self.labels.insert("missing_or_invalid_usage".into());
        }
    }
    fn add(&mut self, row: &UsageRow) {
        let Some(model) = row.model.as_deref() else {
            self.labels.insert("unknown_model_rate".into());
            return;
        };
        let Some(priced) = rate_for(model) else {
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
        match priced {
            PricedBy::Codex(r) => {
                self.components.fresh.0 += (input - cached) * r.fresh as i128;
                self.components.cached.0 += cached * r.cached as i128;
                self.components.output.0 += output * r.output as i128;
            }
            PricedBy::Claude(r) => {
                let cache_write = row.cache_write_input_tokens.unwrap_or(0) as i128;
                self.components.fresh.0 += (input - cached) * r.fresh as i128 / 100;
                self.components.cached.0 += cached * r.cached as i128 / 100;
                self.components.cache_write.0 += cache_write * r.cache_write as i128 / 100;
                self.components.output.0 += output * r.output as i128 / 100;
                self.label_claude_subscription(&row.provider);
            }
        }
    }

    /// Claude Code usage is billed under a subscription, not pay-per-token
    /// API access. This flags rows priced from the Claude card as a modeled
    /// USD-equivalent proxy, not an actual bill, only for the providers that
    /// are genuinely subscription-billed (an `openrouter` row that happened
    /// to run a Claude model is real API spend and does not get this label).
    fn label_claude_subscription(&mut self, provider: &str) {
        if CLAUDE_SUBSCRIPTION_PROVIDERS.contains(&provider) {
            self.labels
                .insert("subscription_usage_priced_at_api_list".into());
            self.labels.insert("api_usd_equivalent".into());
            self.labels
                .insert(format!("claude_rate_version:{CLAUDE_RATE_VERSION}"));
        }
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
                self.components.fresh.0
                    + self.components.cached.0
                    + self.components.output.0
                    + self.components.cache_write.0,
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

#[cfg(test)]
mod tests {

    /// OpenRouter model ids (`anthropic/claude-haiku-4.5`) must price like
    /// the Anthropic id they name.
    #[test]
    fn claude_rate_normalizes_gateway_model_ids() {
        assert_eq!(
            claude_rate("anthropic/claude-haiku-4.5"),
            claude_rate("claude-haiku-4-5")
        );
        assert_eq!(
            claude_rate("anthropic/claude-sonnet-5"),
            claude_rate("claude-sonnet-5")
        );
        assert!(claude_rate("anthropic/claude-haiku-4.5").is_some());
        assert!(claude_rate("gpt-5.6-terra").is_none());
    }
    use super::*;
    use crate::usage_ledger::{FrozenWindow, HalfOpenUtcWindow, ImportOptions, UsageLedger};

    fn t(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn row(
        id: &str,
        provider: &str,
        scope: &str,
        at: &str,
        model: Option<&str>,
        input: Option<i64>,
        cached: Option<i64>,
        cache_write: Option<i64>,
        output: Option<i64>,
    ) -> UsageRow {
        UsageRow {
            provider: provider.into(),
            account_scope: scope.into(),
            response_id: id.into(),
            parent_response_id: None,
            root_task_family: Some("fam".into()),
            event_at: Some(at.into()),
            model: model.map(str::to_owned),
            effort: None,
            input_tokens: input,
            cached_input_tokens: cached,
            cache_write_input_tokens: cache_write,
            output_tokens: output,
            reasoning_output_tokens: None,
            total_tokens: None,
        }
    }

    /// `by_provider` keys on `provider`; `by_source` keys on `account_scope`
    /// and is therefore coarser -- two providers sharing a scope label
    /// (`claude_code`/`codex` both writing into `boi`) fold into one
    /// `by_source` entry, while `by_provider` keeps them apart.
    #[test]
    fn by_provider_and_by_source_aggregate_with_the_documented_key_difference() {
        let rows = vec![
            row(
                "a",
                "claude_code",
                "boi",
                "2026-09-10T01:00:00Z",
                Some("claude-haiku-4-5"),
                Some(100),
                Some(0),
                None,
                Some(10),
            ),
            row(
                "b",
                "codex",
                "boi",
                "2026-09-10T02:00:00Z",
                Some("gpt-5.6-luna"),
                Some(50),
                Some(0),
                None,
                Some(5),
            ),
            row(
                "c",
                "codex",
                "local-codex-history",
                "2026-09-10T03:00:00Z",
                Some("gpt-5.6-luna"),
                Some(10),
                Some(0),
                None,
                Some(1),
            ),
        ];
        let r = report(
            &rows,
            Coverage::default(),
            t("2026-09-10T00:00:00Z"),
            t("2026-09-11T00:00:00Z"),
        );
        assert_eq!(r.by_provider.len(), 2);
        assert_eq!(r.by_provider[0].key, "claude_code");
        assert_eq!(r.by_provider[0].measured.total(), 110);
        assert_eq!(r.by_provider[1].key, "codex");
        assert_eq!(r.by_provider[1].measured.total(), 66);

        assert_eq!(r.by_source.len(), 2);
        assert_eq!(r.by_source[0].key, "boi");
        assert_eq!(r.by_source[0].measured.total(), 165);
        assert_eq!(r.by_source[1].key, "local-codex-history");
        assert_eq!(r.by_source[1].measured.total(), 11);
    }

    /// Same tie-break rule `by_model`/`by_family` already use (descending
    /// tokens, then ascending key) applies to the two new dimensions too.
    #[test]
    fn by_provider_and_by_source_ties_sort_by_key() {
        let rows = vec![
            row(
                "z",
                "zeta-provider",
                "scope-z",
                "2026-09-10T01:00:00Z",
                Some("gpt-5.6-luna"),
                Some(10),
                Some(0),
                None,
                Some(0),
            ),
            row(
                "a",
                "alpha-provider",
                "scope-a",
                "2026-09-10T02:00:00Z",
                Some("gpt-5.6-luna"),
                Some(10),
                Some(0),
                None,
                Some(0),
            ),
        ];
        let r = report(
            &rows,
            Coverage::default(),
            t("2026-09-10T00:00:00Z"),
            t("2026-09-11T00:00:00Z"),
        );
        assert_eq!(r.by_provider[0].key, "alpha-provider");
        assert_eq!(r.by_provider[1].key, "zeta-provider");
        assert_eq!(r.by_source[0].key, "scope-a");
        assert_eq!(r.by_source[1].key, "scope-z");
    }

    /// Exact-match Claude pricing, including the cache-write component the
    /// Codex card never prices. `1_000_000 - 200_000 = 800_000` fresh tokens
    /// at 200 cents/MTok -> `800_000*200/100 = 1_600_000` micro-USD, etc.
    #[test]
    fn claude_rate_table_prices_exact_match_with_cache_write() {
        let rows = vec![row(
            "exact",
            "claude-code",
            "boi",
            "2026-09-10T01:00:00Z",
            Some("claude-sonnet-5"),
            Some(1_000_000),
            Some(200_000),
            Some(100_000),
            Some(50_000),
        )];
        let r = report(
            &rows,
            Coverage::default(),
            t("2026-09-10T00:00:00Z"),
            t("2026-09-11T00:00:00Z"),
        );
        assert_eq!(r.modeled_credit_components.fresh, MicroUnits(1_600_000));
        assert_eq!(r.modeled_credit_components.cached, MicroUnits(40_000));
        assert_eq!(r.modeled_credit_components.cache_write, MicroUnits(250_000));
        assert_eq!(r.modeled_credit_components.output, MicroUnits(500_000));
        assert_eq!(r.modeled_credits.value, MicroUnits(2_390_000));
        assert!(r
            .modeled_credits
            .labels
            .contains("subscription_usage_priced_at_api_list"));
        assert!(r.modeled_credits.labels.contains("api_usd_equivalent"));
        assert!(r
            .modeled_credits
            .labels
            .contains("claude_rate_version:anthropic-api-2026-06-24"));
        assert!(!r
            .modeled_credits
            .labels
            .iter()
            .any(|l| l.starts_with("unknown_model_rate")));
    }

    /// A dated variant not listed explicitly (`claude-sonnet-5-20260615`)
    /// prices off the longest matching prefix (`claude-sonnet-5`) rather
    /// than landing in `unknown_model_rate`.
    #[test]
    fn claude_rate_table_prefix_matches_dated_variant() {
        let rows = vec![row(
            "dated",
            "claude-code",
            "boi",
            "2026-09-10T01:00:00Z",
            Some("claude-sonnet-5-20260615"),
            Some(100),
            Some(0),
            None,
            Some(10),
        )];
        let r = report(
            &rows,
            Coverage::default(),
            t("2026-09-10T00:00:00Z"),
            t("2026-09-11T00:00:00Z"),
        );
        // 100 fresh * 200/100 + 10 output * 1000/100 = 200 + 100.
        assert_eq!(r.modeled_credits.value, MicroUnits(300));
        assert!(!r
            .modeled_credits
            .labels
            .iter()
            .any(|l| l.starts_with("unknown_model_rate")));
    }

    /// A Claude model run through a non-subscription provider (e.g. routed
    /// via OpenRouter) still prices off the Claude card -- it is real,
    /// pay-per-token API spend, not a subscription -- but must not carry the
    /// `subscription_usage_priced_at_api_list` caveat, since that label
    /// specifically means "this number is a proxy for unmetered usage."
    #[test]
    fn claude_priced_row_from_non_subscription_provider_skips_subscription_label() {
        let rows = vec![row(
            "or",
            "openrouter",
            "harness",
            "2026-09-10T01:00:00Z",
            Some("claude-fable-5-1"),
            Some(1_000_000),
            Some(0),
            None,
            Some(0),
        )];
        let r = report(
            &rows,
            Coverage::default(),
            t("2026-09-10T00:00:00Z"),
            t("2026-09-11T00:00:00Z"),
        );
        assert_eq!(r.modeled_credits.value, MicroUnits(10_000_000));
        assert!(!r
            .modeled_credits
            .labels
            .contains("subscription_usage_priced_at_api_list"));
        assert!(!r.modeled_credits.labels.contains("api_usd_equivalent"));
    }

    /// End-to-end through a real ledger (SQLite, temp dir): imports one row
    /// for each of the five live provider/account_scope pairs plus the two
    /// still-`_`-shaped BOI/harness ones the same worker-provider prefix
    /// covers -- codex/local-codex-history, claude-code/local-claude-code,
    /// claude-code/headless, claude_code/boi, codex/boi, openrouter/harness,
    /// claude-cli/harness -- then drives the exact same
    /// coverage()/frozen_read()/summary_groups()/ReportAccumulator path the
    /// `hex usage report` CLI uses, proving the new SQL GROUP BY dimensions
    /// (not just the rows-path Accumulator) produce the new fields.
    ///
    /// NOTE on the "4 scopes" figure some specs of this feature quote: the
    /// seven pairs above carry only FIVE distinct `account_scope` values
    /// (local-codex-history, local-claude-code, headless, boi, harness) --
    /// `boi` and `harness` are each shared by two providers. `by_source`
    /// keys on `account_scope` alone (see its doc comment on `UsageReport`),
    /// so those pairs fold together and `by_source.len()` is 5, not 4.
    #[test]
    fn seven_provider_scope_pairs_populate_by_provider_by_source_and_coverage_sources() {
        let tmp = tempfile::TempDir::new().unwrap();
        let pairs: [(&str, &str, &str); 7] = [
            ("codex", "local-codex-history", "gpt-5.6-sol"),
            ("claude-code", "local-claude-code", "claude-sonnet-5"),
            ("claude-code", "headless", "claude-opus-5"),
            ("claude_code", "boi", "claude-haiku-4-5"),
            ("codex", "boi", "gpt-5.6-terra"),
            ("openrouter", "harness", "claude-fable-5-1"),
            ("claude-cli", "harness", "claude-haiku-4-5-20251001"),
        ];
        let mut body = String::new();
        for (index, (provider, scope, model)) in pairs.iter().enumerate() {
            let line = serde_json::json!({
                "type": "token_usage_record",
                "provider": provider,
                "account_scope": scope,
                "response_id": format!("resp-{index}"),
                "root_task_family": "fam",
                "event_at": "2026-09-14T12:00:00Z",
                "model": model,
                "input_tokens": 1_000,
                "cached_input_tokens": 100,
                "output_tokens": 200,
            });
            body.push_str(&line.to_string());
            body.push('\n');
        }
        let staging = tmp.path().join("staging.jsonl");
        std::fs::write(&staging, body).unwrap();

        let mut ledger = UsageLedger::open(tmp.path().join("usage.db")).unwrap();
        let result = ledger
            .import_jsonl(&staging, ImportOptions::default())
            .unwrap();
        assert_eq!(
            result.accepted, 7,
            "all seven rows must import cleanly: {result:?}"
        );

        let start = t("2026-09-14T00:00:00Z");
        let end = t("2026-09-15T00:00:00Z");
        let preceding_start = t("2026-09-13T00:00:00Z");

        let coverage = ledger.coverage().unwrap();
        assert_eq!(
            coverage.sources.len(),
            7,
            "one coverage.sources entry per distinct (provider, account_scope) pair: {:?}",
            coverage.sources
        );
        for source in &coverage.sources {
            assert_eq!(source.records, 1, "{source:?}");
            assert_eq!(
                source.first_event_at.as_deref(),
                Some("2026-09-14T12:00:00Z")
            );
            assert_eq!(
                source.last_event_at.as_deref(),
                Some("2026-09-14T12:00:00Z")
            );
        }

        let read = ledger
            .frozen_read([
                HalfOpenUtcWindow { start, end },
                HalfOpenUtcWindow {
                    start: preceding_start,
                    end: start,
                },
            ])
            .unwrap();
        let mut accumulator = ReportAccumulator::new(coverage, start, end, false);
        accumulator
            .extend_summary_groups(&read.summary_groups(FrozenWindow::First).unwrap(), false);
        accumulator
            .extend_summary_groups(&read.summary_groups(FrozenWindow::Second).unwrap(), true);
        let r = accumulator.finish();

        let provider_keys: Vec<_> = r.by_provider.iter().map(|c| c.key.clone()).collect();
        assert_eq!(
            r.by_provider.len(),
            5,
            "codex, claude-code, claude_code, openrouter, claude-cli: {provider_keys:?}"
        );
        let source_keys: Vec<_> = r.by_source.iter().map(|c| c.key.clone()).collect();
        assert_eq!(
            r.by_source.len(),
            5,
            "local-codex-history, local-claude-code, headless, boi, harness: {source_keys:?}"
        );

        assert!(
            !r.modeled_credits
                .labels
                .iter()
                .any(|l| l.starts_with("unknown_model_rate")),
            "every seeded model is in the Codex or Claude table: {:?}",
            r.modeled_credits.labels
        );
        assert!(r.modeled_credits.value.0 > 0);
        assert!(r
            .modeled_credits
            .labels
            .contains("subscription_usage_priced_at_api_list"));
    }
}
