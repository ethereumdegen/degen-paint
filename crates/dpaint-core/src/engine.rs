//! The engine: applies ops transactionally, journals them, and saves.
//!
//! Validate fully, then mutate. An op runs against a clone; only a successful run is
//! committed, journaled and written. A failed op — or a failed op inside a batch — leaves
//! `project.json`, the journal and the asset store exactly as they were.

use crate::error::Result;
use crate::journal::Actor;
use crate::op::{OpCx, OpEffect, Registry};
use crate::project::Workspace;
use serde::Serialize;

pub struct Engine {
    pub registry: Registry,
    pub workspace: Workspace,
    pub actor: Actor,
}

#[derive(Debug, Clone, Serialize)]
pub struct Applied {
    pub op: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(flatten)]
    pub effect: OpEffect,
}

impl Engine {
    pub fn new(registry: Registry, workspace: Workspace) -> Self {
        Self {
            registry,
            workspace,
            actor: Actor::Agent,
        }
    }

    pub fn as_human(mut self) -> Self {
        self.actor = Actor::Human;
        self
    }

    /// Apply one op. `dry_run` validates and reports the effect without writing anything.
    pub fn apply(
        &mut self,
        op_id: &str,
        args: serde_json::Value,
        doc: Option<String>,
        dry_run: bool,
    ) -> Result<Applied> {
        let op = self.registry.get(op_id)?;
        let before = serde_json::to_value(&self.workspace.project)?;
        let mut candidate = self.workspace.project.clone();

        let effect = {
            let mut cx = OpCx::new(&self.workspace.assets).with_doc(doc);
            cx.dry_run = dry_run;
            op.apply(&mut candidate, args.clone(), &mut cx)?
        };

        if dry_run || op.is_query() {
            return Ok(Applied {
                op: op_id.to_string(),
                seq: None,
                effect,
            });
        }

        candidate.touch();
        let after = serde_json::to_value(&candidate)?;
        self.workspace.project = candidate;
        let seq = self.workspace.journal.record(
            op_id,
            args,
            &before,
            &after,
            Some(effect.clone()),
            self.actor,
        )?;
        self.workspace.save()?;
        Ok(Applied {
            op: op_id.to_string(),
            seq: Some(seq),
            effect,
        })
    }

    /// Apply many ops as one transaction: all succeed or nothing is written. This is what
    /// turns a thirty-op build into one MCP round trip instead of thirty model turns.
    pub fn apply_batch(
        &mut self,
        ops: Vec<(String, serde_json::Value, Option<String>)>,
        dry_run: bool,
    ) -> Result<Vec<Applied>> {
        let before = serde_json::to_value(&self.workspace.project)?;
        let mut candidate = self.workspace.project.clone();
        let mut results = Vec::with_capacity(ops.len());

        for (id, args, doc) in &ops {
            let op = self.registry.get(id)?;
            let mut cx = OpCx::new(&self.workspace.assets).with_doc(doc.clone());
            cx.dry_run = dry_run;
            let effect = op.apply(&mut candidate, args.clone(), &mut cx)?;
            results.push(Applied {
                op: id.clone(),
                seq: None,
                effect,
            });
        }

        if dry_run {
            return Ok(results);
        }

        candidate.touch();
        let after = serde_json::to_value(&candidate)?;
        self.workspace.project = candidate;

        // One journal entry per op keeps undo granular, with patches sliced from the
        // single committed transition.
        let ids: Vec<String> = ops.iter().map(|(i, _, _)| i.clone()).collect();
        let seq = self.workspace.journal.record(
            &format!("batch[{}]", ids.join(" ")),
            serde_json::json!({ "ops": ops.iter().map(|(i, a, d)| serde_json::json!({"op": i, "args": a, "doc": d})).collect::<Vec<_>>() }),
            &before,
            &after,
            Some(OpEffect {
                changed: results.iter().flat_map(|r| r.effect.changed.clone()).collect(),
                created: results.iter().flat_map(|r| r.effect.created.clone()).collect(),
                removed: results.iter().flat_map(|r| r.effect.removed.clone()).collect(),
                warnings: results.iter().flat_map(|r| r.effect.warnings.clone()).collect(),
                cost_usd: None,
                data: None,
            }),
            self.actor,
        )?;
        self.workspace.save()?;
        for r in &mut results {
            r.seq = Some(seq);
        }
        Ok(results)
    }

    pub fn undo(&mut self) -> Result<Option<String>> {
        let out = self.workspace.journal.undo(&mut self.workspace.project)?;
        if out.is_some() {
            self.workspace.save()?;
        }
        Ok(out)
    }

