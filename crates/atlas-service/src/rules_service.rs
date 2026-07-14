//! The `AtlasRules` gRPC service (docs/phases.md R2, PRD §9.7).
//!
//! Serves the persistent rule/profile CRUD, the dry-run `SimulateRule`, and the
//! live `ListInterventions` surface over the same named pipe as `AtlasQuery` /
//! `AtlasControl`. Rule/profile state lives in the store (schema v8); the live
//! applier + reversal ledger live in [`crate::rules::RulesEngine`], which this
//! service shares so `ListInterventions` reflects exactly what is applied and
//! `SimulateRule` runs the *same* resolver the applier does (a preview can't
//! lie). Enabling a rule is the consent — the applier picks it up on its next
//! tick — so there is no per-call token here (unlike the AtlasControl broker).

#![cfg(windows)]

use std::sync::Arc;

use tonic::{Request, Response, Status};

use atlas_ipc::v0::atlas_rules_server::AtlasRules;
use atlas_ipc::{
    CreateProfileReply, CreateProfileRequest, CreateRuleReply, CreateRuleRequest,
    DeleteProfileReply, DeleteProfileRequest, DeleteRuleReply, DeleteRuleRequest,
    DynamicProtectionConfig, GetDynamicProtectionReply, GetDynamicProtectionRequest, GetRuleReply,
    GetRuleRequest, Intervention, ListInterventionsReply, ListInterventionsRequest,
    ListProfilesReply, ListProfilesRequest, ListRulesReply, ListRulesRequest, Profile, Rule,
    RuleAction, SetDynamicProtectionReply, SetDynamicProtectionRequest, SetProfileActiveReply,
    SetProfileActiveRequest, SetRuleEnabledReply, SetRuleEnabledRequest, SimulateRuleReply,
    SimulateRuleRequest, SimulatedTarget, UpdateProfileReply, UpdateProfileRequest,
    UpdateRuleReply, UpdateRuleRequest,
};
use atlas_store::{ProfileRow, RuleRow};

use crate::dynamic_protection::DynConfig;
use crate::ipc::SharedStore;
use crate::rules::{ResolvableRule, RulesEngine};

/// Maps the live watchdog [`DynConfig`] onto the proto wire type.
fn dyn_config_to_proto(c: DynConfig) -> DynamicProtectionConfig {
    DynamicProtectionConfig {
        enabled: c.enabled,
        cpu_threshold_permille: c.cpu_threshold_permille,
        sustain_seconds: c.sustain_seconds,
        max_intervention_seconds: c.max_intervention_seconds,
    }
}

/// Validates a proto `DynamicProtectionConfig` and, if sound, returns the live
/// [`DynConfig`]. Returns `Err(message)` describing the first violated bound.
///
/// The safety-critical bounds: a threshold in 1..=1000‰ (0 would sweep every
/// process; >1000 is impossible), a sustain of at least 1 s (some observation
/// before acting), and a max-intervention cap that is at least 1 s AND at least
/// the sustain window (a cap shorter than the sustain could never trip, leaving
/// a dampening effectively unbounded).
fn validate_dyn_config(c: &DynamicProtectionConfig) -> Result<DynConfig, String> {
    if c.cpu_threshold_permille == 0 || c.cpu_threshold_permille > 1000 {
        return Err(format!(
            "cpu_threshold_permille must be 1..=1000 (got {})",
            c.cpu_threshold_permille
        ));
    }
    if c.sustain_seconds == 0 {
        return Err("sustain_seconds must be at least 1".to_string());
    }
    if c.max_intervention_seconds == 0 {
        return Err("max_intervention_seconds must be at least 1".to_string());
    }
    if c.max_intervention_seconds < c.sustain_seconds {
        return Err(format!(
            "max_intervention_seconds ({}) must be >= sustain_seconds ({})",
            c.max_intervention_seconds, c.sustain_seconds
        ));
    }
    Ok(DynConfig {
        enabled: c.enabled,
        cpu_threshold_permille: c.cpu_threshold_permille,
        sustain_seconds: c.sustain_seconds,
        max_intervention_seconds: c.max_intervention_seconds,
    })
}

