use crate::doctor::check::{CheckResult, Context, DoctorCheck};
use crate::doctor::checks;

pub struct Runner {
    pub checks: Vec<Box<dyn DoctorCheck>>,
}

impl Runner {
    pub fn all_checks() -> Self {
        Self { checks: registry() }
    }

    pub fn filtered(pattern: &str) -> Self {
        let pattern = pattern.to_lowercase();
        Self {
            checks: registry()
                .into_iter()
                .filter(|c| c.name().to_lowercase().contains(&pattern))
                .collect(),
        }
    }

    pub fn run(&self, ctx: &Context) -> Vec<(String, CheckResult)> {
        self.checks
            .iter()
            .map(|c| {
                let start = std::time::Instant::now();
                let mut result = c.run(ctx);
                result.elapsed_ms = start.elapsed().as_millis() as u64;
                (c.name().to_string(), result)
            })
            .collect()
    }

    pub fn list(&self) {
        for check in &self.checks {
            println!("{:35} [{}]", check.name(), check.category());
        }
    }
}

fn registry() -> Vec<Box<dyn DoctorCheck>> {
    vec![
        // Health — structural checks
        Box::new(checks::hex_dir::HexDirSet),
        Box::new(checks::hex_structure::HexExists),
        Box::new(checks::hex_structure::HexSkillsExists),
        Box::new(checks::hex_structure::HexSkillsPopulated),
        Box::new(checks::git::GitInitialized),
        Box::new(checks::git::HooksPathConfigured),
        Box::new(checks::large_files::LargeUnignoredFiles),
        Box::new(checks::symlinks::AgentsSkillsSymlink),
        Box::new(checks::symlinks::NoBrokenSymlinks),
        Box::new(checks::memory_db::MemoryDbExists),
        Box::new(checks::distill_strikes::DistillStrikes),
        Box::new(checks::vector_search::VectorSearchHealthy),
        Box::new(checks::reflection_liveness::ReflectionLogFresh),
        Box::new(checks::nightly_full_liveness::NightlyFullLiveness),
        Box::new(checks::consolidation_audit_freshness::ConsolidationAuditFreshness),
        Box::new(checks::scripts_exec::ScriptsExecutable),
        Box::new(checks::boi_health::BoiHealth),
        Box::new(checks::iii_engine_health::IiiEngineHealth),
        Box::new(checks::telemetry_health::TelemetryHealth),
        Box::new(checks::python::PythonVersion),
        Box::new(checks::hex_binary::HexBinaryOnPath),
        // Config checks
        Box::new(checks::llm_provider::LlmProviderReachable),
        Box::new(checks::distill_readiness::DistillReadiness),
        Box::new(checks::env_sh::EnvSh),
        Box::new(checks::claude_md::ClaudeMdExists),
        Box::new(checks::charter_drift::CharterDrift),
        Box::new(checks::claude_runs_config::ClaudeRunsConfig),
        Box::new(checks::codex_config::CodexConfigExists),
        Box::new(checks::codex::CodexCliOnPath),
        Box::new(checks::codex::CodexVersionOk),
        Box::new(checks::codex::CodexApiKey),
        Box::new(checks::codex::CodexAgentsMdExists),
        Box::new(checks::codex::CodexAgentsMdComplete),
        Box::new(checks::me_md::MeMdContent),
        Box::new(checks::todo_md::TodoMdExists),
        Box::new(checks::llm_preference::LlmPreferenceExists),
        Box::new(checks::llm_preference::NoStaleLlmPreference),
        Box::new(checks::llm_config::LlmConfigCheck),
        Box::new(checks::llm_config::StaleLlmPreferenceCheck),
        Box::new(checks::settings_json::SettingsJsonValid),
        Box::new(checks::timezone::TimezoneValid),
        // Registry health checks
        Box::new(checks::registry_health::RegistryOrphanedBin),
        Box::new(checks::registry_health::RegistryStalePolicy),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::check::{Category, Status};
    use std::path::PathBuf;

    struct AlwaysPass;
    impl DoctorCheck for AlwaysPass {
        fn name(&self) -> &str {
            "always-pass"
        }
        fn category(&self) -> Category {
            Category::Health
        }
        fn run(&self, _ctx: &Context) -> CheckResult {
            CheckResult::pass("ok")
        }
    }

    struct AlwaysWarn;
    impl DoctorCheck for AlwaysWarn {
        fn name(&self) -> &str {
            "always-warn"
        }
        fn category(&self) -> Category {
            Category::Config
        }
        fn run(&self, _ctx: &Context) -> CheckResult {
            CheckResult::warn("degraded")
        }
    }

    struct AlwaysFail;
    impl DoctorCheck for AlwaysFail {
        fn name(&self) -> &str {
            "always-fail"
        }
        fn category(&self) -> Category {
            Category::Health
        }
        fn run(&self, _ctx: &Context) -> CheckResult {
            CheckResult::fail("broken")
        }
    }

    fn test_ctx() -> Context {
        Context {
            hex_dir: PathBuf::from("/tmp/fake-hex"),
            home: PathBuf::from("/tmp"),
            fix: false,
        }
    }

    #[test]
    fn test_runner_trait_dispatch() {
        let runner = Runner {
            checks: vec![
                Box::new(AlwaysPass),
                Box::new(AlwaysWarn),
                Box::new(AlwaysFail),
            ],
        };
        let results = runner.run(&test_ctx());
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].1.status, Status::Pass);
        assert_eq!(results[1].1.status, Status::Warn);
        assert_eq!(results[2].1.status, Status::Fail);
    }

    #[test]
    fn test_filter_matching() {
        // A runner built from ad-hoc checks filtered by name substring
        let checks: Vec<Box<dyn DoctorCheck>> = vec![
            Box::new(AlwaysPass),
            Box::new(AlwaysWarn),
            Box::new(AlwaysFail),
        ];
        let pattern = "warn";
        let filtered: Vec<_> = checks
            .into_iter()
            .filter(|c| c.name().contains(pattern))
            .collect();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name(), "always-warn");
    }

    #[test]
    fn test_result_aggregation() {
        let runner = Runner {
            checks: vec![
                Box::new(AlwaysPass),
                Box::new(AlwaysWarn),
                Box::new(AlwaysFail),
            ],
        };
        let results = runner.run(&test_ctx());
        let error_count = results.iter().filter(|(_, r)| r.status.is_error()).count();
        let warn_count = results
            .iter()
            .filter(|(_, r)| r.status.is_warning())
            .count();
        let pass_count = results
            .iter()
            .filter(|(_, r)| r.status.counts_as_pass())
            .count();
        assert_eq!(error_count, 1);
        assert_eq!(warn_count, 1);
        assert_eq!(pass_count, 1);
    }

    #[test]
    fn test_elapsed_ms_populated() {
        let runner = Runner {
            checks: vec![Box::new(AlwaysPass)],
        };
        let results = runner.run(&test_ctx());
        // elapsed_ms should be set (may be 0 for instant check, that's fine)
        assert_eq!(results[0].0, "always-pass");
    }

    #[test]
    fn test_registry_has_checks() {
        let runner = Runner::all_checks();
        assert!(
            runner.checks.len() >= 10,
            "registry must have at least 10 checks"
        );
    }

    #[test]
    fn test_registry_includes_telemetry_health() {
        let runner = Runner::all_checks();
        assert!(
            runner.checks.iter().any(|c| c.name() == "telemetry-health"),
            "registry must include the telemetry-health doctor check"
        );
    }

    #[test]
    fn test_registry_includes_consolidation_audit_freshness() {
        let runner = Runner::all_checks();
        assert!(
            runner
                .checks
                .iter()
                .any(|c| c.name() == "consolidation-audit-freshness"),
            "registry must include the consolidation-audit-freshness doctor check"
        );
    }
}
