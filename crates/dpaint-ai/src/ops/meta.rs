//! Ops that need no network: what is configured, and what it is allowed to cost.

use super::*;
use crate::budget::Budget;
use crate::keys::providers_status;
use crate::Runtime;
use dpaint_core::{parse_args, schema_for, Project};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct NoArgs {}

pub(crate) struct ProviderStatusOp {
    pub rt: Runtime,
}

impl Op for ProviderStatusOp {
    fn id(&self) -> &'static str {
        "ai.provider.status"
    }
    fn about(&self) -> &'static str {
        "Report which providers are configured, from which source, and with which models."
    }
    fn schema(&self) -> Value {
        schema_for::<NoArgs>()
    }
    fn is_query(&self) -> bool {
        true
    }

    fn apply(&self, _project: &mut Project, args: Value, _cx: &mut OpCx) -> Result<OpEffect> {
        let _: NoArgs = parse_args(self.id(), args)?;
        let statuses = providers_status(self.rt.keys.as_ref(), self.rt.config.as_ref());
        Ok(OpEffect::default().with_data(json!({ "providers": statuses })))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct BudgetSetArgs {
    /// Spend ceiling for this project, in USD.
    #[serde(default)]
    pub limit_usd: Option<f64>,
    /// Remove the ceiling entirely.
    #[serde(default)]
    pub clear: bool,
    /// Also reset the recorded spend to zero.
    #[serde(default)]
    pub reset_spend: bool,
}

pub(crate) struct BudgetSet;

impl Op for BudgetSet {
    fn id(&self) -> &'static str {
        "ai.budget.set"
    }
    fn about(&self) -> &'static str {
        "Set this project's AI spend ceiling in USD."
    }
    fn schema(&self) -> Value {
        schema_for::<BudgetSetArgs>()
    }

    fn apply(&self, _project: &mut Project, args: Value, cx: &mut OpCx) -> Result<OpEffect> {
        let a: BudgetSetArgs = parse_args(self.id(), args)?;
        if a.clear == a.limit_usd.is_some() {
            return Err(Error::SchemaViolation {
                op: self.id().to_string(),
                detail: "set either limit-usd or clear".into(),
            });
        }
        if let Some(l) = a.limit_usd {
            if l < 0.0 {
                return Err(Error::SchemaViolation {
                    op: self.id().to_string(),
                    detail: format!("limit-usd {l} is negative"),
                });
            }
        }

        let mut budget = Budget::load(cx.assets.root());
        if cx.dry_run {
            return Ok(OpEffect::default().with_data(budget.status()));
        }
        if a.reset_spend {
            budget.entries.clear();
        }
        budget.set_ceiling(if a.clear { None } else { a.limit_usd })?;
        Ok(OpEffect::default().with_data(budget.status()))
    }
}

pub(crate) struct BudgetStatus;

impl Op for BudgetStatus {
    fn id(&self) -> &'static str {
        "ai.budget.status"
    }
    fn about(&self) -> &'static str {
        "Report the AI spend ceiling and what has been spent, by provider, model and op."
    }
    fn schema(&self) -> Value {
        schema_for::<NoArgs>()
    }
    fn is_query(&self) -> bool {
        true
    }

    fn apply(&self, _project: &mut Project, args: Value, cx: &mut OpCx) -> Result<OpEffect> {
        let _: NoArgs = parse_args(self.id(), args)?;
        let budget = Budget::load(cx.assets.root());
        Ok(OpEffect::default().with_data(budget.status()))
    }
}