/// The AtlasRules service: shares the query service's store (rule persistence +
/// audit) and the live [`RulesEngine`] (interventions + the resolver).
pub struct RulesService {
    store: SharedStore,
    engine: Arc<RulesEngine>,
}

impl RulesService {
    pub fn new(store: SharedStore, engine: Arc<RulesEngine>) -> Self {
        Self { store, engine }
    }
}

/// A poisoned-store-mutex status (mirrors the query service).
fn poisoned() -> Status {
    Status::internal("store mutex poisoned")
}

/// Flattens a proto `Rule` (+ its optional `RuleAction`) into a store `RuleRow`.
fn rule_to_row(r: &Rule) -> RuleRow {
    let a = r.action.unwrap_or_default();
    RuleRow {
        id: r.id,
        name: r.name.clone(),
        enabled: r.enabled,
        match_image: r.match_image.clone(),
        trigger: r.trigger,
        priority_class: a.priority,
        affinity_mode: a.affinity_mode,
        affinity_mask: a.affinity_mask,
        eco_qos: a.eco_qos,
        precedence: r.precedence,
        created_ms: r.created_ms,
    }
}

/// Rebuilds a proto `Rule` from a store `RuleRow`.
fn row_to_rule(row: &RuleRow) -> Rule {
    Rule {
        id: row.id,
        name: row.name.clone(),
        enabled: row.enabled,
        match_image: row.match_image.clone(),
        trigger: row.trigger,
        action: Some(RuleAction {
            priority: row.priority_class,
            affinity_mode: row.affinity_mode,
            affinity_mask: row.affinity_mask,
            eco_qos: row.eco_qos,
        }),
        precedence: row.precedence,
        created_ms: row.created_ms,
    }
}

/// Maps a store `ProfileRow` to a proto `Profile`.
fn row_to_profile(row: &ProfileRow) -> Profile {
    Profile {
        id: row.id,
        name: row.name.clone(),
        rule_ids: row.rule_ids.clone(),
        power_mode: row.power_mode.clone(),
        active: row.active,
    }
}

#[tonic::async_trait]
impl AtlasRules for RulesService {
    async fn list_rules(
        &self,
        _req: Request<ListRulesRequest>,
    ) -> Result<Response<ListRulesReply>, Status> {
        let store = self.store.lock().map_err(|_| poisoned())?;
        let rows = store
            .list_rules()
            .map_err(|e| Status::internal(format!("list_rules: {e}")))?;
        Ok(Response::new(ListRulesReply {
            rules: rows.iter().map(row_to_rule).collect(),
        }))
    }

    async fn get_rule(
        &self,
        req: Request<GetRuleRequest>,
    ) -> Result<Response<GetRuleReply>, Status> {
        let id = req.into_inner().id;
        let store = self.store.lock().map_err(|_| poisoned())?;
        let row = store
            .get_rule(id)
            .map_err(|e| Status::internal(format!("get_rule: {e}")))?;
        Ok(Response::new(GetRuleReply {
            found: row.is_some(),
            rule: row.as_ref().map(row_to_rule),
        }))
    }

    async fn create_rule(
        &self,
        req: Request<CreateRuleRequest>,
    ) -> Result<Response<CreateRuleReply>, Status> {
        let proto = req
            .into_inner()
            .rule
            .ok_or_else(|| Status::invalid_argument("rule is required"))?;
        if proto.match_image.trim().is_empty() {
            return Err(Status::invalid_argument(
                "match_image is required (an empty match would sweep nothing)",
            ));
        }
        let row = rule_to_row(&proto); // id ignored on insert
        let store = self.store.lock().map_err(|_| poisoned())?;
        let id = store
            .create_rule(&row)
            .map_err(|e| Status::internal(format!("create_rule: {e}")))?;
        Ok(Response::new(CreateRuleReply { id }))
    }