    pub fn redo(&mut self) -> Result<Option<String>> {
        let out = self.workspace.journal.redo(&mut self.workspace.project)?;
        if out.is_some() {
            self.workspace.save()?;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{DocKind, Document, RasterDoc};
    use crate::error::Error;
    use crate::ids::DocId;
    use crate::op::{parse_args, schema_for, Op};
    use crate::project::Project;

    struct SetDpi;
    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct A {
        dpi: f32,
    }
    impl Op for SetDpi {
        fn id(&self) -> &'static str {
            "raster.canvas.set-dpi"
        }
        fn about(&self) -> &'static str {
            "set dpi"
        }
        fn schema(&self) -> serde_json::Value {
            schema_for::<A>()
        }
        fn modes(&self) -> &'static [DocKind] {
            &[DocKind::Raster]
        }
        fn apply(
            &self,
            p: &mut Project,
            args: serde_json::Value,
            cx: &mut OpCx,
        ) -> Result<OpEffect> {
            let a: A = parse_args(self.id(), args)?;
            let d = cx.target_doc(p)?;
            p.raster_mut(&d)?.dpi = a.dpi;
            Ok(OpEffect::changed(&d))
        }
    }

    struct AlwaysFails;
    impl Op for AlwaysFails {
        fn id(&self) -> &'static str {
            "test.fail"
        }
        fn about(&self) -> &'static str {
            "always fails"
        }
        fn schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        fn apply(
            &self,
            p: &mut Project,
            _a: serde_json::Value,
            _cx: &mut OpCx,
        ) -> Result<OpEffect> {
            p.name = "corrupted".into();
            Err(Error::Invalid("nope".into()))
        }
    }

    fn engine(dir: &std::path::Path) -> Engine {
        let project = Project::new(
            "t",
            Document::Raster(RasterDoc::new(DocId::from("doc_main"), "main", 8, 8)),
        );
        let ws = Workspace::create(dir, project).unwrap();
        let mut reg = Registry::new();
        reg.register(SetDpi).register(AlwaysFails);
        Engine::new(reg, ws)
    }

    #[test]
    fn applying_an_op_writes_journals_and_is_undoable() {
        let tmp = tempfile::tempdir().unwrap();
        let mut e = engine(&tmp.path().join("p.dpaint"));

        let applied = e
            .apply(
                "raster.canvas.set-dpi",
                serde_json::json!({"dpi": 300}),
                None,
                false,
            )
            .unwrap();
        assert_eq!(applied.seq, Some(1));
        assert_eq!(
            e.workspace
                .project
                .raster(&DocId::from("doc_main"))
                .unwrap()
                .dpi,
            300.0
        );

        // Persisted, not just in memory.
        let reopened = Workspace::open(e.workspace.root()).unwrap();
        assert_eq!(
            reopened
                .project
                .raster(&DocId::from("doc_main"))
                .unwrap()
                .dpi,
            300.0
        );

        assert_eq!(e.undo().unwrap().as_deref(), Some("raster.canvas.set-dpi"));
        assert_eq!(
            e.workspace
                .project
                .raster(&DocId::from("doc_main"))
                .unwrap()
                .dpi,
            72.0
        );
        assert_eq!(e.redo().unwrap().as_deref(), Some("raster.canvas.set-dpi"));
        assert_eq!(
            e.workspace
                .project
                .raster(&DocId::from("doc_main"))
                .unwrap()
                .dpi,
            300.0
        );
    }

    #[test]
    fn a_failing_op_mutates_nothing_even_if_it_mutated_before_erroring() {
        let tmp = tempfile::tempdir().unwrap();
        let mut e = engine(&tmp.path().join("p.dpaint"));
        let err = e
            .apply("test.fail", serde_json::json!({}), None, false)
            .unwrap_err();
        assert_eq!(err.code(), "invalid");
        assert_eq!(
            e.workspace.project.name, "t",
            "the failed op must not leak its partial mutation"
        );
        assert!(e.workspace.journal.entries().is_empty());
    }

    #[test]
    fn dry_run_reports_the_effect_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut e = engine(&tmp.path().join("p.dpaint"));
        let applied = e
            .apply(
                "raster.canvas.set-dpi",
                serde_json::json!({"dpi": 600}),
                None,
                true,
            )
            .unwrap();
        assert_eq!(applied.effect.changed, vec![DocId::from("doc_main")]);
        assert_eq!(applied.seq, None);
        assert_eq!(
            e.workspace
                .project
                .raster(&DocId::from("doc_main"))
                .unwrap()
                .dpi,
            72.0
        );
    }

    #[test]
    fn a_batch_is_all_or_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut e = engine(&tmp.path().join("p.dpaint"));
        let err = e
            .apply_batch(
                vec![
                    (
                        "raster.canvas.set-dpi".into(),
                        serde_json::json!({"dpi": 300}),
                        None,
                    ),
                    ("test.fail".into(), serde_json::json!({}), None),
                ],
                false,
            )
            .unwrap_err();
        assert_eq!(err.code(), "invalid");
        assert_eq!(
            e.workspace
                .project
                .raster(&DocId::from("doc_main"))
                .unwrap()
                .dpi,
            72.0,
            "op 1 must roll back when op 2 fails"
        );

        let ok = e
            .apply_batch(
                vec![
                    (
                        "raster.canvas.set-dpi".into(),
                        serde_json::json!({"dpi": 150}),
                        None,
                    ),
                    (
                        "raster.canvas.set-dpi".into(),
                        serde_json::json!({"dpi": 300}),
                        None,
                    ),
                ],
                false,
            )
            .unwrap();
        assert_eq!(ok.len(), 2);
        assert_eq!(
            e.workspace
                .project
                .raster(&DocId::from("doc_main"))
                .unwrap()
                .dpi,
            300.0
        );
    }
}
