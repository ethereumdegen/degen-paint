//! `model.validate` — glTF conformance, topology, UV coverage and texture size.

use super::target_model;
use dpaint_core::doc::DocKind;
use dpaint_core::{Op, OpCx, OpEffect, Project, Result};
use serde::Deserialize;

/// 1x1 transparent PNG, standing in for a raster document while validating. Structural
/// conformance does not depend on the pixels, and the op has no renderer to call.
const STUB_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x60, 0x00, 0x02, 0x00,
    0x00, 0x05, 0x00, 0x01, 0xe9, 0xfa, 0xdc, 0xd8, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
    0xae, 0x42, 0x60, 0x82,
];

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateArgs {
    /// Largest texture edge, in pixels, that does not draw a warning.
    #[serde(default = "default_max_texture")]
    pub max_texture_size: u32,
}

fn default_max_texture() -> u32 {
    4096
}

/// Query op: reports findings in `OpEffect::data`, mutates nothing, never journaled.
pub struct ModelValidate;

impl Op for ModelValidate {
    fn id(&self) -> &'static str {
        "model.validate"
    }

    fn about(&self) -> &'static str {
        "Check glTF conformance, non-manifold edges, degenerate triangles, UV coverage and texture size"
    }

    fn schema(&self) -> serde_json::Value {
        dpaint_core::schema_for::<ValidateArgs>()
    }

    fn modes(&self) -> &'static [DocKind] {
        &[DocKind::Model]
    }

    fn is_query(&self) -> bool {
        true
    }

    fn apply(
        &self,
        project: &mut Project,
        args: serde_json::Value,
        cx: &mut OpCx,
    ) -> Result<OpEffect> {
        let args: ValidateArgs = dpaint_core::parse_args(self.id(), args)?;
        let doc = target_model(project, cx)?;
        let stub = |_: &dpaint_core::DocId| Ok(STUB_PNG.to_vec());
        let findings = crate::validate::validate(
            project,
            &doc,
            cx.assets,
            &stub,
            args.max_texture_size.max(1),
        )?;
        let errors = findings
            .iter()
            .filter(|f| f.severity == crate::validate::Severity::Error)
            .count();
        let warnings = findings
            .iter()
            .filter(|f| f.severity == crate::validate::Severity::Warning)
            .count();
        Ok(OpEffect::default().with_data(serde_json::json!({
            "document": doc.to_string(),
            "ok": errors == 0,
            "errors": errors,
            "warnings": warnings,
            "findings": findings,
        })))
    }
}