    async fn update_rule(
        &self,
        req: Request<UpdateRuleRequest>,
    ) -> Result<Response<UpdateRuleReply>, Status> {
        let proto = req
            .into_inner()
            .rule
            .ok_or_else(|| Status::invalid_argument("rule is required"))?;
        let row = rule_to_row(&proto);
        let store = self.store.lock().map_err(|_| poisoned())?;
        let ok = store
            .update_rule(&row)
            .map_err(|e| Status::internal(format!("update_rule: {e}")))?;
        Ok(Response::new(UpdateRuleReply {
            ok,
            message: if ok {
                String::new()
            } else {
                format!("no rule #{}", row.id)
            },
        }))
    }

    async fn delete_rule(
        &self,
        req: Request<DeleteRuleRequest>,
    ) -> Result<Response<DeleteRuleReply>, Status> {
        let id = req.into_inner().id;
        let store = self.store.lock().map_err(|_| poisoned())?;
        let ok = store
            .delete_rule(id)
            .map_err(|e| Status::internal(format!("delete_rule: {e}")))?;
        Ok(Response::new(DeleteRuleReply { ok }))
    }

    async fn set_rule_enabled(
        &self,
        req: Request<SetRuleEnabledRequest>,
    ) -> Result<Response<SetRuleEnabledReply>, Status> {
        let r = req.into_inner();
        let store = self.store.lock().map_err(|_| poisoned())?;
        let ok = store
            .set_rule_enabled(r.id, r.enabled)
            .map_err(|e| Status::internal(format!("set_rule_enabled: {e}")))?;
        Ok(Response::new(SetRuleEnabledReply { ok }))
    }

    async fn simulate_rule(
        &self,
        req: Request<SimulateRuleRequest>,
    ) -> Result<Response<SimulateRuleReply>, Status> {
        let proto = req
            .into_inner()
            .rule
            .ok_or_else(|| Status::invalid_argument("rule is required"))?;
        let sim_row = rule_to_row(&proto);
        let sim = ResolvableRule::from_row(&sim_row);

        // The other enabled rules provide conflict context; exclude the rule
        // being simulated when it is a saved rule being edited (same id).
        let others: Vec<ResolvableRule> = {
            let store = self.store.lock().map_err(|_| poisoned())?;
            store
                .list_enabled_rules()
                .map_err(|e| Status::internal(format!("simulate_rule (others): {e}")))?
                .iter()
                .filter(|r| sim_row.id == 0 || r.id != sim_row.id)
                .map(ResolvableRule::from_row)
                .collect()
        };

        // The simulation snapshots live processes + reads their current policy
        // (blocking syscalls) — run it off the async runtime.
        let engine = self.engine.clone();
        let result = tokio::task::spawn_blocking(move || engine.simulate(&sim, &others))
            .await
            .map_err(|e| Status::internal(format!("simulate task: {e}")))?;

        Ok(Response::new(SimulateRuleReply {
            targets: result
                .targets
                .into_iter()
                .map(|t| SimulatedTarget {
                    pid: t.pid,
                    image_name: t.image_name,
                    current_priority: t.current_priority,
                    new_priority: t.new_priority,
                    current_affinity: t.current_affinity,
                    new_affinity: t.new_affinity,
                    eco_qos_change: t.eco_qos_change,
                    blocked: t.blocked,
                    blocked_reason: t.blocked_reason,
                })
                .collect(),
            conflicts: result.conflicts,
        }))
    }

