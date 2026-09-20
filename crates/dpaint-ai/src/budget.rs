//! Per-project spend ceiling and ledger.
//!
//! Stored next to the project, so a ceiling travels with the work it protects. A request that
//! would push the total past the ceiling fails with `budget_exceeded` (exit code 6) *before*
//! anything is sent, rather than after the money is gone.

use dpaint_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spend {
    pub at: String,
    pub op: String,
    pub provider: String,
    pub model: String,
    pub cost_usd: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Budget {
    /// `None` means no ceiling has been set for this project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ceiling_usd: Option<f64>,
    #[serde(default)]
    pub entries: Vec<Spend>,
    #[serde(skip)]
    path: PathBuf,
}

impl Budget {
    pub fn path_in(root: &Path) -> PathBuf {
        root.join("ai").join("budget.json")
    }

    pub fn load(root: &Path) -> Self {
        let path = Self::path_in(root);
        let mut b: Budget = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        b.path = path;
        b
    }

    pub fn spent(&self) -> f64 {
        self.entries.iter().map(|e| e.cost_usd).sum()
    }

    pub fn remaining(&self) -> Option<f64> {
        self.ceiling_usd.map(|c| c - self.spent())
    }

    /// Refuse a call whose estimated price would take the project past its ceiling.
    pub fn check(&self, estimate: f64) -> Result<()> {
        let Some(ceiling) = self.ceiling_usd else {
            return Ok(());
        };
        let spent = self.spent();
        if spent + estimate > ceiling + f64::EPSILON {
            return Err(Error::BudgetExceeded { spent: spent + estimate, ceiling });
        }
        Ok(())
    }

    pub fn set_ceiling(&mut self, ceiling: Option<f64>) -> Result<()> {
        self.ceiling_usd = ceiling;
        self.save()
    }

    pub fn record(&mut self, spend: Spend) -> Result<()> {
        self.entries.push(spend);
        self.save()
    }

    /// Spend grouped by a field, for `ai.budget.status`.
    pub fn by<F: Fn(&Spend) -> String>(&self, f: F) -> BTreeMap<String, f64> {
        let mut out: BTreeMap<String, f64> = BTreeMap::new();
        for e in &self.entries {
            *out.entry(f(e)).or_insert(0.0) += e.cost_usd;
        }
        out
    }

    pub fn status(&self) -> serde_json::Value {
        serde_json::json!({
            "ceilingUsd": self.ceiling_usd,
            "spentUsd": round_cents(self.spent()),
            "remainingUsd": self.remaining().map(round_cents),
            "calls": self.entries.len(),
            "byProvider": self.by(|e| e.provider.clone()),
            "byModel": self.by(|e| e.model.clone()),
            "byOp": self.by(|e| e.op.clone()),
        })
    }

    fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

fn round_cents(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spend(cost: f64) -> Spend {
        Spend {
            at: dpaint_core::now_iso(),
            op: "ai.image.generate".into(),
            provider: "fal".into(),
            model: "fal-ai/flux/dev".into(),
            cost_usd: cost,
            request_id: None,
        }
    }

    #[test]
    fn no_ceiling_means_no_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let b = Budget::load(dir.path());
        assert!(b.check(1000.0).is_ok());
    }

    #[test]
    fn a_call_that_would_cross_the_ceiling_is_refused_before_it_is_made() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = Budget::load(dir.path());
        b.set_ceiling(Some(0.10)).unwrap();
        b.record(spend(0.08)).unwrap();

        assert!(b.check(0.02).is_ok(), "exactly at the ceiling is allowed");
        let err = b.check(0.05).unwrap_err();
        assert_eq!(err.code(), "budget_exceeded");
        assert_eq!(err.exit_code(), 6);
    }

    #[test]
    fn the_ledger_survives_a_reload_and_groups_spend() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = Budget::load(dir.path());
        b.set_ceiling(Some(5.0)).unwrap();
        b.record(spend(0.02)).unwrap();
        let mut other = spend(0.03);
        other.provider = "quiver".into();
        other.op = "ai.vector.generate".into();
        b.record(other).unwrap();

        let reloaded = Budget::load(dir.path());
        assert_eq!(reloaded.entries.len(), 2);
        assert!((reloaded.spent() - 0.05).abs() < 1e-9);
        assert!((reloaded.remaining().unwrap() - 4.95).abs() < 1e-9);
        let status = reloaded.status();
        assert_eq!(status["byProvider"]["quiver"], 0.03);
        assert_eq!(status["byOp"]["ai.image.generate"], 0.02);
    }
}