    async fn list_interventions(
        &self,
        _req: Request<ListInterventionsRequest>,
    ) -> Result<Response<ListInterventionsReply>, Status> {
        let interventions = self
            .engine
            .interventions()
            .into_iter()
            .map(|i| Intervention {
                rule_id: i.rule_id,
                rule_name: i.rule_name,
                pid: i.pid,
                image_name: i.image_name,
                applied: i.applied,
                since_ms: i.since_ms,
            })
            .collect();
        Ok(Response::new(ListInterventionsReply { interventions }))
    }

    async fn list_profiles(
        &self,
        _req: Request<ListProfilesRequest>,
    ) -> Result<Response<ListProfilesReply>, Status> {
        let store = self.store.lock().map_err(|_| poisoned())?;
        let rows = store
            .list_profiles()
            .map_err(|e| Status::internal(format!("list_profiles: {e}")))?;
        Ok(Response::new(ListProfilesReply {
            profiles: rows.iter().map(row_to_profile).collect(),
        }))
    }

    async fn create_profile(
        &self,
        req: Request<CreateProfileRequest>,
    ) -> Result<Response<CreateProfileReply>, Status> {
        let p = req
            .into_inner()
            .profile
            .ok_or_else(|| Status::invalid_argument("profile is required"))?;
        let mut store = self.store.lock().map_err(|_| poisoned())?;
        let id = store
            .create_profile(&p.name, &p.power_mode, p.active, &p.rule_ids)
            .map_err(|e| Status::internal(format!("create_profile: {e}")))?;
        Ok(Response::new(CreateProfileReply { id }))
    }

    async fn update_profile(
        &self,
        req: Request<UpdateProfileRequest>,
    ) -> Result<Response<UpdateProfileReply>, Status> {
        let p = req
            .into_inner()
            .profile
            .ok_or_else(|| Status::invalid_argument("profile is required"))?;
        let row = ProfileRow {
            id: p.id,
            name: p.name,
            power_mode: p.power_mode,
            active: p.active,
            rule_ids: p.rule_ids,
        };
        let mut store = self.store.lock().map_err(|_| poisoned())?;
        let ok = store
            .update_profile(&row)
            .map_err(|e| Status::internal(format!("update_profile: {e}")))?;
        Ok(Response::new(UpdateProfileReply { ok }))
    }

    async fn delete_profile(
        &self,
        req: Request<DeleteProfileRequest>,
    ) -> Result<Response<DeleteProfileReply>, Status> {
        let id = req.into_inner().id;
        let store = self.store.lock().map_err(|_| poisoned())?;
        let ok = store
            .delete_profile(id)
            .map_err(|e| Status::internal(format!("delete_profile: {e}")))?;
        Ok(Response::new(DeleteProfileReply { ok }))
    }

    async fn set_profile_active(
        &self,
        req: Request<SetProfileActiveRequest>,
    ) -> Result<Response<SetProfileActiveReply>, Status> {
        let r = req.into_inner();
        let msg = {
            let store = self.store.lock().map_err(|_| poisoned())?;
            set_profile_active_impl(&store, r.id, r.active)
                .map_err(|e| Status::internal(format!("set_profile_active: {e}")))?
        };
        match msg {
            Some(message) => Ok(Response::new(SetProfileActiveReply { ok: true, message })),
            None => Ok(Response::new(SetProfileActiveReply {
                ok: false,
                message: format!("no profile #{}", r.id),
            })),
        }
    }

    /// Returns the current dynamic-responsiveness-protection config (R3,
    /// PRD §9.7.3). Read straight from the live engine (which loaded it from the
    /// store at start and reflects any live `SetDynamicProtection`).
    async fn get_dynamic_protection(
        &self,
        _req: Request<GetDynamicProtectionRequest>,
    ) -> Result<Response<GetDynamicProtectionReply>, Status> {
        let cfg = self.engine.dynamic_config();
        Ok(Response::new(GetDynamicProtectionReply {
            config: Some(dyn_config_to_proto(cfg)),
        }))
    }

    /// Validates + persists a new dynamic-protection config and applies it live
    /// (R3, PRD §9.7.3): enabling lets the next sampler tick begin evaluating
    /// candidates; disabling restores every active dampening immediately.
    async fn set_dynamic_protection(
        &self,
        req: Request<SetDynamicProtectionRequest>,
    ) -> Result<Response<SetDynamicProtectionReply>, Status> {
        let proto = req
            .into_inner()
            .config
            .ok_or_else(|| Status::invalid_argument("config is required"))?;
        let cfg = match validate_dyn_config(&proto) {
            Ok(c) => c,
            Err(message) => {
                return Ok(Response::new(SetDynamicProtectionReply {
                    ok: false,
                    message,
                }));
            }
        };
        // Persist + swap the live config + (on disable) restore all dampenings.
        // The engine touches the store + reversal ledger (blocking) — run it off
        // the async runtime.
        let engine = self.engine.clone();
        tokio::task::spawn_blocking(move || engine.set_dynamic_config(cfg))
            .await
            .map_err(|e| Status::internal(format!("set_dynamic_protection task: {e}")))?;

        let message = if cfg.enabled {
            format!(
                "dynamic protection enabled (threshold {}‰, sustain {}s, max {}s)",
                cfg.cpu_threshold_permille, cfg.sustain_seconds, cfg.max_intervention_seconds
            )
        } else {
            "dynamic protection disabled (all active dampenings restored)".to_string()
        };
        Ok(Response::new(SetDynamicProtectionReply {
            ok: true,
            message,
        }))
    }
}

/// Applies (or clears) a profile as a *bundle* toggling its rules' effective
/// enablement (PRD §9.7.4). Activating a profile enables all its rules, marks it
/// active, and deactivates every other active profile — disabling only the rules
/// those profiles hold exclusively (a rule shared with the newly-active profile
/// stays enabled). Deactivating a profile disables its rules. The power mode is
/// applied best-effort via the power overlay (degraded, never fatal). Returns
/// `Some(summary)` when the profile existed, `None` otherwise. The applier loop
/// picks up the resulting enabled-set on its next tick.
pub(crate) fn set_profile_active_impl(
    store: &atlas_store::Store,
    id: i64,
    active: bool,
) -> anyhow::Result<Option<String>> {
    let target = match store.get_profile(id)? {
        Some(p) => p,
        None => return Ok(None),
    };

    if !active {
        for rid in &target.rule_ids {
            store.set_rule_enabled(*rid, false)?;
        }
        store.set_profile_active(id, false)?;
        return Ok(Some(format!(
            "profile '{}' deactivated ({} rule(s) disabled)",
            target.name,
            target.rule_ids.len()
        )));
    }

    // Activate: enable this profile's rules and mark it active.
    for rid in &target.rule_ids {
        store.set_rule_enabled(*rid, true)?;
    }
    store.set_profile_active(id, true)?;

    // Deactivate every other active profile; disable rules they hold that the
    // newly-active profile does not also include.
    let keep: std::collections::HashSet<i64> = target.rule_ids.iter().copied().collect();
    let mut deactivated = 0u32;
    for other in store.list_profiles()? {
        if other.id == id || !other.active {
            continue;
        }
        for rid in &other.rule_ids {
            if !keep.contains(rid) {
                store.set_rule_enabled(*rid, false)?;
            }
        }
        store.set_profile_active(other.id, false)?;
        deactivated += 1;
    }

    // Best-effort power-mode application (feature-flagged / degrades).
    let power_note = {
        let out = atlas_collectors::set_power_overlay(&target.power_mode);
        if !out.success {
            tracing::warn!("profile power mode: {}", out.message);
        }
        out.message
    };

    Ok(Some(format!(
        "profile '{}' activated ({} rule(s) enabled, {} other profile(s) deactivated); {}",
        target.name,
        target.rule_ids.len(),
        deactivated,
        power_note
    )))
}
